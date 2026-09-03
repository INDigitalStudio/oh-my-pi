//! One upward mailbox for steering and cancellation.
//!
//! Steering is durable from the moment the kernel accepts it: every
//! [`Up::Steer`] lands in `<queues><steering>` through `patch@1` at the
//! mailbox drain that receives it, so a crash or session switch while
//! inference or a tool runs never loses accepted input. The safe point then
//! moves the queued items into the current turn in one atomic patch (pi
//! `getSteeringMessages` dequeue).

use omp_core::Str;
use omp_dom::{Handle, KnownTag, NodeSpec, Op, PropId, PropKey, Tag, Txn, Value};
use omp_session::{Session, SessionError};

pub(crate) const EMPTY_OUTPUT_RETRY_CAP: u8 = 3;
const EMPTY_OUTPUT_CAP_NOTICE: &str =
	"Assistant returned no final output after retry cap; try switching models";

/// A live actor subscription handed back through [`Up::Subscribe`].
pub type Subscription = (omp_dom::Snapshot, flume::Receiver<omp_dom::Event>);

/// Control sent to a running kernel turn.
#[derive(Clone, Debug)]
pub enum Up {
	/// Adds a user steering aside at the next safe point.
	Steer(Str),
	/// Queues a peer/hub message for explicit inbox consumption; unlike
	/// steering, it does not redirect the active turn.
	Peer(Str),
	/// Hands back every steering aside not yet consumed at a safe point (pi
	/// `app.message.dequeue`): the host restores them to its composer.
	Unqueue(flume::Sender<Vec<Str>>),
	/// Interrupts the current inference/tool turn while preserving mutations.
	Interrupt,
	/// Cancels the whole session and every execution scope.
	Cancel,
	/// Delivers an environment observation or host-authority request.
	Env(crate::EnvEvent),
	/// Resolves a journal-backed approval prompt.
	Approve {
		/// Stable prompt identity.
		id:       Str,
		/// Idempotent first decision.
		decision: crate::ApprovalDecision,
	},
	/// Requests a live `(Snapshot, Receiver<Event>)` pair over the session the
	/// kernel is driving (an actor rendering a child session never reads its
	/// `.oms`, ADR 0005). Dropped silently when the requester is gone.
	Subscribe(flume::Sender<Subscription>),
}

/// The `<queues><steering>` element.
pub(crate) fn steering_queue(session: &Session) -> Result<Handle, SessionError> {
	session
		.dom()
		.children(session.dom().queues())
		.iter()
		.copied()
		.find(|handle| {
			session
				.dom()
				.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Steering))
		})
		.ok_or(SessionError::NoActiveTurn)
}

/// Journals one accepted steering message into `<queues><steering>`.
pub(crate) fn queue_steering(session: &mut Session, text: Str) -> Result<(), SessionError> {
	let steering = steering_queue(session)?;
	let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
	session.patch(Txn {
		cause,
		label: Some(Str::new_static("steering.queue")),
		ops: vec![Op::Ins {
			parent: steering,
			after:  session.dom().children(steering).last().copied(),
			node:   NodeSpec::new(KnownTag::User)
				.with_prop(PropId::Status, Value::Str(Str::new_static("queued")))
				.with_content(text),
		}],
	})?;
	Ok(())
}

/// Journals one peer message without making it turn steering.
pub(crate) fn queue_peer(session: &mut Session, text: Str) -> Result<(), SessionError> {
	let steering = steering_queue(session)?;
	let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
	session.patch(Txn {
		cause,
		label: Some(Str::new_static("hub.message")),
		ops: vec![Op::Ins {
			parent: steering,
			after: session.dom().children(steering).last().copied(),
			node: NodeSpec::new(KnownTag::User)
				.with_prop(PropId::Status, Value::Str(Str::new_static("queued")))
				.with_prop(PropKey::Custom(Str::new_static("hub")), Value::Bool(true))
				.with_content(text),
		}],
	})?;
	Ok(())
}

fn is_peer(session: &Session, handle: Handle) -> bool {
	session
		.dom()
		.get(handle)
		.and_then(|node| node.prop(&PropKey::Custom(Str::new_static("hub"))))
		.is_some_and(|value| matches!(value, Value::Bool(true)))
}

/// Texts currently queued in `<queues><steering>`, oldest first.
pub(crate) fn queued_steering(session: &Session) -> Vec<Str> {
	let Ok(steering) = steering_queue(session) else {
		return Vec::new();
	};
	session
		.dom()
		.children(steering)
		.iter()
		.filter(|handle| !is_peer(session, **handle))
		.filter_map(|handle| session.dom().get(*handle)?.content.clone())
		.collect()
}

/// Whether any accepted steering awaits a safe point.
pub(crate) fn steering_pending(session: &Session) -> bool {
	steering_queue(session).is_ok_and(|steering| {
		session.dom().children(steering).iter().any(|handle| !is_peer(session, *handle))
	})
}

/// Moves every queued steering message into `turn` in one atomic patch: the
/// queue items are removed and re-inserted as `<user>` turn children (user
/// authorship is preserved, pi queues steering as `role: "user"`). Returns the
/// consumed texts in queue order.
pub(crate) fn consume_steering(
	session: &mut Session,
	turn: Handle,
) -> Result<Vec<Str>, SessionError> {
	let steering = steering_queue(session)?;
	let queued = session
		.dom()
		.children(steering)
		.iter()
		.filter(|handle| !is_peer(session, **handle))
		.filter_map(|handle| Some((*handle, session.dom().get(*handle)?.content.clone()?)))
		.collect::<Vec<_>>();
	if queued.is_empty() {
		return Ok(Vec::new());
	}
	let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
	let tail = session.dom().children(turn).last().copied();
	let mut ops = Vec::with_capacity(queued.len() * 2);
	ops.extend(queued.iter().map(|(handle, _)| Op::Rm(*handle)));
	// Every insert anchors on the turn's current tail; inserting in reverse
	// queue order therefore lands the items in queue order after it.
	ops.extend(queued.iter().rev().map(|(_, text)| Op::Ins {
		parent: turn,
		after:  tail,
		node:   NodeSpec::new(KnownTag::User)
			.with_prop(PropKey::Custom(Str::new_static("steering")), Value::Bool(true))
			.with_content(text.clone()),
	}));
	session.patch(Txn { cause, label: Some(Str::new_static("steering.safe-point")), ops })?;
	Ok(queued.into_iter().map(|(_, text)| text).collect())
}

/// Removes every queued steering message (host `Unqueue`: the composer takes
/// them back) and returns their texts.
pub(crate) fn unqueue_steering(session: &mut Session) -> Result<Vec<Str>, SessionError> {
	let steering = steering_queue(session)?;
	let queued = session
		.dom()
		.children(steering)
		.iter()
		.filter(|handle| !is_peer(session, **handle))
		.filter_map(|handle| Some((*handle, session.dom().get(*handle)?.content.clone()?)))
		.collect::<Vec<_>>();
	if queued.is_empty() {
		return Ok(Vec::new());
	}
	let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
	let (handles, texts): (Vec<_>, Vec<_>) = queued.into_iter().unzip();
	session.patch(Txn {
		cause,
		label: Some(Str::new_static("steering.unqueue")),
		ops: handles.into_iter().map(Op::Rm).collect(),
	})?;
	Ok(texts)
}

pub(crate) fn append_notice(
	session: &mut Session,
	turn: Handle,
	text: Str,
) -> Result<(), SessionError> {
	append_notice_with_kind(session, turn, text, Str::new_static("info"))
}

/// Appends a `<notice kind=error>` describing why the turn failed.
pub(crate) fn append_error_notice(
	session: &mut Session,
	turn: Handle,
	text: Str,
) -> Result<(), SessionError> {
	append_notice_with_kind(session, turn, text, Str::new_static("error"))
}

/// Appends a producer-named notice (`<notice kind=hook name=…>`).
pub(crate) fn append_named_notice(
	session: &mut Session,
	turn: Handle,
	kind: Str,
	name: Option<Str>,
	body: Str,
) -> Result<(), SessionError> {
	let mut node = NodeSpec::new(KnownTag::Notice)
		.with_prop(PropId::Kind, Value::Str(kind))
		.with_content(body);
	if let Some(name) = name {
		node = node.with_prop(PropKey::Custom(Str::new_static("name")), Value::Str(name));
	}
	append_turn_child(session, turn, node, Str::new_static("kernel.notice"))
}

/// Appends the `<notice kind=warn>` that ends an interrupted turn.
pub(crate) fn append_interrupt_notice(
	session: &mut Session,
	turn: Handle,
) -> Result<(), SessionError> {
	append_notice_with_kind(
		session,
		turn,
		Str::new_static("Turn interrupted"),
		Str::new_static("warn"),
	)
}

pub(crate) fn append_empty_output_retry(
	session: &mut Session,
	turn: Handle,
	attempt: u8,
) -> Result<(), SessionError> {
	append_turn_child(
		session,
		turn,
		NodeSpec::new(KnownTag::Developer).with_content(Str::new(format!(
			"<system-injection>\nStopped without actionable output; task incomplete. Continue with a \
			 user-visible final answer or the next required tool call.\nAttempt \
			 #{attempt}/{EMPTY_OUTPUT_RETRY_CAP}\n</system-injection>"
		))),
		Str::new_static("kernel.empty-output-retry"),
	)
}

pub(crate) fn append_empty_output_cap_notice(
	session: &mut Session,
	turn: Handle,
) -> Result<(), SessionError> {
	append_notice_with_kind(
		session,
		turn,
		Str::new_static(EMPTY_OUTPUT_CAP_NOTICE),
		Str::new_static("error"),
	)
}

fn append_notice_with_kind(
	session: &mut Session,
	turn: Handle,
	text: Str,
	kind: Str,
) -> Result<(), SessionError> {
	append_turn_child(
		session,
		turn,
		NodeSpec::new(KnownTag::Notice)
			.with_prop(PropId::Kind, Value::Str(kind))
			.with_content(text),
		Str::new_static("kernel.notice"),
	)
}

fn append_turn_child(
	session: &mut Session,
	turn: Handle,
	node: NodeSpec,
	label: Str,
) -> Result<(), SessionError> {
	session.patch(Txn {
		cause: session.head().ok_or(SessionError::NoActiveTurn)?,
		label: Some(label),
		ops:   vec![Op::Ins {
			parent: turn,
			after: session.dom().children(turn).last().copied(),
			node,
		}],
	})?;
	Ok(())
}
