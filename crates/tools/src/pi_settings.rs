//! Literal pi-setting convars not otherwise owned by a narrower runtime module.

use omp_con::Kv;
use omp_core::Str;

omp_con::var! {
	/// pi `tools.artifactTailBytes` (number, default: 20).
	pub static SV_TOOLS_ARTIFACT_TAIL_BYTES = sv_tools_artifact_tail_bytes: i64 {
		default: 20,
		flags: archive,
	};
	/// pi `tools.artifactHeadBytes` (number, default: 20).
	pub static SV_TOOLS_ARTIFACT_HEAD_BYTES = sv_tools_artifact_head_bytes: i64 {
		default: 20,
		flags: archive,
	};
	/// pi `tools.outputMaxColumns` (number, default: 768).
	pub static SV_TOOLS_OUTPUT_MAX_COLUMNS = sv_tools_output_max_columns: i64 {
		default: 768,
		flags: archive,
	};
	/// pi `tools.artifactTailLines` (number, default: 500).
	pub static SV_TOOLS_ARTIFACT_TAIL_LINES = sv_tools_artifact_tail_lines: i64 {
		default: 500,
		flags: archive,
	};
	/// pi `images.blockImages` (boolean, default: false).
	pub static SV_IMAGES_BLOCK_IMAGES = sv_images_block_images: bool {
		default: false,
		flags: archive,
	};
	/// pi `images.describeForTextModels` (boolean, default: true).
	pub static SV_IMAGES_DESCRIBE_FOR_TEXT_MODELS = sv_images_describe_for_text_models: bool {
		default: true,
		flags: archive,
	};
	/// pi `images.urls.enabled` (boolean, default: false).
	pub static SV_IMAGES_URLS_ENABLED = sv_images_urls_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `images.urls.backends` (array, default: ["provider-files").
	pub static SV_IMAGES_URLS_BACKENDS = sv_images_urls_backends: Vec<Str> {
		default: vec![Str::new_static("provider-files")],
		flags: archive,
	};
	/// pi `images.urls.options` (record, default: {).
	pub static SV_IMAGES_URLS_OPTIONS = sv_images_urls_options: Kv {
		default: Kv::new(),
		flags: archive,
	};
	/// pi `images.urls.credentials` (record, default: {).
	pub static SV_IMAGES_URLS_CREDENTIALS = sv_images_urls_credentials: Kv {
		default: Kv::new(),
	};
	/// pi `images.urls.command` (string, default: undefined).
	pub static SV_IMAGES_URLS_COMMAND = sv_images_urls_command: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `images.urls.publicBaseUrl` (string, default: undefined).
	pub static SV_IMAGES_URLS_PUBLIC_BASE_URL = sv_images_urls_public_base_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `images.urls.ttlHours` (number, default: 72).
	pub static SV_IMAGES_URLS_TTL_HOURS = sv_images_urls_ttl_hours: i64 {
		default: 72,
		flags: archive,
	};
	/// pi `images.urls.bindHost` (string, default: "127.0.0.1").
	pub static SV_IMAGES_URLS_BIND_HOST = sv_images_urls_bind_host: Str {
		default: Str::new_static("127.0.0.1"),
		flags: archive,
	};
	/// pi `images.urls.sshTarget` (string, default: undefined).
	pub static SV_IMAGES_URLS_SSH_TARGET = sv_images_urls_ssh_target: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `images.urls.sshRemotePort` (number, default: 8787).
	pub static SV_IMAGES_URLS_SSH_REMOTE_PORT = sv_images_urls_ssh_remote_port: i64 {
		default: 8787,
		flags: archive,
	};
	/// pi `tools.format` (enum, default: "auto").
	pub static SV_TOOLS_FORMAT = sv_tools_format: Str {
		default: Str::new_static("auto"),
		flags: archive,
	};
	/// pi `edit.fuzzyThreshold` (number, default: 0.95).
	pub static SV_EDIT_FUZZY_THRESHOLD = sv_edit_fuzzy_threshold: f64 {
		default: 0.95,
		flags: archive,
	};
	/// pi `edit.recoverInlineEdits` (boolean, default: true).
	pub static SV_EDIT_RECOVER_INLINE_EDITS = sv_edit_recover_inline_edits: bool {
		default: true,
		flags: archive,
	};
	/// pi `read.summarize.minBodyLines` (number, default: 4).
	pub static SV_READ_SUMMARIZE_MIN_BODY_LINES = sv_read_summarize_min_body_lines: i64 {
		default: 4,
		flags: archive,
	};
	/// pi `read.summarize.minCommentLines` (number, default: 6).
	pub static SV_READ_SUMMARIZE_MIN_COMMENT_LINES = sv_read_summarize_min_comment_lines: i64 {
		default: 6,
		flags: archive,
	};
	/// pi `read.summarize.minTotalLines` (number, default: 100).
	pub static SV_READ_SUMMARIZE_MIN_TOTAL_LINES = sv_read_summarize_min_total_lines: i64 {
		default: 100,
		flags: archive,
	};
	/// pi `read.summarize.unfoldUntil` (number, default: 50).
	pub static SV_READ_SUMMARIZE_UNFOLD_UNTIL = sv_read_summarize_unfold_until: i64 {
		default: 50,
		flags: archive,
	};
	/// pi `read.summarize.unfoldLimit` (number, default: 100).
	pub static SV_READ_SUMMARIZE_UNFOLD_LIMIT = sv_read_summarize_unfold_limit: i64 {
		default: 100,
		flags: archive,
	};
	/// pi `read.toolResultPreview` (boolean, default: false).
	pub static SV_READ_TOOL_RESULT_PREVIEW = sv_read_tool_result_preview: bool {
		default: false,
		flags: archive,
	};
	/// pi `lsp.shared` (boolean, default: true).
	pub static SV_LSP_SHARED = sv_lsp_shared: bool {
		default: true,
		flags: archive,
	};
	/// pi `bash.patterns` (array, default: []).
	pub static SV_BASH_PATTERNS = sv_bash_patterns: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `eval.py` (boolean, default: true).
	pub static SV_EVAL_PY = sv_eval_py: bool {
		default: true,
		flags: archive,
	};
	/// pi `eval.autoBackground.enabled` (boolean, default: false).
	pub static SV_EVAL_AUTO_BACKGROUND_ENABLED = sv_eval_auto_background_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `eval.autoBackground.thresholdMs` (number, default: 60_000).
	pub static SV_EVAL_AUTO_BACKGROUND_THRESHOLD_MS = sv_eval_auto_background_threshold_ms: i64 {
		default: 60000,
		flags: archive,
	};
	/// pi `python.kernelMode` (enum, default: "session").
	pub static SV_PYTHON_KERNEL_MODE = sv_python_kernel_mode: Str {
		default: Str::new_static("session"),
		flags: archive,
	};
	/// pi `todo.enabled` (boolean, default: true).
	pub static SV_TODO_ENABLED = sv_todo_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `todo.reminders` (boolean, default: true).
	pub static SV_TODO_REMINDERS = sv_todo_reminders: bool {
		default: true,
		flags: archive,
	};
	/// pi `todo.remindersMax` (number, default: 3).
	pub static SV_TODO_REMINDERS_MAX = sv_todo_reminders_max: i64 {
		default: 3,
		flags: archive,
	};
	/// pi `todo.eager` (enum, default: "default").
	pub static SV_TODO_EAGER = sv_todo_eager: Str {
		default: Str::new_static("default"),
		flags: archive,
	};
	/// pi `glob.enabled` (boolean, default: true).
	pub static SV_GLOB_ENABLED = sv_glob_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `grep.enabled` (boolean, default: true).
	pub static SV_GREP_ENABLED = sv_grep_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `astGrep.enabled` (boolean, default: false).
	pub static SV_AST_GREP_ENABLED = sv_ast_grep_enabled: bool {
		default: false,
		flags: archive,
	};
}

omp_con::var! {
	/// pi `astEdit.enabled` (boolean, default: true).
	pub static SV_AST_EDIT_ENABLED = sv_ast_edit_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `debug.enabled` (boolean, default: true).
	pub static SV_DEBUG_ENABLED = sv_debug_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `launch.enabled` (boolean, default: true).
	pub static SV_LAUNCH_ENABLED = sv_launch_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `generate_image.enabled` (boolean, default: false).
	pub static SV_GENERATE_IMAGE_ENABLED = sv_generate_image_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `inspect_image.enabled` (boolean, default: false).
	pub static SV_INSPECT_IMAGE_ENABLED = sv_inspect_image_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `computer.enabled` (boolean, default: false).
	pub static SV_COMPUTER_ENABLED = sv_computer_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `computer.display` (string, default: "all").
	pub static SV_COMPUTER_DISPLAY = sv_computer_display: Str {
		default: Str::new_static("all"),
		flags: archive,
	};
	/// pi `computer.maxWidth` (number, default: 3840).
	pub static SV_COMPUTER_MAX_WIDTH = sv_computer_max_width: i64 {
		default: 3840,
		flags: archive,
	};
	/// pi `computer.maxHeight` (number, default: 2400).
	pub static SV_COMPUTER_MAX_HEIGHT = sv_computer_max_height: i64 {
		default: 2400,
		flags: archive,
	};
	/// pi `inspect_image.timeoutMs` (number, default: 300_000).
	pub static SV_INSPECT_IMAGE_TIMEOUT_MS = sv_inspect_image_timeout_ms: i64 {
		default: 300000,
		flags: archive,
	};
	/// pi `checkpoint.enabled` (boolean, default: false).
	pub static SV_CHECKPOINT_ENABLED = sv_checkpoint_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `vault.enabled` (boolean, default: false).
	pub static SV_VAULT_ENABLED = sv_vault_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `github.enabled` (boolean, default: false).
	pub static SV_GITHUB_ENABLED = sv_github_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `github.cache.enabled` (boolean, default: true).
	pub static SV_GITHUB_CACHE_ENABLED = sv_github_cache_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `github.cache.softTtlSec` (number, default: 300).
	pub static SV_GITHUB_CACHE_SOFT_TTL_SEC = sv_github_cache_soft_ttl_sec: i64 {
		default: 300,
		flags: archive,
	};
	/// pi `github.cache.hardTtlSec` (number, default: 604800).
	pub static SV_GITHUB_CACHE_HARD_TTL_SEC = sv_github_cache_hard_ttl_sec: i64 {
		default: 604800,
		flags: archive,
	};
	/// pi `tools.abortOnFabricatedResult` (boolean, default: true).
	pub static SV_TOOLS_ABORT_ON_FABRICATED_RESULT = sv_tools_abort_on_fabricated_result: bool {
		default: true,
		flags: archive,
	};
	/// pi `tools.xdev` (boolean, default: true).
	pub static SV_TOOLS_XDEV = sv_tools_xdev: bool {
		default: true,
		flags: archive,
	};
	/// pi `tools.xdevDocs` (enum, default: "builtins").
	pub static SV_TOOLS_XDEV_DOCS = sv_tools_xdev_docs: Str {
		default: Str::new_static("builtins"),
		flags: archive,
	};
	/// pi `tools.xdevInlineDevices` (array, default: EMPTY_STRING_ARRAY).
	pub static SV_TOOLS_XDEV_INLINE_DEVICES = sv_tools_xdev_inline_devices: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `exa.enabled` (boolean, default: true).
	pub static SV_EXA_ENABLED = sv_exa_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `exa.searchDelayMs` (number, default: 1_000).
	pub static SV_EXA_SEARCH_DELAY_MS = sv_exa_search_delay_ms: i64 {
		default: 1000,
		flags: archive,
	};
}

/// Exact pi setting keys and their command-stream convar names.
pub const LEGACY_CONVAR_MAPPINGS: &[(&str, &str)] = &[
	("tools.artifactTailBytes", "sv_tools_artifact_tail_bytes"),
	("tools.artifactHeadBytes", "sv_tools_artifact_head_bytes"),
	("tools.outputMaxColumns", "sv_tools_output_max_columns"),
	("tools.artifactTailLines", "sv_tools_artifact_tail_lines"),
	("images.blockImages", "sv_images_block_images"),
	("images.describeForTextModels", "sv_images_describe_for_text_models"),
	("images.urls.enabled", "sv_images_urls_enabled"),
	("images.urls.backends", "sv_images_urls_backends"),
	("images.urls.options", "sv_images_urls_options"),
	("images.urls.credentials", "sv_images_urls_credentials"),
	("images.urls.command", "sv_images_urls_command"),
	("images.urls.publicBaseUrl", "sv_images_urls_public_base_url"),
	("images.urls.ttlHours", "sv_images_urls_ttl_hours"),
	("images.urls.bindHost", "sv_images_urls_bind_host"),
	("images.urls.sshTarget", "sv_images_urls_ssh_target"),
	("images.urls.sshRemotePort", "sv_images_urls_ssh_remote_port"),
	("tools.format", "sv_tools_format"),
	("edit.fuzzyThreshold", "sv_edit_fuzzy_threshold"),
	("edit.recoverInlineEdits", "sv_edit_recover_inline_edits"),
	("read.summarize.minBodyLines", "sv_read_summarize_min_body_lines"),
	("read.summarize.minCommentLines", "sv_read_summarize_min_comment_lines"),
	("read.summarize.minTotalLines", "sv_read_summarize_min_total_lines"),
	("read.summarize.unfoldUntil", "sv_read_summarize_unfold_until"),
	("read.summarize.unfoldLimit", "sv_read_summarize_unfold_limit"),
	("read.toolResultPreview", "sv_read_tool_result_preview"),
	("lsp.shared", "sv_lsp_shared"),
	("bash.patterns", "sv_bash_patterns"),
	("eval.py", "sv_eval_py"),
	("eval.autoBackground.enabled", "sv_eval_auto_background_enabled"),
	("eval.autoBackground.thresholdMs", "sv_eval_auto_background_threshold_ms"),
	("python.kernelMode", "sv_python_kernel_mode"),
	("todo.enabled", "sv_todo_enabled"),
	("todo.reminders", "sv_todo_reminders"),
	("todo.remindersMax", "sv_todo_reminders_max"),
	("todo.eager", "sv_todo_eager"),
	("glob.enabled", "sv_glob_enabled"),
	("grep.enabled", "sv_grep_enabled"),
	("astGrep.enabled", "sv_ast_grep_enabled"),
	("astEdit.enabled", "sv_ast_edit_enabled"),
	("debug.enabled", "sv_debug_enabled"),
	("launch.enabled", "sv_launch_enabled"),
	("generate_image.enabled", "sv_generate_image_enabled"),
	("inspect_image.enabled", "sv_inspect_image_enabled"),
	("computer.enabled", "sv_computer_enabled"),
	("computer.display", "sv_computer_display"),
	("computer.maxWidth", "sv_computer_max_width"),
	("computer.maxHeight", "sv_computer_max_height"),
	("inspect_image.timeoutMs", "sv_inspect_image_timeout_ms"),
	("checkpoint.enabled", "sv_checkpoint_enabled"),
	("vault.enabled", "sv_vault_enabled"),
	("github.enabled", "sv_github_enabled"),
	("github.cache.enabled", "sv_github_cache_enabled"),
	("github.cache.softTtlSec", "sv_github_cache_soft_ttl_sec"),
	("github.cache.hardTtlSec", "sv_github_cache_hard_ttl_sec"),
	("tools.abortOnFabricatedResult", "sv_tools_abort_on_fabricated_result"),
	("tools.xdev", "sv_tools_xdev"),
	("tools.xdevDocs", "sv_tools_xdev_docs"),
	("tools.xdevInlineDevices", "sv_tools_xdev_inline_devices"),
	("exa.enabled", "sv_exa_enabled"),
	("exa.searchDelayMs", "sv_exa_search_delay_ms"),
];
