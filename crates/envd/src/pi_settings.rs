//! Literal pi-setting convars not otherwise owned by a narrower runtime module.

use omp_core::Str;

omp_con::var! {
	/// Enables authored and managed skill discovery.
	pub static SV_SKILLS_ENABLED = sv_skills_enabled: bool {
		default: true,
		flags: archive,
	};
	/// Additional authored skill roots.
	pub static SV_SKILLS_CUSTOM_DIRECTORIES = sv_skills_custom_directories: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// Skill names excluded before publication.
	pub static SV_SKILLS_IGNORE = sv_skills_ignore: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// Optional skill-name inclusion filters.
	pub static SV_SKILLS_INCLUDE = sv_skills_include: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `shellPath` (string, default: undefined).
	pub static SV_SHELL_PATH = sv_shell_path: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `browser.cdpUrl` (string, default: undefined).
	pub static SV_BROWSER_CDP_URL = sv_browser_cdp_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `browser.relay` (boolean, default: false).
	pub static SV_BROWSER_RELAY = sv_browser_relay: bool {
		default: false,
		flags: archive,
	};
	/// pi `browser.relayUrl` (string, default: undefined).
	pub static SV_BROWSER_RELAY_URL = sv_browser_relay_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `browser.cmux` (boolean, default: true).
	pub static SV_BROWSER_CMUX = sv_browser_cmux: bool {
		default: true,
		flags: archive,
	};
	/// pi `browser.screenshotDir` (string, default: undefined).
	pub static SV_BROWSER_SCREENSHOT_DIR = sv_browser_screenshot_dir: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `mcp.renderMarkdownResults` (boolean, default: true).
	pub static SV_MCP_RENDER_MARKDOWN_RESULTS = sv_mcp_render_markdown_results: bool {
		default: true,
		flags: archive,
	};
	/// pi `mcp.notifications` (boolean, default: false).
	pub static SV_MCP_NOTIFICATIONS = sv_mcp_notifications: bool {
		default: false,
		flags: archive,
	};
	/// pi `mcp.notificationDebounceMs` (number, default: 500).
	pub static SV_MCP_NOTIFICATION_DEBOUNCE_MS = sv_mcp_notification_debounce_ms: i64 {
		default: 500,
		flags: archive,
	};
	/// pi `searxng.token` (string, default: undefined).
	pub static SV_SEARXNG_TOKEN = sv_searxng_token: Str {
		default: Str::new_static(""),
	};
	/// pi `searxng.basicUsername` (string, default: undefined).
	pub static SV_SEARXNG_BASIC_USERNAME = sv_searxng_basic_username: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `searxng.basicPassword` (string, default: undefined).
	pub static SV_SEARXNG_BASIC_PASSWORD = sv_searxng_basic_password: Str {
		default: Str::new_static(""),
	};
	/// pi `searxng.categories` (string, default: undefined).
	pub static SV_SEARXNG_CATEGORIES = sv_searxng_categories: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `searxng.engines` (string, default: undefined).
	pub static SV_SEARXNG_ENGINES = sv_searxng_engines: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `searxng.language` (string, default: undefined).
	pub static SV_SEARXNG_LANGUAGE = sv_searxng_language: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `searxng.safesearch` (number, default: undefined).
	pub static SV_SEARXNG_SAFESEARCH = sv_searxng_safesearch: f64 {
		default: -1.0,
		flags: archive,
	};
}

/// Exact pi setting keys and their command-stream convar names.
pub const LEGACY_CONVAR_MAPPINGS: &[(&str, &str)] = &[
	("skills.enabled", "sv_skills_enabled"),
	("skills.customDirectories", "sv_skills_custom_directories"),
	("skills.ignoredSkills", "sv_skills_ignore"),
	("skills.includeSkills", "sv_skills_include"),
	("shellPath", "sv_shell_path"),
	("browser.cdpUrl", "sv_browser_cdp_url"),
	("browser.relay", "sv_browser_relay"),
	("browser.relayUrl", "sv_browser_relay_url"),
	("browser.cmux", "sv_browser_cmux"),
	("browser.screenshotDir", "sv_browser_screenshot_dir"),
	("mcp.renderMarkdownResults", "sv_mcp_render_markdown_results"),
	("mcp.notifications", "sv_mcp_notifications"),
	("mcp.notificationDebounceMs", "sv_mcp_notification_debounce_ms"),
	("searxng.token", "sv_searxng_token"),
	("searxng.basicUsername", "sv_searxng_basic_username"),
	("searxng.basicPassword", "sv_searxng_basic_password"),
	("searxng.categories", "sv_searxng_categories"),
	("searxng.engines", "sv_searxng_engines"),
	("searxng.language", "sv_searxng_language"),
	("searxng.safesearch", "sv_searxng_safesearch"),
];
