//! Curated product settings metadata.
//!
//! This is a projection over convars, not another settings store: value types,
//! defaults, validation, flags, and persistence remain owned by
//! [`VarSpec`](crate::VarSpec). Only entries explicitly listed here, or dynamic
//! variables carrying [`DynamicUiSpec`], are eligible for a product settings
//! surface.

use omp_core::Str;

use crate::Ctx;

/// A settings-panel tab, in pi's `SETTING_TABS` order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum SettingTab {
	/// Theme and terminal presentation.
	Appearance,
	/// Model behavior and sampling.
	Model,
	/// Input and session interaction.
	Interaction,
	/// Context collection and compaction.
	Context,
	/// Memory systems.
	Memory,
	/// File tools and language services.
	Files,
	/// Shell and runtime execution.
	Shell,
	/// Tool behavior.
	Tools,
	/// Task and subagent behavior.
	Tasks,
	/// Provider transports and services.
	Providers,
}

/// Human presentation and group order for one tab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabSpec {
	/// Stable tab identity.
	pub tab:    SettingTab,
	/// Human tab label.
	pub label:  &'static str,
	/// Theme icon key.
	pub icon:   &'static str,
	/// Ordered section headings.
	pub groups: &'static [&'static str],
}

/// pi's `SETTING_TABS`, `TAB_METADATA`, and `TAB_GROUPS`.
pub const SETTING_TABS: &[TabSpec] = &[
	TabSpec {
		tab:    SettingTab::Appearance,
		label:  "Appearance",
		icon:   "tab.appearance",
		groups: &["Theme", "Composer", "Status Line", "Display", "Images"],
	},
	TabSpec {
		tab:    SettingTab::Model,
		label:  "Model",
		icon:   "tab.model",
		groups: &[
			"Thinking",
			"Sampling",
			"Prompt",
			"Retry & Fallback",
			"Advisor",
			"Prewalk",
			"Vision",
		],
	},
	TabSpec {
		tab:    SettingTab::Interaction,
		label:  "Interaction",
		icon:   "tab.interaction",
		groups: &[
			"Input",
			"Approvals",
			"Notifications",
			"Speech",
			"Collab",
			"Magic Keywords",
			"Startup & Updates",
			"Power",
			"Agent",
			"Git",
		],
	},
	TabSpec {
		tab:    SettingTab::Context,
		label:  "Context",
		icon:   "tab.context",
		groups: &["General", "Compaction", "Rules (TTSR)", "Experimental"],
	},
	TabSpec {
		tab:    SettingTab::Memory,
		label:  "Memory",
		icon:   "tab.memory",
		groups: &["General", "Auto-Learn", "Mnemopi", "Hindsight", "Sharpshooter"],
	},
	TabSpec {
		tab:    SettingTab::Files,
		label:  "Files",
		icon:   "tab.files",
		groups: &["Editing", "Reading", "Read Summaries", "LSP"],
	},
	TabSpec {
		tab:    SettingTab::Shell,
		label:  "Shell",
		icon:   "tab.shell",
		groups: &["Bash", "Eval & Runtimes"],
	},
	TabSpec {
		tab:    SettingTab::Tools,
		label:  "Tools",
		icon:   "tab.tools",
		groups: &[
			"Available Tools",
			"Todos",
			"Grep & Browser",
			"Computer",
			"GitHub",
			"Output Limits",
			"Execution",
			"Discovery & MCP",
			"Extensions",
			"Developer",
		],
	},
	TabSpec {
		tab:    SettingTab::Tasks,
		label:  "Tasks",
		icon:   "tab.tasks",
		groups: &["Modes", "Subagents", "Isolation", "Commands & Skills"],
	},
	TabSpec {
		tab:    SettingTab::Providers,
		label:  "Providers",
		icon:   "tab.providers",
		groups: &["Services", "Fireworks", "Tiny Model", "Protocol", "Timeouts", "Privacy"],
	},
];

/// One labeled choice supplied by explicit UI metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiOption {
	/// Stored pi value.
	pub value:       &'static str,
	/// Human option label.
	pub label:       &'static str,
	/// Optional explanatory copy.
	pub description: &'static str,
}

impl UiOption {
	/// Const constructor used by the generated curated table.
	#[must_use]
	pub const fn new(value: &'static str, label: &'static str, description: &'static str) -> Self {
		Self { value, label, description }
	}
}

/// Widget behavior declared by pi UI metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiWidget {
	/// Boolean toggle.
	Boolean,
	/// Inline cycle over raw enum values.
	Enum(&'static [&'static str]),
	/// Labeled single-choice list.
	Submenu(&'static [UiOption]),
	/// Free text (the convar remains the type authority).
	Text {
		/// Whether the editor masks the value.
		secret: bool,
	},
	/// Labeled array-of-enum toggle list.
	MultiSelect {
		/// Available values and their presentation.
		options: &'static [UiOption],
		/// Whether selected values have meaningful order.
		ordered: bool,
	},
}

/// Built-in visibility predicates used by pi's curated selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiCondition {
	/// `retry.usageAwareFallback` is enabled.
	UsageAwareFallbackEnabled,
	/// The memory backend is mnemopi.
	MnemopiActive,
	/// Automatic thinking is selected.
	AutoThinkingActive,
	/// Smart unexpected-stop detection is selected.
	UnexpectedStopSmart,
}

impl UiCondition {
	/// Evaluates the predicate against live convar values.
	#[must_use]
	pub fn visible(self, con: &Ctx) -> bool {
		match self {
			Self::UsageAwareFallbackEnabled => con
				.get("ai_retry_usage_aware_fallback")
				.and_then(|value| value.as_bool())
				.unwrap_or(false),
			Self::MnemopiActive => con
				.get("ai_memory_backend")
				.and_then(|value| value.as_str().map(|value| value == "mnemopi"))
				.unwrap_or(false),
			Self::AutoThinkingActive => con
				.get("ai_default_thinking")
				.and_then(|value| value.as_str().map(|value| value == "auto"))
				.unwrap_or(false),
			Self::UnexpectedStopSmart => con
				.get("ai_features_unexpected_stop_detection")
				.and_then(|value| value.as_str().map(|value| value == "smart"))
				.unwrap_or(false),
		}
	}
}

/// Conversion between pi's displayed value and an intentionally different
/// convar representation. Identity is used unless the literal inventory maps
/// inverted booleans, units, or a compound legacy setting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UiValueCodec {
	/// UI and convar spellings are identical.
	#[default]
	Identity,
	/// UI boolean means the opposite of the convar boolean.
	InvertedBoolean,
	/// pi `on`/`off` enum backed by a boolean convar.
	OnOffBoolean,
	/// pi isolation enable switch backed by `none`/`auto`/backend enum.
	IsolationEnabled,
	/// Decimal kibibytes backed by an integer byte count.
	Kibibytes,
	/// Integer percent backed by a 0–1 fraction.
	PercentFraction,
	/// Integer seconds backed by a duration; zero means `never`.
	SecondsDuration,
	/// Integer milliseconds backed by a duration; zero means `never`.
	MillisecondsDuration,
}

/// Static curated metadata for one mapped built-in setting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiSpec {
	/// Upstream pi settings path, retained as parity evidence only.
	pub pi_path:     &'static str,
	/// Internal convar target. Product views must not render this as the label.
	pub convar:      &'static str,
	/// Product tab.
	pub tab:         SettingTab,
	/// Product section heading.
	pub group:       &'static str,
	/// Human row label.
	pub label:       &'static str,
	/// Human explanatory copy.
	pub description: &'static str,
	/// Optional risk copy.
	pub warning:     Option<&'static str>,
	/// Product widget.
	pub widget:      UiWidget,
	/// Optional visibility predicate.
	pub condition:   Option<UiCondition>,
	/// UI/convar value conversion.
	pub codec:       UiValueCodec,
}

/// Owned choice metadata for a dynamically admitted extension variable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicUiOption {
	/// Stored value.
	pub value:       Str,
	/// Human option label.
	pub label:       Str,
	/// Optional explanatory copy.
	pub description: Str,
}

/// Owned widget metadata for a dynamically admitted extension variable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicUiWidget {
	/// Infer a boolean, enum, or text widget from the variable declaration.
	Auto,
	/// Labeled single-choice list.
	Submenu(Vec<DynamicUiOption>),
	/// Labeled array toggle list.
	MultiSelect {
		/// Available values and their presentation.
		options: Vec<DynamicUiOption>,
		/// Whether selected values have meaningful order.
		ordered: bool,
	},
}

/// Optional curated projection carried by an admitted extension variable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicUiSpec {
	/// Product tab.
	pub tab:         SettingTab,
	/// Existing section heading in that tab.
	pub group:       Str,
	/// Human row label.
	pub label:       Str,
	/// Human explanatory copy.
	pub description: Str,
	/// Optional risk copy.
	pub warning:     Option<Str>,
	/// Widget metadata beyond the variable's own type.
	pub widget:      DynamicUiWidget,
}

impl DynamicUiSpec {
	/// Whether this metadata names a valid product group and a non-technical
	/// label.
	#[must_use]
	pub fn is_valid(&self, convar: &str) -> bool {
		let Some(tab) = SETTING_TABS.iter().find(|tab| tab.tab == self.tab) else {
			return false;
		};
		!self.label.trim().is_empty()
			&& self.label != convar
			&& !self.label.contains("::")
			&& tab.groups.contains(&self.group.as_str())
	}
}

macro_rules! ui {
	(
		$path:literal,
		$convar:literal,
		$tab:ident,
		$group:literal,
		$label:literal,
		$description:literal,
		$widget:expr,
		$condition:expr,
		$codec:ident
	) => {
		UiSpec {
			pi_path:     $path,
			convar:      $convar,
			tab:         SettingTab::$tab,
			group:       $group,
			label:       $label,
			description: $description,
			warning:     None,
			widget:      $widget,
			condition:   $condition,
			codec:       UiValueCodec::$codec,
		}
	};
}

mod ui_appearance_model;
mod ui_contextual;
mod ui_tools_tasks_providers;

/// All curated built-in entries in tab/group/declaration order.
pub fn builtin_ui_entries() -> impl Iterator<Item = &'static UiSpec> {
	ui_appearance_model::ENTRIES
		.iter()
		.chain(ui_contextual::ENTRIES)
		.chain(ui_tools_tasks_providers::ENTRIES)
}

/// Looks up a curated built-in by its internal convar and optional pi path.
#[must_use]
pub fn builtin_ui(convar: &str, pi_path: Option<&str>) -> Option<&'static UiSpec> {
	builtin_ui_entries()
		.find(|entry| entry.convar == convar && pi_path.is_none_or(|path| entry.pi_path == path))
}
