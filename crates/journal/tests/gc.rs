//! Journal branch-pruning integration coverage.

use std::{env, process::Command};

use omp_core::Str;
use omp_journal::{EntryDraft, Journal, Kind, gc::prune_abandoned, kind::KindName, live_chain};

fn draft(
	kind: KindName,
	by: Option<omp_journal::EntryId>,
	prior: Option<omp_journal::EntryId>,
) -> EntryDraft {
	EntryDraft { kind: Kind::known(kind), by, prior, label: None, data: Str::new_static("{}") }
}

#[test]
fn prune_of_branched_journal_preserves_live_snapshot_and_shrinks_bytes() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let path = directory.path().join("branched.oms");
	let mut journal = Journal::create(&path).expect("journal creates");
	let genesis = journal
		.append(draft(KindName::Journal, None, None))
		.expect("genesis appends");
	let branch_point = journal
		.append(draft(KindName::TurnStart, Some(genesis.id), None))
		.expect("branch point appends");
	let abandoned = journal
		.append(draft(KindName::MsgUser, Some(branch_point.id), None))
		.expect("abandoned message appends");
	journal
		.append(draft(KindName::TurnStart, Some(branch_point.id), Some(branch_point.id)))
		.expect("replacement turn appends");
	journal
		.append(draft(KindName::MsgUser, Some(branch_point.id), None))
		.expect("replacement message appends");
	drop(journal);

	let (_, before_entries) = Journal::open(&path).expect("journal opens before prune");
	let before_snapshot: Vec<_> = live_chain(&before_entries).cloned().collect();
	assert!(!before_snapshot.iter().any(|entry| entry.id == abandoned.id));
	let before_bytes = std::fs::metadata(&path).expect("metadata").len();

	let report = prune_abandoned(&path).expect("journal prunes");
	let (_, after_entries) = Journal::open(&path).expect("journal opens after prune");
	let after_snapshot: Vec<_> = live_chain(&after_entries).cloned().collect();

	assert_eq!(after_snapshot, before_snapshot);
	assert_eq!(report.entries_pruned(), 1);
	assert_eq!(report.entries_after, after_entries.len());
	assert!(report.bytes_after < before_bytes);
	assert_eq!(std::fs::metadata(path).expect("metadata").len(), report.bytes_after);
}

/// Subprocess half of the cross-process GC exclusion test.
#[test]
#[ignore = "subprocess helper"]
fn gc_lock_subprocess_helper() {
	let path = env::var_os("OMP_JOURNAL_GC_LOCK_TEST_PATH").expect("journal test path");
	let error = prune_abandoned(path).expect_err("parent process owns the writer lock");
	assert!(matches!(
		error,
		omp_journal::gc::GcError::Journal(omp_journal::JournalError::Locked { .. })
	));
}

/// GC contends on the same cross-process lock as writers.
#[test]
fn prune_in_another_process_refuses_a_live_writer() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let path = directory.path().join("process-live.oms");
	let mut journal = Journal::create(&path).expect("journal creates");
	let genesis = journal
		.append(draft(KindName::Journal, None, None))
		.expect("genesis appends");
	journal
		.append(draft(KindName::TurnStart, Some(genesis.id), None))
		.expect("turn appends");
	let status = Command::new(env::current_exe().expect("journal test executable"))
		.args(["--ignored", "--exact", "gc_lock_subprocess_helper"])
		.env("OMP_JOURNAL_GC_LOCK_TEST_PATH", &path)
		.status()
		.expect("run GC contender");
	assert!(status.success(), "subprocess GC must observe the held writer lock");
}

/// GC coordinates with the writer lock: a session that has the journal open
/// is never left appending to an unlinked inode.
#[test]
fn prune_refuses_a_journal_with_a_live_writer() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let path = directory.path().join("live.oms");
	let mut journal = Journal::create(&path).expect("journal creates");
	let genesis = journal
		.append(draft(KindName::Journal, None, None))
		.expect("genesis appends");
	let branch_point = journal
		.append(draft(KindName::TurnStart, Some(genesis.id), None))
		.expect("branch point appends");
	journal
		.append(draft(KindName::TurnStart, Some(branch_point.id), Some(genesis.id)))
		.expect("rewind appends");
	let error = prune_abandoned(&path).expect_err("a live writer blocks pruning");
	assert!(matches!(
		error,
		omp_journal::gc::GcError::Journal(omp_journal::JournalError::Locked { .. })
	));
	// The writer keeps appending to the same, un-replaced file.
	journal
		.append(draft(KindName::MsgUser, Some(genesis.id), None))
		.expect("append after refused prune");
	drop(journal);
	let report = prune_abandoned(&path).expect("prune once the writer is gone");
	assert_eq!(report.entries_before, 4);
	assert_eq!(report.entries_after, 3);
}
