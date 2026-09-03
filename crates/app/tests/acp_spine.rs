//! ACP controller proofs over a scripted journal-first kernel.

use std::{
	collections::VecDeque,
	future::{Future, ready},
	sync::{Arc, Mutex},
	time::SystemTime,
};

use futures::StreamExt as _;
use omp_agent::{DispatchPolicy, Inference, Kernel, StaticPrompt};
use omp_core::Str;
use omp_driver::{
	headless::kernel::{KernelOptions, SessionHome},
	sessions::SessionRegistry,
};
use omp_inference::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
	ProviderId, RequestId, ResponseMeta, RouteId, Usage,
};
use omp_journal::blob::BlobStore;
use omp_session::{ComponentRegistry, Session};
use omp_tool::Registry;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

#[derive(Clone, Copy)]
enum Script {
	Pending,
	Text(&'static str),
}

struct ScriptedInference {
	scripts: Mutex<VecDeque<Script>>,
}

impl Inference for ScriptedInference {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send {
		let script = self
			.scripts
			.get_mut()
			.expect("script mutex poisoned")
			.pop_front()
			.expect("one scripted turn");
		ready(Ok(match script {
			Script::Pending => {
				let events = futures::stream::once(ready(Ok(started())))
					.chain(futures::stream::pending::<Result<ChatEvent, omp_inference::Error>>());
				ChatStream::ordinary(Box::pin(events))
			},
			Script::Text(text) => {
				let events = vec![
					started(),
					ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
					ChatEvent::TextDelta { index: 0, text: Str::new(text) },
					ChatEvent::Completed(Completion {
						reason:  FinishReason::Stop,
						blocks:  1,
						usage:   Usage::default(),
						receipt: ExecutionReceipt::default().into(),
					}),
				]
				.into_iter()
				.map(Ok);
				ChatStream::ordinary(Box::pin(futures::stream::iter(events)))
			},
		}))
	}
}

fn started() -> ChatEvent {
	ChatEvent::Started(ResponseMeta {
		request_id:          RequestId::from("acp-script"),
		provider:            ProviderId::from("scripted"),
		route:               RouteId::from("scripted/test"),
		model:               None,
		provider_request_id: None,
		created_at:          SystemTime::UNIX_EPOCH,
	})
}

fn harness(
	directory: &tempfile::TempDir,
	scripts: impl IntoIterator<Item = Script>,
) -> (Kernel<ScriptedInference>, Session, SessionHome) {
	let sessions_dir = directory.path().join("sessions");
	std::fs::create_dir_all(&sessions_dir).expect("sessions directory");
	let spill = BlobStore::open(directory.path().join("blobs")).expect("blob store");
	let kernel = Kernel::new(
		ScriptedInference { scripts: Mutex::new(scripts.into_iter().collect()) },
		Arc::new(Registry::new()),
		DispatchPolicy::new(spill),
		StaticPrompt(Str::new_static("system")),
	);
	let live = Arc::new(SessionRegistry::new());
	let options = KernelOptions {
		sessions_dir: Some(sessions_dir.clone()),
		sessions: Some(live),
		..KernelOptions::default()
	};
	let home = SessionHome::new(
		directory.path(),
		directory.path(),
		&options,
		Str::new_static("scripted/test"),
		kernel.mailbox(),
	)
	.expect("session home");
	let session = Session::create(
		sessions_dir.join("startup.oms"),
		ComponentRegistry::standard(),
	)
	.expect("startup session");
	(kernel, session, home)
}

async fn exchange(
	kernel: Kernel<ScriptedInference>,
	session: Session,
	home: SessionHome,
	requests: &'static [u8],
) -> Vec<Value> {
	let (client_io, server_io) = tokio::io::duplex(64 * 1024);
	let (server_read, server_write) = tokio::io::split(server_io);
	let server = omp_app::acp_mode::serve_acp(kernel, session, home, server_read, server_write);
	let client = async move {
		let (client_read, mut client_write) = tokio::io::split(client_io);
		client_write.write_all(requests).await.expect("requests");
		client_write.shutdown().await.expect("request shutdown");
		let mut lines = BufReader::new(client_read).lines();
		let mut frames = Vec::new();
		while let Some(line) = lines.next_line().await.expect("response") {
			frames.push(serde_json::from_str(&line).expect("JSON response"));
		}
		frames
	};
	let (server, frames) = tokio::time::timeout(
		std::time::Duration::from_secs(2),
		async { tokio::join!(server, client) },
	)
	.await
	.expect("ACP exchange must not deadlock");
	server.expect("ACP server");
	frames
}

fn response<'a>(frames: &'a [Value], id: &str) -> &'a Value {
	frames
		.iter()
		.find(|frame| frame.get("id").and_then(Value::as_str) == Some(id))
		.unwrap_or_else(|| panic!("missing response {id}: {frames:#?}"))
}

#[tokio::test]
async fn control_requests_remain_live_during_a_prompt() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (kernel, session, home) = harness(&directory, [Script::Pending]);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"prompt":"wait"}}
{"jsonrpc":"2.0","id":"approval","method":"session/approve","params":{"promptId":"approval-1","approved":true}}
{"jsonrpc":"2.0","id":"cancel","method":"session/cancel","params":{}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	assert_eq!(response(&frames, "approval")["result"], serde_json::json!({}));
	assert_eq!(response(&frames, "cancel")["result"], serde_json::json!({}));
	assert_eq!(response(&frames, "prompt")["result"]["stopReason"], "cancelled");
	let approval = frames
		.iter()
		.position(|frame| frame.get("id").and_then(Value::as_str) == Some("approval"))
		.expect("approval response");
	let prompt = frames
		.iter()
		.position(|frame| frame.get("id").and_then(Value::as_str) == Some("prompt"))
		.expect("prompt response");
	assert!(approval < prompt, "approval must dispatch before the active prompt completes");
}

#[tokio::test]
async fn new_load_and_resume_switch_the_authoritative_durable_session() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let target_path = directory.path().join("sessions").join("target.oms");
	let resumed_path = directory.path().join("sessions").join("resumed.oms");
	std::fs::create_dir_all(target_path.parent().expect("session parent"))
		.expect("sessions directory");
	drop(
		Session::create(&target_path, ComponentRegistry::standard())
			.expect("durable load target"),
	);
	drop(
		Session::create(&resumed_path, ComponentRegistry::standard())
			.expect("durable resume target"),
	);
	let (kernel, session, home) = harness(&directory, [Script::Text("written")]);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"new","method":"session/new","params":{}}
{"jsonrpc":"2.0","id":"load","method":"session/load","params":{"sessionId":"target"}}
{"jsonrpc":"2.0","id":"resume","method":"session/resume","params":{"sessionId":"resumed"}}
{"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"prompt":"durable marker"}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	let new_id = response(&frames, "new")["result"]["sessionId"]
		.as_str()
		.expect("new session id");
	assert_ne!(new_id, "startup");
	assert_ne!(new_id, "target");
	assert!(directory.path().join("sessions").join(format!("{new_id}.oms")).exists());
	assert_eq!(response(&frames, "load")["result"]["sessionId"], "target");
	assert_eq!(response(&frames, "resume")["result"]["sessionId"], "resumed");

	let target =
		Session::open(&target_path, ComponentRegistry::standard()).expect("load target reopens");
	let target_snapshot = target.dom().snapshot();
	assert!(
		!String::from_utf8_lossy(target_snapshot.as_bytes()).contains("durable marker"),
		"resuming another session must switch authority away from the prior load target"
	);
	let resumed = Session::open(&resumed_path, ComponentRegistry::standard())
		.expect("resume target reopens");
	let resumed_snapshot = resumed.dom().snapshot();
	assert!(
		String::from_utf8_lossy(resumed_snapshot.as_bytes()).contains("durable marker"),
		"prompt must be journaled in the requested resumed session"
	);
}
