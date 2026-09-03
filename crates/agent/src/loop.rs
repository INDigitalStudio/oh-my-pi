//! Journal-first agent turn kernel.

use std::{sync::Arc, time::Instant};

use futures::StreamExt as _;
use omp_core::{FastHashMap, Str};
use omp_dom::{Handle, KnownTag, PropId, Tag, Txn};
use omp_inference::{
	ArtifactBody, BlockKind, ChatEvent, ChatRequest, ChatStream, Client, Completion, FinishReason,
	Message as InferenceMessage, NegotiationPolicy, Planner, SafetySetting, Sampling, Setting,
	Usage,
};
use omp_journal::{EntryId, blob::BlobRef, data::TurnReceipt};
use omp_proto::{
	thread::v1::{Item, Message, Part as ThreadPart, Role, item, part},
	toolhost::v1::HookEventId,
};
use omp_session::{Session, SessionError, project_thread};
use omp_tool::{Abort, LoweringCaps, Registry, RegistryError, ToolIdentity};
use serde_json::value::RawValue;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tower::Service;

use crate::{
	CallControl, CancelTree, Director as _, DirectorCx, DirectorError, DirectorRegistry,
	DirectorStack, DispatchError, DispatchPolicy, Dispatcher,
	ExternalToolExecutor, KernelEvent, LiveComponent, LiveComponentError, LoopDecision,
	MutDirectorCx, Prepared, PreparedCall, Received, RouteFacts, SessionTool, ToolCancellation,
	TurnView, Up,
	directors::compaction::CompactionDirector,
	steering::{
		EMPTY_OUTPUT_RETRY_CAP, append_empty_output_cap_notice, append_empty_output_retry,
		append_error_notice, append_interrupt_notice, append_named_notice, append_notice,
		consume_steering, steering_pending,
	},
};

/// Pure system-prompt projection from the authoritative session tree.
pub trait PromptSource: Send + Sync {
	/// Projects ordered system items without retaining parallel session state.
	///
	/// A failure (a template that cannot render from the journal-derived
	/// facts) ends the turn before inference and is journaled as a
	/// `<notice kind=error>` by the kernel rather than aborting the host.
	fn system_items(&self, dom: &omp_dom::Dom) -> Result<Vec<Item>, crate::PromptError>;
}

/// Fixed system prompt useful for tests and small embeddings.
#[derive(Clone, Debug)]
pub struct StaticPrompt(pub Str);

impl PromptSource for StaticPrompt {
	fn system_items(&self, _dom: &omp_dom::Dom) -> Result<Vec<Item>, crate::PromptError> {
		Ok(vec![Item {
			kind: Some(item::Kind::Message(Message {
				role: Role::System as i32,
				parts: vec![ThreadPart { kind: Some(part::Kind::Text(self.0.as_str().to_owned())) }],
				..Default::default()
			})),
			..Default::default()
		}])
	}
}

/// Minimal inference capability required by the agent kernel.
pub trait Inference: Send {
	/// Starts one canonical streaming chat operation.
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send;

	/// Installs the observer that receives same-route retry notices for
	/// every subsequent chat. Inference stacks without a retry layer keep the
	/// default no-op.
	fn install_retry_sink(&mut self, sink: omp_inference::RetrySink) {
		let _ = sink;
	}
}

impl<S, P> Inference for Client<S, P>
where
	S: Service<
			omp_inference::call::Call,
			Response = omp_inference::Answer,
			Error = omp_inference::Error,
		> + Send,
	S::Future: Send,
	P: Planner + Send,
{
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send {
		self.execute(request)
	}

	fn install_retry_sink(&mut self, sink: omp_inference::RetrySink) {
		let mut meta = self.call_meta().clone();
		meta.response_hooks = meta.response_hooks.with_retry_sink(sink);
		self.set_call_meta(meta);
	}
}

/// User input that begins one explicit session turn.
pub struct TurnInput {
	/// User-authored text.
	pub text:        Str,
	/// Content-addressed attachments.
	pub attachments: Vec<BlobRef>,
}

/// Why the kernel returned control to its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnStop {
	/// The candidate yield passed the Director stack.
	Completed,
	/// Turn or session cancellation was observed.
	Cancelled,
	/// Steering was consumed at a safe point before yielding.
	Steered,
	/// The turn ended in a journaled error notice (only reported through
	/// [`KernelEvent::TurnEnded`]; `run_turn` returns the error itself).
	Failed,
}

/// Durable summary of one explicit turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnOutcome {
	/// Terminal control reason.
	pub stop:           TurnStop,
	/// Visible assistant text accumulated across tool continuations.
	pub assistant_text: Str,
	/// Total input tokens across inference attempts.
	pub tokens_in:      u64,
	/// Total output tokens across inference attempts.
	pub tokens_out:     u64,
}

/// Caller-owned cancellation and optional deadline for one turn.
#[derive(Clone, Debug)]
pub struct RunControl {
	cancellation: CancellationToken,
	deadline:     Option<Instant>,
	max_requests:          Option<u32>,
	request_budget_notice: bool,
}

impl RunControl {
	/// Creates turn control from an external cancellation token and deadline.
	#[must_use]
	pub const fn new(cancellation: CancellationToken, deadline: Option<Instant>) -> Self {
		Self { cancellation, deadline, max_requests: None, request_budget_notice: true }
	}

	/// Limits the number of provider requests this turn may start.
	#[must_use]
	pub const fn with_request_budget(mut self, max_requests: u32) -> Self {
		self.max_requests = Some(max_requests);
		self
	}

	/// Controls whether reaching the soft request budget grants one wrap-up
	/// request carrying a durable notice.
	#[must_use]
	pub const fn with_request_budget_notice(mut self, enabled: bool) -> Self {
		self.request_budget_notice = enabled;
		self
	}

	/// Returns whether request ordinal `started` may begin.
	#[must_use]
	pub fn permits_request(&self, started: u32, notice_sent: bool) -> bool {
		self.max_requests.is_none_or(|maximum| {
			started < maximum
				|| (self.request_budget_notice && started == maximum && !notice_sent)
		})
	}

	fn should_emit_request_budget_notice(&self, started: u32, notice_sent: bool) -> bool {
		self.request_budget_notice
			&& !notice_sent
			&& self.max_requests.is_some_and(|maximum| started == maximum)
	}

	/// Reports whether cancellation or the deadline has already fired.
	#[must_use]
	pub fn is_expired(&self) -> bool {
		self.cancellation.is_cancelled()
			|| self
				.deadline
				.is_some_and(|deadline| Instant::now() >= deadline)
	}

	pub(crate) async fn cancelled(&self) {
		if let Some(deadline) = self.deadline {
			tokio::select! {
				() = self.cancellation.cancelled() => {},
				() = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {},
			}
		} else {
			self.cancellation.cancelled().await;
		}
	}
}

impl Default for RunControl {
	fn default() -> Self {
		Self::new(CancellationToken::new(), None)
	}
}

/// Turn-loop construction, inference, dispatch, or session failure.
#[derive(Debug, Error)]
pub enum KernelError {
	/// Session journal or DOM fold failed.
	#[error(transparent)]
	Session(#[from] SessionError),
	/// Inference planning or streaming failed.
	#[error(transparent)]
	Inference(#[from] omp_inference::Error),
	/// Tool registry operation failed.
	#[error(transparent)]
	Registry(#[from] RegistryError),
	/// Canonical thread projection failed.
	#[error(transparent)]
	ThreadProjection(#[from] omp_inference::ThreadProjectionError),
	/// Blob persistence failed.
	#[error(transparent)]
	Blob(#[from] omp_journal::blob::Error),
	/// Tool dispatch failed.
	#[error(transparent)]
	Dispatch(#[from] DispatchError),
	/// Director reconstruction or execution failed.
	#[error(transparent)]
	Director(#[from] DirectorError),
	/// JSON serialization failed.
	#[error(transparent)]
	Json(#[from] serde_json::Error),
	/// An inference stream emitted output before response metadata.
	#[error("inference output arrived before response metadata")]
	MissingResponseStart,
	/// A tool argument block did not contain UTF-8 JSON text.
	#[error("tool argument delta is not UTF-8")]
	ToolArgumentUtf8 {
		/// UTF-8 validation failure.
		#[source]
		source: std::str::Utf8Error,
	},
	/// A ready tool call conflicts with its streamed call identity.
	#[error("ready tool call does not match its streamed call")]
	ToolCallMismatch,
	/// A live lifecycle hook denied or malformed a transition.
	#[error(transparent)]
	LifecycleHook(#[from] crate::LifecycleHookError),
	/// A live extension Component reducer failed.
	#[error(transparent)]
	LiveComponent(#[from] LiveComponentError),
	/// The system prompt could not be projected from the session tree.
	#[error("system prompt projection failed")]
	Prompt(#[source] crate::PromptError),
}

/// Journal-backed host state which must flush and rehydrate with the session.
pub trait SessionStateBridge: Send + Sync {
	/// Journals pending host writes before session readers project state.
	fn flush(&self, session: &mut Session) -> Result<(), SessionError>;
	/// Rehydrates disposable host state after rewind or session switch.
	fn resync(&self, dom: &omp_dom::Dom);
}

/// Cross-crate runtime switches resolved by the composition owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeFlags {
	/// Whether automatic context compaction may engage.
	pub automatic_compaction:    bool,
	/// Whether Goal engagements may remain active.
	pub goal_enabled:            bool,
	/// Whether a substantive turn schedules automatic learning.
	pub autolearn_enabled:       bool,
	/// Minimum settled non-learn calls before automatic learning.
	pub autolearn_min_tool_calls: usize,
	/// Whether plain-text sloppy edit payloads become real edit calls.
	pub recover_inline_edits:     bool,
}

impl Default for RuntimeFlags {
	fn default() -> Self {
		Self {
			automatic_compaction: true,
			goal_enabled: true,
			autolearn_enabled: false,
			autolearn_min_tool_calls: 5,
			recover_inline_edits: true,
		}
	}
}

/// Agent kernel composed from inference, tool, prompt, and Director registries.
pub struct Kernel<C> {
	client:                       C,
	pub(crate) dispatcher:        Dispatcher,
	pub(crate) cancel:            CancelTree,
	director_registry:            DirectorRegistry,
	live_components:              Vec<Box<dyn LiveComponent>>,
	lifecycle_hooks:              Option<crate::LifecycleHooks>,
	state_bridges:                Vec<Arc<dyn SessionStateBridge>>,
	pub(crate) events:            crate::events::KernelEvents,
	prompt:                       Arc<dyn PromptSource>,
	route:                        RouteFacts,
	con:                          Option<Arc<omp_con::Ctx>>,
	runtime_flags:                RuntimeFlags,
	pub(crate) mailbox_tx:        flume::Sender<Up>,
	mailbox_rx:                   flume::Receiver<Up>,
}

impl<C> Kernel<C> {
	/// Constructs a kernel with the standard Director registry.
	#[must_use]
	pub fn new(
		mut client: C,
		registry: Arc<Registry>,
		policy: DispatchPolicy,
		prompt: impl PromptSource + 'static,
	) -> Self
	where
		C: Inference,
	{
		let (mailbox_tx, mailbox_rx) = flume::unbounded();
		let events = crate::events::KernelEvents::default();
		let retry_events = events.clone();
		client.install_retry_sink(Arc::new(move |notice: omp_inference::RetryNotice| {
			retry_events.publish(KernelEvent::InferenceRetry {
				attempt:      notice.attempt,
				max_attempts: notice.max_attempts,
				delay:        notice.delay,
				reason:       notice.message,
			});
		}));
		Self {
			client,
			dispatcher: Dispatcher::new(registry, policy).with_events(events.clone()),
			cancel: CancelTree::new(),
			director_registry: DirectorRegistry::standard(),
			live_components: Vec::new(),
			lifecycle_hooks: None,
			state_bridges: Vec::new(),
			events,
			prompt: Arc::new(prompt),
			route: RouteFacts::default(),
			con: None,
			runtime_flags: RuntimeFlags::default(),
			mailbox_tx,
			mailbox_rx,
		}
	}

	/// Replaces the Director registry assembled by the host.
	#[must_use]
	pub fn with_director_registry(mut self, registry: DirectorRegistry) -> Self {
		self.director_registry = registry;
		self
	}

	/// Installs the shared extension lifecycle gate.
	#[must_use]
	pub fn with_hook_gate(mut self, gate: Arc<crate::HookGate>) -> Self {
		let hooks = crate::LifecycleHooks::new(gate);
		self.dispatcher = self.dispatcher.with_lifecycle_hooks(hooks.clone());
		self.lifecycle_hooks = Some(hooks);
		self
	}

	/// Returns the shared lifecycle facade for host-side session transitions.
	#[must_use]
	pub fn lifecycle_hooks(&self) -> Option<crate::LifecycleHooks> {
		self.lifecycle_hooks.clone()
	}

	/// Retains a journal-backed host state bridge for turn and session boundaries.
	#[must_use]
	pub fn with_session_state_bridge(mut self, bridge: Arc<dyn SessionStateBridge>) -> Self {
		self.state_bridges.push(bridge);
		self
	}

	/// Flushes pending host state into the authoritative journal.
	pub fn flush_session_state(&self, session: &mut Session) -> Result<(), SessionError> {
		for bridge in &self.state_bridges {
			bridge.flush(session)?;
		}
		Ok(())
	}

	/// Rehydrates disposable host state and Director layers from the current DOM.
	pub fn resync_session_state(&self, session: &Session) {
		for bridge in &self.state_bridges {
			bridge.resync(session.dom());
		}
		self.reconcile_director_binds(session);
	}

	/// Registers a live extension Component reducer.
	pub fn register_live_component(&mut self, component: Box<dyn LiveComponent>) {
		self.live_components.push(component);
	}

	/// Replaces catalog-derived facts for the selected route.
	#[must_use]
	pub const fn with_route_facts(mut self, route: RouteFacts) -> Self {
		self.route = route;
		self
	}

	/// Injects the effective control-plane context used for Director layers.
	#[must_use]
	pub fn with_con_context(mut self, con: Arc<omp_con::Ctx>) -> Self {
		self.con = Some(con);
		self
	}

	/// Replaces cross-crate runtime switches resolved by host composition.
	#[must_use]
	pub const fn with_runtime_flags(mut self, flags: RuntimeFlags) -> Self {
		self.runtime_flags = flags;
		self
	}

	/// Injects execution for worker- and remote-routed tools.
	#[must_use]
	pub fn with_external_executor(mut self, executor: Arc<dyn ExternalToolExecutor>) -> Self {
		self.dispatcher = self.dispatcher.with_external_executor(executor);
		self
	}

	/// Registers a host-authority tool that operates on the session DOM.
	#[must_use]
	pub fn with_session_tool(mut self, tool: Arc<dyn SessionTool>) -> Self {
		self.dispatcher = self.dispatcher.with_session_tool(tool);
		self
	}

	/// Injects the host-owned live-session routing authority.
	#[must_use]
	pub fn with_session_authority(mut self, authority: Arc<dyn crate::SessionAuthority>) -> Self {
		self.dispatcher = self.dispatcher.with_session_authority(authority);
		self
	}

	/// Borrows the composed inference owner.
	#[must_use]
	pub const fn inference(&self) -> &C {
		&self.client
	}

	/// Borrows the composed runtime tool registry.
	#[must_use]
	pub fn tool_registry(&self) -> &Arc<Registry> {
		self.dispatcher.registry()
	}

	/// Returns the one upward control mailbox.
	#[must_use]
	pub fn mailbox(&self) -> flume::Sender<Up> {
		self.mailbox_tx.clone()
	}

	/// Subscribes to lossless observer notifications for subsequent journaled
	/// progress.
	pub fn subscribe(&mut self) -> flume::Receiver<KernelEvent> {
		self.events.subscribe()
	}

	/// Cancels the owning session and every active or future tool scope.
	pub fn cancel_session(&self) {
		self.cancel.cancel_session();
	}

	/// Applies rewind/resume lifecycle work to every runtime execution unit.
	pub fn apply_lifecycle(
		&self,
		session: &Session,
		work: &omp_session::LifecycleWork,
	) -> impl Future<Output = ()> + Send + 'static {
		self.dispatcher.jobs().apply_lifecycle(session, work)
	}

	/// Re-derives effective Director convar layers after rewind or session switch.
	pub fn reconcile_director_binds(&self, session: &Session) {
		if let Some(con) = &self.con {
			DirectorStack::from_dom(session.dom(), &self.director_registry)
				.apply_binds(session.dom(), con);
		}
	}

	pub(crate) fn apply_live_components(&mut self, session: &mut Session) -> Result<(), KernelError> {
		let Some(head) = session.head() else {
			return Ok(());
		};
		let Some(entry) = session.entry(head).cloned() else {
			return Ok(());
		};
		let mut patches = Vec::new();
		let mut failed = false;
		for component in &self.live_components {
			if !component.interested(&entry.kind) {
				continue;
			}
			match component.reduce(&entry, session.dom()) {
				Ok(ops) if !ops.is_empty() => {
					patches.push((Str::new(component.id()), ops));
				},
				Ok(_) => {},
				Err(error) => {
					tracing::warn!(?error, component = component.id(), "live Component failed");
					failed = true;
				},
			}
		}
		for (id, ops) in patches {
			session.patch(Txn { cause: entry.id, label: Some(Str::new(format!("ext:{id}"))), ops })?;
		}
		if failed && let Ok(turn) = current_turn(session) {
			append_notice(
				session,
				turn,
				Str::new_static("Python extension Component callback failed"),
			)?;
		}
		Ok(())
	}
}

impl<C: Inference> Kernel<C> {
	/// Runs one explicit user turn through inference, tools, steering, and
	/// Directors.
	///
	/// A failure after the turn opened is journaled before it is returned: any
	/// open `<assistant>` is closed with stop reason `error` and the turn gains
	/// a `<notice kind=error>` carrying the full error chain, so a resumed or
	/// rendered session shows why the turn ended and observers never see a
	/// dangling assistant.
	pub async fn run_turn(
		&mut self,
		session: &mut Session,
		mut input: TurnInput,
		control: RunControl,
	) -> Result<TurnOutcome, KernelError> {
		if control.is_expired() || self.cancel.is_session_cancelled() {
			return Ok(cancelled_outcome());
		}
		self.flush_session_state(session)?;
		let submission_id = session
			.head()
			.map_or_else(|| Str::new_static("submission"), |id| Str::new(id.to_string()));
		if let Some(hooks) = &self.lifecycle_hooks {
			let payload = hooks
				.gate(
					HookEventId::HookEventBeforeAgentStart,
					serde_json::json!({
						"submission_id": submission_id,
						"text": input.text,
						"items": [],
						"source": "interactive",
						"prompt_rev": "1",
						"staged_interrupts": 0,
						"resuming": false,
						"schedule_id": serde_json::Value::Null,
					}),
				)
				.await?;
			if let Some(text) = payload.get("text").and_then(serde_json::Value::as_str) {
				input.text = Str::new(text);
			}
			hooks.notify(
				HookEventId::HookEventAgentStart,
				serde_json::json!({
					"submission_id": submission_id,
					"from_phase": "idle",
					"pending_items": 1,
				}),
			)?;
		}
		let turn_cancel = self.cancel.begin_turn();
		session.begin_turn()?;
		self.apply_live_components(session)?;
		session.user(input.text, input.attachments)?;
		self.apply_live_components(session)?;
		let turn = current_turn(session)?;
		let result = self
			.run_turn_body(session, turn, &turn_cancel, &control)
			.await;
		match &result {
			Err(error) => self.journal_turn_failure(session, turn, error),
			Ok(outcome) if outcome.stop == TurnStop::Cancelled => {
				self.journal_turn_interrupt(session, turn);
			},
			Ok(_) => {},
		}
		self.events.publish(KernelEvent::TurnEnded {
			stop: match &result {
				Ok(outcome) => outcome.stop,
				Err(_) => TurnStop::Failed,
			},
		});
		self.flush_session_state(session)?;
		self.resync_session_state(session);
		if let Some(hooks) = &self.lifecycle_hooks {
			let (stop, interrupted, error) = match &result {
				Ok(outcome) => (
					format!("{:?}", outcome.stop).to_ascii_lowercase(),
					outcome.stop == TurnStop::Cancelled,
					None,
				),
				Err(_) => ("error".to_owned(), false, Some("agent turn failed")),
			};
			hooks.notify(
				HookEventId::HookEventTurnEnd,
				serde_json::json!({
					"turn_id": turn.to_string(),
					"turn_index": 0,
					"event_index": 0,
					"stop": stop,
					"usage": {
						"input_tokens": result.as_ref().map_or(0, |value| value.tokens_in),
						"cached_input_tokens": 0,
						"output_tokens": result.as_ref().map_or(0, |value| value.tokens_out),
						"reasoning_tokens": 0,
						"cache_write_tokens": 0,
						"requests": 0,
						"cost_usd": 0.0,
						"wall": "0s",
					},
					"session_usage": {
						"input_tokens": result.as_ref().map_or(0, |value| value.tokens_in),
						"cached_input_tokens": 0,
						"output_tokens": result.as_ref().map_or(0, |value| value.tokens_out),
						"reasoning_tokens": 0,
						"cache_write_tokens": 0,
						"requests": 0,
						"cost_usd": 0.0,
						"wall": "0s",
					},
					"revision": serde_json::Value::Null,
					"calls": [],
					"items": [],
				}),
			)?;
			hooks.notify(
				HookEventId::HookEventAgentEnd,
				serde_json::json!({
					"submission_id": submission_id,
					"summary": {
						"committed_turns": 1,
						"interrupted": interrupted,
						"stop": stop,
					},
					"continued": false,
					"error": error,
				}),
			)?;
		}
		result
	}

	/// Records an interrupted turn in the tree (ADR 0004: lifecycle derives
	/// from the tree): an open assistant closes with `cancelled` and the turn
	/// ends with `<notice kind=warn>`, never a receipt or a false completion.
	fn journal_turn_interrupt(&mut self, session: &mut Session, turn: Handle) {
		match session.assistant_end("cancelled") {
			Ok(_) => {
				if let Err(error) = self.apply_live_components(session) {
					tracing::warn!(?error, "live Components failed after an assistant interrupt close");
				}
			},
			Err(SessionError::NoActiveAssistant) => {},
			Err(journal) => {
				tracing::warn!(error = ?journal, "failed to close the assistant after an interrupt");
			},
		}
		if let Err(journal) = append_interrupt_notice(session, turn) {
			tracing::warn!(error = ?journal, "failed to journal the turn interrupt notice");
		}
	}

	fn journal_turn_failure(&mut self, session: &mut Session, turn: Handle, error: &KernelError) {
		match session.assistant_end("error") {
			Ok(_) => {
				if let Err(error) = self.apply_live_components(session) {
					tracing::warn!(?error, "live Components failed after an assistant error close");
				}
			},
			Err(SessionError::NoActiveAssistant) => {},
			Err(journal) => {
				tracing::warn!(error = ?journal, "failed to close the assistant after a turn error");
			},
		}
		if let Err(journal) = append_error_notice(session, turn, Str::new(error_chain(error))) {
			tracing::warn!(error = ?journal, "failed to journal the turn error notice");
		}
	}

	async fn run_turn_body(
		&mut self,
		session: &mut Session,
		turn: Handle,
		turn_cancel: &crate::TurnCancellation,
		control: &RunControl,
	) -> Result<TurnOutcome, KernelError> {
		let mut directors = DirectorStack::from_dom(session.dom(), &self.director_registry);
		if !self.runtime_flags.goal_enabled
			&& let Some((goal, _)) = crate::find_director(session.dom(), "goal")
		{
			session.patch(Txn {
				cause: session.head().ok_or(SessionError::NoActiveTurn)?,
				label: Some(Str::new_static("director.goal-disabled")),
				ops: vec![omp_dom::Op::Rm(goal)],
			})?;
			directors = DirectorStack::from_dom(session.dom(), &self.director_registry);
		}
		if self.runtime_flags.automatic_compaction
			&& !directors.active_ids().contains(&"compaction")
			&& !directors.queued_ids().contains(&"compaction")
		{
			directors.engage(session, Box::new(CompactionDirector::new()))?;
		}
		let mut total_text = String::new();
		let mut tokens_in = 0_u64;
		let mut tokens_out = 0_u64;
		let mut was_steered = false;
		let mut empty_output_retries = 0_u8;
		let mut requests_started = 0_u32;
		let mut request_budget_notice_sent = false;
		let route = self.route;

		loop {
			if control.is_expired() || turn_cancel.is_turn_cancelled() {
				turn_cancel.cancel_turn();
				return Ok(outcome(TurnStop::Cancelled, total_text, tokens_in, tokens_out));
			}
			if !control.permits_request(requests_started, request_budget_notice_sent) {
				append_named_notice(
					session,
					turn,
					Str::new_static("warn"),
					Some(Str::new_static("request-budget")),
					Str::new_static("Subagent request budget exhausted before another inference"),
				)?;
				self.apply_live_components(session)?;
				return Ok(outcome(TurnStop::Completed, total_text, tokens_in, tokens_out));
			}
			if control.should_emit_request_budget_notice(
				requests_started,
				request_budget_notice_sent,
			) {
				append_named_notice(
					session,
					turn,
					Str::new_static("warn"),
					Some(Str::new_static("request-budget")),
					Str::new_static(
						"Soft request budget reached; use this final request to yield a concise result.",
					),
				)?;
				request_budget_notice_sent = true;
			}
			self.flush_session_state(session)?;
			if let Some(con) = &self.con {
				directors.apply_binds(session.dom(), con);
			}
			let mut request = self.build_request(session)?;
			if let Some(hooks) = &self.lifecycle_hooks {
				let enabled_tools =
					request.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>();
				let payload = hooks
					.gate(
						HookEventId::HookEventTurnStart,
						serde_json::json!({
							"turn_id": turn.to_string(),
							"turn_index": requests_started,
							"prompt_hash": "",
							"toolset_hash": "",
							"enabled_tools": enabled_tools,
							"input_mode": "full",
							"model": {"provider": "", "api": "", "model": ""},
							"route": {"provider": "", "route": ""},
							"thinking": "none",
							"deadline": serde_json::Value::Null,
							"attempt": requests_started,
							"prompt_changed": requests_started == 0,
							"toolset_changed": requests_started == 0,
						}),
					)
					.await?;
				hooks.notify(HookEventId::HookEventTurnStart, payload.clone())?;
				if let Some(enabled) =
					payload.get("enabled_tools").and_then(serde_json::Value::as_array)
				{
					request.tools = request
						.tools
						.iter()
						.filter(|tool| {
							enabled.iter().any(|name| name.as_str() == Some(tool.name.as_str()))
						})
						.cloned()
						.collect::<Vec<_>>()
						.into();
				}
			}
			let preflight_control = CallControl::new(
				self.mailbox_rx.clone(),
				turn_cancel.clone(),
				self.cancel.clone(),
				Some(control.clone()),
			);
			let preflight = {
				let mut cx = MutDirectorCx {
					session,
					inference: &mut self.client,
					blobs: &self.dispatcher.policy().spill,
					route: &route,
					turn,
					director: None,
					events: Some(&self.events),
				};
				let preparing = directors.before_inference(&mut cx, &request);
				tokio::pin!(preparing);
				tokio::select! {
					biased;
					result = &mut preparing => PreflightSignal::Ready(result),
					() = control.cancelled() => PreflightSignal::Cancelled,
					message = preflight_control.recv() => PreflightSignal::Control(message),
				}
			};
			let prepared = match preflight {
				PreflightSignal::Ready(result) => result?,
				PreflightSignal::Cancelled => {
					turn_cancel.cancel_turn();
					return Ok(outcome(TurnStop::Cancelled, total_text, tokens_in, tokens_out));
				},
				PreflightSignal::Control(message) => {
					match preflight_control.handle(session, message)? {
						Received::Cancelled => {
							return Ok(outcome(TurnStop::Cancelled, total_text, tokens_in, tokens_out));
						},
						Received::Rewound(work) => {
							self.dispatcher.jobs().apply_lifecycle(session, &work).await;
							turn_cancel.cancel_turn();
							return Ok(outcome(TurnStop::Cancelled, total_text, tokens_in, tokens_out));
						},
						Received::None | Received::Steering => {},
					}
					continue;
				},
			};
			self.apply_live_components(session)?;
			if prepared == Prepared::Rebuild {
				request = self.build_request(session)?;
				directors = DirectorStack::from_dom(session.dom(), &self.director_registry);
			}
			let director_cx = DirectorCx::new(turn, &route);
			directors.prepare_inference(session.dom(), &director_cx, &mut request);
			let request_started = Instant::now();
			requests_started = requests_started.saturating_add(1);
			let opening_control = CallControl::new(
				self.mailbox_rx.clone(),
				turn_cancel.clone(),
				self.cancel.clone(),
				Some(control.clone()),
			);
			let stream = {
				let opening = self.client.chat(request);
				tokio::pin!(opening);
				loop {
					tokio::select! {
						biased;
						result = &mut opening => break result?,
						() = control.cancelled() => {
							turn_cancel.cancel_turn();
							return Ok(outcome(
								TurnStop::Cancelled,
								total_text,
								tokens_in,
								tokens_out,
							));
						},
						message = opening_control.recv() => {
							match opening_control.handle(session, message)? {
								Received::Cancelled => {
									return Ok(outcome(
										TurnStop::Cancelled,
										total_text,
										tokens_in,
										tokens_out,
									));
								},
								Received::Rewound(work) => {
									self.dispatcher.jobs().apply_lifecycle(session, &work).await;
									turn_cancel.cancel_turn();
									return Ok(outcome(
										TurnStop::Cancelled,
										total_text,
										tokens_in,
										tokens_out,
									));
								},
								Received::None | Received::Steering => {},
							}
						},
					}
				}
			};
			let mut driven = self
				.drive_inference(session, stream, control, turn_cancel, request_started)
				.await?;
			tokens_in = tokens_in.saturating_add(driven.usage.input_tokens);
			tokens_out = tokens_out.saturating_add(driven.usage.output_tokens);
			total_text.push_str(driven.text.as_str());
			if driven.cancelled {
				return Ok(outcome(TurnStop::Cancelled, total_text, tokens_in, tokens_out));
			}
			let had_tool_calls = driven.had_tool_calls;
			if had_tool_calls {
				if let Some(hooks) = &self.lifecycle_hooks {
					for call in &driven.calls {
						hooks.notify(
							HookEventId::HookEventToolExecutionStart,
							serde_json::json!({
								"call_id": call.call_id(),
								"invocation_id": call.call_id(),
								"target": {
									"kind": "core",
									"name": call.identity().name,
									"rev": format!(
										"{}@{}",
										call.identity().rev.family,
										call.identity().rev.n,
									),
									"args": {},
								},
								"place": {"kind": "host", "name": serde_json::Value::Null},
								"deadline": serde_json::Value::Null,
							}),
						)?;
					}
				}
				let settled_calls = driven
					.calls
					.iter()
					.map(|call| (call.call_id().clone(), call.identity().clone()))
					.collect::<Vec<_>>();
				let call_control = CallControl::new(
					self.mailbox_rx.clone(),
					turn_cancel.clone(),
					self.cancel.clone(),
					Some(control.clone()),
				);
				let reports = self
					.dispatcher
					.drive(session, std::mem::take(&mut driven.calls), Some(&call_control))
					.await?;
				self.apply_live_components(session)?;
				if let Some(hooks) = &self.lifecycle_hooks {
					for ((call_id, identity), report) in settled_calls.into_iter().zip(reports) {
						let target = serde_json::json!({
							"kind": "core",
							"name": identity.name,
							"rev": format!("{}@{}", identity.rev.family, identity.rev.n),
							"args": {},
						});
						hooks.notify(
							HookEventId::HookEventToolExecutionEnd,
							serde_json::json!({
								"call_id": call_id,
								"target": target.clone(),
								"outcome": if report.is_error { "faulted" } else { "ok" },
								"duration": "0s",
								"spilled": report.spilled.is_some(),
								"artifact": report.spilled.as_ref().map(|blob| {
									format!("artifact://sha256/{}", blob.to_hex())
								}),
								"effects_unknown": false,
							}),
						)?;
						hooks.notify(
							HookEventId::HookEventToolResult,
							serde_json::json!({
								"call_id": call_id,
								"target": target,
								"outcome": if report.is_error { "faulted" } else { "ok" },
								"payload": if report.is_error {
									serde_json::Value::Null
								} else {
									serde_json::json!({})
								},
								"fault": if report.is_error {
									serde_json::json!({})
								} else {
									serde_json::Value::Null
								},
								"abort": serde_json::Value::Null,
								"artifact": report.spilled.as_ref().map(|blob| {
									format!("artifact://sha256/{}", blob.to_hex())
								}),
								"useless": false,
								"annotate": [],
								"spill": serde_json::Value::Null,
							}),
						)?;
					}
				}
			}
			let steering = self.drain_mailbox(session, turn_cancel)?;
			if steering.cancelled {
				return Ok(outcome(TurnStop::Cancelled, total_text, tokens_in, tokens_out));
			}
			let steering_received = steering.received || steering_pending(session);
			if steering_received {
				was_steered = true;
				let _ = consume_steering(session, turn)?;
				self.apply_live_components(session)?;
			}
			let turn_view = TurnView {
				turn,
				had_tool_calls,
				assistant_text: driven.text,
				stop_reason: driven.stop_reason,
			};
			directors.observe_turn(session, &director_cx, &turn_view)?;
			self.apply_live_components(session)?;
			if turn_view.had_tool_calls || steering_received {
				continue;
			}
			if turn_view.assistant_text.is_empty() {
				if empty_output_retries < EMPTY_OUTPUT_RETRY_CAP {
					empty_output_retries = empty_output_retries.saturating_add(1);
					append_empty_output_retry(session, turn, empty_output_retries)?;
					self.apply_live_components(session)?;
					continue;
				}
				append_empty_output_cap_notice(session, turn)?;
				self.apply_live_components(session)?;
			}
			if self.runtime_flags.autolearn_enabled
				&& should_schedule_autolearn(
					session.dom(),
					turn,
					self.runtime_flags.autolearn_min_tool_calls,
				)
				&& self.dispatcher.registry().resolved_identity("learn").is_some()
			{
				directors.engage(
					session,
					Box::new(crate::directors::force_tool::ForceTool::new(
						"learn",
						crate::ForceUntil::ToolCalled(Str::new_static("learn")),
						Some(Str::new_static(
							"Capture the substantive work from this turn with the learn tool.",
						)),
						1,
					)),
				)?;
				self.apply_live_components(session)?;
				continue;
			}
			let decision = directors.on_yield(session, &director_cx, &turn_view)?;
			self.apply_live_components(session)?;
			if let Some(con) = &self.con {
				directors.apply_binds(session.dom(), con);
			}
			let late = self.drain_mailbox(session, turn_cancel)?;
			if late.cancelled {
				return Ok(outcome(TurnStop::Cancelled, total_text, tokens_in, tokens_out));
			}
			if late.received || steering_pending(session) {
				was_steered = true;
				let _ = consume_steering(session, turn)?;
				self.apply_live_components(session)?;
				continue;
			}
			match decision {
				LoopDecision::Continue { .. } => continue,
				LoopDecision::Yield => {
					let stop = if was_steered {
						TurnStop::Steered
					} else {
						TurnStop::Completed
					};
					return Ok(outcome(stop, total_text, tokens_in, tokens_out));
				},
			}
		}
	}

	/// Dispatches one ready call and journals its outcome. The mailbox stays
	/// live while the tool runs: an interrupt cancels the turn scope the tool
	/// observes (pi aborts running tools on ctrl+c) instead of waiting for
	/// the tool to finish on its own; steering arriving meanwhile lands in
	/// `streamed_steering` for the next safe point.
	pub(crate) async fn dispatch_call(
		&mut self,
		session: &mut Session,
		call: ReadyCall,
		turn_cancel: &crate::TurnCancellation,
		control: &RunControl,
		streamed_steering: &mut Vec<Str>,
	) -> Result<bool, KernelError> {
		let cancellation =
			tool_cancellation(self.dispatcher.registry(), call.identity.name.as_str(), turn_cancel)?;
		let call_id = call.call_id;
		let mut prepared =
			self.dispatcher.prepare(call.identity, call_id, call.entry, cancellation)?;
		prepared.commit(call.args);
		let call_control = CallControl::new(
			self.mailbox_rx.clone(),
			turn_cancel.clone(),
			self.cancel.clone(),
			Some(control.clone()),
		);
		let mut reports = self.dispatcher.drive(session, vec![prepared], Some(&call_control)).await?;
		let report = reports.remove(0);
		if steering_pending(session) {
			streamed_steering.extend(crate::steering::queued_steering(session));
		}
		self.apply_live_components(session)?;
		Ok(report.is_error)
	}

	fn build_request(&self, session: &Session) -> Result<ChatRequest, KernelError> {
		let mut items = self
			.prompt
			.system_items(session.dom())
			.map_err(KernelError::Prompt)?;
		items.extend(project_thread(session.dom()));
		let mut messages = InferenceMessage::from_thread_items(&items)?;
		crate::events::strip_unsigned_reasoning(&mut messages);
		crate::vision::apply(session.dom(), &self.route, &mut messages);
		let tools = self
			.dispatcher
			.registry()
			.advertise(LoweringCaps {
				strict_schema:  false,
				grammar:        Default::default(),
				maximum_tools:  None,
				maximum_strict: None,
			})?
			.into_iter()
			.map(|tool| tool.definition)
			.collect::<Vec<_>>();
		Ok(ChatRequest {
			messages:          messages.into(),
			tools:             tools.into(),
			hosted_tools:      Arc::from([]),
			tool_choice:       Setting::Unset,
			output:            Setting::Unset,
			reasoning:         Setting::Unset,
			verbosity:         Setting::Unset,
			cache_retention:   Setting::Unset,
			service_tier:      Setting::Unset,
			sampling:          Sampling::default(),
			max_output_tokens: None,
			top_logprobs:      None,
			safety:            Arc::<[SafetySetting]>::from([]),
			negotiation:       NegotiationPolicy::default(),
			forced_call:       None,
		})
	}

	async fn drive_inference(
		&mut self,
		session: &mut Session,
		mut stream: ChatStream,
		control: &RunControl,
		turn_cancel: &crate::TurnCancellation,
		request_started: Instant,
	) -> Result<DrivenInference, KernelError> {
		let mut assistant = None;
		let mut content_streams = FastHashMap::<u32, u32>::default();
		let mut pending = FastHashMap::<u32, StreamingCall>::default();
		let mut ready = Vec::new();
		let mut text = String::new();
		let mut usage = Usage::default();
		let mut stop_reason = Str::new_static("stop");
		let mut completed = false;
		let mut had_tool_calls = false;
		let call_control = CallControl::new(
			self.mailbox_rx.clone(),
			turn_cancel.clone(),
			self.cancel.clone(),
			Some(control.clone()),
		);
		// pi `message.ttft`: first visible or reasoning byte (or the first
		// streamed tool-call fragment) after the request left the kernel.
		let mut first_token: Option<Instant> = None;
		let fold: Result<Fold, KernelError> = async {
			loop {
				let signal = tokio::select! {
					biased;
					() = control.cancelled() => StreamSignal::Cancelled,
					message = self.mailbox_rx.recv_async() => StreamSignal::Control(message.ok()),
					event = stream.next() => StreamSignal::Event(event),
				};
				let event = match signal {
					StreamSignal::Cancelled => {
						turn_cancel.cancel_turn();
						return Ok(Fold::Cancelled);
					},
					StreamSignal::Control(Some(message)) => {
						match call_control.handle(session, message)? {
							Received::Cancelled => return Ok(Fold::Cancelled),
							Received::Rewound(work) => {
								self.dispatcher.jobs().apply_lifecycle(session, &work).await;
								turn_cancel.cancel_turn();
								return Ok(Fold::Cancelled);
							},
							Received::None | Received::Steering => {},
						}
						continue;
					},
					StreamSignal::Control(None) => continue,
					StreamSignal::Event(Some(event)) => event?,
					StreamSignal::Event(None) => break Ok(Fold::Ended),
				};
				match event {
					ChatEvent::Started(meta) => {
						let model = meta.model.map_or_else(
							|| Str::new_static("unknown"),
							|value| Str::new(value.to_string()),
						);
						session.assistant_start(
							model,
							Str::new(meta.provider.to_string()),
							Str::new(meta.route.to_string()),
						)?;
						self.apply_live_components(session)?;
						assistant = Some(current_assistant(session)?);
						if let (Some(hooks), Some(item)) = (&self.lifecycle_hooks, assistant) {
							hooks.notify(
								HookEventId::HookEventMessageStart,
								serde_json::json!({
									"turn_id": current_turn(session)?.to_string(),
									"item_id": item.to_string(),
									"role": "assistant",
									"index": 0,
								}),
							)?;
						}
						self.events.publish(KernelEvent::InferenceStarted);
					},
					ChatEvent::BlockStarted { index, kind } => match kind {
						BlockKind::Text => {
							let handle = assistant.ok_or(KernelError::MissingResponseStart)?;
							let sid = session.stream_open(handle, PropId::Text.into())?;
							self.apply_live_components(session)?;
							content_streams.insert(index, sid);
						},
						BlockKind::Thinking => {
							let handle = assistant.ok_or(KernelError::MissingResponseStart)?;
							let sid = session.stream_open(handle, PropId::Thinking.into())?;
							self.apply_live_components(session)?;
							content_streams.insert(index, sid);
						},
						BlockKind::ToolCall | BlockKind::Artifact => {},
					},
					ChatEvent::TextDelta { index, text: delta } => {
						first_token.get_or_insert_with(Instant::now);
						let sid =
							content_sid(session, assistant, &mut content_streams, index, PropId::Text)?;
						session.stream_append(sid, delta.as_str())?;
						self.apply_live_components(session)?;
						self.events.publish(KernelEvent::TextDelta(delta.clone()));
						text.push_str(delta.as_str());
						if let (Some(hooks), Some(item)) = (&self.lifecycle_hooks, assistant) {
							hooks.notify(
								HookEventId::HookEventMessageUpdate,
								serde_json::json!({
									"turn_id": current_turn(session)?.to_string(),
									"item_id": item.to_string(),
									"part_index": index,
									"kind": "text",
									"delta": delta,
									"coalesced": 1,
									"total_chars": text.chars().count(),
								}),
							)?;
						}
					},
					ChatEvent::ThinkingDelta { index, text: delta } => {
						first_token.get_or_insert_with(Instant::now);
						let sid = content_sid(
							session,
							assistant,
							&mut content_streams,
							index,
							PropId::Thinking,
						)?;
						session.stream_append(sid, delta.as_str())?;
						self.apply_live_components(session)?;
						if let (Some(hooks), Some(item)) = (&self.lifecycle_hooks, assistant) {
							hooks.notify(
								HookEventId::HookEventMessageUpdate,
								serde_json::json!({
									"turn_id": current_turn(session)?.to_string(),
									"item_id": item.to_string(),
									"part_index": index,
									"kind": "reasoning",
									"delta": delta,
									"coalesced": 1,
									"total_chars": 0,
								}),
							)?;
						}
						self.events.publish(KernelEvent::ThinkingDelta(delta));
					},
					ChatEvent::ToolCallStarted { index, id, name } => {
						first_token.get_or_insert_with(Instant::now);
						let identity = self
							.dispatcher
							.registry()
							.resolved_identity(name.as_str())
							.ok_or_else(|| RegistryError::UnknownTool(name.clone()))?;
						let (entry, sid) = session.call_streaming(
							name.clone(),
							crate::journal_revision(&identity.rev),
							Str::new(id.to_string()),
							None,
						)?;
						self.apply_live_components(session)?;
						let call_id = Str::new(id.to_string());
						let cancellation = tool_cancellation(
							self.dispatcher.registry(),
							identity.name.as_str(),
							turn_cancel,
						)?;
						let prepared = self.dispatcher.prepare(
							identity.clone(),
							call_id.clone(),
							entry,
							cancellation,
						)?;
						if let Some(hooks) = &self.lifecycle_hooks {
							hooks.notify(
								HookEventId::HookEventCallOpen,
								serde_json::json!({
									"call_id": call_id,
									"target": {
										"kind": "core",
										"name": identity.name,
										"rev": format!("{}@{}", identity.rev.family, identity.rev.n),
										"args": {},
									},
									"kind": "core",
									"turn_id": current_turn(session)?.to_string(),
									"place": {"kind": "host", "name": serde_json::Value::Null},
								}),
							)?;
						}
						pending.insert(index, StreamingCall {
							entry,
							sid,
							identity,
							call_id,
							prepared,
							raw_args: String::new(),
						});
					},
					ChatEvent::ToolArgumentsDelta { index, bytes } => {
						let call = pending.get_mut(&index).ok_or(KernelError::ToolCallMismatch)?;
						let fragment = std::str::from_utf8(&bytes)
							.map_err(|source| KernelError::ToolArgumentUtf8 { source })?;
						session.stream_append(call.sid, fragment)?;
						call.raw_args.push_str(fragment);
						call.prepared.arg_delta(fragment);
						self.apply_live_components(session)?;
						let abort_invalid_edit = streamed_edit_must_abort(
							self.con.as_deref(),
							call.identity.name.as_str(),
							&call.raw_args,
						);
						if abort_invalid_edit {
							turn_cancel.cancel_turn();
							return Ok(Fold::InvalidEditArguments);
						}
					},
					ChatEvent::ToolCallReady { index, call } => {
						had_tool_calls = true;
						let args = serde_json::value::to_raw_value(call.arguments.as_value())?;
						let (entry, identity, mut prepared) =
							if let Some(streaming) = pending.remove(&index) {
							if streaming.call_id.as_str() != call.id.to_string()
								|| streaming.identity.name != call.name
							{
								return Err(KernelError::ToolCallMismatch);
							}
							(streaming.entry, streaming.identity, streaming.prepared)
						} else {
							let identity = self
								.dispatcher
								.registry()
								.resolved_identity(call.name.as_str())
								.ok_or_else(|| RegistryError::UnknownTool(call.name.clone()))?;
							let intent = call
								.arguments
								.as_value()
								.get("i")
								.and_then(serde_json::Value::as_str)
								.map(Str::new);
							let call_id = Str::new(call.id.to_string());
							let (entry, _) = session.call_streaming(
								call.name.clone(),
								crate::journal_revision(&identity.rev),
								call_id.clone(),
								intent,
							)?;
							self.apply_live_components(session)?;
							let cancellation = tool_cancellation(
								self.dispatcher.registry(),
								identity.name.as_str(),
								turn_cancel,
							)?;
							let prepared = self.dispatcher.prepare(
								identity.clone(),
								call_id,
								entry,
								cancellation,
							)?;
							(entry, identity, prepared)
						};
						let call_id = Str::new(call.id.to_string());
						let denied_args = args.clone();
						let session_id = session
							.journal_path()
							.file_stem()
							.and_then(|value| value.to_str())
							.map_or_else(|| Str::new_static("session"), Str::new);
						let turn_id = current_turn(session)
							.map(|handle| Str::new(handle.to_string()))
							.unwrap_or_else(|_| Str::new_static("turn"));
						let (identity, args) =
							match Self::gate_tool_call(
								self.lifecycle_hooks.clone(),
								Arc::clone(self.dispatcher.registry()),
								&session_id,
								&turn_id,
								&identity,
								&call_id,
								args,
							)
							.await
							{
								ToolGate::Allow { identity, args } => (identity, args),
								ToolGate::Deny(reason) => {
									session.call_ready(entry, denied_args.clone())?;
									prepared.commit(denied_args);
									self.dispatcher.abort_prepared(
										session,
										prepared,
										Abort::Skipped { reason },
									)?;
									self.apply_live_components(session)?;
									continue;
								},
							};
						session.call_ready(entry, args.clone())?;
						self.apply_live_components(session)?;
						if prepared.identity().name != identity.name || args.get() != denied_args.get() {
							prepared.discard();
							let cancellation = tool_cancellation(
								self.dispatcher.registry(),
								identity.name.as_str(),
								turn_cancel,
							)?;
							prepared = self.dispatcher.prepare(
								identity.clone(),
								call_id.clone(),
								entry,
								cancellation,
							)?;
							prepared.arg_delta(args.get());
						}
						prepared.commit(args);
						self.events.publish(KernelEvent::ToolReady {
							call_id: call_id.clone(),
							name:    identity.name.clone(),
						});
						ready.push(prepared);
					},
					ChatEvent::Usage(update) => {
						usage = update.usage;
						self.events.publish(KernelEvent::Usage {
							output_tokens:    usage.output_tokens,
							reasoning_tokens: usage.reasoning_tokens,
						});
					},
					ChatEvent::Completed(completion) => {
						close_streams(session, &mut content_streams)?;
						self.apply_live_components(session)?;
						stop_reason = finish_reason(&completion.reason);
						if self.runtime_flags.recover_inline_edits
							&& matches!(completion.reason, FinishReason::Stop)
							&& pending.is_empty()
							&& ready.is_empty()
							&& let Some(identity) =
								self.dispatcher.registry().resolved_identity("edit")
							&& identity.rev.family.as_str() == "sloppy"
							&& let Some((remaining, input, _regions)) =
								extract_inline_sloppy_edits(&text)
						{
							let assistant =
								assistant.ok_or(KernelError::MissingResponseStart)?;
							session.patch(Txn {
								cause: session.head().ok_or(SessionError::NoActiveTurn)?,
								label: Some(Str::new_static("edit.inline-recovery")),
								ops: vec![omp_dom::Op::Set {
									h: assistant,
									prop: PropId::Text.into(),
									value: omp_dom::Value::Str(Str::new(remaining.clone())),
								}],
							})?;
							text = remaining;
							let args = serde_json::value::to_raw_value(
								&serde_json::json!({"input": input}),
							)?;
							let call_id = Str::new(format!(
								"inline-edit-{}",
								session.head().ok_or(SessionError::NoActiveTurn)?
							));
							let entry = session.call(
								"edit",
								crate::journal_revision(&identity.rev),
								call_id.clone(),
								None,
								Some(args.clone()),
								None,
							)?;
							let cancellation = tool_cancellation(
								self.dispatcher.registry(),
								"edit",
								turn_cancel,
							)?;
							let mut prepared = self.dispatcher.prepare(
								identity,
								call_id.clone(),
								entry,
								cancellation,
							)?;
							prepared.arg_delta(args.get());
							prepared.commit(args);
							ready.push(prepared);
							had_tool_calls = true;
							self.events.publish(KernelEvent::ToolReady {
								call_id,
								name: Str::new_static("edit"),
							});
						}
						usage = completion.usage;
						session.assistant_end(stop_reason.clone())?;
						self.apply_live_components(session)?;
						if let (Some(hooks), Some(item)) = (&self.lifecycle_hooks, assistant) {
							hooks.notify(
								HookEventId::HookEventMessageEnd,
								serde_json::json!({
									"turn_id": current_turn(session)?.to_string(),
									"item_id": item.to_string(),
									"role": "assistant",
									"parts": completion.blocks,
									"finish": if matches!(completion.reason, FinishReason::Length) {
										"truncated"
									} else if matches!(completion.reason, FinishReason::Cancelled) {
										"interrupted"
									} else {
										"complete"
									},
								}),
							)?;
						}
						session.receipt(receipt_facts(
							&usage,
							cost_nano_usd(&completion),
							request_started,
							first_token,
						))?;
						self.apply_live_components(session)?;
						completed = true;
						break Ok(Fold::Ended);
					},
					ChatEvent::Artifact { artifact, .. } => {
						let uri = self.artifact_uri(artifact).await?;
						let sid =
							content_sid(session, assistant, &mut content_streams, u32::MAX, PropId::Text)?;
						session.stream_append(sid, uri.as_str())?;
						self.apply_live_components(session)?;
						self.events.publish(KernelEvent::TextDelta(uri.clone()));
						text.push_str(uri.as_str());
					},
					ChatEvent::WorkflowAction(action) => {
						append_notice(
							session,
							current_turn(session)?,
							Str::new(format!("provider workflow action: {}", action.name)),
						)?;
						self.apply_live_components(session)?;
					},
					ChatEvent::WorkflowResume(resume) => {
						append_notice(
							session,
							current_turn(session)?,
							Str::new(format!("provider workflow resumed: {}", resume.workflow_id)),
						)?;
						self.apply_live_components(session)?;
					},
					ChatEvent::WorkflowCancelled { invocation } => {
						append_notice(
							session,
							current_turn(session)?,
							Str::new(format!("provider workflow cancelled: {invocation}")),
						)?;
						self.apply_live_components(session)?;
					},
				}
			}
		}
		.await;
		match fold {
			Ok(Fold::Ended) => {},
			Ok(state @ (Fold::Cancelled | Fold::InvalidEditArguments)) => {
				let invalid_edit = matches!(state, Fold::InvalidEditArguments);
				close_streams(session, &mut content_streams)?;
				for (_, streaming) in pending.drain() {
					let _ = session.stream_close(streaming.sid);
					self.dispatcher.abort_prepared(
						session,
						streaming.prepared,
						if invalid_edit {
							Abort::Skipped {
								reason: Str::new_static(
									"streamed edit arguments became irrecoverably invalid before commit",
								),
							}
						} else {
							Abort::Interrupted {
								reason: Str::new_static(
									"inference cancelled before tool arguments settled",
								),
							}
						},
					)?;
				}
				for prepared in ready.drain(..) {
					self.dispatcher.abort_prepared(
						session,
						prepared,
						Abort::Interrupted {
							reason: Str::new_static("inference cancelled before tool execution"),
						},
					)?;
				}
				return Ok(DrivenInference::cancelled(text, usage));
			},
			Err(error) => {
				if let Err(journal) = close_streams(session, &mut content_streams) {
					tracing::warn!(error = ?journal, "failed to close reveal streams after a stream error");
				}
				for (_, streaming) in pending.drain() {
					let _ = session.stream_close(streaming.sid);
					self.dispatcher.abort_prepared(
						session,
						streaming.prepared,
						Abort::InputDropped,
					)?;
				}
				if ready.is_empty() {
					return Err(error);
				}
				// A trailer/read failure does not discard already-complete calls.
				// They remain a valid tool-use turn and execute below.
				append_notice(
					session,
					current_turn(session)?,
					Str::new(format!(
						"inference stream ended after complete tool calls: {}",
						error_chain(&error)
					)),
				)?;
				stop_reason = Str::new_static("tool_calls");
			},
		}
		if !completed {
			close_streams(session, &mut content_streams)?;
			self.apply_live_components(session)?;
			session.assistant_end("stream_closed")?;
			self.apply_live_components(session)?;
			session.receipt(receipt_facts(&usage, 0, request_started, first_token))?;
			self.apply_live_components(session)?;
		}
		Ok(DrivenInference {
			text: Str::new(text),
			usage,
			stop_reason,
			calls: ready,
			had_tool_calls,
			cancelled: false,
		})
	}

	async fn gate_tool_call(
		hooks: Option<crate::LifecycleHooks>,
		registry: Arc<Registry>,
		session_id: &Str,
		turn_id: &Str,
		identity: &ToolIdentity,
		call_id: &Str,
		args: Box<RawValue>,
	) -> ToolGate {
		let Some(hooks) = hooks else {
			return ToolGate::Allow { identity: identity.clone(), args };
		};
		let Ok(args_value) = serde_json::from_str::<serde_json::Value>(args.get()) else {
			return ToolGate::Deny(Str::new_static("tool-call arguments are not valid JSON"));
		};
		let rev = format!("{}@{}", identity.rev.family, identity.rev.n);
		let target = serde_json::json!({
			"kind": "core",
			"name": identity.name,
			"rev": rev,
			"args": args_value.clone(),
		});
		let payload = serde_json::json!({
			"call_id": call_id,
			"invocation_id": call_id,
			"target": target.clone(),
			"kind": "core",
			"args": args_value,
			"raw_args": {
				"$bytes": omp_core::base64::encode(args.get().as_bytes()).into_string(),
			},
			"repaired": false,
			"turn_id": turn_id,
			"session_id": session_id,
			"cwd": ".",
			"origin": "model",
			"batch": [{"call_id": call_id, "target": target}],
			"deadline": serde_json::Value::Null,
			"bash": serde_json::Value::Null,
		});
		let transformed = match hooks.gate(HookEventId::HookEventToolCall, payload).await {
			Ok(value) => value,
			Err(crate::LifecycleHookError::Denied { reason, .. }) => return ToolGate::Deny(reason),
			Err(error) => {
				tracing::warn!(?error, "tool-call lifecycle hook failed");
				return ToolGate::Deny(Str::new_static("tool-call lifecycle hook failed"));
			},
		};
		let Some(name) = transformed
			.get("target")
			.and_then(|target| target.get("name"))
			.and_then(serde_json::Value::as_str)
		else {
			return ToolGate::Deny(Str::new_static("tool-call hook removed the target name"));
		};
		let Some(identity) = registry.resolved_identity(name) else {
			return ToolGate::Deny(Str::new_static("tool-call hook selected an unknown target"));
		};
		let Some(args) = transformed.get("args") else {
			return ToolGate::Deny(Str::new_static("tool-call hook removed canonical arguments"));
		};
		match serde_json::value::to_raw_value(args) {
			Ok(args) => ToolGate::Allow { identity, args },
			Err(error) => {
				tracing::warn!(?error, "tool-call hook returned malformed arguments");
				ToolGate::Deny(Str::new_static("tool-call hook returned malformed arguments"))
			},
		}
	}

	async fn artifact_uri(&self, artifact: omp_inference::Artifact) -> Result<Str, KernelError> {
		match artifact.body {
			ArtifactBody::Bytes(bytes) => {
				let blob = self.dispatcher.policy().spill.put(&bytes)?;
				Ok(Str::new(format!("artifact://sha256/{}", blob.to_hex())))
			},
			ArtifactBody::Stored(reference) => {
				Ok(Str::new(format!("artifact://{}/{}", reference.store, reference.id)))
			},
			ArtifactBody::Stream(mut stream) => {
				let mut bytes = Vec::new();
				while let Some(chunk) = stream.next().await {
					bytes.extend_from_slice(&chunk?);
				}
				let blob = self.dispatcher.policy().spill.put(&bytes)?;
				Ok(Str::new(format!("artifact://sha256/{}", blob.to_hex())))
			},
		}
	}

	fn drain_mailbox(
		&self,
		session: &mut Session,
		turn: &crate::TurnCancellation,
	) -> Result<DrainedSteering, SessionError> {
		let mut drained = DrainedSteering::default();
		let control =
			CallControl::new(self.mailbox_rx.clone(), turn.clone(), self.cancel.clone(), None);
		while let Ok(message) = self.mailbox_rx.try_recv() {
			match control.handle(session, message)? {
				Received::Steering => drained.received = true,
				Received::Cancelled | Received::Rewound(_) => drained.cancelled = true,
				Received::None => {},
			}
		}
		Ok(drained)
	}

	/// Runs the manual compaction path between turns (`/compact`,
	/// `/handoff`): summarizes the projected history through the
	/// [`CompactionDirector`] and journals a `compaction@1` labeled
	/// `method`. Returns whether a compaction landed (an empty session
	/// projects nothing to summarize and journals nothing).
	pub async fn compact(
		&mut self,
		session: &mut Session,
		focus: Option<Str>,
		method: &'static str,
	) -> Result<bool, KernelError> {
		let Ok(turn) = current_turn(session) else {
			return Ok(false);
		};
		let request = self.build_request(session)?;
		let director = CompactionDirector::manual(focus).with_method(method);
		let route = self.route;
		let prepared = {
			let mut cx = MutDirectorCx {
				session,
				inference: &mut self.client,
				blobs: &self.dispatcher.policy().spill,
				route: &route,
				turn,
				director: None,
				events: Some(&self.events),
			};
			director.before_inference(&mut cx, &request).await?
		};
		self.apply_live_components(session)?;
		Ok(prepared == Prepared::Rebuild)
	}
}

struct StreamingCall {
	entry:    EntryId,
	sid:      u32,
	identity: ToolIdentity,
	call_id:  Str,
	prepared: PreparedCall,
	raw_args: String,
}

pub(crate) struct ReadyCall {
	pub(crate) entry:    EntryId,
	pub(crate) identity: ToolIdentity,
	pub(crate) call_id:  Str,
	pub(crate) args:     Box<RawValue>,
}

struct DrivenInference {
	text:        Str,
	usage:       Usage,
	stop_reason: Str,
	calls:          Vec<PreparedCall>,
	had_tool_calls: bool,
	cancelled:      bool,
}

impl DrivenInference {
	fn cancelled(text: String, usage: Usage) -> Self {
		Self {
			text: Str::new(text),
			usage,
			stop_reason: Str::new_static("cancelled"),
			calls: Vec::new(),
			had_tool_calls: false,
			cancelled: true,
		}
	}
}

enum ToolGate {
	Allow {
		identity: ToolIdentity,
		args:     Box<RawValue>,
	},
	Deny(Str),
}

enum PreflightSignal {
	Ready(Result<Prepared, DirectorError>),
	Control(Up),
	Cancelled,
}

enum StreamSignal {
	Event(Option<Result<ChatEvent, omp_inference::Error>>),
	Control(Option<Up>),
	Cancelled,
}

/// How one inference fold left the stream.
enum Fold {
	/// The stream completed or closed on its own.
	Ended,
	/// Caller control ended the stream before completion.
	Cancelled,
	/// Strict streamed edit validation proved the argument prefix invalid.
	InvalidEditArguments,
}

/// Renders an error with its full `source()` chain, one cause per line.
fn error_chain(error: &dyn std::error::Error) -> String {
	let mut text = error.to_string();
	let mut source = error.source();
	while let Some(cause) = source {
		text.push_str("\n  caused by: ");
		text.push_str(&cause.to_string());
		source = cause.source();
	}
	text
}

#[derive(Default)]
struct DrainedSteering {
	received:  bool,
	cancelled: bool,
}

fn content_sid(
	session: &mut Session,
	assistant: Option<Handle>,
	streams: &mut FastHashMap<u32, u32>,
	index: u32,
	prop: PropId,
) -> Result<u32, KernelError> {
	if let Some(sid) = streams.get(&index) {
		return Ok(*sid);
	}
	let sid =
		session.stream_open(assistant.ok_or(KernelError::MissingResponseStart)?, prop.into())?;
	streams.insert(index, sid);
	Ok(sid)
}

fn close_streams(
	session: &mut Session,
	streams: &mut FastHashMap<u32, u32>,
) -> Result<(), SessionError> {
	for (_, sid) in streams.drain() {
		session.stream_close(sid)?;
	}
	Ok(())
}

pub(crate) fn current_turn(session: &Session) -> Result<Handle, KernelError> {
	session
		.dom()
		.children(session.dom().body())
		.last()
		.copied()
		.ok_or(KernelError::MissingResponseStart)
}

fn current_assistant(session: &Session) -> Result<Handle, KernelError> {
	let turn = current_turn(session)?;
	session
		.dom()
		.children(turn)
		.iter()
		.rev()
		.copied()
		.find(|handle| {
			session
				.dom()
				.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
		})
		.ok_or(KernelError::MissingResponseStart)
}

fn tool_cancellation(
	registry: &Registry,
	name: &str,
	turn: &crate::TurnCancellation,
) -> Result<ToolCancellation, RegistryError> {
	let effects = registry.effects_owned(name)?;
	let mutating = effects
		.documents
		.as_ref()
		.is_some_and(|effects| !effects.write_globs.is_empty())
		|| effects
			.exec
			.as_ref()
			.is_some_and(|effects| !effects.is_empty())
		|| effects
			.inference
			.as_ref()
			.is_some_and(|effects| !effects.is_empty())
		|| effects
			.desktop
			.as_ref()
			.is_some_and(|effects| effects.input)
		|| effects.subagents != 0;
	Ok(if mutating {
		ToolCancellation::Foreground(turn.foreground_mutation())
	} else {
		ToolCancellation::ReadOnly(turn.read_only_tool())
	})
}

fn extract_inline_sloppy_edits(text: &str) -> Option<(String, String, usize)> {
	const OPEN: &str = "<SM:EDIT ";
	const CLOSE: &str = "</SM:EDIT>";
	let mut remaining = String::with_capacity(text.len());
	let mut payloads = Vec::new();
	let mut cursor = 0;
	while let Some(relative) = text[cursor..].find(OPEN) {
		let start = cursor + relative;
		let Some(close_relative) = text[start..].find(CLOSE) else {
			break;
		};
		let end = start + close_relative + CLOSE.len();
		let payload = &text[start..end];
		let valid = payload.contains("<SM:FIND>")
			&& payload.contains("</SM:FIND>")
			&& (payload.contains("<SM:PUT>") || payload.contains("<SM:PUT></SM:PUT>"))
			&& (payload.contains("</SM:PUT>") || payload.contains("<SM:PUT></SM:PUT>"));
		if !valid {
			remaining.push_str(&text[cursor..end]);
			cursor = end;
			continue;
		}
		remaining.push_str(&text[cursor..start]);
		payloads.push(payload.to_owned());
		cursor = end;
	}
	if payloads.is_empty() {
		return None;
	}
	remaining.push_str(&text[cursor..]);
	let regions = payloads.len();
	Some((remaining, payloads.join("\n"), regions))
}

fn should_schedule_autolearn(dom: &omp_dom::Dom, turn: Handle, minimum: usize) -> bool {
	let mut settled = 0_usize;
	for handle in dom.children(turn) {
		let Some(node) = dom.get(*handle) else {
			continue;
		};
		let Tag::Custom(name) = &node.tag else {
			continue;
		};
		if name.as_str() == "learn" {
			return false;
		}
		let done = node
			.prop(&omp_dom::PropKey::from(PropId::Status))
			.and_then(omp_dom::Value::as_str)
			.is_some_and(|status| matches!(status, "ok" | "error"));
		settled += usize::from(done);
	}
	settled >= minimum
}

fn streamed_edit_must_abort(con: Option<&omp_con::Ctx>, name: &str, raw: &str) -> bool {
	name == "edit"
		&& con
			.and_then(|con| con.get("sv_tools_edit_streaming_abort"))
			.is_some_and(|value| matches!(value, omp_con::Value::Bool(true)))
		&& serde_json::from_str::<serde_json::Value>(raw)
			.is_err_and(|error| error.classify() != serde_json::error::Category::Eof)
}

fn finish_reason(reason: &FinishReason) -> Str {
	match reason {
		FinishReason::Stop => Str::new_static("stop"),
		FinishReason::Length => Str::new_static("length"),
		FinishReason::ToolCalls => Str::new_static("tool_calls"),
		FinishReason::ContentFilter => Str::new_static("content_filter"),
		FinishReason::Cancelled => Str::new_static("cancelled"),
		FinishReason::Other(reason) => reason.clone(),
	}
}

/// The `turn.receipt@1` payload for one completed inference: provider usage
/// plus the kernel-clock timings pi's usage row shows (TTFT, duration →
/// tok/s).
fn receipt_facts(
	usage: &Usage,
	cost_nano_usd: u64,
	request_started: Instant,
	first_token: Option<Instant>,
) -> TurnReceipt {
	let millis = |elapsed: std::time::Duration| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
	TurnReceipt {
		tokens_in: usage.input_tokens,
		tokens_out: usage.output_tokens,
		cost_nano_usd,
		cache_read: usage.cache_read_tokens,
		cache_write: usage.cache_write_tokens,
		ttft_ms: first_token.map(|at| millis(at.duration_since(request_started))),
		duration_ms: Some(millis(request_started.elapsed())),
	}
}

fn cost_nano_usd(completion: &Completion) -> u64 {
	completion
		.receipt
		.cost
		.micro_usd
		.max(0)
		.saturating_mul(1_000)
		.try_into()
		.unwrap_or(u64::MAX)
}

pub(crate) fn outcome(stop: TurnStop, text: String, tokens_in: u64, tokens_out: u64) -> TurnOutcome {
	TurnOutcome { stop, assistant_text: Str::new(text), tokens_in, tokens_out }
}

#[cfg(test)]
mod streaming_edit_tests {
	use omp_con::{Ctx, DynamicVarSpec, Origin, TypeSpec, Value, VarFlags};
	use omp_core::Str;
	use omp_session::{ComponentRegistry, Session};

	use super::{should_schedule_autolearn, streamed_edit_must_abort};

	fn context(enabled: bool) -> Ctx {
		let ctx = Ctx::new();
		ctx.register_dynamic_var(DynamicVarSpec {
			name: Str::new_static("sv_tools_edit_streaming_abort"),
			desc: Str::new_static("Abort invalid streamed edit arguments"),
			ty: TypeSpec::BOOL,
			flags: VarFlags::SESSION,
			default: Value::Bool(false),
		})
		.expect("setting registers");
		ctx.set(
			"sv_tools_edit_streaming_abort",
			Value::Bool(enabled),
			Origin::Session,
		)
		.expect("setting writes");
		ctx
	}

	#[test]
	fn autolearn_threshold_counts_settled_non_learn_calls_at_the_boundary() {
		let temp = tempfile::tempdir().expect("tempdir");
		let mut session =
			Session::create(temp.path().join("autolearn.oms"), ComponentRegistry::standard())
				.expect("session");
		session.begin_turn().expect("turn");
		let turn = *session.dom().children(session.dom().body()).last().expect("turn");
		for index in 0..2 {
			let call = session
				.call(
					"read",
					1,
					format!("call-{index}"),
					None,
					Some(serde_json::value::to_raw_value(&serde_json::json!({})).expect("args")),
					None,
				)
				.expect("call");
			session
				.settle(
					call,
					serde_json::value::to_raw_value(&serde_json::json!({})).expect("outcome"),
				)
				.expect("settle");
		}
		assert!(!should_schedule_autolearn(session.dom(), turn, 3));
		assert!(should_schedule_autolearn(session.dom(), turn, 2));
		let learn = session
			.call(
				"learn",
				1,
				"learn-1",
				None,
				Some(serde_json::value::to_raw_value(&serde_json::json!({})).expect("args")),
				None,
			)
			.expect("learn");
		session
			.settle(
				learn,
				serde_json::value::to_raw_value(&serde_json::json!({})).expect("outcome"),
			)
			.expect("settle learn");
		assert!(!should_schedule_autolearn(session.dom(), turn, 2));
	}

	#[test]
	fn edit_streaming_abort_only_fires_when_enabled_and_irrecoverably_invalid() {
		let disabled = context(false);
		let enabled = context(true);
		assert!(!streamed_edit_must_abort(Some(&disabled), "edit", r#"{"input":]"#));
		assert!(streamed_edit_must_abort(Some(&enabled), "edit", r#"{"input":]"#));
		assert!(!streamed_edit_must_abort(Some(&enabled), "edit", r#"{"input":"#));
		assert!(!streamed_edit_must_abort(Some(&enabled), "read", r#"{"input":]"#));
	}
}

pub(crate) fn cancelled_outcome() -> TurnOutcome {
	TurnOutcome {
		stop:           TurnStop::Cancelled,
		assistant_text: Str::new_static(""),
		tokens_in:      0,
		tokens_out:     0,
	}
}
