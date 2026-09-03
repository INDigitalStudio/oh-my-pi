//! Literal pi-setting convars not otherwise owned by a narrower runtime module.

use omp_core::Str;

omp_con::var! {
	/// pi `setupVersion` (number, default: 0).
	pub static CL_SETUP_VERSION = cl_setup_version: i64 {
		default: 0,
		flags: archive,
	};
	/// pi `auth.broker.url` (string, default: undefined).
	pub static CL_AUTH_BROKER_URL = cl_auth_broker_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `auth.broker.token` (string, default: undefined).
	pub static CL_AUTH_BROKER_TOKEN = cl_auth_broker_token: Str {
		default: Str::new_static(""),
	};
	/// pi `autoResume` (boolean, default: false).
	pub static CL_AUTO_RESUME = cl_auto_resume: bool {
		default: false,
		flags: archive,
	};
	/// pi `power.sleepPrevention` (enum, default: "idle").
	pub static CL_POWER_SLEEP_PREVENTION = cl_power_sleep_prevention: Str {
		default: Str::new_static("idle"),
		flags: archive,
	};
	/// pi `extensions` (array, default: EMPTY_STRING_ARRAY).
	pub static CL_EXTENSIONS = cl_extensions: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `startup.showSplash` (boolean, default: false).
	pub static CL_STARTUP_SHOW_SPLASH = cl_startup_show_splash: bool {
		default: false,
		flags: archive,
	};
	/// pi `startup.setupWizard` (boolean, default: true).
	pub static CL_STARTUP_SETUP_WIZARD = cl_startup_setup_wizard: bool {
		default: true,
		flags: archive,
	};
	/// pi `startup.checkUpdate` (boolean, default: true).
	pub static CL_STARTUP_CHECK_UPDATE = cl_startup_check_update: bool {
		default: true,
		flags: archive,
	};
	/// pi `update.channel` (enum, default: "stable").
	pub static CL_UPDATE_CHANNEL = cl_update_channel: Str {
		default: Str::new_static("stable"),
		flags: archive,
	};
	/// pi `marketplace.autoUpdate` (enum, default: "notify").
	pub static CL_MARKETPLACE_AUTO_UPDATE = cl_marketplace_auto_update: Str {
		default: Str::new_static("notify"),
		flags: archive,
	};
	/// pi `startup.changelogMode` (enum, default: "summary").
	pub static CL_STARTUP_CHANGELOG_MODE = cl_startup_changelog_mode: Str {
		default: Str::new_static("summary"),
		flags: archive,
	};
}

/// Exact pi setting keys and their command-stream convar names.
pub const LEGACY_CONVAR_MAPPINGS: &[(&str, &str)] = &[
	("setupVersion", "cl_setup_version"),
	("auth.broker.url", "cl_auth_broker_url"),
	("auth.broker.token", "cl_auth_broker_token"),
	("autoResume", "cl_auto_resume"),
	("power.sleepPrevention", "cl_power_sleep_prevention"),
	("extensions", "cl_extensions"),
	("disabledExtensions", "cl_disabled_extensions"),
	("startup.showSplash", "cl_startup_show_splash"),
	("startup.setupWizard", "cl_startup_setup_wizard"),
	("startup.checkUpdate", "cl_startup_check_update"),
	("update.channel", "cl_update_channel"),
	("marketplace.autoUpdate", "cl_marketplace_auto_update"),
	("startup.changelogMode", "cl_startup_changelog_mode"),
];
