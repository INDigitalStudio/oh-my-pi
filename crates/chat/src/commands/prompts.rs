//! Prompt templates as slash commands (pi `interactive-mode.ts`
//! `promptTemplateCommands`): every discovered template is a dynamic console
//! command named after it, so `/review fix the tests` in the composer, a
//! bound key, and a cfg line all run the same statement. The handler expands
//! the template with the statement's words and posts the text as a
//! [`CommandAction::Prompt`], which the host submits exactly like a typed
//! prompt.
//!
//! Discovery and substitution live with the application (the driver's
//! `discovery::prompts`); this module owns only the console seam, reached
//! through the one typed [`PromptExpander`] the application installs.

use std::sync::Arc;

use omp_con::{Arg, ConError, ConResult, Ctx, DynamicCmdSpec, Severity};
use omp_core::Str;

use super::CommandAction;

/// The application's prompt-template table, as the console sees it.
pub trait PromptExpander: Send + Sync + 'static {
	/// `(name, description)` rows in registration order.
	fn templates(&self) -> Vec<(Str, Str)>;

	/// Expands template `name` with the statement's words (`$1`,
	/// `$ARGUMENTS`, …); `None` when no template carries that name.
	fn expand(&self, name: &str, args: &[Str]) -> Option<Str>;
}

/// The installed expander, stored on the console as host data.
struct Installed(Arc<dyn PromptExpander>);

/// Registers every template as a dynamic console command and installs
/// `expander` as the table those commands expand from. Returns the names
/// that could not be registered because a built-in command already owns
/// them (pi drops those templates: `reservedNames`).
pub fn register(ctx: &Ctx, expander: Arc<dyn PromptExpander>) -> Vec<Str> {
	let mut reserved = Vec::new();
	for (name, desc) in expander.templates() {
		match ctx.register_dynamic_cmd(DynamicCmdSpec { name: name.clone(), desc, handler: run }) {
			Ok(()) => {},
			Err(ConError::Duplicate { .. }) => reserved.push(name),
			Err(error) => {
				ctx.reply(
					Severity::Warn,
					&format!("prompt template `{name}` was not registered: {error}"),
				);
			},
		}
	}
	ctx.insert_user(Installed(expander));
	reserved
}

/// Shared handler for every prompt-template command.
fn run(ctx: &Ctx, name: &str, args: &[Arg]) -> ConResult<()> {
	let Some(installed) = ctx.user::<Installed>() else {
		ctx.reply(Severity::Warn, "prompt templates are not installed on this console");
		return Ok(());
	};
	let words = args
		.iter()
		.map(|arg| match arg {
			Arg::Atom(word) => word.clone(),
			other => other.to_script(),
		})
		.collect::<Vec<_>>();
	match installed.0.expand(name, &words) {
		Some(text) => super::post(ctx, CommandAction::Prompt { text }),
		None => {
			ctx.reply(Severity::Warn, &format!("unknown prompt template `{name}`"));
			Ok(())
		},
	}
}

#[cfg(test)]
mod tests {
	use omp_con::Ctx;

	use super::*;
	use crate::actions::{HostAction, HostMailbox};

	struct Table;

	impl PromptExpander for Table {
		fn templates(&self) -> Vec<(Str, Str)> {
			vec![
				(Str::new_static("review"), Str::new_static("Review a file (project)")),
				(Str::new_static("help"), Str::new_static("collides with the console builtin")),
			]
		}

		fn expand(&self, name: &str, args: &[Str]) -> Option<Str> {
			(name == "review").then(|| {
				let mut text = String::from("Review ");
				text.push_str(args.first().map_or("", Str::as_str));
				text.push_str(" carefully: ");
				text.push_str(&args.join(" "));
				Str::new(text)
			})
		}
	}

	#[test]
	fn template_command_expands_words_and_posts_a_prompt() {
		let ctx = HostMailbox::new().attach(Ctx::builder()).build();
		let reserved = register(&ctx, Arc::new(Table));
		assert_eq!(reserved, [Str::new_static("help")], "builtin names stay reserved");
		assert!(ctx.dynamic_cmds().any(|(name, _)| name == "review"));

		ctx.run("review src/lib.rs \"the tests\"").unwrap();
		let mailbox = ctx.user::<HostMailbox>().expect("attached mailbox");
		let posted = mailbox
			.drain()
			.find_map(|action| match action {
				HostAction::Command(CommandAction::Prompt { text }) => Some(text),
				_ => None,
			})
			.expect("a posted prompt");
		assert_eq!(posted, "Review src/lib.rs carefully: src/lib.rs the tests");
	}
}
