//! Joined proof for live-session hub routing.

use std::{future::ready, sync::Arc, time::SystemTime};

use futures::stream;
use omp_agent::{Inference, Kernel, RunControl, StaticPrompt, TurnInput};
use omp_catalog::{ProviderId, RouteId};
use omp_core::Str;
use omp_dom::{KnownTag, NodeSpec, Op, PropId, PropKey, Tag, Txn, Value};
use omp_driver::{
	sessions::{KernelHandle, SessionId, SessionRegistry},
	subagent::hub::SessionHub,
};
use omp_inference::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
	RequestId, ResponseMeta, Usage,
};
use omp_session::{ComponentRegistry, Session};
use omp_tool::Registry;
use parking_lot::RwLock;

struct OneTurn;

impl Inference for OneTurn {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send {
		let events = [
			ChatEvent::Started(ResponseMeta {
				request_id:          RequestId::from("hub-test"),
				provider:            ProviderId::from("test"),
				route:               RouteId::from("test/route"),
				model:               None,
				provider_request_id: None,
				created_at:          SystemTime::UNIX_EPOCH,
			}),
			ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
			ChatEvent::TextDelta { index: 0, text: Str::new_static("done") },
			ChatEvent::Completed(Completion {
				reason:  FinishReason::Stop,
				blocks:  1,
				usage:   Usage::default(),
				receipt: ExecutionReceipt::default().into(),
			}),
		]
		.into_iter()
		.map(Ok);
		ready(Ok(ChatStream::ordinary(Box::pin(stream::iter(events)))))
	}
}

#[tokio::test]
async fn send_lands_in_child_steering_and_inbox_reads_it() {
	let temp = tempfile::tempdir().expect("temporary directory");
	let mut child = Session::create(temp.path().join("child.oms"), ComponentRegistry::standard())
		.expect("child session");
	let spill =
		omp_journal::blob::BlobStore::open(temp.path().join("artifacts")).expect("artifact store");
	let mut kernel = Kernel::new(
		OneTurn,
		Arc::new(Registry::new()),
		omp_agent::DispatchPolicy::new(spill),
		StaticPrompt(Str::new_static("test")),
	);
	let sessions = SessionRegistry::new();
	sessions.register(Str::new_static("Child"), KernelHandle {
		id:       SessionId::new("child"),
		name:     Str::new_static("Child"),
		up:       kernel.mailbox(),
		snapshot: Arc::new(RwLock::new(child.dom().snapshot())),
	});

	SessionHub::send(&sessions, "child", Str::new_static("please adjust")).expect("hub send");
	kernel
		.run_turn(
			&mut child,
			TurnInput { text: Str::new_static("work"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("child turn");

	let response = SessionHub::inbox(&mut child, true).expect("hub inbox");
	assert!(response.text.as_str().contains("please adjust"));
	let drained = SessionHub::inbox(&mut child, false).expect("hub drain");
	assert!(drained.text.as_str().contains("please adjust"));
	assert!(
		SessionHub::inbox(&mut child, true)
			.expect("empty inbox")
			.useless
	);
}

/// `hub inbox` is the peer bus: it drains `hub=true` queue items only. User
/// steering shares `<queues><steering>` but belongs to the kernel safe point.
#[test]
fn hub_inbox_leaves_user_steering_queued() {
	let temp = tempfile::tempdir().expect("temporary directory");
	let mut session =
		Session::create(temp.path().join("s.oms"), ComponentRegistry::standard()).expect("session");
	let steering = session
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
		.expect("steering queue");
	let queued =
		|node: NodeSpec| node.with_prop(PropId::Status, Value::Str(Str::new_static("queued")));
	let cause = session.head().expect("journal head");
	session
		.patch(Txn {
			cause,
			label: None,
			ops: vec![
				Op::Ins {
					parent: steering,
					after:  None,
					node:   queued(NodeSpec::new(KnownTag::User))
						.with_prop(PropKey::Custom(Str::new_static("hub")), Value::Bool(true))
						.with_content(Str::new_static("peer says hi")),
				},
				Op::Ins {
					parent: steering,
					after:  None,
					node:   queued(NodeSpec::new(KnownTag::User))
						.with_content(Str::new_static("user redirect")),
				},
			],
		})
		.expect("queue both items");

	let peeked = SessionHub::inbox(&mut session, true).expect("peek");
	assert!(peeked.text.as_str().contains("peer says hi"));
	assert!(!peeked.text.as_str().contains("user redirect"));

	let drained = SessionHub::inbox(&mut session, false).expect("drain");
	assert!(drained.text.as_str().contains("peer says hi"));
	assert!(!drained.text.as_str().contains("user redirect"));

	let remaining = session
		.dom()
		.children(steering)
		.iter()
		.filter_map(|handle| session.dom().get(*handle)?.content.clone())
		.collect::<Vec<_>>();
	assert_eq!(remaining, vec![Str::new_static("user redirect")]);
	assert!(
		SessionHub::inbox(&mut session, true)
			.expect("peer inbox is empty")
			.useless
	);
}
