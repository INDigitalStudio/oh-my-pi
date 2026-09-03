//! Agent Client Protocol adapter over the journal-first kernel and session.

use std::{borrow::Cow, fs, path::Path, sync::Arc};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{
	ApprovalDecision, ApprovalScope, ApprovalSource, Inference, Kernel, RunControl, TurnInput, Up,
};
use omp_core::{Str, base64};
use omp_dom::Event;
use omp_driver::headless::kernel::SessionHome;
use omp_session::{AttachmentInput, Session, SessionError};
use serde_json::{Map, Value, json};
use tokio::io::{
	AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader, stdin, stdout,
};

use crate::{
	chat_cmd::{Launch, LaunchEnv},
	cli::{AcpArgs, ChatArgs},
};

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
	let project = fs::canonicalize(&args.project).into_diagnostic()?;
	let ctx = Arc::new(crate::process_ctx(&project)?);
	let env = LaunchEnv::production(&project, args.gateway.is_some())?;
	let launch = Launch::prepare(args, ctx, env).await?;
	let (kernel, session) = launch.compose().await?;
	let home = SessionHome::new(
		&launch.data_dir,
		&launch.project,
		&launch.options,
		launch.model.clone(),
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
	mut kernel: Kernel<C>,
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
	// pi `acp-permission-gate.ts`: every journaled approval prompt becomes
	// one `session/request_permission` request; the client's selected
	// option answers the prompt (`session/approve` remains for clients that
	// answer by prompt id).
	let permission_requests = request_permissions(kernel.subscribe(), output_tx.clone());
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
			// A response to one of our `session/request_permission` requests.
			if let Some((prompt_id, decision)) =
				permission_requests.answer(id.as_ref(), frame.get("result"))
			{
				let _ = mailbox.send(Up::Approve { id: prompt_id, decision });
				continue;
			}
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
							"promptCapabilities": {"image": true, "embeddedContext": true},
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
							output_tx
								.send(error(id, -32602, message))
								.into_diagnostic()?;
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
			"session/prompt" => match prompt_input(&params) {
				Ok(prompt) => {
					let (mut kernel, mut session) = controller
						.take()
						.expect("idle ACP controller owns its kernel and session");
					let input = match prompt.into_turn_input(&session) {
						Ok(input) => input,
						Err(source) => {
							controller = Some((kernel, session));
							if let Some(id) = id {
								output_tx
									.send(error(id, -32000, &source.to_string()))
									.into_diagnostic()?;
							}
							continue;
						},
					};
					active = Some(tokio::spawn(async move {
						let response = match kernel
							.run_turn(&mut session, input, RunControl::default())
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
			// ACP names the notification `cancel`; `session/cancel` is the
			// legacy spelling earlier omp clients used.
			"cancel" | "session/cancel" => {
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

/// Outstanding `session/request_permission` requests keyed by JSON-RPC id.
#[derive(Clone)]
struct PermissionRequests {
	pending: Arc<parking_lot::Mutex<std::collections::BTreeMap<u64, Str>>>,
	_task:   Arc<tokio::task::JoinHandle<()>>,
}

impl PermissionRequests {
	/// Maps a client response to the prompt it answers: pi's option ids
	/// `allow_once`/`allow_always`/`reject_once`/`reject_always`; a
	/// `cancelled` outcome or an unknown option fails closed.
	fn answer(&self, id: Option<&Value>, result: Option<&Value>) -> Option<(Str, ApprovalDecision)> {
		let id = id.and_then(Value::as_u64)?;
		let prompt_id = self.pending.lock().remove(&id)?;
		let outcome = result.and_then(|result| result.get("outcome"));
		let option = outcome
			.filter(|outcome| outcome.get("outcome").and_then(Value::as_str) == Some("selected"))
			.and_then(|outcome| outcome.get("optionId"))
			.and_then(Value::as_str);
		let (approved, scope) = match option {
			Some("allow_once") => (true, ApprovalScope::Once),
			Some("allow_always") => (true, ApprovalScope::Session),
			_ => (false, ApprovalScope::Once),
		};
		Some((prompt_id, ApprovalDecision {
			approved,
			scope,
			source: ApprovalSource::External,
			decided_by: None,
			reason: (!approved).then(|| Str::new_static("rejected by ACP client")),
			audited: false,
		}))
	}
}

fn request_permissions(
	events: flume::Receiver<omp_agent::KernelEvent>,
	output: flume::Sender<Value>,
) -> PermissionRequests {
	let pending = Arc::new(parking_lot::Mutex::new(std::collections::BTreeMap::new()));
	let table = Arc::clone(&pending);
	let task = tokio::spawn(async move {
		let mut next_id = 1_u64;
		while let Ok(event) = events.recv_async().await {
			let omp_agent::KernelEvent::ApprovalRequested(ticket) = event else {
				continue;
			};
			let id = next_id;
			next_id += 1;
			table.lock().insert(id, ticket.ticket_id.clone());
			let first = ticket.reasons.first();
			let request = json!({
				"jsonrpc": "2.0",
				"id": id,
				"method": "session/request_permission",
				"params": {
					"toolCall": {
						"toolCallId": ticket.invocation_id.as_deref().unwrap_or(ticket.ticket_id.as_str()),
						"title": first.map_or("Approval required", |spec| spec.title.as_str()),
						"status": "pending",
						"rawInput": {"subject": first.map(|spec| spec.subject.as_str()), "body": first.map(|spec| spec.body.as_str())},
					},
					"options": [
						{"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
						{"optionId": "allow_always", "name": "Always allow", "kind": "allow_always"},
						{"optionId": "reject_once", "name": "Reject", "kind": "reject_once"},
						{"optionId": "reject_always", "name": "Always reject", "kind": "reject_always"},
					],
				},
			});
			if output.send(request).is_err() {
				break;
			}
		}
	});
	PermissionRequests { pending, _task: Arc::new(task) }
}

fn forward_events(
	events: flume::Receiver<Event>,
	output: flume::Sender<Value>,
	session_id: Str,
) -> tokio::task::JoinHandle<miette::Result<()>> {
	tokio::spawn(async move {
		while let Ok(event) = events.recv_async().await {
			if output
				.send(acp_event_value(session_id.as_str(), event)?)
				.is_err()
			{
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
	let (kernel, mut previous) = controller
		.take()
		.expect("idle ACP controller owns its kernel and session");
	let _ = previous.session_switch();
	home.unregister(&previous);
	drop(previous);
	if let Some(previous_forwarder) = forwarder.take() {
		previous_forwarder.await.into_diagnostic()??;
	}
	home.register(&next);
	*session_id = session_identifier(&next);
	output
		.send(acp_event_value(session_id.as_str(), Event::Reset { snapshot })?)
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

/// A `session/prompt` request reduced to the turn text and its image blocks
/// (pi `acp-agent.ts` `#convertPromptBlocks`), each block decoded with the
/// `mimeType` it declared.
struct PromptInput {
	text:   Str,
	images: Vec<AttachmentInput>,
}

impl PromptInput {
	/// Stores every image in the session's blob store and returns the turn
	/// input whose attachments reference them — the seam the chat composer's
	/// image chips also take.
	fn into_turn_input(self, session: &Session) -> Result<TurnInput, SessionError> {
		Ok(TurnInput { text: self.text, attachments: session.store_attachments(self.images)? })
	}
}

/// Reduces the request's `prompt` to text plus images: a bare string, a
/// `{text}` object, or the ACP content-block array (`text`, `image`,
/// `resource`, `resource_link`, `audio`).
fn prompt_input(params: &Map<String, Value>) -> Result<PromptInput, &'static str> {
	let prompt = params
		.get("prompt")
		.or_else(|| params.get("message"))
		.ok_or("session/prompt requires a prompt")?;
	if let Some(text) = prompt.as_str() {
		return Ok(PromptInput { text: Str::new(text), images: Vec::new() });
	}
	if let Some(text) = prompt.get("text").and_then(Value::as_str) {
		return Ok(PromptInput { text: Str::new(text), images: Vec::new() });
	}
	let blocks = prompt
		.as_array()
		.ok_or("session/prompt requires a prompt string or content blocks")?;
	let mut texts: Vec<Cow<'_, str>> = Vec::with_capacity(blocks.len());
	let mut images = Vec::new();
	for block in blocks {
		match block.get("type").and_then(Value::as_str) {
			Some("text") => {
				texts.push(Cow::Borrowed(
					block.get("text").and_then(Value::as_str).unwrap_or_default(),
				));
			},
			Some("image") => {
				let data = block
					.get("data")
					.and_then(Value::as_str)
					.ok_or("image content block requires base64 data")?;
				let mime = block
					.get("mimeType")
					.and_then(Value::as_str)
					.ok_or("image content block requires mimeType")?;
				images.push(decode_image(data, mime)?);
			},
			Some("resource") => {
				let resource = block.get("resource").ok_or("resource block requires a resource")?;
				if let Some(text) = resource.get("text").and_then(Value::as_str) {
					texts.push(Cow::Borrowed(text));
				} else if let Some(mime) = resource
					.get("mimeType")
					.and_then(Value::as_str)
					.filter(|mime| mime.starts_with("image/"))
					&& let Some(blob) = resource.get("blob").and_then(Value::as_str)
				{
					images.push(decode_image(blob, mime)?);
				} else {
					let uri = resource.get("uri").and_then(Value::as_str).unwrap_or_default();
					texts.push(Cow::Owned(format!("[embedded resource: {uri}]")));
				}
			},
			Some("resource_link") => {
				texts.push(Cow::Borrowed(
					block
						.get("title")
						.or_else(|| block.get("name"))
						.or_else(|| block.get("uri"))
						.and_then(Value::as_str)
						.unwrap_or_default(),
				));
			},
			Some("audio") => texts.push(Cow::Borrowed("[audio omitted]")),
			_ => return Err("unsupported prompt content block"),
		}
	}
	let text = texts.join("\n\n");
	let text = text.trim();
	if text.is_empty() && images.is_empty() {
		return Err("prompt contains no text");
	}
	Ok(PromptInput { text: Str::new(text), images })
}

fn decode_image(data: &str, mime: &str) -> Result<AttachmentInput, &'static str> {
	base64::decode(data.as_bytes())
		.into_vec()
		.map(|bytes| AttachmentInput { mime: Str::new(mime), bytes: bytes.into() })
		.map_err(|_| "image content block data is not valid base64")
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

#[cfg(test)]
mod tests {
	use super::*;

	fn params(value: Value) -> Map<String, Value> {
		value.as_object().expect("object params").clone()
	}

	#[test]
	fn prompt_accepts_string_object_and_content_blocks() {
		let plain = prompt_input(&params(json!({"prompt": "hi"}))).expect("string prompt");
		assert_eq!(plain.text.as_str(), "hi");
		assert!(plain.images.is_empty());

		let object =
			prompt_input(&params(json!({"prompt": {"text": "structured"}}))).expect("object prompt");
		assert_eq!(object.text.as_str(), "structured");

		let blocks = prompt_input(&params(json!({"prompt": [
			{"type": "text", "text": "look"},
			{"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
			{"type": "resource", "resource": {"uri": "file:///a.txt", "text": "alpha"}},
			{"type": "resource", "resource": {"uri": "file:///b.bin", "mimeType": "application/octet-stream", "blob": "AAAA"}},
			{"type": "resource", "resource": {"uri": "file:///c.png", "mimeType": "image/png", "blob": "d29ybGQ="}},
			{"type": "resource_link", "uri": "file:///d.md", "title": "Design"},
			{"type": "audio", "data": "", "mimeType": "audio/wav"},
		]})))
		.expect("content blocks");
		assert_eq!(
			blocks.text.as_str(),
			"look\n\nalpha\n\n[embedded resource: file:///b.bin]\n\nDesign\n\n[audio omitted]"
		);
		let images = blocks
			.images
			.iter()
			.map(|image| (image.mime.as_str(), image.bytes.as_ref()))
			.collect::<Vec<_>>();
		assert_eq!(images, vec![("image/png", b"hello".as_slice()), ("image/png", b"world".as_slice())]);
		assert_eq!(
			prompt_input(&params(json!({"prompt": [{"type": "image", "data": "aGVsbG8="}]}))).err(),
			Some("image content block requires mimeType")
		);
	}

	#[test]
	fn prompt_rejects_missing_and_malformed_content() {
		assert_eq!(
			prompt_input(&params(json!({}))).err(),
			Some("session/prompt requires a prompt")
		);
		assert_eq!(
			prompt_input(&params(json!({"prompt": [{"type": "text", "text": "  "}]}))).err(),
			Some("prompt contains no text")
		);
		assert_eq!(
			prompt_input(&params(json!({"prompt": [{"type": "image", "data": "%%%", "mimeType": "image/png"}]})))
				.err(),
			Some("image content block data is not valid base64")
		);
		assert_eq!(
			prompt_input(&params(json!({"prompt": [{"type": "video"}]}))).err(),
			Some("unsupported prompt content block")
		);
		let image_only = prompt_input(&params(json!({"prompt": [
			{"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
		]})))
		.expect("an image-only prompt is a valid turn");
		assert!(image_only.text.is_empty());
		assert_eq!(image_only.images.len(), 1);
	}
}
