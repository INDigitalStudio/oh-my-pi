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
				JobSettlement {
					status: Str::new_static("cancelled"),
					output: None,
					error: None,
				}
			})
		}
	}));
	assert_eq!(starts.load(Ordering::SeqCst), 1);

	let work = session.rewind(before).expect("rewind before spawn");
	assert_eq!(work.terminate, vec![handle]);
	board.apply_lifecycle(&session, &work).await;
	assert!(board.list().is_empty());

	let work = session.rewind(inserted).expect("rewind onto spawned branch");
	assert_eq!(work.spawn.len(), 1);
	board.apply_lifecycle(&session, &work).await;
	assert_eq!(starts.load(Ordering::SeqCst), 2);
	assert_eq!(board.list().len(), 1);
}
