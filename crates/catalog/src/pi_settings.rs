//! Literal pi-setting convars not otherwise owned by a narrower runtime module.

use omp_core::Str;

omp_con::var! {
	/// pi `enabledProviders` (array, default: EMPTY_STRING_ARRAY).
	pub static AI_ENABLED_PROVIDERS = ai_enabled_providers: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `providers.openai-codex.codeMode` (enum, default: "off").
	pub static AI_PROVIDERS_OPENAI_CODEX_CODE_MODE = ai_providers_openai_codex_code_mode: Str {
		default: Str::new_static("off"),
		flags: archive,
	};
	/// pi `providers.openai-codex.codeModeDirectTools` (array, default: EMPTY_STRING_ARRAY).
	pub static AI_PROVIDERS_OPENAI_CODEX_CODE_MODE_DIRECT_TOOLS = ai_providers_openai_codex_code_mode_direct_tools: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `inlineToolDescriptors` (enum, default: "auto").
	pub static AI_INLINE_TOOL_DESCRIPTORS = ai_inline_tool_descriptors: Str {
		default: Str::new_static("auto"),
		flags: archive,
	};
	/// pi `includeModelInPrompt` (boolean, default: true).
	pub static AI_INCLUDE_MODEL_IN_PROMPT = ai_include_model_in_prompt: bool {
		default: true,
		flags: archive,
	};
	/// pi `includeWorkspaceTree` (boolean, default: false).
	pub static AI_INCLUDE_WORKSPACE_TREE = ai_include_workspace_tree: bool {
		default: false,
		flags: archive,
	};
	/// pi `personality` (enum, default: "default").
	pub static AI_PERSONALITY = ai_personality: Str {
		default: Str::new_static("default"),
		flags: archive,
	};
}

/// Exact pi setting keys and their command-stream convar names.
pub const LEGACY_CONVAR_MAPPINGS: &[(&str, &str)] = &[
	("enabledProviders", "ai_enabled_providers"),
	("providers.openai-codex.codeMode", "ai_providers_openai_codex_code_mode"),
	(
		"providers.openai-codex.codeModeDirectTools",
		"ai_providers_openai_codex_code_mode_direct_tools",
	),
	("inlineToolDescriptors", "ai_inline_tool_descriptors"),
	("includeModelInPrompt", "ai_include_model_in_prompt"),
	("includeWorkspaceTree", "ai_include_workspace_tree"),
	("personality", "ai_personality"),
];
