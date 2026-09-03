//! Boot chrome: the welcome banner, the composer status band, and the
//! composer shell. Every glyph and color comes from the ambient
//! [`UiContext`](omp_tui::UiContext); the shapes follow pi's `welcome.ts`
//! and the status-band composer.

use std::time::Duration;

use omp_core::Str;
use omp_tui::{
	Charset, Prop, Ui, UiContext,
	components::{Col, ComposerStyle, EditorPane, KeywordAccent, Spacer},
};

use crate::overlays::ModelRow;
pub use crate::{
	status_band::{PathLabel, StatusBand, StatusFacts, display_path},
	welcome::{Welcome, tip_for},
};

omp_con::var! {
	/// pi `startup.quiet`: skip the welcome box and startup-only status
	/// notices on interactive launch.
	pub static CL_STARTUP_QUIET = cl_startup_quiet: bool {
		default: false,
		flags: archive,
	};
	/// pi `tui.titleState`: show the agent run state in the terminal title's
	/// separator — a spinner while working, `>` on the user's turn, `!`
	/// while the agent waits on the user.
	pub static CL_TITLE_STATE = cl_title_state: bool {
		default: true,
		flags: archive,
	};
	/// pi `terminal.showProgress`: emit OSC 9;4 indeterminate progress
	/// while a turn or context maintenance runs. Off by default.
	pub static CL_SHOW_PROGRESS = cl_show_progress: bool {
		default: false,
		flags: archive,
	};
}

/// Element id of the composer editor inside the chrome tree.
pub const COMPOSER_ID: &str = "composer";
/// Element id of the status band inside the chrome tree.
pub const STATUS_ID: &str = "status-band";
/// Element id of the one-row gap above the composer (pi `EditorTopGap`).
pub const GAP_ID: &str = "composer-gap";
/// Composer placeholder shared with the gallery composer previews.
pub const PLACEHOLDER: &str = "Ask anything, edit files, run tools";

/// Catalog facts for the active model. The host seeds these at launch and
/// replaces them on every live model switch; they are projections, never
/// journal authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelBadge {
	/// Canonical `provider/model` identifier the session was launched with.
	pub identifier:     Str,
	/// Human-readable model name (catalog display name).
	pub name:           Str,
	/// Provider identifier.
	pub provider:       Str,
	/// Total context window in tokens when the catalog knows it.
	pub context_window: Option<u64>,
	/// Whether the model can reason (the band then shows the thinking level).
	pub reasoning:      bool,
}

impl ModelBadge {
	/// Derives a badge from a `provider/model` identifier when no catalog
	/// record is available.
	#[must_use]
	pub fn from_identifier(identifier: &str) -> Self {
		let (provider, name) = identifier.split_once('/').unwrap_or(("", identifier));
		Self {
			identifier:     Str::new(identifier),
			name:           Str::new(name),
			provider:       Str::new(provider),
			context_window: None,
			reasoning:      false,
		}
	}

	/// Rebuilds the badge from a picker row: the catalog facts of a model
	/// the user switched to (pi refreshes welcome and status on every
	/// `model_changed`).
	#[must_use]
	pub fn from_row(row: &ModelRow) -> Self {
		Self {
			identifier:     row.key.clone(),
			name:           if row.name.is_empty() {
				row.key.clone()
			} else {
				row.name.clone()
			},
			provider:       row.provider_id.clone(),
			context_window: row.context,
			reasoning:      !row.efforts.is_empty(),
		}
	}

	/// Model label for the status band: pi drops the `Claude ` prefix
	/// (`status-line/segments.ts` `modelSegment`).
	#[must_use]
	pub fn short_name(&self) -> Str {
		match self.name.as_str().strip_prefix("Claude ") {
			Some(short) => Str::new(short),
			None => self.name.clone(),
		}
	}
}

/// Agent run state carried by the terminal title's separator (pi
/// `TerminalTitleState`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TitleState {
	/// The user's turn: `>` reads like a shell prompt awaiting input.
	#[default]
	Idle,
	/// A turn or maintenance is running: the spinner (`:` on Windows).
	Working,
	/// The agent is blocked on the user (ask / approval): `!`.
	Attention,
}

/// pi `title-generator.ts`: the terminal title as a run-state machine.
/// The label is the sanitized session name, else the project directory's
/// base name; the separator carries the state while `cl_title_state` is
/// on (`π > label`, `π ⠋ label`, `π ! label`), else `π: label`. The
/// composed text is deduplicated so a terminal sees one OSC per change.
#[derive(Clone, Debug, Default)]
pub struct TerminalTitle {
	label:   Option<Str>,
	state:   TitleState,
	enabled: bool,
	/// Title last handed to the terminal.
	sent:    String,
	/// Composition scratch, reused per frame.
	text:    String,
}

impl TerminalTitle {
	/// A title with the run state enabled and no label.
	#[must_use]
	pub fn new() -> Self {
		Self { enabled: true, ..Self::default() }
	}

	/// Sets the session label: `name` sanitized when present, else the base
	/// name of `cwd` (pi `setSessionTerminalTitle`).
	pub fn set_label(&mut self, name: Option<&str>, cwd: &str) {
		self.label = name
			.and_then(sanitize_title_part)
			.or_else(|| fallback_title(cwd));
	}

	/// Sets the run state (pi `setTerminalTitleState`).
	pub const fn set_state(&mut self, state: TitleState) {
		self.state = state;
	}

	/// Enables or disables the run-state separator (pi
	/// `setTerminalTitleStateEnabled`).
	pub const fn set_enabled(&mut self, enabled: bool) {
		self.enabled = enabled;
	}

	/// The current run state.
	#[must_use]
	pub const fn state(&self) -> TitleState {
		self.state
	}

	/// Composes the title at `now` and returns it when it differs from the
	/// last one handed out; the caller writes it to the terminal. `None`
	/// means the terminal already shows this title.
	pub fn emit(&mut self, charset: Charset, now: Duration) -> Option<&str> {
		self.text.clear();
		compose_title(&mut self.text, self.label.as_deref(), self.state, self.enabled, charset, now);
		if self.text == self.sent {
			return None;
		}
		self.sent.clear();
		self.sent.push_str(&self.text);
		Some(&self.sent)
	}

	/// When the title next changes on its own: the spinner frame after
	/// `now` while working with the run state enabled.
	#[must_use]
	pub fn next_wake(&self, charset: Charset, now: Duration) -> Option<Duration> {
		(self.enabled && self.state == TitleState::Working && !cfg!(windows))
			.then(|| charset.spinner().next_change(now))
	}
}

/// The brand at the head of the title: `π`, or `pi` for an ASCII terminal
/// (the window-system font renders the title, so never a Nerd Font glyph).
const fn title_brand(charset: Charset) -> &'static str {
	match charset {
		Charset::Ascii => "pi",
		Charset::Unicode | Charset::NerdFont => "π",
	}
}

/// pi `buildTerminalTitleWithState`, written into `out`.
fn compose_title(
	out: &mut String,
	label: Option<&str>,
	state: TitleState,
	enabled: bool,
	charset: Charset,
	now: Duration,
) {
	out.push_str(title_brand(charset));
	if !enabled {
		if let Some(label) = label {
			out.push_str(": ");
			out.push_str(label);
		}
		return;
	}
	out.push(' ');
	match state {
		TitleState::Working if cfg!(windows) => out.push(':'),
		TitleState::Working => out.push_str(charset.spinner().at(now)),
		TitleState::Attention => out.push('!'),
		TitleState::Idle => out.push('>'),
	}
	if let Some(label) = label {
		out.push(' ');
		out.push_str(label);
	}
}

/// Drops control characters so a model-generated title can never terminate
/// the OSC or inject another terminal command; empty after trimming is
/// `None` (pi `sanitizeTerminalTitlePart`).
fn sanitize_title_part(value: &str) -> Option<Str> {
	let clean = value
		.chars()
		.filter(|ch| !ch.is_control())
		.collect::<String>();
	let trimmed = clean.trim();
	(!trimmed.is_empty()).then(|| Str::new(trimmed))
}

/// The project directory's base name, unless it is a filesystem root (pi
/// `getFallbackTerminalTitle`).
fn fallback_title(cwd: &str) -> Option<Str> {
	let path = std::path::Path::new(cwd);
	let base = path.file_name()?.to_str()?;
	sanitize_title_part(base)
}

/// pi `EditorTopGap`: the one-row margin above the editor stays for every
/// shape except the band (omp's [`ComposerStyle::Borderless`]), whose status
/// band is designed to sit flush under an occupied status row — the notice
/// row the host paints directly above the composer. An empty status row
/// keeps the gap so the band never sits flush against the transcript.
#[must_use]
pub const fn top_gap_shown(shape: ComposerStyle, status_row_occupied: bool) -> bool {
	!matches!(shape, ComposerStyle::Borderless) || !status_row_occupied
}

/// Builds the composer chrome tree: the top-gap row, then the editor in
/// `shape` with its status band above the prompt and pi's magic-keyword
/// shimmer. Mount it with [`composer_ui`], which applies the gap rule.
#[must_use]
pub fn composer_root(facts: StatusFacts, shape: ComposerStyle) -> Col {
	let editor = EditorPane::new()
		.composer_style(shape)
		.keyword_accent(KeywordAccent::magic())
		.with(Prop::Id, COMPOSER_ID)
		.with(Prop::Submit, true)
		.with(Prop::Placeholder, PLACEHOLDER)
		.status(StatusBand::new(facts));
	Col::new()
		.child(Spacer::new().with(Prop::Id, GAP_ID))
		.child(editor)
}

/// Mounts [`composer_root`] as a retained tree at `width`, showing the top
/// gap per [`top_gap_shown`] for an unoccupied status row.
#[must_use]
pub fn composer_ui(facts: StatusFacts, shape: ComposerStyle, width: u16, ctx: UiContext) -> Ui {
	let mut ui = Ui::from_root(composer_root(facts, shape), width, ctx);
	ui.set_visible(GAP_ID, top_gap_shown(shape, false));
	ui
}

#[cfg(test)]
mod tests {
	use omp_con::{Ctx, RegItem, VarFlags};
	use omp_tui::frame_text;

	use super::*;
	use crate::status_band::tests::facts;

	#[test]
	fn startup_and_terminal_chrome_convars_match_pi_defaults_and_persist() {
		let con = Ctx::new();
		assert!(!CL_STARTUP_QUIET.get(&con), "welcome is shown by default");
		assert!(CL_TITLE_STATE.get(&con), "terminal title state is enabled by default");
		assert!(!CL_SHOW_PROGRESS.get(&con), "native terminal progress is opt-in");
		for name in ["cl_startup_quiet", "cl_title_state", "cl_show_progress"] {
			let Some(RegItem::Var(spec)) = con.find(name) else {
				panic!("missing chrome convar {name}");
			};
			assert!(spec.flags.contains(VarFlags::ARCHIVE), "{name} must persist");
		}
	}

	/// At rest (no notice above), pi's `EditorTopGap` keeps the blank row
	/// above the band, as in the boot reference capture.
	#[test]
	fn composer_root_paints_status_then_prompt_gutter() {
		let mut ui = composer_ui(
			StatusFacts { tokens: 0, ..facts() },
			ComposerStyle::Borderless,
			80,
			UiContext::default(),
		);
		ui.focus_first();
		let rows = frame_text(ui.frame())
			.lines()
			.map(str::to_owned)
			.collect::<Vec<_>>();
		assert_eq!(rows[0].trim(), "");
		assert!(rows[1].starts_with(" π  >"), "{}", rows[1]);
		assert!(rows[2].starts_with("╰─ Ask anything"), "{}", rows[2]);
		assert_eq!(ui.frame().cursor(), Some((3, 2)), "caret sits after the prompt gutter");
	}

	/// pi `EditorTopGap`: only the band over an occupied status row sits
	/// flush; every other shape keeps the one-row gap regardless.
	#[test]
	fn top_gap_collapses_only_for_the_band_over_an_occupied_status_row() {
		assert!(!top_gap_shown(ComposerStyle::Borderless, true));
		assert!(top_gap_shown(ComposerStyle::Borderless, false));
		assert!(top_gap_shown(ComposerStyle::Rail, true));
		assert!(top_gap_shown(ComposerStyle::Box, true));

		let mut ui = composer_ui(facts(), ComposerStyle::Rail, 80, UiContext::default());
		ui.focus_first();
		let rows = frame_text(ui.frame())
			.lines()
			.map(str::to_owned)
			.collect::<Vec<_>>();
		assert_eq!(rows[0].trim(), "", "rail shape keeps the top gap");
		assert_ne!(rows[1].trim(), "");
	}

	/// pi `buildTerminalTitleWithState`: `π > label` idle, `π ⠋ label`
	/// working, `π ! label` attention, `π: label` disabled; without a label
	/// the separator trails the brand.
	#[test]
	fn terminal_title_follows_pi_state_separators_and_dedupes() {
		let charset = Charset::Unicode;
		let mut title = TerminalTitle::new();
		title.set_label(Some("refactor auth"), "/work/omp");
		assert_eq!(title.emit(charset, Duration::ZERO), Some("π > refactor auth"));
		assert_eq!(title.emit(charset, Duration::ZERO), None, "unchanged titles are not re-sent");
		assert_eq!(title.next_wake(charset, Duration::ZERO), None, "idle never animates");

		title.set_state(TitleState::Working);
		if cfg!(windows) {
			assert_eq!(title.emit(charset, Duration::ZERO), Some("π : refactor auth"));
		} else {
			assert_eq!(title.emit(charset, Duration::ZERO), Some("π ⠋ refactor auth"));
			assert_eq!(
				title.next_wake(charset, Duration::ZERO),
				Some(Duration::from_millis(80)),
				"pi TITLE_SPINNER_INTERVAL_MS"
			);
			assert_eq!(title.emit(charset, Duration::from_millis(80)), Some("π ⠙ refactor auth"));
		}

		title.set_state(TitleState::Attention);
		assert_eq!(title.emit(charset, Duration::ZERO), Some("π ! refactor auth"));

		title.set_enabled(false);
		assert_eq!(title.emit(charset, Duration::ZERO), Some("π: refactor auth"));
		title.set_state(TitleState::Working);
		assert_eq!(title.emit(charset, Duration::ZERO), None, "disabled titles ignore the state");
		assert_eq!(title.next_wake(charset, Duration::ZERO), None);

		let mut bare = TerminalTitle::new();
		bare.set_label(None, "/");
		assert_eq!(bare.emit(charset, Duration::ZERO), Some("π >"), "no label: separator trails");
		assert_eq!(bare.emit(Charset::Ascii, Duration::ZERO), Some("pi >"));
	}

	/// pi `sanitizeTerminalTitlePart` / `getFallbackTerminalTitle`: control
	/// characters never reach the OSC; an unnamed session falls back to the
	/// project directory's base name.
	#[test]
	fn terminal_title_label_is_sanitized_and_falls_back_to_the_cwd_base_name() {
		let mut title = TerminalTitle::new();
		title.set_label(Some("  evil\x1b]0;pwned\x07 name\n"), "/work/omp");
		assert_eq!(title.emit(Charset::Unicode, Duration::ZERO), Some("π > evil]0;pwned name"));
		title.set_label(Some("   \x07 "), "/work/omp");
		assert_eq!(
			title.emit(Charset::Unicode, Duration::ZERO),
			Some("π > omp"),
			"a blank name falls back to the cwd base name"
		);
		title.set_label(None, "/");
		assert_eq!(title.emit(Charset::Unicode, Duration::ZERO), Some("π >"));
	}

	#[test]
	fn badge_from_row_carries_the_catalog_facts() {
		let row = ModelRow {
			key:         Str::new_static("openai/gpt-5"),
			name:        Str::new_static("GPT-5"),
			provider_id: Str::new_static("openai"),
			provider:    Str::new_static("OpenAI"),
			context:     Some(400_000),
			input_mtok:  None,
			output_mtok: None,
			efforts:     vec![Str::new_static("low"), Str::new_static("high")],
		};
		let badge = ModelBadge::from_row(&row);
		assert_eq!(badge.identifier.as_str(), "openai/gpt-5");
		assert_eq!(badge.name.as_str(), "GPT-5");
		assert_eq!(badge.provider.as_str(), "openai");
		assert_eq!(badge.context_window, Some(400_000));
		assert!(badge.reasoning);
		let plain =
			ModelBadge::from_row(&ModelRow { efforts: Vec::new(), name: Str::default(), ..row });
		assert!(!plain.reasoning);
		assert_eq!(plain.name.as_str(), "openai/gpt-5", "a nameless row shows its key");
	}
}
