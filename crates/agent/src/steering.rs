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
use omp_journal::data::Attachment;
use omp_session::{Session, SessionError};

use crate::SteeringMode;

pub(crate) const EMPTY_OUTPUT_RETRY_CAP: u8 = 3;
const EMPTY_OUTPUT_CAP_NOTICE: &str =
	"Assistant returned no final output after retry cap; try switching models";

/// A live actor subscription handed back through [`Up::Subscribe`].
pub type Subscription = (omp_dom::Snapshot, flume::Receiver<omp_dom::Event>);

/// Control sent to a running kernel turn.
#[derive(Clone, Debug)]
pub enum Up {
	/// Adds a user steering aside at the next safe point (pi
	/// `steer(text, images)`): `attachments` are already in the session's
	/// blob store and positional against `[Image #N]` in `text`.
	Steer {
		/// The aside.
		text:        Str,
		/// Media journaled beside it.
		attachments: Vec<Attachment>,
	},
	/// Queues a peer/hub message for explicit inbox consumption; unlike
	/// steering, it does not redirect the active turn.
	Peer(Str),
	/// Queues a follow-up prompt behind the active turn (pi `followUp`):
	/// journaled into `<queues><prompts>` at the drain that receives it; the
	/// controller pops it when the turn yields. Never steering: it does not
	/// re-run a safe point.
	Queue(Str),
	/// Hands back every steering aside not yet consumed at a safe point (pi
	/// `app.message.dequeue`): the host restores them to its composer.
	Unqueue(flume::Sender<Vec<Str>>),
	/// Interrupts the current inference/tool turn while preserving mutations.
	Interrupt,
	/// Cancels the whole session and every execution scope.
	Cancel,
	/// Delivers an environment observation or host-authority request.
	Env(crate::EnvEvent),
	/// A policy filed an approval prompt through the kernel-bound
	/// [`crate::ApprovalRoute`]: journaled as a pending `<prompt>` at the
	/// drain that receives it, answered by [`Up::Approve`].
	Approval(crate::ApprovalRequest),
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

/// Journals one accepted steering message into `<queues><steering>`; its
/// attachments ride the same `data` prop a `msg.user@1` fold writes, so the
/// safe point moves them into the turn and the projection types them.
pub(crate) fn queue_steering(
	session: &mut Session,
	text: Str,
	attachments: &[Attachment],
) -> Result<(), SessionError> {
	let steering = steering_queue(session)?;
	let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
	let mut node = NodeSpec::new(KnownTag::User)
		.with_prop(PropId::Status, Value::Str(Str::new_static("queued")))
		.with_content(text);
	if !attachments.is_empty() {
		node = node.with_prop(PropId::Data, Value::Json(serde_json::value::to_raw_value(attachments)?));
	}
	session.patch(Txn {
		cause,
		label: Some(Str::new_static("steering.queue")),
		ops: vec![Op::Ins {
			parent: steering,
			after: session.dom().children(steering).last().copied(),
			node,
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
			after:  session.dom().children(steering).last().copied(),
			node:   NodeSpec::new(KnownTag::User)
				.with_prop(PropId::Status, Value::Str(Str::new_static("queued")))
				.with_prop(PropKey::Custom(Str::new_static("hub")), Value::Bool(true))
				.with_content(text),
		}],
	})?;
	Ok(())
}

/// Journals one follow-up prompt as `<prompt kind=queued status=pending>`
/// under `<queues><prompts>` (the controller's `queue.push` shape).
pub(crate) fn queue_prompt(session: &mut Session, text: Str) -> Result<(), SessionError> {
	let prompts = omp_session::components::prompts::prompts_handle(session.dom())
		.ok_or(SessionError::NoActiveTurn)?;
	let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
	let id = Str::new(format!("queued-{}", omp_core::Ulid::generate()));
	session.patch(Txn {
		cause,
		label: Some(Str::new_static("queue.push")),
		ops: vec![Op::Ins {
			parent: prompts,
			after:  session.dom().children(prompts).last().copied(),
			node:   NodeSpec::new(KnownTag::Prompt)
				.with_prop(PropId::Kind, Value::Str(Str::new_static("queued")))
				.with_prop(PropId::Id, Value::Str(id))
				.with_prop(PropId::Status, Value::Str(Str::new_static("pending")))
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

/// Asides currently queued in `<queues><steering>` with their attachments,
/// oldest first.
pub(crate) fn queued_steering(session: &Session) -> Vec<(Str, Vec<Attachment>)> {
	let Ok(steering) = steering_queue(session) else {
		return Vec::new();
	};
	session
		.dom()
		.children(steering)
		.iter()
		.filter(|handle| !is_peer(session, **handle))
		.filter_map(|handle| {
			let node = session.dom().get(*handle)?;
			let attachments = match node.prop(&PropKey::from(PropId::Data)) {
				Some(Value::Json(raw)) => serde_json::from_str(raw.get()).unwrap_or_default(),
				_ => Vec::new(),
			};
			Some((node.content.clone()?, attachments))
		})
		.collect()
}

/// Whether any accepted steering awaits a safe point.
pub(crate) fn steering_pending(session: &Session) -> bool {
	steering_queue(session).is_ok_and(|steering| {
		session
			.dom()
			.children(steering)
			.iter()
			.any(|handle| !is_peer(session, *handle))
	})
}

/// Moves queued steering into `turn` in one atomic patch: the queue items are
/// removed and re-inserted as `<user steering=true>` turn children (user
/// authorship is preserved, pi queues steering as `role: "user"`). Under
/// [`SteeringMode::OneAtATime`] only the oldest item moves and the rest stay
/// queued (so [`steering_pending`] keeps the loop reaching further safe
/// points); [`SteeringMode::All`] moves every item (pi
/// `#dequeueSteeringMessages`). Returns the consumed texts in queue order.
pub(crate) fn consume_steering(
	session: &mut Session,
	turn: Handle,
	mode: SteeringMode,
) -> Result<Vec<Str>, SessionError> {
	let steering = steering_queue(session)?;
	let queued = session
		.dom()
		.children(steering)
		.iter()
		.filter(|handle| !is_peer(session, **handle))
		.filter_map(|handle| {
			let node = session.dom().get(*handle)?;
			let data = node.prop(&PropKey::from(PropId::Data)).cloned();
			Some((*handle, node.content.clone()?, data))
		})
		.take(match mode {
			SteeringMode::OneAtATime => 1,
			SteeringMode::All => usize::MAX,
		})
		.collect::<Vec<_>>();
	if queued.is_empty() {
		return Ok(Vec::new());
	}
	let cause = session.head().ok_or(SessionError::NoActiveTurn)?;
	let tail = session.dom().children(turn).last().copied();
	let mut ops = Vec::with_capacity(queued.len() * 2);
	ops.extend(queued.iter().map(|(handle, ..)| Op::Rm(*handle)));
	// Every insert anchors on the turn's current tail; inserting in reverse
	// queue order therefore lands the items in queue order after it. The
	// aside keeps its attachments (`data`) so the projection types them.
	ops.extend(queued.iter().rev().map(|(_, text, data)| {
		let mut node = NodeSpec::new(KnownTag::User)
			.with_prop(PropKey::Custom(Str::new_static("steering")), Value::Bool(true))
			.with_content(text.clone());
		if let Some(data) = data {
			node = node.with_prop(PropId::Data, data.clone());
		}
		Op::Ins { parent: turn, after: tail, node }
	}));
	session.patch(Txn { cause, label: Some(Str::new_static("steering.safe-point")), ops })?;
	Ok(queued.into_iter().map(|(_, text, _)| text).collect())
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

#[cfg(test)]
mod tests {
	use omp_session::ComponentRegistry;

	use super::*;

	fn session_with_turn(path: &std::path::Path) -> (Session, Handle) {
		let mut session =
			Session::create(path, ComponentRegistry::default()).expect("session creates");
		session.begin_turn().expect("turn starts");
		let turn = *session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn exists");
		(session, turn)
	}

	fn turn_steering(session: &Session, turn: Handle) -> Vec<Str> {
		session
			.dom()
			.children(turn)
			.iter()
			.filter_map(|handle| {
				let node = session.dom().get(*handle)?;
				(node.tag == Tag::Known(KnownTag::User)
					&& node.prop(&PropKey::Custom(Str::new_static("steering")))
						== Some(&Value::Bool(true)))
				.then(|| node.content.clone())
				.flatten()
			})
			.collect()
	}

	#[test]
	fn one_at_a_time_consumes_one_item_per_safe_point() {
		let directory = tempfile::tempdir().expect("temporary session directory");
		let (mut session, turn) = session_with_turn(&directory.path().join("steer.oms"));
		queue_steering(&mut session, Str::new_static("first"), &[]).expect("first queues");
		queue_steering(&mut session, Str::new_static("second"), &[]).expect("second queues");

		let consumed =
			consume_steering(&mut session, turn, SteeringMode::OneAtATime).expect("first safe point");
		assert_eq!(consumed, [Str::new_static("first")]);
		assert_eq!(turn_steering(&session, turn), [Str::new_static("first")]);
		assert_eq!(queued_steering(&session), [(Str::new_static("second"), Vec::new())]);
		assert!(steering_pending(&session), "the remaining item waits for another safe point");

		let consumed =
			consume_steering(&mut session, turn, SteeringMode::OneAtATime).expect("second safe point");
		assert_eq!(consumed, [Str::new_static("second")]);
		assert_eq!(turn_steering(&session, turn), [
			Str::new_static("first"),
			Str::new_static("second")
		]);
		assert!(queued_steering(&session).is_empty());
		assert!(!steering_pending(&session));
	}

	#[test]
	fn all_consumes_every_item_at_one_safe_point() {
		let directory = tempfile::tempdir().expect("temporary session directory");
		let (mut session, turn) = session_with_turn(&directory.path().join("steer-all.oms"));
		queue_steering(&mut session, Str::new_static("first"), &[]).expect("first queues");
		queue_steering(&mut session, Str::new_static("second"), &[]).expect("second queues");

		let consumed = consume_steering(&mut session, turn, SteeringMode::All).expect("safe point");
		assert_eq!(consumed, [Str::new_static("first"), Str::new_static("second")]);
		assert_eq!(turn_steering(&session, turn), [
			Str::new_static("first"),
			Str::new_static("second")
		]);
		assert!(!steering_pending(&session));
	}

	#[test]
	fn queue_journals_pending_prompt_under_queues_prompts() {
		let directory = tempfile::tempdir().expect("temporary session directory");
		let (mut session, _) = session_with_turn(&directory.path().join("queue.oms"));
		queue_prompt(&mut session, Str::new_static("follow up")).expect("prompt queues");

		assert!(!steering_pending(&session), "a queued prompt is not steering");
		let prompts = omp_session::components::prompts::prompts_handle(session.dom())
			.expect("prompt queue exists");
		let children = session.dom().children(prompts);
		assert_eq!(children.len(), 1);
		let node = session.dom().get(children[0]).expect("queued prompt node");
		assert_eq!(node.tag, Tag::Known(KnownTag::Prompt));
		assert_eq!(node.content.as_deref(), Some("follow up"));
		assert_eq!(
			node.prop(&PropKey::from(PropId::Kind)),
			Some(&Value::Str(Str::new_static("queued")))
		);
		assert_eq!(
			node.prop(&PropKey::from(PropId::Status)),
			Some(&Value::Str(Str::new_static("pending")))
		);
		assert!(
			node
				.prop(&PropKey::from(PropId::Id))
				.and_then(Value::as_str)
				.is_some_and(|id| id.starts_with("queued-"))
		);
	}

	#[test]
	fn one_at_a_time_skips_peer_messages() {
		let directory = tempfile::tempdir().expect("temporary session directory");
		let (mut session, turn) = session_with_turn(&directory.path().join("steer-peer.oms"));
		queue_peer(&mut session, Str::new_static("hub hello")).expect("peer queues");
		queue_steering(&mut session, Str::new_static("redirect"), &[]).expect("steer queues");

		let consumed =
			consume_steering(&mut session, turn, SteeringMode::OneAtATime).expect("safe point");
		assert_eq!(consumed, [Str::new_static("redirect")]);
		assert!(!steering_pending(&session));
		let steering = steering_queue(&session).expect("queue exists");
		assert_eq!(session.dom().children(steering).len(), 1, "the peer message stays queued");
	}
}
