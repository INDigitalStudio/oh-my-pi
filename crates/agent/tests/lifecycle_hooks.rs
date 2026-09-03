//! Joined production lifecycle-hook integration over one real kernel tool turn.

use std::{sync::Arc, time::Duration};

use async_stream::stream;
use bytes::Bytes;
use futures::Stream;
use omp_agent::{
	DispatchPolicy, GateDecision, HookGate, HookPatch, HookPhase, Kernel, OnFailure, RunControl,
	SourceRef, StaticPrompt, TurnInput, When,
};
use omp_core::sf;
use omp_journal::{blob::BlobStore, kind};
use omp_proto::toolhost::v1::HookEventId;
use omp_tool::{
	Claims, Constraint, Effects, Ev, IncomingParams, Part, Precedence, Presentation, PromptCaps,
	Registry, Rev, Tool, ToolSpec, ToolTerminal,
};
use parking_lot::Mutex;
use serde_json::Value;

mod support;

use support::{ScriptedInference, fresh_session, journal_entries, text_script, tool_script};

struct CaptureTool {
	spec: ToolSpec,
	seen: Arc<Mutex<Option<Value>>>,
}

impl Tool for CaptureTool {
	type Fault = Value;
	type Params = Value;
	type Payload = Value;
	type Update = Value;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut params: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<Self::Update, Self::Payload, Self::Fault>> + Send + 'c {
		stream! {
			let args = params.whole::<Value>().await.expect("transformed args decode");
			*self.seen.lock() = Some(args.clone());
			yield Ev::Update(serde_json::json!({"stage": "running"}));
			yield Ev::Done(ToolTerminal::Done { result: Ok(args), useless: false });
		}
	}

	fn prompt(&self, view: Result<&Value, &Value>, _: &PromptCaps) -> Vec<Part> {
		vec![Part::Json {
			json: Bytes::from(serde_json::to_vec(view.unwrap_or_else(|fault| fault)).expect("JSON")),
		}]
	}
}

fn capture_registry(seen: Arc<Mutex<Option<Value>>>) -> Arc<Registry> {
	let mut registry = Registry::new();
	registry
		.register(
			CaptureTool {
				spec: ToolSpec {
					name: sf!("capture"),
					rev: Rev { family: sf!("test"), n: 1 },
					description: sf!("capture transformed arguments"),
					schema: Bytes::from_static(
						br#"{"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}"#,
					),
					constraint: Constraint::None,
					effects: Effects::empty(),
					projection_code: [7; 32],
				},
				seen,
			},
			Presentation::Slot,
			Claims {
				precedence: Precedence::CORE,
				claimant: sf!("omp/core"),
				replaces: None,
			},
		)
		.expect("capture tool registers");
	Arc::new(registry)
}

fn subscription(
	id: u32,
	event: HookEventId,
	phase: HookPhase,
) -> omp_agent::hooks::Subscription {
	omp_agent::hooks::Subscription {
		host: sf!("test"),
		source: SourceRef { layer: 0, publisher: sf!("test"), extension_id: sf!("lifecycle") },
		id,
		event,
		phase,
		order: 0,
		on_failure: OnFailure::Deny,
		when: When::default(),
	}
}

#[tokio::test]
async fn lifecycle_tool_call_transform_reaches_executor_and_observations_are_complete() {
	let (gate, receiver) = HookGate::channel();
	let gate = Arc::new(gate);
	let observed = Arc::new(Mutex::new(Vec::new()));
	let events = [
		HookEventId::HookEventAgentStart,
		HookEventId::HookEventTurnStart,
		HookEventId::HookEventMessageStart,
		HookEventId::HookEventMessageUpdate,
		HookEventId::HookEventMessageEnd,
		HookEventId::HookEventCallOpen,
		HookEventId::HookEventToolExecutionStart,
		HookEventId::HookEventToolUpdate,
		HookEventId::HookEventToolExecutionEnd,
		HookEventId::HookEventToolResult,
		HookEventId::HookEventTurnEnd,
		HookEventId::HookEventAgentEnd,
	];
	let mut subscriptions = vec![subscription(1, HookEventId::HookEventToolCall, HookPhase::Transform)];
	subscriptions.extend(events.into_iter().enumerate().map(|(index, event)| {
		subscription(u32::try_from(index).expect("small") + 2, event, HookPhase::Observe)
	}));
	gate.subscribe("test", subscriptions).expect("subscriptions");
	let responder = {
		let gate = Arc::clone(&gate);
		let observed = Arc::clone(&observed);
		tokio::spawn(async move {
			while let Ok(dispatch) = receiver.recv_async().await {
				let payload: Value = serde_json::from_slice(&dispatch.payload).expect("hook payload");
				observed.lock().push((dispatch.event, payload.clone()));
				if dispatch.event == HookEventId::HookEventToolCall {
					let mut transformed = payload;
					transformed["args"] = serde_json::json!({"value": 2});
					transformed["target"]["args"] = serde_json::json!({"value": 2});
					gate.answer(
						dispatch.dispatch_id,
						vec![(1, GateDecision::Modify(HookPatch {
							target: None,
							args: Some(Bytes::from(serde_json::to_vec(&transformed).expect("transform"))),
						}))],
					)
					.expect("hook answer");
				}
			}
		})
	};
	let seen = Arc::new(Mutex::new(None));
	let temp = tempfile::tempdir().expect("tempdir");
	let (inference, _) = ScriptedInference::new([
		tool_script("capture-1", "capture", serde_json::json!({"value": 1})),
		text_script("done"),
	]);
	let mut kernel = Kernel::new(
		inference,
		capture_registry(Arc::clone(&seen)),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(sf!("system")),
	)
	.with_hook_gate(Arc::clone(&gate));
	let mut session = fresh_session(&temp.path().join("hooks.oms"));
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: sf!("capture"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("turn");
	assert_eq!(*seen.lock(), Some(serde_json::json!({"value": 2})));
	tokio::time::sleep(Duration::from_millis(20)).await;
	for event in events {
		assert!(observed.lock().iter().any(|(actual, _)| *actual == event), "missing {event:?}");
	}
	let tool_call = observed
		.lock()
		.iter()
		.find(|(event, _)| *event == HookEventId::HookEventToolCall)
		.map(|(_, payload)| payload.clone())
		.expect("tool-call payload");
	for key in ["call_id", "invocation_id", "target", "kind", "args", "raw_args", "repaired", "turn_id", "session_id", "cwd", "origin", "batch", "deadline", "bash"] {
		assert!(tool_call.get(key).is_some(), "missing strict ToolCall key {key}");
	}
	drop(kernel);
	responder.abort();
}

#[tokio::test]
async fn lifecycle_tool_call_denial_skips_executor_and_journals_abort() {
	let (gate, receiver) = HookGate::channel();
	let gate = Arc::new(gate);
	gate
		.subscribe("test", [subscription(1, HookEventId::HookEventToolCall, HookPhase::Precheck)])
		.expect("subscription");
	let responder = {
		let gate = Arc::clone(&gate);
		tokio::spawn(async move {
			let dispatch = receiver.recv_async().await.expect("tool call gate");
			gate.answer(dispatch.dispatch_id, vec![(1, GateDecision::Deny(sf!("blocked")))])
				.expect("deny");
		})
	};
	let seen = Arc::new(Mutex::new(None));
	let temp = tempfile::tempdir().expect("tempdir");
	let (inference, _) = ScriptedInference::new([
		tool_script("capture-1", "capture", serde_json::json!({"value": 1})),
		text_script("done"),
	]);
	let mut kernel = Kernel::new(
		inference,
		capture_registry(Arc::clone(&seen)),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(sf!("system")),
	)
	.with_hook_gate(gate);
	let path = temp.path().join("deny.oms");
	let mut session = fresh_session(&path);
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: sf!("capture"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("turn");
	assert!(seen.lock().is_none(), "denied tool never executes");
	let entries = journal_entries(&path);
	let call = entries.iter().find(|entry| entry.kind.name.as_str() == kind::TOOL_CALL).expect("call");
	assert!(entries.iter().any(|entry| entry.kind.name.as_str() == kind::TOOL_RESULT && entry.by == Some(call.id)));
	responder.await.expect("responder");
}
