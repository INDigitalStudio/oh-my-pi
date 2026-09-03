//! Workspace slash commands (pi `builtin-lifecycle.ts`): `/add-dir`,
//! `/remove-dir`, `/dirs`, `/move`, `/wt` (`/worktree`).
//!
//! pi keeps the additional workspace directories on the session manager;
//! here they are the `SESSION` convar [`SV_WORKSPACE_DIRS`] (ADR 0012), so
//! they journal into `<meta><con>`, survive `-c` resume, fall off on rewind,
//! and seed spawned children. `/move` and `/wt` relocate the journal and
//! the process working directory through the controller
//! ([`HostCommand::Move`](crate::HostCommand::Move)); the worktree itself
//! is created by the application ([`Services::create_worktree`]).
//!
//! [`Services::create_worktree`]: crate::overlays::services::Services::create_worktree

use std::path::{Path, PathBuf};

use omp_con::{ConError, Value};
use omp_core::{Str, sf};
use omp_tui::Icon;

use super::{CommandAction, PaletteEntry, rest};
use crate::{
	actions::{HostAction, post},
	overlays::{PanelCall, PanelCx, PanelEvent},
};

/// Palette icons for this module's commands.
pub const PALETTE: &[PaletteEntry] = &[
	PaletteEntry { name: "add-dir", icon: Icon::FolderPlus },
	PaletteEntry { name: "remove-dir", icon: Icon::FolderMinus },
	PaletteEntry { name: "dirs", icon: Icon::Folder },
	PaletteEntry { name: "move", icon: Icon::FolderMove },
	PaletteEntry { name: "wt", icon: Icon::Worktree },
	PaletteEntry { name: "worktree", icon: Icon::Worktree },
];

omp_con::var! {
	/// Additional workspace directories of this session (pi multi-root
	/// `/add-dir`), beside the working directory.
	pub static SV_WORKSPACE_DIRS = sv_workspace_dirs: Vec<Str> {
		default: Vec::new(),
		flags: session,
	};
}

const fn usage(message: &'static str) -> ConError {
	ConError::Usage(Str::new_static(message))
}

fn call(ctx: &omp_con::Ctx, call: PanelCall) -> omp_con::ConResult<()> {
	post(ctx, HostAction::Call(call))
}

fn notice(text: impl Into<Str>) -> PanelEvent {
	PanelEvent::Notice(text.into())
}

/// pi `resolveToCwd`: absolute paths stand, `~` expands, the rest joins the
/// working directory; the result is lexically normalized.
#[must_use]
pub fn resolve_to_cwd(input: &str, cwd: &Path) -> PathBuf {
	let input = input.trim();
	let input = input
		.strip_prefix('"')
		.and_then(|rest| rest.strip_suffix('"'))
		.unwrap_or(input);
	let raw = if let Some(rest) = input.strip_prefix("~/") {
		std::env::var_os("HOME")
			.map_or_else(|| PathBuf::from(input), |home| Path::new(&home).join(rest))
	} else if input == "~" {
		std::env::var_os("HOME").map_or_else(|| PathBuf::from(input), PathBuf::from)
	} else {
		PathBuf::from(input)
	};
	let joined = if raw.is_absolute() {
		raw
	} else {
		cwd.join(raw)
	};
	let mut out = PathBuf::new();
	for component in joined.components() {
		match component {
			std::path::Component::CurDir => {},
			std::path::Component::ParentDir => {
				out.pop();
			},
			other => out.push(other),
		}
	}
	out
}

/// pi `defaultSessionWorktreeBranch`: `wt/<yyyymmdd-hhmmss>` in local time.
#[must_use]
pub fn default_worktree_branch() -> Str {
	let now = jiff::Zoned::now();
	Str::new(
		jiff::fmt::strtime::format("wt/%Y%m%d-%H%M%S", &now)
			.unwrap_or_else(|_| format!("wt/{}", now.timestamp().as_second())),
	)
}

/// The working directory the commands resolve against.
fn cwd(cx: &PanelCx<'_>) -> PathBuf {
	cx.services
		.project_dir()
		.or_else(|_| std::env::current_dir())
		.unwrap_or_else(|_| PathBuf::from("."))
}

fn dirs(cx: &PanelCx<'_>) -> Vec<Str> {
	SV_WORKSPACE_DIRS.get(cx.con)
}

fn set_dirs(cx: &PanelCx<'_>, dirs: &[Str]) -> Result<(), Str> {
	let value = Value::List(dirs.iter().cloned().map(Value::Str).collect());
	cx.con
		.exec(&format!("sv_workspace_dirs {value}"), omp_con::Source::Console)
		.map(|_| ())
		.map_err(|error| Str::new(error.to_string()))
}

/// pi `formatWorkspaceDirectories`.
#[must_use]
pub fn format_dirs(cwd: &Path, additional: &[Str], note: Option<&str>) -> Str {
	let mut lines = String::new();
	if let Some(note) = note {
		lines.push_str(note);
		lines.push('\n');
	}
	lines.push_str("Workspace directories:\n  ");
	lines.push_str(&cwd.display().to_string());
	lines.push_str(" (working directory)");
	for dir in additional {
		lines.push_str("\n  ");
		lines.push_str(dir);
	}
	Str::new(lines)
}

fn add_dir(cx: &PanelCx<'_>, input: &str) -> PanelEvent {
	let cwd = cwd(cx);
	let resolved = resolve_to_cwd(input, &cwd);
	if !resolved.is_dir() {
		return notice(if resolved.exists() {
			sf!("Not a directory: {}", resolved.display())
		} else {
			sf!("Directory does not exist: {}", resolved.display())
		});
	}
	let resolved = Str::new(resolved.display().to_string());
	let mut dirs = dirs(cx);
	if resolved.as_str() == cwd.display().to_string() || dirs.contains(&resolved) {
		return notice(sf!("Already in the workspace: {resolved}"));
	}
	dirs.push(resolved.clone());
	if let Err(error) = set_dirs(cx, &dirs) {
		return notice(error);
	}
	notice(format_dirs(&cwd, &dirs, Some(&format!("Added {resolved}."))))
}

fn remove_dir(cx: &PanelCx<'_>, input: &str) -> PanelEvent {
	let cwd = cwd(cx);
	let resolved = resolve_to_cwd(input, &cwd);
	if resolved == cwd {
		return notice("Cannot remove the working directory; use /move to change it.");
	}
	let resolved = Str::new(resolved.display().to_string());
	let mut dirs = dirs(cx);
	let Some(at) = dirs.iter().position(|dir| *dir == resolved) else {
		return notice(sf!("Not a workspace directory: {resolved}"));
	};
	dirs.remove(at);
	if let Err(error) = set_dirs(cx, &dirs) {
		return notice(error);
	}
	notice(format_dirs(&cwd, &dirs, Some(&format!("Removed {resolved}."))))
}

omp_con::cmd! {
	/// Adds a workspace directory to this session (multi-root): `/add-dir <path>`.
	"add-dir"(?path: Str) = |ctx, args| {
		let path = rest(args, 0);
		call(ctx, PanelCall::new(move |cx| match &path {
			Some(path) => add_dir(cx, path),
			None => notice(format_dirs(&cwd(cx), &dirs(cx), Some("Usage: /add-dir <path>"))),
		}))
	};

	/// Removes a workspace directory from this session: `/remove-dir <path>`.
	"remove-dir"(?path: Str) = |ctx, args| {
		let path = rest(args, 0).ok_or_else(|| usage("Usage: /remove-dir <path>"))?;
		call(ctx, PanelCall::new(move |cx| remove_dir(cx, &path)))
	};

	/// Lists this session's workspace directories.
	dirs() = |ctx, _args| {
		call(ctx, PanelCall::new(|cx| notice(format_dirs(&cwd(cx), &dirs(cx), None))))
	};

	/// Moves the current session to a different directory: `/move <path>`.
	"move"(?path: Str) = |ctx, args| {
		let path = rest(args, 0).ok_or_else(|| usage("Usage: /move <path>"))?;
		post(ctx, HostAction::Command(CommandAction::Move { path }))
	};

	/// Moves this session into a new worktree, changes included: `/wt [branch]`.
	wt(?branch: Str) = |ctx, args| {
		post(ctx, HostAction::Command(CommandAction::Worktree { branch: rest(args, 0) }))
	};

	/// Moves this session into a new worktree (alias of `wt`).
	worktree(?branch: Str) = |ctx, args| {
		post(ctx, HostAction::Command(CommandAction::Worktree { branch: rest(args, 0) }))
	};
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn paths_resolve_against_the_working_directory_and_normalize() {
		let cwd = Path::new("/work/omp");
		assert_eq!(resolve_to_cwd("crates", cwd), PathBuf::from("/work/omp/crates"));
		assert_eq!(resolve_to_cwd("../pi", cwd), PathBuf::from("/work/pi"));
		assert_eq!(resolve_to_cwd("/tmp/x/./y", cwd), PathBuf::from("/tmp/x/y"));
		assert_eq!(resolve_to_cwd("\"/tmp/quoted\"", cwd), PathBuf::from("/tmp/quoted"));
	}

	#[test]
	fn default_worktree_branch_is_a_timestamp() {
		let branch = default_worktree_branch();
		assert!(branch.starts_with("wt/"), "{branch}");
		assert_eq!(branch.len(), "wt/20260903-120000".len(), "{branch}");
	}

	#[test]
	fn workspace_listing_matches_pi() {
		let text = format_dirs(
			Path::new("/work/omp"),
			&[Str::new_static("/work/pi")],
			Some("Added /work/pi."),
		);
		assert_eq!(
			text,
			"Added /work/pi.\nWorkspace directories:\n  /work/omp (working directory)\n  /work/pi"
		);
	}
}
