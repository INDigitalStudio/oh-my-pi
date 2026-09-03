//! Mechanical projection of current pi settings UI metadata.

use super::*;

pub(super) const ENTRIES: &[UiSpec] = &[
	ui!(
		"symbolPreset",
		"cl_charset",
		Appearance,
		"Theme",
		"Symbol Preset",
		"Glyph set for icons and symbols (Unicode, Nerd Font, or ASCII)",
		UiWidget::Submenu(&[
			UiOption::new("unicode", "Unicode", "Standard symbols (default)"),
			UiOption::new("nerd", "Nerd Font", "Requires Nerd Font"),
			UiOption::new("ascii", "ASCII", "Maximum compatibility")
		]),
		None,
		Identity
	),
	ui!(
		"statusLine.compactThinkingLevel",
		"cl_status_compact_thinking",
		Appearance,
		"Status Line",
		"Compact Thinking Level",
		"Show the thinking level as a single icon on the model name instead of a separate ` · \
		 <level>` suffix.",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"tui.resizeScrollback",
		"cl_resize_policy",
		Appearance,
		"Display",
		"Resize Scrollback",
		"How a settled terminal resize refreshes transcript rows retained in terminal scrollback",
		UiWidget::Submenu(&[
			UiOption::new(
				"append",
				"Append",
				"Replay the transcript at the new width below retained history"
			),
			UiOption::new(
				"rebuild",
				"Rebuild",
				"Erase all terminal scrollback, then replay one current-width transcript"
			),
			UiOption::new(
				"preserve",
				"Preserve",
				"Repaint only the viewport and keep history wrapped at its old width"
			)
		]),
		None,
		Identity
	),
	ui!(
		"tui.codexResetFireworks",
		"cl_codex_fireworks",
		Appearance,
		"Display",
		"Codex Reset Fireworks",
		"Celebrate unscheduled Codex weekly usage resets and newly banked saved resets with a \
		 top-third fireworks overlay that remains until Escape",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"display.smoothStreaming",
		"cl_smooth_streaming",
		Appearance,
		"Display",
		"Smooth Streaming",
		"Reveal assistant text and streamed tool input smoothly while chunks arrive",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"display.hideToolActivity",
		"cl_showtools",
		Appearance,
		"Display",
		"Hide Tool Activity",
		"Hide model-initiated tool calls and results from the transcript",
		UiWidget::Boolean,
		None,
		InvertedBoolean
	),
	ui!(
		"tui.imeSafeCursor",
		"cl_ime_safe_cursor",
		Appearance,
		"Display",
		"IME-Safe Prompt Layout",
		"Move the prompt's bottom border to a separate row so macOS IME preedit cannot displace it",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"task.showResolvedModelBadge",
		"cl_task_show_resolved_model_badge",
		Appearance,
		"Display",
		"Show Resolved Model Badge",
		"Display the actual model ID used by each subagent in the task widget status line",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"images.autoResize",
		"sv_images_auto_resize",
		Appearance,
		"Images",
		"Auto-Resize Images",
		"Resize large images to 2000x2000 max for better model compatibility",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"defaultThinkingLevel",
		"ai_default_thinking",
		Model,
		"Thinking",
		"Thinking Level",
		"Reasoning depth for thinking-capable models",
		UiWidget::Submenu(&[
			UiOption::new("auto", "auto", "Auto-detect per prompt"),
			UiOption::new("minimal", "min", "Very brief reasoning (~1k tokens)"),
			UiOption::new("low", "low", "Light reasoning (~2k tokens)"),
			UiOption::new("medium", "medium", "Moderate reasoning (~8k tokens)"),
			UiOption::new("high", "high", "Deep reasoning (~16k tokens)"),
			UiOption::new("xhigh", "xhigh", "Extended reasoning (~32k tokens)"),
			UiOption::new("max", "max", "Maximum reasoning the model supports")
		]),
		None,
		Identity
	),
	ui!(
		"hideThinkingBlock",
		"cl_showthinking",
		Model,
		"Thinking",
		"Hide Thinking Blocks",
		"Hide thinking blocks in assistant responses",
		UiWidget::Boolean,
		None,
		InvertedBoolean
	),
	ui!(
		"proseOnlyThinking",
		"cl_thinking_prose_only",
		Model,
		"Thinking",
		"Prose Only Thinking",
		"Omit code blocks from thinking summaries and replace them with an ellipsis",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"providers.autoThinkingModel",
		"ai_auto_thinking_selector",
		Model,
		"Thinking",
		"Auto Thinking Model",
		"Difficulty classifier for the `auto` thinking level: online (the TINY role from /models, \
		 else smol) by default, or a local on-device model",
		UiWidget::Submenu(&[
			UiOption::new(
				"online",
				"Online (TINY role, else @smol)",
				"Classify prompt difficulty online with the TINY role model (set one in /models) or \
				 @smol; no local download or on-device inference."
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
		Some(UiCondition::AutoThinkingActive),
		Identity
	),
	ui!(
		"temperature",
		"ai_sampling_temperature",
		Model,
		"Sampling",
		"Temperature",
		"Sampling temperature (0 = deterministic, 1 = creative, -1 = provider default)",
		UiWidget::Submenu(&[
			UiOption::new("-1", "Default", "Use provider default"),
			UiOption::new("0", "0", "Deterministic"),
			UiOption::new("0.2", "0.2", "Focused"),
			UiOption::new("0.5", "0.5", "Balanced"),
			UiOption::new("0.7", "0.7", "Creative"),
			UiOption::new("1", "1", "Maximum variety")
		]),
		None,
		Identity
	),
	ui!(
		"topP",
		"ai_sampling_top_p",
		Model,
		"Sampling",
		"Top P",
		"Nucleus sampling cutoff (0-1, -1 = provider default)",
		UiWidget::Submenu(&[
			UiOption::new("-1", "Default", "Use provider default"),
			UiOption::new("0.1", "0.1", "Very focused"),
			UiOption::new("0.3", "0.3", "Focused"),
			UiOption::new("0.5", "0.5", "Balanced"),
			UiOption::new("0.9", "0.9", "Broad"),
			UiOption::new("1", "1", "No nucleus filtering")
		]),
		None,
		Identity
	),
	ui!(
		"topK",
		"ai_sampling_top_k",
		Model,
		"Sampling",
		"Top K",
		"Sample from top-K tokens (-1 = provider default)",
		UiWidget::Submenu(&[
			UiOption::new("-1", "Default", "Use provider default"),
			UiOption::new("1", "1", "Greedy top token"),
			UiOption::new("20", "20", "Focused"),
			UiOption::new("40", "40", "Balanced"),
			UiOption::new("100", "100", "Broad")
		]),
		None,
		Identity
	),
	ui!(
		"minP",
		"ai_sampling_min_p",
		Model,
		"Sampling",
		"Min P",
		"Minimum probability threshold (0-1, -1 = provider default)",
		UiWidget::Submenu(&[
			UiOption::new("-1", "Default", "Use provider default"),
			UiOption::new("0.01", "0.01", "Very permissive"),
			UiOption::new("0.05", "0.05", "Balanced"),
			UiOption::new("0.1", "0.1", "Strict")
		]),
		None,
		Identity
	),
	ui!(
		"presencePenalty",
		"ai_sampling_presence_penalty",
		Model,
		"Sampling",
		"Presence Penalty",
		"Penalty for introducing already-present tokens (-1 = provider default)",
		UiWidget::Submenu(&[
			UiOption::new("-1", "Default", "Use provider default"),
			UiOption::new("0", "0", "No penalty"),
			UiOption::new("0.5", "0.5", "Mild novelty"),
			UiOption::new("1", "1", "Encourage novelty"),
			UiOption::new("2", "2", "Strong novelty")
		]),
		None,
		Identity
	),
	ui!(
		"repetitionPenalty",
		"ai_sampling_repetition_penalty",
		Model,
		"Sampling",
		"Repetition Penalty",
		"Penalty for repeated tokens (-1 = provider default)",
		UiWidget::Submenu(&[
			UiOption::new("-1", "Default", "Use provider default"),
			UiOption::new("0.8", "0.8", "Allow repetition"),
			UiOption::new("1", "1", "No penalty"),
			UiOption::new("1.1", "1.1", "Mild penalty"),
			UiOption::new("1.2", "1.2", "Balanced"),
			UiOption::new("1.5", "1.5", "Strong penalty")
		]),
		None,
		Identity
	),
	ui!(
		"textVerbosity",
		"ai_sampling_verbosity",
		Model,
		"Sampling",
		"Text Verbosity",
		"OpenAI Responses and Codex response verbosity (low, medium, or high)",
		UiWidget::Submenu(&[
			UiOption::new("low", "Low", "Prefer concise responses"),
			UiOption::new("medium", "Medium", "Balance brevity and detail (default)"),
			UiOption::new("high", "High", "Prefer detailed responses")
		]),
		None,
		Identity
	),
	ui!(
		"tier.openai",
		"ai_tier_openai",
		Model,
		"Sampling",
		"Service Tier — OpenAI",
		"Processing tier for OpenAI / OpenAI-Codex requests, and OpenAI-family models routed via \
		 OpenRouter (none = omit). Sent as `service_tier`.",
		UiWidget::Submenu(&[
			UiOption::new("none", "None", "Omit service_tier (standard processing)"),
			UiOption::new("auto", "Auto", "Provider default tier selection"),
			UiOption::new("default", "Default", "Standard priority processing"),
			UiOption::new("flex", "Flex", "Lower cost, higher latency when available"),
			UiOption::new("scale", "Scale", "Scale Tier credits when available"),
			UiOption::new("priority", "Priority", "Faster, higher cost (premium request)")
		]),
		None,
		Identity
	),
	ui!(
		"tier.anthropic",
		"ai_tier_anthropic",
		Model,
		"Sampling",
		"Service Tier — Anthropic",
		"Processing tier for Claude requests. `priority` realizes fast mode (`speed: \"fast\"`) on \
		 supported direct Anthropic models; ignored on Bedrock/Vertex Claude and via OpenRouter.",
		UiWidget::Submenu(&[
			UiOption::new("none", "None", "Standard processing"),
			UiOption::new(
				"priority",
				"Priority",
				"Fast mode (`speed: \"fast\"`) on supported direct Claude models; ignored on \
				 Bedrock/Vertex"
			)
		]),
		None,
		Identity
	),
	ui!(
		"tier.google",
		"ai_tier_google",
		Model,
		"Sampling",
		"Service Tier — Google",
		"Processing tier for Gemini (Google AI Studio + Vertex) requests, and Google-family models \
		 routed via OpenRouter (none = omit). Sent as the top-level `serviceTier` field.",
		UiWidget::Submenu(&[
			UiOption::new("none", "None", "Standard processing"),
			UiOption::new("flex", "Flex", "Lower cost, higher latency (Gemini API + Vertex)"),
			UiOption::new("priority", "Priority", "Faster, higher reliability (Gemini API + Vertex)")
		]),
		None,
		Identity
	),
	ui!(
		"modelRoleStorage",
		"ai_model_role_storage",
		Model,
		"Prompt",
		"Model Role Storage",
		"Where model selector role assignments are saved",
		UiWidget::Submenu(&[
			UiOption::new(
				"global",
				"Global",
				"Save role models in the active profile config (current behavior)"
			),
			UiOption::new(
				"project",
				"Per-project",
				"Save project role models in .omp/config.yml; missing project roles use global \
				 defaults"
			)
		]),
		None,
		Identity
	),
	ui!(
		"retry.maxRetries",
		"ai_retry_max_retries",
		Model,
		"Retry & Fallback",
		"Retry Attempts",
		"Maximum retry attempts on API errors",
		UiWidget::Submenu(&[
			UiOption::new("1", "1 retry", ""),
			UiOption::new("2", "2 retries", ""),
			UiOption::new("3", "3 retries", ""),
			UiOption::new("5", "5 retries", ""),
			UiOption::new("10", "10 retries", "")
		]),
		None,
		Identity
	),
	ui!(
		"retry.modelFallback",
		"ai_retry_model_fallback",
		Model,
		"Retry & Fallback",
		"Retry Model Fallback",
		"Allow retry recovery to switch to configured fallback models",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"retry.usageAwareFallback",
		"ai_retry_usage_aware_fallback",
		Model,
		"Retry & Fallback",
		"Usage-Aware Fallback",
		"Use reliable coding-plan quota reports to prefer same-provider accounts, then configured \
		 fallback models, before a hard usage limit. Ordinary configured API keys are excluded.",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"retry.usageReservePct",
		"ai_retry_usage_reserve_pct",
		Model,
		"Retry & Fallback",
		"Reserve Margin",
		"Treat a coding-plan model as near its limit below this remaining percentage. Unknown or \
		 unmapped usage keeps the primary model.",
		UiWidget::Submenu(&[
			UiOption::new("5", "5%", "Act only when nearly exhausted"),
			UiOption::new("10", "10%", "Balanced safety margin"),
			UiOption::new("15", "15%", "Conservative"),
			UiOption::new("20", "20%", "Early protection"),
			UiOption::new("25", "25%", "Very conservative")
		]),
		Some(UiCondition::UsageAwareFallbackEnabled),
		Identity
	),
	ui!(
		"retry.usageReservePolicy",
		"ai_retry_usage_reserve_policy",
		Model,
		"Retry & Fallback",
		"Reserve Policy",
		"What to do when every same-provider coding-plan account is inside the reserve margin.",
		UiWidget::Submenu(&[
			UiOption::new(
				"confirm",
				"Confirm interactively",
				"Keep interactive sessions on the primary until confirmed; background agents \
				 auto-fallback"
			),
			UiOption::new(
				"auto",
				"Auto-fallback",
				"Always select the next eligible configured fallback"
			),
			UiOption::new(
				"fail-closed",
				"Fail closed",
				"Do not spend reserve quota or select a fallback"
			)
		]),
		Some(UiCondition::UsageAwareFallbackEnabled),
		Identity
	),
	ui!(
		"retry.fallbackChains",
		"ai_retry_fallback_chains",
		Model,
		"Retry & Fallback",
		"Retry Fallback Chains",
		"JSON object mapping model roles, model selectors (\"provider/model-id\"), or provider \
		 wildcards (\"provider/*\") to ordered fallback selectors, e.g. \
		 {\"default\":[\"openai/gpt-4o-mini\"],\"google-antigravity/*\":[\"google/*\",\"\
		 google-vertex/*\"]}. Model-oriented keys apply whenever that model/provider is active, \
		 regardless of role; a \"provider/*\" entry keeps the failing model's id and swaps the \
		 provider. An id-prefixed wildcard (\"openrouter/google/*\") re-prefixes the failing \
		 model's bare id (google-antigravity/gemini-x -> openrouter/google/gemini-x) and, used as a \
		 key, matches only that provider's ids under the prefix.",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"retry.fallbackRevertPolicy",
		"ai_retry_fallback_revert",
		Model,
		"Retry & Fallback",
		"Fallback Revert Policy",
		"When to return to the primary model after a fallback",
		UiWidget::Submenu(&[
			UiOption::new(
				"cooldown-expiry",
				"Cooldown expiry",
				"Return to the primary model after its suppression window ends"
			),
			UiOption::new("never", "Never", "Stay on the fallback model until manually changed")
		]),
		None,
		Identity
	),
	ui!(
		"providers.anthropic.serverSideFallback",
		"ai_retry_server_side_fallback",
		Model,
		"Retry & Fallback",
		"Anthropic Server-Side Fallback (Fable 5)",
		"When a Claude Fable 5 / Mythos 5 request is blocked by Anthropic's safety classifier, \
		 retry it on Claude Opus 4.8 server-side (Anthropic `server-side-fallback-2026-06-01` \
		 beta). Opt-in — leaving this off preserves the pre-fallback behavior for every request.",
		UiWidget::Boolean,
		None,
		Identity
	),
];
