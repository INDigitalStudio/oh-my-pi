//! Whole-lifetime tool dispatch and central execution policy.
//!
//! A call is *prepared* the moment the provider names it (ADR 0008: the
//! executor consumes argument streaming live), *committed* when its canonical
//! arguments settle, and *driven* by one multiplexed loop that journals every
//! event of every call in a batch. Independent read-only calls run
//! concurrently; a mutating call is exclusive within its batch. Every call
//! settles through exactly one path: a typed terminal, a harness abort, or
//! detachment into the job primitive (ADR 0010) when it outlives the central
//! blocking limit.

use std::{
	future::Future,
	io::Write as _,
	pin::Pin,
	sync::Arc,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use flume::Receiver;
use futures::{Stream, StreamExt as _, future::select_all};
use omp_core::{FastHashMap, Str, sf};
use omp_dom::{Handle, KnownTag, PropId, Sid, Tag};
use omp_journal::{
	EntryId,
	blob::{BlobRef, BlobStage, BlobStore},
};
use omp_session::{Session, SessionError};
use omp_tool::{
	Abort, ArtifactLifetime, BlobRef as ToolBlobRef, CallOutcome, CallOutcomeDetails, CapsBase,
	ErasedEv, ErasedOutcome, ExpectedArtifact, IncomingParams, Interrupt, InvocationFeed, JobKind,
	JobMetadata, JobOwner, JobRef, ModelClass, Part, PromptCaps, Registry, RegistryError, Rev,
	ToolIdentity, ToolRoute, ToolSpec,
};
use serde_json::value::RawValue;
use thiserror::Error;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
	CancelTree, JobBoard, KernelEvent, SessionAuthority, TurnCancellation, Up,
	cancel::{BackgroundToolCancellation, ForegroundMutationCancellation, ReadOnlyToolCancellation},
	events::KernelEvents,
	steering,
};

/// Cancellation authority selected from a tool's declared effects.
#[derive(Clone, Debug)]
pub enum ToolCancellation {
	/// Session-only cancellation for a foreground mutation.
	Foreground(ForegroundMutationCancellation),
	/// Turn-scoped cancellation for a read-only call.
	ReadOnly(ReadOnlyToolCancellation),
	/// Turn-scoped cancellation for detached or background work.
	Background(BackgroundToolCancellation),
}

impl ToolCancellation {
	/// Host stop request for this call: turn interruption or session
	/// cancellation for every scope.
	fn interrupt_token(&self) -> CancellationToken {
		match self {
			Self::Foreground(scope) => scope.interrupt_token(),
			Self::ReadOnly(scope) => scope.token(),
			Self::Background(scope) => scope.token(),
		}
	}

	/// Whether the call mutates shared state and therefore runs exclusively
	/// within its batch (pi: mutating calls never overlap).
	const fn is_exclusive(&self) -> bool {
		matches!(self, Self::Foreground(_))
	}
}

/// Central policy applied once to every tool call.
#[derive(Clone, Debug)]
pub struct DispatchPolicy {
	/// Maximum inline output bytes.
	pub max_output_bytes: usize,
	/// Maximum bytes retained from one output line.
	pub max_line_bytes:   usize,
	/// Maximum time a call may block the turn.
	pub blocking_limit:   Duration,
	/// Bounded wait after a stop request before a call that has not settled
	/// is forcibly terminated and journaled as effects-unknown (ADR 0011).
	/// Execution units apply their own courtesy grace inside this bound.
	pub interrupt_grace:  Duration,
	/// Content-addressed store for complete spilled output.
	pub spill:            BlobStore,
}

impl DispatchPolicy {
	/// Creates the standard 64 KiB / 512-byte / 30-second / 1-second policy.
	#[must_use]
	pub const fn new(spill: BlobStore) -> Self {
		Self {
			max_output_bytes: 64 * 1024,
			max_line_bytes: 512,
			blocking_limit: Duration::from_secs(30),
			interrupt_grace: Duration::from_secs(1),
			spill,
		}
	}

	/// Replaces the bounded settle window granted after a stop request.
	#[must_use]
	pub const fn with_interrupt_grace(mut self, interrupt_grace: Duration) -> Self {
		self.interrupt_grace = interrupt_grace;
		self
	}

	/// Replaces central limits while retaining the selected blob store.
	#[must_use]
	pub const fn with_limits(
		mut self,
		max_output_bytes: usize,
		max_line_bytes: usize,
		blocking_limit: Duration,
	) -> Self {
		self.max_output_bytes = max_output_bytes;
		self.max_line_bytes = max_line_bytes;
		self.blocking_limit = blocking_limit;
		self
	}
}

/// Per-call policy choices parsed from model arguments.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DispatchOptions {
	/// Explicitly bypasses both central inline limits.
	pub notrunc: bool,
}

impl DispatchOptions {
	/// Reads the caller-owned `notrunc` escape hatch from canonical arguments.
	#[must_use]
	pub fn from_args(args: &RawValue) -> Self {
		let notrunc = serde_json::from_str::<serde_json::Value>(args.get())
			.ok()
			.and_then(|value| value.get("notrunc").and_then(serde_json::Value::as_bool))
			.unwrap_or(false);
		Self { notrunc }
	}
}

/// One authorized invocation ready for registry dispatch.
pub struct DispatchRequest {
	/// Exact live tool identity recorded on the call element.
	pub identity:     ToolIdentity,
	/// Stable provider call identity.
	pub call_id:      Str,
	/// Journal identity of the corresponding `tool.call@1`.
	pub call:         EntryId,
	/// Canonical committed argument object.
	pub args:         Box<RawValue>,
	/// Central caller choices.
	pub options:      DispatchOptions,
	/// Cancellation scope selected from the tool's effects.
	pub cancellation: ToolCancellation,
}

/// One externally routed invocation with committed canonical arguments.
pub struct ExternalDispatchRequest {
	/// Exact selected tool identity.
	pub identity:       ToolIdentity,
	/// Stable provider call identity.
	pub call_id:        Str,
	/// Canonical committed argument object.
	pub args:           Box<RawValue>,
	/// Resolved worker or remote execution route.
	pub route:          ToolRoute,
	/// Maximum time the invocation may block this turn.
	pub blocking_limit: Duration,
	/// Turn/session cancellation the executor must honor (ADR 0011): once
	/// cancelled, the invocation is interrupted and settles aborted.
	pub cancellation:   CancellationToken,
}

/// One state mutation produced by an externally routed tool executor.
pub enum ExternalDispatchEvent {
	/// Ephemeral structured progress.
	Update(Box<RawValue>),
	/// Durable structured outcome and its canonical model-facing projection.
	Done {
		/// Serialized `CallOutcome` truth.
		outcome:  Box<RawValue>,
		/// Canonical bounded-later model-facing parts.
		parts:    Vec<Part>,
		/// Whether the outcome is model-facing error content.
		is_error: bool,
	},
	/// Execution stopped without a normal typed verdict.
	Aborted(Abort),
}

/// Owned externally routed tool event stream.
pub type ExternalDispatchStream =
	Pin<Box<dyn Stream<Item = ExternalDispatchEvent> + Send + 'static>>;

/// Host composition seam for worker- and remote-routed tool execution.
pub trait ExternalToolExecutor: Send + Sync {
	/// Opens one committed invocation. Every stream must end in `Done` or
	/// `Aborted`; transport failures are mapped to an explicit abort by the
	/// adapter while their typed source is logged at that boundary.
	fn invoke(&self, request: ExternalDispatchRequest) -> ExternalDispatchStream;
}

/// One boxed cold-call future at the session-tool dynamic quarantine.
///
/// Session-owned tools are rare, spawn-scale operations (`task`, `hub`), so
/// one allocation per call is intentional; ordinary tools retain static
/// dispatch through [`Registry`].
pub type SessionToolFuture<'a> = Pin<
	Box<
		dyn Future<Output = Result<CallOutcome<Box<RawValue>, Box<RawValue>>, SessionToolError>>
			+ Send
			+ 'a,
	>,
>;

/// The kernel's live control surface handed to whoever holds the session
/// while a call runs: the upward mailbox plus the cancellation scopes it
/// steers. Steering received here is journaled on receipt.
#[derive(Clone)]
pub struct CallControl {
	mailbox: Receiver<Up>,
	turn:    TurnCancellation,
	session: CancelTree,
	run:     Option<crate::RunControl>,
}

/// What one handled mailbox message meant for the running batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Received {
	/// Nothing the batch must react to.
	None,
	/// Steering was journaled: not-yet-started calls are skipped and the
	/// batch stops at the next safe point.
	Steering,
	/// The authoritative session rewound and runtime lifecycle must follow.
	Rewound(omp_session::LifecycleWork),
	/// The turn or session was cancelled.
	Cancelled,
}

impl CallControl {
	pub(crate) const fn new(
		mailbox: Receiver<Up>,
		turn: TurnCancellation,
		session: CancelTree,
		run: Option<crate::RunControl>,
	) -> Self {
		Self { mailbox, turn, session, run }
	}

	/// Receives the next mailbox message; pending forever once the kernel is
	/// gone.
	pub async fn recv(&self) -> Up {
		match self.mailbox.recv_async().await {
			Ok(message) => message,
			Err(_) => std::future::pending().await,
		}
	}

	/// Resolves when the caller-owned run control (deadline or external
	/// cancellation) fires; pending forever without one.
	pub async fn run_expired(&self) {
		match &self.run {
			Some(run) => run.cancelled().await,
			None => std::future::pending().await,
		}
	}

	/// Whether the turn was interrupted or the session cancelled.
	#[must_use]
	pub fn is_cancelled(&self) -> bool {
		self.turn.is_turn_cancelled() || self.session.is_session_cancelled()
	}

	/// Interrupts the current turn.
	pub fn cancel_turn(&self) {
		self.turn.cancel_turn();
	}

	/// Applies one mailbox message to the session: steering and notices are
	/// journaled immediately, approvals decided, subscriptions served, and
	/// interrupts routed to the cancellation tree.
	pub fn handle(&self, session: &mut Session, message: Up) -> Result<Received, SessionError> {
		match message {
			Up::Steer(text) => {
				steering::queue_steering(session, text)?;
				Ok(Received::Steering)
			},
			Up::Peer(text) => {
				steering::queue_peer(session, text)?;
				Ok(Received::None)
			},
			Up::Unqueue(reply) => {
				let _ = reply.send(steering::unqueue_steering(session)?);
				Ok(Received::None)
			},
			Up::Interrupt => {
				self.turn.cancel_turn();
				Ok(Received::Cancelled)
			},
			Up::Cancel => {
				self.session.cancel_session();
				self.turn.cancel_turn();
				Ok(Received::Cancelled)
			},
			Up::Env(event) => Ok(journal_env_event(session, event)?
				.map_or(Received::None, Received::Rewound)),
			Up::Approve { id, decision } => {
				let _ = crate::ApprovalBook::new().decide(session, id.as_str(), decision);
				Ok(Received::None)
			},
			Up::Subscribe(reply) => {
				let _ = reply.send(session.subscribe());
				Ok(Received::None)
			},
		}
	}
}

/// Journals an environment observation under the current turn.
pub(crate) fn journal_env_event(
	session: &mut Session,
	event: crate::EnvEvent,
) -> Result<Option<omp_session::LifecycleWork>, SessionError> {
	let Ok(turn) = crate::current_turn(session) else {
		return Ok(None);
	};
	match event {
		crate::EnvEvent::DeviceAvailability { payload } => {
			steering::append_notice(session, turn, payload)?;
			Ok(None)
		},
		crate::EnvEvent::StagedPreview { proposal_id, source_tool } => {
			steering::append_notice(
				session,
				turn,
				Str::new(format!(
					"Staged proposal {proposal_id} from {source_tool} awaits `dyn resolve` or dyn \
					 reject."
				)),
			)?;
			Ok(None)
		},
		crate::EnvEvent::CheckpointControl { operation, payload } => {
			checkpoint_control(session, operation.as_str(), payload.as_str())
		},
		crate::EnvEvent::Notice { kind, name, body } => {
			steering::append_named_notice(session, turn, kind, name, body)?;
			Ok(None)
		},
	}
}

fn checkpoint_control(
	session: &mut Session,
	operation: &str,
	payload: &str,
) -> Result<Option<omp_session::LifecycleWork>, SessionError> {
	let value: serde_json::Value = serde_json::from_str(payload)?;
	let Some(token) = value.get("token").and_then(serde_json::Value::as_str) else {
		return Ok(None);
	};
	if operation == "checkpoint" {
		let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
		session.patch(omp_dom::Txn {
			cause,
			label: Some(Str::new_static("checkpoint.open")),
			ops: vec![omp_dom::Op::Ins {
				parent: session.dom().meta(),
				after: session.dom().children(session.dom().meta()).last().copied(),
				node: omp_dom::NodeSpec::new(omp_dom::Tag::Custom(Str::new_static("rewind-checkpoint")))
					.with_prop(
						omp_dom::PropKey::Custom(Str::new_static("token")),
						omp_dom::Value::Str(Str::new(token)),
					)
					.with_prop(
						omp_dom::PropKey::Custom(Str::new_static("target")),
						omp_dom::Value::Str(Str::new(cause.to_string())),
					),
			}],
		})?;
		return Ok(None);
	}
	if operation != "schedule_rewind" {
		return Ok(None);
	}
	let target = session.dom().handles().find_map(|handle| {
		let node = session.dom().get(handle)?;
		if node.tag != omp_dom::Tag::Custom(Str::new_static("rewind-checkpoint"))
			|| node
				.prop(&omp_dom::PropKey::Custom(Str::new_static("token")))
				.and_then(omp_dom::Value::as_str)
				!= Some(token)
		{
			return None;
		}
		node
			.prop(&omp_dom::PropKey::Custom(Str::new_static("target")))
			.and_then(omp_dom::Value::as_str)?
			.parse::<EntryId>()
			.ok()
	});
	target.map(|target| session.rewind(target)).transpose()
}

/// Runtime context available only to a session-owned tool.
pub struct SessionToolCx<'a> {
	/// Authoritative parent session controller.
	pub session:   &'a mut Session,
	/// Materialized tool-call element.
	pub call:      Handle,
	/// Disposable runtime index over `<meta><jobs>`.
	pub jobs:      &'a JobBoard,
	/// Kill boundary for detached work.
	pub cancel:    BackgroundToolCancellation,
	/// Host-owned routing authority for live peer sessions.
	pub authority: Option<&'a dyn SessionAuthority>,
	/// The kernel's mailbox and cancellation scopes, for tools that wait
	/// (`hub wait`): steering and interrupts are observed while blocked.
	pub control:   Option<&'a CallControl>,
}

/// Failure before a session tool can produce its typed terminal outcome.
#[derive(Debug, Error)]
pub enum SessionToolError {
	/// Session-tool argument or outcome JSON was malformed.
	#[error("session tool JSON is invalid")]
	Json(#[from] serde_json::Error),
	/// Host composition rejected the operation before a typed tool fault.
	#[error("{message}")]
	Rejected {
		/// Stable diagnostic.
		message: Str,
	},
	/// Journaling a job or steering side effect failed.
	#[error(transparent)]
	Session(#[from] SessionError),
}

/// A host-authority tool whose operation requires the session DOM.
///
/// Implementations must journal through `Session`; private durable state is
/// prohibited.
pub trait SessionTool: Send + Sync {
	/// Exact model-facing declaration.
	fn spec(&self) -> &ToolSpec;
	/// Executes one committed call against the authoritative session.
	fn call<'a>(&'a self, cx: SessionToolCx<'a>, args: Box<RawValue>) -> SessionToolFuture<'a>;
}

/// Live ordered output of one dispatched call (ADR 0008 tool output
/// streaming), bounded on the host side (ADR 0009).
///
/// A tool update carrying `sequence` and `data` is an ordered output frame
/// (`omp_tools::shell::Update`, eval process frames). The dispatcher binds a
/// DOM text stream to the call's `<result>` text at the first frame, appends
/// each frame's bytes as UTF-8 in sequence order up to the central inline
/// limit, and diverts everything beyond it into a content-addressed spill
/// stage so neither the DOM, the journal, nor an actor's patch queue ever
/// receives more than the policy allows. The typed update still journals the
/// frame's metadata; its bytes live only in the stream or the artifact.
pub(crate) struct OutputStream {
	sid:     Option<Sid>,
	/// Highest sequence appended; stale or duplicate frames are dropped.
	last:    Option<u64>,
	/// Bytes of a UTF-8 sequence split across frames, completed by the next.
	carry:   Vec<u8>,
	/// Inline byte budget; `None` when the caller opted out with `notrunc`.
	limit:   Option<usize>,
	/// Bytes revealed inline so far.
	shown:   usize,
	/// Inline prefix retained until an overflow opens the spill stage, so the
	/// artifact holds the complete output.
	prefix:  String,
	/// Complete-output spill once the inline budget is exhausted.
	stage:   Option<BlobStage>,
	/// Whether any byte was diverted from the DOM.
	spilled: bool,
}

impl OutputStream {
	fn new(limit: Option<usize>) -> Self {
		Self {
			sid: None,
			last: None,
			carry: Vec::new(),
			limit,
			shown: 0,
			prefix: String::new(),
			stage: None,
			spilled: false,
		}
	}

	/// Reads the frame's ordering and bytes when `value` is an output frame.
	fn frame(value: &serde_json::Value) -> Option<(u64, Vec<u8>)> {
		let sequence = value.get("sequence")?.as_u64()?;
		let bytes = match value.get("data")? {
			serde_json::Value::String(text) => text.as_bytes().to_vec(),
			serde_json::Value::Array(items) => items
				.iter()
				.filter_map(serde_json::Value::as_u64)
				.filter_map(|byte| u8::try_from(byte).ok())
				.collect(),
			_ => return None,
		};
		Some((sequence, bytes))
	}

	/// Decodes a frame's bytes, carrying an incomplete trailing sequence to
	/// the next frame instead of replacing it.
	fn decode(&mut self, bytes: &[u8]) -> String {
		let mut buffer = std::mem::take(&mut self.carry);
		buffer.extend_from_slice(bytes);
		let text = match std::str::from_utf8(&buffer) {
			Ok(text) => text.to_owned(),
			Err(error) if error.error_len().is_none() => {
				let valid = error.valid_up_to();
				let text = String::from_utf8_lossy(&buffer[..valid]).into_owned();
				buffer.drain(..valid);
				self.carry = buffer;
				return text;
			},
			Err(_) => String::from_utf8_lossy(&buffer).into_owned(),
		};
		buffer.clear();
		self.carry = buffer;
		text
	}

	/// Appends one frame in order; returns the update with its bytes
	/// removed so they are not journaled twice.
	fn push(
		&mut self,
		session: &mut Session,
		call: EntryId,
		spill: &BlobStore,
		mut value: serde_json::Value,
		sequence: u64,
		bytes: &[u8],
	) -> Result<Box<RawValue>, DispatchError> {
		if self.last.is_none_or(|last| sequence > last) {
			self.last = Some(sequence);
			let text = self.decode(bytes);
			if !text.is_empty() {
				self.reveal(session, call, spill, &text)?;
			}
		}
		if let Some(data) = value.get_mut("data") {
			*data = match data {
				serde_json::Value::String(_) => serde_json::Value::String(String::new()),
				_ => serde_json::Value::Array(Vec::new()),
			};
		}
		Ok(serde_json::value::to_raw_value(&value)?)
	}

	/// Reveals `text` inline up to the budget and diverts the rest to the
	/// spill stage.
	fn reveal(
		&mut self,
		session: &mut Session,
		call: EntryId,
		spill: &BlobStore,
		text: &str,
	) -> Result<(), DispatchError> {
		let available = self
			.limit
			.map_or(usize::MAX, |limit| limit.saturating_sub(self.shown));
		let visible = utf8_prefix(text, available);
		if !visible.is_empty() {
			let sid = match self.sid {
				Some(sid) => sid,
				None => {
					let sid = session.stream_open(result_handle(session, call)?, PropId::Text.into())?;
					self.sid = Some(sid);
					sid
				},
			};
			session.stream_append(sid, visible)?;
			self.shown += visible.len();
			if self.stage.is_none() {
				self.prefix.push_str(visible);
			}
		}
		let overflow = &text[visible.len()..];
		if overflow.is_empty() {
			return Ok(());
		}
		self.spilled = true;
		let stage = match self.stage.as_mut() {
			Some(stage) => stage,
			None => {
				let mut stage = spill.begin_put()?;
				stage
					.write_all(std::mem::take(&mut self.prefix).as_bytes())
					.map_err(omp_journal::blob::Error::from)?;
				self.stage.insert(stage)
			},
		};
		stage
			.write_all(overflow.as_bytes())
			.map_err(omp_journal::blob::Error::from)?;
		Ok(())
	}

	/// Closes the stream, flushing any dangling partial sequence lossily, and
	/// finalizes the complete-output artifact when bytes were diverted.
	fn close(&mut self, session: &mut Session) -> Result<Option<BlobRef>, DispatchError> {
		if !self.carry.is_empty() {
			let tail = String::from_utf8_lossy(&self.carry).into_owned();
			self.carry.clear();
			if let Some(stage) = self.stage.as_mut() {
				stage
					.write_all(tail.as_bytes())
					.map_err(omp_journal::blob::Error::from)?;
			} else if let Some(sid) = self.sid {
				session.stream_append(sid, &tail)?;
			}
		}
		if let Some(sid) = self.sid.take() {
			session.stream_close(sid)?;
		}
		let spilled = self.stage.take().map(BlobStage::finish).transpose()?;
		Ok(spilled)
	}
}

/// The `<result>` element of a live call.
fn result_handle(session: &Session, call: EntryId) -> Result<Handle, DispatchError> {
	let element = session.call_handle(call)?;
	let dom = session.dom();
	dom.children(element)
		.iter()
		.copied()
		.find(|child| {
			dom.get(*child)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Result))
		})
		.ok_or(DispatchError::Session(SessionError::UnknownCall { id: call }))
}

/// Durable result of one dispatched invocation.
#[derive(Clone, Debug)]
pub struct DispatchReport {
	/// Whether the model-facing terminal is an error.
	pub is_error:      bool,
	/// Complete-output artifact created by central bounding.
	pub spilled:       Option<BlobRef>,
	/// Number of individual lines clamped.
	pub lines_clamped: u64,
	/// Job reference when execution detached.
	pub detached:      Option<JobRef>,
}

/// Registry dispatch, projection, persistence, or journal failure.
#[derive(Debug, Error)]
pub enum DispatchError {
	/// Tool registry operation failed.
	#[error(transparent)]
	Registry(#[from] RegistryError),
	/// Session journal or DOM fold failed.
	#[error(transparent)]
	Session(#[from] SessionError),
	/// Blob persistence failed.
	#[error(transparent)]
	Blob(#[from] omp_journal::blob::Error),
	/// JSON serialization failed.
	#[error(transparent)]
	Json(#[from] serde_json::Error),
	/// Tool event task failed independently of its terminal stream.
	#[error(transparent)]
	Join(#[from] JoinError),
	/// A lifecycle hook rejected or malformed a tool transition.
	#[error(transparent)]
	LifecycleHook(#[from] crate::LifecycleHookError),
	/// Invocation input was dropped before the executor received commitment.
	#[error("tool invocation input channel closed before commitment")]
	InputClosed,
	/// Session-owned tool failed before producing a terminal outcome.
	#[error(transparent)]
	SessionTool(#[from] SessionToolError),
	/// No host executor was injected for an externally routed tool.
	#[error("externally routed tool {name} has no host executor")]
	ExternalExecutorMissing {
		/// Selected tool name.
		name: Str,
	},
	/// A model-facing JSON part contained invalid UTF-8.
	#[error("tool JSON projection is not UTF-8")]
	ProjectionUtf8 {
		/// UTF-8 validation failure.
		#[source]
		source: std::str::Utf8Error,
	},
	/// A prepared call was driven before its arguments were committed.
	#[error("tool call {call_id} was driven before its arguments were committed")]
	Uncommitted {
		/// Stable provider call identity.
		call_id: Str,
	},
}

/// One event from an execution unit, native or external.
pub(crate) enum DispatchEvent {
	Native(Result<ErasedEv, RegistryError>),
	External(ExternalDispatchEvent),
}

/// The journaling half of dispatch: registry projection, central bounding,
/// and terminal commitment. Cloned into detached jobs so they settle through
/// the same code the foreground path uses.
#[derive(Clone)]
pub(crate) struct Committer {
	registry: Arc<Registry>,
	policy:          DispatchPolicy,
	events:          KernelEvents,
	lifecycle_hooks: Option<crate::LifecycleHooks>,
}

/// Executes registry calls and commits every event through `omp-session`.
#[derive(Clone)]
pub struct Dispatcher {
	committer:     Committer,
	external:      Option<Arc<dyn ExternalToolExecutor>>,
	session_tools: FastHashMap<Str, Arc<dyn SessionTool>>,
	jobs:          Arc<JobBoard>,
	authority:     Option<Arc<dyn SessionAuthority>>,
}

/// Where a prepared call executes.
enum Unit {
	/// In-process registry tool fed live argument fragments.
	Native {
		feed: InvocationFeed,
	},
	/// Worker/remote tool started at commit with complete arguments.
	External {
		executor: Arc<dyn ExternalToolExecutor>,
		route:    ToolRoute,
		event_tx: Option<flume::Sender<DispatchEvent>>,
	},
	/// Host-authority tool run inline against the session.
	Session(Arc<dyn SessionTool>),
	/// Runtime-only view used to settle an already-detached unit.
	Detached {
		feed: Option<InvocationFeed>,
	},
}

/// Lifecycle of a prepared call within its batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
	/// Committed and waiting for the scheduler.
	Pending,
	/// Executing.
	Running,
	/// Stop requested; the unit has until the grace expires.
	Interrupting,
	/// Journaled a terminal.
	Settled,
}

/// A call whose execution unit is open and consuming argument streaming.
pub struct PreparedCall {
	identity:     ToolIdentity,
	call_id:      Str,
	call:         EntryId,
	cancellation: ToolCancellation,
	interrupt:    CancellationToken,
	unit:         Unit,
	events:       Receiver<DispatchEvent>,
	task:         Option<tokio::task::JoinHandle<Result<(), RegistryError>>>,
	args:         Option<Box<RawValue>>,
	options:      DispatchOptions,
	output:       Option<OutputStream>,
	phase:        Phase,
	started:      Option<Instant>,
	grace_until:  Option<Instant>,
	closed:       bool,
	report:       Option<DispatchReport>,
}

impl PreparedCall {
	/// Journal identity of the `tool.call@1`.
	#[must_use]
	pub const fn entry(&self) -> EntryId {
		self.call
	}

	/// Stable provider call identity.
	#[must_use]
	pub fn call_id(&self) -> &Str {
		&self.call_id
	}

	/// Exact tool identity.
	#[must_use]
	pub const fn identity(&self) -> &ToolIdentity {
		&self.identity
	}

	/// Whether canonical arguments were committed.
	#[must_use]
	pub const fn is_committed(&self) -> bool {
		self.args.is_some()
	}

	/// Feeds one streamed argument fragment to the executor as it arrives
	/// (ADR 0008: preview work happens once, while arguments stream).
	pub fn arg_delta(&self, fragment: &str) {
		if let Unit::Native { feed } = &self.unit {
			// A closed feed means the unit already ended (e.g. it rejected the
			// call); commitment reports that as the terminal.
			let _ = feed.arg_text(Str::new(fragment));
		}
	}

	/// Records the canonical committed arguments. Execution starts when the
	/// batch scheduler admits the call.
	pub fn commit(&mut self, args: Box<RawValue>) {
		self.options = DispatchOptions::from_args(&args);
		self.args = Some(args);
	}

	fn is_exclusive(&self) -> bool {
		self.cancellation.is_exclusive() || matches!(self.unit, Unit::Session(_))
	}

	fn output(&mut self, policy: &DispatchPolicy) -> &mut OutputStream {
		let limit = (!self.options.notrunc).then_some(policy.max_output_bytes);
		self.output.get_or_insert_with(|| OutputStream::new(limit))
	}

	/// Discards a speculative execution unit before replacing transformed input.
	pub(crate) fn discard(&mut self) {
		if let Some(task) = self.task.take() {
			task.abort();
		}
	}

	/// Whether an executor task is still running.
	fn is_live(&self) -> bool {
		matches!(self.phase, Phase::Running | Phase::Interrupting)
	}
}

impl Dispatcher {
	/// Creates a dispatcher over one runtime registry and central policy.
	#[must_use]
	pub fn new(registry: Arc<Registry>, policy: DispatchPolicy) -> Self {
		Self {
			committer: Committer {
				registry,
				policy,
				events: KernelEvents::default(),
				lifecycle_hooks: None,
			},
			external: None,
			session_tools: FastHashMap::default(),
			jobs: Arc::new(JobBoard::new()),
			authority: None,
		}
	}

	/// Injects the host adapter for worker- and remote-routed tools.
	#[must_use]
	pub fn with_external_executor(mut self, executor: Arc<dyn ExternalToolExecutor>) -> Self {
		self.external = Some(executor);
		self
	}

	/// Registers a session-authority tool before registry route lookup.
	#[must_use]
	pub fn with_session_tool(mut self, tool: Arc<dyn SessionTool>) -> Self {
		self.session_tools.insert(tool.spec().name.clone(), tool);
		self
	}

	/// Uses the supplied runtime job index for session tools and rewind work.
	#[must_use]
	pub fn with_job_board(mut self, jobs: Arc<JobBoard>) -> Self {
		self.jobs = jobs;
		self
	}

	/// Injects the host-owned live-session routing authority.
	#[must_use]
	pub fn with_session_authority(mut self, authority: Arc<dyn SessionAuthority>) -> Self {
		self.authority = Some(authority);
		self
	}

	pub(crate) fn with_events(mut self, events: KernelEvents) -> Self {
		self.committer.events = events;
		self
	}

	pub(crate) fn with_lifecycle_hooks(mut self, hooks: crate::LifecycleHooks) -> Self {
		self.committer.lifecycle_hooks = Some(hooks);
		self
	}

	/// Borrows the runtime registry.
	#[must_use]
	pub fn registry(&self) -> &Arc<Registry> {
		&self.committer.registry
	}

	/// Borrows the central dispatch policy.
	#[must_use]
	pub const fn policy(&self) -> &DispatchPolicy {
		&self.committer.policy
	}

	/// Borrows the runtime job index.
	#[must_use]
	pub fn jobs(&self) -> &Arc<JobBoard> {
		&self.jobs
	}

	/// Opens the execution unit for a call the provider just named. Native
	/// tools start consuming argument fragments immediately; they act only
	/// after [`PreparedCall::commit`] and scheduler admission.
	pub fn prepare(
		&self,
		identity: ToolIdentity,
		call_id: Str,
		call: EntryId,
		cancellation: ToolCancellation,
	) -> Result<PreparedCall, DispatchError> {
		let interrupt = cancellation.interrupt_token();
		let (event_tx, events) = flume::unbounded();
		let name = identity.name.clone();
		if let Some(tool) = self.session_tools.get(&name).cloned() {
			return Ok(PreparedCall {
				identity,
				call_id,
				call,
				cancellation,
				interrupt,
				unit: Unit::Session(tool),
				events,
				task: None,
				args: None,
				options: DispatchOptions::default(),
				output: None,
				phase: Phase::Pending,
				started: None,
				grace_until: None,
				closed: true,
				report: None,
			});
		}
		let route = self.committer.registry.route(name.as_str())?;
		let (unit, task) = match route {
			ToolRoute::Native => {
				let (feed, params) = IncomingParams::channel_for(None, Some(call_id.clone()));
				let registry = Arc::clone(&self.committer.registry);
				let task = tokio::spawn(async move {
					let mut stream = registry.invoke(name.as_str(), params)?;
					while let Some(event) = stream.next().await {
						if event_tx.send(DispatchEvent::Native(event)).is_err() {
							break;
						}
					}
					Ok::<_, RegistryError>(())
				});
				(Unit::Native { feed }, Some(task))
			},
			route => {
				let executor = self
					.external
					.clone()
					.ok_or_else(|| DispatchError::ExternalExecutorMissing { name: name.clone() })?;
				(Unit::External { executor, route, event_tx: Some(event_tx) }, None)
			},
		};
		Ok(PreparedCall {
			identity,
			call_id,
			call,
			cancellation,
			interrupt,
			unit,
			events,
			task,
			args: None,
			options: DispatchOptions::default(),
			output: None,
			phase: Phase::Pending,
			started: None,
			grace_until: None,
			closed: false,
			report: None,
		})
	}

	/// Drives one authorized call to exactly one journaled terminal.
	pub async fn dispatch(
		&self,
		session: &mut Session,
		request: DispatchRequest,
	) -> Result<DispatchReport, DispatchError> {
		let mut call =
			self.prepare(request.identity, request.call_id, request.call, request.cancellation)?;
		call.commit(request.args);
		call.options = request.options;
		let mut reports = self.drive(session, vec![call], None).await?;
		Ok(reports.remove(0))
	}

	/// Settles a prepared call that will never run (the stream that named it
	/// was cancelled or failed) with a harness abort, so the journal never
	/// keeps an unpaired call.
	pub fn abort_prepared(
		&self,
		session: &mut Session,
		mut call: PreparedCall,
		abort: Abort,
	) -> Result<DispatchReport, DispatchError> {
		if let Some(task) = call.task.take() {
			task.abort();
		}
		let mut output = std::mem::take(call.output(&self.committer.policy));
		self.committer.commit_abort(session, &call, abort, &mut output)
	}

	/// Drives a batch of committed calls to their terminals, journaling every
	/// event through the session as it arrives. Read-only calls run
	/// concurrently; a mutating or session-owned call is exclusive. Steering
	/// journaled meanwhile skips every call that has not started (pi
	/// `interrupt_skipped`); a call outliving the blocking limit detaches into
	/// the job primitive; a stop request follows the cooperative → grace →
	/// forced ladder (ADR 0011). Reports are returned in batch order.
	pub async fn drive(
		&self,
		session: &mut Session,
		mut calls: Vec<PreparedCall>,
		control: Option<&CallControl>,
	) -> Result<Vec<DispatchReport>, DispatchError> {
		for call in &calls {
			if !call.is_committed() {
				return Err(DispatchError::Uncommitted { call_id: call.call_id.clone() });
			}
		}
		let policy = self.committer.policy.clone();
		loop {
			self.admit(session, &mut calls, control).await?;
			if calls.iter().all(|call| call.phase == Phase::Settled) {
				break;
			}
			if let Some(index) =
				calls.iter().position(|call| call.closed && call.task.is_some())
			{
				let task = calls[index].task.take().expect("filtered on presence");
				let joined = task.await;
				let call = &mut calls[index];
				match joined {
					Ok(Ok(())) => {},
					Ok(Err(error)) => return Err(error.into()),
					Err(error) => return Err(error.into()),
				}
				let mut terminal = None;
				while let Ok(event) = call.events.try_recv() {
					if let Some(report) = self.committer.apply_event(session, call, event)? {
						terminal = Some(report);
						break;
					}
				}
				let report = match terminal {
					Some(report) => report,
					None => {
						let mut output = std::mem::take(call.output(&policy));
						self
							.committer
							.commit_abort(session, call, Abort::MissingOutcome, &mut output)?
					},
				};
				call.phase = Phase::Settled;
				call.report = Some(report);
				self.on_settled(session, &mut calls, control).await?;
				continue;
			}
			let now = Instant::now();
			let deadline = calls
				.iter()
				.filter(|call| call.phase == Phase::Running)
				.filter_map(|call| call.started)
				.map(|started| started + policy.blocking_limit)
				.min();
			let grace = calls
				.iter()
				.filter(|call| call.phase == Phase::Interrupting)
				.filter_map(|call| call.grace_until)
				.min();
			let wake = match (deadline, grace) {
				(Some(a), Some(b)) => Some(a.min(b)),
				(a, b) => a.or(b),
			};
			let live = calls
				.iter()
				.enumerate()
				.filter(|(_, call)| call.is_live())
				.map(|(index, _)| index)
				.collect::<Vec<_>>();
			let signal = {
				let receivers: Vec<
					Pin<Box<dyn Future<Output = (usize, Option<DispatchEvent>)> + Send + '_>>,
				> = live
					.iter()
					.filter(|index| !calls[**index].closed)
					.map(|index| {
						let receiver = calls[*index].events.clone();
						let index = *index;
						Box::pin(async move { (index, receiver.recv_async().await.ok()) })
							as Pin<
								Box<
									dyn Future<Output = (usize, Option<DispatchEvent>)> + Send + '_,
								>,
							>
					})
					.collect();
				let interrupts: Vec<Pin<Box<dyn Future<Output = usize> + Send + '_>>> = live
					.iter()
					.filter(|index| calls[**index].phase == Phase::Running)
					.map(|index| {
						let token = calls[*index].interrupt.clone();
						let index = *index;
						Box::pin(async move {
							token.cancelled().await;
							index
						}) as Pin<Box<dyn Future<Output = usize> + Send + '_>>
					})
					.collect();
				tokio::select! {
					biased;
					() = control_expired(control) => Signal::RunExpired,
					message = control_recv(control) => Signal::Mailbox(message),
					index = select_first(interrupts) => Signal::Interrupt(index),
					() = sleep_until(wake) => Signal::Wake,
					(index, event) = select_first(receivers) => Signal::Event(index, event),
				}
			};
			match signal {
				Signal::RunExpired => {
					if let Some(control) = control {
						control.cancel_turn();
					}
				},
				Signal::Mailbox(message) => {
					let Some(control) = control else { continue };
					match control.handle(session, message)? {
						Received::Steering => self.skip_pending(session, &mut calls).await?,
						Received::Rewound(work) => {
							control.cancel_turn();
							for call in &calls {
								call.interrupt.cancel();
							}
							self.jobs.apply_lifecycle(session, &work).await;
						},
						Received::None | Received::Cancelled => {},
					}
				},
				Signal::Interrupt(index) => {
					let call = &mut calls[index];
					call.phase = Phase::Interrupting;
					call.grace_until = Some(Instant::now() + policy.interrupt_grace);
					if let Unit::Native { feed } = &call.unit {
						let _ = feed.interrupt(Interrupt {
							class:  Str::new_static(Interrupt::ESCAPE),
							reason: Str::new_static("tool execution cancelled"),
						});
					}
				},
				Signal::Wake => {
					let now = Instant::now();
					for index in 0..calls.len() {
						let call = &mut calls[index];
						if call.phase == Phase::Interrupting
							&& call.grace_until.is_some_and(|until| until <= now)
						{
							if let Some(task) = call.task.take() {
								task.abort();
								let _ = task.await;
							}
							let mut output = std::mem::take(call.output(&policy));
							let report = self.committer.commit_abort(
								session,
								call,
								Abort::EffectsUnknown {
									reason: Str::new_static(
										"tool execution cancelled; the call did not settle within the \
										 interrupt grace and was terminated",
									),
								},
								&mut output,
							)?;
							call.phase = Phase::Settled;
							call.report = Some(report);
						} else if call.phase == Phase::Running
							&& call
								.started
								.is_some_and(|started| started + policy.blocking_limit <= now)
						{
							self.detach(session, call)?;
						}
					}
				},
				Signal::Event(index, Some(event)) => {
					let call = &mut calls[index];
					if let Some(report) = self.committer.apply_event(session, call, event)? {
						call.phase = Phase::Settled;
						call.report = Some(report);
						if let Some(task) = call.task.take() {
							let _ = task.await;
						}
						self.on_settled(session, &mut calls, control).await?;
					}
				},
				Signal::Event(index, None) => calls[index].closed = true,
			}
			let _ = now;
		}
		Ok(calls
			.into_iter()
			.map(|call| call.report.expect("settled calls carry a report"))
			.collect())
	}

	/// Starts every pending call the exclusivity rule admits.
	async fn admit(
		&self,
		session: &mut Session,
		calls: &mut [PreparedCall],
		control: Option<&CallControl>,
	) -> Result<(), DispatchError> {
		loop {
			let Some(index) = (0..calls.len()).find(|index| {
				let call = &calls[*index];
				if call.phase != Phase::Pending {
					return false;
				}
				let earlier_live = calls[..*index].iter().any(PreparedCall::is_live);
				let earlier_exclusive_live = calls[..*index]
					.iter()
					.any(|earlier| earlier.is_live() && earlier.is_exclusive());
				let later_live = calls[index + 1..].iter().any(PreparedCall::is_live);
				if call.is_exclusive() {
					!earlier_live && !later_live
				} else {
					!earlier_exclusive_live
				}
			}) else {
				return Ok(());
			};
			let call = &mut calls[index];
			if call.interrupt.is_cancelled() {
				// A stop already requested never starts new work.
				let mut output = std::mem::take(call.output(&self.committer.policy));
				let report = self.committer.commit_abort(
					session,
					call,
					Abort::Interrupted { reason: Str::new_static("tool execution cancelled") },
					&mut output,
				)?;
				call.phase = Phase::Settled;
				call.report = Some(report);
				continue;
			}
			let args = call.args.clone().expect("drive requires committed calls");
			call.started = Some(Instant::now());
			match &mut call.unit {
				Unit::Native { feed } => {
					feed
						.args_committed(Str::new(args.get()))
						.map_err(|_| DispatchError::InputClosed)?;
					call.phase = Phase::Running;
				},
				Unit::External { executor, route, event_tx } => {
					let executor = Arc::clone(executor);
					let request = ExternalDispatchRequest {
						identity: call.identity.clone(),
						call_id: call.call_id.clone(),
						args,
						route: route.clone(),
						blocking_limit: self.committer.policy.blocking_limit,
						cancellation: call.interrupt.clone(),
					};
					let event_tx = event_tx.take().expect("external unit starts once");
					call.task = Some(tokio::spawn(async move {
						let mut stream = executor.invoke(request);
						while let Some(event) = stream.next().await {
							if event_tx.send(DispatchEvent::External(event)).is_err() {
								break;
							}
						}
						Ok(())
					}));
					call.phase = Phase::Running;
				},
				Unit::Session(tool) => {
					let tool = Arc::clone(tool);
					call.phase = Phase::Running;
					let report = self.run_session_tool(session, call, &tool, args, control).await?;
					call.phase = Phase::Settled;
					call.report = Some(report);
				},
				Unit::Detached { .. } => unreachable!("detached units are never scheduler inputs"),
			}
		}
	}

	/// Runs a session-owned tool inline under the ADR 0011 ladder: a stop
	/// request is followed by the interrupt grace, after which the future is
	/// dropped and the call journaled as effects-unknown.
	async fn run_session_tool(
		&self,
		session: &mut Session,
		call: &mut PreparedCall,
		tool: &Arc<dyn SessionTool>,
		args: Box<RawValue>,
		control: Option<&CallControl>,
	) -> Result<DispatchReport, DispatchError> {
		self.jobs.rebuild(session);
		let handle = session.call_handle(call.call)?;
		let interrupt = call.interrupt.clone();
		let grace = self.committer.policy.interrupt_grace;
		let outcome = {
			let future = tool.call(
				SessionToolCx {
					session,
					call: handle,
					jobs: &self.jobs,
					cancel: BackgroundToolCancellation::from_token(interrupt.clone()),
					authority: self.authority.as_deref(),
					control,
				},
				args,
			);
			tokio::pin!(future);
			tokio::select! {
				biased;
				outcome = &mut future => Some(outcome),
				() = interrupt.cancelled() => {
					tokio::select! {
						biased;
						outcome = &mut future => Some(outcome),
						() = tokio::time::sleep(grace) => None,
					}
				},
			}
		};
		let mut output = std::mem::take(call.output(&self.committer.policy));
		let outcome = match outcome {
			Some(outcome) => outcome?,
			None => {
				return self.committer.commit_abort(
					session,
					call,
					Abort::EffectsUnknown {
						reason: Str::new_static(
							"tool execution cancelled; the session tool did not settle within the \
							 interrupt grace and was terminated",
						),
					},
					&mut output,
				);
			},
		};
		let is_error = matches!(
			outcome,
			CallOutcome::Faulted(_) | CallOutcome::ArgsRejected(_) | CallOutcome::Aborted { .. }
		);
		let parts = match &outcome {
			CallOutcome::Ok(payload) | CallOutcome::Faulted(payload) => {
				vec![Part::Json { json: bytes::Bytes::copy_from_slice(payload.get().as_bytes()) }]
			},
			CallOutcome::ArgsRejected(_) | CallOutcome::Aborted { .. } => Vec::new(),
		};
		let outcome = serde_json::value::to_raw_value(&outcome)?;
		self
			.committer
			.finish_external(session, call, outcome, parts, is_error, &mut output)
	}

	/// After a settlement, applies the steering-stop rule: when steering is
	/// waiting for the safe point, every call that has not started is skipped
	/// so the user's redirection is served before further side effects.
	async fn on_settled(
		&self,
		session: &mut Session,
		calls: &mut [PreparedCall],
		control: Option<&CallControl>,
	) -> Result<(), DispatchError> {
		if control.is_some() && steering::steering_pending(session) {
			self.skip_pending(session, calls).await?;
		}
		Ok(())
	}

	async fn skip_pending(
		&self,
		session: &mut Session,
		calls: &mut [PreparedCall],
	) -> Result<(), DispatchError> {
		for call in calls.iter_mut() {
			if call.phase != Phase::Pending {
				continue;
			}
			if let Some(task) = call.task.take() {
				task.abort();
			}
			let mut output = std::mem::take(call.output(&self.committer.policy));
			let report = self.committer.commit_abort(
				session,
				call,
				Abort::Skipped {
					reason: Str::new_static(
						"pending steering message. Do not count this skipped result as completed \
						 work or verification. After the queued message is handled on the next \
						 step, retry the skipped tool if it is still needed",
					),
				},
				&mut output,
			)?;
			call.phase = Phase::Settled;
			call.report = Some(report);
		}
		Ok(())
	}

	/// Moves a call that outlived the blocking limit into the job primitive:
	/// the call settles for the model with a job reference, and its live
	/// execution unit (event receiver, task, kill boundary) is retained by the
	/// [`JobBoard`] so `hub jobs/wait/logs/cancel` address it and its eventual
	/// terminal is journaled onto the same element.
	fn detach(&self, session: &mut Session, call: &mut PreparedCall) -> Result<(), DispatchError> {
		let job = timeout_job(&call.identity);
		let outcome = detached_outcome(&job)?;
		let prompt = vec![Part::Text { text: sf!("detached job {}", job.id) }];
		let mut output = std::mem::take(call.output(&self.committer.policy));
		// The stream keeps flowing into the element after detachment; the
		// terminal below closes nothing that is still open.
		let output_sid = output.sid.take();
		self
			.committer
			.commit_terminal(session, call, outcome, prompt, false, &mut output)?;
		output.sid = output_sid;
		let detached = crate::jobs::DetachedCall {
			committer: self.committer.clone(),
			identity:  call.identity.clone(),
			call_id:   call.call_id.clone(),
			call:      call.call,
			options:   call.options,
			events:    call.events.clone(),
			task:      call.task.take(),
			feed:      match &call.unit {
				Unit::Native { feed } => Some(feed.clone()),
				Unit::External { .. } | Unit::Session(_) | Unit::Detached { .. } => None,
			},
			output,
			closed:    call.closed,
		};
		self
			.jobs
			.adopt_tool_job(session, &job.id, call.interrupt.clone(), detached);
		call.phase = Phase::Settled;
		call.report = Some(DispatchReport {
			is_error:      false,
			spilled:       None,
			lines_clamped: 0,
			detached:      Some(job),
		});
		Ok(())
	}
}

impl Default for OutputStream {
	fn default() -> Self {
		Self::new(None)
	}
}

/// One wake reason of the batch loop.
enum Signal {
	RunExpired,
	Mailbox(Up),
	Interrupt(usize),
	Wake,
	Event(usize, Option<DispatchEvent>),
}

async fn control_expired(control: Option<&CallControl>) {
	match control {
		Some(control) => control.run_expired().await,
		None => std::future::pending().await,
	}
}

async fn control_recv(control: Option<&CallControl>) -> Up {
	match control {
		Some(control) => control.recv().await,
		None => std::future::pending().await,
	}
}

async fn sleep_until(at: Option<Instant>) {
	match at {
		Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
		None => std::future::pending().await,
	}
}

async fn select_first<T>(futures: Vec<Pin<Box<dyn Future<Output = T> + Send + '_>>>) -> T {
	if futures.is_empty() {
		return std::future::pending().await;
	}
	select_all(futures).await.0
}

impl crate::jobs::DetachedCall {
	pub(crate) fn poll(
		&mut self,
		session: &mut Session,
	) -> Result<Option<DispatchReport>, DispatchError> {
		let (_, empty_events) = flume::unbounded();
		let stub = PreparedCall {
			identity: self.identity.clone(),
			call_id: self.call_id.clone(),
			call: self.call,
			cancellation: ToolCancellation::Background(
				BackgroundToolCancellation::from_token(CancellationToken::new()),
			),
			interrupt: CancellationToken::new(),
			unit: Unit::Detached { feed: self.feed.clone() },
			events: empty_events,
			task: None,
			args: None,
			options: self.options,
			output: None,
			phase: Phase::Running,
			started: None,
			grace_until: None,
			closed: false,
			report: None,
		};
		while let Ok(event) = self.events.try_recv() {
			if let Some(report) =
				self.committer.apply_event_with(session, &stub, event, &mut self.output)?
			{
				self.task.take();
				return Ok(Some(report));
			}
		}
		self.closed |= self.events.is_disconnected();
		if self.closed || self.task.as_ref().is_some_and(JoinHandle::is_finished) {
			self.task.take();
			let report =
				self
					.committer
					.commit_abort(session, &stub, Abort::MissingOutcome, &mut self.output)?;
			return Ok(Some(report));
		}
		Ok(None)
	}
}

impl Committer {
	/// Journals one execution-unit event; returns the report when it was the
	/// terminal.
	pub(crate) fn apply_event(
		&self,
		session: &mut Session,
		call: &mut PreparedCall,
		event: DispatchEvent,
	) -> Result<Option<DispatchReport>, DispatchError> {
		let mut output = std::mem::take(call.output(&self.policy));
		let result = self.apply_event_with(session, call, event, &mut output);
		call.output = Some(output);
		result
	}

	fn apply_event_with(
		&self,
		session: &mut Session,
		call: &PreparedCall,
		event: DispatchEvent,
		output: &mut OutputStream,
	) -> Result<Option<DispatchReport>, DispatchError> {
		match event {
			DispatchEvent::Native(Ok(ErasedEv::Update(update))) => {
				let update = RawValue::from_string(String::from_utf8(update.to_vec()).map_err(
					|source| {
						serde_json::Error::io(std::io::Error::new(
							std::io::ErrorKind::InvalidData,
							source,
						))
					},
				)?)?;
				self.commit_update(session, call, update, output)?;
				Ok(None)
			},
			DispatchEvent::Native(Ok(ErasedEv::Done(outcome))) => {
				self.commit_finalized_args(session, call, output)?;
				Ok(Some(self.finish(session, call, outcome, output)?))
			},
			DispatchEvent::Native(Err(error)) => Err(error.into()),
			DispatchEvent::External(ExternalDispatchEvent::Update(update)) => {
				self.commit_update(session, call, update, output)?;
				Ok(None)
			},
			DispatchEvent::External(ExternalDispatchEvent::Done { outcome, parts, is_error }) => {
				Ok(Some(self.finish_external(session, call, outcome, parts, is_error, output)?))
			},
			DispatchEvent::External(ExternalDispatchEvent::Aborted(abort)) => {
				Ok(Some(self.commit_abort(session, call, abort, output)?))
			},
		}
	}

	fn commit_finalized_args(
		&self,
		session: &mut Session,
		call: &PreparedCall,
		output: &mut OutputStream,
	) -> Result<(), DispatchError> {
		let feed = match &call.unit {
			Unit::Native { feed } => feed,
			Unit::Detached { feed: Some(feed) } => feed,
			Unit::External { .. } | Unit::Session(_) | Unit::Detached { feed: None } => return Ok(()),
		};
		let Some(finalized) = feed.take_finalized_args() else {
			return Ok(());
		};
		if finalized.repairs().is_empty() {
			return Ok(());
		}
		let update = serde_json::value::to_raw_value(&serde_json::json!({
			"kernel": "arguments_finalized",
			"repairs": finalized.repairs(),
		}))?;
		self.commit_update(session, call, update, output)
	}

	fn finish(
		&self,
		session: &mut Session,
		call: &PreparedCall,
		outcome: ErasedOutcome,
		output: &mut OutputStream,
	) -> Result<DispatchReport, DispatchError> {
		match outcome {
			ErasedOutcome::Detached(job) => {
				let raw = detached_outcome(&job)?;
				self.commit_terminal(
					session,
					call,
					raw,
					vec![Part::Text { text: sf!("detached job {}", job.id) }],
					false,
					output,
				)?;
				Ok(DispatchReport {
					is_error:      false,
					spilled:       None,
					lines_clamped: 0,
					detached:      Some(job),
				})
			},
			ErasedOutcome::Done { verdict, useless } => {
				let caps = PromptCaps::for_tool(
					CapsBase {
						maximum_parts:      u16::MAX,
						maximum_text_bytes: u32::MAX,
						media:              true,
						model_class:        ModelClass::Standard,
					},
					&call.identity.rev,
				);
				let projected =
					self
						.registry
						.project_verdict(&call.identity, &verdict, useless, &caps)?;
				let raw =
					RawValue::from_string(String::from_utf8(verdict.to_vec()).map_err(|source| {
						serde_json::Error::io(std::io::Error::new(
							std::io::ErrorKind::InvalidData,
							source,
						))
					})?)?;
				self.finish_external(
					session,
					call,
					raw,
					projected.parts.to_vec(),
					projected.is_error,
					output,
				)
			},
		}
	}

	fn finish_external(
		&self,
		session: &mut Session,
		call: &PreparedCall,
		outcome: Box<RawValue>,
		parts: Vec<Part>,
		is_error: bool,
		output: &mut OutputStream,
	) -> Result<DispatchReport, DispatchError> {
		let bounded = bound_parts(&parts, call.options, &self.policy)?;
		let stream_spill = output.close(session)?;
		let spilled = bounded.spilled.or(stream_spill);
		if let Some(artifact) = &spilled {
			let diag = serde_json::value::to_raw_value(&serde_json::json!({
				"diag": {
					"kind": "truncated",
					"severity": "info",
					"artifact": artifact_address(artifact),
					"lines_clamped": bounded.lines_clamped,
				}
			}))?;
			self.commit_update(session, call, diag, output)?;
		}
		self.commit_terminal(session, call, outcome, bounded.parts, is_error, output)?;
		Ok(DispatchReport { is_error, spilled, lines_clamped: bounded.lines_clamped, detached: None })
	}

	pub(crate) fn commit_abort(
		&self,
		session: &mut Session,
		call: &PreparedCall,
		abort: Abort,
		output: &mut OutputStream,
	) -> Result<DispatchReport, DispatchError> {
		// An abort is harness-owned: its projection never depends on the tool
		// or its route, so external units settle exactly like native ones.
		let parts = vec![Part::Text { text: abort.render() }];
		let outcome = serde_json::value::to_raw_value(&CallOutcome::<
			serde_json::Value,
			serde_json::Value,
		>::aborted(abort))?;
		self.finish_external(session, call, outcome, parts, true, output)
	}

	fn commit_update(
		&self,
		session: &mut Session,
		call: &PreparedCall,
		update: Box<RawValue>,
		output: &mut OutputStream,
	) -> Result<(), DispatchError> {
		let value: serde_json::Value = serde_json::from_str(update.get())?;
		if let Some(hooks) = &self.lifecycle_hooks {
			let target = serde_json::json!({
				"kind": "core",
				"name": call.identity.name,
				"rev": format!("{}@{}", call.identity.rev.family, call.identity.rev.n),
				"args": {},
			});
			hooks.notify(
				omp_proto::toolhost::v1::HookEventId::HookEventToolUpdate,
				serde_json::json!({
					"call_id": call.call_id,
					"target": target,
					"update": value,
					"coalesced": 1,
				}),
			)?;
		}
		let update = match OutputStream::frame(&value) {
			Some((sequence, bytes)) => {
				output.push(session, call.call, &self.policy.spill, value, sequence, &bytes)?
			},
			None => update,
		};
		session.call_update(call.call, update)?;
		self
			.events
			.publish(KernelEvent::ToolUpdate { call_id: call.call_id.clone() });
		Ok(())
	}

	fn commit_terminal(
		&self,
		session: &mut Session,
		call: &PreparedCall,
		outcome: Box<RawValue>,
		parts: Vec<Part>,
		is_error: bool,
		output: &mut OutputStream,
	) -> Result<(), DispatchError> {
		output.close(session)?;
		// The raw outcome is published on the element and travels in every
		// snapshot and patch: bound it under the same policy as the prompt
		// projection so an actor never receives an unbounded payload (ADR
		// 0009). `notrunc` keeps it inline by explicit caller choice.
		let outcome = if call.options.notrunc || outcome.get().len() <= self.policy.max_output_bytes {
			outcome
		} else {
			let artifact = self.policy.spill.put(outcome.get().as_bytes())?;
			serde_json::value::to_raw_value(&CallOutcomeDetails::Spilled {
				blob: ToolBlobRef {
					hash: Str::new(artifact.to_hex()),
					media_type: Str::new_static("application/json"),
					byte_len: u64::try_from(outcome.get().len()).unwrap_or(u64::MAX),
				},
				byte_len: u64::try_from(outcome.get().len()).unwrap_or(u64::MAX),
			})?
		};
		let parts = serde_json::value::to_raw_value(&parts)?;
		if is_error {
			session.fail_projected(call.call, outcome, parts)?;
		} else {
			session.settle_projected(call.call, outcome, parts)?;
		}
		self
			.events
			.publish(KernelEvent::ToolSettled { call_id: call.call_id.clone(), is_error });
		Ok(())
	}
}

fn detached_outcome(job: &JobRef) -> Result<Box<RawValue>, serde_json::Error> {
	serde_json::value::to_raw_value(&serde_json::json!({
		"kind": "detached",
		"id": job.id,
		"job": job,
	}))
}

fn timeout_job(identity: &ToolIdentity) -> JobRef {
	let now = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis() as u64;
	let id = Str::new(omp_core::Ulid::generate().to_string());
	JobRef {
		id:       id.clone(),
		owner:    JobOwner::AgentLoop { agent_id: Str::new_static("kernel") },
		metadata: Arc::new(JobMetadata::running(JobKind::Shell, identity.name.clone(), now)),
		artifact: ExpectedArtifact {
			description: sf!("detached {} output", identity.name),
			media_type:  Some(Str::new_static("application/json")),
			lifetime:    ArtifactLifetime::Session,
		},
	}
}

struct BoundedParts {
	parts:         Vec<Part>,
	spilled:       Option<BlobRef>,
	lines_clamped: u64,
}

fn bound_parts(
	parts: &[Part],
	options: DispatchOptions,
	policy: &DispatchPolicy,
) -> Result<BoundedParts, DispatchError> {
	if options.notrunc {
		return Ok(BoundedParts {
			parts:         parts.to_vec(),
			spilled:       None,
			lines_clamped: 0,
		});
	}
	let mut output = Vec::with_capacity(parts.len());
	let mut full = String::new();
	let mut shown_bytes = 0;
	let mut lines_clamped = 0;
	let mut changed = false;
	for part in parts {
		match part {
			Part::Text { text } => {
				full.push_str(text.as_str());
				let (line_bounded, count) = clamp_lines(text.as_str(), policy.max_line_bytes);
				lines_clamped += count;
				changed |= count != 0;
				let available = policy.max_output_bytes.saturating_sub(shown_bytes);
				let visible = utf8_prefix(&line_bounded, available);
				shown_bytes += visible.len();
				changed |= visible.len() != line_bounded.len();
				output.push(Part::Text { text: Str::new(visible) });
			},
			Part::Json { json } => {
				let text = std::str::from_utf8(json)
					.map_err(|source| DispatchError::ProjectionUtf8 { source })?;
				full.push_str(text);
				let (line_bounded, count) = clamp_lines(text, policy.max_line_bytes);
				lines_clamped += count;
				changed |= count != 0;
				let available = policy.max_output_bytes.saturating_sub(shown_bytes);
				let visible = utf8_prefix(&line_bounded, available);
				shown_bytes += visible.len();
				changed |= visible.len() != line_bounded.len();
				output.push(Part::Text { text: Str::new(visible) });
			},
			Part::Blob { blob, alt } => {
				if let Some(alt) = alt {
					full.push_str(alt.as_str());
				}
				output.push(Part::Blob { blob: blob.clone(), alt: alt.clone() });
			},
		}
	}
	let spilled = changed
		.then(|| policy.spill.put(full.as_bytes()))
		.transpose()?;
	if let Some(artifact) = spilled {
		output.push(Part::Text { text: artifact_address(&artifact) });
	}
	Ok(BoundedParts { parts: output, spilled, lines_clamped })
}

fn clamp_lines(text: &str, maximum: usize) -> (String, u64) {
	let mut output = String::with_capacity(text.len().min(maximum.saturating_mul(2)));
	let mut line_bytes: usize = 0;
	let mut eliding = false;
	let mut clamped = 0;
	for character in text.chars() {
		if character == '\n' {
			output.push(character);
			line_bytes = 0;
			eliding = false;
			continue;
		}
		if eliding {
			continue;
		}
		if line_bytes.saturating_add(character.len_utf8()) > maximum {
			output.push('…');
			eliding = true;
			clamped += 1;
			continue;
		}
		output.push(character);
		line_bytes += character.len_utf8();
	}
	(output, clamped)
}

fn utf8_prefix(text: &str, maximum: usize) -> &str {
	if text.len() <= maximum {
		return text;
	}
	let mut end = maximum;
	while !text.is_char_boundary(end) {
		end -= 1;
	}
	&text[..end]
}

#[cfg(test)]
mod checkpoint_tests {
	use omp_core::Str;
	use omp_session::{ComponentRegistry, Session};

	use super::journal_env_event;

	#[test]
	fn checkpoint_control_rewinds_to_the_journaled_token_target() {
		let temp = tempfile::tempdir().expect("tempdir");
		let mut session =
			Session::create(temp.path().join("checkpoint.oms"), ComponentRegistry::standard())
				.expect("session");
		session.begin_turn().expect("turn");
		session.user("before", Vec::new()).expect("before");
		journal_env_event(
			&mut session,
			crate::EnvEvent::CheckpointControl {
				operation: Str::new_static("checkpoint"),
				payload: Str::new_static(r#"{"token":"checkpoint-1"}"#),
			},
		)
		.expect("checkpoint");
		session.user("after", Vec::new()).expect("after");
		let work = journal_env_event(
			&mut session,
			crate::EnvEvent::CheckpointControl {
				operation: Str::new_static("schedule_rewind"),
				payload: Str::new_static(
					r#"{"token":"checkpoint-1","report":"done","receipt":"rewind-1"}"#,
				),
			},
		)
		.expect("schedule")
		.expect("rewind work");
		assert!(work.terminate.is_empty());
		let texts = session
			.dom()
			.select("body turn user")
			.expect("selector")
			.filter_map(|handle| session.dom().get(handle)?.content.as_deref())
			.collect::<Vec<_>>();
		assert_eq!(texts, ["before"]);
		assert_eq!(session.dom().count("rewind-checkpoint").expect("selector"), 0);
	}
}

pub(crate) fn artifact_address(blob: &BlobRef) -> Str {
	sf!("artifact://sha256/{}", blob.to_hex())
}

/// Converts a semantic revision into the journal's numeric revision field.
#[must_use]
pub fn journal_revision(rev: &Rev) -> u32 {
	u32::from(rev.n)
}
