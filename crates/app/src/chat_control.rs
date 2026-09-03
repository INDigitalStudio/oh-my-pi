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
		Outcome, Services,
		git::{GitOp, GitOutcome, GitPatchAction, GitPatchScope},
		hub::{AgentOp, AgentOutcome},
		services::{Mutation, Mutations, ServiceError, ServiceOutcome},
		sessions::SessionIndexOutcome,
	},
};
use omp_con::{Ctx, Severity};
use omp_core::{Str, Ulid};
use omp_dom::{Event, Handle, KnownTag, NodeSpec, Op, PropId, PropKey, Tag, Txn, Value};
use omp_driver::headless::kernel::{ComposedInference, KernelOptions, SessionHome};
use omp_journal::{EntryId, blob::BlobStore, data::Attachment};
use omp_proto::toolhost::v1::HookEventId;
use omp_session::{AttachmentInput, Session, SessionError, components::jobs};
use parking_lot::RwLock;

/// pi `background-tan-dispatch.md`.
const TAN_DISPATCH: &str = include_str!("../../chat/prompts/background-tan-dispatch.md");
/// pi `tan-context-switch.md`.
const TAN_CONTEXT: &str = include_str!("../../chat/prompts/tan-context-switch.md");
/// `<prompt kind>` of a `/queue` entry under `<queues><prompts>`.
const QUEUED: &str = "queued";
/// How often the idle loop commits settlements of agents it revived.
const REVIVED_POLL: std::time::Duration = std::time::Duration::from_millis(500);

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

fn store_attachments(
	blobs: &BlobStore,
	inputs: Vec<AttachmentInput>,
) -> Result<Vec<Attachment>, omp_journal::blob::Error> {
	inputs
		.into_iter()
		.map(|input| {
			blobs
				.put(&input.bytes)
				.map(|blob| Attachment { blob, mime: input.mime })
		})
		.collect()
}

/// What the idle loop does next after one command.
enum Flow {
	/// Keep waiting for commands.
	Idle,
	/// Run this turn.
	Turn(TurnInput),
	/// Re-run the aborted tool tail of the last turn (pi `retry()`).
	Retry,
	/// Run one tool without inference (`!` / `$` prefix modes).
	Local(omp_agent::LocalRun),
	/// Leave the controller.
	Quit,
}

/// Builds the tool call behind a `!` / `$` composer line (pi
/// `handleBashCommand` → `bash`, `handlePythonCommand` → `eval` with the Python
/// kernel).
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
	services:     Arc<dyn Services>,
	/// Environment authority (isolated workspaces for revived agents).
	env:          omp_env::EnvClient,
	up:           flume::Sender<Up>,
	live_journal: Arc<RwLock<PathBuf>>,
	data_dir:     PathBuf,
	voice:        crate::chat_voice::PushToTalk,
	paused:       bool,
	/// Commands that mutate the session, deferred while a turn runs.
	pending:      Vec<HostCommand>,
	tan_tx:       flume::Sender<TanDone>,
	tan_rx:       flume::Receiver<TanDone>,
	/// Agents the controller revived from the hub and still running: their
	/// settlement is journaled by the idle loop's poll tick, since no turn
	/// may follow to commit it.
	revived:      Vec<Str>,
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
		services: Arc<dyn Services>,
		env: omp_env::EnvClient,
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
			services,
			env,
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
			revived: Vec::new(),
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
				() = tokio::time::sleep(REVIVED_POLL), if !self.revived.is_empty() => {
					self.settle_revived()?;
					Flow::Idle
				},
			};
			match flow {
				Flow::Idle => {},
				Flow::Turn(input) => {
					let quit = self.run_turn(Some(input), &command_rx).await?
						|| self.after_turn(&command_rx).await?;
					if quit {
						self.shutdown()?;
						return Ok(());
					}
				},
				Flow::Retry => {
					let quit =
						self.run_turn(None, &command_rx).await? || self.after_turn(&command_rx).await?;
					if quit {
						self.shutdown()?;
						return Ok(());
					}
				},
				Flow::Local(run) => {
					let quit =
						self.run_local(run, &command_rx).await? || self.after_turn(&command_rx).await?;
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
			if !self.paused
				&& let Some(input) = self.pop_queued()?
			{
				let quit = self.run_turn(Some(input), &command_rx).await?
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
					self.queue_prompt(text, Vec::new())?;
					self.reply(Severity::Info, "Paused: prompt queued until you resume");
					return Ok(Flow::Idle);
				}
				self.record_loop_prompt(&text)?;
				Flow::Turn(TurnInput { text, attachments: Vec::new() })
			},
			HostCommand::SubmitWithAttachments { text, attachments } => {
				// The same seam ACP image blocks take: content-address the
				// bytes in the session store, journal the references.
				let attachments = self
					.session
					.store_attachments(attachments)
					.into_diagnostic()?;
				if self.paused {
					self.queue_prompt(text, attachments)?;
					self.reply(Severity::Info, "Paused: prompt queued until you resume");
					return Ok(Flow::Idle);
				}
				self.record_loop_prompt(&text)?;
				Flow::Turn(TurnInput { text, attachments })
			},
			HostCommand::Steer(text) => {
				let _ = self.up.send(Up::Steer { text, attachments: Vec::new() });
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
			HostCommand::Retry => {
				if self.paused {
					self.reply(Severity::Info, "Paused: resume before retrying");
					Flow::Idle
				} else {
					Flow::Retry
				}
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

	/// Runs one turn (`Some(input)`) or re-runs the last turn's aborted tool
	/// tail (`None`, pi `viewSession.retry()`), routing commands that arrive
	/// meanwhile: steering, interrupts, and approvals go to the kernel now;
	/// session mutations wait for the turn to end (ADR 0004: one writer per
	/// journal head). Returns whether the host asked to quit.
	async fn run_turn(
		&mut self,
		input: Option<TurnInput>,
		command_rx: &flume::Receiver<HostCommand>,
	) -> miette::Result<bool> {
		let mut quit = false;
		let ask = self.ask.clone();
		// The kernel holds the session for the turn; media steered in
		// meanwhile is content-addressed through this handle to the same
		// store (pi `steer(text, images)`).
		let blobs = self.session.blobs().clone();
		let failure = {
			let control = omp_agent::RunControl::default();
			let turn = match input {
				Some(input) => futures::future::Either::Left(self.kernel.run_turn(
					&mut self.session,
					input,
					control,
				)),
				None => futures::future::Either::Right(
					self.kernel.retry_tool_tail(&mut self.session, control),
				),
			};
			tokio::pin!(turn);
			loop {
				tokio::select! {
					result = &mut turn => break result.err(),
					command = command_rx.recv_async() => match command {
						Ok(HostCommand::Submit(text) | HostCommand::Steer(text)) => {
							let _ = self.up.send(Up::Steer { text, attachments: Vec::new() });
						},
						Ok(HostCommand::SubmitWithAttachments { text, attachments }) => {
							let stored = store_attachments(&blobs, attachments);
							match stored {
								Ok(attachments) => {
									let _ = self.up.send(Up::Steer { text, attachments });
								},
								// The turn keeps running; the failure is journaled
								// where the user sees it instead of silently dropping
								// the images with the aside.
								Err(error) => {
									let _ = self.up.send(Up::Env(omp_agent::EnvEvent::Notice {
										kind: Str::new_static("error"),
										name: None,
										body: Str::new(format!("Could not store the attached images: {error}")),
									}));
								},
							}
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
						Ok(HostCommand::Queue { prompt, attachments }) => {
							let stored = store_attachments(&blobs, attachments);
							match stored {
								Ok(attachments) => {
									let _ = self.up.send(Up::Queue { text: prompt, attachments });
								},
								Err(error) => {
									let _ = self.up.send(Up::Env(omp_agent::EnvEvent::Notice {
										kind: Str::new_static("error"),
										name: None,
										body: Str::new(format!("Could not store the queued images: {error}")),
									}));
								},
							}
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
					break turn.await.err();
				}
			}
		};
		if let Some(error) = failure {
			if matches!(error, omp_agent::KernelError::NothingToRetry) {
				// pi `input-controller.ts:1311`.
				self.reply(Severity::Info, "Nothing to retry");
				return Ok(quit);
			}
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
	async fn after_turn(
		&mut self,
		command_rx: &flume::Receiver<HostCommand>,
	) -> miette::Result<bool> {
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
				if effective
					.get("summarize")
					.and_then(serde_json::Value::as_bool)
					!= Some(false)
				{
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
			HostCommand::Queue { prompt, attachments } => {
				let attachments = self
					.session
					.store_attachments(attachments)
					.into_diagnostic()?;
				self.queue_prompt(prompt, attachments)?;
			},
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
				let _ = self.up.send(Up::Steer { text, attachments: Vec::new() });
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
				let result = self
					.services
					.sessions(scope)
					.map_err(|error| Str::new(error.to_string()));
				self.post_outcome(Outcome::SessionIndex(SessionIndexOutcome { scope, result }));
			},
			HostCommand::Git(op) => self.apply_git(op),
			HostCommand::Agent { id, op } => {
				let result = self.supervise_agent(&id, op.clone()).await;
				self.post_outcome(Outcome::Agent(AgentOutcome { id, op, result }));
			},
			// A retry deferred behind a running turn has nothing to re-run
			// once that turn settled; the idle path handles a live one.
			HostCommand::Retry => {},
			// Deferred local runs are drained by `after_turn`, never applied
			// as a plain session command.
			HostCommand::RunLocal { .. }
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

	/// Runs one Git workbench mutation against the project checkout on a
	/// blocking worker and answers with `Outcome::Git` (pi
	/// `git-workbench.ts` stage/unstage/apply/commit paths).
	fn apply_git(&self, op: GitOp) {
		let root = self.home.project_root.clone();
		let ctx = Arc::clone(&self.ctx);
		tokio::task::spawn_blocking(move || {
			let result = run_git(&root, &op);
			if let Some(mailbox) = ctx.user::<HostMailbox>() {
				mailbox.post(HostAction::Outcome(Outcome::Git(GitOutcome { op, result })));
			}
		});
	}

	/// Supervises one `<meta><jobs>` agent from the hub: `x` terminates a
	/// live one, `r` revives a finished one, and a message steers a running
	/// one or revives a finished one with the message as its next prompt (pi
	/// `agent-hub.ts` kill/revive and `agent-transcript-viewer.ts` send).
	async fn supervise_agent(&mut self, id: &str, op: AgentOp) -> Result<Str, ServiceError> {
		let jobs = Arc::clone(self.kernel.jobs());
		jobs.poll(&mut self.session).map_err(ServiceError::failed)?;
		let record = jobs
			.list()
			.into_iter()
			.find(|job| job.id.as_str() == id)
			.ok_or_else(|| {
				ServiceError::Failed(Str::new(format!("no agent \"{id}\" in this session")))
			})?;
		let running = matches!(record.status.as_str(), "running" | "starting");
		match op {
			AgentOp::Kill => {
				if !running {
					return Err(ServiceError::Failed(Str::new(format!(
						"Agent \"{id}\" is {} — nothing to kill",
						record.status
					))));
				}
				let terminated = jobs
					.terminate(&mut self.session, record.handle)
					.await
					.map_err(ServiceError::failed)?;
				if !terminated {
					return Err(ServiceError::Failed(Str::new(format!(
						"Agent \"{id}\" is not supervised by this kernel"
					))));
				}
				self.revived.retain(|revived| revived.as_str() != id);
				Ok(Str::new(format!("Killed {id}")))
			},
			AgentOp::Revive => {
				if running {
					return Err(ServiceError::Failed(Str::new(format!(
						"Agent \"{id}\" is running — only finished agents can be revived"
					))));
				}
				self.revive(id, None)?;
				Ok(Str::new(format!("Revived {id}")))
			},
			AgentOp::Send(text) => {
				if running {
					let live = self
						.home
						.live
						.lookup(&omp_driver::sessions::SessionId::new(Str::new(id)))
						.ok_or_else(|| {
							ServiceError::Failed(Str::new(format!(
								"Agent \"{id}\" is running but not addressable from this process"
							)))
						})?;
					live
						.up
						.send(Up::Steer { text, attachments: Vec::new() })
						.map_err(|_| {
							ServiceError::Failed(Str::new(format!("Agent \"{id}\" stopped listening")))
						})?;
					return Ok(Str::new(format!("Sent to {id}")));
				}
				self.revive(id, Some(text))?;
				Ok(Str::new(format!("Revived {id} with your message")))
			},
		}
	}

	/// Brings a settled agent back over its journal, optionally with a first
	/// prompt; its settlement is committed by the idle poll tick.
	fn revive(&mut self, id: &str, prompt: Option<Str>) -> Result<(), ServiceError> {
		let cfg_root = self.home.project_root.join(".omp");
		let cfg = move |name: &str| fs::read_to_string(cfg_root.join(name)).ok().map(Str::new);
		omp_driver::subagent::revive::revive_child(
			&mut self.session,
			omp_driver::subagent::revive::ReviveRequest {
				data_dir: &self.data_dir,
				project_root: &self.home.project_root,
				sessions_dir: &self.home.sessions_dir,
				sessions: &self.home.live,
				parent_ctx: &self.ctx,
				cfg: &cfg,
				jobs: self.kernel.jobs(),
				env: &self.env,
				model: self.home.model.as_str(),
				id,
				prompt,
			},
		)
		.map_err(ServiceError::failed)?;
		self.revived.push(Str::new(id));
		Ok(())
	}

	/// Commits settlements of revived agents and forgets the ones that ended.
	fn settle_revived(&mut self) -> miette::Result<()> {
		let records = self
			.kernel
			.jobs()
			.poll(&mut self.session)
			.into_diagnostic()?;
		self.revived.retain(|id| {
			records
				.iter()
				.any(|job| &job.id == id && matches!(job.status.as_str(), "running" | "starting"))
		});
		Ok(())
	}

	fn apply_mutation(&self, mutation: Mutation) {
		let pending = match self.mutations.apply(mutation.clone()) {
			Ok(pending) => pending,
			Err(error) => {
				self.post_outcome(Outcome::Service(ServiceOutcome { mutation, result: Err(error) }));
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
				mailbox
					.post(HostAction::Outcome(Outcome::Service(ServiceOutcome { mutation, result })));
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
		let blobs = self.session.blobs().clone();
		let failure = {
			let local =
				self
					.kernel
					.run_local(&mut self.session, run, omp_agent::RunControl::default());
			tokio::pin!(local);
			loop {
				tokio::select! {
					result = &mut local => break result.err(),
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
							let _ = self.up.send(Up::Steer { text, attachments: Vec::new() });
						},
						Ok(HostCommand::SubmitWithAttachments { text, attachments }) => {
							match store_attachments(&blobs, attachments) {
								Ok(attachments) => {
									let _ = self.up.send(Up::Steer { text, attachments });
								},
								Err(error) => {
									let _ = self.up.send(Up::Env(omp_agent::EnvEvent::Notice {
										kind: Str::new_static("error"),
										name: None,
										body: Str::new(format!(
											"Could not store the attached images: {error}"
										)),
									}));
								},
							}
						},
						Ok(HostCommand::AskAnswer { id, answers }) => answer_ask(&ask, &id, answers),
						Ok(HostCommand::Overlay { .. }) => {},
						Ok(other) => self.pending.push(other),
					},
				}
				if quit {
					break local.await.err();
				}
			}
		};
		if let Some(error) = failure {
			crate::chat_cmd::record_turn_failure(&mut self.session, &error).into_diagnostic()?;
		}
		Ok(quit)
	}

	/// Replaces the live session: the old one records a switch, the new
	/// one's subscription is relayed after exactly one `Reset`.
	async fn switch_to(&mut self, mut next: Session, reason: &'static str) -> miette::Result<()> {
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
		self.kernel.set_debug_session(
			self
				.session
				.journal_path()
				.file_stem()
				.and_then(|name| name.to_str())
				.map(Str::new),
		);
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
			facts: self.home.facts.clone(),
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

	/// Journals a `/queue` prompt under `<queues><prompts>`; its attachments
	/// (already content-addressed) ride the same `data` prop a `msg.user@1`
	/// fold writes, so the pop that starts the turn hands them on typed.
	fn queue_prompt(
		&mut self,
		prompt: Str,
		attachments: Vec<omp_journal::data::Attachment>,
	) -> miette::Result<()> {
		let dom = self.session.dom();
		let prompts = prompts_root(dom).ok_or_else(|| miette!("session has no prompt queue"))?;
		let id = Str::new(format!("queued-{}", Ulid::generate()));
		let mut node = NodeSpec::new(KnownTag::Prompt)
			.with_prop(PropId::Kind, Value::Str(Str::new_static(QUEUED)))
			.with_prop(PropId::Id, Value::Str(id))
			.with_prop(PropId::Status, Value::Str(Str::new_static("pending")))
			.with_content(prompt);
		if !attachments.is_empty() {
			let raw = serde_json::value::to_raw_value(&attachments).into_diagnostic()?;
			node = node.with_prop(PropId::Data, Value::Json(raw));
		}
		let cause = self.head()?;
		self
			.session
			.patch(Txn {
				cause,
				label: Some(Str::new_static("queue.push")),
				ops: vec![Op::Ins {
					parent: prompts,
					after: dom.children(prompts).last().copied(),
					node,
				}],
			})
			.into_diagnostic()?;
		Ok(())
	}

	/// Takes the oldest pending `/queue` prompt with its attachments,
	/// marking it sent.
	fn pop_queued(&mut self) -> miette::Result<Option<TurnInput>> {
		let Some((text, attachments)) =
			omp_agent::pop_queued_prompt(&mut self.session).into_diagnostic()?
		else {
			return Ok(None);
		};
		self.record_loop_prompt(&text)?;
		Ok(Some(TurnInput { text, attachments }))
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
		use omp_agent::directors::{
			advisor::Advisor, force_tool::ForceTool, goal::Goal, loop_mode::LoopMode, vibe::Vibe,
		};
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
			"advisor" => Box::new(Advisor::new()),
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
				let count = args.first().and_then(|value| value.parse::<u32>().ok());
				let prompt = args.get(1).cloned().unwrap_or_default();
				Box::new(LoopMode::new(prompt, count))
			},
			"force_tool" => {
				let tool = args
					.first()
					.cloned()
					.ok_or(DirectorFailure::MissingArgument("tool"))?;
				if self
					.kernel
					.tool_registry()
					.live_spec(tool.as_str())
					.is_err()
				{
					return Err(DirectorFailure::UnknownTool(tool));
				}
				Box::new(ForceTool::new(tool.clone(), omp_agent::ForceUntil::ToolCalled(tool), None, 3))
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
							if method == CompactionMethod::Handoff {
								"Handoff"
							} else {
								"Compaction"
							}
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
				let Some(node) = dom.get(*handle) else {
					continue;
				};
				match (mode, &node.tag) {
					(ShakeMode::Elide, Tag::Custom(_)) => {
						for child in dom.children(*handle) {
							let Some(part) = dom.get(*child) else {
								continue;
							};
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
							ops.push(Op::Set {
								h:     *child,
								prop:  PropId::Data.into(),
								value: Value::Null,
							});
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
							ops.push(Op::Set {
								h:     *handle,
								prop:  PropId::Data.into(),
								value: Value::Null,
							});
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
		let _ = self
			.up
			.send(Up::Steer { text: Str::new(breadcrumb), attachments: Vec::new() });
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
			let composed = omp_driver::headless::kernel::compose_kernel(
				&data_dir,
				&project,
				model.as_str(),
				ctx,
				options,
			)
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
		let preview = done.answer.lines().next().unwrap_or_default();
		self.reply(
			if done.ok {
				Severity::Info
			} else {
				Severity::Warn
			},
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
				.map(|(handle, ..)| *handle)
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
					vec![Op::Ins {
						parent: todo,
						after: items.last().map(|(handle, ..)| *handle),
						node,
					}],
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
					|| items.iter().map(|(handle, ..)| *handle).collect::<Vec<_>>(),
					matches,
				);
				if found.is_empty() {
					return Err(TodoFailure::NoMatch(text.unwrap_or_default()));
				}
				("todo.done", set_status(&found, "completed"), "Completed".to_owned())
			},
			TodoOp::Drop(text) => {
				let found = text.as_deref().map_or_else(
					|| items.iter().map(|(handle, ..)| *handle).collect::<Vec<_>>(),
					matches,
				);
				if found.is_empty() {
					return Err(TodoFailure::NoMatch(text.unwrap_or_default()));
				}
				("todo.drop", set_status(&found, "abandoned"), "Dropped".to_owned())
			},
			TodoOp::Remove(text) => {
				let found = text.as_deref().map_or_else(
					|| items.iter().map(|(handle, ..)| *handle).collect::<Vec<_>>(),
					matches,
				);
				if found.is_empty() {
					return Err(TodoFailure::NoMatch(text.unwrap_or_default()));
				}
				("todo.rm", found.iter().map(|handle| Op::Rm(*handle)).collect(), "Removed".to_owned())
			},
			TodoOp::Import(path) => {
				let path =
					path.map_or_else(|| PathBuf::from("TODO.md"), |path| PathBuf::from(path.as_str()));
				let text =
					fs::read_to_string(&path).map_err(|error| TodoFailure::Io(error.to_string()))?;
				let mut ops = items
					.iter()
					.map(|(handle, ..)| Op::Rm(*handle))
					.collect::<Vec<_>>();
				let mut phase = Str::new_static("Tasks");
				let mut count = 0usize;
				for line in text.lines() {
					let line = line.trim();
					if let Some(heading) = line.strip_prefix("## ") {
						phase = Str::new(heading.trim());
						continue;
					}
					let Some(rest) = line.strip_prefix("- [") else {
						continue;
					};
					let Some((mark, label)) = rest.split_once(']') else {
						continue;
					};
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
							.with_prop(
								PropKey::Custom(Str::new_static("phase")),
								Value::Str(phase.clone()),
							),
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

/// Executes one Git workbench mutation on the checkout containing `root`
/// and returns the workbench's status line.
fn run_git(root: &std::path::Path, op: &GitOp) -> Result<Str, ServiceError> {
	use omp_vcs::{ApplyOptions, CommitOptions, RestoreOptions, git::GitRepo};
	let repo = GitRepo::require(root).map_err(ServiceError::failed)?;
	let owned = |paths: &Option<Vec<Str>>| {
		paths
			.as_ref()
			.map(|paths| paths.iter().map(ToString::to_string).collect::<Vec<_>>())
			.unwrap_or_default()
	};
	match op {
		GitOp::Stage(paths) => {
			let paths = owned(paths);
			repo.stage_files(&paths).map_err(ServiceError::failed)?;
			Ok(match paths.as_slice() {
				[] => Str::new_static("Staged all changes"),
				[path] => Str::new(format!("Staged {path}")),
				paths => Str::new(format!("Staged {} files", paths.len())),
			})
		},
		GitOp::Unstage(paths) => {
			let paths = owned(paths);
			repo.unstage(&paths).map_err(ServiceError::failed)?;
			Ok(match paths.as_slice() {
				[] => Str::new_static("Unstaged all changes"),
				[path] => Str::new(format!("Unstaged {path}")),
				paths => Str::new(format!("Unstaged {} files", paths.len())),
			})
		},
		GitOp::Apply { patch, action, scope } => {
			let options = ApplyOptions {
				cached:     *action != GitPatchAction::Discard,
				index_path: None,
				reverse:    *action != GitPatchAction::Stage,
				three_way:  false,
			};
			repo
				.apply_patch(patch.as_str(), &options)
				.map_err(ServiceError::failed)?;
			let verb = match action {
				GitPatchAction::Stage => "Staged",
				GitPatchAction::Unstage => "Unstaged",
				GitPatchAction::Discard => "Discarded",
			};
			let scope = match scope {
				GitPatchScope::Selection => "selection",
				GitPatchScope::Hunk => "hunk",
			};
			Ok(Str::new(format!("{verb} {scope}")))
		},
		GitOp::Discard(paths) => {
			let files = paths.iter().map(ToString::to_string).collect::<Vec<_>>();
			repo
				.restore(&RestoreOptions { source: None, staged: false, worktree: true, files })
				.map_err(ServiceError::failed)?;
			Ok(match paths.as_slice() {
				[path] => Str::new(format!("Discarded {path}")),
				paths => Str::new(format!("Discarded {} files", paths.len())),
			})
		},
		GitOp::Commit { message, amend, stage_all } => {
			if *stage_all {
				repo.stage_files(&[]).map_err(ServiceError::failed)?;
			}
			let sha = repo
				.commit_create(message.as_str(), &CommitOptions {
					amend: *amend,
					..CommitOptions::default()
				})
				.map_err(ServiceError::failed)?;
			let short = &sha[..sha.len().min(7)];
			Ok(Str::new(if *amend {
				format!("Amended {short}")
			} else {
				format!("Committed {short}")
			}))
		},
	}
}

/// Forwards one session's DOM events onto the host's relay until the
/// session is dropped.
fn forward(
	events: flume::Receiver<Event>,
	relay: flume::Sender<Event>,
) -> tokio::task::JoinHandle<()> {
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
		hooks::{GateDecision, HookGate, HookPhase, OnFailure, SourceRef, Subscription, When},
	};
	use omp_chat::{
		composer::{LocalInput, PrefixMode},
		overlays::services::SessionScope,
	};
	use omp_inference::{
		BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
		ProviderId, RequestId, ResponseMeta, RouteId, ToolCall, ToolCallId, Usage, call::OpaqueJson,
	};
	use omp_tool::{
		Claims, Constraint, Effects, Ev, IncomingParams, Part, Precedence, Presentation, PromptCaps,
		Registry, Rev, Tool, ToolSpec, ToolTerminal,
	};
	use serde::{Deserialize, Serialize};

	use super::*;

	/// Provider call id of the scripted `bash` call.
	const SCRIPTED_CALL: &str = "scripted-bash-1";

	/// What the scripted provider answers.
	#[derive(Clone, Copy)]
	enum Script {
		/// `pong` on every request.
		Text,
		/// A `bash` tool call on the first request, `pong` afterwards.
		BashThenText,
	}

	/// One scripted answer per request, delivered after `delay` so a command
	/// sent right behind the prompt is provably received mid-turn.
	struct SlowInference {
		delay:    Duration,
		script:   Script,
		requests: usize,
	}

	impl omp_agent::Inference for SlowInference {
		fn chat(
			&mut self,
			_request: ChatRequest,
		) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send {
			let delay = self.delay;
			let tool_call = matches!(self.script, Script::BashThenText) && self.requests == 0;
			self.requests += 1;
			async move {
				tokio::time::sleep(delay).await;
				let started = ChatEvent::Started(ResponseMeta {
					request_id:          RequestId::from("scripted-request"),
					provider:            ProviderId::from("scripted"),
					route:               RouteId::from("scripted/test"),
					model:               None,
					provider_request_id: None,
					created_at:          SystemTime::UNIX_EPOCH,
				});
				let events = if tool_call {
					vec![
						started,
						ChatEvent::ToolCallStarted {
							index: 0,
							id:    ToolCallId::from(SCRIPTED_CALL),
							name:  Str::new_static("bash"),
						},
						ChatEvent::ToolCallReady {
							index: 0,
							call:  ToolCall {
								id:        ToolCallId::from(SCRIPTED_CALL),
								name:      Str::new_static("bash"),
								arguments: OpaqueJson::new(serde_json::json!({ "command": "sleep 20" })),
							},
						},
						ChatEvent::Completed(Completion {
							reason:  FinishReason::ToolCalls,
							blocks:  1,
							usage:   Usage::default(),
							receipt: ExecutionReceipt::default().into(),
						}),
					]
				} else {
					vec![
						started,
						ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
						ChatEvent::TextDelta { index: 0, text: Str::new_static("pong") },
						ChatEvent::Completed(Completion {
							reason:  FinishReason::Stop,
							blocks:  1,
							usage:   Usage::default(),
							receipt: ExecutionReceipt::default().into(),
						}),
					]
				};
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

		fn prompt(
			&self,
			view: Result<&Self::Payload, &Self::Fault>,
			_caps: &PromptCaps,
		) -> Vec<Part> {
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
				layer:        0,
				publisher:    Str::new_static("omp-app"),
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
		/// The controller's journal, readable once the controller exited.
		journal:  PathBuf,
		/// The live-session routing index the controller resolves agents in.
		live:     Arc<omp_driver::sessions::SessionRegistry>,
		dir:      tempfile::TempDir,
	}

	impl Harness {
		fn mailbox(&self) -> Arc<HostMailbox> {
			self.ctx.user::<HostMailbox>().expect("mailbox")
		}

		/// Quits the controller and waits for its run to settle; the journal
		/// path stays readable while the returned directory lives.
		async fn quit(self) -> (PathBuf, tempfile::TempDir) {
			self.commands.send(HostCommand::Quit).expect("quit");
			tokio::time::timeout(Duration::from_secs(5), self.run)
				.await
				.expect("controller exits")
				.expect("controller task")
				.expect("controller run");
			(self.journal, self.dir)
		}
	}

	/// The controller's tempdir, scripted provider, service feeds, and a
	/// hook that prepares the session and kernel before the controller
	/// takes them.
	struct HarnessSpec {
		dir:      tempfile::TempDir,
		script:   Script,
		delay:    Duration,
		services: Arc<dyn Services>,
	}

	impl HarnessSpec {
		fn new(script: Script, delay: Duration) -> Self {
			Self {
				dir: tempfile::tempdir().expect("temp dir"),
				script,
				delay,
				services: Arc::new(NoServices),
			}
		}

		fn services(mut self, services: Arc<dyn Services>) -> Self {
			self.services = services;
			self
		}

		fn build(self) -> Harness {
			self.build_with(|_, _| {})
		}

		fn build_with(self, prepare: impl FnOnce(&mut Session, &Kernel<SlowInference>)) -> Harness {
			let Self { dir, script, delay, services } = self;
			let mut kernel = Kernel::new(
				SlowInference { delay, script, requests: 0 },
				SleepingBash::registry(),
				DispatchPolicy::new(
					omp_journal::blob::BlobStore::open(dir.path().join("blobs")).expect("blob store"),
				),
				StaticPrompt(Str::new_static("test system")),
			);
			let events = kernel.subscribe();
			let live = Arc::new(omp_driver::sessions::SessionRegistry::new());
			let home = SessionHome {
				sessions_dir: dir.path().join("sessions"),
				project_root: dir.path().to_path_buf(),
				model:        Str::new_static("test/model"),
				prompt:       omp_driver::headless::kernel::PromptOverrides::default(),
				facts:        Default::default(),
				live:         Arc::clone(&live),
				up:           kernel.mailbox(),
			};
			fs::create_dir_all(&home.sessions_dir).expect("sessions dir");
			let mut session = home.create(None).expect("session");
			prepare(&mut session, &kernel);
			let journal = session.journal_path().to_path_buf();
			let (relay, _dom_events) = flume::unbounded();
			let ctx = Arc::new(HostMailbox::new().attach(Ctx::builder()).build());
			let live_journal = Arc::new(RwLock::new(journal.clone()));
			let (controller, _snapshot) = Controller::new(
				kernel,
				session,
				home,
				relay,
				Arc::clone(&ctx),
				Arc::new(omp_chat::overlays::services::NoMutations),
				services,
				detached_env(),
				live_journal,
				dir.path().join("data"),
				None,
				omp_driver::headless::AskRoute::new(),
			);
			let (commands, command_rx) = flume::unbounded();
			let run = tokio::spawn(controller.run(command_rx));
			Harness { commands, events, ctx, run, journal, live, dir }
		}
	}

	/// A detached environment client: no controller test reaches the
	/// Environment, so its frames go nowhere.
	fn detached_env() -> omp_env::EnvClient {
		let (outgoing, _requests) = flume::unbounded();
		let (_responses, incoming) = flume::unbounded();
		omp_env::EnvClient::from_channels(outgoing, incoming)
	}

	/// Read feeds no controller test exercises beyond the defaults.
	struct NoServices;

	impl Services for NoServices {}

	fn harness(inference_delay: Duration) -> Harness {
		HarnessSpec::new(Script::Text, inference_delay).build()
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

	/// The next settled controller outcome posted to the host mailbox.
	async fn next_outcome(mailbox: &HostMailbox) -> Outcome {
		tokio::time::timeout(Duration::from_secs(10), async {
			loop {
				match mailbox.next().await.expect("mailbox open") {
					HostAction::Outcome(outcome) => return outcome,
					_ => continue,
				}
			}
		})
		.await
		.expect("outcome arrives in time")
	}

	/// The next console reply line posted to the host mailbox.
	async fn next_reply(mailbox: &HostMailbox) -> (Severity, Str) {
		tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				match mailbox.next().await.expect("mailbox open") {
					HostAction::Reply { severity, text } => return (severity, text),
					_ => continue,
				}
			}
		})
		.await
		.expect("reply arrives in time")
	}

	fn git(root: &std::path::Path, args: &[&str]) -> String {
		let output = std::process::Command::new("git")
			.args(args)
			.current_dir(root)
			.output()
			.expect("git runs");
		assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
		String::from_utf8(output.stdout).expect("utf-8 git output")
	}

	/// Initializes a repository with one committed `a.txt` at `root`.
	fn init_repo(root: &std::path::Path) {
		git(root, &["init", "-q", "-b", "main"]);
		git(root, &["config", "user.email", "t@example.com"]);
		git(root, &["config", "user.name", "t"]);
		fs::write(root.join("a.txt"), "one\n").expect("write a.txt");
		git(root, &["add", "a.txt"]);
		git(root, &["commit", "-q", "-m", "init"]);
	}

	/// Journal entries of a settled controller session.
	fn journal_entries(path: &std::path::Path) -> Vec<omp_journal::Entry> {
		omp_journal::Journal::scan(path).expect("journal scans")
	}

	fn subagent_status(dom: &omp_dom::Dom, id: &str) -> Option<Str> {
		dom.select("meta jobs subagent")
			.expect("selector")
			.filter_map(|handle| dom.get(handle))
			.find(|node| prop_str(node, PropId::Id) == Some(id))
			.and_then(|node| prop_str(node, PropId::Status).map(Str::new))
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
			SlowInference { delay: Duration::ZERO, script: Script::Text, requests: 0 },
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
			model:        Str::new_static("test/model"),
			prompt:       omp_driver::headless::kernel::PromptOverrides::default(),
			facts:        Default::default(),
			live:         Arc::new(omp_driver::sessions::SessionRegistry::new()),
			up:           kernel.mailbox(),
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
			Arc::new(NoServices),
			detached_env(),
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
		assert_eq!(order_rx.try_iter().collect::<Vec<_>>(), ["before", "flush", "resync", "after"],);
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
		harness
			.commands
			.send(sleep_command())
			.expect("local command");
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Completed })
		})
		.await;
		next_event(
			&harness.events,
			|event| matches!(event, KernelEvent::ToolReady { name, .. } if name == "bash"),
		)
		.await;
		harness
			.commands
			.send(HostCommand::Interrupt)
			.expect("interrupt");
		let ended =
			next_event(&harness.events, |event| matches!(event, KernelEvent::TurnEnded { .. })).await;
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

	/// An image prompt typed while a local command runs is re-queued for the
	/// next model turn with its bytes and MIME intact.
	#[tokio::test]
	async fn image_prompt_during_local_run_reaches_the_next_turn() {
		const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x04\0\0\0\x03";
		let harness = harness(Duration::from_millis(50));
		harness.commands.send(sleep_command()).expect("local command");
		next_event(
			&harness.events,
			|event| matches!(event, KernelEvent::ToolReady { name, .. } if name == "bash"),
		)
		.await;
		harness
			.commands
			.send(HostCommand::SubmitWithAttachments {
				text:        Str::new_static("inspect [Image #1]"),
				attachments: vec![AttachmentInput {
					mime:  Str::new_static("image/png"),
					bytes: bytes::Bytes::from_static(PNG),
				}],
			})
			.expect("image prompt");
		harness
			.commands
			.send(HostCommand::Interrupt)
			.expect("interrupt local run");
		// This command is deferred by `run_local`; observing its outcome proves
		// the controller has returned to its idle loop before the next submit.
		harness
			.commands
			.send(HostCommand::SessionIndex { scope: SessionScope::Project })
			.expect("idle barrier");
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Cancelled })
		})
		.await;
		let _ = next_outcome(&harness.mailbox()).await;
		harness
			.commands
			.send(HostCommand::Submit(Str::new_static("continue")))
			.expect("next model turn");
		next_event(&harness.events, |event| matches!(event, KernelEvent::TurnEnded { .. })).await;

		let (journal, _dir) = harness.quit().await;
		let session = Session::open(&journal, omp_session::ComponentRegistry::standard())
			.expect("journal replays");
		let dom = session.dom();
		let steering = dom
			.select("user[steering=true]")
			.expect("selector")
			.find(|handle| {
				dom.get(*handle)
					.and_then(|node| node.content.as_deref())
					== Some("inspect [Image #1]")
			})
			.expect("image prompt is not dropped");
		let node = dom.get(steering).expect("steering node");
		let Some(Value::Json(raw)) = node.prop(&PropKey::from(PropId::Data)) else {
			panic!("steering prompt carries attachment data: {node:?}");
		};
		let attachments: Vec<Attachment> =
			serde_json::from_str(raw.get()).expect("attachment json");
		assert_eq!(attachments.len(), 1);
		assert_eq!(attachments[0].mime, "image/png");
		assert_eq!(
			session
				.blobs()
				.get(&attachments[0].blob)
				.expect("image blob")
				.as_ref(),
			PNG
		);
	}

	/// A `/queue` prompt posted while a turn runs is journaled as a pending
	/// `<prompt kind=queued>` under `<queues><prompts>` (pi `followUp`:
	/// "for when the agent yields") — never smuggled into the running turn
	/// as `<user steering>` — and runs as its own turn once the current one
	/// ends, carrying the images queued with it (pi `followUp(text,
	/// images)`).
	#[tokio::test]
	async fn queue_during_a_turn_waits_as_a_pending_prompt_and_never_steers() {
		const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x04\0\0\0\x03";
		let harness = harness(Duration::from_millis(300));
		harness
			.commands
			.send(HostCommand::Submit(Str::new_static("hello")))
			.expect("submit");
		next_event(&harness.events, |event| matches!(event, KernelEvent::InferenceStarted)).await;
		harness
			.commands
			.send(HostCommand::Queue {
				prompt:      Str::new_static("later [Image #1]"),
				attachments: vec![omp_session::AttachmentInput {
					mime:  Str::new_static("image/png"),
					bytes: bytes::Bytes::from_static(PNG),
				}],
			})
			.expect("queue");
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Completed })
		})
		.await;
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Completed })
		})
		.await;
		let (journal, _dir) = harness.quit().await;
		let session = Session::open(&journal, omp_session::ComponentRegistry::standard())
			.expect("journal replays");
		let dom = session.dom();
		assert_eq!(
			dom.count("user[steering=true]").expect("selector"),
			0,
			"a queued prompt is never a steering aside"
		);
		let queued = dom
			.select("prompt[kind=queued]")
			.expect("selector")
			.collect::<Vec<_>>();
		let [queued] = queued.as_slice() else {
			panic!("exactly one queued prompt is journaled: {queued:?}");
		};
		let node = dom.get(*queued).expect("queued prompt node");
		assert_eq!(node.content.as_deref(), Some("later [Image #1]"));
		assert_eq!(prop_str(node, PropId::Status), Some("sent"), "popped once the turn ended");
		let turns = dom.children(dom.body());
		assert_eq!(turns.len(), 2, "the queued prompt ran as its own turn");
		let user = dom
			.children(turns[1])
			.iter()
			.filter_map(|handle| dom.get(*handle))
			.find(|node| node.tag == Tag::Known(KnownTag::User))
			.expect("second turn opens with the queued prompt");
		assert_eq!(user.content.as_deref(), Some("later [Image #1]"));
		let Some(Value::Json(raw)) = user.prop(&PropKey::from(PropId::Data)) else {
			panic!("the popped turn carries the queued attachment: {user:?}");
		};
		let attachments: Vec<omp_journal::data::Attachment> =
			serde_json::from_str(raw.get()).expect("attachment json");
		assert_eq!(attachments.len(), 1);
		assert_eq!(attachments[0].mime, "image/png");
		assert_eq!(
			session
				.blobs()
				.get(&attachments[0].blob)
				.expect("queued image blob")
				.as_ref(),
			PNG,
			"the queued bytes were content-addressed once and reach the turn"
		);
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
		harness
			.commands
			.send(sleep_command())
			.expect("local command");
		let refused = tokio::time::timeout(Duration::from_secs(5), mailbox.next())
			.await
			.expect("refusal arrives")
			.expect("mailbox open");
		assert_eq!(refused, HostAction::LocalRefused {
			draft:  Str::new_static("!sleep 20"),
			reason: Str::new_static("Paused: resume before running local commands"),
		});
		assert!(
			harness
				.events
				.try_iter()
				.all(|event| !matches!(event, KernelEvent::ToolReady { .. })),
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
		harness
			.commands
			.send(sleep_command())
			.expect("deferred local command");
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Completed })
		})
		.await;
		let refused = tokio::time::timeout(Duration::from_secs(5), mailbox.next())
			.await
			.expect("refusal arrives")
			.expect("mailbox open");
		assert!(
			matches!(refused, HostAction::LocalRefused { ref draft, .. } if draft == "!sleep 20")
		);
		assert!(
			harness
				.events
				.try_iter()
				.all(|event| !matches!(event, KernelEvent::ToolReady { .. })),
			"the deferred run never started"
		);
		harness.commands.send(HostCommand::Quit).expect("quit");
		tokio::time::timeout(Duration::from_secs(5), harness.run)
			.await
			.expect("controller exits")
			.expect("controller task")
			.expect("controller run");
	}

	/// `HostCommand::Git` mutates the project checkout and answers with
	/// `Outcome::Git` (pi `git-workbench.ts`): stage + commit land a real
	/// commit, and discard restores the worktree from the index.
	#[tokio::test]
	async fn git_stage_commit_and_discard_mutate_the_checkout_and_settle_as_outcomes() {
		let spec = HarnessSpec::new(Script::Text, Duration::ZERO);
		init_repo(spec.dir.path());
		let harness = spec.build();
		let root = harness.dir.path().to_path_buf();
		let mailbox = harness.mailbox();
		fs::write(root.join("a.txt"), "two\n").expect("edit a.txt");

		let stage = GitOp::Stage(Some(vec![Str::new_static("a.txt")]));
		harness
			.commands
			.send(HostCommand::Git(stage.clone()))
			.expect("stage");
		let Outcome::Git(outcome) = next_outcome(&mailbox).await else {
			panic!("stage settles as a Git outcome");
		};
		assert_eq!(outcome, GitOutcome {
			op:     stage,
			result: Ok(Str::new_static("Staged a.txt")),
		});
		assert_eq!(git(&root, &["diff", "--cached", "--name-only"]).trim(), "a.txt");

		let commit =
			GitOp::Commit { message: Str::new_static("bump a"), amend: false, stage_all: false };
		harness
			.commands
			.send(HostCommand::Git(commit.clone()))
			.expect("commit");
		let Outcome::Git(outcome) = next_outcome(&mailbox).await else {
			panic!("commit settles as a Git outcome");
		};
		assert_eq!(outcome.op, commit);
		let line = outcome.result.expect("commit succeeds");
		let short = line
			.strip_prefix("Committed ")
			.expect("commit status line names the sha");
		let head = git(&root, &["rev-parse", "HEAD"]);
		assert!(head.starts_with(short.as_str()), "HEAD {head} is the committed {short}");
		assert_eq!(git(&root, &["log", "-1", "--format=%s"]).trim(), "bump a");
		assert_eq!(git(&root, &["status", "--porcelain", "a.txt"]).trim(), "", "clean after commit");

		fs::write(root.join("a.txt"), "three\n").expect("dirty a.txt");
		let discard = GitOp::Discard(vec![Str::new_static("a.txt")]);
		harness
			.commands
			.send(HostCommand::Git(discard.clone()))
			.expect("discard");
		let Outcome::Git(outcome) = next_outcome(&mailbox).await else {
			panic!("discard settles as a Git outcome");
		};
		assert_eq!(outcome, GitOutcome {
			op:     discard,
			result: Ok(Str::new_static("Discarded a.txt")),
		});
		assert_eq!(fs::read_to_string(root.join("a.txt")).expect("a.txt"), "two\n");
		harness.quit().await;
	}

	/// The Agent Hub supervises `<meta><jobs>` children through the
	/// controller: a message to a running child lands as `Up::Steer` on its
	/// kernel mailbox, `Kill` journals the child as `cancelled`, and a
	/// second kill has nothing to do.
	#[tokio::test]
	async fn agent_send_steers_the_live_child_and_kill_journals_it_cancelled() {
		let (child_up, child_inbox) = flume::unbounded::<Up>();
		let cancel = tokio_util::sync::CancellationToken::new();
		let harness = HarnessSpec::new(Script::Text, Duration::ZERO).build_with(|session, kernel| {
			let cause = session.head().expect("head");
			let txn = jobs::insert(session.dom(), cause, jobs::JobSpec {
				id:      Str::new_static("child"),
				kind:    Str::new_static("subagent"),
				owner:   Str::new_static("Main"),
				started: Str::new_static("0"),
				agent:   Some(Str::new_static("task")),
			})
			.expect("jobs component");
			session.patch(txn).expect("journal the child");
			let handle = session
				.dom()
				.select("meta jobs subagent")
				.expect("selector")
				.next()
				.expect("child element");
			let unit = cancel.clone();
			let attached = kernel.jobs().attach_task(
				session.dom(),
				handle,
				cancel.clone(),
				tokio::spawn(async move {
					unit.cancelled().await;
					omp_agent::JobSettlement {
						status: Str::new_static("completed"),
						output: None,
						error:  None,
					}
				}),
			);
			assert!(attached, "the child has a runtime kill boundary");
		});
		harness
			.live
			.register(Str::new_static("child"), omp_driver::sessions::KernelHandle {
				id:       omp_driver::sessions::SessionId::new("child"),
				name:     Str::new_static("child"),
				up:       child_up,
				snapshot: Arc::new(RwLock::new(omp_dom::Dom::new().snapshot())),
			});
		let mailbox = harness.mailbox();

		let send = AgentOp::Send(Str::new_static("please adjust"));
		harness
			.commands
			.send(HostCommand::Agent { id: Str::new_static("child"), op: send.clone() })
			.expect("send");
		let Outcome::Agent(outcome) = next_outcome(&mailbox).await else {
			panic!("send settles as an Agent outcome");
		};
		assert_eq!(outcome, AgentOutcome {
			id:     Str::new_static("child"),
			op:     send,
			result: Ok(Str::new_static("Sent to child")),
		});
		let steer = child_inbox
			.try_recv()
			.expect("the child's mailbox received the message");
		assert!(matches!(&steer, Up::Steer { text, .. } if text == "please adjust"), "{steer:?}");

		harness
			.commands
			.send(HostCommand::Agent { id: Str::new_static("child"), op: AgentOp::Kill })
			.expect("kill");
		let Outcome::Agent(outcome) = next_outcome(&mailbox).await else {
			panic!("kill settles as an Agent outcome");
		};
		assert_eq!(outcome.result, Ok(Str::new_static("Killed child")));
		assert!(cancel.is_cancelled(), "the runtime unit was cancelled");

		harness
			.commands
			.send(HostCommand::Agent { id: Str::new_static("child"), op: AgentOp::Kill })
			.expect("second kill");
		let Outcome::Agent(outcome) = next_outcome(&mailbox).await else {
			panic!("second kill settles as an Agent outcome");
		};
		assert!(
			matches!(&outcome.result, Err(ServiceError::Failed(reason)) if reason.contains("cancelled")),
			"a settled child cannot be killed again: {:?}",
			outcome.result
		);

		let (journal, _dir) = harness.quit().await;
		let session =
			Session::open(&journal, omp_session::ComponentRegistry::standard()).expect("reopen");
		assert_eq!(
			subagent_status(session.dom(), "child").as_deref(),
			Some("cancelled"),
			"the kill is a journaled fact"
		);
	}

	/// `Services` over explicit index roots (the production feed's seam).
	struct IndexServices {
		data_dir:     PathBuf,
		sessions_dir: PathBuf,
		state_dir:    PathBuf,
	}

	impl Services for IndexServices {
		fn sessions(
			&self,
			scope: omp_chat::overlays::services::SessionScope,
		) -> omp_chat::overlays::services::ServiceResult<Vec<omp_chat::overlays::services::SessionRow>>
		{
			crate::chat_services::sessions::rows_in(
				&self.data_dir,
				&self.sessions_dir,
				&self.state_dir,
				scope,
			)
		}
	}

	/// `HostCommand::SessionIndex` answers with the scoped index: the
	/// all-projects scope lists journals from every `projects/*/sessions`
	/// directory plus this project's, the project scope only this one.
	#[tokio::test]
	async fn session_index_all_projects_scope_lists_every_project_directory() {
		use omp_chat::overlays::services::SessionScope;

		let spec = HarnessSpec::new(Script::Text, Duration::ZERO);
		let data_dir = spec.dir.path().join("data");
		for (project, id) in [("alpha", "alpha-1"), ("beta", "beta-1")] {
			let path = data_dir
				.join("projects")
				.join(project)
				.join("sessions")
				.join(format!("{id}.oms"));
			fs::create_dir_all(path.parent().expect("parent")).expect("project sessions dir");
			let mut session =
				Session::create(&path, omp_session::ComponentRegistry::standard()).expect("journal");
			session.begin_turn().expect("turn");
			session
				.user(Str::new(format!("work in {project}")), Vec::new())
				.expect("user");
		}
		let services = Arc::new(IndexServices {
			data_dir:     data_dir.clone(),
			sessions_dir: spec.dir.path().join("sessions"),
			state_dir:    spec.dir.path().to_path_buf(),
		});
		let harness = spec.services(services).build();
		let own = harness
			.journal
			.file_stem()
			.and_then(|stem| stem.to_str())
			.map(Str::new)
			.expect("own session id");
		let mailbox = harness.mailbox();

		harness
			.commands
			.send(HostCommand::SessionIndex { scope: SessionScope::All })
			.expect("index all");
		let Outcome::SessionIndex(outcome) = next_outcome(&mailbox).await else {
			panic!("the index settles as a SessionIndex outcome");
		};
		assert_eq!(outcome.scope, SessionScope::All);
		let rows = outcome.result.expect("index succeeds");
		let mut ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
		ids.sort();
		let mut expected = vec![Str::new_static("alpha-1"), Str::new_static("beta-1"), own.clone()];
		expected.sort();
		assert_eq!(ids, expected);
		assert!(
			rows
				.iter()
				.any(|row| row.id == "alpha-1" && row.path.starts_with(data_dir.join("projects/alpha"))),
			"rows keep their project's journal path"
		);
		assert_eq!(
			rows
				.iter()
				.find(|row| row.id == "alpha-1")
				.and_then(|row| row.title.as_deref()),
			Some("work in alpha")
		);

		harness
			.commands
			.send(HostCommand::SessionIndex { scope: SessionScope::Project })
			.expect("index project");
		let Outcome::SessionIndex(outcome) = next_outcome(&mailbox).await else {
			panic!("the project index settles as a SessionIndex outcome");
		};
		assert_eq!(outcome.scope, SessionScope::Project);
		let ids = outcome
			.result
			.expect("index succeeds")
			.into_iter()
			.map(|row| row.id)
			.collect::<Vec<_>>();
		assert_eq!(ids, vec![own]);
		harness.quit().await;
	}

	/// `HostCommand::Retry` re-runs the last turn's aborted tool tail
	/// without a model round trip (pi `retry()`): the same call id becomes
	/// ready again, and the live chain keeps exactly one `tool.call@1`.
	#[tokio::test]
	async fn retry_reruns_the_aborted_tool_tail_with_the_same_call_id() {
		let harness = HarnessSpec::new(Script::BashThenText, Duration::ZERO).build();
		harness
			.commands
			.send(HostCommand::Submit(Str::new_static("use bash")))
			.expect("submit");
		let ready =
			next_event(&harness.events, |event| matches!(event, KernelEvent::ToolReady { .. })).await;
		assert_eq!(ready, KernelEvent::ToolReady {
			call_id: Str::new_static(SCRIPTED_CALL),
			name:    Str::new_static("bash"),
		});
		harness
			.commands
			.send(HostCommand::Interrupt)
			.expect("interrupt");
		let ended =
			next_event(&harness.events, |event| matches!(event, KernelEvent::TurnEnded { .. })).await;
		assert_eq!(ended, KernelEvent::TurnEnded { stop: TurnStop::Cancelled });

		harness.commands.send(HostCommand::Retry).expect("retry");
		let ready =
			next_event(&harness.events, |event| matches!(event, KernelEvent::ToolReady { .. })).await;
		assert_eq!(
			ready,
			KernelEvent::ToolReady {
				call_id: Str::new_static(SCRIPTED_CALL),
				name:    Str::new_static("bash"),
			},
			"the retry re-dispatches the same call"
		);
		harness
			.commands
			.send(HostCommand::Interrupt)
			.expect("interrupt the retry");
		let ended =
			next_event(&harness.events, |event| matches!(event, KernelEvent::TurnEnded { .. })).await;
		assert_eq!(ended, KernelEvent::TurnEnded { stop: TurnStop::Cancelled });
		assert!(
			harness
				.mailbox()
				.drain()
				.all(|action| !matches!(action, HostAction::Reply { .. })),
			"a real retry posts no notice"
		);

		let (journal, _dir) = harness.quit().await;
		let entries = journal_entries(&journal);
		let live = omp_journal::live_chain(&entries).collect::<Vec<_>>();
		assert_eq!(
			live
				.iter()
				.filter(|entry| entry.kind.name.as_str() == omp_journal::kind::TOOL_CALL)
				.count(),
			1,
			"one call on the live chain, re-executed in place"
		);
		assert!(
			live.len() < entries.len(),
			"the aborted result is abandoned by the rewind, never deleted"
		);
	}

	/// `HostCommand::Retry` on a session whose last turn has no aborted
	/// tool tail posts pi's `Nothing to retry` notice and runs nothing.
	#[tokio::test]
	async fn retry_without_an_aborted_tail_posts_the_info_notice() {
		let harness = harness(Duration::ZERO);
		harness.commands.send(HostCommand::Retry).expect("retry");
		let (severity, text) = next_reply(&harness.mailbox()).await;
		assert_eq!(severity, Severity::Info);
		assert_eq!(text, "Nothing to retry");
		assert!(harness.events.try_iter().count() == 0, "no turn ran");

		// A completed text turn is not retryable either.
		harness
			.commands
			.send(HostCommand::Submit(Str::new_static("hello")))
			.expect("submit");
		next_event(&harness.events, |event| {
			matches!(event, KernelEvent::TurnEnded { stop: TurnStop::Completed })
		})
		.await;
		harness
			.commands
			.send(HostCommand::Retry)
			.expect("retry again");
		let (severity, text) = next_reply(&harness.mailbox()).await;
		assert_eq!(severity, Severity::Info);
		assert_eq!(text, "Nothing to retry");
		harness.quit().await;
	}
}
