//! Single-shot adapter over the journal-first production agent kernel.
//!
//! JSON mode is an NDJSON lifecycle stream: one `session` header, then
//! `agent_start` → `turn_start` → message/tool events → `turn_end` →
//! `agent_end` for each submitted prompt. `--shape-transcript` removes repeated
//! message/partial snapshots from `message_update` while preserving its
//! incremental `assistantMessageEvent`; terminal messages and tool results
//! remain complete.

use std::{fs, io::IsTerminal as _, path::PathBuf, sync::Arc, time::Instant};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{RunControl, TurnInput, TurnStop};
use omp_core::{FastHashMap, Str};
use omp_dom::{
	Dom, Event, Handle, KnownTag, Node, Op, PropId, PropKey, Sid, StreamOp, Tag, Value,
};
use omp_driver::{
	discovery::roles,
	headless::kernel::{KernelOptions, compose_kernel},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

use crate::{cli::PrintArgs, usage_error::CliUsageError};

/// Runs prompts through the new durable headless kernel.
pub async fn run(args: PrintArgs) -> miette::Result<()> {
	let Some(max_time) = args.max_time.map(|duration| duration.0) else {
		return run_inner(args).await;
	};
	match tokio::time::timeout(max_time, run_inner(args)).await {
		Ok(result) => result,
		Err(_) => Err(miette!("print mode exceeded --max-time")),
	}
}

async fn run_inner(args: PrintArgs) -> miette::Result<()> {
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let project = fs::canonicalize(&args.project).into_diagnostic()?;
	let ctx = Arc::new(crate::process_ctx(&project)?);
	for overlay in &args.config {
		let script = fs::read_to_string(overlay).into_diagnostic()?;
		ctx.exec(&script, omp_con::Source::Config(Str::new(overlay.to_string_lossy())))
			.into_diagnostic()?;
	}
	let home = std::env::var_os("HOME").map_or_else(|| project.clone(), PathBuf::from);
	let prompt = crate::chat_cmd::prompt_overrides(&project, &home, &args.prompt_settings)?;
	let extensions = crate::chat_cmd::driver_extension_policy(&args.extension_launch);
	let model_settings =
		omp_catalog::settings::ModelSettings::from_con(&ctx).resolve_path_scopes(&project, &home);
	let catalog =
		omp_driver::registry::production_catalog(&data_dir).map_err(|source| miette!(source))?;
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
		.or_else(|| launch_roles.primary.map(|model| Str::from(model.as_str())))
		.ok_or_else(|| miette!("print mode requires a configured default model role"))?;
	if args.api_key.is_some() && args.model.is_none() && args.models.is_none() {
		return Err(miette!("--api-key requires a model to be specified via --model or --models"));
	}
	if args.from_claude || args.from_codex {
		return Err(miette!("print mode does not accept interactive legacy session imports"));
	}

	let initial = initial_prompt(&args.prompt).await?;
	if initial.is_empty() {
		return Err(
			CliUsageError::new("print mode requires a prompt or piped standard input").into(),
		);
	}
	let explicit_session = args
		.resume
		.as_ref()
		.map(|value| PathBuf::from(value.as_str()));
	let (mut kernel, mut session, _) =
		compose_kernel(&data_dir, &project, model.as_str(), Arc::clone(&ctx), KernelOptions {
			continue_session:   args.continue_session,
			session:            explicit_session,
			fork:               args
				.fork
				.as_ref()
				.map(|value| PathBuf::from(value.as_str())),
			sessions_dir:       args.session_dir.clone(),
			ephemeral:          args.no_session,
			no_tools:           args.no_tools,
			tools:              args.tools.as_ref().map(|tools| tools.0.clone()),
			py_eval:            args.py_eval,
			spawn_idle_timeout: args.envd_idle_timeout,
			api_key:            args.api_key.clone(),
			approval_mode:      args.effective_approval().map(Into::into),
			model_override:     args.model.is_some(),
			prompt,
			extensions,
			provider:           args
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
			gateway:            None,
			sessions:           None,
						session_name: None,
			tool_registry: None,
			output_schema: None,
			schema_mode: None,
		})
		.await
		.into_diagnostic()?;
	crate::chat_cmd::apply_launch_thinking(&ctx, args.thinking).into_diagnostic()?;
	crate::chat_cmd::apply_launch_plan(&mut session, args.plan_mode, args.plan_yolo)
		.into_diagnostic()?;
	let ephemeral_path = args
		.no_session
		.then(|| session.journal_path().to_path_buf());
	let session_id = session
		.journal_path()
		.file_stem()
		.and_then(|value| value.to_str())
		.unwrap_or("ephemeral")
		.to_owned();
	let (snapshot, events) = session.subscribe();
	let mut replica = Dom::from_snapshot(&snapshot);
	let mut json = JsonState::default();
	let mut stdout = tokio::io::stdout();
	if args.mode == "json" {
		write_json_line(&mut stdout, &session_header(&session_id, model.as_str()))
		.await?;
	}
	let mut prompts = Vec::with_capacity(1 + args.follow_ups.len());
	prompts.push(initial);
	prompts.extend(args.follow_ups.iter().cloned());

	for prompt in prompts {
		let submission_turn = replica.children(replica.body()).len();
		if args.mode == "json" {
			write_json_line(&mut stdout, &serde_json::json!({"type":"agent_start"})).await?;
		}
		let deadline = args.max_time.map(|duration| Instant::now() + duration.0);
		let control = RunControl::new(CancellationToken::new(), deadline);
		let turn = kernel.run_turn(
			&mut session,
			TurnInput { text: prompt, attachments: Vec::new() },
			control,
		);
		tokio::pin!(turn);
		let mut ended_with_newline = true;
		let outcome = loop {
			tokio::select! {
				biased;
				event = events.recv_async() => {
					if let Ok(event) = event {
						print_event(
							&mut stdout,
							&args,
							&mut replica,
							&mut json,
							event,
							&mut ended_with_newline,
						).await?;
					}
				},
				result = &mut turn => break result.into_diagnostic()?,
			}
		};
		while let Ok(event) = events.try_recv() {
			print_event(
				&mut stdout,
				&args,
				&mut replica,
				&mut json,
				event,
				&mut ended_with_newline,
			)
			.await?;
		}
		if args.mode == "text" && !ended_with_newline {
			stdout.write_all(b"\n").await.into_diagnostic()?;
		} else if args.mode == "json" {
			if let Some(event) = json.finish_turn(&replica) {
				write_json_line(&mut stdout, &event).await?;
			}
			write_json_line(
				&mut stdout,
				&serde_json::json!({
					"type": "agent_end",
					"messages": transcript_messages_from(&replica, submission_turn),
					"isTerminal": true,
				}),
			)
			.await?;
		}
		stdout.flush().await.into_diagnostic()?;
		if outcome.stop != TurnStop::Completed {
			return Err(miette!("print turn stopped before completion: {:?}", outcome.stop));
		}
	}

	drop(session);
	if let Some(path) = ephemeral_path {
		let _ = fs::remove_file(path);
	}
	Ok(())
}

#[derive(Clone, Copy)]
enum PrintedStream {
	Text(Handle),
	Thinking(Handle),
	ToolArguments(Handle),
	ToolResult(Handle),
}

#[derive(Default)]
struct JsonState {
	streams:   FastHashMap<Sid, PrintedStream>,
	open_turn: Option<Handle>,
}

impl JsonState {
	fn finish_turn(&mut self, dom: &Dom) -> Option<serde_json::Value> {
		let turn = self.open_turn.take()?;
		Some(turn_end_value(dom, turn))
	}
}

async fn print_event(
	stdout: &mut tokio::io::Stdout,
	args: &PrintArgs,
	replica: &mut Dom,
	state: &mut JsonState,
	event: Event,
	ended_with_newline: &mut bool,
) -> miette::Result<()> {
	let (values, text, tools) = project_print_event(args, replica, state, event)?;
	if args.mode == "json" {
		for value in values {
			write_json_line(stdout, &value).await?;
		}
		*ended_with_newline = true;
		return Ok(());
	}
	if let Some(text) = text {
		stdout.write_all(text.as_bytes()).await.into_diagnostic()?;
		*ended_with_newline = text.ends_with('\n');
	}
	for name in tools {
		if !*ended_with_newline {
			stdout.write_all(b"\n").await.into_diagnostic()?;
		}
		stdout
			.write_all(format!("[tool: {name}]\n").as_bytes())
			.await
			.into_diagnostic()?;
		*ended_with_newline = true;
	}
	Ok(())
}

fn project_print_event(
	args: &PrintArgs,
	replica: &mut Dom,
	state: &mut JsonState,
	event: Event,
) -> miette::Result<(Vec<serde_json::Value>, Option<Str>, Vec<Str>)> {
	let mut values = Vec::new();
	let mut text = None;
	let mut tools = Vec::new();
	let mut inserted = Vec::new();
	let mut appended = None;

	match &event {
		Event::Patch(patch) => {
			let mut next = replica.high_water() + 1;
			for op in &patch.ops {
				if let Op::Ins { node, .. } = op {
					if let Some(handle) = Handle::new(next) {
						inserted.push((handle, node.tag.clone()));
					}
					next += 1;
					if node.tag == Tag::Known(KnownTag::Turn)
						&& let Some(turn) = state.open_turn.take()
					{
						values.push(turn_end_value(replica, turn));
					}
				}
			}
		},
		Event::Stream { sid, op: StreamOp::Open, node: Some(node), prop: Some(prop), .. } => {
			let target = replica.get(*node).and_then(|target| match &target.tag {
				Tag::Known(KnownTag::Assistant) if *prop == PropId::Text.into() => {
					Some(PrintedStream::Text(*node))
				},
				Tag::Known(KnownTag::Assistant) if *prop == PropId::Thinking.into() => {
					Some(PrintedStream::Thinking(*node))
				},
				Tag::Known(KnownTag::Input) => {
					replica.parent(*node).map(PrintedStream::ToolArguments)
				},
				Tag::Known(KnownTag::Result | KnownTag::Diag) => {
					replica.parent(*node).map(PrintedStream::ToolResult)
				},
				_ => None,
			});
			if let Some(target) = target {
				state.streams.insert(*sid, target);
			}
		},
		Event::Stream { sid, op: StreamOp::Append, text: Some(delta), .. } => {
			appended = state.streams.get(sid).copied().map(|stream| (stream, delta.clone()));
		},
		Event::Reset { .. } => {
			state.streams.clear();
			state.open_turn = None;
		},
		Event::Stream { .. } => {},
	}

	replica.apply_event(&event).into_diagnostic()?;

	for (handle, tag) in inserted {
		match tag {
			Tag::Known(KnownTag::Turn) => {
				state.open_turn = Some(handle);
				values.push(serde_json::json!({"type":"turn_start"}));
			},
			Tag::Known(KnownTag::User) => {
				let message = message_value(replica, handle);
				values.push(serde_json::json!({
					"type": "message_start",
					"message": message.clone(),
				}));
				values.push(serde_json::json!({"type":"message_end","message":message}));
			},
			Tag::Known(KnownTag::Assistant) => {
				values.push(serde_json::json!({
					"type": "message_start",
					"message": message_value(replica, handle),
				}));
			},
			Tag::Custom(name) => {
				tools.push(name);
				values.push(tool_call_update(replica, handle, "toolcall_start", "", args));
				if prop_text(replica.get(handle), PropId::Status) == Some("running") {
					let delta = serde_json::to_string(&tool_args(replica, handle))
						.unwrap_or_else(|_| "{}".to_owned());
					values.push(tool_call_update(
						replica,
						handle,
						"toolcall_delta",
						&delta,
						args,
					));
					values.push(tool_call_update(replica, handle, "toolcall_end", "", args));
					values.push(tool_execution_start(replica, handle));
				}
			},
			_ => {},
		}
	}

	if let Some((stream, delta)) = appended {
		match stream {
			PrintedStream::Text(assistant) => {
				text = Some(delta.clone());
				values.push(message_delta(replica, assistant, "text_delta", delta.as_str(), args));
			},
			PrintedStream::Thinking(assistant) => {
				if args.print_thoughts {
					text = Some(delta.clone());
				}
				values.push(message_delta(
					replica,
					assistant,
					"thinking_delta",
					delta.as_str(),
					args,
				));
			},
			PrintedStream::ToolArguments(call) => {
				values.push(tool_call_update(replica, call, "toolcall_delta", delta.as_str(), args));
			},
			PrintedStream::ToolResult(call) => {
				values.push(serde_json::json!({
					"type": "tool_execution_update",
					"toolCallId": prop_text(replica.get(call), PropId::Id).unwrap_or_default(),
					"toolName": tool_name(replica.get(call)).unwrap_or_default(),
					"args": tool_args(replica, call),
					"partialResult": tool_result_value(replica, call),
				}));
			},
		}
	}

	if let Event::Patch(patch) = &event {
		for op in &patch.ops {
			let Op::Set { h, prop, value } = op else {
				continue;
			};
			if *prop == PropId::StopReason.into()
				&& replica
					.get(*h)
					.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
			{
				values.push(serde_json::json!({
					"type": "message_end",
					"message": message_value(replica, *h),
				}));
			} else if *prop == PropId::Status.into()
				&& matches!(replica.get(*h).map(|node| &node.tag), Some(Tag::Custom(_)))
			{
				match value.as_str() {
					Some("running") => {
						values.push(tool_call_update(replica, *h, "toolcall_end", "", args));
						values.push(tool_execution_start(replica, *h));
					},
					Some("ok" | "error" | "cancelled" | "aborted") => {
						let result = tool_result_value(replica, *h);
						values.push(tool_execution_end(replica, *h));
						values.push(serde_json::json!({
							"type": "message_start",
							"message": result.clone(),
						}));
						values.push(serde_json::json!({
							"type": "message_end",
							"message": result,
						}));
					},
					_ => {},
				}
			}
		}
	}
	if let Event::Stream { sid, op: StreamOp::Close, .. } = event {
		state.streams.remove(&sid);
	}
	Ok((values, text, tools))
}

fn session_header(id: &str, model: &str) -> serde_json::Value {
	serde_json::json!({"type":"session","version":1,"id":id,"model":model})
}

async fn write_json_line(
	stdout: &mut tokio::io::Stdout,
	value: &serde_json::Value,
) -> miette::Result<()> {
	let mut line = serde_json::to_vec(value).into_diagnostic()?;
	line.push(b'\n');
	stdout.write_all(&line).await.into_diagnostic()
}

fn message_delta(
	dom: &Dom,
	assistant: Handle,
	kind: &str,
	delta: &str,
	args: &PrintArgs,
) -> serde_json::Value {
	let stream = serde_json::json!({"type":kind,"contentIndex":0,"delta":delta});
	shaped_message_update(dom, assistant, stream, args.shape_transcript)
}

fn tool_call_update(
	dom: &Dom,
	call: Handle,
	kind: &str,
	delta: &str,
	args: &PrintArgs,
) -> serde_json::Value {
	let id = prop_text(dom.get(call), PropId::Id).unwrap_or_default();
	let name = tool_name(dom.get(call)).unwrap_or_default();
	let stream = serde_json::json!({
		"type": kind,
		"contentIndex": 0,
		"toolCallId": id,
		"toolName": name,
		"delta": delta,
	});
	let assistant = dom
		.parent(call)
		.and_then(|turn| {
			dom.children(turn).iter().copied().find(|handle| {
				dom.get(*handle)
					.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
			})
		})
		.unwrap_or(call);
	shaped_message_update(dom, assistant, stream, args.shape_transcript)
}

fn shaped_message_update(
	dom: &Dom,
	assistant: Handle,
	mut stream: serde_json::Value,
	shaped: bool,
) -> serde_json::Value {
	let mut event =
		serde_json::json!({"type":"message_update","assistantMessageEvent":stream.clone()});
	if !shaped {
		let message = message_value(dom, assistant);
		stream["partial"] = message.clone();
		event["message"] = message;
		event["assistantMessageEvent"] = stream;
	}
	event
}

fn tool_execution_start(dom: &Dom, call: Handle) -> serde_json::Value {
	serde_json::json!({
		"type": "tool_execution_start",
		"toolCallId": prop_text(dom.get(call), PropId::Id).unwrap_or_default(),
		"toolName": tool_name(dom.get(call)).unwrap_or_default(),
		"args": tool_args(dom, call),
	})
}

fn tool_execution_end(dom: &Dom, call: Handle) -> serde_json::Value {
	let result = tool_result_value(dom, call);
	serde_json::json!({
		"type": "tool_execution_end",
		"toolCallId": prop_text(dom.get(call), PropId::Id).unwrap_or_default(),
		"toolName": tool_name(dom.get(call)).unwrap_or_default(),
		"result": result,
		"isError": prop_text(dom.get(call), PropId::Status) == Some("error"),
	})
}

fn turn_end_value(dom: &Dom, turn: Handle) -> serde_json::Value {
	let message = dom
		.children(turn)
		.iter()
		.rev()
		.copied()
		.find(|handle| {
			dom.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
		})
		.map_or(serde_json::Value::Null, |handle| message_value(dom, handle));
	let tool_results = dom
		.children(turn)
		.iter()
		.copied()
		.filter(|handle| matches!(dom.get(*handle).map(|node| &node.tag), Some(Tag::Custom(_))))
		.filter(|handle| {
			matches!(
				prop_text(dom.get(*handle), PropId::Status),
				Some("ok" | "error" | "cancelled" | "aborted")
			)
		})
		.map(|handle| tool_result_value(dom, handle))
		.collect::<Vec<_>>();
	let usage = dom
		.children(turn)
		.iter()
		.copied()
		.find(|handle| {
			dom.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Usage))
		})
		.map_or_else(|| serde_json::json!({}), |handle| usage_value(dom.get(handle)));
	serde_json::json!({
		"type": "turn_end",
		"message": message,
		"toolResults": tool_results,
		"usage": usage,
	})
}

fn transcript_messages_from(dom: &Dom, first_turn: usize) -> Vec<serde_json::Value> {
	let mut messages = Vec::new();
	for turn in dom.children(dom.body()).iter().skip(first_turn) {
		for handle in dom.children(*turn) {
			match dom.get(*handle).map(|node| &node.tag) {
				Some(Tag::Known(KnownTag::User | KnownTag::Assistant)) => {
					messages.push(message_value(dom, *handle));
				},
				Some(Tag::Custom(_))
					if matches!(
						prop_text(dom.get(*handle), PropId::Status),
						Some("ok" | "error" | "cancelled" | "aborted")
					) =>
				{
					messages.push(tool_result_value(dom, *handle));
				},
				_ => {},
			}
		}
	}
	messages
}

fn message_value(dom: &Dom, handle: Handle) -> serde_json::Value {
	let Some(node) = dom.get(handle) else {
		return serde_json::Value::Null;
	};
	let role = if node.tag == Tag::Known(KnownTag::User) { "user" } else { "assistant" };
	let mut content = Vec::new();
	if let Some(text) = node_text(node, PropId::Thinking)
		&& !text.is_empty()
	{
		content.push(serde_json::json!({"type":"thinking","thinking":text}));
	}
	if let Some(text) = node_text(node, PropId::Text)
		&& !text.is_empty()
	{
		content.push(serde_json::json!({"type":"text","text":text}));
	} else if let Some(text) = node.content.as_deref()
		&& !text.is_empty()
	{
		content.push(serde_json::json!({"type":"text","text":text}));
	}
	let mut message = serde_json::json!({"role":role,"content":content});
	if role == "assistant"
		&& let Some(reason) = prop_text(Some(node), PropId::StopReason)
	{
		message["stopReason"] = serde_json::json!(reason);
	}
	message
}

fn tool_result_value(dom: &Dom, call: Handle) -> serde_json::Value {
	let node = dom.get(call);
	let terminal = dom.children(call).iter().rev().find_map(|handle| {
		let node = dom.get(*handle)?;
		matches!(node.tag, Tag::Known(KnownTag::Result | KnownTag::Diag)).then_some(node)
	});
	let text = terminal
		.and_then(|node| node.content.as_deref().or_else(|| node_text(node, PropId::Text)))
		.unwrap_or_default();
	serde_json::json!({
		"role": "toolResult",
		"toolCallId": prop_text(node, PropId::Id).unwrap_or_default(),
		"toolName": tool_name(node).unwrap_or_default(),
		"content": [{"type":"text","text":text}],
		"isError": prop_text(node, PropId::Status) == Some("error"),
	})
}

fn tool_args(dom: &Dom, call: Handle) -> serde_json::Value {
	let raw = dom.children(call).iter().find_map(|handle| {
		let node = dom.get(*handle)?;
		(node.tag == Tag::Known(KnownTag::Input))
			.then(|| node.content.as_deref().or_else(|| node_text(node, PropId::Text)))
			.flatten()
	});
	raw.and_then(|raw| serde_json::from_str(raw).ok())
		.unwrap_or_else(|| serde_json::json!({}))
}

fn usage_value(node: Option<&Node>) -> serde_json::Value {
	let integer = |prop| {
		node.and_then(|node| node.prop(&PropKey::Known(prop)))
			.and_then(|value| match value {
				Value::Int(value) => Some(*value),
				_ => None,
			})
			.unwrap_or_default()
	};
	serde_json::json!({
		"input": integer(PropId::TokensIn),
		"output": integer(PropId::TokensOut),
		"cacheRead": integer(PropId::CacheRead),
		"cacheWrite": integer(PropId::CacheWrite),
		"costNanoUsd": integer(PropId::CostNanoUsd),
	})
}

fn node_text(node: &Node, prop: PropId) -> Option<&str> {
	node.prop(&PropKey::Known(prop))
		.and_then(Value::as_str)
}

fn prop_text(node: Option<&Node>, prop: PropId) -> Option<&str> {
	node?.prop(&PropKey::Known(prop)).and_then(Value::as_str)
}

fn tool_name(node: Option<&Node>) -> Option<&str> {
	match &node?.tag {
		Tag::Custom(name) => Some(name.as_str()),
		_ => None,
	}
}

async fn initial_prompt(words: &[Str]) -> miette::Result<Str> {
	if !words.is_empty() {
		return Ok(Str::new(words.iter().map(Str::as_str).collect::<Vec<_>>().join(" ")));
	}
	if std::io::stdin().is_terminal() {
		return Ok(Str::default());
	}
	let mut input = String::new();
	tokio::io::stdin()
		.read_to_string(&mut input)
		.await
		.into_diagnostic()?;
	Ok(Str::new(input))
}

/// Projects the plain headless transcript from the authoritative session DOM.
#[must_use]
pub fn transcript_text(dom: &Dom) -> String {
	let mut output = String::new();
	for turn in dom.children(dom.body()) {
		for handle in dom.children(*turn) {
			let Some(node) = dom.get(*handle) else {
				continue;
			};
			match &node.tag {
				Tag::Known(KnownTag::Assistant) => {
					if let Some(Value::Str(text)) = node.prop(&PropId::Text.into()) {
						output.push_str(text.as_str());
						if !text.is_empty() && !text.ends_with('\n') {
							output.push('\n');
						}
					}
				},
				Tag::Custom(name) => {
					output.push_str("[tool: ");
					output.push_str(name.as_str());
					output.push_str("]\n");
				},
				_ => {},
			}
		}
	}
	output
}

#[cfg(test)]
mod tests {
	use serde_json::value::RawValue;
	use tempfile::tempdir;

	use super::*;

	#[test]
	fn json_stream_starts_with_resumable_session_header() {
		assert_eq!(
			session_header("01TEST", "test/model"),
			serde_json::json!({
				"type": "session",
				"version": 1,
				"id": "01TEST",
				"model": "test/model",
			}),
		);
	}

	#[test]
	fn shaped_updates_drop_snapshots_but_keep_incremental_tool_identity() {
		let scratch = tempdir().expect("scratch");
		let mut session = omp_session::Session::create(
			scratch.path().join("shape.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("session");
		session.begin_turn().expect("turn");
		session
			.assistant_start("test/model", "test", "test/model")
			.expect("assistant");
		let turn = *session.dom().children(session.dom().body()).last().expect("turn node");
		let assistant = *session
			.dom()
			.children(turn)
			.iter()
			.find(|handle| {
				session
					.dom()
					.get(**handle)
					.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
			})
			.expect("assistant node");
		let stream = serde_json::json!({
			"type": "toolcall_delta",
			"toolCallId": "call-7",
			"delta": "{\"path\":",
		});
		let shaped = shaped_message_update(session.dom(), assistant, stream.clone(), true);
		assert!(shaped.get("message").is_none());
		assert!(
			shaped["assistantMessageEvent"].get("partial").is_none(),
			"shaped stream must not repeat an ever-growing partial snapshot",
		);
		assert_eq!(shaped["assistantMessageEvent"]["toolCallId"], "call-7");

		let full = shaped_message_update(session.dom(), assistant, stream, false);
		assert!(full.get("message").is_some());
		assert!(full["assistantMessageEvent"].get("partial").is_some());
	}

	#[test]
	fn terminal_turn_event_carries_tool_results_and_agent_messages() {
		let scratch = tempdir().expect("scratch");
		let mut session = omp_session::Session::create(
			scratch.path().join("events.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("session");
		session.begin_turn().expect("turn");
		session.user("run it", Vec::new()).expect("user");
		session
			.assistant_start("test/model", "test", "test/model")
			.expect("assistant");
		session.assistant_end("tool_calls").expect("assistant end");
		let call = session
			.call(
				"bash",
				1,
				"call-1",
				None,
				Some(RawValue::from_string(r#"{"command":"echo ok"}"#.to_owned()).expect("args")),
				None,
			)
			.expect("call");
		session
			.settle(
				call,
				RawValue::from_string(
					r#"{"content":[{"type":"text","text":"ok"}]}"#.to_owned(),
				)
				.expect("outcome"),
			)
			.expect("settle");
		let turn = *session.dom().children(session.dom().body()).last().expect("turn node");
		let event = turn_end_value(session.dom(), turn);
		assert_eq!(event["type"], "turn_end");
		assert_eq!(event["toolResults"][0]["toolCallId"], "call-1");
		assert!(
			event["toolResults"][0]["content"][0]["text"]
				.as_str()
				.is_some_and(|text| text.contains("ok")),
		);
		let messages = transcript_messages_from(session.dom(), 0);
		assert!(messages.iter().any(|message| message["role"] == "user"));
		assert!(messages.iter().any(|message| message["role"] == "assistant"));
		assert!(messages.iter().any(|message| message["role"] == "toolResult"));
	}
}
