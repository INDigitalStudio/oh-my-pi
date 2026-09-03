//! Physical key chords lowered to command-stream bindings.
//!
//! ADR 0014: keybindings are `bind` lines. [`DEFAULT_BINDS`] is the cfg of
//! defaults (pi parity), executed before the user's `config.cfg`;
//! [`PI_ACTIONS`] is the migration table from pi's action ids to the console
//! command each default binds.

pub mod config;

/// Default `bind` script, executed before `config.cfg`.
#[cfg(target_os = "macos")]
pub const DEFAULT_BINDS: &str =
	concat!(include_str!("default.cfg"), include_str!("default-macos.cfg"));
/// Default `bind` script, executed before `config.cfg`.
#[cfg(target_os = "windows")]
pub const DEFAULT_BINDS: &str =
	concat!(include_str!("default.cfg"), include_str!("default-windows.cfg"));
/// Default `bind` script, executed before `config.cfg`.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const DEFAULT_BINDS: &str = include_str!("default.cfg");

/// Name of the default bind cfg as reported in command-stream provenance.
pub const DEFAULT_BINDS_NAME: &str = "default-binds.cfg";

/// pi keybinding id → console command used by migration.
///
/// The default cfg may bind one physical chord to a short fallback script
/// when pi gives that chord to several contextual actions. Each action still
/// has its own command here, so a migrated user remap retains its exact scope.
pub const PI_ACTIONS: &[(&str, &str)] = &[
	("app.interrupt", "cl_interrupt"),
	("app.clear", "cl_clear"),
	("app.exit", "cl_exit"),
	("app.suspend", "cl_suspend"),
	("app.display.reset", "cl_display_reset"),
	("app.thinking.cycle", "cl_thinking_cycle"),
	("app.thinking.toggle", "toggle cl_showthinking"),
	("app.model.cycleForward", "cl_model_cycle"),
	("app.model.cycleBackward", "cl_model_cycle back"),
	("app.model.select", "cl_model_select"),
	("app.model.selectTemporary", "cl_model_select session"),
	("app.tools.expand", "cl_tools_expand"),
	("app.tools.toggleVisibility", "toggle cl_showtools"),
	("app.editor.external", "cl_editor_external"),
	("app.message.followUp", "cl_followup"),
	("app.retry", "cl_retry"),
	("app.plan.toggle", "cl_plan_toggle"),
	("app.history.search", "cl_history_search"),
	("app.message.dequeue", "cl_dequeue"),
	("app.clipboard.pasteImage", "cl_paste_image"),
	("app.clipboard.pasteTextRaw", "cl_paste_raw"),
	("app.clipboard.copyLine", "cl_copy_line"),
	("app.clipboard.copyPrompt", "cl_copy_prompt"),
	("app.agents.hub", "agents"),
	("app.session.observe", "hub"),
	("app.session.togglePath", "panel_toggle_path"),
	("app.session.toggleSort", "panel_toggle_sort"),
	("app.session.rename", "panel_rename"),
	("app.session.delete", "panel_delete"),
	("app.session.deleteNoninvasive", "panel_delete_fast"),
	("app.tree.foldOrUp", "panel_fold_up"),
	("app.tree.unfoldOrDown", "panel_unfold_down"),
	("app.session.new", "new"),
	("app.session.tree", "tree"),
	("app.session.fork", "fork"),
	("app.session.resume", "resume"),
	("app.stt.toggle", "cl_stt_toggle"),
	("app.live.toggle", "live"),
	("tui.editor.cursorUp", "ed_up"),
	("tui.editor.cursorDown", "ed_down"),
	("tui.editor.cursorLeft", "ed_left"),
	("tui.editor.cursorRight", "ed_right"),
	("tui.editor.cursorWordLeft", "ed_word_left"),
	("tui.editor.cursorWordRight", "ed_word_right"),
	("tui.editor.cursorLineStart", "ed_home"),
	("tui.editor.cursorLineEnd", "ed_end"),
	("tui.editor.jumpForward", "ed_jump_forward"),
	("tui.editor.jumpBackward", "ed_jump_backward"),
	("tui.editor.pageUp", "ed_page_up"),
	("tui.editor.pageDown", "ed_page_down"),
	("tui.editor.deleteCharBackward", "ed_backspace"),
	("tui.editor.deleteCharForward", "ed_delete"),
	("tui.editor.deleteWordBackward", "ed_delete_word_backward"),
	("tui.editor.deleteWordForward", "ed_delete_word_forward"),
	("tui.editor.deleteToLineStart", "ed_delete_to_start"),
	("tui.editor.deleteToLineEnd", "ed_delete_to_end"),
	("tui.editor.yank", "ed_yank"),
	("tui.editor.yankPop", "ed_yank_pop"),
	("tui.editor.undo", "ed_undo"),
	("tui.editor.spellingSuggestions", "ed_spelling"),
	("tui.input.newLine", "ed_newline"),
	("tui.input.submit", "ed_enter"),
	("tui.input.tab", "ed_tab"),
	("tui.input.copy", "ed_copy"),
	("tui.select.up", "ed_up"),
	("tui.select.down", "ed_down"),
	("tui.select.pageUp", "ed_page_up"),
	("tui.select.pageDown", "ed_page_down"),
	("tui.select.confirm", "ed_enter"),
	("tui.select.cancel", "cl_interrupt"),
];

/// Console command for a pi keybinding id, when omp implements it.
#[must_use]
pub fn pi_action_command(action: &str) -> Option<&'static str> {
	PI_ACTIONS
		.iter()
		.find_map(|(id, command)| (*id == action).then_some(*command))
}
