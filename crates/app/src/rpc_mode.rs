//! Stateful JSON-line RPC actor over the journal-first kernel and session DOM.

use std::{
	collections::{BTreeMap, HashSet},
	fs,
	future::Future,
	path::Path,
	pin::Pin,
	sync::Arc,
};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{
	Inference, Kernel, KernelError, KernelEvent, RunControl, TurnInput, TurnOutcome, TurnStop, Up,
};
use omp_core::Str;
use omp_dom::{Dom, Event};
use omp_driver::headless::kernel::SessionHome;
use omp_rpc::{
	framing::{
		JsonLineDecoder, MAX_FRAME_BYTES, MAX_REASSEMBLED_BYTES, RpcFrameDecoder, encode_json_v1,
		encode_json_v2,
	},
	protocol::{
		PROTOCOL_V1, PROTOCOL_V2, ReadyFrame, RequestId, RpcErrorCode, RpcRequest, RpcResponse,
	},
};
use omp_session::Session;
use omp_tools::ask::{Answer, AskPresenter, Fault as AskFault, Presentation, Question};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, stdin, stdout};

use crate::{
	chat_cmd::{Launch, LaunchEnv},
	cli::{ChatArgs, RpcArgs},
};

/// Runs the RPC server using stdin exclusively for protocol input and stdout
/// exclusively for protocol output.
pub async fn run(args: RpcArgs, ui_enabled: bool) -> miette::Result<()> {
	let max_time = args.max_time.map(|duration| duration.0);
	let future = run_inner(args.launch, ui_enabled);
	match max_time {
		Some(limit) => tokio::time::timeout(limit, future)
			.await
			.map_err(|_| miette!("RPC mode exceeded --max-time"))?,
		None => future.await,
	}
}

async fn run_inner(args: ChatArgs, ui_enabled: bool) -> miette::Result<()> {
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
	let ui = ui_enabled.then(RpcUiBridge::new);
	if let Some(ui) = &ui {
		kernel
			.inference()
			.environment()
			.bind_ask_presenter(Arc::new(ui.clone()));
	}
	serve_rpc(kernel, session, home, ui, stdin(), stdout()).await
}

/// Remote retained-dialog bridge enabled by `rpc-ui`.
///
/// The environment's `ask@1` presenter emits ordinary
/// `extension_ui_request` frames and waits for correlated
/// `extension_ui_response` input. Plain `rpc` never installs this presenter.
#[doc(hidden)]
#[derive(Clone)]
pub struct RpcUiBridge {
	inner: Arc<RpcUiInner>,
}

struct RpcUiInner {
	requests_tx: flume::Sender<Value>,
	requests_rx: flume::Receiver<Value>,
	pending:     Mutex<BTreeMap<String, flume::Sender<Map<String, Value>>>>,
}

struct PendingUiReply {
	bridge: RpcUiBridge,
	id:     String,
}

impl Drop for PendingUiReply {
	fn drop(&mut self) {
		self.bridge.inner.pending.lock().remove(&self.id);
	}
}

impl RpcUiBridge {
	/// Creates an unattached retained-dialog bridge.
	#[doc(hidden)]
	#[must_use]
	pub fn new() -> Self {
		let (requests_tx, requests_rx) = flume::unbounded();
		Self {
			inner: Arc::new(RpcUiInner {
				requests_tx,
				requests_rx,
				pending: Mutex::new(BTreeMap::new()),
			}),
		}
	}

	fn requests(&self) -> flume::Receiver<Value> {
		self.inner.requests_rx.clone()
	}

	fn respond(&self, id: &str, params: Map<String, Value>) -> bool {
		let Some(sender) = self.inner.pending.lock().remove(id) else {
			return false;
		};
		sender.try_send(params).is_ok()
	}
}

impl Default for RpcUiBridge {
	fn default() -> Self {
		Self::new()
	}
}

impl AskPresenter for RpcUiBridge {
	fn present<'p>(
		&'p self,
		questions: &'p [Question],
		invocation: Option<&'p str>,
	) -> Pin<Box<dyn Future<Output = Result<Presentation, AskFault>> + Send + 'p>> {
		let bridge = self.clone();
		let questions = questions.to_vec();
		let invocation = invocation.map(str::to_owned);
		Box::pin(async move {
			let Some(invocation) = invocation else {
				return Err(AskFault::Presenter {
					message: Str::new_static("RPC UI ask requires a call identity"),
				});
			};
			let mut answers = Vec::with_capacity(questions.len());
			for (index, question) in questions.iter().enumerate() {
				let id = format!("{invocation}:{index}");
				let (reply_tx, reply_rx) = flume::bounded(1);
				bridge.inner.pending.lock().insert(id.clone(), reply_tx);
				let pending = PendingUiReply { bridge: bridge.clone(), id: id.clone() };
				let options = question
					.options
					.iter()
					.map(|option| option.label.as_str())
					.collect::<Vec<_>>();
				let option_details = question
					.options
					.iter()
					.map(|option| json!({ "description": option.description }))
					.collect::<Vec<_>>();
				let request = json!({
					"type": "extension_ui_request",
					"id": id,
					"method": "select",
					"title": question.question,
					"options": options,
					"optionDetails": option_details,
					"multi": question.multi,
					"recommended": question.recommended,
				});
				if bridge.inner.requests_tx.send_async(request).await.is_err() {
					return Err(AskFault::Presenter {
						message: Str::new_static("RPC UI host went away before showing ask"),
					});
				}
				let fields = reply_rx
					.recv_async()
					.await
					.map_err(|_| AskFault::Presenter {
						message: Str::new_static("RPC UI host went away before answering ask"),
					})?;
				drop(pending);
				if fields.get("cancelled").and_then(Value::as_bool) == Some(true) {
					return Err(AskFault::cancelled());
				}
				let selected = selected_values(&fields);
				if selected.iter().any(|selected| {
					!question
						.options
						.iter()
						.any(|option| option.label.as_str() == selected.as_str())
				}) {
					return Err(AskFault::Presenter {
						message: Str::new_static("RPC UI host returned an unknown ask option"),
					});
				}
				answers.push(Answer {
					id: question.id.clone(),
					selected,
					custom_input: fields
						.get("customInput")
						.and_then(Value::as_str)
						.map(Str::new),
					note: fields.get("note").and_then(Value::as_str).map(Str::new),
					timed_out: false,
				});
			}
			Ok(Presentation { answers, headless: false })
		})
	}
}

fn selected_values(fields: &Map<String, Value>) -> Vec<Str> {
	if let Some(values) = fields.get("values").and_then(Value::as_array) {
		return values
			.iter()
			.filter_map(Value::as_str)
			.map(Str::new)
			.collect();
	}
	fields
		.get("value")
		.and_then(Value::as_str)
		.map_or_else(Vec::new, |value| vec![Str::new(value)])
}

enum Incoming {
	Request(RpcRequest),
	Error(Value),
	End { truncated: bool },
}

enum Outgoing {
	Frame(Value),
	Negotiated { frame: Value, protocol: u8 },
}

/// What a spawned turn hands back: the kernel and session it borrowed plus
/// the turn's result.
type TurnCompletion<C> = (Kernel<C>, Session, Result<TurnOutcome, KernelError>);

/// Moves the idle kernel and session into a spawned turn (`prompt`, an idle
/// `follow_up`, `abort_and_prompt`, and the follow-up pop after a turn all
/// start turns through this one path) and announces `turn_start`.
fn start_turn<C>(
	current: &mut Option<(Kernel<C>, Session)>,
	turn_tx: &flume::Sender<TurnCompletion<C>>,
	outgoing_tx: &flume::Sender<Outgoing>,
	input: TurnInput,
) -> miette::Result<()>
where
	C: Inference + Send + Sync + 'static,
{
	let (mut kernel, mut session) = current.take().expect("idle RPC owns kernel and session");
	let turn_tx = turn_tx.clone();
	drop(tokio::spawn(async move {
		let result = kernel
			.run_turn(&mut session, input, RunControl::default())
			.await;
		let _ = turn_tx.send_async((kernel, session, result)).await;
	}));
	outgoing_tx
		.send(Outgoing::Frame(json!({ "type": "turn_start" })))
		.into_diagnostic()
}

/// Serves RPC over caller-provided transport halves.
///
/// Exposed for joined scripted-kernel transport proofs. Production passes
/// stdio and a [`SessionHome`]; tests pass an in-memory duplex stream through
/// this exact path.
#[doc(hidden)]
pub async fn serve_rpc<C, R, W>(
	mut kernel: Kernel<C>,
	mut session: Session,
	home: SessionHome,
	ui: Option<RpcUiBridge>,
	mut input: R,
	mut output: W,
) -> miette::Result<()>
where
	C: Inference + Send + Sync + 'static,
	R: AsyncRead + Unpin + Send + 'static,
	W: AsyncWrite + Unpin + Send + 'static,
{
	let (outgoing_tx, outgoing_rx) = flume::unbounded::<Outgoing>();
	let writer = tokio::spawn(async move {
		let mut protocol = PROTOCOL_V1;
		let streamed = HashSet::<String>::new();
		while let Ok(message) = outgoing_rx.recv_async().await {
			let (value, negotiated) = match message {
				Outgoing::Frame(value) => (value, None),
				Outgoing::Negotiated { frame, protocol } => (frame, Some(protocol)),
			};
			let frames = if protocol == PROTOCOL_V2 {
				encode_json_v2(&value, "server").map_err(|source| miette!(source))?
			} else {
				vec![encode_json_v1(&value, &streamed)]
			};
			for bytes in frames {
				output.write_all(&bytes).await.into_diagnostic()?;
			}
			output.flush().await.into_diagnostic()?;
			if let Some(next) = negotiated {
				protocol = next;
			}
		}
		Ok::<(), miette::Report>(())
	});
	outgoing_tx
		.send(Outgoing::Frame(
			serde_json::to_value(ReadyFrame::v2_capable(MAX_FRAME_BYTES, MAX_REASSEMBLED_BYTES))
				.into_diagnostic()?,
		))
		.into_diagnostic()?;

	let (snapshot, mut dom_events) = session.subscribe();
	// The actor's own projection of the session tree (ADR 0005): `get_state`
	// answers from it at any time, including while a turn owns the session.
	let mut replica = Dom::from_snapshot(&snapshot);
	outgoing_tx
		.send(Outgoing::Frame(json!({
			"type": "snapshot",
			"snapshot": serde_json::from_slice::<Value>(snapshot.as_bytes()).into_diagnostic()?,
		})))
		.into_diagnostic()?;
	let kernel_events = kernel.subscribe();
	let mailbox = kernel.mailbox();

	let (incoming_tx, incoming_rx) = flume::unbounded();
	let input_task = tokio::spawn(async move {
		let mut lines = JsonLineDecoder::new();
		let mut logical = RpcFrameDecoder::new();
		let mut logical_pending = false;
		let mut buffer = [0_u8; 16 * 1024];
		loop {
			let count = match input.read(&mut buffer).await {
				Ok(count) => count,
				Err(source) => {
					let _ = incoming_tx
						.send_async(Incoming::Error(error_frame(
							None,
							"transport",
							"io_error",
							&source.to_string(),
						)))
						.await;
					break;
				},
			};
			if count == 0 {
				let _ = incoming_tx
					.send_async(Incoming::End {
						truncated: !lines.remainder().is_empty() || logical_pending,
					})
					.await;
				break;
			}
			let batch = lines.push(&buffer[..count]);
			for diagnostic in batch.diagnostics {
				let _ = incoming_tx
					.send_async(Incoming::Error(error_frame(
						None,
						"transport",
						"invalid_frame",
						diagnostic.reason,
					)))
					.await;
			}
			for bytes in batch.frames {
				let value = match logical.push_frame(&bytes) {
					Ok(Some(value)) => {
						logical_pending = false;
						value
					},
					Ok(None) => {
						logical_pending = true;
						continue;
					},
					Err(source) => {
						logical.reset();
						logical_pending = false;
						let _ = incoming_tx
							.send_async(Incoming::Error(error_frame(
								None,
								"transport",
								"invalid_frame",
								&source.to_string(),
							)))
							.await;
						continue;
					},
				};
				match serde_json::from_value::<RpcRequest>(value) {
					Ok(request) => {
						if incoming_tx
							.send_async(Incoming::Request(request))
							.await
							.is_err()
						{
							return;
						}
					},
					Err(source) => {
						let _ = incoming_tx
							.send_async(Incoming::Error(error_frame(
								None,
								"parse",
								"invalid_request",
								&source.to_string(),
							)))
							.await;
					},
				}
			}
		}
	});

	let ui_requests = ui.as_ref().map(RpcUiBridge::requests);
	let (turn_tx, turn_rx) = flume::unbounded::<TurnCompletion<C>>();
	let mut current = Some((kernel, session));
	let mut turn_running = false;
	// `abort_and_prompt` while a turn runs: the interrupt is sent now and the
	// prompt starts the moment the aborted turn hands the session back.
	let mut abort_prompt: Option<TurnInput> = None;
	// `cancel` kills the session scope (ADR 0011): no further turn can run,
	// so queued follow-ups stay journaled for a later resume instead of
	// being popped into immediately-cancelled turns.
	let mut session_cancelled = false;
	let mut input_open = true;
	let mut dom_open = true;
	let mut kernel_open = true;
	let mut ui_open = ui_requests.is_some();
	let mut shutting_down = false;

	loop {
		tokio::select! {
			incoming = incoming_rx.recv_async(), if input_open && !shutting_down => {
				match incoming {
					Ok(Incoming::Error(frame)) => {
						outgoing_tx.send(Outgoing::Frame(frame)).into_diagnostic()?;
					},
					Ok(Incoming::End { truncated }) => {
						input_open = false;
						if truncated {
							outgoing_tx.send(Outgoing::Frame(error_frame(
								None,
								"transport",
								"truncated_frame",
								"input ended mid-frame",
							))).into_diagnostic()?;
						}
						if turn_running {
							let _ = mailbox.send(Up::Cancel);
							shutting_down = true;
						} else {
							break;
						}
					},
					Err(_) => {
						input_open = false;
						if turn_running {
							let _ = mailbox.send(Up::Cancel);
							shutting_down = true;
						} else {
							break;
						}
					},
					Ok(Incoming::Request(request)) => {
						let id = request.id.clone();
						let command = request.command.clone();
						match command.as_str() {
							"negotiate_protocol" => {
								let response = negotiate(id, &request.params);
								let protocol = request.params
									.get("protocolVersion")
									.and_then(Value::as_u64)
									.and_then(|value| u8::try_from(value).ok())
									.filter(|value| matches!(*value, PROTOCOL_V1 | PROTOCOL_V2));
								let frame = serde_json::to_value(response).into_diagnostic()?;
								match protocol {
									Some(protocol) => outgoing_tx.send(Outgoing::Negotiated { frame, protocol }).into_diagnostic()?,
									None => outgoing_tx.send(Outgoing::Frame(frame)).into_diagnostic()?,
								}
							},
							"prompt" => {
								let response = if turn_running {
									busy_response(id, command.as_str())
								} else {
									match message_text(&request.params) {
										Some(text) => {
											start_turn(&mut current, &turn_tx, &outgoing_tx, text_input(text))?;
											turn_running = true;
											RpcResponse::success(id, command.as_str(), json!({ "accepted": true })).into_diagnostic()?
										},
										None => missing_message(id, command.as_str()),
									}
								};
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							"steer" => {
								let response = up_response(id, command.as_str(), &request.params, &mailbox, |text| Up::Steer {
									text,
									attachments: Vec::new(),
								});
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							// pi `followUp`: behind a running turn the prompt is
							// journaled into `<queues><prompts>` and popped when the
							// turn yields; idle, it runs now (pi's idle queue drain).
							"follow_up" => {
								let response = if turn_running {
									up_response(id, command.as_str(), &request.params, &mailbox, |text| Up::Queue {
										text,
										attachments: Vec::new(),
									})
								} else {
									match message_text(&request.params) {
										Some(text) => {
											start_turn(&mut current, &turn_tx, &outgoing_tx, text_input(text))?;
											turn_running = true;
											RpcResponse::success(id, command.as_str(), json!({ "queued": false })).into_diagnostic()?
										},
										None => missing_message(id, command.as_str()),
									}
								};
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							// pi `abort_and_prompt`: interrupt the running turn, then
							// prompt; the response acknowledges the abort and the new
							// turn's events stream after it.
							"abort_and_prompt" => {
								let response = match message_text(&request.params) {
									Some(text) => {
										let input = text_input(text);
										if turn_running {
											abort_prompt = Some(input);
											let _ = mailbox.send(Up::Interrupt);
										} else {
											start_turn(&mut current, &turn_tx, &outgoing_tx, input)?;
											turn_running = true;
										}
										RpcResponse::success_empty(id, command.as_str())
									},
									None => missing_message(id, command.as_str()),
								};
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							"approve" => {
								let response = approve_response(id, command.as_str(), &request.params, &mailbox);
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							"interrupt" | "abort" => {
								let _ = mailbox.send(Up::Interrupt);
								let response = RpcResponse::success_empty(id, command.as_str());
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							"cancel" => {
								let _ = mailbox.send(Up::Cancel);
								session_cancelled = true;
								let response = RpcResponse::success_empty(id, command.as_str());
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							"extension_ui_response" => {
								let answered = ui.as_ref().is_some_and(|ui| {
									request.id.as_ref().is_some_and(|id| ui.respond(id.as_str(), request.params))
								});
								if !answered {
									outgoing_tx.send(Outgoing::Frame(error_frame(
										id,
										command.as_str(),
										"invalid_request",
										"no matching RPC UI request",
									))).into_diagnostic()?;
								}
							},
							// pi answers `get_state` while streaming (`isStreaming`
							// is part of the state); the replica projects the tree
							// whether or not a turn owns the session.
							"get_state" => {
								let response = RpcResponse::success(
									id,
									command.as_str(),
									serde_json::from_slice::<Value>(replica.snapshot().as_bytes()).into_diagnostic()?,
								).into_diagnostic()?;
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							"new_session" | "switch_session" | "branch" => {
								let response = if turn_running {
									busy_response(id, command.as_str())
								} else {
									let (idle_kernel, mut old) = current.take().expect("idle RPC owns session");
									let transition = match idle_kernel.flush_session_state(&mut old) {
										Ok(()) => transition_session(&home, old, command.as_str(), &request.params),
										Err(source) => Err((source.to_string(), old)),
									};
									match transition {
										Ok(mut next) => {
											idle_kernel.resync_session_state(&next);
											let (snapshot, events) = next.subscribe();
											dom_events = events;
											dom_open = true;
											replica = Dom::from_snapshot(&snapshot);
											let session_path = next.journal_path().to_path_buf();
											current = Some((idle_kernel, next));
											outgoing_tx.send(Outgoing::Frame(json!({
												"type": "snapshot",
												"snapshot": serde_json::from_slice::<Value>(snapshot.as_bytes()).into_diagnostic()?,
											}))).into_diagnostic()?;
											RpcResponse::success(id, command.as_str(), json!({
												"cancelled": false,
												"sessionPath": session_path,
											})).into_diagnostic()?
										},
										Err((source, old)) => {
											current = Some((idle_kernel, old));
											RpcResponse::error(id, command.as_str(), source, Some(RpcErrorCode::new("session_error")))
										},
									}
								};
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
							"quit" | "shutdown" => {
								let response = RpcResponse::success_empty(id, command.as_str());
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
								if turn_running {
									let _ = mailbox.send(Up::Cancel);
									shutting_down = true;
								} else {
									break;
								}
							},
							_ => {
								let response = RpcResponse::error(
									id,
									command.as_str(),
									"unknown RPC command",
									Some(RpcErrorCode::new("unknown_command")),
								);
								outgoing_tx.send(Outgoing::Frame(serde_json::to_value(response).into_diagnostic()?)).into_diagnostic()?;
							},
						}
					},
				}
			},
			completed = turn_rx.recv_async(), if turn_running => {
				let (turn_kernel, mut turn_session, result) = completed.into_diagnostic()?;
				while let Ok(event) = dom_events.try_recv() {
					replica.apply_event(&event).into_diagnostic()?;
					outgoing_tx.send(Outgoing::Frame(dom_event_value(event)?)).into_diagnostic()?;
				}
				while let Ok(event) = kernel_events.try_recv() {
					if let Some(value) = kernel_event_value(event) {
						outgoing_tx.send(Outgoing::Frame(value)).into_diagnostic()?;
					}
				}
				let terminal = match result {
					Ok(outcome) => json!({
						"type": "agent_end",
						"messages": [],
						"cancelled": outcome.stop == TurnStop::Cancelled,
						"steered": outcome.stop == TurnStop::Steered,
						"text": outcome.assistant_text,
						"tokensIn": outcome.tokens_in,
						"tokensOut": outcome.tokens_out,
					}),
					Err(source) => json!({
						"type": "agent_end",
						"messages": [],
						"cancelled": false,
						"error": source.to_string(),
					}),
				};
				outgoing_tx.send(Outgoing::Frame(terminal)).into_diagnostic()?;
				if shutting_down || !input_open {
					current = Some((turn_kernel, turn_session));
					break;
				}
				// The aborted-then-prompted turn outranks the follow-up queue;
				// otherwise the oldest queued follow-up runs now that the
				// agent yielded (pi `followUp`).
				let next = match abort_prompt.take() {
					Some(input) => Some(input),
					None if session_cancelled => None,
					None => omp_agent::pop_queued_prompt(&mut turn_session)
						.into_diagnostic()?
						.map(|(text, attachments)| TurnInput { text, attachments }),
				};
				current = Some((turn_kernel, turn_session));
				match next {
					Some(input) => start_turn(&mut current, &turn_tx, &outgoing_tx, input)?,
					None => turn_running = false,
				}
			},
			event = dom_events.recv_async(), if dom_open => {
				match event {
					Ok(event) => {
						replica.apply_event(&event).into_diagnostic()?;
						outgoing_tx.send(Outgoing::Frame(dom_event_value(event)?)).into_diagnostic()?;
					},
					Err(_) => dom_open = false,
				}
			},
			event = kernel_events.recv_async(), if kernel_open => {
				match event {
					Ok(event) => {
						if let Some(value) = kernel_event_value(event) {
							outgoing_tx.send(Outgoing::Frame(value)).into_diagnostic()?;
						}
					},
					Err(_) => kernel_open = false,
				}
			},
			request = async {
				match &ui_requests {
					Some(requests) => requests.recv_async().await,
					None => std::future::pending().await,
				}
			}, if ui_open => {
				match request {
					Ok(request) => outgoing_tx.send(Outgoing::Frame(request)).into_diagnostic()?,
					Err(_) => ui_open = false,
				}
			},
		}
	}

	input_task.abort();
	let _ = input_task.await;
	let (kernel, mut session) = current.expect("RPC shutdown waits for active turn");
	kernel.flush_session_state(&mut session).into_diagnostic()?;
	session.process_exit().into_diagnostic()?;
	while let Ok(event) = dom_events.try_recv() {
		outgoing_tx
			.send(Outgoing::Frame(dom_event_value(event)?))
			.into_diagnostic()?;
	}
	drop(session);
	drop(outgoing_tx);
	writer.await.into_diagnostic()??;
	Ok(())
}

fn transition_session(
	home: &SessionHome,
	mut old: Session,
	command: &str,
	params: &Map<String, Value>,
) -> Result<Session, (String, Session)> {
	let result: Result<Session, String> = match command {
		"new_session" => home.create(None).map_err(|source| source.to_string()),
		"switch_session" => {
			let Some(path) = params.get("sessionPath").and_then(Value::as_str) else {
				return Err(("switch_session requires `sessionPath`".into(), old));
			};
			home
				.open(Path::new(path))
				.map_err(|source| source.to_string())
		},
		"branch" => {
			let Some(entry) = params.get("entryId").and_then(Value::as_str) else {
				return Err(("branch requires `entryId`".into(), old));
			};
			let target: omp_journal::EntryId = match entry.parse() {
				Ok(target) => target,
				Err(source) => return Err((source.to_string(), old)),
			};
			let source_path = old.journal_path().to_path_buf();
			match home.fork(&source_path) {
				Ok(mut next) => match next.rewind(target) {
					Ok(_) => Ok(next),
					Err(source) => {
						let path = next.journal_path().to_path_buf();
						home.unregister(&next);
						drop(next);
						let _ = fs::remove_file(path);
						Err(source.to_string())
					},
				},
				Err(source) => Err(source.to_string()),
			}
		},
		_ => unreachable!("session transition command is matched by caller"),
	};
	match result {
		Ok(next) => {
			if let Err(source) = old.session_switch() {
				home.unregister(&next);
				return Err((source.to_string(), old));
			}
			home.unregister(&old);
			Ok(next)
		},
		Err(source) => Err((source, old)),
	}
}

fn busy_response(id: Option<RequestId>, command: &str) -> RpcResponse {
	RpcResponse::error(
		id,
		command,
		"another RPC operation is active",
		Some(RpcErrorCode::new(RpcErrorCode::SESSION_BUSY)),
	)
}

fn negotiate(id: Option<RequestId>, params: &Map<String, Value>) -> RpcResponse {
	let version = params.get("protocolVersion").and_then(Value::as_u64);
	if matches!(version, Some(value) if value == u64::from(PROTOCOL_V1) || value == u64::from(PROTOCOL_V2))
	{
		RpcResponse::success(id, "negotiate_protocol", json!({ "protocolVersion": version }))
			.expect("static protocol response serializes")
	} else {
		RpcResponse::error(
			id,
			"negotiate_protocol",
			"only protocol versions 1 and 2 are supported",
			Some(RpcErrorCode::new(RpcErrorCode::UNSUPPORTED_PROTOCOL)),
		)
	}
}

/// The prompt text of a `prompt`/`steer`/`follow_up`/`abort_and_prompt`
/// request (`message`, or the legacy `text`).
fn message_text(params: &Map<String, Value>) -> Option<&str> {
	params
		.get("message")
		.or_else(|| params.get("text"))
		.and_then(Value::as_str)
}

fn text_input(text: &str) -> TurnInput {
	TurnInput { text: Str::new(text), attachments: Vec::new() }
}

fn missing_message(id: Option<RequestId>, command: &str) -> RpcResponse {
	RpcResponse::error(
		id,
		command,
		format!("{command} requires `message` or `text`"),
		Some(RpcErrorCode::new("invalid_params")),
	)
}

/// Sends the request's message to the running turn through `up` and reports
/// it queued (`steer` → [`Up::Steer`], `follow_up` → [`Up::Queue`]).
fn up_response(
	id: Option<RequestId>,
	command: &str,
	params: &Map<String, Value>,
	mailbox: &flume::Sender<Up>,
	up: impl FnOnce(Str) -> Up,
) -> RpcResponse {
	match message_text(params) {
		Some(text) => {
			let _ = mailbox.send(up(Str::new(text)));
			RpcResponse::success(id, command, json!({ "queued": true }))
				.expect("static queue response serializes")
		},
		None => missing_message(id, command),
	}
}

fn kernel_event_value(event: KernelEvent) -> Option<Value> {
	match event {
		KernelEvent::InferenceStarted => Some(json!({ "type": "agent_start" })),
		KernelEvent::InferenceRetry { attempt, max_attempts, delay, reason } => Some(json!({
			"type": "auto_retry_start",
			"attempt": attempt,
			"maxAttempts": max_attempts,
			"delayMs": delay.as_millis(),
			"reason": reason,
		})),
		KernelEvent::Usage { output_tokens, reasoning_tokens } => Some(json!({
			"type": "message_update",
			"usage": { "outputTokens": output_tokens, "reasoningTokens": reasoning_tokens },
		})),
		KernelEvent::TextDelta(text) => Some(json!({
			"type": "message_update",
			"delta": { "type": "text_delta", "text": text },
		})),
		KernelEvent::ThinkingDelta(text) => Some(json!({
			"type": "message_update",
			"delta": { "type": "thinking_delta", "text": text },
		})),
		KernelEvent::ToolReady { call_id, name } => Some(json!({
			"type": "tool_execution_start",
			"toolCallId": call_id,
			"toolName": name,
		})),
		KernelEvent::ToolUpdate { call_id } => Some(json!({
			"type": "tool_execution_update",
			"toolCallId": call_id,
		})),
		KernelEvent::ToolSettled { call_id, is_error } => Some(json!({
			"type": "tool_execution_end",
			"toolCallId": call_id,
			"isError": is_error,
		})),
		KernelEvent::CompactionSpeculating { percent } => Some(json!({
			"type": "auto_compaction_start",
			"percent": percent,
		})),
		KernelEvent::CompactionSettled { applied } => Some(json!({
			"type": "auto_compaction_end",
			"applied": applied,
		})),
		KernelEvent::JobsDelivered { ids } => Some(json!({
			"type": "async_result",
			"jobIds": ids,
		})),
		KernelEvent::WorkflowActionAnswered { invocation, name, is_error } => Some(json!({
			"type": "workflow_action_end",
			"invocation": invocation,
			"toolName": name,
			"isError": is_error,
		})),
		// pi surfaces the wrapper's approval `select` as an extension UI
		// request; the journal-first host names the durable prompt so the
		// client answers with `approve`.
		KernelEvent::ApprovalRequested(ticket) => {
			let first = ticket.reasons.first();
			Some(json!({
				"type": "tool_approval_request",
				"promptId": ticket.ticket_id,
				"toolCallId": ticket.invocation_id,
				"title": first.map(|spec| spec.title.as_str()),
				"body": first.map(|spec| spec.body.as_str()),
				"subject": first.map(|spec| spec.subject.as_str()),
				"kind": first.map(|spec| spec.kind.as_str()),
				"scopes": first.map(|spec| spec.scopes.clone()),
				"timeoutMs": first.map(|spec| spec.timeout_ms),
			}))
		},
		KernelEvent::TurnEnded { .. } => None,
	}
}

/// `approve {promptId, approved, scope?, reason?}` → [`Up::Approve`].
fn approve_response(
	id: Option<RequestId>,
	command: &str,
	params: &Map<String, Value>,
	mailbox: &flume::Sender<Up>,
) -> RpcResponse {
	let Some(prompt_id) = params
		.get("promptId")
		.or_else(|| params.get("id"))
		.and_then(Value::as_str)
	else {
		return RpcResponse::error(
			id,
			command,
			"approve requires `promptId`",
			Some(RpcErrorCode::new("invalid_params")),
		);
	};
	let approved = params
		.get("approved")
		.and_then(Value::as_bool)
		.unwrap_or(false);
	let scope = params
		.get("scope")
		.and_then(Value::as_str)
		.unwrap_or("once")
		.parse::<omp_agent::ApprovalScope>()
		.expect("approval scope parsing is infallible");
	let _ = mailbox.send(Up::Approve {
		id:       Str::new(prompt_id),
		decision: omp_agent::ApprovalDecision {
			approved,
			scope,
			source: omp_agent::ApprovalSource::External,
			decided_by: None,
			reason: params.get("reason").and_then(Value::as_str).map(Str::new),
			audited: false,
		},
	});
	RpcResponse::success(id, command, json!({ "queued": true }))
		.expect("static approval response serializes")
}

fn dom_event_value(event: Event) -> miette::Result<Value> {
	match event {
		Event::Patch(patch) => Ok(json!({
			"type": "session_event",
			"event": "patch@1",
			"data": serde_json::to_value(patch).into_diagnostic()?,
		})),
		Event::Reset { snapshot } => Ok(json!({
			"type": "snapshot",
			"snapshot": serde_json::from_slice::<Value>(snapshot.as_bytes()).into_diagnostic()?,
		})),
		Event::Stream { cause, sid, op, node, prop, text } => Ok(json!({
			"type": "session_event",
			"event": "stream@1",
			"data": {
				"cause": cause,
				"sid": sid,
				"op": op,
				"node": node,
				"prop": prop,
				"text": text,
			},
		})),
	}
}

fn error_frame(id: Option<RequestId>, command: &str, code: &str, message: &str) -> Value {
	serde_json::to_value(RpcResponse::error(id, command, message, Some(RpcErrorCode::new(code))))
		.expect("RPC error envelope serializes")
}
