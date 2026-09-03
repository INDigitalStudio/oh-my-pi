//! Journal-level abandoned-branch pruning.

use std::{
	fs, io,
	path::{Path, PathBuf},
};

use miette::IntoDiagnostic as _;
use omp_journal::{Journal, abandoned, gc::prune_abandoned};
use serde_json::json;

use crate::cli::GcArgs;

/// Scans native `.oms` journals and optionally prunes abandoned branches.
pub fn run(args: GcArgs) -> miette::Result<()> {
	let data_dir = omp_core::dirs::data_dir(args.data_dir).into_diagnostic()?;
	let roots = args
		.sessions_dir
		.map_or_else(|| project_session_roots(&data_dir), |directory| Ok(vec![directory]))
		.into_diagnostic()?;
	let mut paths = Vec::new();
	for sessions in roots {
		collect_journals(&sessions, &mut paths).into_diagnostic()?;
	}
	paths.sort();

	let mut journals = 0usize;
	let mut entries_pruned = 0usize;
	let mut bytes_reclaimed = 0u64;
	for path in paths {
		let entries = Journal::scan(&path).into_diagnostic()?;
		let abandoned_count = abandoned(&entries).count();
		if abandoned_count == 0 {
			continue;
		}
		journals += 1;
		entries_pruned += abandoned_count;
		if args.apply {
			bytes_reclaimed += prune_abandoned(&path).into_diagnostic()?.bytes_reclaimed();
		}
	}

	if args.json {
		println!(
			"{}",
			json!({
				"applied": args.apply,
				"journals": journals,
				"entries_pruned": entries_pruned,
				"bytes_reclaimed": bytes_reclaimed,
			})
		);
	} else if args.apply {
		println!(
			"pruned {entries_pruned} abandoned entries from {journals} journals; reclaimed \
			 {bytes_reclaimed} bytes"
		);
	} else {
		println!(
			"dry run: {entries_pruned} abandoned entries in {journals} journals; pass --apply to \
			 prune"
		);
	}
	Ok(())
}

fn project_session_roots(data_dir: &Path) -> io::Result<Vec<PathBuf>> {
	let projects = data_dir.join("projects");
	let entries = match fs::read_dir(&projects) {
		Ok(entries) => entries,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
		Err(error) => return Err(error),
	};
	let mut roots = Vec::new();
	for entry in entries {
		let sessions = entry?.path().join("sessions");
		if sessions.is_dir() {
			roots.push(sessions);
		}
	}
	roots.sort();
	Ok(roots)
}

fn collect_journals(directory: &Path, output: &mut Vec<PathBuf>) -> io::Result<()> {
	let entries = match fs::read_dir(directory) {
		Ok(entries) => entries,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
		Err(error) => return Err(error),
	};
	for entry in entries {
		let path = entry?.path();
		if path.is_dir() {
			collect_journals(&path, output)?;
		} else if path.extension().and_then(|value| value.to_str())
			== Some(omp_journal::FILE_EXTENSION)
		{
			output.push(path);
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use tempfile::tempdir;

	use super::*;

	#[test]
	fn defaults_to_every_project_session_root() {
		let scratch = tempdir().expect("scratch");
		let first = scratch.path().join("projects/first/sessions");
		let second = scratch.path().join("projects/second/sessions");
		fs::create_dir_all(&first).expect("first project");
		fs::create_dir_all(&second).expect("second project");
		fs::create_dir_all(scratch.path().join("projects/third/cache")).expect("unrelated state");

		let roots = project_session_roots(scratch.path()).expect("project roots");
		assert_eq!(roots, vec![first.clone(), second.clone()]);

		let first_journal = first.join("a.oms");
		let second_journal = second.join("b.oms");
		fs::write(&first_journal, "").expect("first journal");
		fs::write(&second_journal, "").expect("second journal");
		let mut journals = Vec::new();
		for root in roots {
			collect_journals(&root, &mut journals).expect("collect");
		}
		journals.sort();
		assert_eq!(journals, vec![first_journal, second_journal]);
	}
}
