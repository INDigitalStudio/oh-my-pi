//! Rewind-to-runtime lifecycle integration for the shared job primitive.

use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};

use omp_agent::{JobBoard, JobSettlement};
use omp_core::Str;
use omp_session::{
	ComponentRegistry, Session,
	components::jobs::{self, JobSpec},
};
use tempfile::tempdir;

#[tokio::test]
async fn jobs_rewind_removing_a_subagent_terminates_it() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("parent.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let before = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), before, JobSpec {
		id:      Str::new_static("child-1"),
		kind:    Str::new_static("subagent"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   Some(Str::new_static("task")),
	})
	.expect("jobs root");
	session.patch(txn).expect("insert subagent");

	let handle = session
		.dom()
		.select("jobs subagent[id=child-1]")
		.expect("valid selector")
		.into_iter()
		.next()
		.expect("subagent element");
	let inserted = session.head().expect("insert head");
	let starts = Arc::new(AtomicUsize::new(0));
	let board = JobBoard::new();
	assert!(board.attach_restartable(session.dom(), handle, {
		let starts = Arc::clone(&starts);
		move |cancel| {
			starts.fetch_add(1, Ordering::SeqCst);
			tokio::spawn(async move {
				cancel.cancelled().await;
				JobSettlement { status: Str::new_static("cancelled"), output: None, error: None }
			})
		}
	}));
	assert_eq!(starts.load(Ordering::SeqCst), 1);

	let work = session.rewind(before).expect("rewind before spawn");
	assert_eq!(work.terminate, vec![handle]);
	board.apply_lifecycle(&session, &work).await;
	assert!(board.list().is_empty());

	let work = session
		.rewind(inserted)
		.expect("rewind onto spawned branch");
	assert_eq!(work.spawn.len(), 1);
	board.apply_lifecycle(&session, &work).await;
	assert_eq!(starts.load(Ordering::SeqCst), 2);
	assert_eq!(board.list().len(), 1);
}

/// A `<job kind=tool>` re-derived without its execution unit (a forward
/// rewind over a detached call, or a restart) can never settle on its own:
/// the board journals it `failed` at the next poll instead of leaving
/// `hub wait` blocked on a phantom `running` job.
#[tokio::test]
async fn jobs_tool_job_without_an_execution_unit_settles_failed_at_poll() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("parent.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let head = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), head, JobSpec {
		id:      Str::new_static("bash-timeout-1"),
		kind:    Str::new_static("tool"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   None,
	})
	.expect("jobs root");
	session.patch(txn).expect("insert detached tool job");

	let board = JobBoard::new();
	board.rebuild(&session);
	assert!(board.has_finished_units(), "an orphaned tool job wakes the settlement poll");
	let settled = board
		.wait(&mut session, Some(&[Str::new_static("bash-timeout-1")]))
		.await
		.expect("poll commits the orphan")
		.expect("the orphan settles rather than hanging the wait");
	assert_eq!(settled.status.as_str(), "failed");
	assert_eq!(settled.error.as_deref(), Some(omp_agent::ORPHANED_TOOL_JOB));
	assert!(!board.has_finished_units(), "the orphan is journaled exactly once");
}

/// ADR 0009: a settlement larger than the central inline bound never lands
/// on the `<subagent>` element verbatim; the full JSON goes to the session
/// CAS and the element carries the artifact address plus a bounded head of
/// the child's text, which `resolve_output` reads back whole.
#[tokio::test]
async fn jobs_oversized_settlement_is_spilled_to_the_cas_and_resolvable() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("parent.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let head = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), head, JobSpec {
		id:      Str::new_static("child-big"),
		kind:    Str::new_static("subagent"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   Some(Str::new_static("task")),
	})
	.expect("jobs root");
	session.patch(txn).expect("insert subagent");
	let handle = session
		.dom()
		.select("jobs subagent[id=child-big]")
		.expect("valid selector")
		.into_iter()
		.next()
		.expect("subagent element");

	let text = "x".repeat(4_096);
	let full = serde_json::json!({"id": "child-big", "text": text, "error": null});
	let board = JobBoard::new();
	board.set_output_bound(512);
	assert!(board.attach_task(
		session.dom(),
		handle,
		tokio_util::sync::CancellationToken::new(),
		tokio::spawn({
			let full = full.clone();
			async move {
				JobSettlement {
					status: Str::new_static("completed"),
					output: serde_json::value::to_raw_value(&full).ok(),
					error:  None,
				}
			}
		}),
	));
	let settled = board
		.wait(&mut session, Some(&[Str::new_static("child-big")]))
		.await
		.expect("poll")
		.expect("settles");
	assert_eq!(settled.status.as_str(), "completed");
	let inline = settled.output.as_deref().expect("inline output");
	assert!(inline.get().len() <= 512, "the element carries a bounded stand-in");
	let spilled: omp_agent::SpilledOutput =
		serde_json::from_str(inline.get()).expect("spilled shape");
	assert!(spilled.artifact.starts_with("artifact://sha256/"));
	assert_eq!(spilled.byte_len, serde_json::to_string(&full).expect("json").len() as u64);
	assert_eq!(spilled.text.as_deref(), Some("x".repeat(128).as_str()));
	let resolved = omp_agent::resolve_output(&session, inline)
		.expect("blob read")
		.expect("addressable");
	assert_eq!(serde_json::from_str::<serde_json::Value>(resolved.get()).expect("json"), full);
}
