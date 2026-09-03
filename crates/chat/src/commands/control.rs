//! Model, lifecycle, and MCP slash commands (pi `builtin-modes.ts`,
//! `builtin-lifecycle.ts`, `builtin-control.ts`, `builtin-collaboration.ts`,
//! `builtin-session.ts`): `/model` (`/models`), `/switch`, `/fast`,
//! `/retry`, `/clear`, `/exit`, `/quit` (`/q`), `/restart`, `/dump`,
//! `/mcp`.
//!
//! The model commands reuse the host's picker and roster
//! ([`HostAction::ModelSelect`], [`HostAction::ModelSet`]); `/fast` flips
//! the `ai_fastmode` convar (ADR 0012: the convar *is* the setting);
//! `/clear` asks the controller for a context reset; `/restart` marks the
//! process for re-exec and leaves through the same exit path as `/quit`;
//! `/dump` copies the transcript pi-style and writes the request sidecar
//! through the [`Services`] seam; `/mcp` runs every pi subcommand through
//! [`Services::mcp`] in a loader panel.
//!
//! [`Services`]: crate::overlays::services::Services
//! [`Services::mcp`]: crate::overlays::services::Services::mcp

use std::fmt::Write as _;

use omp_con::{ConError, Severity, Value};
use omp_core::{Str, sf};
use omp_tui::Icon;

use super::{CommandAction, PaletteEntry, rest};
use crate::{
	actions::{HostAction, post},
	overlays::{
		PanelAnchor, PanelCall, PanelCx, PanelEvent, PanelOpener,
		report::ReportPanel,
		services::{McpAdd, McpOp, McpScope},
		tasks::{PendingPanel, Settle},
	},
	project::{BlockKind, block_views},
};

/// Palette icons for this module's commands.
pub const PALETTE: &[PaletteEntry] = &[
	PaletteEntry { name: "model", icon: Icon::Model },
	PaletteEntry { name: "models", icon: Icon::Model },
	PaletteEntry { name: "switch", icon: Icon::Swap },
	PaletteEntry { name: "fast", icon: Icon::Fast },
	PaletteEntry { name: "retry", icon: Icon::Redo },
	PaletteEntry { name: "clear", icon: Icon::Broom },
	PaletteEntry { name: "exit", icon: Icon::Exit },
	PaletteEntry { name: "quit", icon: Icon::Power },
	PaletteEntry { name: "q", icon: Icon::Power },
	PaletteEntry { name: "restart", icon: Icon::Refresh },
	PaletteEntry { name: "dump", icon: Icon::Clipboard },
	PaletteEntry { name: "mcp", icon: Icon::Mcp },
];

/// pi `MCPCommandController.#showHelp`.
const MCP_HELP: &str =
	"**MCP Server Management**\n\n`/mcp add <name> [--scope project|user] [--url <url>] [-- \
	 <command...>]` — Add a new MCP server\n`/mcp list` — List all configured MCP servers\n`/mcp \
	 remove <name> [--scope project|user]` — Remove an MCP server\n`/mcp test <name>` — Test \
	 connection to a server\n`/mcp reauth <name>` — Reauthorize OAuth for a server\n`/mcp unauth \
	 <name>` — Remove OAuth auth from a server\n`/mcp enable <name>` — Enable an MCP server\n`/mcp \
	 disable <name>` — Disable an MCP server\n`/mcp reconnect <name>` — Reconnect to a specific \
	 MCP server\n`/mcp reload` — Force reload MCP runtime tools\n`/mcp resources` — List available \
	 resources from connected servers\n`/mcp prompts` — List available prompts from connected \
	 servers\n`/mcp notifications` — Show notification capabilities and subscriptions\n`/mcp \
	 smithery-search <keyword>`, `/mcp smithery-login`, `/mcp smithery-logout` — Smithery registry \
	 (not available on this host)\n`/mcp help` — Show this message";
const SMITHERY_UNAVAILABLE: &str =
	"Smithery registry integration is not available on this host; add servers with /mcp add.";

const fn usage(message: &'static str) -> ConError {
	ConError::Usage(Str::new_static(message))
}

fn call(ctx: &omp_con::Ctx, call: PanelCall) -> omp_con::ConResult<()> {
	post(ctx, HostAction::Call(call))
}

fn open(ctx: &omp_con::Ctx, opener: PanelOpener) -> omp_con::ConResult<()> {
	post(ctx, HostAction::Open(opener))
}

fn notice(text: impl Into<Str>) -> PanelEvent {
	PanelEvent::Notice(text.into())
}

/// `/fast` argument (pi accepts `toggle`, `on`, `off`, `status`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FastOp {
	/// Bare `/fast` or `toggle`.
	Toggle,
	/// `on`.
	On,
	/// `off`.
	Off,
	/// `status`.
	Status,
}

/// Parses the `/fast` word.
pub fn fast_op(word: Option<&str>) -> Result<FastOp, ConError> {
	Ok(match word.map(|word| word.trim().to_ascii_lowercase()).as_deref() {
		None | Some("" | "toggle") => FastOp::Toggle,
		Some("on") => FastOp::On,
		Some("off") => FastOp::Off,
		Some("status") => FastOp::Status,
		Some(_) => return Err(usage("Usage: /fast [on|off|status]")),
	})
}

/// Applies a `/fast` word against the live convar and returns pi's line.
fn apply_fast(cx: &PanelCx<'_>, op: FastOp) -> PanelEvent {
	let current = omp_con::AI_FASTMODE.get(cx.con);
	let next = match op {
		FastOp::Toggle => Some(!current),
		FastOp::On => Some(true),
		FastOp::Off => Some(false),
		FastOp::Status => None,
	};
	if let Some(next) = next {
		let script = format!("ai_fastmode {}", Value::Bool(next));
		if let Err(error) = cx.con.exec(&script, omp_con::Source::Console) {
			return notice(error.to_string());
		}
		return notice(if next {
			"Fast mode enabled."
		} else {
			"Fast mode disabled."
		});
	}
	notice(if current {
		"Fast mode is on."
	} else {
		"Fast mode is off."
	})
}

/// pi `formatSessionAsText`: the transcript as role-labelled plain text.
#[must_use]
pub fn transcript_text(dom: &omp_dom::Dom) -> Str {
	let mut out = String::new();
	for block in block_views(dom, true) {
		let role = match block.kind {
			BlockKind::User => "User",
			BlockKind::Assistant => "Assistant",
			BlockKind::Thinking => "Thinking",
			BlockKind::Tool => "Tool",
			BlockKind::Notice => "Notice",
			BlockKind::Usage | BlockKind::Divider | BlockKind::Welcome => continue,
		};
		if !out.is_empty() {
			out.push_str("\n\n");
		}
		let _ = write!(out, "{role}:\n{}", block.text.trim_end());
	}
	Str::new(out)
}

/// `/dump`: copies the transcript (plus the sidecar path and pi's warning)
/// and reports through the console reply sink, which the host shows as a
/// status notice.
fn dump(cx: &PanelCx<'_>) -> PanelEvent {
	let text = transcript_text(cx.dom);
	if text.is_empty() {
		return notice("No messages to dump yet.");
	}
	let mut doc = text.to_string();
	let mut status = vec![Str::new_static("Session copied to clipboard")];
	match cx.services.dump_request() {
		Ok(path) => {
			let _ = write!(
				doc,
				"\n\n---\nLLM request JSON: {}\nThis file persists on disk and may contain raw \
				 context/secrets — treat accordingly.",
				path.display()
			);
			status.push(sf!("LLM request JSON: {}", path.display()));
		},
		Err(error) => status.push(sf!("LLM request JSON unavailable: {error}")),
	}
	cx.con.reply(Severity::Info, &status.join("\n"));
	PanelEvent::Copy(Str::new(doc))
}

/// Parses `/mcp <sub> [args…]` into the operation the services run, the
/// help report, or pi's error line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpCommand {
	/// Bare `/mcp` or `help`.
	Help,
	/// A runnable operation.
	Run(McpOp),
	/// Smithery subcommands: reported as unavailable.
	Smithery,
}

/// pi `MCPCommandController.handle` dispatch.
pub fn mcp_command(words: &[&str]) -> Result<McpCommand, ConError> {
	let Some((verb, tail)) = words.split_first() else {
		return Ok(McpCommand::Help);
	};
	let name = |usage_line: &'static str| -> Result<Str, ConError> {
		tail
			.first()
			.filter(|name| !name.starts_with("--"))
			.map(|name| Str::new(*name))
			.ok_or_else(|| usage(usage_line))
	};
	Ok(match verb.to_ascii_lowercase().as_str() {
		"help" => McpCommand::Help,
		"list" => McpCommand::Run(McpOp::List),
		"reload" => McpCommand::Run(McpOp::Reload),
		"resources" => McpCommand::Run(McpOp::Resources),
		"prompts" => McpCommand::Run(McpOp::Prompts),
		"notifications" => McpCommand::Run(McpOp::Notifications),
		"test" => {
			McpCommand::Run(McpOp::Test(name("Server name required. Usage: /mcp test <name>")?))
		},
		"reconnect" => McpCommand::Run(McpOp::Reconnect(name(
			"Server name required. Usage: /mcp reconnect <name>",
		)?)),
		"reauth" => {
			McpCommand::Run(McpOp::Reauth(name("Server name required. Usage: /mcp reauth <name>")?))
		},
		"unauth" => {
			McpCommand::Run(McpOp::Unauth(name("Server name required. Usage: /mcp unauth <name>")?))
		},
		"enable" => McpCommand::Run(McpOp::SetEnabled(
			name("Server name required. Usage: /mcp enable <name>")?,
			true,
		)),
		"disable" => McpCommand::Run(McpOp::SetEnabled(
			name("Server name required. Usage: /mcp disable <name>")?,
			false,
		)),
		"remove" | "rm" => {
			let name = name("Server name required. Usage: /mcp remove <name> [--scope project|user]")?;
			McpCommand::Run(McpOp::Remove(name, scope(&tail[1..])?))
		},
		"add" => McpCommand::Run(McpOp::Add(parse_add(tail)?)),
		"smithery-search" | "smithery-login" | "smithery-logout" => McpCommand::Smithery,
		_ => {
			return Err(ConError::Usage(sf!("Unknown subcommand: {verb}. Type /mcp help for usage.")));
		},
	})
}

fn scope(words: &[&str]) -> Result<McpScope, ConError> {
	let mut scope = McpScope::Project;
	let mut words = words.iter();
	while let Some(word) = words.next() {
		if *word == "--scope" {
			scope = words
				.next()
				.and_then(|value| value.parse().ok())
				.ok_or_else(|| usage("--scope must be project or user"))?;
		}
	}
	Ok(scope)
}

/// pi `parseAddArgs`: `<name> [--scope project|user] [--url <url>] [--
/// <command…>]`.
fn parse_add(words: &[&str]) -> Result<McpAdd, ConError> {
	const USAGE: &str =
		"Usage: /mcp add <name> [--scope project|user] [--url <url>] [-- <command...>]";
	let mut words = words.iter().copied();
	let name = words
		.next()
		.filter(|name| !name.starts_with("--"))
		.ok_or_else(|| usage(USAGE))?;
	let mut add = McpAdd {
		name:    Str::new(name),
		scope:   McpScope::Project,
		url:     None,
		command: Vec::new(),
	};
	while let Some(word) = words.next() {
		match word {
			"--scope" => {
				add.scope = words
					.next()
					.and_then(|value| value.parse().ok())
					.ok_or_else(|| usage("--scope must be project or user"))?;
			},
			"--url" => {
				add.url = Some(Str::new(words.next().ok_or_else(|| usage(USAGE))?));
			},
			"--" => {
				add.command = words.by_ref().map(Str::new).collect();
			},
			_ => return Err(usage(USAGE)),
		}
	}
	if add.url.is_none() && add.command.is_empty() {
		return Err(usage("Provide --url <url> or -- <command...> for the server."));
	}
	if add.url.is_some() && !add.command.is_empty() {
		return Err(usage("Provide either --url or a command, not both."));
	}
	Ok(add)
}

/// Loader panel over one MCP operation, titled by its verb.
fn mcp_panel(op: McpOp) -> PanelOpener {
	PanelOpener::new(move |cx| {
		let (title, message) = match &op {
			McpOp::List => ("MCP servers", sf!("Listing MCP servers…")),
			McpOp::Test(name) => ("MCP test", sf!("Testing connection to \"{name}\"...")),
			McpOp::Reload => ("MCP reload", sf!("Reloading MCP servers and runtime tools...")),
			McpOp::Reconnect(name) => ("MCP reconnect", sf!("Reconnecting to \"{name}\"...")),
			McpOp::SetEnabled(name, true) => ("MCP enable", sf!("Enabling \"{name}\"…")),
			McpOp::SetEnabled(name, false) => ("MCP disable", sf!("Disabling \"{name}\"…")),
			McpOp::Remove(name, _) => ("MCP remove", sf!("Removing \"{name}\"…")),
			McpOp::Add(add) => ("MCP add", sf!("Adding \"{}\"…", add.name)),
			McpOp::Reauth(name) => ("MCP reauth", sf!("Reauthorizing \"{name}\"…")),
			McpOp::Unauth(name) => ("MCP unauth", sf!("Clearing auth for \"{name}\"…")),
			McpOp::Resources => ("MCP resources", sf!("Listing resources…")),
			McpOp::Prompts => ("MCP prompts", sf!("Listing prompts…")),
			McpOp::Notifications => ("MCP notifications", sf!("Reading notification state…")),
		};
		let run = cx
			.services
			.mcp(op.clone())
			.map_err(|error| sf!("{error}"))?;
		Ok(Box::new(PendingPanel::new(
			"mcp",
			PanelAnchor::Center,
			title,
			message,
			run.done,
			run.cancel,
			Settle::Show,
			cx.ui,
		)))
	})
}

omp_con::cmd! {
	/// Switches the model for this session; `/model <id>` sets it directly.
	model(?selector: Str) = |ctx, args| match rest(args, 0) {
		Some(selector) => post(ctx, HostAction::ModelSet(selector)),
		None => post(ctx, HostAction::ModelSelect { session_only: false }),
	};

	/// Switches the model for this session (alias of `model`).
	models(?selector: Str) = |ctx, args| match rest(args, 0) {
		Some(selector) => post(ctx, HostAction::ModelSet(selector)),
		None => post(ctx, HostAction::ModelSelect { session_only: false }),
	};

	/// Switches the model for this session only (same as Alt+P).
	switch() = |ctx, _args| post(ctx, HostAction::ModelSelect { session_only: true });

	/// Toggles the priority service tier: `/fast [on|off|status]`.
	fast(?mode: Str) = |ctx, args| {
		let op = fast_op(rest(args, 0).as_deref())?;
		call(ctx, PanelCall::new(move |cx| apply_fast(cx, op)))
	};

	/// Retries the last failed agent turn.
	retry() = |ctx, _args| post(ctx, HostAction::Retry);

	/// Clears the conversation context in place, keeping the session.
	clear() = |ctx, _args| post(ctx, HostAction::Command(CommandAction::Clear));

	/// Exits the application.
	exit() = |ctx, _args| post(ctx, HostAction::Exit);

	/// Quits the application.
	quit() = |ctx, _args| post(ctx, HostAction::Exit);

	/// Quits the application (alias of `quit`).
	q() = |ctx, _args| post(ctx, HostAction::Exit);

	/// Restarts omp with the same launch flags, resuming this session.
	restart() = |ctx, _args| {
		call(ctx, PanelCall::new(|cx| match cx.services.request_restart() {
			Ok(()) => PanelEvent::Finish(Str::new_static("cl_exit")),
			Err(error) => notice(sf!("Restart is unavailable: {error}")),
		}))
	};

	/// Copies the session transcript to the clipboard and writes the LLM request JSON to tmp.
	dump() = |ctx, _args| call(ctx, PanelCall::new(dump));

	/// Manages MCP servers: `/mcp <add|list|remove|test|reauth|unauth|enable|disable|reconnect|reload|resources|prompts|notifications|help>`.
	mcp(?sub: Str, ?args: Str) = |ctx, args| {
		let words = rest(args, 0).unwrap_or_default();
		let words = words.split_whitespace().collect::<Vec<_>>();
		match mcp_command(&words)? {
			McpCommand::Help => open(ctx, PanelOpener::new(|cx| {
				Ok(Box::new(ReportPanel::new("mcp", "MCP Server Management", MCP_HELP, cx.ui)))
			})),
			McpCommand::Smithery => call(ctx, PanelCall::new(|_cx| notice(SMITHERY_UNAVAILABLE))),
			McpCommand::Run(op) => open(ctx, mcp_panel(op)),
		}
	};
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fast_words_follow_pi() {
		assert_eq!(fast_op(None).unwrap(), FastOp::Toggle);
		assert_eq!(fast_op(Some("toggle")).unwrap(), FastOp::Toggle);
		assert_eq!(fast_op(Some("ON")).unwrap(), FastOp::On);
		assert_eq!(fast_op(Some("off")).unwrap(), FastOp::Off);
		assert_eq!(fast_op(Some("status")).unwrap(), FastOp::Status);
		assert_eq!(fast_op(Some("bogus")).unwrap_err().to_string(), "Usage: /fast [on|off|status]");
	}

	#[test]
	fn mcp_words_dispatch_every_pi_subcommand() {
		assert_eq!(mcp_command(&[]).unwrap(), McpCommand::Help);
		assert_eq!(mcp_command(&["help"]).unwrap(), McpCommand::Help);
		assert_eq!(mcp_command(&["list"]).unwrap(), McpCommand::Run(McpOp::List));
		assert_eq!(mcp_command(&["reload"]).unwrap(), McpCommand::Run(McpOp::Reload));
		assert_eq!(
			mcp_command(&["test", "github"]).unwrap(),
			McpCommand::Run(McpOp::Test(Str::new_static("github")))
		);
		assert_eq!(
			mcp_command(&["rm", "github", "--scope", "user"]).unwrap(),
			McpCommand::Run(McpOp::Remove(Str::new_static("github"), McpScope::User))
		);
		assert_eq!(
			mcp_command(&["disable", "github"]).unwrap(),
			McpCommand::Run(McpOp::SetEnabled(Str::new_static("github"), false))
		);
		assert_eq!(
			mcp_command(&["add", "fs", "--", "npx", "server-fs", "."]).unwrap(),
			McpCommand::Run(McpOp::Add(McpAdd {
				name:    Str::new_static("fs"),
				scope:   McpScope::Project,
				url:     None,
				command: vec![
					Str::new_static("npx"),
					Str::new_static("server-fs"),
					Str::new_static(".")
				],
			}))
		);
		assert_eq!(
			mcp_command(&["add", "linear", "--scope", "user", "--url", "https://mcp.linear.app/sse"])
				.unwrap(),
			McpCommand::Run(McpOp::Add(McpAdd {
				name:    Str::new_static("linear"),
				scope:   McpScope::User,
				url:     Some(Str::new_static("https://mcp.linear.app/sse")),
				command: Vec::new(),
			}))
		);
		assert_eq!(mcp_command(&["smithery-login"]).unwrap(), McpCommand::Smithery);
		assert_eq!(
			mcp_command(&["test"]).unwrap_err().to_string(),
			"Server name required. Usage: /mcp test <name>"
		);
		assert_eq!(
			mcp_command(&["bogus"]).unwrap_err().to_string(),
			"Unknown subcommand: bogus. Type /mcp help for usage."
		);
		assert!(mcp_command(&["add", "x"]).is_err());
	}
}
