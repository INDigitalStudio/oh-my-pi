//! Agent Client Protocol adapter over the journal-first kernel and session.

use std::{
	env, fs,
	path::{Path, PathBuf},
	sync::Arc,
};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{
	ApprovalDecision, ApprovalScope, ApprovalSource, Inference, Kernel, RunControl, TurnInput, Up,
};
use omp_core::Str;
use omp_dom::Event;
use omp_driver::{discovery::roles, headless::kernel::SessionHome};
use omp_session::Session;
use serde_json::{Map, Value, json};
use tokio::io::{
	AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader, stdin, stdout,
};

use crate::cli::{AcpArgs, ChatArgs};

/// Runs ACP using stdin for NDJSON requests and stdout for NDJSON responses.
pub async fn run(args: AcpArgs) -> miette::Result<()> {
	let max_time = args.max_time.map(|duration| duration.0);
	let future = run_inner(args.launch);
	match max_time {
		Some(limit) => tokio::time::timeout(limit, future)
			.await
			.map_err(|_| miette!("ACP mode exceeded --max-time"))?,
		None => future.await,
	}
}

async fn run_inner(args: ChatArgs) -> miette::Result<()> {
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let project = fs::canonicalize(&args.project).into_diagnostic()?;
	let ctx = Arc::new(crate::process_ctx(&project)?);
	for overlay in &args.config {
		let script = fs::read_to_string(overlay).into_diagnostic()?;
		ctx.exec(&script, omp_con::Source::Config(Str::new(overlay.to_string_lossy())))
			.into_diagnostic()?;
	}
	let home = env::var_os("HOME").map_or_else(|| project.clone(), PathBuf::from);
	let model_settings =
		omp_catalog::settings::ModelSettings::from_con(&ctx).resolve_path_scopes(&project, &home);
	let catalog = if args.gateway.is_some() {
		Arc::new(omp_catalog::snapshot::Catalog::embedded().clone())
	} else {
		omp_driver::registry::production_catalog(&data_dir).map_err(|source| miette!(source))?
	};
	let launch_roles = roles::resolve_launch_roles(
		catalog.as_ref(),
		&model_settings,
		None,
		args.smol.as_deref(),
		args.slow.as_deref(),
		args.plan.as_deref(),
	)
	.map_err(|source| miette!(source))?;
	let model = args
		.model
		.clone()
		.or_else(|| launch_roles.primary.map(|value| Str::from(value.as_str())))
		.ok_or_else(|| miette!("ACP mode requires a configured default model role"))?;
	let prompt_slots = crate::spec::resolve_prompt_slots(
		&project,
		&home,
		args.prompt_settings.custom_prompt.as_deref(),
		args.prompt_settings.append_prompt.as_deref(),
	)?;
	let prompt = omp_driver::headless::kernel::PromptOverrides {
		custom_prompt: prompt_slots.system,
		append_prompt: prompt_slots.append,
		personality: args.prompt_settings.personality.clone(),
		include_model: args.prompt_settings.include_model_in_prompt,
		include_workstation: args.prompt_settings.include_workstation,
		include_workspace_tree: args.prompt_settings.include_workspace_tree,
		render_mermaid: args.prompt_settings.render_mermaid,
		include_skills: args.prompt_settings.skills_enabled,
		null_prompt: args.prompt_settings.null_prompt,
	};
	let approval_mode = args.effective_approval().map(Into::into);
	let live_sessions = Arc::new(omp_driver::sessions::SessionRegistry::new());
	let gateway = match args.gateway.as_ref() {
		Some(endpoint) => Some(endpoint.connect().await.into_diagnostic()?),
		None => None,
	};
	let options = omp_driver::headless::kernel::KernelOptions {
		continue_session: args.continue_session,
		session: args
			.resume
			.as_ref()
			.map(|value| PathBuf::from(value.as_str())),
		fork: args
			.fork
			.as_ref()
			.map(|value| PathBuf::from(value.as_str())),
		sessions_dir: args.session_dir.clone(),
		ephemeral: args.no_session,
		no_tools: args.no_tools,
		tools: args.tools.as_ref().map(|tools| tools.0.clone()),
		py_eval: args.py_eval,
		spawn_idle_timeout: args.envd_idle_timeout,
		api_key: args.api_key.clone(),
		approval_mode,
		model_override: args.model.is_some(),
		prompt,
		provider: args
			.provider
			.as_ref()
			.map(|value| omp_catalog::ProviderId::from(value.as_str()))
			.or_else(|| {
				args.api_key.as_ref().and_then(|_| {
					model
						.split_once('/')
						.map(|(provider, _)| omp_catalog::ProviderId::from(provider))
				})
			}),
		gateway,
		sessions: Some(Arc::clone(&live_sessions)),
		session_name: None,
		tool_registry: None,
		..omp_driver::headless::kernel::KernelOptions::default()
	};
	let (kernel, mut session, _) = omp_driver::headless::kernel::compose_kernel(
		&data_dir,
		&project,
		model.as_str(),
		Arc::clone(&ctx),
		options.clone(),
	)
	.await
	.into_diagnostic()?;
	crate::chat_cmd::apply_launch_thinking(&ctx, args.thinking).into_diagnostic()?;
	if args.plan_mode || args.plan_yolo {
		crate::chat_cmd::set_plan_mode(&mut session, true).into_diagnostic()?;
	}
	let home = SessionHome::new(
		&data_dir,
		&project,
		&options,
		model,
		kernel.mailbox(),
	)
	.into_diagnostic()?;
	serve_acp(kernel, session, home, stdin(), stdout()).await
}

struct TurnCompletion<C> {
	kernel:   Kernel<C>,
	session:  Session,
	id:       Option<Value>,
	response: Result<Value, (i64, &'static str)>,
}

enum InputEvent<C> {
	Line(Option<String>),
	Turn(TurnCompletion<C>),
}

/// Serves ACP over caller-provided NDJSON transport halves.
#[doc(hidden)]
pub async fn serve_acp<C, R, W>(
	kernel: Kernel<C>,
	mut session: Session,
	home: SessionHome,
	input: R,
	mut output: W,
) -> miette::Result<()>
where
	C: Inference + Send + Sync + 'static,
	R: AsyncRead + Unpin,
	W: AsyncWrite + Unpin + Send + 'static,
{
	home.register(&session);
	let mut session_id = session_identifier(&session);
	let (output_tx, output_rx) = flume::unbounded::<Value>();
	let writer = tokio::spawn(async move {
		while let Ok(value) = output_rx.recv_async().await {
			let mut bytes = serde_json::to_vec(&value).into_diagnostic()?;
			bytes.push(b'\n');
			output.write_all(&bytes).await.into_diagnostic()?;
			output.flush().await.into_diagnostic()?;
		}
		Ok::<(), miette::Report>(())
	});
	let (_, events) = session.subscribe();
	let mut forwarder = Some(forward_events(events, output_tx.clone(), session_id.clone()));
	let mailbox = kernel.mailbox();
	let mut controller = Some((kernel, session));
	let mut active: Option<tokio::task::JoinHandle<TurnCompletion<C>>> = None;
	let mut initialized = false;
	let mut lines = BufReader::new(input).lines();

	loop {
		let input_event: InputEvent<C> = if let Some(turn) = active.as_mut() {
			tokio::select! {
				completed = turn => InputEvent::Turn(completed.into_diagnostic()?),
				line = lines.next_line() => InputEvent::Line(line.into_diagnostic()?),
			}
		} else {
			InputEvent::Line(lines.next_line().await.into_diagnostic()?)
		};
		let line = match input_event {
			InputEvent::Turn(completed) => {
				active = None;
				restore_turn(completed, &mut controller, &output_tx)?;
				continue;
			},
			InputEvent::Line(Some(line)) => line,
			InputEvent::Line(None) => {
				if let Some(turn) = active.take() {
					let _ = mailbox.send(Up::Interrupt);
					restore_turn(turn.await.into_diagnostic()?, &mut controller, &output_tx)?;
				}
				break;
			},
		};
		if line.trim().is_empty() {
			continue;
		}
		let frame: Value = match serde_json::from_str(&line) {
			Ok(frame) => frame,
			Err(source) => {
				output_tx
					.send(error(Value::Null, -32700, &source.to_string()))
					.into_diagnostic()?;
				continue;
			},
		};
		let id = frame.get("id").cloned();
		let Some(method) = frame.get("method").and_then(Value::as_str) else {
			if let Some(id) = id {
				output_tx
					.send(error(id, -32600, "request has no method"))
					.into_diagnostic()?;
			}
			continue;
		};
		let params = frame
			.get("params")
			.and_then(Value::as_object)
			.cloned()
			.unwrap_or_default();
		if method != "initialize" && !initialized {
			if let Some(id) = id {
				output_tx
					.send(error(id, -32002, "initialize must complete before other requests"))
					.into_diagnostic()?;
			}
			continue;
		}
		let result = match method {
			"initialize" => {
				let version = params
					.get("protocolVersion")
					.and_then(Value::as_u64)
					.unwrap_or(1);
				if version != 1 {
					Err((-32602, "unsupported ACP protocol version"))
				} else {
					initialized = true;
					Ok(json!({
						"protocolVersion": 1,
						"agentInfo": {
							"name": "oh-my-pi",
							"title": "Oh My Pi",
							"version": env!("CARGO_PKG_VERSION"),
						},
						"authMethods": [],
						"agentCapabilities": {
							"loadSession": true,
							"sessionCapabilities": {"resume": {}, "close": {}},
							"promptCapabilities": {"image": false, "embeddedContext": false},
						},
					}))
				}
			},
			"authenticate" => Ok(json!({})),
			"session/new" if active.is_some() => Err((-32001, "a turn is already running")),
			"session/new" => {
				let next = match home.create(None) {
					Ok(next) => next,
					Err(source) => {
						if let Some(id) = id {
							output_tx
								.send(error(id, -32000, &source.to_string()))
								.into_diagnostic()?;
						}
						continue;
					},
				};
				switch_session(
					&mut controller,
					next,
					&home,
					&output_tx,
					&mut forwarder,
					&mut session_id,
				)
				.await?;
				Ok(session_descriptor(session_id.as_str()))
			},
			"session/load" | "session/resume" if active.is_some() => {
				Err((-32001, "a turn is already running"))
			},
			"session/load" | "session/resume" => {
				let selector = match requested_session(&params) {
					Ok(selector) => selector,
					Err(message) => {
						if let Some(id) = id {
							output_tx.send(error(id, -32602, message)).into_diagnostic()?;
						}
						continue;
					},
				};
				let next = match home.open(Path::new(selector)) {
					Ok(next) => next,
					Err(source) => {
						if let Some(id) = id {
							output_tx
								.send(error(id, -32000, &source.to_string()))
								.into_diagnostic()?;
						}
						continue;
					},
				};
				switch_session(
					&mut controller,
					next,
					&home,
					&output_tx,
					&mut forwarder,
					&mut session_id,
				)
				.await?;
				Ok(session_descriptor(session_id.as_str()))
			},
			"session/prompt" if active.is_some() => Err((-32001, "a turn is already running")),
			"session/prompt" => match prompt_text(&params) {
				Ok(text) => {
					let (mut kernel, mut session) =
						controller.take().expect("idle ACP controller owns its kernel and session");
					active = Some(tokio::spawn(async move {
						let response = match kernel
							.run_turn(
								&mut session,
								TurnInput { text, attachments: Vec::new() },
								RunControl::default(),
							)
							.await
						{
							Ok(outcome) => Ok(json!({
								"stopReason": if outcome.stop == omp_agent::TurnStop::Cancelled {
									"cancelled"
								} else {
									"end_turn"
								},
								"text": outcome.assistant_text,
							})),
							Err(_) => Err((-32000, "agent turn failed")),
						};
						TurnCompletion { kernel, session, id, response }
					}));
					continue;
				},
				Err(message) => Err((-32602, message)),
			},
			"session/cancel" => {
				if active.is_some() {
					let _ = mailbox.send(Up::Interrupt);
				}
				Ok(json!({}))
			},
			"session/approve" => match approval(&params) {
				Ok((id, decision)) => {
					if active.is_some() {
						let _ = mailbox.send(Up::Approve { id, decision });
					}
					Ok(json!({}))
				},
				Err(message) => Err((-32602, message)),
			},
			"session/close" | "shutdown" => {
				if let Some(id) = id {
					output_tx.send(success(id, json!({}))).into_diagnostic()?;
				}
				if let Some(turn) = active.take() {
					let _ = mailbox.send(Up::Interrupt);
					restore_turn(turn.await.into_diagnostic()?, &mut controller, &output_tx)?;
				}
				break;
			},
			_ => Err((-32601, "unknown ACP method")),
		};
		if let Some(id) = id {
			let response = match result {
				Ok(value) => success(id, value),
				Err((code, message)) => error(id, code, message),
			};
			output_tx.send(response).into_diagnostic()?;
		}
	}

	let (kernel, mut session) = controller
		.take()
		.expect("ACP controller owns its kernel and session after active turn completion");
	session.process_exit().into_diagnostic()?;
	home.unregister(&session);
	drop(session);
	drop(kernel);
	if let Some(forwarder) = forwarder {
		forwarder.await.into_diagnostic()??;
	}
	drop(output_tx);
	writer.await.into_diagnostic()??;
	Ok(())
}

fn forward_events(
	events: flume::Receiver<Event>,
	output: flume::Sender<Value>,
	session_id: Str,
) -> tokio::task::JoinHandle<miette::Result<()>> {
	tokio::spawn(async move {
		while let Ok(event) = events.recv_async().await {
			if output.send(acp_event_value(session_id.as_str(), event)?).is_err() {
				break;
			}
		}
		Ok(())
	})
}

fn restore_turn<C>(
	completed: TurnCompletion<C>,
	controller: &mut Option<(Kernel<C>, Session)>,
	output: &flume::Sender<Value>,
) -> miette::Result<()> {
	let TurnCompletion { kernel, session, id, response } = completed;
	*controller = Some((kernel, session));
	if let Some(id) = id {
		let response = match response {
			Ok(value) => success(id, value),
			Err((code, message)) => error(id, code, message),
		};
		output.send(response).into_diagnostic()?;
	}
	Ok(())
}

async fn switch_session<C>(
	controller: &mut Option<(Kernel<C>, Session)>,
	mut next: Session,
	home: &SessionHome,
	output: &flume::Sender<Value>,
	forwarder: &mut Option<tokio::task::JoinHandle<miette::Result<()>>>,
	session_id: &mut Str,
) -> miette::Result<()> {
	let (snapshot, events) = next.subscribe();
	let (kernel, mut previous) =
		controller.take().expect("idle ACP controller owns its kernel and session");
	let _ = previous.session_switch();
	home.unregister(&previous);
	drop(previous);
	if let Some(previous_forwarder) = forwarder.take() {
		previous_forwarder.await.into_diagnostic()??;
	}
	home.register(&next);
	*session_id = session_identifier(&next);
	output
		.send(acp_event_value(
			session_id.as_str(),
			Event::Reset { snapshot },
		)?)
		.into_diagnostic()?;
	*forwarder = Some(forward_events(events, output.clone(), session_id.clone()));
	*controller = Some((kernel, next));
	Ok(())
}

fn session_identifier(session: &Session) -> Str {
	session
		.journal_path()
		.file_stem()
		.and_then(|value| value.to_str())
		.map_or_else(|| Str::new_static("session"), Str::new)
}

fn requested_session(params: &Map<String, Value>) -> Result<&str, &'static str> {
	params
		.get("sessionId")
		.or_else(|| params.get("session"))
		.and_then(Value::as_str)
		.ok_or("session/load requires sessionId")
}

fn session_descriptor(session_id: &str) -> Value {
	json!({
		"sessionId": session_id,
		"modes": {"currentModeId": "default", "availableModes": []},
		"models": {"currentModelId": "configured", "availableModels": []},
	})
}

fn prompt_text(params: &Map<String, Value>) -> Result<Str, &'static str> {
	if let Some(text) = params
		.get("prompt")
		.or_else(|| params.get("message"))
		.and_then(Value::as_str)
	{
		return Ok(Str::new(text));
	}
	let Some(parts) = params.get("prompt").and_then(Value::as_array) else {
		return Err("session/prompt requires a prompt");
	};
	let mut text = String::new();
	for part in parts {
		if part.get("type").and_then(Value::as_str) == Some("text")
			&& let Some(value) = part.get("text").and_then(Value::as_str)
		{
			text.push_str(value);
		}
	}
	if text.is_empty() {
		Err("prompt contains no text")
	} else {
		Ok(Str::new(text))
	}
}

fn approval(params: &Map<String, Value>) -> Result<(Str, ApprovalDecision), &'static str> {
	let id = params
		.get("promptId")
		.or_else(|| params.get("id"))
		.and_then(Value::as_str)
		.ok_or("session/approve requires promptId")?;
	let approved = params
		.get("approved")
		.and_then(Value::as_bool)
		.unwrap_or(false);
	let scope = params
		.get("scope")
		.and_then(Value::as_str)
		.unwrap_or("once")
		.parse::<ApprovalScope>()
		.expect("approval scope parsing is infallible");
	Ok((Str::new(id), ApprovalDecision {
		approved,
		scope,
		source: ApprovalSource::External,
		decided_by: None,
		reason: None,
		audited: false,
	}))
}

fn acp_event_value(session_id: &str, event: Event) -> miette::Result<Value> {
	let update = match event {
		Event::Patch(patch) => json!({
			"sessionUpdate": "patch",
			"event": "patch@1",
			"data": serde_json::to_value(patch).into_diagnostic()?,
		}),
		Event::Reset { snapshot } => json!({
			"sessionUpdate": "snapshot",
			"data": serde_json::from_slice::<Value>(snapshot.as_bytes()).into_diagnostic()?,
		}),
		Event::Stream { cause, sid, op, node, prop, text } => json!({
			"sessionUpdate": "patch",
			"event": "stream@1",
			"data": {"cause": cause, "sid": sid, "op": op, "node": node, "prop": prop, "text": text},
		}),
	};
	Ok(json!({
		"jsonrpc": "2.0",
		"method": "session/update",
		"params": {"sessionId": session_id, "update": update},
	}))
}

fn success(id: Value, result: Value) -> Value {
	json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
	json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
