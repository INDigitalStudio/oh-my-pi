//! Single-shot adapter over the journal-first production agent kernel.
//!
//! Text mode keeps stdout clean for shell captures (pi `print-mode.ts`): the
//! only bytes written there are the final assistant response (thinking first
//! when `--print-thoughts`), after every prompt settled. Progress (`Working...`)
//! and failures go to stderr, and a failed or aborted turn exits non-zero.
//!
//! JSON mode is an NDJSON lifecycle stream: one `session` header, then
//! `agent_start` → `turn_start` → message/tool events → `turn_end` →
//! `agent_end` for each submitted prompt. A failed turn still closes with
//! `turn_end` and `agent_end`; the terminal assistant message carries
//! `stopReason` and `errorMessage` instead of the stream ending without a
//! terminal frame. `--shape-transcript` removes repeated message/partial
//! snapshots from `message_update` while preserving its incremental
//! `assistantMessageEvent`; terminal messages and tool results remain complete.

use std::{fs, io::IsTerminal as _, sync::Arc, time::Instant};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{RunControl, TurnInput, TurnStop, Up};
use omp_core::{FastHashMap, Str};
use omp_dom::{Dom, Event, Handle, KnownTag, Node, Op, PropId, PropKey, Sid, StreamOp, Tag, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

use crate::{
	chat_cmd::{Launch, LaunchEnv},
	cli::PrintArgs,
	usage_error::CliUsageError,
};

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

/// Output shaping selected by the print flags that are not launch flags.
struct PrintOptions {
	mode:             String,
	print_thoughts:   bool,
	shape_transcript: bool,
}

async fn run_inner(args: PrintArgs) -> miette::Result<()> {
	let PrintArgs { launch, mode, print_thoughts, follow_ups, shape_transcript } = args;
	let args = PrintOptions { mode, print_thoughts, shape_transcript };
	if launch.from_claude || launch.from_codex {
		return Err(miette!("print mode does not accept interactive legacy session imports"));
	}
	let project = fs::canonicalize(&launch.project).into_diagnostic()?;
	let ctx = Arc::new(crate::process_ctx(&project)?);
	let env = LaunchEnv::production(&project, launch.gateway.is_some())?;
	let launch = Launch::prepare(launch, ctx, env).await?;
	let initial = initial_prompt(&launch).await?;
	if initial.is_empty() {
		return Err(
			CliUsageError::new("print mode requires a prompt or piped standard input").into(),
		);
	}
	let (mut kernel, mut session) = launch.compose().await?;
	let model = &launch.model;
	let ephemeral_path = launch
		.ephemeral
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
	// pi `wrapper.ts`: without an interactive UI an approval-requiring call
	// is denied immediately (`--approval-mode yolo` or `tools.approval.<tool>
	// allow` opt it back in); the denial is journaled like any other.
	let kernel_events = kernel.subscribe();
	let mailbox = kernel.mailbox();
	let approvals = tokio::spawn(async move {
		while let Ok(event) = kernel_events.recv_async().await {
			if let omp_agent::KernelEvent::ApprovalRequested(ticket) = event {
				let _ = mailbox.send(Up::Approve {
					id:       ticket.ticket_id,
					decision: omp_agent::ApprovalDecision {
						approved:   false,
						scope:      omp_agent::ApprovalScope::Once,
						source:     omp_agent::ApprovalSource::Unavailable,
						decided_by: None,
						reason:     Some(Str::new_static(
							"requires approval but no interactive UI is available; use \
							 --approval-mode yolo or tools.approval.<tool> allow",
						)),
						audited:    false,
					},
				});
			}
		}
	});
	let mut stdout = tokio::io::stdout();
	if args.mode == "json" {
		write_json_line(&mut stdout, &session_header(&session_id, model.as_str())).await?;
	}
	let mut prompts = Vec::with_capacity(1 + follow_ups.len());
	prompts.push(initial);
	prompts.extend(follow_ups);
	let first_turn = replica.children(replica.body()).len();

	if args.mode == "text" {
		tokio::io::stderr()
			.write_all(b"Working...\n")
			.await
			.into_diagnostic()?;
	}
	for prompt in prompts {
		let submission_turn = replica.children(replica.body()).len();
		if args.mode == "json" {
			write_json_line(&mut stdout, &serde_json::json!({"type":"agent_start"})).await?;
		}
		let deadline = launch.max_time.map(|duration| Instant::now() + duration);
		let control = RunControl::new(CancellationToken::new(), deadline);
		let turn = kernel.run_turn(
			&mut session,
			TurnInput { text: prompt, attachments: Vec::new() },
			control,
		);
		tokio::pin!(turn);
		let result = loop {
			tokio::select! {
				biased;
				event = events.recv_async() => {
					if let Ok(event) = event {
						print_event(&mut stdout, &args, &mut replica, &mut json, event).await?;
					}
				},
				result = &mut turn => break result,
			}
		};
		// The kernel journals how a turn ended (assistant close + notice)
		// before returning; those patches are still queued here and the
		// terminal frames must reflect them.
		while let Ok(event) = events.try_recv() {
			print_event(&mut stdout, &args, &mut replica, &mut json, event).await?;
		}
		if args.mode == "json" {
			if let Some(event) = json.finish_turn(&replica) {
				write_json_line(&mut stdout, &event).await?;
			}
			write_json_line(&mut stdout, &agent_end_value(&replica, submission_turn)).await?;
		}
		stdout.flush().await.into_diagnostic()?;
		let stop = match result {
			Ok(outcome) => outcome.stop,
			Err(error) => return Err(miette::Report::from_err(error)),
		};
		if stop != TurnStop::Completed {
			return Err(miette!(
				"{}",
				turn_error_message(&replica, submission_turn)
					.unwrap_or_else(|| Str::new(format!("Request {}", stop_reason_name(stop))))
			));
		}
	}

	if args.mode == "text" {
		stdout
			.write_all(final_response_text(&replica, first_turn, args.print_thoughts).as_bytes())
			.await
			.into_diagnostic()?;
		stdout.flush().await.into_diagnostic()?;
	}

	approvals.abort();
	drop(session);
	if let Some(path) = ephemeral_path {
		let _ = fs::remove_file(path);
	}
	Ok(())
}

/// pi's stop-reason vocabulary for a turn that did not complete.
const fn stop_reason_name(stop: TurnStop) -> &'static str {
	match stop {
		TurnStop::Completed => "stop",
		TurnStop::Cancelled | TurnStop::Steered => "aborted",
		TurnStop::Failed => "error",
	}
}

/// The final `agent_end` frame: every message the submission produced, with
/// the terminal assistant carrying `stopReason`/`errorMessage` when the turn
/// failed or was aborted (pi `agent_end.messages`).
fn agent_end_value(dom: &Dom, first_turn: usize) -> serde_json::Value {
	serde_json::json!({
		"type": "agent_end",
		"messages": transcript_messages_from(dom, first_turn),
		"isTerminal": true,
	})
}

/// The journaled reason the newest turn at or after `first_turn` failed or
/// was interrupted: the content of its last `<notice kind=error|warn>`.
fn turn_error_message(dom: &Dom, first_turn: usize) -> Option<Str> {
	let turns = dom.children(dom.body());
	turns
		.iter()
		.skip(first_turn)
		.rev()
		.find_map(|turn| turn_failure_notice(dom, *turn))
}

/// The last `<notice kind=error|warn>` under `turn`, which is how the kernel
/// journals a failed or interrupted turn.
fn turn_failure_notice(dom: &Dom, turn: Handle) -> Option<Str> {
	dom.children(turn).iter().rev().find_map(|handle| {
		let node = dom.get(*handle)?;
		if node.tag != Tag::Known(KnownTag::Notice) {
			return None;
		}
		match node_text(node, PropId::Kind) {
			Some("error" | "warn") => node.content.clone(),
			_ => None,
		}
	})
}

/// Text-mode stdout: the last assistant response across the submitted
/// prompts, thinking first when requested, each block newline-terminated.
/// Intermediate assistant messages before tool calls and the tool calls
/// themselves never reach stdout (pi `print-mode.ts` text output).
fn final_response_text(dom: &Dom, first_turn: usize, print_thoughts: bool) -> String {
	let mut output = String::new();
	let Some(assistant) = dom
		.children(dom.body())
		.iter()
		.skip(first_turn)
		.rev()
		.find_map(|turn| last_assistant(dom, *turn))
	else {
		return output;
	};
	let Some(node) = dom.get(assistant) else {
		return output;
	};
	if print_thoughts
		&& let Some(thinking) = node_text(node, PropId::Thinking)
		&& !thinking.trim().is_empty()
	{
		output.push_str(thinking);
		output.push('\n');
	}
	let text = node_text(node, PropId::Text)
		.or(node.content.as_deref())
		.unwrap_or_default();
	if !text.is_empty() {
		output.push_str(text);
		output.push('\n');
	}
	output
}

fn last_assistant(dom: &Dom, turn: Handle) -> Option<Handle> {
	dom.children(turn).iter().rev().copied().find(|handle| {
		dom.get(*handle)
			.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
	})
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

/// Folds one session event into the replica; JSON mode additionally writes
/// the projected lifecycle frames. Text mode writes nothing here: its stdout
/// is the final response, written once every prompt settled.
async fn print_event(
	stdout: &mut tokio::io::Stdout,
	args: &PrintOptions,
	replica: &mut Dom,
	state: &mut JsonState,
	event: Event,
) -> miette::Result<()> {
	let values = project_print_event(args, replica, state, event)?;
	if args.mode == "json" {
		for value in values {
			write_json_line(stdout, &value).await?;
		}
	}
	Ok(())
}

fn project_print_event(
	args: &PrintOptions,
	replica: &mut Dom,
	state: &mut JsonState,
	event: Event,
) -> miette::Result<Vec<serde_json::Value>> {
	let mut values = Vec::new();
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
				Tag::Known(KnownTag::Input) => replica.parent(*node).map(PrintedStream::ToolArguments),
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
			appended = state
				.streams
				.get(sid)
				.copied()
				.map(|stream| (stream, delta.clone()));
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
			Tag::Custom(_) => {
				values.push(tool_call_update(replica, handle, "toolcall_start", "", args));
				if prop_text(replica.get(handle), PropId::Status) == Some("running") {
					let delta = serde_json::to_string(&tool_args(replica, handle))
						.unwrap_or_else(|_| "{}".to_owned());
					values.push(tool_call_update(replica, handle, "toolcall_delta", &delta, args));
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
				values.push(message_delta(replica, assistant, "text_delta", delta.as_str(), args));
			},
			PrintedStream::Thinking(assistant) => {
				values.push(message_delta(replica, assistant, "thinking_delta", delta.as_str(), args));
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
	Ok(values)
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
	args: &PrintOptions,
) -> serde_json::Value {
	let stream = serde_json::json!({"type":kind,"contentIndex":0,"delta":delta});
	shaped_message_update(dom, assistant, stream, args.shape_transcript)
}

fn tool_call_update(
	dom: &Dom,
	call: Handle,
	kind: &str,
	delta: &str,
	args: &PrintOptions,
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
	let role = if node.tag == Tag::Known(KnownTag::User) {
		"user"
	} else {
		"assistant"
	};
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
		let reason = match reason {
			"cancelled" => "aborted",
			reason => reason,
		};
		message["stopReason"] = serde_json::json!(reason);
		if matches!(reason, "error" | "aborted")
			&& let Some(turn) = dom.parent(handle)
			&& let Some(text) = turn_failure_notice(dom, turn)
		{
			message["errorMessage"] = serde_json::json!(text);
		}
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
		.and_then(|node| {
			node
				.content
				.as_deref()
				.or_else(|| node_text(node, PropId::Text))
		})
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
			.then(|| {
				node
					.content
					.as_deref()
					.or_else(|| node_text(node, PropId::Text))
			})
			.flatten()
	});
	raw.and_then(|raw| serde_json::from_str(raw).ok())
		.unwrap_or_else(|| serde_json::json!({}))
}

fn usage_value(node: Option<&Node>) -> serde_json::Value {
	let integer = |prop| {
		node
			.and_then(|node| node.prop(&PropKey::Known(prop)))
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
	node.prop(&PropKey::Known(prop)).and_then(Value::as_str)
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

/// The prompt words (a leading `/template` expanded, pi
/// `expandPromptTemplate`), else piped standard input.
async fn initial_prompt(launch: &Launch) -> miette::Result<Str> {
	if let Some(text) = launch.initial_prompt() {
		return Ok(text);
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
	use omp_dom::{NodeSpec, Txn};
	use serde_json::value::RawValue;
	use tempfile::tempdir;

	use super::*;

	fn current_turn(session: &omp_session::Session) -> Handle {
		*session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn node")
	}

	fn assistant_with(
		session: &mut omp_session::Session,
		thinking: Option<&str>,
		text: &str,
		stop: &str,
	) {
		session
			.assistant_start("test/model", "test", "test/model")
			.expect("assistant");
		let turn = current_turn(session);
		let assistant = last_assistant(session.dom(), turn).expect("assistant node");
		if let Some(thinking) = thinking {
			let sid = session
				.stream_open(assistant, PropId::Thinking.into())
				.expect("thinking stream");
			session.stream_append(sid, thinking).expect("thinking delta");
			session.stream_close(sid).expect("thinking close");
		}
		let sid = session
			.stream_open(assistant, PropId::Text.into())
			.expect("text stream");
		session.stream_append(sid, text).expect("text delta");
		session.stream_close(sid).expect("text close");
		session.assistant_end(stop).expect("assistant end");
	}

	fn settled_call(session: &mut omp_session::Session, name: &str) {
		let call = session
			.call(
				name,
				1,
				"call-1",
				None,
				Some(RawValue::from_string(r#"{"path":"note.txt"}"#.to_owned()).expect("args")),
				None,
			)
			.expect("call");
		session
			.settle(
				call,
				RawValue::from_string(
					r#"{"content":[{"type":"text","text":"hello from fixture"}]}"#.to_owned(),
				)
				.expect("outcome"),
			)
			.expect("settle");
	}

	fn error_notice(session: &mut omp_session::Session, text: &str) {
		let turn = current_turn(session);
		let after = session.dom().children(turn).last().copied();
		let cause = session.head().expect("head");
		session
			.patch(Txn {
				cause,
				label: Some(Str::new_static("kernel.notice")),
				ops: vec![Op::Ins {
					parent: turn,
					after,
					node: NodeSpec::new(KnownTag::Notice)
						.with_prop(PropId::Kind, Value::Str(Str::new_static("error")))
						.with_content(Str::new(text)),
				}],
			})
			.expect("notice");
	}

	#[test]
	fn text_mode_stdout_is_only_the_final_response() {
		let scratch = tempdir().expect("scratch");
		let mut session = omp_session::Session::create(
			scratch.path().join("text.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("session");
		session.begin_turn().expect("turn");
		session.user("read note.txt", Vec::new()).expect("user");
		assistant_with(&mut session, None, "Let me read that file.", "tool_calls");
		settled_call(&mut session, "read");
		assistant_with(
			&mut session,
			Some("The file says hello."),
			"hello from fixture",
			"stream_closed",
		);

		assert_eq!(
			final_response_text(session.dom(), 0, false),
			"hello from fixture\n",
			"intermediate assistant text and tool markers must never reach stdout",
		);
		assert_eq!(
			final_response_text(session.dom(), 0, true),
			"The file says hello.\nhello from fixture\n",
		);
		assert_eq!(final_response_text(session.dom(), 1, false), "");
	}

	#[test]
	fn failed_turn_agent_end_carries_stop_reason_and_error_message() {
		let scratch = tempdir().expect("scratch");
		let mut session = omp_session::Session::create(
			scratch.path().join("failed.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("session");
		session.begin_turn().expect("turn");
		session.user("hi", Vec::new()).expect("user");
		assistant_with(&mut session, None, "partial", "error");
		error_notice(&mut session, "provider exploded: http 500");

		let end = agent_end_value(session.dom(), 0);
		assert_eq!(end["type"], "agent_end");
		assert_eq!(end["isTerminal"], true);
		let assistant = end["messages"]
			.as_array()
			.expect("messages")
			.iter()
			.rev()
			.find(|message| message["role"] == "assistant")
			.expect("terminal assistant");
		assert_eq!(assistant["stopReason"], "error");
		assert_eq!(assistant["errorMessage"], "provider exploded: http 500");
		assert_eq!(
			turn_error_message(session.dom(), 0).as_deref(),
			Some("provider exploded: http 500"),
		);
		assert_eq!(turn_error_message(session.dom(), 1), None);
	}

	#[test]
	fn interrupted_assistant_reports_pi_aborted_stop_reason() {
		let scratch = tempdir().expect("scratch");
		let mut session = omp_session::Session::create(
			scratch.path().join("aborted.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("session");
		session.begin_turn().expect("turn");
		assistant_with(&mut session, None, "part", "cancelled");
		let turn = current_turn(&session);
		let assistant = last_assistant(session.dom(), turn).expect("assistant");
		let message = message_value(session.dom(), assistant);
		assert_eq!(message["stopReason"], "aborted");
		assert!(message.get("errorMessage").is_none());
		assert_eq!(stop_reason_name(TurnStop::Cancelled), "aborted");
		assert_eq!(stop_reason_name(TurnStop::Failed), "error");
	}

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
		let turn = *session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn node");
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
				RawValue::from_string(r#"{"content":[{"type":"text","text":"ok"}]}"#.to_owned())
					.expect("outcome"),
			)
			.expect("settle");
		let turn = *session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn node");
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
		assert!(
			messages
				.iter()
				.any(|message| message["role"] == "assistant")
		);
		assert!(
			messages
				.iter()
				.any(|message| message["role"] == "toolResult")
		);
	}
}
