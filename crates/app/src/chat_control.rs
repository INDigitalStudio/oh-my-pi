//! Application controller behind the interactive chat host: the one owner
//! of the `Session` and the kernel. It turns [`HostCommand`]s into
//! journal writes (ADR 0005: the host is a projection; the controller is
//! the actor that mutates), runs turns, and swaps sessions in place for
//! `/new`, `/resume`, `/fork`, `/drop`, and rewinds.
//!
//! Session switches keep the host alive: every session's DOM subscription
//! is relayed onto the host's one `dom_events` channel, and a switch
//! publishes exactly one [`Event::Reset`] carrying the new snapshot.

use std::{
	fs,
	path::PathBuf,
	sync::Arc,
	time::{SystemTime, UNIX_EPOCH},
};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{Kernel, LifecycleHooks, TurnInput, Up};
use omp_chat::{
	HostAction, HostCommand, HostMailbox,
	commands::{CompactionMethod, ShakeMode, TodoOp},
	host::SpawnKind,
	overlays::{
		Outcome,
		git::GitOutcome,
		hub::AgentOutcome,
		services::{Mutation, Mutations, ServiceError, ServiceOutcome},
		sessions::SessionIndexOutcome,
	},
};
use omp_con::{Ctx, Severity};
use omp_core::{Str, Ulid};
use omp_dom::{Event, Handle, KnownTag, NodeSpec, Op, PropId, PropKey, Tag, Txn, Value};
use omp_driver::headless::kernel::{ComposedInference, KernelOptions, SessionHome};
use omp_journal::EntryId;
use omp_proto::toolhost::v1::HookEventId;
use omp_session::{Session, SessionError, components::jobs};
use parking_lot::RwLock;

/// pi `background-tan-dispatch.md`.
const TAN_DISPATCH: &str = include_str!("../../chat/prompts/background-tan-dispatch.md");
/// pi `tan-context-switch.md`.
const TAN_CONTEXT: &str = include_str!("../../chat/prompts/tan-context-switch.md");
/// `<prompt kind>` of a `/queue` entry under `<queues><prompts>`.
const QUEUED: &str = "queued";

/// A lifecycle transform requested behavior the controller cannot perform.
#[derive(Debug, thiserror::Error)]
enum SessionHookError {
	/// The hook transformed a field without a corresponding runtime operation.
	#[error("hook {event:?} transformed unsupported field {field}")]
	UnsupportedTransform {
		/// Lifecycle event whose output requested the operation.
		event: HookEventId,
		/// Mutable field without an implementation.
		field: &'static str,
	},
}

/// What the idle loop does next after one command.
enum Flow {
	/// Keep waiting for commands.
	Idle,
	/// Run this turn.
	Turn(TurnInput),
	/// Run one tool without inference (`!` / `$` prefix modes).
	Local(omp_agent::LocalRun),
	/// Leave the controller.
	Quit,
}

/// Builds the tool call behind a `!` / `$` composer line (pi `handleBashCommand`
/// → `bash`, `handlePythonCommand` → `eval` with the Python kernel).
fn local_run(input: omp_chat::composer::LocalInput) -> omp_agent::LocalRun {
	let (name, args) = match input.mode {
		omp_chat::composer::PrefixMode::Bash => {
			("bash", serde_json::json!({ "command": input.code.as_str() }))
		},
		omp_chat::composer::PrefixMode::Eval => {
			("eval", serde_json::json!({ "language": "py", "code": input.code.as_str() }))
		},
	};
	omp_agent::LocalRun {
		name:    Str::new_static(name),
		args:    serde_json::value::to_raw_value(&args).expect("literal JSON serializes"),
		intent:  None,
		exclude: input.exclude,
	}
}

/// A background `/tan` child finished.
struct TanDone {
	id:     Str,
	ok:     bool,
	answer: Str,
}

/// The chat controller: session owner, kernel driver, command applier.
pub(crate) struct Controller<C = ComposedInference> {
	kernel:       Kernel<C>,
	lifecycle:    Option<LifecycleHooks>,
	session:      Session,
	home:         SessionHome,
	relay:        flume::Sender<Event>,
	forwarder:    Option<tokio::task::JoinHandle<()>>,
	ctx:          Arc<Ctx>,
	mutations:    Arc<dyn Mutations>,
	up:           flume::Sender<Up>,
	live_journal: Arc<RwLock<PathBuf>>,
	data_dir:     PathBuf,
	voice:        crate::chat_voice::PushToTalk,
	paused:       bool,
	/// Commands that mutate the session, deferred while a turn runs.
	pending:      Vec<HostCommand>,
	tan_tx:       flume::Sender<TanDone>,
	tan_rx:       flume::Receiver<TanDone>,
	/// Journal deleted by `/drop` once the replacement session is live.
	ephemeral:    Option<PathBuf>,
	/// Pairs waiting `ask` calls with the host's answers.
	ask:          omp_driver::headless::AskRoute,
}

impl<C: omp_agent::Inference> Controller<C> {
	/// Takes ownership of the composed kernel and session and starts
	/// relaying the session's DOM events onto `relay`.
	pub(crate) fn new(
		kernel: Kernel<C>,
		mut session: Session,
		home: SessionHome,
		relay: flume::Sender<Event>,
		ctx: Arc<Ctx>,
		mutations: Arc<dyn Mutations>,
		live_journal: Arc<RwLock<PathBuf>>,
		data_dir: PathBuf,
		ephemeral: Option<PathBuf>,
		ask: omp_driver::headless::AskRoute,
	) -> (Self, omp_dom::Snapshot) {
		let up = kernel.mailbox();
		let lifecycle = kernel.lifecycle_hooks();
		let (snapshot, events) = session.subscribe();
		let forwarder = Some(forward(events, relay.clone()));
		let (tan_tx, tan_rx) = flume::unbounded();
		let controller = Self {
			kernel,
			lifecycle,
			session,
			home,
			relay,
			forwarder,
			ctx,
			mutations,
			up,
			live_journal,
			data_dir,
			voice: crate::chat_voice::PushToTalk::new(
				crate::audio_coordinator::InteractiveAudioController::new(),
			),
			paused: false,
			pending: Vec::new(),
			tan_tx,
			tan_rx,
			ephemeral,
			ask,
		};
		(controller, snapshot)
	}

	/// Drives commands until the host quits.
	pub(crate) async fn run(
		mut self,
		command_rx: flume::Receiver<HostCommand>,
	) -> miette::Result<()> {
		let _ = Self::gate_lifecycle(
			self.lifecycle.clone(),
			HookEventId::HookEventSessionStart,
				serde_json::json!({
					"session_id": display_name(&self.session),
					"root": &self.home.project_root,
					"cwd": &self.home.project_root,
					"dirs": [],
					"resumed": !self.session.dom().children(self.session.dom().body()).is_empty(),
					"forked_from": serde_json::Value::Null,
					"agent": serde_json::Value::Null,
					"trust": "trusted",
					"head_event": self.head()?,
					"prompt_rev": "1",
					"previous_session": serde_json::Value::Null,
				}),
			)
			.await?;
		loop {
			let flow = tokio::select! {
				command = command_rx.recv_async() => match command {
					Ok(command) => self.apply_idle(command).await?,
					Err(_) => Flow::Quit,
				},
				done = self.tan_rx.recv_async() => {
					if let Ok(done) = done {
						self.settle_tan(done)?;
					}
					Flow::Idle
				},
			};
			match flow {
				Flow::Idle => {},
				Flow::Turn(input) => {
					let quit = self.run_turn(input, &command_rx).await? || self.after_turn(&command_rx).await?;
					if quit {
						self.shutdown()?;
						return Ok(());
					}
				},
				Flow::Local(run) => {
					let quit = self.run_local(run, &command_rx).await? || self.after_turn(&command_rx).await?;
					if quit {
						self.shutdown()?;
						return Ok(());
					}
				},
				Flow::Quit => {
					self.shutdown()?;
					return Ok(());
				},
			}
			// A queued prompt runs as soon as the controller is idle and
			// not paused (pi `followUp`: "for when the agent yields").
			if !self.paused && let Some(prompt) = self.pop_queued()? {
				let quit = self
					.run_turn(TurnInput { text: prompt, attachments: Vec::new() }, &command_rx)
					.await?
					|| self.after_turn(&command_rx).await?;
				if quit {
					self.shutdown()?;
					return Ok(());
				}
			}
		}
	}

	/// Runs a lifecycle admission gate when extensions subscribed to it.
	async fn gate_lifecycle(
		lifecycle: Option<LifecycleHooks>,
		event: HookEventId,
		payload: serde_json::Value,
	) -> miette::Result<serde_json::Value> {
		match lifecycle {
			Some(lifecycle) => lifecycle.gate(event, payload).await.into_diagnostic(),
			None => Ok(payload),
		}
	}

	/// Notifies lifecycle observers when extensions subscribed to the event.
	fn notify_lifecycle(
		&self,
		event: HookEventId,
		payload: serde_json::Value,
	) -> miette::Result<()> {
		if let Some(lifecycle) = &self.lifecycle {
			lifecycle.notify(event, payload).into_diagnostic()?;
		}
		Ok(())
	}

	/// Commits process exit before observers see the bounded shutdown edge.
	fn shutdown(&mut self) -> miette::Result<()> {
		let session = display_name(&self.session);
		self
			.kernel
			.flush_session_state(&mut self.session)
			.into_diagnostic()?;
		self.session.process_exit().into_diagnostic()?;
		self.notify_lifecycle(
			HookEventId::HookEventSessionShutdown,
			serde_json::json!({
				"session_id": session,
				"reason": "user_exit",
				"budget": "1s",
				"target_session": serde_json::Value::Null,
			}),
		)
	}

	/// Applies one command while no turn is running.
	async fn apply_idle(&mut self, command: HostCommand) -> miette::Result<Flow> {
		// Console writes happen in the host's `Ctx`. Persist them at the next
		// controller boundary; transition commands flush after their before
		// hook so admission observes the pre-transition state.
		if !matches!(
			&command,
			HostCommand::SessionOpen { .. }
				| HostCommand::SessionNew { .. }
				| HostCommand::SessionDrop
				| HostCommand::Fork { .. }
				| HostCommand::Rewind { .. }
				| HostCommand::Quit
		) {
			self
				.kernel
				.flush_session_state(&mut self.session)
				.into_diagnostic()?;
			self.kernel.resync_session_state(&self.session);
		}
		Ok(match command {
			HostCommand::Submit(text) => {
				if self.paused {
					self.queue_prompt(text)?;
					self.reply(Severity::Info, "Paused: prompt queued until you resume");
					return Ok(Flow::Idle);
				}
				self.record_loop_prompt(&text)?;
				Flow::Turn(TurnInput { text, attachments: Vec::new() })
			},
			HostCommand::SubmitWithAttachments { text, attachments } => {
				if self.paused {
					self.queue_prompt(text)?;
					return Ok(Flow::Idle);
				}
				Flow::Turn(TurnInput { text, attachments })
			},
			HostCommand::Steer(text) => {
				let _ = self.up.send(Up::Steer(text));
				Flow::Idle
			},
			HostCommand::Interrupt => {
				let _ = self.up.send(Up::Interrupt);
				Flow::Idle
			},
			HostCommand::Approve { id, decision } => {
				let _ = self.up.send(Up::Approve { id, decision });
				Flow::Idle
			},
			HostCommand::RunLocal { input, draft } => {
				if self.paused {
					self.refuse_local(draft);
					return Ok(Flow::Idle);
				}
				Flow::Local(local_run(input))
			},
			HostCommand::AskAnswer { id, answers } => {
				self.answer_ask(&id, answers);
				Flow::Idle
			},
			HostCommand::Overlay { .. } => Flow::Idle,
			HostCommand::PushToTalk { active } => {
				self.voice.set_active(active, &self.ctx);
				Flow::Idle
			},
			HostCommand::LiveVoice { active } => {
				self.voice.set_live(active, &self.ctx);
				Flow::Idle
			},
			HostCommand::Quit => Flow::Quit,
			other => {
				self.apply_session_command(other).await?;
				Flow::Idle
			},
		})
	}

	/// Runs one turn, routing commands that arrive meanwhile: steering,
	/// interrupts, and approvals go to the kernel now; session mutations
	/// wait for the turn to end (ADR 0004: one writer per journal head).
	/// Returns whether the host asked to quit.
	async fn run_turn(
		&mut self,
		input: TurnInput,
		command_rx: &flume::Receiver<HostCommand>,
	) -> miette::Result<bool> {
		let mut quit = false;
		let ask = self.ask.clone();
		let failure = {
			let mut failure = None;
			let turn =
				self
					.kernel
					.run_turn(&mut self.session, input, omp_agent::RunControl::default());
			tokio::pin!(turn);
			loop {
				tokio::select! {
					result = &mut turn => {
						failure = result.err();
						break;
					},
					command = command_rx.recv_async() => match command {
						Ok(HostCommand::Submit(text) | HostCommand::Steer(text)) => {
							let _ = self.up.send(Up::Steer(text));
						},
						Ok(HostCommand::SubmitWithAttachments { text, .. }) => {
							let _ = self.up.send(Up::Steer(text));
						},
						Ok(HostCommand::Interrupt) => {
							let _ = self.up.send(Up::Interrupt);
						},
						Ok(HostCommand::Approve { id, decision }) => {
							let _ = self.up.send(Up::Approve { id, decision });
						},
						Ok(HostCommand::AskAnswer { id, answers }) => answer_ask(&ask, &id, answers),
						Ok(HostCommand::Quit) | Err(_) => {
							let _ = self.up.send(Up::Cancel);
							quit = true;
						},
						Ok(HostCommand::Overlay { .. }) => {},
						Ok(HostCommand::PushToTalk { active }) => self.voice.set_active(active, &self.ctx),
						Ok(HostCommand::LiveVoice { active }) => self.voice.set_live(active, &self.ctx),
						Ok(HostCommand::Queue { prompt }) => {
							let _ = self.up.send(Up::Steer(Str::new(format!("<queued>{prompt}</queued>"))));
						},
						Ok(other) => {
							// Session switches and rewinds end the running turn
							// first (pi aborts the active turn on /new).
							if matches!(
								other,
								HostCommand::SessionOpen { .. }
									| HostCommand::SessionNew { .. }
									| HostCommand::SessionDrop
									| HostCommand::Rewind { .. }
							) {
								let _ = self.up.send(Up::Interrupt);
							}
							self.pending.push(other);
						},
					},
				}
				if quit {
					failure = turn.await.err();
					break;
				}
			}
			failure
		};
		if let Some(error) = failure {
			// The kernel journals the failure as a `<notice kind=error>` before
			// returning; the host renders it and the composer stays live (pi
			// keeps the session open on a failed turn).
			crate::chat_cmd::record_turn_failure(&mut self.session, &error).into_diagnostic()?;
		}
		Ok(quit)
	}

	/// Applies every command deferred during the turn, in arrival order. A
	/// deferred `!` / `$` run (pi `pendingBashComponents`) executes here with
	/// the live command receiver, so Esc and quit still reach it; anything
	/// it defers in turn is drained too. Returns whether the host asked to
	/// quit.
	async fn after_turn(&mut self, command_rx: &flume::Receiver<HostCommand>) -> miette::Result<bool> {
		while !self.pending.is_empty() {
			for command in std::mem::take(&mut self.pending) {
				match command {
					HostCommand::RunLocal { input, draft } => {
						if self.paused {
							self.refuse_local(draft);
						} else if self.run_local(local_run(input), command_rx).await? {
							return Ok(true);
						}
					},
					other => self.apply_session_command(other).await?,
				}
			}
		}
		Ok(false)
	}

	/// Hands a `!` / `$` line back to the composer: the controller is paused
	/// and will not run tools until resumed.
	fn refuse_local(&self, draft: Str) {
		if let Some(mailbox) = self.ctx.user::<HostMailbox>() {
			mailbox.post(HostAction::LocalRefused {
				draft,
				reason: Str::new_static("Paused: resume before running local commands"),
			});
		}
	}

	/// Applies one session-mutating command between turns.
	async fn apply_session_command(&mut self, command: HostCommand) -> miette::Result<()> {
		match command {
			HostCommand::PlanMode { engage } => {
				crate::chat_cmd::set_plan_mode(&mut self.session, engage).into_diagnostic()?;
			},
			HostCommand::SessionOpen { path } => {
				let next = self.home.open(&path).map_err(|error| miette!(error))?;
				let name = display_name(&next);
				self.switch_to(next, "resume").await?;
				self.reply(Severity::Info, format!("Resumed session {name}"));
			},
			HostCommand::SessionNew { model: _ } => {
				let next = self.home.create(None).map_err(|error| miette!(error))?;
				self.switch_to(next, "new").await?;
				self.reply(Severity::Info, "✓ New session started");
			},
			HostCommand::SessionDrop => {
				let dropped = self.session.journal_path().to_path_buf();
				let next = self.home.create(None).map_err(|error| miette!(error))?;
				self.switch_to(next, "new").await?;
				let _ = fs::remove_file(&dropped);
				if self.ephemeral.as_ref() == Some(&dropped) {
					self.ephemeral = None;
				}
				self.reply(Severity::Info, "✓ Session dropped");
			},
			HostCommand::Fork { target } => {
				let source = self.session.journal_path().to_path_buf();
				let at_event = self.head()?;
				let effective = Self::gate_lifecycle(
					self.lifecycle.clone(),
					HookEventId::HookEventSessionBranch,
						serde_json::json!({
							"at_event": at_event,
							"keep_event": target,
							"reason": "user",
							"summarize": false,
						}),
					)
					.await?;
				if effective.get("summarize").and_then(serde_json::Value::as_bool) != Some(false) {
					return Err(SessionHookError::UnsupportedTransform {
						event: HookEventId::HookEventSessionBranch,
						field: "summarize",
					})
					.into_diagnostic();
				}
				self
					.kernel
					.flush_session_state(&mut self.session)
					.into_diagnostic()?;
				let mut next = self.home.fork(&source).map_err(|error| miette!(error))?;
				if let Some(target) = target {
					next.rewind(target).map_err(|error| miette!(error))?;
				}
				let name = display_name(&next);
				self.switch_to(next, "fork").await?;
				self.notify_lifecycle(
					HookEventId::HookEventSessionBranched,
					serde_json::json!({
						"at_event": at_event,
						"new_head": self.head()?,
						"summary_event": serde_json::Value::Null,
					}),
				)?;
				self.reply(Severity::Info, format!("✓ Session forked to {name}"));
			},
			HostCommand::Rewind { target } => {
				let effective = Self::gate_lifecycle(
					self.lifecycle.clone(),
					HookEventId::HookEventSessionRewind,
						serde_json::json!({
							"to_event": target,
							"restore_workspace": false,
							"targets": [],
							"dropped_items": 0,
						}),
					)
					.await?;
				if effective
					.get("restore_workspace")
					.and_then(serde_json::Value::as_bool)
					!= Some(false)
				{
					return Err(SessionHookError::UnsupportedTransform {
						event: HookEventId::HookEventSessionRewind,
						field: "restore_workspace",
					})
					.into_diagnostic();
				}
				self
					.kernel
					.flush_session_state(&mut self.session)
					.into_diagnostic()?;
				let before = self.session.dom().snapshot();
				match self.session.rewind(target) {
					Ok(work) => {
						self.kernel.apply_lifecycle(&self.session, &work).await;
						self.home.register(&self.session);
						self.kernel.resync_session_state(&self.session);
						let new_head = self.head()?;
						let cancelled_jobs = work
							.terminate
							.iter()
							.filter_map(|handle| before.get(*handle))
							.filter_map(|node| node.prop(&PropId::Id.into()))
							.filter_map(Value::as_str)
							.map(Str::new)
							.collect::<Vec<_>>();
						let running_jobs = work
							.spawn
							.iter()
							.filter_map(|handle| self.session.dom().get(*handle))
							.filter_map(|node| node.prop(&PropId::Id.into()))
							.filter_map(Value::as_str)
							.map(Str::new)
							.collect::<Vec<_>>();
						self.notify_lifecycle(
							HookEventId::HookEventSessionRewound,
							serde_json::json!({
								"to_event": target,
								"new_head": new_head,
								"restored_workspace": false,
								"running_jobs": running_jobs,
								"cancelled_jobs": cancelled_jobs,
							}),
						)?;
						if !work.terminate.is_empty() {
							self.reply(
								Severity::Warn,
								format!(
									"Rewound; {} background job(s) fell off the live chain",
									work.terminate.len()
								),
							);
						}
					},
					Err(error) => self.reply(Severity::Warn, format!("Rewind failed: {error}")),
				}
			},
			HostCommand::Rename { title } => {
				let cause = self.head()?;
				self
					.session
					.patch(Txn {
						cause,
						label: Some(Str::new_static("session.rename")),
						ops: vec![Op::Set {
							h:     self.session.dom().meta(),
							prop:  PropId::Name.into(),
							value: Value::Str(title.clone()),
						}],
					})
					.into_diagnostic()?;
				self.notify_lifecycle(
					HookEventId::HookEventSessionRenamed,
					serde_json::json!({
						"session": display_name(&self.session),
						"name": title,
					}),
				)?;
			},
			HostCommand::Compact { method, hint } => self.compact(method, hint).await?,
			HostCommand::Queue { prompt } => self.queue_prompt(prompt)?,
			HostCommand::Dequeue { prompts } => {
				let dom = self.session.dom();
				let ops = prompts
					.iter()
					.filter_map(|id| queued_prompt(dom, id))
					.map(|handle| Op::Set {
						h:     handle,
						prop:  PropId::Status.into(),
						value: Value::Str(Str::new_static("dequeued")),
					})
					.collect::<Vec<_>>();
				if !ops.is_empty() {
					let cause = self.head()?;
					self
						.session
						.patch(Txn { cause, label: Some(Str::new_static("queue.dequeue")), ops })
						.into_diagnostic()?;
				}
			},
			HostCommand::Director { id, engage, args } => {
				if let Err(error) = self.director(id.as_str(), engage, &args) {
					self.reply(Severity::Warn, format!("{id}: {error}"));
				}
			},
			HostCommand::Spawn { kind: SpawnKind::Tan, text } => self.spawn_tan(text)?,
			HostCommand::Spawn { kind: SpawnKind::Btw, text } => {
				// `/btw` streams through `Services::btw`; a stray spawn request
				// is answered the same way pi answers without a panel.
				let _ = self.up.send(Up::Steer(text));
			},
			HostCommand::Pause { active } => {
				self.paused = active;
			},
			HostCommand::Todo(op) => {
				if let Err(error) = self.todo(op) {
					self.reply(Severity::Warn, format!("todo: {error}"));
				}
			},
			HostCommand::ContextReset => match self.reset_context() {
				Ok(dropped) => {
					if dropped > 0 {
						self.notify_lifecycle(
							HookEventId::HookEventSessionReset,
							serde_json::json!({
								"at_event": self.head()?,
								"kept_events": 0,
							}),
						)?;
					}
					self.reply(
						Severity::Info,
						format!(
							"✓ Context reset — {dropped} {} dropped; session continues.",
							if dropped == 1 { "message" } else { "messages" }
						),
					);
				},
				Err(error) => self.reply(Severity::Warn, format!("Context reset failed: {error}")),
			},
			HostCommand::Move { path } => match self.relocate(&path).await {
				Ok(()) => self.reply(Severity::Info, format!("✓ Moved to {}", path.display())),
				Err(error) => self.reply(Severity::Warn, format!("Move failed: {error}")),
			},
			HostCommand::AskAnswer { id, answers } => self.answer_ask(&id, answers),
			HostCommand::Service(mutation) => self.apply_mutation(mutation),
			HostCommand::SessionIndex { scope } => {
				self.post_outcome(Outcome::SessionIndex(SessionIndexOutcome {
					scope,
					result: Err(Str::new_static("session index authority unavailable")),
				}));
			},
			HostCommand::Git(op) => self.post_outcome(Outcome::Git(GitOutcome {
				op,
				result: Err(ServiceError::Unavailable("controller-owned git operations")),
			})),
			HostCommand::Agent { id, op } => {
				self.post_outcome(Outcome::Agent(AgentOutcome {
					id,
					op,
					result: Err(ServiceError::Unavailable("agent supervision")),
				}));
			},
			// Deferred local runs are drained by `after_turn`, never applied
			// as a plain session command.
			HostCommand::RunLocal { .. }
			| HostCommand::Retry
			| HostCommand::Submit(_)
			| HostCommand::SubmitWithAttachments { .. }
			| HostCommand::Steer(_)
			| HostCommand::Interrupt
			| HostCommand::Approve { .. }
			| HostCommand::Overlay { .. }
			| HostCommand::PushToTalk { .. }
			| HostCommand::LiveVoice { .. }
			| HostCommand::Quit => {},
		}
		Ok(())
	}

	fn answer_ask(&self, id: &str, answers: Option<Vec<omp_tools::ask::Answer>>) {
		answer_ask(&self.ask, id, answers);
	}

	fn apply_mutation(&self, mutation: Mutation) {
		let pending = match self.mutations.apply(mutation.clone()) {
			Ok(pending) => pending,
			Err(error) => {
				self.post_outcome(Outcome::Service(ServiceOutcome {
					mutation,
					result: Err(error),
				}));
				return;
			},
		};
		let ctx = Arc::clone(&self.ctx);
		tokio::spawn(async move {
			let result = pending
				.recv_async()
				.await
				.unwrap_or_else(|_| Err(ServiceError::Unavailable("mutation result")));
			if let Some(mailbox) = ctx.user::<HostMailbox>() {
				mailbox.post(HostAction::Outcome(Outcome::Service(ServiceOutcome {
					mutation,
					result,
				})));
			}
		});
	}

	fn post_outcome(&self, outcome: Outcome) {
		if let Some(mailbox) = self.ctx.user::<HostMailbox>() {
			mailbox.post(HostAction::Outcome(outcome));
		}
	}

	/// Runs one tool outside a model turn (`!` / `$`), routing interrupts,
	/// approvals, and quit from the host meanwhile. Returns whether the host
	/// asked to quit.
	async fn run_local(
		&mut self,
		run: omp_agent::LocalRun,
		command_rx: &flume::Receiver<HostCommand>,
	) -> miette::Result<bool> {
		let mut quit = false;
		let ask = self.ask.clone();
		let failure = {
			let local =
				self
					.kernel
					.run_local(&mut self.session, run, omp_agent::RunControl::default());
			tokio::pin!(local);
			let mut failure = None;
			loop {
				tokio::select! {
					result = &mut local => {
						failure = result.err();
						break;
					},
					command = command_rx.recv_async() => match command {
						Ok(HostCommand::Interrupt) => {
							let _ = self.up.send(Up::Interrupt);
						},
						Ok(HostCommand::Approve { id, decision }) => {
							let _ = self.up.send(Up::Approve { id, decision });
						},
						Ok(HostCommand::Quit) | Err(_) => {
							let _ = self.up.send(Up::Cancel);
							quit = true;
						},
						Ok(HostCommand::Submit(text) | HostCommand::Steer(text)) => {
							let _ = self.up.send(Up::Steer(text));
						},
						Ok(HostCommand::AskAnswer { id, answers }) => answer_ask(&ask, &id, answers),
						Ok(HostCommand::Overlay { .. }) => {},
						Ok(other) => self.pending.push(other),
					},
				}
				if quit {
					failure = local.await.err();
					break;
				}
			}
			failure
		};
		if let Some(error) = failure {
			crate::chat_cmd::record_turn_failure(&mut self.session, &error).into_diagnostic()?;
		}
		Ok(quit)
	}

	/// Replaces the live session: the old one records a switch, the new
	/// one's subscription is relayed after exactly one `Reset`.
	async fn switch_to(
		&mut self,
		mut next: Session,
		reason: &'static str,
	) -> miette::Result<()> {
		let from = display_name(&self.session);
		let to = display_name(&next);
		let _ = Self::gate_lifecycle(
			self.lifecycle.clone(),
			HookEventId::HookEventSessionSwitch,
				serde_json::json!({
					"reason": reason,
					"from_session": from,
					"to_session": to,
					"target_cwd": next.journal_path().parent(),
				}),
			)
			.await?;
		self
			.kernel
			.flush_session_state(&mut self.session)
			.into_diagnostic()?;
		// Subscribe before the swap: nothing writes `next` until it is live,
		// so its receiver holds no events when the reset goes out.
		let (snapshot, events) = next.subscribe();
		let _ = self.session.session_switch();
		self.home.unregister(&self.session);
		let previous = std::mem::replace(&mut self.session, next);
		drop(previous);
		if let Some(forwarder) = self.forwarder.take() {
			// The old DOM's sender is gone; the forwarder drains what it
			// buffered and ends, so nothing from the old session lands after
			// the reset.
			let _ = forwarder.await;
		}
		*self.live_journal.write() = self.session.journal_path().to_path_buf();
		let _ = self.relay.send(Event::Reset { snapshot });
		self.forwarder = Some(forward(events, self.relay.clone()));
		self.kernel.resync_session_state(&self.session);
		self.notify_lifecycle(
			HookEventId::HookEventSessionSwitched,
			serde_json::json!({
				"reason": reason,
				"from_session": from,
				"to_session": to,
				"head_event": self.head()?,
			}),
		)?;
		Ok(())
	}

	/// pi `resetSessionContext`: journals a `compaction@1` at the head whose
	/// summary is empty, so the provider projection starts over while the
	/// session id, title, and journal survive. Returns the message count
	/// the boundary hides.
	fn reset_context(&mut self) -> miette::Result<usize> {
		let dropped = omp_chat::commands::message_count(self.session.dom());
		if dropped == 0 {
			return Ok(0);
		}
		let root = self
			.session
			.journal_path()
			.parent()
			.map(PathBuf::from)
			.unwrap_or_else(|| PathBuf::from("."));
		let summary = omp_journal::blob::BlobStore::open(root)
			.into_diagnostic()?
			.put(b"")
			.into_diagnostic()?;
		let boundary = self.head()?;
		self
			.session
			.compaction(omp_journal::data::Compaction {
				summary,
				boundary,
				method: Some(Str::new_static("clear")),
				tokens_before: None,
				tokens_after: None,
				warning: None,
			})
			.into_diagnostic()?;
		Ok(dropped)
	}

	/// pi `moveSession` + `setProjectDir`: copies the journal (and its blob
	/// store) into `target`'s session bucket, opens the copy as the live
	/// session, removes the old file, and moves the process working
	/// directory. A failure before the switch leaves everything in place.
	async fn relocate(&mut self, target: &std::path::Path) -> miette::Result<()> {
		let target = fs::canonicalize(target).into_diagnostic()?;
		if !target.is_dir() {
			return Err(miette!("not a directory: {}", target.display()));
		}
		let state_dir =
			omp_env::project_state::directory(&self.data_dir, &target).into_diagnostic()?;
		let sessions_dir = state_dir.join("sessions");
		fs::create_dir_all(&sessions_dir).into_diagnostic()?;
		let source = self.session.journal_path().to_path_buf();
		let file = source
			.file_name()
			.ok_or_else(|| miette!("journal has no file name"))?;
		let destination = sessions_dir.join(file);
		if destination == source {
			return Err(miette!("the session already lives in {}", target.display()));
		}
		if let Some(blobs) = source.parent().map(|dir| dir.join("blobs"))
			&& blobs.is_dir()
		{
			copy_tree(&blobs, &sessions_dir.join("blobs")).into_diagnostic()?;
		}
		fs::copy(&source, &destination).into_diagnostic()?;
		let home = SessionHome {
			sessions_dir,
			project_root: target.clone(),
			model: self.home.model.clone(),
			prompt: self.home.prompt.clone(),
			live: Arc::clone(&self.home.live),
			up: self.home.up.clone(),
		};
		let next = match home.open(&destination) {
			Ok(next) => next,
			Err(error) => {
				let _ = fs::remove_file(&destination);
				return Err(miette!(error));
			},
		};
		self.switch_to(next, "handoff").await?;
		self.home = home;
		let _ = fs::remove_file(&source);
		if self.ephemeral.as_ref() == Some(&source) {
			self.ephemeral = Some(destination);
		}
		if let Err(error) = std::env::set_current_dir(&target) {
			self.reply(
				Severity::Warn,
				format!("Session moved, but the working directory could not change: {error}"),
			);
		}
		Ok(())
	}

	fn head(&self) -> miette::Result<EntryId> {
		self
			.session
			.head()
			.ok_or_else(|| miette!("session has no journal head"))
	}

	fn reply(&self, severity: Severity, text: impl Into<Str>) {
		if let Some(mailbox) = self.ctx.user::<HostMailbox>() {
			mailbox.post(HostAction::Reply { severity, text: text.into() });
		}
	}

	/// Journals a `/queue` prompt under `<queues><prompts>`.
	fn queue_prompt(&mut self, prompt: Str) -> miette::Result<()> {
		let dom = self.session.dom();
		let prompts = prompts_root(dom).ok_or_else(|| miette!("session has no prompt queue"))?;
		let id = Str::new(format!("queued-{}", Ulid::generate()));
		let node = NodeSpec::new(KnownTag::Prompt)
			.with_prop(PropId::Kind, Value::Str(Str::new_static(QUEUED)))
			.with_prop(PropId::Id, Value::Str(id))
			.with_prop(PropId::Status, Value::Str(Str::new_static("pending")))
			.with_content(prompt);
		let cause = self.head()?;
		self
			.session
			.patch(Txn {
				cause,
				label: Some(Str::new_static("queue.push")),
				ops: vec![Op::Ins { parent: prompts, after: dom.children(prompts).last().copied(), node }],
			})
			.into_diagnostic()?;
		Ok(())
	}

	/// Takes the oldest pending `/queue` prompt, marking it sent.
	fn pop_queued(&mut self) -> miette::Result<Option<Str>> {
		let dom = self.session.dom();
		let Some(prompts) = prompts_root(dom) else {
			return Ok(None);
		};
		let Some((handle, text)) = dom
			.children(prompts)
			.iter()
			.copied()
			.find_map(|handle| {
				let node = dom.get(handle)?;
				(node.tag == Tag::Known(KnownTag::Prompt)
					&& prop_str(node, PropId::Kind) == Some(QUEUED)
					&& prop_str(node, PropId::Status) == Some("pending"))
				.then(|| (handle, node.content.clone().unwrap_or_default()))
			})
		else {
			return Ok(None);
		};
		let cause = self.head()?;
		self
			.session
			.patch(Txn {
				cause,
				label: Some(Str::new_static("queue.pop")),
				ops: vec![Op::Set {
					h:     handle,
					prop:  PropId::Status.into(),
					value: Value::Str(Str::new_static("sent")),
				}],
			})
			.into_diagnostic()?;
		self.record_loop_prompt(&text)?;
		Ok(Some(text))
	}

	/// `/loop` without a prompt records the next prompt as the loop prompt
	/// (pi "Your next prompt will repeat after each turn.").
	fn record_loop_prompt(&mut self, text: &Str) -> miette::Result<()> {
		let dom = self.session.dom();
		let Some((handle, node)) = omp_agent::find_director(dom, "loop_mode") else {
			return Ok(());
		};
		if omp_agent::state_str(node, "prompt").is_some_and(|prompt| !prompt.is_empty()) {
			return Ok(());
		}
		let cause = self.head()?;
		self
			.session
			.patch(Txn {
				cause,
				label: Some(Str::new_static("director.state")),
				ops: vec![Op::Set {
					h:     handle,
					prop:  PropKey::Custom(Str::new_static("state/prompt")),
					value: Value::Str(text.clone()),
				}],
			})
			.into_diagnostic()?;
		Ok(())
	}

	/// Engages or exits one Director family (ADR 0015 `<meta><directors>`).
	fn director(&mut self, id: &str, engage: bool, args: &[Str]) -> Result<(), DirectorFailure> {
		use omp_agent::directors::{force_tool::ForceTool, goal::Goal, loop_mode::LoopMode, vibe::Vibe};
		let registry = omp_agent::DirectorRegistry::standard();
		let mut stack = omp_agent::DirectorStack::from_dom(self.session.dom(), &registry);
		let active = stack.active_ids().contains(&id);
		if !engage {
			return match omp_agent::find_director(self.session.dom(), id) {
				Some((handle, _)) => {
					let cause = self.session.head().ok_or(DirectorFailure::NoHead)?;
					self.session.patch(Txn {
						cause,
						label: Some(Str::new_static("director.exit")),
						ops: vec![Op::Rm(handle)],
					})?;
					Ok(())
				},
				None => Ok(()),
			};
		}
		let director: Box<dyn omp_agent::Director> = match id {
			"vibe" => Box::new(Vibe::new()),
			"goal" => {
				let verb = args.first().map(Str::as_str).unwrap_or_default();
				match verb {
					"budget" => {
						let (handle, _) = omp_agent::find_director(self.session.dom(), "goal")
							.ok_or(DirectorFailure::NotActive)?;
						let budget = args
							.get(1)
							.and_then(|value| value.parse::<i64>().ok())
							.map_or(Value::Null, Value::Int);
						let cause = self.session.head().ok_or(DirectorFailure::NoHead)?;
						self.session.patch(Txn {
							cause,
							label: Some(Str::new_static("director.state")),
							ops: vec![Op::Set {
								h:     handle,
								prop:  PropKey::Custom(Str::new_static("state/token_budget")),
								value: budget,
							}],
						})?;
						return Ok(());
					},
					_ => {
						let objective = args.get(1).cloned().unwrap_or_default();
						if active {
							// Replace the objective in place (pi `replaceGoalFromObjective`).
							let (handle, _) = omp_agent::find_director(self.session.dom(), "goal")
								.ok_or(DirectorFailure::NotActive)?;
							let cause = self.session.head().ok_or(DirectorFailure::NoHead)?;
							self.session.patch(Txn {
								cause,
								label: Some(Str::new_static("director.state")),
								ops: vec![Op::Set {
									h:     handle,
									prop:  PropKey::Custom(Str::new_static("state/objective")),
									value: Value::Str(objective),
								}],
							})?;
							return Ok(());
						}
						Box::new(Goal::new(objective, None))
					},
				}
			},
			"loop_mode" => {
				let count = args
					.first()
					.and_then(|value| value.parse::<u32>().ok());
				let prompt = args.get(1).cloned().unwrap_or_default();
				Box::new(LoopMode::new(prompt, count))
			},
			"force_tool" => {
				let tool = args.first().cloned().ok_or(DirectorFailure::MissingArgument("tool"))?;
				if self.kernel.tool_registry().live_spec(tool.as_str()).is_err() {
					return Err(DirectorFailure::UnknownTool(tool));
				}
				Box::new(ForceTool::new(
					tool.clone(),
					omp_agent::ForceUntil::ToolCalled(tool),
					None,
					3,
				))
			},
			_ => return Err(DirectorFailure::UnknownDirector(Str::new(id))),
		};
		if active {
			return Ok(());
		}
		stack.engage(&mut self.session, director)?;
		Ok(())
	}

	/// `/compact`, `/handoff`, `/shake`.
	async fn compact(&mut self, method: CompactionMethod, hint: Option<Str>) -> miette::Result<()> {
		match method {
			CompactionMethod::Compact | CompactionMethod::Handoff => {
				let label = if method == CompactionMethod::Handoff {
					"handoff"
				} else {
					"manual"
				};
				match self.kernel.compact(&mut self.session, hint, label).await {
					Ok(true) => self.reply(
						Severity::Info,
						if method == CompactionMethod::Handoff {
							"Context handed off and compacted in place."
						} else {
							"Compaction complete."
						},
					),
					Ok(false) => self.reply(Severity::Warn, "Nothing to compact (no messages yet)"),
					Err(error) => self.reply(
						Severity::Error,
						format!(
							"{} failed: {error}",
							if method == CompactionMethod::Handoff { "Handoff" } else { "Compaction" }
						),
					),
				}
			},
			CompactionMethod::Shake => {
				let mode = hint
					.as_deref()
					.and_then(|mode| mode.parse::<ShakeMode>().ok())
					.unwrap_or(ShakeMode::Elide);
				let summary = self.shake(mode)?;
				self.reply(Severity::Info, summary);
			},
		}
		Ok(())
	}

	/// pi `session.shake`: drops recoverable heavy content in place without
	/// an LLM call. `elide` blanks settled tool results (the call and its
	/// status stay, so the transcript and the provider thread remain
	/// well-formed); `thinking` clears assistant reasoning; `images` drops
	/// user attachments.
	fn shake(&mut self, mode: ShakeMode) -> miette::Result<Str> {
		const ELIDED: &str = "[elided by /shake]";
		let dom = self.session.dom();
		let mut ops = Vec::new();
		let mut freed = 0usize;
		for turn in dom.children(dom.body()) {
			for handle in dom.children(*turn) {
				let Some(node) = dom.get(*handle) else { continue };
				match (mode, &node.tag) {
					(ShakeMode::Elide, Tag::Custom(_)) => {
						for child in dom.children(*handle) {
							let Some(part) = dom.get(*child) else { continue };
							if part.tag != Tag::Known(KnownTag::Result) {
								continue;
							}
							let text = part
								.content
								.as_deref()
								.or_else(|| part.prop(&PropId::Text.into()).and_then(Value::as_str))
								.unwrap_or_default();
							if text.len() <= ELIDED.len() {
								continue;
							}
							freed += text.len();
							ops.push(Op::Set {
								h:     *child,
								prop:  PropId::Text.into(),
								value: Value::Str(Str::new_static(ELIDED)),
							});
							ops.push(Op::Set { h: *child, prop: PropId::Data.into(), value: Value::Null });
						}
					},
					(ShakeMode::Thinking, Tag::Known(KnownTag::Assistant)) => {
						let text = node
							.prop(&PropId::Thinking.into())
							.and_then(Value::as_str)
							.unwrap_or_default();
						if text.is_empty() {
							continue;
						}
						freed += text.len();
						ops.push(Op::Set {
							h:     *handle,
							prop:  PropId::Thinking.into(),
							value: Value::Str(Str::new_static("")),
						});
					},
					(ShakeMode::Images, Tag::Known(KnownTag::User)) => {
						// Attachments ride the user node's `data` prop (fold: blob refs).
						if matches!(node.prop(&PropId::Data.into()), Some(Value::Json(_))) {
							freed += 1;
							ops.push(Op::Set { h: *handle, prop: PropId::Data.into(), value: Value::Null });
						}
					},
					_ => {},
				}
			}
		}
		let count = match mode {
			ShakeMode::Elide => ops.len() / 2,
			_ => ops.len(),
		};
		if count == 0 {
			return Ok(Str::new_static(match mode {
				ShakeMode::Elide => "Nothing to shake.",
				ShakeMode::Images => "No images found in this session.",
				ShakeMode::Thinking => "No thinking blocks found in this session.",
			}));
		}
		let cause = self.head()?;
		self
			.session
			.patch(Txn { cause, label: Some(Str::new_static("shake")), ops })
			.into_diagnostic()?;
		Ok(Str::new(match mode {
			ShakeMode::Elide => {
				format!("Shook {count} tool result(s) (~{} tokens freed).", freed / 4)
			},
			ShakeMode::Images => format!("Dropped {count} image(s) from this session."),
			ShakeMode::Thinking => format!("Dropped {count} thinking block(s) from this session."),
		}))
	}

	/// `/tan`: journals a `<subagent>` job in the parent, runs a full-tool
	/// child kernel in the background, and leaves pi's dispatch breadcrumb
	/// as steering for the parent's next safe point.
	fn spawn_tan(&mut self, work: Str) -> miette::Result<()> {
		let id = Str::new(format!("tan-{}", Ulid::generate()));
		let started = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.into_diagnostic()?
			.as_millis()
			.to_string();
		let cause = self.head()?;
		let txn = jobs::insert(self.session.dom(), cause, jobs::JobSpec {
			id:      id.clone(),
			kind:    Str::new_static("subagent"),
			owner:   Str::new_static("Main"),
			started: Str::new(started),
			agent:   Some(Str::new_static("tan")),
		})
		.ok_or_else(|| miette!("session has no jobs component"))?;
		self.session.patch(txn).into_diagnostic()?;
		let breadcrumb = TAN_DISPATCH
			.replace("{{jobId}}", id.as_str())
			.replace("{{work}}", work.as_str());
		let _ = self.up.send(Up::Steer(Str::new(breadcrumb)));
		self.reply(Severity::Info, format!("Dispatched background tan {id}"));

		let data_dir = self.data_dir.clone();
		let project = self.home.project_root.clone();
		let model = self.home.model.clone();
		let ctx = Arc::clone(&self.ctx);
		let sessions_dir = self.home.sessions_dir.clone();
		let live = Arc::clone(&self.home.live);
		let done = self.tan_tx.clone();
		let prompt = Str::new(format!("{TAN_CONTEXT}\n\n{work}"));
		tokio::spawn(async move {
			let options = KernelOptions {
				session: Some(sessions_dir.join(format!("{id}.oms"))),
				sessions_dir: Some(sessions_dir),
				sessions: Some(live),
				session_name: Some(id.clone()),
				..KernelOptions::default()
			};
			let composed =
				omp_driver::headless::kernel::compose_kernel(&data_dir, &project, model.as_str(), ctx, options)
					.await;
			let outcome = match composed {
				Ok((mut kernel, mut session, _)) => kernel
					.run_turn(
						&mut session,
						TurnInput { text: prompt, attachments: Vec::new() },
						omp_agent::RunControl::default(),
					)
					.await
					.map(|outcome| outcome.assistant_text)
					.map_err(|error| error.to_string()),
				Err(error) => Err(error.to_string()),
			};
			let _ = done.send(match outcome {
				Ok(answer) => TanDone { id, ok: true, answer },
				Err(error) => TanDone { id, ok: false, answer: Str::new(error) },
			});
		});
		Ok(())
	}

	/// Settles a finished `/tan` job in the parent tree.
	fn settle_tan(&mut self, done: TanDone) -> miette::Result<()> {
		let dom = self.session.dom();
		let handle = dom
			.select(&format!("jobs subagent[id={}]", done.id))
			.ok()
			.and_then(|mut handles| handles.next());
		if let Some(handle) = handle {
			let cause = self.head()?;
			self
				.session
				.patch(jobs::set_status(cause, handle, if done.ok { "completed" } else { "failed" }))
				.into_diagnostic()?;
		}
		let preview = done
			.answer
			.lines()
			.next()
			.unwrap_or_default();
		self.reply(
			if done.ok { Severity::Info } else { Severity::Warn },
			format!(
				"Background tan {} {}: {preview}",
				done.id,
				if done.ok { "finished" } else { "failed" }
			),
		);
		Ok(())
	}

	/// `/todo` edits over `<meta><todo>` items (pi `helpers/todo.ts`).
	fn todo(&mut self, op: TodoOp) -> Result<(), TodoFailure> {
		let dom = self.session.dom();
		let todo = dom
			.children(dom.meta())
			.iter()
			.copied()
			.find(|handle| {
				dom.get(*handle)
					.is_some_and(|node| node.tag == Tag::Known(KnownTag::Todo))
			})
			.ok_or(TodoFailure::NoComponent)?;
		let items = dom
			.children(todo)
			.iter()
			.copied()
			.filter_map(|handle| {
				let node = dom.get(handle)?;
				let label = prop_str(node, PropId::Label).unwrap_or_default();
				let phase = node
					.prop(&PropKey::Custom(Str::new_static("phase")))
					.and_then(Value::as_str)
					.unwrap_or_default();
				Some((handle, Str::new(label), Str::new(phase)))
			})
			.collect::<Vec<_>>();
		let matches = |needle: &str| -> Vec<Handle> {
			let needle = needle.to_lowercase();
			items
				.iter()
				.filter(|(_, label, phase)| {
					label.to_lowercase().contains(&needle) || phase.to_lowercase() == needle
				})
				.map(|(handle, _, _)| *handle)
				.collect()
		};
		let set_status = |handles: &[Handle], status: &'static str| -> Vec<Op> {
			handles
				.iter()
				.map(|handle| Op::Set {
					h:     *handle,
					prop:  PropId::Status.into(),
					value: Value::Str(Str::new_static(status)),
				})
				.collect()
		};
		let (label, ops, message) = match op {
			TodoOp::Append(text) => {
				let (phase, task) = match text.split_once(char::is_whitespace) {
					Some((first, rest))
						if items
							.iter()
							.any(|(_, _, phase)| phase.eq_ignore_ascii_case(first)) =>
					{
						(Str::new(first), Str::new(rest.trim())) 
					},
					_ => (
						items
							.last()
							.map(|(_, _, phase)| phase.clone())
							.unwrap_or_else(|| Str::new_static("Tasks")),
						text,
					),
				};
				let node = NodeSpec::new(KnownTag::Item)
					.with_prop(PropId::Label, Value::Str(task))
					.with_prop(PropId::Status, Value::Str(Str::new_static("pending")))
					.with_prop(PropKey::Custom(Str::new_static("phase")), Value::Str(phase.clone()));
				(
					"todo.append",
					vec![Op::Ins { parent: todo, after: items.last().map(|(handle, _, _)| *handle), node }],
					format!("Added task to phase \"{phase}\""),
				)
			},
			TodoOp::Start(text) => {
				let found = matches(&text);
				let first = found.first().copied().ok_or(TodoFailure::NoMatch(text))?;
				("todo.start", set_status(&[first], "in_progress"), "Started".to_owned())
			},
			TodoOp::Done(text) => {
				let found = text.as_deref().map_or_else(
					|| items.iter().map(|(handle, _, _)| *handle).collect::<Vec<_>>(),
					matches,
				);
				if found.is_empty() {
					return Err(TodoFailure::NoMatch(text.unwrap_or_default()));
				}
				("todo.done", set_status(&found, "completed"), "Completed".to_owned())
			},
			TodoOp::Drop(text) => {
				let found = text.as_deref().map_or_else(
					|| items.iter().map(|(handle, _, _)| *handle).collect::<Vec<_>>(),
					matches,
				);
				if found.is_empty() {
					return Err(TodoFailure::NoMatch(text.unwrap_or_default()));
				}
				("todo.drop", set_status(&found, "abandoned"), "Dropped".to_owned())
			},
			TodoOp::Remove(text) => {
				let found = text.as_deref().map_or_else(
					|| items.iter().map(|(handle, _, _)| *handle).collect::<Vec<_>>(),
					matches,
				);
				if found.is_empty() {
					return Err(TodoFailure::NoMatch(text.unwrap_or_default()));
				}
				("todo.rm", found.iter().map(|handle| Op::Rm(*handle)).collect(), "Removed".to_owned())
			},
			TodoOp::Import(path) => {
				let path = path.map_or_else(|| PathBuf::from("TODO.md"), |path| PathBuf::from(path.as_str()));
				let text = fs::read_to_string(&path).map_err(|error| TodoFailure::Io(error.to_string()))?;
				let mut ops = items.iter().map(|(handle, _, _)| Op::Rm(*handle)).collect::<Vec<_>>();
				let mut phase = Str::new_static("Tasks");
				let mut count = 0usize;
				for line in text.lines() {
					let line = line.trim();
					if let Some(heading) = line.strip_prefix("## ") {
						phase = Str::new(heading.trim());
						continue;
					}
					let Some(rest) = line.strip_prefix("- [") else { continue };
					let Some((mark, label)) = rest.split_once(']') else { continue };
					let status = match mark.trim() {
						"x" | "X" => "completed",
						"-" => "abandoned",
						">" => "in_progress",
						_ => "pending",
					};
					count += 1;
					ops.push(Op::Ins {
						parent: todo,
						after:  None,
						node:   NodeSpec::new(KnownTag::Item)
							.with_prop(PropId::Label, Value::Str(Str::new(label.trim())))
							.with_prop(PropId::Status, Value::Str(Str::new_static(status)))
							.with_prop(PropKey::Custom(Str::new_static("phase")), Value::Str(phase.clone())),
					});
				}
				("todo.import", ops, format!("Imported {count} todos from {}", path.display()))
			},
			TodoOp::List | TodoOp::Copy | TodoOp::Export(_) => return Ok(()),
		};
		let cause = self.session.head().ok_or(TodoFailure::NoHead)?;
		self
			.session
			.patch(Txn { cause, label: Some(Str::new_static(label)), ops })
			.map_err(|error| TodoFailure::Session(error.to_string()))?;
		self.reply(Severity::Info, message);
		Ok(())
	}
}

/// Resolves the `ask` dialog reply for call `id`; a stale reply (the call
/// already settled or the turn was interrupted) is dropped.
fn answer_ask(
	route: &omp_driver::headless::AskRoute,
	id: &str,
	answers: Option<Vec<omp_tools::ask::Answer>>,
) {
	let reply = match answers {
		Some(answers) => omp_driver::headless::AskReply::Answers(answers),
		None => omp_driver::headless::AskReply::Cancelled,
	};
	if !route.answer(id, reply) {
		tracing::debug!(id, "ask reply had no waiting call");
	}
}

/// Forwards one session's DOM events onto the host's relay until the
/// session is dropped.
fn forward(events: flume::Receiver<Event>, relay: flume::Sender<Event>) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		while let Ok(event) = events.recv_async().await {
			if relay.send(event).is_err() {
				break;
			}
		}
	})
}

/// Copies every file under `from` into `to` (blob stores are flat,
/// content-addressed, and idempotent to merge).
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
	fs::create_dir_all(to)?;
	for entry in fs::read_dir(from)? {
		let entry = entry?;
		let target = to.join(entry.file_name());
		if entry.file_type()?.is_dir() {
			copy_tree(&entry.path(), &target)?;
		} else if !target.exists() {
			fs::copy(entry.path(), target)?;
		}
	}
	Ok(())
}

fn display_name(session: &Session) -> Str {
	session
		.journal_path()
		.file_name()
		.and_then(|name| name.to_str())
		.map_or_else(|| Str::new_static("session"), Str::new)
}

fn prompts_root(dom: &omp_dom::Dom) -> Option<Handle> {
	dom.children(dom.queues()).iter().copied().find(|handle| {
		dom.get(*handle)
			.is_some_and(|node| node.tag == Tag::Known(KnownTag::Prompts))
	})
}

fn queued_prompt(dom: &omp_dom::Dom, id: &str) -> Option<Handle> {
	let prompts = prompts_root(dom)?;
	dom.children(prompts).iter().copied().find(|handle| {
		dom.get(*handle).is_some_and(|node| {
			node.tag == Tag::Known(KnownTag::Prompt)
				&& prop_str(node, PropId::Kind) == Some(QUEUED)
				&& prop_str(node, PropId::Id) == Some(id)
		})
	})
}

fn prop_str(node: &omp_dom::Node, prop: PropId) -> Option<&str> {
	node.prop(&prop.into()).and_then(Value::as_str)
}

/// Why a Director command could not be applied.
#[derive(Debug, thiserror::Error)]
enum DirectorFailure {
	#[error("session has no journal head")]
	NoHead,
	#[error("no active engagement to update")]
	NotActive,
	#[error("missing argument `{0}`")]
	MissingArgument(&'static str),
	#[error("Tool \"{0}\" is not currently active.")]
	UnknownTool(Str),
	#[error("unknown Director `{0}`")]
	UnknownDirector(Str),
	#[error(transparent)]
	Director(#[from] omp_agent::DirectorError),
	#[error(transparent)]
	Session(#[from] SessionError),
}

/// Why a `/todo` edit could not be applied.
#[derive(Debug, thiserror::Error)]
enum TodoFailure {
	#[error("session has no todo component")]
	NoComponent,
	#[error("session has no journal head")]
	NoHead,
	#[error("no todo matches \"{0}\"")]
	NoMatch(Str),
	#[error("{0}")]
	Io(String),
	#[error("{0}")]
	Session(String),
}

#[cfg(test)]
mod tests {
	use std::{future::Future, sync::Arc, time::Duration};

	use async_stream::stream;
	use futures::Stream;
	use omp_agent::{
		DispatchPolicy, KernelEvent, SessionStateBridge, StaticPrompt, TurnStop,
		hooks::{
			GateDecision, HookGate, HookPhase, OnFailure, SourceRef, Subscription, When,
		},
	};
	use omp_chat::composer::{LocalInput, PrefixMode};
	use omp_inference::{
		BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
		ProviderId, RequestId, ResponseMeta, RouteId, Usage,
	};
	use omp_tool::{
		Claims, Constraint, Effects, Ev, IncomingParams, Part, Precedence, Presentation, PromptCaps,
		Registry, Rev, Tool, ToolSpec, ToolTerminal,
	};
	use serde::{Deserialize, Serialize};

	use super::*;

	/// One text answer per request, delivered after `delay` so a command sent
	/// right behind the prompt is provably received mid-turn.
	struct SlowInference {
		delay: Duration,
	}

	impl omp_agent::Inference for SlowInference {
		fn chat(
			&mut self,
			_request: ChatRequest,
		) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send {
			let delay = self.delay;
			async move {
				tokio::time::sleep(delay).await;
				let events = vec![
					ChatEvent::Started(ResponseMeta {
						request_id:          RequestId::from("scripted-request"),
						provider:            ProviderId::from("scripted"),
						route:               RouteId::from("scripted/test"),
						model:               None,
						provider_request_id: None,
						created_at:          SystemTime::UNIX_EPOCH,
					}),
					ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
					ChatEvent::TextDelta { index: 0, text: Str::new_static("pong") },
					ChatEvent::Completed(Completion {
						reason:  FinishReason::Stop,
						blocks:  1,
						usage:   Usage::default(),
						receipt: ExecutionReceipt::default().into(),
					}),
				];
				Ok(ChatStream::ordinary(Box::pin(futures::stream::iter(events.into_iter().map(Ok)))))
			}
		}
	}

	#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
	struct Payload {
		text: Str,
	}

	#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
	struct Fault {
		message: Str,
	}

	/// A `bash` stand-in that runs far longer than the test is willing to
	/// wait; only an interrupt reaching the kernel ends it.
	struct SleepingBash {
		spec: ToolSpec,
	}

	impl SleepingBash {
		fn registry() -> Arc<Registry> {
			let tool = Self {
				spec: ToolSpec {
					name:            Str::new_static("bash"),
					rev:             Rev { family: Str::new_static("test"), n: 1 },
					description:     Str::new_static("sleeping bash"),
					schema:          bytes::Bytes::from_static(br#"{"type":"object"}"#),
					constraint:      Constraint::None,
					effects:         Effects::empty(),
					projection_code: [1; 32],
				},
			};
			let mut registry = Registry::new();
			registry
				.register(tool, Presentation::Slot, Claims {
					precedence: Precedence::CORE,
					claimant:   Str::new_static("omp-app/tests"),
					replaces:   None,
				})
				.expect("tool registers");
			Arc::new(registry)
		}
	}

	impl Tool for SleepingBash {
		type Fault = Fault;
		type Params = serde_json::Value;
		type Payload = Payload;
		type Update = Str;

		fn spec(&self) -> &ToolSpec {
			&self.spec
		}

		fn call<'c>(
			&'c self,
			mut params: IncomingParams<'c>,
		) -> impl Stream<Item = Ev<Self::Update, Self::Payload, Self::Fault>> + Send + 'c {
			stream! {
				let _ = params.committed().await;
				tokio::time::sleep(Duration::from_secs(20)).await;
				yield Ev::Done(ToolTerminal::Done {
					result: Ok(Payload { text: Str::new_static("slept") }),
					useless: false,
				});
			}
		}

		fn prompt(&self, view: Result<&Self::Payload, &Self::Fault>, _caps: &PromptCaps) -> Vec<Part> {
			let text = match view {
				Ok(payload) => payload.text.clone(),
				Err(fault) => fault.message.clone(),
			};
			vec![Part::Text { text }]
		}
	}

	struct OrderBridge {
		order: flume::Sender<&'static str>,
	}

	impl SessionStateBridge for OrderBridge {
		fn flush(&self, _session: &mut Session) -> Result<(), SessionError> {
			let _ = self.order.send("flush");
			Ok(())
		}

		fn resync(&self, _dom: &omp_dom::Dom) {
			let _ = self.order.send("resync");
		}
	}

	fn subscription(event: HookEventId, phase: HookPhase, id: u32) -> Subscription {
		Subscription {
			host: Str::new_static("controller-test"),
			source: SourceRef {
				layer: 0,
				publisher: Str::new_static("omp-app"),
				extension_id: Str::new_static("controller-test"),
			},
			id,
			event,
			phase,
			order: 0,
			on_failure: OnFailure::Defer,
			when: When::default(),
		}
	}

	struct Harness {
		commands: flume::Sender<HostCommand>,
		events:   flume::Receiver<KernelEvent>,
		ctx:      Arc<Ctx>,
		run:      tokio::task::JoinHandle<miette::Result<()>>,
		_dir:     tempfile::TempDir,
	}

	fn harness(inference_delay: Duration) -> Harness {
		let dir = tempfile::tempdir().expect("temp dir");
		let mut kernel = Kernel::new(
			SlowInference { delay: inference_delay },
			SleepingBash::registry(),
			DispatchPolicy::new(
				omp_journal::blob::BlobStore::open(dir.path().join("blobs")).expect("blob store"),
			),
			StaticPrompt(Str::new_static("test system")),
		);
		let events = kernel.subscribe();
		let home = SessionHome {
			sessions_dir: dir.path().join("sessions"),
			project_root: dir.path().to_path_buf(),
			model:        Str::new_static("test/model"),
			prompt:       omp_driver::headless::kernel::PromptOverrides::default(),
			live:         Arc::new(omp_driver::sessions::SessionRegistry::new()),
			up:           kernel.mailbox(),
		};
		fs::create_dir_all(&home.sessions_dir).expect("sessions dir");
		let session = home.create(None).expect("session");
		let (relay, _dom_events) = flume::unbounded();
		let ctx = Arc::new(HostMailbox::new().attach(Ctx::builder()).build());
		let live_journal = Arc::new(RwLock::new(session.journal_path().to_path_buf()));
		let (controller, _snapshot) = Controller::new(
			kernel,
			session,
			home,
			relay,
			Arc::clone(&ctx),
			Arc::new(omp_chat::overlays::services::NoMutations),
			live_journal,
			dir.path().to_path_buf(),
			None,
			omp_driver::headless::AskRoute::new(),
		);
		let (commands, command_rx) = flume::unbounded();
		let run = tokio::spawn(controller.run(command_rx));
		Harness { commands, events, ctx, run, _dir: dir }
	}

	fn sleep_command() -> HostCommand {
		HostCommand::RunLocal {
			input: LocalInput {
				mode:    PrefixMode::Bash,
				code:    Str::new_static("sleep 20"),
				exclude: false,
			},
			draft: Str::new_static("!sleep 20"),
		}
	}

	async fn next_event(
		events: &flume::Receiver<KernelEvent>,
		accept: impl Fn(&KernelEvent) -> bool,
	) -> KernelEvent {
		tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				let event = events.recv_async().await.expect("kernel event");
				if accept(&event) {
					return event;
				}
			}
		})
		.await
		.expect("event arrives in time")
	}

	#[tokio::test]
	async fn session_switch_orders_gate_flush_transition_resync_then_observation() {
		let dir = tempfile::tempdir().expect("temp dir");
		let (gate, hook_rx) = HookGate::channel();
		let gate = Arc::new(gate);
		gate
			.subscribe("controller-test", [
				subscription(HookEventId::HookEventSessionSwitch, HookPhase::Precheck, 1),
				subscription(HookEventId::HookEventSessionSwitched, HookPhase::Observe, 2),
			])
			.expect("subscriptions");
		let (order_tx, order_rx) = flume::unbounded();
		let kernel = Kernel::new(
			SlowInference { delay: Duration::ZERO },
			SleepingBash::registry(),
			DispatchPolicy::new(
				omp_journal::blob::BlobStore::open(dir.path().join("blobs")).expect("blob store"),
			),
			StaticPrompt(Str::new_static("test system")),
		)
		.with_hook_gate(Arc::clone(&gate))
		.with_session_state_bridge(Arc::new(OrderBridge { order: order_tx.clone() }));
		let home = SessionHome {
			sessions_dir: dir.path().join("sessions"),
			project_root: dir.path().to_path_buf(),
			model: Str::new_static("test/model"),
			prompt: omp_driver::headless::kernel::PromptOverrides::default(),
			live: Arc::new(omp_driver::sessions::SessionRegistry::new()),
			up: kernel.mailbox(),
		};
		fs::create_dir_all(&home.sessions_dir).expect("sessions dir");
		let session = home.create(None).expect("session");
		let next = home.create(None).expect("next session");
		let (relay, _dom_events) = flume::unbounded();
		let ctx = Arc::new(HostMailbox::new().attach(Ctx::builder()).build());
		let live_journal = Arc::new(RwLock::new(session.journal_path().to_path_buf()));
		let (mut controller, _snapshot) = Controller::new(
			kernel,
			session,
			home,
			relay,
			ctx,
			Arc::new(omp_chat::overlays::services::NoMutations),
			live_journal,
			dir.path().to_path_buf(),
			None,
			omp_driver::headless::AskRoute::new(),
		);
		let responder_gate = Arc::clone(&gate);
		let responder_order = order_tx;
		let responder = tokio::spawn(async move {
			while let Ok(dispatch) = hook_rx.recv_async().await {
				let payload: serde_json::Value =
					serde_json::from_slice(&dispatch.payload).expect("JSON lifecycle payload");
				match dispatch.event {
					HookEventId::HookEventSessionSwitch => {
						assert!(payload.get("reason").is_some());
						assert!(payload.get("from_session").is_some());
						assert!(payload.get("to_session").is_some());
						assert!(payload.get("target_cwd").is_some());
						let _ = responder_order.send("before");
						let decisions = dispatch
							.subscriptions
							.iter()
							.map(|subscription| (subscription.id, GateDecision::Defer))
							.collect();
						responder_gate
							.answer(dispatch.dispatch_id, decisions)
							.expect("answer switch gate");
					},
					HookEventId::HookEventSessionSwitched => {
						assert!(payload.get("reason").is_some());
						assert!(payload.get("from_session").is_some());
						assert!(payload.get("to_session").is_some());
						assert!(payload.get("head_event").is_some());
						let _ = responder_order.send("after");
						break;
					},
					other => panic!("unexpected hook dispatch {other:?}"),
				}
			}
		});
		controller.switch_to(next, "new").await.expect("switch");
		responder.await.expect("responder");
		assert_eq!(
			order_rx.try_iter().collect::<Vec<_>>(),
			["before", "flush", "resync", "after"],
		);
	}

	/// A `!` command typed during a model turn runs after it (pi
	/// `pendingBashComponents`) and still hears Esc: the interrupt from the
	/// host reaches the deferred run instead of waiting for it to finish.
	#[tokio::test]
	async fn deferred_local_run_is_interrupted_by_the_host() {
		let harness = harness(Duration::from_millis(300));
		harness
			.commands
			.send(HostCommand::Submit(Str::new_static("hello")))
			.expect("submit");
		harness.commands.send(sleep_command()).expect("local command");
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Completed })
		})
		.await;
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::ToolReady { name, .. } if name == "bash")
		})
		.await;
		harness.commands.send(HostCommand::Interrupt).expect("interrupt");
		let ended = next_event(&harness.events, |event| matches!(event, KernelEvent::TurnEnded { .. })).await;
		assert!(
			matches!(ended, KernelEvent::TurnEnded { stop: TurnStop::Cancelled }),
			"the deferred run ends on the interrupt: {ended:?}"
		);
		harness.commands.send(HostCommand::Quit).expect("quit");
		tokio::time::timeout(Duration::from_secs(5), harness.run)
			.await
			.expect("controller exits")
			.expect("controller task")
			.expect("controller run");
	}

	/// A paused controller hands the `!` line back instead of dropping it,
	/// both when idle and when the run was deferred behind a turn.
	#[tokio::test]
	async fn paused_controller_refuses_local_runs_with_the_draft() {
		let harness = harness(Duration::from_millis(300));
		let mailbox = harness.ctx.user::<HostMailbox>().expect("mailbox");
		harness
			.commands
			.send(HostCommand::Pause { active: true })
			.expect("pause");
		harness.commands.send(sleep_command()).expect("local command");
		let refused = tokio::time::timeout(Duration::from_secs(5), mailbox.next())
			.await
			.expect("refusal arrives")
			.expect("mailbox open");
		assert_eq!(refused, HostAction::LocalRefused {
			draft:  Str::new_static("!sleep 20"),
			reason: Str::new_static("Paused: resume before running local commands"),
		});
		assert!(
			harness.events.try_iter().all(|event| !matches!(event, KernelEvent::ToolReady { .. })),
			"nothing ran"
		);
		// Deferred behind a turn: pausing during the turn wins over the
		// queued run.
		harness
			.commands
			.send(HostCommand::Pause { active: false })
			.expect("resume");
		harness
			.commands
			.send(HostCommand::Submit(Str::new_static("hello")))
			.expect("submit");
		harness
			.commands
			.send(HostCommand::Pause { active: true })
			.expect("pause mid-turn");
		harness.commands.send(sleep_command()).expect("deferred local command");
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Completed })
		})
		.await;
		let refused = tokio::time::timeout(Duration::from_secs(5), mailbox.next())
			.await
			.expect("refusal arrives")
			.expect("mailbox open");
		assert!(matches!(refused, HostAction::LocalRefused { ref draft, .. } if draft == "!sleep 20"));
		assert!(
			harness.events.try_iter().all(|event| !matches!(event, KernelEvent::ToolReady { .. })),
			"the deferred run never started"
		);
		harness.commands.send(HostCommand::Quit).expect("quit");
		tokio::time::timeout(Duration::from_secs(5), harness.run)
			.await
			.expect("controller exits")
			.expect("controller task")
			.expect("controller run");
	}
}
