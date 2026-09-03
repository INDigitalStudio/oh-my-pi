//! Observer-local composer: a retained editor tree whose draft never enters
//! the session DOM until submission.

use std::{cell::Cell, fmt::Write as _, path::Path, rc::Rc, time::Duration};

use omp_core::Str;
use omp_tui::{
	Command, EditorOptions, Frame, Key, SpellingFeatures, Ui, UiContext, UiEvent,
	components::{
		AttachmentContent, ComposerStyle, EditorPane, InlineAccent, PrefixAccent, marker_sized_paste,
	},
};

use crate::{
	autocomplete::{PromptAction, PromptActions, UrlCompleter, composer_chain},
	chrome::{COMPOSER_ID, GAP_ID, STATUS_ID, StatusBand, StatusFacts, composer_ui, top_gap_shown},
};

/// Editor row budget for a terminal of `rows` rows (pi
/// `computeEditorMaxHeight`): roomy terminals get the comfortable `[6, 18]`
/// band below twelve reserved rows; small terminals shrink the cap so the
/// editor leaves at least four rows for the transcript and status, never
/// dropping under the three-row bordered floor.
#[must_use]
pub fn editor_max_rows(rows: u16) -> u16 {
	const MIN: u16 = 6;
	const MAX: u16 = 18;
	const RESERVED: u16 = 12;
	const FALLBACK_ROWS: u16 = 24;
	const MIN_CHROME_ROWS: u16 = 4;
	const MIN_RENDERED_ROWS: u16 = 3;
	let rows = if rows == 0 { FALLBACK_ROWS } else { rows };
	let comfortable = rows.saturating_sub(RESERVED).clamp(MIN, MAX);
	comfortable
		.min(rows.saturating_sub(MIN_CHROME_ROWS))
		.max(MIN_RENDERED_ROWS)
}

/// Result of applying a composer key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComposerAction {
	/// Composer changed and needs repainting.
	Changed,
	/// Submit the current draft as a prompt.
	Submit(Str),
	/// Submit the draft with the image chips it references (pi
	/// `pendingImages`): `[Image #N]` in `text` is positional against
	/// `images[N-1]`, each an image file path the host reads once.
	SubmitWithImages {
		/// The draft with chips expanded to their wire markers.
		text:   Str,
		/// Staged image sources in marker order.
		images: Vec<Str>,
	},
	/// Run a submitted `/…` line as the console statement after the slash.
	Command(Str),
	/// Queue the draft behind the active turn (pi `->` / `=>` yield-queue
	/// shorthand): the body runs when the agent yields, or at once when it
	/// is idle with an empty queue. Image chips travel with it exactly as
	/// they do for [`ComposerAction::SubmitWithImages`].
	Queue {
		/// The body behind the sigil, chips expanded to their wire markers.
		text:   Str,
		/// Staged image sources in marker order.
		images: Vec<Str>,
	},
	/// Write text to the clipboard (the host owns OSC 52 / native access).
	Copy(Str),
	/// No composer action.
	Ignored,
}

/// pi `compactImageMarkers`: the submitted images are the visible chips in
/// marker order, so `[Image #M…]` for the K-th surviving marker `M` is
/// rewritten to `[Image #K…]` — the positional `[Image #N] ↔ images[N-1]`
/// contract holds after chips were deleted from the draft. Markers already
/// dense (the common case) leave the text untouched.
fn compact_image_markers(text: &str, markers: &[usize]) -> String {
	if markers
		.iter()
		.enumerate()
		.all(|(index, marker)| *marker == index + 1)
	{
		return text.to_owned();
	}
	const PREFIX: &str = "[Image #";
	let mut out = String::with_capacity(text.len());
	let mut rest = text;
	while let Some(at) = rest.find(PREFIX) {
		let (before, tail) = rest.split_at(at + PREFIX.len());
		out.push_str(before);
		let digits = tail
			.find(|c: char| !c.is_ascii_digit())
			.unwrap_or(tail.len());
		match tail[..digits]
			.parse::<usize>()
			.ok()
			.and_then(|old| markers.iter().position(|marker| *marker == old))
		{
			Some(index) if digits > 0 => {
				let _ = write!(out, "{}", index + 1);
			},
			_ => out.push_str(&tail[..digits]),
		}
		rest = &tail[digits..];
	}
	out.push_str(rest);
	out
}

/// pi `QUEUE_PREFIXES`: the yield-queue shorthand sigils.
const QUEUE_PREFIXES: [&str; 2] = ["->", "=>"];

/// pi `parseQueueShorthand`: the message body behind a leading `->` / `=>`
/// on the trimmed draft, `None` when the draft carries no shorthand.
#[must_use]
pub fn parse_queue_shorthand(text: &str) -> Option<&str> {
	let text = text.trim();
	QUEUE_PREFIXES
		.iter()
		.find_map(|prefix| text.strip_prefix(prefix))
		.map(str::trim)
}

/// What [`Composer::paste`] did with the text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasteOutcome {
	/// The editor took the paste (inline, or collapsed to a chip).
	Inserted,
	/// A marker-sized paste of `lines` lines reached
	/// `cl_paste_large_menu_threshold`: nothing was inserted and the host
	/// presents the large-paste menu (pi `handleLargePaste`).
	Menu {
		/// Line count of the paste, for the menu title.
		lines: usize,
	},
}

/// Composer knobs mirrored from convars (pi editor preferences).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ComposerSettings {
	/// pi `autocompleteMaxVisible`: dropdown window rows.
	pub autocomplete_max_visible: i64,
	/// pi `emojiAutocomplete`: `:shortcode:` dropdown and emoticon expansion.
	pub emoji_autocomplete:       bool,
	/// pi `paste.largeMenuThreshold`: line count at which a marker-sized
	/// paste opens the large-paste menu; `<= 0` disables the menu.
	pub paste_large_menu_lines:   i64,
}

impl Default for ComposerSettings {
	fn default() -> Self {
		Self {
			autocomplete_max_visible: 10,
			emoji_autocomplete:       true,
			paste_large_menu_lines:   100,
		}
	}
}

/// pi `shouldSkipHistory`: whether a submitted line is kept out of Up/Down
/// recall.
///
/// A `/login`, `/join`, or `/mcp add --token` line carries a secret (OAuth
/// callback, room key, bearer token). The command name is split exactly
/// like pi's `parseSlashCommand`: at the earliest whitespace or colon.
#[must_use]
pub fn should_skip_history(text: &str) -> bool {
	let Some(body) = text.strip_prefix('/') else {
		return false;
	};
	let separator = body.find(|character: char| character.is_whitespace() || character == ':');
	let (name, args) = match separator {
		Some(at) => (&body[..at], Some(body[at + 1..].trim())),
		None => (body, None),
	};
	match name {
		"login" | "join" => args.is_some(),
		"mcp" => args.is_some_and(|args| {
			args.starts_with("add")
				&& args
					.split_once("--token")
					.is_some_and(|(_, rest)| rest.starts_with(char::is_whitespace))
		}),
		_ => false,
	}
}

/// Composer prefix mode: the leading sigil recolors the chrome and Esc
/// clears the draft instead of interrupting (pi rung 8).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefixMode {
	/// `!` — shell command.
	Bash,
	/// `$` — eval expression.
	Eval,
}

/// One submitted prefix-mode line: what to run locally and whether the
/// model may see it (pi `!!` / `$$` `excludeFromContext`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalInput {
	/// Which executor the sigil selects.
	pub mode:    PrefixMode,
	/// The command or code after the sigil, trimmed.
	pub code:    Str,
	/// Keep the run out of the model's context.
	pub exclude: bool,
}

/// pi `pythonCommandPrefixLength`: `$` / `$$` starts eval mode only when
/// followed by whitespace or the end of input, so `$HOME is set` and `${x}`
/// stay prose.
fn eval_prefix_len(trimmed: &str) -> usize {
	let bytes = trimmed.as_bytes();
	if bytes.first() != Some(&b'$') || bytes.get(1) == Some(&b'{') {
		return 0;
	}
	let prefix = if bytes.get(1) == Some(&b'$') { 2 } else { 1 };
	match bytes.get(prefix) {
		None => prefix,
		Some(b' ' | b'\t' | b'\n' | b'\r') => prefix,
		Some(_) => 0,
	}
}

/// Commands a pasted shell prompt typically starts with (pi
/// `SHELL_PROMPT_COMMAND_RE`, minus the path forms handled inline).
const SHELL_PROMPT_COMMANDS: &[&str] = &[
	"cd", "sudo", "git", "bun", "npm", "pnpm", "yarn", "node", "cargo", "go", "make", "docker",
	"kubectl",
];

/// Whether `word` is a shell-prompt command: one of [`SHELL_PROMPT_COMMANDS`]
/// or `python` with an optional version suffix (`python`, `python3`,
/// `python3.12` is not: pi's `python\d*` stops at the digits).
fn is_shell_prompt_command(word: &str) -> bool {
	SHELL_PROMPT_COMMANDS.contains(&word)
		|| word
			.strip_prefix("python")
			.is_some_and(|rest| rest.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Whether `token` is a shell operator standing alone between whitespace
/// (pi `SHELL_PROMPT_OPERATOR_RE`: `&&`, `||`, `|`, `2>&1`, and one or two
/// redirection chevrons).
fn is_shell_operator(token: &str) -> bool {
	matches!(token, "&&" | "||" | "|" | "2>&1")
		|| ((1..=2).contains(&token.len()) && token.bytes().all(|byte| matches!(byte, b'<' | b'>')))
}

/// Whether `line` is omp's own status line (`in: 12 out: 34 [cache …] t: …
/// tok/s: …`, pi `OMP_STATUS_LINE_RE`), the tell of a pasted transcript.
fn is_status_line(line: &str) -> bool {
	fn number(word: &str) -> bool {
		!word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit())
	}
	let mut words = line.split_ascii_whitespace();
	if words.next() != Some("in:") || !words.next().is_some_and(number) {
		return false;
	}
	if words.next() != Some("out:") || !words.next().is_some_and(number) {
		return false;
	}
	let mut next = words.next();
	if next == Some("cache") {
		if words.next().is_none() {
			return false;
		}
		next = words.next();
	}
	next == Some("t:")
		&& words.next().is_some()
		&& words.next() == Some("tok/s:")
		&& words.next().is_some()
}

/// pi `looksLikePastedShellPrompt`: a single-`$` body shaped like a copied
/// terminal line (`$ cd ~/project && cargo test`, `$ git status`) stays an
/// ordinary prompt instead of being run as Python.
#[must_use]
pub fn looks_like_pasted_shell_prompt(code: &str) -> bool {
	let first = code.split('\n').next().unwrap_or_default().trim_start();
	let starts_like_path = first.starts_with('/')
		|| first.starts_with("./")
		|| first.starts_with("../")
		|| first.starts_with("~/");
	let head = first
		.split(|c: char| c.is_whitespace())
		.next()
		.unwrap_or_default();
	starts_like_path
		|| is_shell_prompt_command(head)
		|| first.split_whitespace().any(is_shell_operator)
		|| code.lines().any(is_status_line)
}

/// Splits a draft into its sigil and body (pi `parsePythonCommandInput`
/// plus the `!` branch of `handleSubmit`): the mode, the prefix length, and
/// the trimmed code. `None` is prose.
fn split_local(text: &str) -> Option<(PrefixMode, usize, &str)> {
	let trimmed = text.trim_start();
	let (mode, prefix) = if trimmed.starts_with('!') {
		(PrefixMode::Bash, if trimmed.starts_with("!!") { 2 } else { 1 })
	} else {
		match eval_prefix_len(trimmed) {
			0 => return None,
			len => (PrefixMode::Eval, len),
		}
	};
	let code = trimmed[prefix..].trim();
	if mode == PrefixMode::Eval && prefix == 1 && looks_like_pasted_shell_prompt(code) {
		return None;
	}
	Some((mode, prefix, code))
}

/// Classifies a draft's leading sigil (pi `isBashMode` / `isPythonMode`);
/// a pasted shell prompt behind a single `$` is prose.
#[must_use]
pub fn prefix_mode_of(text: &str) -> Option<PrefixMode> {
	split_local(text).map(|(mode, ..)| mode)
}

/// The editor chrome accent for a draft: the same grammar as
/// [`prefix_mode_of`], so `$HOME is set`, `${x}`, and a pasted `$ git
/// status` never paint eval mode.
fn prefix_accent_of(text: &str) -> Option<PrefixAccent> {
	prefix_mode_of(text).map(|mode| match mode {
		PrefixMode::Bash => PrefixAccent::Bash,
		PrefixMode::Eval => PrefixAccent::Eval,
	})
}

/// Dims the leading `->` / `=>` of a yield-queue shorthand draft (pi's
/// `QUEUE_LIST_MARKER_RE` editor highlighting).
fn queue_shorthand_spans(text: &str) -> smallvec::SmallVec<(usize, usize, InlineAccent), 4> {
	let mut spans = smallvec::SmallVec::new();
	let start = text.len() - text.trim_start().len();
	if let Some(prefix) = QUEUE_PREFIXES
		.iter()
		.find(|prefix| text[start..].starts_with(*prefix))
	{
		spans.push((start, start + prefix.len(), InlineAccent::Dim));
	}
	spans
}

/// Parses a submitted line into a local run (pi `input-controller.ts`
/// `handleSubmit`: `!cmd`, `!!cmd`, `$ code`, `$$ code`). `None` is an
/// ordinary prompt, including a bare sigil with nothing to run and a
/// single-`$` line that [`looks_like_pasted_shell_prompt`].
#[must_use]
pub fn parse_local_input(text: &str) -> Option<LocalInput> {
	let (mode, prefix, code) = split_local(text)?;
	if code.is_empty() {
		return None;
	}
	Some(LocalInput { mode, code: Str::new(code), exclude: prefix == 2 })
}

/// Max gap between two spaces for the later one to count as OS auto-repeat
/// (pi `SPACE_REPEAT_MAX_GAP_MS`).
pub const SPACE_REPEAT_MAX_GAP: Duration = Duration::from_millis(120);
/// Absolute jitter floor between two mechanical gaps (pi
/// `SPACE_REPEAT_JITTER_MS`).
pub const SPACE_REPEAT_JITTER: Duration = Duration::from_millis(18);
/// Proportional jitter tolerance for slower repeat rates (pi
/// `SPACE_REPEAT_JITTER_RATIO`).
pub const SPACE_REPEAT_JITTER_RATIO: f64 = 0.35;
/// Consecutive mechanical gaps that confirm a held bar (pi
/// `SPACE_HOLD_MECHANICAL_RUN`).
pub const SPACE_HOLD_MECHANICAL_RUN: u8 = 2;
/// Idle gap after the last repeated space that counts as release (pi
/// `SPACE_HOLD_RELEASE_MS`).
pub const SPACE_HOLD_RELEASE: Duration = Duration::from_millis(250);

/// What the space-hold detector decided about one key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpaceHoldEvent {
	/// Not part of a hold: the key reaches the editor as usual.
	Pass,
	/// A repeat inside a recognized hold (or a pre-burst space already
	/// typed): swallowed.
	Swallow,
	/// The bar is held: delete `track_back` pre-burst spaces and start
	/// recording.
	Begin {
		/// Spaces already inserted before the cadence was recognized.
		track_back: usize,
	},
	/// A non-space key arrived during a hold: stop recording, then let the
	/// key through.
	EndThenPass,
}

/// pi `#handleSpaceHold`: recognizes a held space bar from the metronomic
/// OS auto-repeat cadence, never from taps or smashing.
#[derive(Clone, Copy, Debug, Default)]
pub struct SpaceHold {
	active:         bool,
	last_space:     Option<Duration>,
	prev_gap:       Option<Duration>,
	mechanical_run: u8,
	inserted:       usize,
}

/// Whether two consecutive inter-space gaps look machine-driven.
fn gaps_are_mechanical(gap: Duration, prev: Duration) -> bool {
	if gap > SPACE_REPEAT_MAX_GAP || prev > SPACE_REPEAT_MAX_GAP {
		return false;
	}
	let smaller = gap.min(prev).as_secs_f64() * SPACE_REPEAT_JITTER_RATIO;
	let tolerance = SPACE_REPEAT_JITTER.as_secs_f64().max(smaller);
	(gap.as_secs_f64() - prev.as_secs_f64()).abs() <= tolerance
}

impl SpaceHold {
	/// Whether a recording is in progress.
	#[must_use]
	pub const fn active(&self) -> bool {
		self.active
	}

	/// Observes one key at `now`. `enabled` gates recognition (the setting
	/// and the autocomplete popup); a hold already in progress still ends.
	pub fn observe(&mut self, key: Key, now: Duration, enabled: bool) -> SpaceHoldEvent {
		let is_space = matches!(key, Key::Space | Key::Char(' '));
		if self.active {
			if is_space {
				self.last_space = Some(now);
				return SpaceHoldEvent::Swallow;
			}
			self.end();
			return SpaceHoldEvent::EndThenPass;
		}
		if !is_space {
			self.reset_run();
			return SpaceHoldEvent::Pass;
		}
		if !enabled {
			return SpaceHoldEvent::Pass;
		}
		let gap = self.last_space.map(|last| now.saturating_sub(last));
		let prev = self.prev_gap;
		self.last_space = Some(now);
		self.prev_gap = gap;
		let mechanical = match (gap, prev) {
			(Some(gap), Some(prev)) => gaps_are_mechanical(gap, prev),
			_ => false,
		};
		if !mechanical {
			// First space, a deliberate tap, or jittery smashing: a real space.
			self.mechanical_run = 0;
			self.inserted += 1;
			return SpaceHoldEvent::Pass;
		}
		self.mechanical_run += 1;
		if self.mechanical_run >= SPACE_HOLD_MECHANICAL_RUN {
			let track_back = self.inserted;
			self.reset_run();
			self.active = true;
			self.last_space = Some(now);
			return SpaceHoldEvent::Begin { track_back };
		}
		SpaceHoldEvent::Swallow
	}

	/// Whether the release idle gap elapsed at `now`; ends the hold when so.
	pub fn release_due(&mut self, now: Duration) -> bool {
		let due = self.active
			&& self
				.last_space
				.is_some_and(|last| now.saturating_sub(last) >= SPACE_HOLD_RELEASE);
		if due {
			self.end();
		}
		due
	}

	/// Host-clock deadline for the release check.
	#[must_use]
	pub fn next_wake(&self) -> Option<Duration> {
		self.active.then_some(self.last_space? + SPACE_HOLD_RELEASE)
	}

	/// Ends a hold unconditionally (toggle, interrupt).
	pub fn end(&mut self) {
		self.active = false;
		self.reset_run();
	}

	fn reset_run(&mut self) {
		self.inserted = 0;
		self.mechanical_run = 0;
		self.prev_gap = None;
		self.last_space = None;
	}
}

/// Retained composer chrome: status band plus the borderless editor.
///
/// The hardware caret is the editor's insertion point; the host places the
/// terminal cursor from [`Composer::frame`].
pub struct Composer {
	ui:       Ui,
	width:    u16,
	/// Active chrome shape: the band at rest, the rail while the plan
	/// Director is engaged.
	shape:    ComposerStyle,
	/// Whether the host paints a status/notice row directly above the
	/// composer (drives pi's `EditorTopGap`).
	occupied: bool,
	/// IME-safe caret-row layout (`cl_ime_safe_cursor`).
	ime_safe: bool,
	/// Native spelling gates applied to the editor (`cl_spelling_*`).
	spelling: SpellingFeatures,
	/// Dropdown, emoji, and large-paste knobs (`cl_autocomplete_max_visible`,
	/// `cl_emoji_autocomplete`, `cl_paste_large_menu_threshold`).
	settings: ComposerSettings,
	/// Prompt action accepted from the `#` menu, applied after the key.
	pending:  Rc<Cell<Option<PromptAction>>>,
}

impl Composer {
	/// Creates a focused composer at `width` for the launch facts, with the
	/// slash `roster`, `scheme://` completion from `urls`, and `@` file
	/// completion under `project_root`.
	#[must_use]
	pub fn new(
		width: u16,
		ctx: UiContext,
		facts: StatusFacts,
		roster: Vec<Command>,
		urls: UrlCompleter,
		project_root: Option<&Path>,
	) -> Self {
		let actions = PromptActions::new();
		let pending = actions.slot();
		let chain = composer_chain(roster, actions, urls, project_root);
		let shape = ComposerStyle::Borderless;
		let mut ui = composer_ui(facts, shape, width, ctx);
		ui.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| {
			pane.set_completion(Box::new(chain));
			// The chrome recolors on the composer's own sigil grammar, not
			// on a bare leading byte (pi `isPythonMode` after
			// `pythonCommandPrefixLength` + the pasted-shell-prompt guard).
			pane.set_prefix_classifier(prefix_accent_of);
			pane.set_inline_decorator(Some(Box::new(queue_shorthand_spans)));
		});
		ui.focus_first();
		Self {
			ui,
			width,
			shape,
			occupied: false,
			ime_safe: false,
			spelling: SpellingFeatures::default(),
			settings: ComposerSettings::default(),
			pending,
		}
	}

	/// Applies the dropdown/emoji/large-paste knobs; returns whether they
	/// changed.
	pub fn set_settings(&mut self, settings: ComposerSettings) -> bool {
		if self.settings == settings {
			return false;
		}
		self.settings = settings;
		self.ui.update_component::<EditorPane>(COMPOSER_ID, |pane| {
			let options = pane.editor_options();
			pane.set_editor_options(EditorOptions {
				emoji: settings.emoji_autocomplete,
				picker_rows: usize::try_from(settings.autocomplete_max_visible).unwrap_or(0),
				..options
			});
			true
		});
		true
	}

	/// The dropdown/emoji/large-paste knobs currently applied.
	#[must_use]
	pub const fn settings(&self) -> ComposerSettings {
		self.settings
	}

	/// The editor's chrome accent for the current draft (`None` is prose).
	#[must_use]
	pub fn prefix_accent(&self) -> Option<PrefixAccent> {
		self
			.ui
			.with_component::<EditorPane, _>(COMPOSER_ID, EditorPane::prefix_accent)
			.flatten()
	}

	/// Tells the composer whether a status/notice row is painted directly
	/// above it (pi `statusRowOccupied`); the band then sits flush under it.
	/// Returns whether the top gap changed.
	pub fn set_status_row_occupied(&mut self, occupied: bool) -> bool {
		if self.occupied == occupied {
			return false;
		}
		self.occupied = occupied;
		self.sync_gap()
	}

	/// Applies the native spelling gates (`cl_spelling_*`); returns whether
	/// they changed.
	pub fn set_spelling_features(&mut self, features: SpellingFeatures) -> bool {
		if self.spelling == features {
			return false;
		}
		self.spelling = features;
		self.ui.update_component::<EditorPane>(COMPOSER_ID, |pane| {
			pane.set_spelling_features(features);
			true
		});
		true
	}

	/// Native spelling gates currently applied to the editor.
	#[must_use]
	pub const fn spelling_features(&self) -> SpellingFeatures {
		self.spelling
	}

	/// Seeds Up/Down prompt recall with `prompts`, newest first (pi
	/// `setHistoryStorage` on a resumed session).
	pub fn seed_history(&mut self, prompts: impl IntoIterator<Item = Str>) {
		self
			.ui
			.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| pane.seed_history(prompts));
	}

	/// Toggles pi's IME-safe cursor layout (`cl_ime_safe_cursor`); returns
	/// whether it changed.
	pub fn set_ime_safe_cursor(&mut self, enabled: bool) -> bool {
		if self.ime_safe == enabled {
			return false;
		}
		self.ime_safe = enabled;
		self.ui.update_component::<EditorPane>(COMPOSER_ID, |pane| {
			pane.set_ime_safe_cursor(enabled);
			true
		});
		true
	}

	/// Active chrome shape.
	#[must_use]
	pub const fn shape(&self) -> ComposerStyle {
		self.shape
	}

	/// Switches the chrome shape, re-evaluating the top gap (pi
	/// `syncComposerShape` + `EditorTopGap`); returns whether it changed.
	pub fn set_shape(&mut self, shape: ComposerStyle) -> bool {
		if self.shape == shape {
			return false;
		}
		self.shape = shape;
		self.ui.update_component::<EditorPane>(COMPOSER_ID, |pane| {
			pane.set_composer_style(shape);
			true
		});
		self.sync_gap();
		true
	}

	/// The plan Director engaged (or exited): the composer wears the rail
	/// shape while planning and the band otherwise.
	pub fn set_plan_mode(&mut self, engaged: bool) -> bool {
		self.set_shape(if engaged {
			ComposerStyle::Rail
		} else {
			ComposerStyle::Borderless
		})
	}

	fn sync_gap(&mut self) -> bool {
		let shown = top_gap_shown(self.shape, self.occupied);
		let before = self.ui.height();
		self.ui.set_visible(GAP_ID, shown);
		self.ui.height() != before
	}

	/// Whether the completion dropdown is open (pi routes `Esc` to it before
	/// any global interrupt).
	#[must_use]
	pub fn popup_open(&self) -> bool {
		self
			.ui
			.with_component::<EditorPane, _>(COMPOSER_ID, EditorPane::popup_open)
			.unwrap_or(false)
	}

	/// Replaces the draft, leaving the caret at its end.
	pub fn set_text(&mut self, text: &str) {
		self.ui.set_text(COMPOSER_ID, text);
		self.ui.resize(self.width);
	}

	/// Clears the draft.
	pub fn clear(&mut self) {
		self.set_text("");
	}

	/// Current unsent draft in its submitted form: every collapsed paste or
	/// attachment chip is expanded to its full text (pi expands `[Paste #N]`
	/// markers before handing the draft to `$EDITOR` or the model).
	#[must_use]
	pub fn text(&self) -> String {
		self
			.ui
			.values()
			.get(COMPOSER_ID)
			.and_then(serde_json::Value::as_str)
			.map(str::to_owned)
			.unwrap_or_default()
	}

	/// Draft as displayed: chips stay collapsed to their `<icon> #N` markers.
	#[must_use]
	pub fn text_displayed(&self) -> String {
		self
			.ui
			.with_component::<EditorPane, _>(COMPOSER_ID, |pane| pane.displayed_text().to_owned())
			.unwrap_or_default()
	}

	/// Replaces the draft with text edited outside the composer (pi
	/// `handleExternalEditor`): the chips were expanded into `text`, so the
	/// staged attachment cards are dropped rather than re-collapsed, and the
	/// edited text lands verbatim with the caret at its end.
	pub fn replace_edited(&mut self, text: &str) {
		self
			.ui
			.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| pane.attachments().take());
		self.set_text(text);
	}

	/// Rendered chrome, including the caret.
	#[must_use]
	pub const fn frame(&self) -> &Frame {
		self.ui.frame()
	}

	/// Chrome height in rows at the current width.
	#[must_use]
	pub const fn height(&self) -> u16 {
		self.ui.height()
	}

	/// Inserts sanitized pasted text at the caret, unless the paste is
	/// marker-sized and reaches `cl_paste_large_menu_threshold` lines: then
	/// nothing is inserted and the host shows the large-paste menu (pi
	/// `handleLargePaste`), which lands the text through
	/// [`Composer::paste_chip`] once the user chooses.
	pub fn paste(&mut self, text: &str) -> PasteOutcome {
		let threshold = usize::try_from(self.settings.paste_large_menu_lines).unwrap_or(0);
		if threshold > 0 && marker_sized_paste(text) {
			let lines = text.split('\n').count();
			if lines >= threshold {
				return PasteOutcome::Menu { lines };
			}
		}
		let _ = self.ui.handle_paste(text);
		PasteOutcome::Inserted
	}

	/// Stages pasted text as an attachment chip (pi `insertTextAttachment`):
	/// the buffer holds a compact token and the submitted form is
	/// `expansion` (default: the text itself). The large-paste menu's
	/// "wrapped block" choice passes the `<attachment>`-wrapped text.
	pub fn paste_chip(&mut self, text: &str, expansion: Option<&str>) {
		let charset = self.ui.context().charset;
		self
			.ui
			.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| {
				pane.stage_text_attachment(text, expansion, charset)
			});
		self.ui.resize(self.width);
	}

	/// Inserts `text` at the caret verbatim as ordinary draft text (a
	/// `local://paste-N.md` reference from the large-paste menu).
	pub fn insert_text(&mut self, text: &str) {
		let _ = self.ui.handle_paste_raw(text);
	}

	/// Inserts pasted text verbatim (pi `app.clipboard.pasteTextRaw`): no
	/// chip collapse, no drop classification, newlines kept.
	pub fn paste_raw(&mut self, text: &str) {
		let _ = self.ui.handle_paste_raw(text);
	}

	/// Whether the draft is in a `!` (bash) or `$` (eval) prefix mode (pi
	/// `isBashMode` / `isPythonMode`).
	#[must_use]
	pub fn prefix_mode(&self) -> Option<PrefixMode> {
		prefix_mode_of(&self.text())
	}

	/// Current composer line, for `cl_copy_line`.
	#[must_use]
	pub fn current_line(&self) -> Str {
		self
			.ui
			.with_component::<EditorPane, _>(COMPOSER_ID, |pane| Str::new(pane.current_line()))
			.unwrap_or_default()
	}

	/// Deletes `count` graphemes before the caret (space-hold track-back).
	pub fn delete_before_caret(&mut self, count: usize) {
		for _ in 0..count {
			let _ = self.ui.handle_key(Key::Backspace);
		}
	}

	/// Applies one terminal key.
	pub fn key(&mut self, key: Key) -> ComposerAction {
		let (event, claimed) = self.ui.handle_key_claimed(key);
		if let Some(action) = self.pending.take() {
			return self.apply_prompt_action(action);
		}
		match event {
			UiEvent::Submit => self.take_submission(),
			UiEvent::Copied(text) => ComposerAction::Copy(text),
			_ if claimed => ComposerAction::Changed,
			_ => ComposerAction::Ignored,
		}
	}

	/// Submits the draft: clears the editor, drops staged paste chips, records
	/// the line for Up/Down recall (pi `addToHistory`, skipping secret-bearing
	/// commands), and classifies it as a prompt or a `/` console statement.
	/// `Ignored` when the draft is blank.
	pub fn take_submission(&mut self) -> ComposerAction {
		let text = self.text();
		if text.trim().is_empty() {
			return ComposerAction::Ignored;
		}
		self.ui.set_text(COMPOSER_ID, "");
		// Chips leave the band with the draft: a large paste already expanded
		// into the text, an image expanded into its `[Image #N]` marker and
		// travels beside it as the N-th source.
		let staged = self
			.ui
			.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| pane.attachments().take())
			.unwrap_or_default();
		if !staged.is_empty() {
			self.ui.resize(self.width);
		}
		let (markers, images): (Vec<usize>, Vec<Str>) = staged
			.into_iter()
			.filter_map(|attachment| match attachment.content {
				AttachmentContent::Image { source, .. } => Some((attachment.marker, source)),
				AttachmentContent::Text { .. } => None,
			})
			.unzip();
		let text = compact_image_markers(&text, &markers);
		if !should_skip_history(text.trim_start()) {
			self
				.ui
				.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| pane.add_to_history(&text));
		}
		// pi `parseQueueShorthand` runs before slash commands: `-> body`
		// queues `body` for the next yield.
		if let Some(body) = parse_queue_shorthand(&text) {
			return ComposerAction::Queue { text: Str::new(body), images };
		}
		// pi: a leading `/` line is a command, never a prompt.
		match text.trim_start().strip_prefix('/') {
			Some(command) if !command.starts_with('/') => {
				ComposerAction::Command(Str::new(command.trim()))
			},
			_ if !images.is_empty() => {
				ComposerAction::SubmitWithImages { text: Str::new(text), images }
			},
			_ => ComposerAction::Submit(Str::new(text)),
		}
	}

	/// Runs an accepted `#` prompt action against the editor.
	fn apply_prompt_action(&mut self, action: PromptAction) -> ComposerAction {
		match action {
			PromptAction::CopyLine => {
				let line = self
					.ui
					.with_component::<EditorPane, _>(COMPOSER_ID, |pane| Str::new(pane.current_line()))
					.unwrap_or_default();
				ComposerAction::Copy(line)
			},
			PromptAction::CopyPrompt => ComposerAction::Copy(Str::new(self.text())),
			PromptAction::Undo { transient } => {
				self
					.ui
					.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| {
						pane.undo_past_transient(&transient);
					});
				ComposerAction::Changed
			},
			PromptAction::MessageEnd | PromptAction::MessageStart => {
				let end = action == PromptAction::MessageEnd;
				self
					.ui
					.with_component_mut::<EditorPane, _>(COMPOSER_ID, |pane| {
						pane.move_to_message_edge(end);
					});
				ComposerAction::Changed
			},
			PromptAction::LineStart => {
				self.ui.handle_key(Key::Home);
				ComposerAction::Changed
			},
			PromptAction::LineEnd => {
				self.ui.handle_key(Key::End);
				ComposerAction::Changed
			},
		}
	}

	/// Reflows the chrome for a new terminal size: the editor grows with its
	/// content up to [`editor_max_rows`] of `height`.
	pub fn resize(&mut self, width: u16, height: u16) {
		self.width = width;
		let rows = editor_max_rows(height);
		self.ui.update_component::<EditorPane>(COMPOSER_ID, |pane| {
			pane.set_max_rows(rows);
			true
		});
		self.ui.resize(width);
	}

	/// Replaces the presentation context (theme, charset, terminal caps).
	pub fn set_context(&mut self, ctx: UiContext) {
		self.ui.set_context(ctx);
	}

	/// Updates the status band; returns whether it repainted.
	pub fn set_status(&mut self, facts: StatusFacts) -> bool {
		self
			.ui
			.update_component::<StatusBand>(STATUS_ID, |band| band.set_facts(facts))
	}

	/// Advances chrome animations (the working spinner).
	pub fn tick(&mut self, now: Duration) -> bool {
		self.ui.tick(now)
	}

	/// Next animation deadline, if any component asked to be woken.
	#[must_use]
	pub fn next_wake(&self) -> Option<Duration> {
		self.ui.next_wake()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn facts() -> StatusFacts {
		StatusFacts {
			model: Str::new_static("Sonnet 4.5"),
			thinking: None,
			cwd: Str::new_static("~/proj"),
			scratch: false,
			branch: None,
			tokens: 0,
			context_window: Some(200_000),
			compact_percent: 80,
			working: None,
			..StatusFacts::default()
		}
	}

	fn no_urls() -> UrlCompleter {
		std::sync::Arc::new(|_scheme: &str| None)
	}

	fn composer() -> Composer {
		Composer::new(
			60,
			UiContext::default(),
			facts(),
			vec![Command::new("help", "Shows a name's description", &[])],
			no_urls(),
			None,
		)
	}

	fn type_text(composer: &mut Composer, text: &str) {
		for character in text.chars() {
			composer.key(Key::Char(character));
		}
	}

	fn rows(composer: &Composer) -> Vec<String> {
		omp_tui::frame_text(composer.frame())
			.lines()
			.map(|line| line.trim_end().to_owned())
			.collect()
	}

	#[test]
	fn typing_moves_the_caret_and_enter_submits_then_clears() {
		let mut composer = composer();
		let (column, row) = composer.frame().cursor().expect("caret placed at boot");
		assert_eq!((column, row), (3, 2));
		for character in "hi".chars() {
			assert_eq!(composer.key(Key::Char(character)), ComposerAction::Changed);
		}
		assert_eq!(composer.text(), "hi");
		assert_eq!(composer.frame().cursor(), Some((5, 2)));
		// pi `band` shape: `╰─ ` gutter at column 0, paddingX 0, no frame.
		assert_eq!(rows(&composer)[2], "╰─ hi");
		assert_eq!(composer.key(Key::Enter), ComposerAction::Submit(Str::new_static("hi")));
		assert_eq!(composer.text(), "");
		assert_eq!(composer.frame().cursor(), Some((3, 2)));
	}

	/// pi `addToHistory` + `navigateHistory`: every submission (prompt,
	/// `/command`, `!shell`) is recalled by Up on an empty draft; Down walks
	/// back to the draft the user was writing.
	#[test]
	fn submissions_are_recalled_by_up_and_down_on_the_draft_edges() {
		let mut composer = composer();
		composer.key(Key::Up);
		assert_eq!(composer.text(), "", "nothing to recall");
		type_text(&mut composer, "first prompt");
		assert!(matches!(composer.key(Key::Enter), ComposerAction::Submit(_)));
		type_text(&mut composer, "/help");
		assert!(matches!(composer.key(Key::Enter), ComposerAction::Command(_)));
		type_text(&mut composer, "!ls");
		assert!(matches!(composer.key(Key::Enter), ComposerAction::Submit(_)));
		assert_eq!(composer.text(), "");
		assert_eq!(composer.key(Key::Up), ComposerAction::Changed);
		assert_eq!(composer.text(), "!ls");
		assert_eq!(composer.key(Key::Up), ComposerAction::Changed);
		assert_eq!(composer.text(), "/help");
		assert_eq!(composer.key(Key::Up), ComposerAction::Changed);
		assert_eq!(composer.text(), "first prompt");
		composer.key(Key::Up);
		assert_eq!(composer.text(), "first prompt", "oldest entry");
		composer.key(Key::End);
		composer.key(Key::Down);
		composer.key(Key::Down);
		assert_eq!(composer.key(Key::Down), ComposerAction::Changed);
		assert_eq!(composer.text(), "", "back to the empty draft");
		// A non-empty draft keeps Up for the host (transcript scrolling).
		type_text(&mut composer, "draft");
		composer.key(Key::Up);
		assert_eq!(composer.text(), "draft");
		// Recalling then submitting re-records without duplicates.
		composer.clear();
		composer.key(Key::Up);
		assert!(matches!(composer.key(Key::Enter), ComposerAction::Submit(_)));
		composer.key(Key::Up);
		assert_eq!(composer.text(), "!ls");
		composer.key(Key::Up);
		assert_eq!(composer.text(), "/help");
	}

	/// pi `shouldSkipHistory`: secret-bearing commands never enter recall.
	#[test]
	fn secret_bearing_commands_are_kept_out_of_history() {
		assert!(should_skip_history("/login https://x?code=abc&state=1"));
		assert!(should_skip_history("/login:?code=abc"));
		assert!(should_skip_history("/join room-link"));
		assert!(should_skip_history("/mcp add --token abc srv"));
		assert!(!should_skip_history("/login"));
		assert!(!should_skip_history("/mcp add srv"));
		assert!(!should_skip_history("/mcp list --tokens"));
		assert!(!should_skip_history("login secret"));
		let mut composer = composer();
		type_text(&mut composer, "/login secret-code");
		assert!(matches!(composer.key(Key::Enter), ComposerAction::Command(_)));
		type_text(&mut composer, "safe");
		assert!(matches!(composer.key(Key::Enter), ComposerAction::Submit(_)));
		composer.key(Key::Up);
		assert_eq!(composer.text(), "safe");
		composer.key(Key::Up);
		assert_eq!(composer.text(), "safe", "the login line is absent");
	}

	/// pi `setHistoryStorage`: a resumed session seeds recall newest first.
	#[test]
	fn seeded_history_is_recalled_newest_first() {
		let mut composer = composer();
		composer.seed_history([Str::new_static("newest"), Str::new_static("older")]);
		assert_eq!(composer.key(Key::Up), ComposerAction::Changed);
		assert_eq!(composer.text(), "newest");
		assert_eq!(composer.key(Key::Up), ComposerAction::Changed);
		assert_eq!(composer.text(), "older");
	}

	/// The `cl_spelling_*` convars reach the live editor.
	#[test]
	fn spelling_features_apply_to_the_editor_and_report_change() {
		let mut composer = composer();
		assert_eq!(composer.spelling_features(), SpellingFeatures::default());
		let off =
			SpellingFeatures { typo_detection: false, autocomplete: false, autocorrect: false };
		assert!(composer.set_spelling_features(off));
		assert!(!composer.set_spelling_features(off), "unchanged");
		assert_eq!(composer.spelling_features(), off);
		let pane = composer
			.ui
			.with_component::<EditorPane, _>(COMPOSER_ID, EditorPane::active_spelling_features)
			.expect("composer pane");
		assert_eq!(pane, off);
	}

	/// pi `useTerminalCursor`: the caret cell is never painted as a block;
	/// only the frame's hardware cursor moves.
	#[test]
	fn caret_cell_stays_unstyled_while_typing() {
		let mut composer = composer();
		for character in "hi".chars() {
			composer.key(Key::Char(character));
		}
		let frame = composer.frame();
		let (column, row) = frame.cursor().expect("caret placed");
		let theme = UiContext::default().theme;
		for x in 0..frame.size().width {
			assert_ne!(
				frame.cell(x, row).style().background_color(),
				theme.accent,
				"column {x} paints a software caret; hardware caret is at {column}"
			);
		}
	}

	#[test]
	fn slash_opens_the_command_popup_below_the_prompt_and_enter_runs_it() {
		let mut composer = composer();
		assert!(!composer.popup_open());
		assert_eq!(composer.key(Key::Char('/')), ComposerAction::Changed);
		assert!(composer.popup_open(), "slash opens the roster");
		let rows = rows(&composer);
		let prompt = rows
			.iter()
			.position(|row| row.starts_with("╰─ /"))
			.expect("prompt row");
		assert!(rows[prompt + 1].contains("help"), "{rows:?}");
		assert!(rows[prompt + 1].contains("Shows a name's description"), "{rows:?}");
		assert_eq!(composer.key(Key::Esc), ComposerAction::Changed);
		assert!(!composer.popup_open(), "esc closes the popup");
		for character in "help".chars() {
			composer.key(Key::Char(character));
		}
		assert_eq!(composer.key(Key::Enter), ComposerAction::Command(Str::new_static("help")));
		assert_eq!(composer.text(), "");
	}

	#[test]
	fn hash_menu_runs_prompt_actions_and_removes_the_trigger() {
		let mut composer = composer();
		for character in "hello world".chars() {
			composer.key(Key::Char(character));
		}
		composer.key(Key::Home);
		composer.key(Key::Char('#'));
		assert!(composer.popup_open(), "# opens prompt actions");
		let rows = rows(&composer);
		assert!(rows.iter().any(|row| row.contains("Copy current line")), "{rows:?}");
		// pi: a space ends the `#query` token, so the query is one word.
		for character in "msgend".chars() {
			composer.key(Key::Char(character));
		}
		assert_eq!(composer.key(Key::Tab), ComposerAction::Changed);
		assert_eq!(composer.text(), "hello world", "the #query token is removed");
		assert_eq!(composer.frame().cursor(), Some((3 + 11, 2)), "caret moved to the message end");
		for character in " #copywhole".chars() {
			composer.key(Key::Char(character));
		}
		assert_eq!(
			composer.key(Key::Tab),
			ComposerAction::Copy(Str::new_static("hello world ")),
			"copy prompt reports the draft without the trigger"
		);
		assert_eq!(composer.text(), "hello world ");
	}

	#[test]
	fn at_lists_project_files_and_accepts_with_a_trailing_space() {
		let root = tempfile::tempdir().expect("scratch project");
		std::fs::write(root.path().join("note.txt"), "hi").expect("fixture");
		std::fs::create_dir(root.path().join("src")).expect("fixture dir");
		let mut composer =
			Composer::new(60, UiContext::default(), facts(), Vec::new(), no_urls(), Some(root.path()));
		composer.key(Key::Char('@'));
		let deadline = std::time::Instant::now() + Duration::from_secs(5);
		while !composer.popup_open() && std::time::Instant::now() < deadline {
			std::thread::sleep(Duration::from_millis(10));
			// The index lands asynchronously; a caret motion re-queries it.
			composer.key(Key::Left);
			composer.key(Key::Right);
		}
		assert!(composer.popup_open(), "@ lists the indexed project");
		for character in "no".chars() {
			composer.key(Key::Char(character));
		}
		assert_eq!(composer.key(Key::Tab), ComposerAction::Changed);
		assert_eq!(composer.text(), "@note.txt ");
	}

	#[test]
	fn colon_opens_the_builtin_emoji_popup() {
		let mut composer = composer();
		for character in ":joy".chars() {
			composer.key(Key::Char(character));
		}
		assert!(composer.popup_open(), "emoji dropdown");
		assert!(rows(&composer).iter().any(|row| row.contains("joy")));
	}

	#[test]
	fn set_text_and_clear_replace_the_draft_with_the_caret_at_the_end() {
		let mut composer = composer();
		composer.set_text("draft");
		assert_eq!(composer.text(), "draft");
		assert_eq!(composer.frame().cursor(), Some((8, 2)));
		composer.clear();
		assert_eq!(composer.text(), "");
		assert_eq!(composer.frame().cursor(), Some((3, 2)));
	}

	#[test]
	fn empty_enter_is_ignored_and_status_updates_repaint() {
		let mut composer = composer();
		assert!(!matches!(composer.key(Key::Enter), ComposerAction::Submit(_)));
		let working = Some(Duration::ZERO);
		assert!(composer.set_status(StatusFacts { working, ..facts() }));
		assert!(!composer.set_status(StatusFacts { working, ..facts() }));
		assert!(composer.next_wake().is_some(), "spinner schedules a wake");
	}

	/// Plan mode swaps the band for the rail and back; pi `EditorTopGap`
	/// keeps the one-row gap for the rail and collapses it for the band
	/// only while a status row is painted directly above.
	#[test]
	fn plan_mode_switches_the_shape_and_the_top_gap() {
		let mut composer = composer();
		for character in "hi".chars() {
			composer.key(Key::Char(character));
		}
		assert_eq!(composer.shape(), ComposerStyle::Borderless);
		assert_eq!(rows(&composer)[0], "", "band at rest keeps the gap row");
		assert_eq!(rows(&composer)[2], "╰─ hi");
		assert!(composer.set_status_row_occupied(true), "band under a notice: gap collapses");
		assert_eq!(rows(&composer)[1], "╰─ hi");
		assert!(!composer.set_status_row_occupied(true));

		assert!(composer.set_plan_mode(true));
		assert!(!composer.set_plan_mode(true), "already engaged");
		assert_eq!(composer.shape(), ComposerStyle::Rail);
		let railed = rows(&composer);
		assert_eq!(railed[0], "", "rail keeps the top gap even under a notice");
		assert!(
			railed
				.iter()
				.any(|row| row.starts_with('▎') && row.contains("hi")),
			"{railed:?}"
		);
		assert_eq!(composer.text(), "hi", "the draft survives the reshape");

		assert!(composer.set_plan_mode(false));
		assert_eq!(rows(&composer)[1], "╰─ hi", "band again, notice still up: flush");
		assert!(composer.set_status_row_occupied(false));
		assert_eq!(rows(&composer)[2], "╰─ hi", "notice gone: the gap returns");
	}

	/// pi `computeEditorMaxHeight`, then the composer grows with content up
	/// to that budget.
	#[test]
	fn editor_height_budget_follows_pi_and_caps_growth() {
		assert_eq!(editor_max_rows(40), 18);
		assert_eq!(editor_max_rows(24), 12);
		assert_eq!(editor_max_rows(10), 6);
		assert_eq!(editor_max_rows(6), 3);
		assert_eq!(editor_max_rows(0), 12, "unknown size falls back to 24 rows");

		let mut composer = composer();
		let base = composer.height();
		composer.paste_raw("a\nb\nc\nd");
		assert_eq!(composer.height(), base + 3, "four lines grow the editor by three rows");
		composer.resize(60, 10);
		// Budget 6 rows: the status band, the four content rows, and picker
		// room all fit; a 20-line draft is clamped instead.
		composer.paste_raw(&"\nx".repeat(20));
		assert!(
			composer.height() <= base + 6,
			"height {} exceeds the small-terminal budget",
			composer.height()
		);
		composer.resize(60, 40);
		assert!(composer.height() > base + 6, "a roomy terminal lets the draft grow again");
	}

	/// pi `handleExternalEditor`: the draft handed to `$EDITOR` expands every
	/// chip; the edited text comes back verbatim with the cards dropped.
	#[test]
	fn external_editor_round_trip_expands_chips_and_lands_verbatim() {
		let mut composer = composer();
		let paste = (0..12)
			.map(|n| format!("line{n}"))
			.collect::<Vec<_>>()
			.join("\n");
		composer.paste(&paste);
		for character in "tail".chars() {
			composer.key(Key::Char(character));
		}
		let displayed = composer.text_displayed();
		assert!(displayed.contains("#1 tail"), "{displayed}");
		assert!(!displayed.contains("line0"), "the chip stays collapsed on screen");
		let expanded = composer.text();
		assert_eq!(expanded, format!("{paste} tail"), "the editor draft carries the paste");
		assert!(rows(&composer).iter().any(|row| row.contains("#1 ───")), "card band shown");

		let edited = format!("{expanded}\nedited");
		composer.replace_edited(&edited);
		assert_eq!(composer.text(), edited, "verbatim replacement, nothing re-collapsed");
		assert_eq!(composer.text_displayed(), edited);
		let after = rows(&composer);
		assert!(!after.iter().any(|row| row.contains("#1 ───")), "the attachment card band is gone");
		assert!(after.iter().any(|row| row.contains("line0")), "the expanded lines are editable");
	}

	#[test]
	fn prefix_lines_parse_like_pi_and_prose_stays_prose() {
		let bash = parse_local_input("  !echo hi").expect("bash");
		assert_eq!(bash, LocalInput {
			mode:    PrefixMode::Bash,
			code:    "echo hi".into(),
			exclude: false,
		});
		let hidden = parse_local_input("!! ls -la ").expect("excluded bash");
		assert_eq!(hidden.code, "ls -la");
		assert!(hidden.exclude);
		let eval = parse_local_input("$ 1+1").expect("eval");
		assert_eq!(eval, LocalInput {
			mode:    PrefixMode::Eval,
			code:    "1+1".into(),
			exclude: false,
		});
		let hidden_eval = parse_local_input("$$\tprint(2)").expect("excluded eval");
		assert!(hidden_eval.exclude && hidden_eval.mode == PrefixMode::Eval);
		// A bare sigil runs nothing; shell-style variables and `${…}` are prose.
		assert_eq!(parse_local_input("!"), None);
		assert_eq!(parse_local_input("$ "), None);
		assert_eq!(parse_local_input("$HOME is set"), None);
		assert_eq!(parse_local_input("${x} costs $5"), None);
		assert_eq!(prefix_mode_of("$HOME"), None);
		assert_eq!(prefix_mode_of("$"), Some(PrefixMode::Eval));
		assert_eq!(prefix_mode_of("!"), Some(PrefixMode::Bash));
	}

	/// pi `looksLikePastedShellPrompt`: every branch of the three regexes.
	#[test]
	fn pasted_shell_prompts_behind_a_single_dollar_stay_prose() {
		// SHELL_PROMPT_COMMAND_RE: path forms and the command roster.
		for line in [
			"$ cd ~/project && cargo test",
			"$ git status",
			"$ ./run.sh",
			"$ ../scripts/build",
			"$ /usr/bin/env",
			"$ ~/bin/tool --flag",
			"$ sudo make install",
			"$ python3 -m venv .venv",
			"$ python",
			"$ kubectl get pods",
			"$ cd",
			// SHELL_PROMPT_OPERATOR_RE: standalone operators anywhere on the first line.
			"$ cat a | sort",
			"$ a || b",
			"$ run 2>&1",
			"$ echo hi > out.txt",
			"$ prog << EOF",
			"$ cmd < input",
			// OMP_STATUS_LINE_RE: a pasted omp status line on any line.
			"$ first\nin: 12 out: 34 t: 1.2s tok/s: 40",
			"$ first\n  in: 12 out: 34 cache 5% t: 1.2s tok/s: 40",
		] {
			assert_eq!(parse_local_input(line), None, "{line:?} must stay a prompt");
			assert_eq!(prefix_mode_of(line), None, "{line:?} must not paint eval mode");
		}
		// The guard is about shell shapes, not tokens inside Python.
		for line in ["$ print('cd')", "$ gitlab = 1", "$ python_version()", "$ x|y", "$ 1<2"] {
			let parsed = parse_local_input(line).unwrap_or_else(|| panic!("{line:?} is Python"));
			assert_eq!(parsed.mode, PrefixMode::Eval);
			assert_eq!(prefix_mode_of(line), Some(PrefixMode::Eval));
		}
		// `$$` is explicit: pi skips the guard for the excluded form.
		let forced = parse_local_input("$$ git status").expect("explicit eval");
		assert!(forced.exclude);
		assert_eq!(forced.code, "git status");
		assert_eq!(prefix_mode_of("$$ git status"), Some(PrefixMode::Eval));
		// Only the first line decides the command/operator shape.
		assert!(parse_local_input("$ x = 1\ncd home").is_some());
		assert!(looks_like_pasted_shell_prompt("cd home\nx = 1"));
	}

	/// The editor chrome recolors on the composer's grammar, not on a bare
	/// leading `$`: shell variables and pasted shell prompts stay prose
	/// (pi `isPythonMode` after `pythonCommandPrefixLength` + the guard).
	#[test]
	fn editor_chrome_follows_the_composer_sigil_grammar() {
		let info = UiContext::default().theme.info;
		let warn = UiContext::default().theme.warn;
		let gutter = |composer: &Composer| composer.frame().cell(0, 2).style().foreground_color();
		for (draft, accent) in [
			("$HOME is set", None),
			("${x} costs $5", None),
			("$ git status", None),
			("$ cd ~/project && cargo test", None),
			("$ 1+1", Some(PrefixAccent::Eval)),
			("$$ git status", Some(PrefixAccent::Eval)),
			("!ls", Some(PrefixAccent::Bash)),
			("plain prose", None),
		] {
			let mut composer = composer();
			composer.set_text(draft);
			assert_eq!(composer.prefix_accent(), accent, "{draft:?}");
			let painted = gutter(&composer);
			match accent {
				Some(PrefixAccent::Eval) => assert_eq!(painted, info, "{draft:?}"),
				Some(PrefixAccent::Bash) => assert_eq!(painted, warn, "{draft:?}"),
				None => {
					assert_ne!(painted, info, "{draft:?} must not paint eval mode");
					assert_ne!(painted, warn, "{draft:?} must not paint bash mode");
				},
			}
		}
	}

	/// pi `parseQueueShorthand`: `->` / `=>` on the trimmed draft queues the
	/// body for the next yield; it wins over `/` classification and the
	/// draft is still recalled by Up.
	#[test]
	fn queue_shorthand_submits_a_queue_action() {
		assert_eq!(parse_queue_shorthand("-> run tests"), Some("run tests"));
		assert_eq!(parse_queue_shorthand("  =>check logs  "), Some("check logs"));
		assert_eq!(parse_queue_shorthand("->"), Some(""));
		assert_eq!(parse_queue_shorthand("- > nope"), None);
		assert_eq!(parse_queue_shorthand("a -> b"), None);

		let mut composer = composer();
		type_text(&mut composer, "-> /help later");
		assert_eq!(composer.key(Key::Enter), ComposerAction::Queue {
			text:   Str::new_static("/help later"),
			images: Vec::new(),
		});
		assert_eq!(composer.text(), "");
		assert_eq!(composer.key(Key::Up), ComposerAction::Changed);
		assert_eq!(composer.text(), "-> /help later", "history keeps the shorthand");
		composer.clear();
		type_text(&mut composer, "=> second");
		assert_eq!(composer.key(Key::Enter), ComposerAction::Queue {
			text:   Str::new_static("second"),
			images: Vec::new(),
		});
		type_text(&mut composer, "plain -> not shorthand");
		assert!(matches!(composer.key(Key::Enter), ComposerAction::Submit(_)));
	}

	/// pi `#queueForYield(text, { images })`: the yield-queue shorthand
	/// carries the staged image chips with the body, positional against the
	/// markers that survive the stripped sigil.
	#[test]
	fn queue_shorthand_keeps_image_chips() {
		let dir = tempfile::tempdir().expect("tempdir");
		let image = dir.path().join("shot.png");
		std::fs::write(&image, b"\x89PNG\r\n\x1a\n").expect("png");
		let mut composer = composer();
		type_text(&mut composer, "-> ");
		composer.paste(&format!("'{}'", image.display()));
		type_text(&mut composer, "later");
		assert_eq!(composer.key(Key::Enter), ComposerAction::Queue {
			text:   Str::new_static("[Image #1] later"),
			images: vec![Str::new(image.to_string_lossy())],
		});
	}

	/// `cl_autocomplete_max_visible`, `cl_emoji_autocomplete`, and
	/// `cl_paste_large_menu_threshold` reach the live editor through
	/// [`Composer::set_settings`].
	#[test]
	fn settings_reach_the_editor_and_gate_the_paste_menu() {
		let mut composer = composer();
		assert!(!composer.set_settings(ComposerSettings::default()), "defaults are a no-op");
		// Emoji dropdown follows the switch.
		type_text(&mut composer, ":joy");
		assert!(composer.popup_open(), "emoji dropdown opens by default");
		assert!(composer.set_settings(ComposerSettings {
			emoji_autocomplete: false,
			..ComposerSettings::default()
		}));
		assert!(!composer.popup_open(), "the switch closes the built-in dropdown");
		composer.clear();

		// A marker-sized paste at or above the line threshold is held for the
		// menu; below it, or with the menu disabled, it collapses to a chip.
		let twelve = (0..12)
			.map(|n| format!("l{n}"))
			.collect::<Vec<_>>()
			.join("\n");
		composer.set_settings(ComposerSettings {
			paste_large_menu_lines: 12,
			..ComposerSettings::default()
		});
		assert_eq!(composer.paste(&twelve), PasteOutcome::Menu { lines: 12 });
		assert_eq!(composer.text(), "", "the menu holds the text; nothing landed");
		composer.set_settings(ComposerSettings {
			paste_large_menu_lines: 13,
			..ComposerSettings::default()
		});
		assert_eq!(composer.paste(&twelve), PasteOutcome::Inserted);
		assert!(composer.text_displayed().contains("#1"), "below the threshold: chip");
		composer.replace_edited("");
		composer.set_settings(ComposerSettings {
			paste_large_menu_lines: 0,
			..ComposerSettings::default()
		});
		assert_eq!(composer.paste(&twelve), PasteOutcome::Inserted, "0 disables the menu");
		composer.replace_edited("");
		composer.set_settings(ComposerSettings {
			paste_large_menu_lines: 2,
			..ComposerSettings::default()
		});
		assert_eq!(
			composer.paste("a\nb\nc"),
			PasteOutcome::Inserted,
			"a small paste is never marker-sized, whatever the threshold"
		);
		assert_eq!(composer.text(), "a\nb\nc");

		// Menu choices land through `paste_chip` / `insert_text`.
		composer.replace_edited("");
		composer.paste_chip(&twelve, Some(&format!("<attachment>\n{twelve}\n</attachment>")));
		assert!(composer.text_displayed().contains("#1"), "{}", composer.text_displayed());
		assert_eq!(composer.text().trim_end(), format!("<attachment>\n{twelve}\n</attachment>"));
		composer.replace_edited("");
		composer.insert_text("local://paste-1.md");
		assert_eq!(composer.text(), "local://paste-1.md");
	}

	/// pi `compactImageMarkers`: surviving markers renumber densely against
	/// the images actually submitted; dense drafts and foreign brackets stay
	/// untouched.
	#[test]
	fn image_markers_compact_to_the_submitted_image_order() {
		assert_eq!(
			compact_image_markers("[Image #2, 4x3] then [Image #3] x", &[2, 3]),
			"[Image #1, 4x3] then [Image #2] x"
		);
		assert_eq!(compact_image_markers("[Image #1] [Image #2]", &[1, 2]), "[Image #1] [Image #2]");
		assert_eq!(
			compact_image_markers("[Image #9] [Image #] [x]", &[3]),
			"[Image #9] [Image #] [x]"
		);
		assert_eq!(compact_image_markers("no markers", &[]), "no markers");
	}

	/// Dropping an image stages a chip whose submission carries the source
	/// beside pi's wire marker; a text-only draft still submits plainly.
	#[test]
	fn image_chips_submit_their_sources_in_marker_order() {
		let dir = tempfile::tempdir().expect("tempdir");
		let first = dir.path().join("one.png");
		let second = dir.path().join("two.png");
		std::fs::write(&first, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x04\0\0\0\x03").expect("png");
		std::fs::write(&second, b"\x89PNG\r\n\x1a\n").expect("png");
		let mut composer = composer();
		composer.paste(&format!("'{}' '{}'", first.display(), second.display()));
		assert!(composer.text_displayed().contains("#1") && composer.text_displayed().contains("#2"));
		type_text(&mut composer, "compare");
		assert_eq!(composer.key(Key::Enter), ComposerAction::SubmitWithImages {
			text:   Str::new("[Image #1, 4x3] [Image #2] compare"),
			images: vec![Str::new(first.to_string_lossy()), Str::new(second.to_string_lossy())],
		});
		assert_eq!(composer.text(), "");
		type_text(&mut composer, "plain");
		assert_eq!(composer.key(Key::Enter), ComposerAction::Submit(Str::new_static("plain")));
	}
}
