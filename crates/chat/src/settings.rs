//! Literal pi-setting convars not otherwise owned by a narrower runtime module.

use omp_con::Kv;
use omp_core::Str;

omp_con::var! {
	/// pi `colorBlindMode` (boolean, default: false).
	pub static CL_COLOR_BLIND_MODE = cl_color_blind_mode: bool {
		default: false,
		flags: archive,
	};
	/// pi `composer.shape` (string, default: "band").
	pub static CL_COMPOSER_SHAPE = cl_composer_shape: Str {
		default: Str::new_static("band"),
		flags: archive,
	};
	/// pi `statusLine.preset` (enum, default: "default").
	pub static CL_STATUS_LINE_PRESET = cl_status_line_preset: Str {
		default: Str::new_static("default"),
		flags: archive,
	};
	/// pi `statusLine.separator` (enum, default: "powerline-thin").
	pub static CL_STATUS_LINE_SEPARATOR = cl_status_line_separator: Str {
		default: Str::new_static("powerline-thin"),
		flags: archive,
	};
	/// pi `statusLine.contextLine` (enum, default: "embedded").
	pub static CL_STATUS_LINE_CONTEXT_LINE = cl_status_line_context_line: Str {
		default: Str::new_static("embedded"),
		flags: archive,
	};
	/// pi `statusLine.sessionAccent` (boolean, default: true).
	pub static CL_STATUS_LINE_SESSION_ACCENT = cl_status_line_session_accent: bool {
		default: true,
		flags: archive,
	};
	/// pi `statusLine.transparent` (boolean, default: false).
	pub static CL_STATUS_LINE_TRANSPARENT = cl_status_line_transparent: bool {
		default: false,
		flags: archive,
	};
	/// pi `statusLine.showHookStatus` (boolean, default: true).
	pub static CL_STATUS_LINE_SHOW_HOOK_STATUS = cl_status_line_show_hook_status: bool {
		default: true,
		flags: archive,
	};
	/// pi `statusLine.leftSegments` (array, default: [] as StatusLineSegmentId[]).
	pub static CL_STATUS_LINE_LEFT_SEGMENTS = cl_status_line_left_segments: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `statusLine.rightSegments` (array, default: [] as StatusLineSegmentId[]).
	pub static CL_STATUS_LINE_RIGHT_SEGMENTS = cl_status_line_right_segments: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `statusLine.segmentOptions` (record, default: {).
	pub static CL_STATUS_LINE_SEGMENT_OPTIONS = cl_status_line_segment_options: Kv {
		default: Kv::new(),
		flags: archive,
	};
	/// pi `terminal.showImages` (boolean, default: true).
	pub static CL_TERMINAL_SHOW_IMAGES = cl_terminal_show_images: bool {
		default: true,
		flags: archive,
	};
	/// pi `tui.maxInlineImageColumns` (number, default: 100).
	pub static CL_TUI_MAX_INLINE_IMAGE_COLUMNS = cl_tui_max_inline_image_columns: i64 {
		default: 100,
		flags: archive,
	};
	/// pi `tui.maxInlineImageRows` (number, default: 20).
	pub static CL_TUI_MAX_INLINE_IMAGE_ROWS = cl_tui_max_inline_image_rows: i64 {
		default: 20,
		flags: archive,
	};
	/// pi `tui.maxInlineImages` (number, default: 8).
	pub static CL_TUI_MAX_INLINE_IMAGES = cl_tui_max_inline_images: i64 {
		default: 8,
		flags: archive,
	};
	/// pi `terminal.showProgress` (boolean, default: false).
	pub static CL_TERMINAL_SHOW_PROGRESS = cl_terminal_show_progress: bool {
		default: false,
		flags: archive,
	};
	/// pi `tui.textSizing` (boolean, default: false).
	pub static CL_TUI_TEXT_SIZING = cl_tui_text_sizing: bool {
		default: false,
		flags: archive,
	};
	/// pi `tui.renderMermaid` (boolean, default: true).
	pub static CL_TUI_RENDER_MERMAID = cl_tui_render_mermaid: bool {
		default: true,
		flags: archive,
	};
	/// pi `tui.titleState` (boolean, default: true).
	pub static CL_TUI_TITLE_STATE = cl_tui_title_state: bool {
		default: true,
		flags: archive,
	};
	/// pi `tui.hyperlinks` (enum, default: "auto").
	pub static CL_TUI_HYPERLINKS = cl_tui_hyperlinks: Str {
		default: Str::new_static("auto"),
		flags: archive,
	};
	/// pi `tui.tight` (boolean, default: false).
	pub static CL_TUI_TIGHT = cl_tui_tight: bool {
		default: false,
		flags: archive,
	};
	/// pi `display.shimmer` (enum, default: "classic").
	pub static CL_DISPLAY_SHIMMER = cl_display_shimmer: Str {
		default: Str::new_static("classic"),
		flags: archive,
	};
	/// pi `display.showTokenUsage` (boolean, default: false).
	pub static CL_DISPLAY_SHOW_TOKEN_USAGE = cl_display_show_token_usage: bool {
		default: false,
		flags: archive,
	};
	/// pi `display.showTurnTime` (boolean, default: false).
	pub static CL_DISPLAY_SHOW_TURN_TIME = cl_display_show_turn_time: bool {
		default: false,
		flags: archive,
	};
	/// pi `display.cacheMissMarker` (boolean, default: false).
	pub static CL_DISPLAY_CACHE_MISS_MARKER = cl_display_cache_miss_marker: bool {
		default: false,
		flags: archive,
	};
	/// pi `display.collapseCompacted` (boolean, default: true).
	pub static CL_DISPLAY_COLLAPSE_COMPACTED = cl_display_collapse_compacted: bool {
		default: true,
		flags: archive,
	};
	/// pi `showHardwareCursor` (boolean, default: true).
	pub static CL_SHOW_HARDWARE_CURSOR = cl_show_hardware_cursor: bool {
		default: true,
		flags: archive,
	};
	/// pi `steeringMode` (enum, default: "one-at-a-time").
	pub static CL_STEERING_MODE = cl_steering_mode: Str {
		default: Str::new_static("one-at-a-time"),
		flags: archive,
	};
	/// pi `followUpMode` (enum, default: "one-at-a-time").
	pub static CL_FOLLOW_UP_MODE = cl_follow_up_mode: Str {
		default: Str::new_static("one-at-a-time"),
		flags: archive,
	};
	/// pi `interruptMode` (enum, default: "immediate").
	pub static CL_INTERRUPT_MODE = cl_interrupt_mode: Str {
		default: Str::new_static("immediate"),
		flags: archive,
	};
	/// pi `loop.mode` (enum, default: "prompt").
	pub static CL_LOOP_MODE = cl_loop_mode: Str {
		default: Str::new_static("prompt"),
		flags: archive,
	};
	/// pi `treeFilterMode` (enum, default: "default").
	pub static CL_TREE_FILTER_MODE = cl_tree_filter_mode: Str {
		default: Str::new_static("default"),
		flags: archive,
	};
	/// pi `autocompleteMaxVisible` (number, default: 10).
	pub static CL_AUTOCOMPLETE_MAX_VISIBLE = cl_autocomplete_max_visible: i64 {
		default: 10,
		flags: archive,
	};
	/// pi `spelling.typoDetection` (boolean, default: true).
	pub static CL_SPELLING_TYPO_DETECTION = cl_spelling_typo_detection: bool {
		default: true,
		flags: archive,
	};
	/// pi `spelling.autocomplete` (boolean, default: true).
	pub static CL_SPELLING_AUTOCOMPLETE = cl_spelling_autocomplete: bool {
		default: true,
		flags: archive,
	};
	/// pi `spelling.autocorrect` (boolean, default: false).
	pub static CL_SPELLING_AUTOCORRECT = cl_spelling_autocorrect: bool {
		default: false,
		flags: archive,
	};
	/// pi `emojiAutocomplete` (boolean, default: true).
	pub static CL_EMOJI_AUTOCOMPLETE = cl_emoji_autocomplete: bool {
		default: true,
		flags: archive,
	};
	/// pi `paste.largeMenuThreshold` (number, default: 100).
	pub static CL_PASTE_LARGE_MENU_THRESHOLD = cl_paste_large_menu_threshold: i64 {
		default: 100,
		flags: archive,
	};
	/// pi `magicKeywords.enabled` (boolean, default: true).
	pub static CL_MAGIC_KEYWORDS_ENABLED = cl_magic_keywords_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `magicKeywords.ultrathink` (boolean, default: true).
	pub static CL_MAGIC_KEYWORDS_ULTRATHINK = cl_magic_keywords_ultrathink: bool {
		default: true,
		flags: archive,
	};
}

omp_con::var! {
	/// pi `magicKeywords.orchestrate` (boolean, default: true).
	pub static CL_MAGIC_KEYWORDS_ORCHESTRATE = cl_magic_keywords_orchestrate: bool {
		default: true,
		flags: archive,
	};
	/// pi `magicKeywords.workflow` (boolean, default: true).
	pub static CL_MAGIC_KEYWORDS_WORKFLOW = cl_magic_keywords_workflow: bool {
		default: true,
		flags: archive,
	};
	/// pi `recap.enabled` (boolean, default: true).
	pub static CL_RECAP_ENABLED = cl_recap_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `recap.idleSeconds` (number, default: 240).
	pub static CL_RECAP_IDLE_SECONDS = cl_recap_idle_seconds: i64 {
		default: 240,
		flags: archive,
	};
	/// pi `collab.relayUrl` (string, default: DEFAULT_RELAY_URL).
	pub static CL_COLLAB_RELAY_URL = cl_collab_relay_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `collab.webUrl` (string, default: "").
	pub static CL_COLLAB_WEB_URL = cl_collab_web_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `ask.enabled` (boolean, default: true).
	pub static CL_ASK_ENABLED = cl_ask_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `goal.enabled` (boolean, default: true).
	pub static CL_GOAL_ENABLED = cl_goal_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `goal.statusInFooter` (boolean, default: true).
	pub static CL_GOAL_STATUS_IN_FOOTER = cl_goal_status_in_footer: bool {
		default: true,
		flags: archive,
	};
	/// pi `goal.continuationModes` (array, default: ["interactive"]).
	pub static CL_GOAL_CONTINUATION_MODES = cl_goal_continuation_modes: Vec<Str> {
		default: vec![Str::new_static("interactive")],
		flags: archive,
	};
	/// pi `title.refreshOnReplan` (boolean, default: true).
	pub static CL_TITLE_REFRESH_ON_REPLAN = cl_title_refresh_on_replan: bool {
		default: true,
		flags: archive,
	};
	/// pi `codexResets.autoRedeem` (enum, default: "unset" as const).
	pub static CL_CODEX_RESETS_AUTO_REDEEM = cl_codex_resets_auto_redeem: Str {
		default: Str::new_static("unset"),
		flags: archive,
	};
	/// pi `codexResets.minBlockedMinutes` (number, default: 60).
	pub static CL_CODEX_RESETS_MIN_BLOCKED_MINUTES = cl_codex_resets_min_blocked_minutes: i64 {
		default: 60,
		flags: archive,
	};
	/// pi `codexResets.keepCredits` (number, default: 0).
	pub static CL_CODEX_RESETS_KEEP_CREDITS = cl_codex_resets_keep_credits: i64 {
		default: 0,
		flags: archive,
	};
	/// pi `codexResets.salvageHorizonHours` (number, default: 12).
	pub static CL_CODEX_RESETS_SALVAGE_HORIZON_HOURS = cl_codex_resets_salvage_horizon_hours: i64 {
		default: 12,
		flags: archive,
	};
}

/// Exact pi setting keys and their command-stream convar names.
pub const LEGACY_CONVAR_MAPPINGS: &[(&str, &str)] = &[
	("colorBlindMode", "cl_color_blind_mode"),
	("composer.shape", "cl_composer_shape"),
	("statusLine.preset", "cl_status_line_preset"),
	("statusLine.separator", "cl_status_line_separator"),
	("statusLine.contextLine", "cl_status_line_context_line"),
	("statusLine.sessionAccent", "cl_status_line_session_accent"),
	("statusLine.transparent", "cl_status_line_transparent"),
	("statusLine.showHookStatus", "cl_status_line_show_hook_status"),
	("statusLine.leftSegments", "cl_status_line_left_segments"),
	("statusLine.rightSegments", "cl_status_line_right_segments"),
	("statusLine.segmentOptions", "cl_status_line_segment_options"),
	("terminal.showImages", "cl_terminal_show_images"),
	("tui.maxInlineImageColumns", "cl_tui_max_inline_image_columns"),
	("tui.maxInlineImageRows", "cl_tui_max_inline_image_rows"),
	("tui.maxInlineImages", "cl_tui_max_inline_images"),
	("terminal.showProgress", "cl_terminal_show_progress"),
	("tui.textSizing", "cl_tui_text_sizing"),
	("tui.renderMermaid", "cl_tui_render_mermaid"),
	("tui.titleState", "cl_tui_title_state"),
	("tui.hyperlinks", "cl_tui_hyperlinks"),
	("tui.tight", "cl_tui_tight"),
	("display.shimmer", "cl_display_shimmer"),
	("display.showTokenUsage", "cl_display_show_token_usage"),
	("display.showTurnTime", "cl_display_show_turn_time"),
	("display.cacheMissMarker", "cl_display_cache_miss_marker"),
	("display.collapseCompacted", "cl_display_collapse_compacted"),
	("showHardwareCursor", "cl_show_hardware_cursor"),
	("steeringMode", "cl_steering_mode"),
	("followUpMode", "cl_follow_up_mode"),
	("interruptMode", "cl_interrupt_mode"),
	("loop.mode", "cl_loop_mode"),
	("treeFilterMode", "cl_tree_filter_mode"),
	("autocompleteMaxVisible", "cl_autocomplete_max_visible"),
	("spelling.typoDetection", "cl_spelling_typo_detection"),
	("spelling.autocomplete", "cl_spelling_autocomplete"),
	("spelling.autocorrect", "cl_spelling_autocorrect"),
	("emojiAutocomplete", "cl_emoji_autocomplete"),
	("paste.largeMenuThreshold", "cl_paste_large_menu_threshold"),
	("magicKeywords.enabled", "cl_magic_keywords_enabled"),
	("magicKeywords.ultrathink", "cl_magic_keywords_ultrathink"),
	("magicKeywords.orchestrate", "cl_magic_keywords_orchestrate"),
	("magicKeywords.workflow", "cl_magic_keywords_workflow"),
	("recap.enabled", "cl_recap_enabled"),
	("recap.idleSeconds", "cl_recap_idle_seconds"),
	("collab.relayUrl", "cl_collab_relay_url"),
	("collab.webUrl", "cl_collab_web_url"),
	("ask.enabled", "cl_ask_enabled"),
	("goal.enabled", "cl_goal_enabled"),
	("goal.statusInFooter", "cl_goal_status_in_footer"),
	("goal.continuationModes", "cl_goal_continuation_modes"),
	("title.refreshOnReplan", "cl_title_refresh_on_replan"),
	("codexResets.autoRedeem", "cl_codex_resets_auto_redeem"),
	("codexResets.minBlockedMinutes", "cl_codex_resets_min_blocked_minutes"),
	("codexResets.keepCredits", "cl_codex_resets_keep_credits"),
	("codexResets.salvageHorizonHours", "cl_codex_resets_salvage_horizon_hours"),
];
