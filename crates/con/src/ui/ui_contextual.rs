//! Mechanical projection of current pi settings UI metadata.

use super::*;

pub(super) const ENTRIES: &[UiSpec] = &[
	ui!(
		"doubleEscapeAction",
		"cl_double_escape",
		Interaction,
		"Input",
		"Double-Escape Action",
		"What pressing Escape twice with an empty editor does: open the transcript rewind selector, \
		 open the session tree, or nothing",
		UiWidget::Enum(&["rewind", "tree", "none"]),
		None,
		Identity
	),
	ui!(
		"tools.approval",
		"sv_tools_approval",
		Interaction,
		"Approvals",
		"Tool Approval Policies",
		"Per-tool approval policies. Set to 'allow' to auto-approve, 'prompt' to require \
		 confirmation, or 'deny' to block. Overrides are honored in every approval mode.",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"tools.approvalMode",
		"sv_tools_approval_mode",
		Interaction,
		"Approvals",
		"Tool Approval",
		"Default approval behavior for tool calls. 'Always ask' auto-approves read-only tools only. \
		 'Write' auto-approves read and workspace-write tools. 'Yolo' auto-approves all tiers; user \
		 policy may still prompt or block.",
		UiWidget::Submenu(&[
			UiOption::new(
				"always-ask",
				"Always ask",
				"Auto-approve read-only tools; require confirmation for write and exec tools."
			),
			UiOption::new(
				"write",
				"Write",
				"Auto-approve read-only and write tools; require confirmation for exec tools such as \
				 bash, eval, browser, and task."
			),
			UiOption::new(
				"yolo",
				"Yolo",
				"Auto-approve read, write, and exec tools. User policy can still require confirmation \
				 or block calls."
			)
		]),
		None,
		Identity
	),
	ui!(
		"completion.notify",
		"cl_notify_completion",
		Interaction,
		"Notifications",
		"Completion Notification",
		"Notify when the agent finishes a turn",
		UiWidget::Enum(&["on", "off"]),
		None,
		OnOffBoolean
	),
	ui!(
		"error.notify",
		"cl_notify_error",
		Interaction,
		"Notifications",
		"Error Notification",
		"Notify when the agent stops with an error",
		UiWidget::Enum(&["on", "off"]),
		None,
		OnOffBoolean
	),
	ui!(
		"ask.timeout",
		"cl_ask_timeout",
		Interaction,
		"Notifications",
		"Ask Timeout",
		"Auto-select the recommended ask option after this many seconds (0 disables)",
		UiWidget::Submenu(&[
			UiOption::new("0", "Disabled", ""),
			UiOption::new("15", "15 seconds", ""),
			UiOption::new("30", "30 seconds", ""),
			UiOption::new("60", "60 seconds", ""),
			UiOption::new("120", "120 seconds", "")
		]),
		None,
		Identity
	),
	ui!(
		"ask.notify",
		"cl_notify_ask",
		Interaction,
		"Notifications",
		"Ask Notification",
		"Notify when the ask tool is waiting for input",
		UiWidget::Enum(&["on", "off"]),
		None,
		OnOffBoolean
	),
	ui!(
		"stt.enabled",
		"cl_voice_stt_enabled",
		Interaction,
		"Speech",
		"Speech-to-Text",
		"Enable speech-to-text input via microphone",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"stt.modelName",
		"cl_stt_model",
		Interaction,
		"Speech",
		"Speech Model",
		"Local on-device speech model. Parakeet TDT v3 (sherpa-onnx) is the SoTA default; Whisper \
		 base/small/large-v3-turbo tiers (transformers.js) trade size for multilingual coverage. \
		 Downloaded on first use.",
		UiWidget::Submenu(&[
			UiOption::new(
				"fast",
				"Fast (Whisper base)",
				"Whisper base, multilingual. Smallest + fastest; lowest accuracy. Best for \
				 low-resource machines."
			),
			UiOption::new(
				"balanced",
				"Balanced (Whisper small)",
				"Whisper small, multilingual. More accurate than Fast, still light on CPU/RAM."
			),
			UiOption::new(
				"turbo",
				"Turbo (Whisper large-v3)",
				"Whisper large-v3-turbo, 99 languages. Widest language coverage; large download, \
				 slower."
			),
			UiOption::new(
				"parakeet",
				"Parakeet TDT v3 (SoTA)",
				"NVIDIA Parakeet TDT 0.6B v3, 25 languages. Open ASR Leaderboard leader — best \
				 accuracy and far fastest decoding. Default."
			)
		]),
		None,
		Identity
	),
	ui!(
		"stt.submitTrigger",
		"cl_stt_submit_trigger",
		Interaction,
		"Speech",
		"Speech-to-Text Submit Trigger",
		"Choose when speech dictation automatically submits: Never, Release (2+ words), Release \
		 with complete sentence, or When I Say Submit.",
		UiWidget::Submenu(&[
			UiOption::new(
				"never",
				"Never",
				"Never automatically submit; insert dictation and remain in editor."
			),
			UiOption::new(
				"release",
				"Release",
				"Submit on release if the utterance has 2+ words to avoid accidental sends."
			),
			UiOption::new(
				"release-complete",
				"Release with complete sentence",
				"Submit on release if the utterance ends with sentence-terminal punctuation (. ? ! \
				 etc.)."
			),
			UiOption::new(
				"say-submit",
				"When I Say Submit",
				"Submit if the utterance ends with a word containing 'submit' (strips that word \
				 before submitting)."
			)
		]),
		None,
		Identity
	),
	ui!(
		"collab.displayName",
		"cl_collab_display_name",
		Interaction,
		"Collab",
		"Display Name",
		"Name shown to other collab participants (default: OS username)",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"share.serverUrl",
		"sv_share_server",
		Interaction,
		"Collab",
		"Share Server",
		"Share viewer/upload base used by /share (encrypted blob upload + viewer; links are \
		 <base>/<id>#<key>)",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"share.store",
		"sv_share_store",
		Interaction,
		"Collab",
		"Share Store",
		"Where /share uploads the encrypted session blob",
		UiWidget::Submenu(&[
			UiOption::new(
				"blob",
				"Encrypted Blob",
				"Upload to the share server (no GitHub account needed; avoids gist API rate limits)"
			),
			UiOption::new(
				"gist",
				"GitHub Gist",
				"Push to a secret gist (needs authenticated gh), falling back to the share server"
			)
		]),
		None,
		Identity
	),
	ui!(
		"share.redactSecrets",
		"sv_share_redact_secrets",
		Interaction,
		"Collab",
		"Share Secret Redaction",
		"Run the secret obfuscator over /share snapshots before upload (uses the secrets.* config)",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"contextPromotion.enabled",
		"ai_context_promotion_enabled",
		Context,
		"General",
		"Auto-Promote Context",
		"Promote to a larger-context model on context overflow instead of compacting",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"compaction.enabled",
		"ai_compaction_enabled",
		Context,
		"Compaction",
		"Auto-Compact",
		"Automatically compact context when it gets too large",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"compaction.thresholdPercent",
		"ai_compact_threshold",
		Context,
		"Compaction",
		"Compaction Threshold",
		"Percent threshold for context maintenance; set to Default to use legacy reserve-based \
		 behavior",
		UiWidget::Submenu(&[
			UiOption::new("default", "Default", "Legacy reserve-based threshold"),
			UiOption::new("10", "10%", "Extremely early maintenance"),
			UiOption::new("20", "20%", "Very early maintenance"),
			UiOption::new("30", "30%", "Early maintenance"),
			UiOption::new("40", "40%", "Moderately early maintenance"),
			UiOption::new("50", "50%", "Halfway point"),
			UiOption::new("60", "60%", "Moderate context usage"),
			UiOption::new("70", "70%", "Balanced"),
			UiOption::new("75", "75%", "Slightly aggressive"),
			UiOption::new("80", "80%", "Typical threshold"),
			UiOption::new("85", "85%", "Aggressive context usage"),
			UiOption::new("90", "90%", "Very aggressive"),
			UiOption::new("95", "95%", "Near context limit")
		]),
		None,
		PercentFraction
	),
	ui!(
		"memory.backend",
		"ai_memory_backend",
		Memory,
		"General",
		"Memory Backend",
		"Off, local summary pipeline, Mnemopi SQLite, Hindsight remote memory, or Sharpshooter",
		UiWidget::Submenu(&[
			UiOption::new("off", "Off", "No memory subsystem runs"),
			UiOption::new(
				"local",
				"Local",
				"Local rollout summarisation pipeline (memory_summary.md)"
			),
			UiOption::new("hindsight", "Hindsight", "Vectorize Hindsight remote memory service"),
			UiOption::new(
				"mnemopi",
				"Mnemopi",
				"Local SQLite recall/retain backend with optional embeddings"
			),
			UiOption::new(
				"sharpshooter",
				"Sharpshooter",
				"Friction-gated project decision files (architecture/product/style), consolidated in \
				 the background"
			)
		]),
		None,
		Identity
	),
	ui!(
		"providers.memoryModel",
		"ai_memory_selector",
		Memory,
		"General",
		"Memory Model",
		"Mnemopi LLM for fact extraction + consolidation: online (the TINY role from /models, else \
		 smol/remote) by default, or a local on-device model",
		UiWidget::Submenu(&[
			UiOption::new(
				"online",
				"Online (TINY role, else @smol)",
				"Use the online model: the TINY role from /models when set, otherwise @smol. No local \
				 model download or on-device inference."
			),
			UiOption::new(
				"qwen3-1.7b",
				"Qwen3 1.7B",
				"MLX only (providers.tinyModelDevice=mlx): onnxruntime-node cannot run this ONNX \
				 export's RotaryEmbedding cache updates."
			),
			UiOption::new(
				"llama3.2:3b",
				"Llama 3.2 3B",
				"Larger Llama 3.2 option for local memory/classifier tasks; higher quality potential \
				 at higher disk/RAM/latency cost."
			),
			UiOption::new(
				"gemma-3-1b",
				"Gemma 3 1B",
				"Best consolidation/dedup; lighter footprint, but leaks small talk during extraction."
			),
			UiOption::new(
				"qwen2.5-1.5b",
				"Qwen2.5 1.5B",
				"Best extraction granularity (atomic facts); weaker consolidation."
			),
			UiOption::new(
				"lfm2-1.2b",
				"LFM2 1.2B",
				"Fastest load; solid all-rounder, slightly noisier extraction labels."
			)
		]),
		Some(UiCondition::MnemopiActive),
		Identity
	),
	ui!(
		"autolearn.enabled",
		"ai_autolearn_enabled",
		Memory,
		"Auto-Learn",
		"Auto-Learn (experimental)",
		"After the agent stops, nudge it to capture lessons to memory and create/enhance isolated \
		 managed skills",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"mnemopi.dbPath",
		"ai_mnemopi_db_path",
		Memory,
		"Mnemopi",
		"Mnemopi DB Path",
		"Optional SQLite DB path. Defaults to the agent memories directory.",
		UiWidget::Text { secret: false },
		Some(UiCondition::MnemopiActive),
		Identity
	),
	ui!(
		"mnemopi.scoping",
		"ai_mnemopi_scoping",
		Memory,
		"Mnemopi",
		"Mnemopi Scoping",
		"global = one shared bank; per-project = isolated bank per cwd; per-project-tagged = \
		 project-local writes plus global recall visibility",
		UiWidget::Submenu(&[
			UiOption::new("global", "Global", "One shared Mnemopi bank for every project"),
			UiOption::new("per-project", "Per project", "Project-local Mnemopi bank per cwd basename"),
			UiOption::new(
				"per-project-tagged",
				"Per project (tagged)",
				"Write to a project-local bank but merge project + shared recall results"
			)
		]),
		Some(UiCondition::MnemopiActive),
		Identity
	),
	ui!(
		"edit.mode",
		"sv_tools_edit_dialect",
		Files,
		"Editing",
		"Edit Mode",
		"Select the edit tool variant (replace, patch, hashline, or apply_patch)",
		UiWidget::Enum(&["apply_patch", "hashline", "patch", "replace", "sloppy"]),
		None,
		Identity
	),
	ui!(
		"edit.fuzzyMatch",
		"sv_tools_edit_fuzzy",
		Files,
		"Editing",
		"Fuzzy Match",
		"Accept high-confidence fuzzy matches for whitespace differences",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"edit.fuzzyThreshold",
		"sv_edit_fuzzy_threshold",
		Files,
		"Editing",
		"Fuzzy Match Threshold",
		"Similarity threshold (0-1) for accepting fuzzy matches",
		UiWidget::Submenu(&[
			UiOption::new("0.85", "0.85", "Lenient"),
			UiOption::new("0.90", "0.90", "Moderate"),
			UiOption::new("0.95", "0.95", "Default"),
			UiOption::new("0.98", "0.98", "Strict")
		]),
		None,
		Identity
	),
	ui!(
		"edit.streamingAbort",
		"sv_tools_edit_streaming_abort",
		Files,
		"Editing",
		"Abort on Failed Preview",
		"Abort streaming edit tool calls when patch preview fails",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"edit.recoverInlineEdits",
		"sv_edit_recover_inline_edits",
		Files,
		"Editing",
		"Recover Inline Edit Payloads",
		"Execute edit payloads the model emits as plain text by converting them into edit tool calls",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"edit.blockAutoGenerated",
		"sv_tools_edit_guard_generated",
		Files,
		"Editing",
		"Block Auto-Generated Files",
		"Prevent editing of files that appear to be auto-generated (protoc, sqlc, swagger, etc.)",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"edit.blackbox.enabled",
		"sv_tools_edit_blackbox_enabled",
		Files,
		"Editing",
		"Record Parse Regressions",
		"Append full before/after source when an edit introduces an AST parse failure",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"edit.autoRepair.enabled",
		"sv_tools_edit_auto_repair",
		Files,
		"Editing",
		"Auto-Repair Parse Regressions",
		"When an edit breaks a file's AST parse, ask the smol model to fix the broken region \
		 (validated by re-parse; falls back to a warning)",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"readLineNumbers",
		"sv_tools_read_line_numbers",
		Files,
		"Reading",
		"Line Numbers",
		"Prepend line numbers to read tool output by default",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"read.renderMarkdown",
		"cl_read_render_markdown",
		Files,
		"Reading",
		"Markdown Previews",
		"Render Markdown read results as formatted terminal Markdown previews instead of raw source",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"read.summarize.enabled",
		"sv_tools_read_summarize",
		Files,
		"Read Summaries",
		"Read Summaries",
		"Return structural code summaries when read is called without an explicit selector",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"lsp.enabled",
		"sv_lsp_enabled",
		Files,
		"LSP",
		"LSP",
		"Enable the lsp tool for code intelligence (definitions, references, diagnostics, rename)",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"lsp.lazy",
		"sv_lsp_lazy",
		Files,
		"LSP",
		"Lazy LSP Startup",
		"Start language servers on first use (lsp tool or editing a matching file type) instead of \
		 at session startup",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"lsp.formatOnWrite",
		"sv_lsp_format_on_write",
		Files,
		"LSP",
		"Format on Write",
		"Automatically format code files using LSP after writing",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"lsp.diagnosticsOnWrite",
		"sv_lsp_diagnostics_on_write",
		Files,
		"LSP",
		"Diagnostics on Write",
		"Return LSP diagnostics after writing code files",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"lsp.diagnosticsOnEdit",
		"sv_lsp_diagnostics_on_edit",
		Files,
		"LSP",
		"Diagnostics on Edit",
		"Return LSP diagnostics after editing code files",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"lsp.diagnosticsDeduplicate",
		"sv_lsp_diagnostics_deduplicate",
		Files,
		"LSP",
		"Deduplicate Diagnostics",
		"Suppress post-edit LSP diagnostics already shown for a file; only surface new or changed \
		 ones",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"bash.enabled",
		"sv_shell_enabled",
		Shell,
		"Bash",
		"Bash",
		"Enable the bash tool for shell command execution",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"bash.autoBackground.enabled",
		"sv_shell_auto_background_enabled",
		Shell,
		"Bash",
		"Bash Auto-Background",
		"Automatically background long-running bash commands and deliver the result later",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"bashInterceptor.enabled",
		"sv_shell_interceptor_enabled",
		Shell,
		"Bash",
		"Bash Interceptor",
		"Block shell commands that have dedicated tools",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"bash.direnv",
		"sv_shell_direnv",
		Shell,
		"Bash",
		"direnv Auto-Load",
		"Auto-load a repo's direnv/devenv `.envrc` into the bash session so devenv tools and env \
		 vars are present without manual `direnv exec`. Honors direnv's allow list: an `.envrc` you \
		 haven't `direnv allow`ed is never executed",
		UiWidget::Enum(&["auto", "off"]),
		None,
		Identity
	),
	ui!(
		"eval.py",
		"sv_eval_py",
		Shell,
		"Eval & Runtimes",
		"Python Eval Backend",
		"Allow the eval tool to dispatch Python cells to the IPython kernel",
		UiWidget::Boolean,
		None,
		Identity
	),
];
