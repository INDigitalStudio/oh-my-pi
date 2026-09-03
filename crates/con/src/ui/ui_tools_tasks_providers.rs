//! Mechanical projection of current pi settings UI metadata.

use super::*;

pub(super) const ENTRIES: &[UiSpec] = &[
	ui!(
		"astGrep.enabled",
		"sv_ast_grep_enabled",
		Tools,
		"Available Tools",
		"AST Grep",
		"Enable the ast_grep tool for structural AST search",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"inspect_image.mode",
		"ai_vision",
		Tools,
		"Available Tools",
		"Inspect Image",
		"Controls the inspect_image tool, which delegates image understanding to a vision-capable \
		 model. 'auto' exposes it only when the active model lacks native image input; 'on' always \
		 exposes it; 'off' never does.",
		UiWidget::Submenu(&[
			UiOption::new("auto", "Auto (only for models without vision)", ""),
			UiOption::new("on", "On", ""),
			UiOption::new("off", "Off", "")
		]),
		None,
		Identity
	),
	ui!(
		"fetch.enabled",
		"sv_fetch_enabled",
		Tools,
		"Available Tools",
		"Read URLs",
		"Allow the read tool to fetch and process URLs",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"browser.enabled",
		"sv_browser_enabled",
		Tools,
		"Available Tools",
		"Browser",
		"Enable the browser tool for scripted Chromium automation (puppeteer)",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"grep.contextAfter",
		"sv_tools_grep_context_after",
		Tools,
		"Grep & Browser",
		"Grep Context After",
		"Lines of context after each grep match",
		UiWidget::Submenu(&[
			UiOption::new("0", "0 lines", ""),
			UiOption::new("1", "1 line", ""),
			UiOption::new("2", "2 lines", ""),
			UiOption::new("3", "3 lines", ""),
			UiOption::new("5", "5 lines", ""),
			UiOption::new("10", "10 lines", "")
		]),
		None,
		Identity
	),
	ui!(
		"browser.headless",
		"sv_browser_headless",
		Tools,
		"Grep & Browser",
		"Headless Browser",
		"Launch browser in headless mode (disable to show browser UI)",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"tools.artifactSpillThreshold",
		"sv_tools_output_spill_bytes",
		Tools,
		"Output Limits",
		"Artifact Spill Threshold (KB)",
		"Tool output above this size is saved as an artifact; tail is kept inline",
		UiWidget::Submenu(&[
			UiOption::new("1", "1 KB", "~250 tokens"),
			UiOption::new("2.5", "2.5 KB", "~625 tokens"),
			UiOption::new("5", "5 KB", "~1.25K tokens"),
			UiOption::new("10", "10 KB", "~2.5K tokens"),
			UiOption::new("20", "20 KB", "~5K tokens"),
			UiOption::new("30", "30 KB", "~7.5K tokens"),
			UiOption::new("50", "50 KB", "Default; ~12.5K tokens"),
			UiOption::new("75", "75 KB", "~19K tokens"),
			UiOption::new("100", "100 KB", "~25K tokens"),
			UiOption::new("200", "200 KB", "~50K tokens"),
			UiOption::new("500", "500 KB", "~125K tokens"),
			UiOption::new("1000", "1 MB", "~250K tokens")
		]),
		None,
		Kibibytes
	),
	ui!(
		"tools.intentTracing",
		"sv_tools_intent_tracing",
		Tools,
		"Execution",
		"Intent Tracing",
		"Ask the agent to describe the intent of each tool call before executing it",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"tools.maxTimeout",
		"sv_tools_max_timeout",
		Tools,
		"Execution",
		"Max Tool Timeout",
		"Maximum timeout in seconds the agent can set for any tool (0 = no limit)",
		UiWidget::Submenu(&[
			UiOption::new("0", "No limit", ""),
			UiOption::new("30", "30 seconds", ""),
			UiOption::new("60", "60 seconds", ""),
			UiOption::new("120", "120 seconds", ""),
			UiOption::new("300", "5 minutes", ""),
			UiOption::new("600", "10 minutes", "")
		]),
		None,
		SecondsDuration
	),
	ui!(
		"async.enabled",
		"sv_async_enabled",
		Tools,
		"Execution",
		"Async Execution",
		"Enable async bash commands and background task execution",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"async.pollWaitDuration",
		"sv_async_poll_wait_duration",
		Tools,
		"Execution",
		"Max Poll Time",
		"How long a `hub` wait watches background jobs before returning the current state. A fixed \
		 value waits that exact duration every time. `smart` adapts: it starts at 5s and lengthens \
		 with each back-to-back wait (up to 5m), then resets to 5s after about a minute without \
		 waiting.",
		UiWidget::Submenu(&[
			UiOption::new("5s", "5 seconds", ""),
			UiOption::new("10s", "10 seconds", ""),
			UiOption::new("30s", "30 seconds", ""),
			UiOption::new("1m", "1 minute", ""),
			UiOption::new("5m", "5 minutes", ""),
			UiOption::new("smart", "Smart", "Default — adaptive 5s→5m, resets when you stop polling")
		]),
		None,
		Identity
	),
	ui!(
		"mcp.enableProjectConfig",
		"sv_mcp_enable_project_config",
		Tools,
		"Discovery & MCP",
		"MCP Project Config",
		"Load .mcp.json/mcp.json from project root",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"goal.enabled",
		"cl_goal_enabled",
		Tasks,
		"Modes",
		"Goal Mode",
		"Enable per-session goal mode and the hidden goal tool",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"task.eager",
		"sv_task_eager",
		Tasks,
		"Subagents",
		"Prefer Task Delegation",
		"How strongly to push delegating work to subagents",
		UiWidget::Submenu(&[
			UiOption::new(
				"default",
				"Default",
				"Uses the selected model's policy; some models require an explicit delegation request"
			),
			UiOption::new("preferred", "Preferred", "Adds delegation guidance to the system prompt"),
			UiOption::new("always", "Always", "Prompt guidance plus a first-turn delegation reminder")
		]),
		None,
		Identity
	),
	ui!(
		"task.maxConcurrency",
		"sv_task_max_concurrency",
		Tasks,
		"Subagents",
		"Max Concurrent Tasks",
		"Maximum number of subagents running concurrently",
		UiWidget::Submenu(&[
			UiOption::new("0", "Unlimited", ""),
			UiOption::new("1", "1 task", ""),
			UiOption::new("2", "2 tasks", ""),
			UiOption::new("4", "4 tasks", ""),
			UiOption::new("8", "8 tasks", ""),
			UiOption::new("16", "16 tasks", ""),
			UiOption::new("32", "32 tasks", ""),
			UiOption::new("64", "64 tasks", "")
		]),
		None,
		Identity
	),
	ui!(
		"task.enableLsp",
		"sv_task_enable_lsp",
		Tasks,
		"Subagents",
		"LSP in Subagents",
		"Allow subagents spawned via the task tool to use the lsp tool. Off by default to keep \
		 subagents cheap; enable when LSP-aware delegation is worth the extra tokens.",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"task.maxRecursionDepth",
		"sv_task_max_recursion_depth",
		Tasks,
		"Subagents",
		"Max Task Recursion",
		"How many levels deep subagents can spawn their own subagents",
		UiWidget::Submenu(&[
			UiOption::new("-1", "Unlimited", ""),
			UiOption::new("0", "None", ""),
			UiOption::new("1", "Single", ""),
			UiOption::new("2", "Double", ""),
			UiOption::new("3", "Triple", "")
		]),
		None,
		Identity
	),
	ui!(
		"task.maxRuntimeMs",
		"sv_task_max_runtime",
		Tasks,
		"Subagents",
		"Max Subagent Runtime",
		"Hard wall-clock limit per subagent (ms). 0 disables it. Defense-in-depth against \
		 provider-side stream hangs that escape the inference-layer watchdog; triggers a normal \
		 subagent abort with a 'timed out' reason.",
		UiWidget::Submenu(&[
			UiOption::new("0", "Unlimited", "Default"),
			UiOption::new("300000", "5 minutes", ""),
			UiOption::new("900000", "15 minutes", ""),
			UiOption::new("1800000", "30 minutes", ""),
			UiOption::new("3600000", "1 hour", "")
		]),
		None,
		MillisecondsDuration
	),
	ui!(
		"task.softRequestBudget",
		"sv_task_soft_request_budget",
		Tasks,
		"Subagents",
		"Soft Subagent Request Budget",
		"Soft per-subagent request budget (assistant requests per run). Crossing it injects a \
		 wrap-up steering notice (see task.softRequestBudgetNotice); at 1.5x the budget the run is \
		 force-stopped and the agent must yield its partial findings. 0 disables the guard. Bundled \
		 scout/sonic agents cap out at a lower built-in budget, so a value below that cap still \
		 applies to them.",
		UiWidget::Submenu(&[
			UiOption::new("0", "Disabled", ""),
			UiOption::new("90", "90 requests", ""),
			UiOption::new("150", "150 requests", ""),
			UiOption::new("200", "200 requests", "Default")
		]),
		None,
		Identity
	),
	ui!(
		"task.softRequestBudgetNotice",
		"sv_task_soft_request_budget_notice",
		Tasks,
		"Subagents",
		"Soft Request Budget Notice",
		"Inject one steering notice when a subagent crosses its soft request budget, asking it to \
		 wrap up before the 1.5x forced-yield stop.",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"task.maxEffort",
		"sv_task_max_effort",
		Tasks,
		"Subagents",
		"Maximum Per-Spawn Effort",
		"Maximum reasoning effort allowed for the task tool's per-spawn effort hint. Lower values \
		 prevent callers from escalating subagents above this ceiling; the default preserves the \
		 model's full range.",
		UiWidget::Submenu(&[
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
		"task.isolation.enabled",
		"sv_task_isolation_mode",
		Tasks,
		"Isolation",
		"Isolate Subagents",
		"Run subagents in an isolated copy of the checkout and integrate their changes afterwards",
		UiWidget::Boolean,
		None,
		IsolationEnabled
	),
	ui!(
		"isolation.backend",
		"sv_task_isolation_mode",
		Tasks,
		"Isolation",
		"Isolation Backend",
		"Backend used for subagent isolation and worktree cloning",
		UiWidget::Submenu(&[
			UiOption::new("auto", "Auto", "Let the PAL pick the best available backend"),
			UiOption::new("apfs", "APFS", "macOS clonefile reflink (APFS)"),
			UiOption::new("btrfs", "btrfs", "btrfs subvolume snapshot"),
			UiOption::new("zfs", "ZFS", "ZFS snapshot + clone"),
			UiOption::new("reflink", "Reflink", "Linux FICLONE per-file reflink"),
			UiOption::new(
				"overlayfs",
				"Overlayfs",
				"Linux kernel overlay (or fuse-overlayfs fallback)"
			),
			UiOption::new("projfs", "ProjFS", "Windows Projected File System"),
			UiOption::new(
				"block-clone",
				"Block clone",
				"Windows FSCTL_DUPLICATE_EXTENTS_TO_FILE (NTFS/ReFS)"
			),
			UiOption::new(
				"rcopy",
				"Recursive copy",
				"git worktree if available, otherwise recursive copy"
			)
		]),
		None,
		Identity
	),
	ui!(
		"task.isolation.apply",
		"sv_task_isolation_apply",
		Tasks,
		"Isolation",
		"Apply Isolated Changes",
		"Automatically apply successful isolated task changes to the parent checkout; disable to \
		 retain patch or branch artifacts",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"task.isolation.merge",
		"sv_task_isolation_merge",
		Tasks,
		"Isolation",
		"Isolation Merge Strategy",
		"How isolated task changes are integrated (patch apply or branch merge)",
		UiWidget::Submenu(&[
			UiOption::new("patch", "Patch", "Combine diffs and git apply"),
			UiOption::new("branch", "Branch", "Commit per task, merge with --no-ff")
		]),
		None,
		Identity
	),
	ui!(
		"worktree.base",
		"sv_worktree_base",
		Tasks,
		"Isolation",
		"Worktree Base Directory",
		"Base directory for agent-managed worktrees — task-isolation copies, `github` PR checkouts, \
		 and `omp worktree` cleanup all live here. Unset uses ~/.omp/wt. Must be an absolute or \
		 ~-relative path; relative paths are ignored. The OMP_WORKTREE_DIR env var overrides this.",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"providers.maxInFlightRequests",
		"ai_provider_max_in_flight",
		Providers,
		"Services",
		"Max In-Flight Requests",
		"Maximum concurrent LLM requests per provider id (for example \"openai\" or \"anthropic\"), \
		 shared across local OMP processes with this config root. Omitted providers are unlimited.",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"providers.webSearchOrder",
		"ai_search_order",
		Providers,
		"Services",
		"Web Search Provider Order",
		"Prioritized providers for the web_search tool; unlisted providers retain their default \
		 order afterward",
		UiWidget::MultiSelect {
			options: &[
				UiOption::new(
					"perplexity",
					"Perplexity",
					"Uses auth when configured; explicit selection falls back to anonymous search"
				),
				UiOption::new(
					"gemini",
					"Gemini",
					"Google Search grounding via Gemini (uses google-gemini-cli or google-antigravity \
					 OAuth)"
				),
				UiOption::new(
					"anthropic",
					"Anthropic",
					"Claude's native web_search tool (uses Anthropic OAuth or ANTHROPIC_API_KEY)"
				),
				UiOption::new(
					"codex",
					"OpenAI",
					"OpenAI's native web_search (uses ChatGPT OAuth via /login openai-codex)"
				),
				UiOption::new(
					"xai",
					"xAI",
					"Grok web search via xAI Responses API (uses SuperGrok/X Premium+ OAuth via /login \
					 xai-oauth, or XAI_API_KEY)"
				),
				UiOption::new("zai", "Z.AI", "Calls Z.AI webSearchPrime MCP"),
				UiOption::new(
					"exa",
					"Exa",
					"API via /login exa or EXA_API_KEY; explicit keyless fallback via MCP"
				),
				UiOption::new("tinyfish", "TinyFish", "Requires TINYFISH_API_KEY"),
				UiOption::new("jina", "Jina", "Requires JINA_API_KEY"),
				UiOption::new("kagi", "Kagi", "Requires KAGI_API_KEY and Kagi Search API beta access"),
				UiOption::new("tavily", "Tavily", "Requires TAVILY_API_KEY"),
				UiOption::new(
					"firecrawl",
					"Firecrawl",
					"Uses Firecrawl API when FIRECRAWL_API_KEY is set; falls back to keyless mode"
				),
				UiOption::new("brave", "Brave", "Requires BRAVE_API_KEY"),
				UiOption::new(
					"kimi",
					"Kimi",
					"Kimi Code search (requires a Kimi Code Console key via \
					 KIMI_SEARCH_API_KEY/MOONSHOT_SEARCH_API_KEY or /login kimi-code; not \
					 MOONSHOT_API_KEY)"
				),
				UiOption::new("parallel", "Parallel", "Requires PARALLEL_API_KEY"),
				UiOption::new("synthetic", "Synthetic", "Requires SYNTHETIC_API_KEY"),
				UiOption::new("searxng", "SearXNG", "Requires SEARXNG_ENDPOINT or searxng.endpoint"),
				UiOption::new(
					"startpage",
					"Startpage",
					"Credential-free scrape of Startpage (Google-backed) results; may be bot-challenged"
				),
				UiOption::new(
					"duckduckgo",
					"DuckDuckGo",
					"Credential-free best-effort fallback; may be bot-challenged on \
					 datacenter/shared-egress IPs"
				),
				UiOption::new(
					"ecosia",
					"Ecosia",
					"Credential-free browser-backed scrape of Ecosia (Google-backed) results"
				),
				UiOption::new(
					"google",
					"Google",
					"Credential-free browser-backed fallback; slower and may be bot-challenged"
				),
				UiOption::new(
					"mojeek",
					"Mojeek",
					"Credential-free browser-backed scrape of Mojeek's independent index"
				),
				UiOption::new(
					"public",
					"Public Web",
					"Queries every credential-free engine in parallel and consolidates deduplicated \
					 results"
				)
			],
			ordered: true,
		},
		None,
		Identity
	),
	ui!(
		"providers.webSearchExclude",
		"ai_search_exclusions",
		Providers,
		"Services",
		"Excluded Web Search Providers",
		"Providers that web_search should never use, even as fallbacks",
		UiWidget::MultiSelect {
			options: &[
				UiOption::new(
					"perplexity",
					"Perplexity",
					"Uses auth when configured; explicit selection falls back to anonymous search"
				),
				UiOption::new(
					"gemini",
					"Gemini",
					"Google Search grounding via Gemini (uses google-gemini-cli or google-antigravity \
					 OAuth)"
				),
				UiOption::new(
					"anthropic",
					"Anthropic",
					"Claude's native web_search tool (uses Anthropic OAuth or ANTHROPIC_API_KEY)"
				),
				UiOption::new(
					"codex",
					"OpenAI",
					"OpenAI's native web_search (uses ChatGPT OAuth via /login openai-codex)"
				),
				UiOption::new(
					"xai",
					"xAI",
					"Grok web search via xAI Responses API (uses SuperGrok/X Premium+ OAuth via /login \
					 xai-oauth, or XAI_API_KEY)"
				),
				UiOption::new("zai", "Z.AI", "Calls Z.AI webSearchPrime MCP"),
				UiOption::new(
					"exa",
					"Exa",
					"API via /login exa or EXA_API_KEY; explicit keyless fallback via MCP"
				),
				UiOption::new("tinyfish", "TinyFish", "Requires TINYFISH_API_KEY"),
				UiOption::new("jina", "Jina", "Requires JINA_API_KEY"),
				UiOption::new("kagi", "Kagi", "Requires KAGI_API_KEY and Kagi Search API beta access"),
				UiOption::new("tavily", "Tavily", "Requires TAVILY_API_KEY"),
				UiOption::new(
					"firecrawl",
					"Firecrawl",
					"Uses Firecrawl API when FIRECRAWL_API_KEY is set; falls back to keyless mode"
				),
				UiOption::new("brave", "Brave", "Requires BRAVE_API_KEY"),
				UiOption::new(
					"kimi",
					"Kimi",
					"Kimi Code search (requires a Kimi Code Console key via \
					 KIMI_SEARCH_API_KEY/MOONSHOT_SEARCH_API_KEY or /login kimi-code; not \
					 MOONSHOT_API_KEY)"
				),
				UiOption::new("parallel", "Parallel", "Requires PARALLEL_API_KEY"),
				UiOption::new("synthetic", "Synthetic", "Requires SYNTHETIC_API_KEY"),
				UiOption::new("searxng", "SearXNG", "Requires SEARXNG_ENDPOINT or searxng.endpoint"),
				UiOption::new(
					"startpage",
					"Startpage",
					"Credential-free scrape of Startpage (Google-backed) results; may be bot-challenged"
				),
				UiOption::new(
					"duckduckgo",
					"DuckDuckGo",
					"Credential-free best-effort fallback; may be bot-challenged on \
					 datacenter/shared-egress IPs"
				),
				UiOption::new(
					"ecosia",
					"Ecosia",
					"Credential-free browser-backed scrape of Ecosia (Google-backed) results"
				),
				UiOption::new(
					"google",
					"Google",
					"Credential-free browser-backed fallback; slower and may be bot-challenged"
				),
				UiOption::new(
					"mojeek",
					"Mojeek",
					"Credential-free browser-backed scrape of Mojeek's independent index"
				),
				UiOption::new(
					"public",
					"Public Web",
					"Queries every credential-free engine in parallel and consolidates deduplicated \
					 results"
				)
			],
			ordered: false,
		},
		None,
		Identity
	),
	ui!(
		"providers.webSearchTimeoutSeconds",
		"ai_search_timeout_seconds",
		Providers,
		"Services",
		"Web Search Timeout",
		"Hard timeout for each provider's search transport before web_search advances to the next \
		 fallback, in seconds (maximum 300)",
		UiWidget::Submenu(&[
			UiOption::new("30", "30 seconds", ""),
			UiOption::new("60", "1 minute", ""),
			UiOption::new("120", "2 minutes", ""),
			UiOption::new("180", "3 minutes", ""),
			UiOption::new("300", "5 minutes", "")
		]),
		None,
		Identity
	),
	ui!(
		"providers.webSearchGeminiModel",
		"ai_search_gemini_model",
		Providers,
		"Services",
		"Gemini web_search model",
		"Model ID for Gemini Google Search grounding. Defaults to gemini-2.5-flash.",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"providers.antigravityEndpoint",
		"ai_search_antigravity_mode",
		Providers,
		"Services",
		"Antigravity Endpoint Mode",
		"Endpoint routing strategy for google-antigravity providers (chat, search, image, discovery)",
		UiWidget::Submenu(&[
			UiOption::new("auto", "Auto", "Try production endpoint, fail over to sandbox on 5xx/429"),
			UiOption::new("production", "Production Only", "Force production endpoint only"),
			UiOption::new("sandbox", "Sandbox Only", "Force sandbox endpoint only")
		]),
		None,
		Identity
	),
	ui!(
		"tts.localModel",
		"cl_tts_model",
		Providers,
		"Services",
		"Local TTS Model",
		"On-device neural TTS model (Kokoro-82M) used by the local TTS backend",
		UiWidget::Submenu(&[UiOption::new(
			"kokoro",
			"Kokoro-82M",
			"Kokoro-82M neural TTS — SoTA on-device quality, multi-voice, fully local"
		)]),
		None,
		Identity
	),
	ui!(
		"speech.enabled",
		"cl_speech_enabled",
		Providers,
		"Services",
		"Speech Vocalization",
		"Speak the assistant's output aloud through the speakers as it streams",
		UiWidget::Boolean,
		None,
		Identity
	),
	ui!(
		"speech.mode",
		"cl_speech_mode",
		Providers,
		"Services",
		"Speech Vocalization Mode",
		"What to speak: all = assistant messages + thinking; assistant = messages only; yield = \
		 only the final message at turn end",
		UiWidget::Submenu(&[
			UiOption::new("all", "All (messages + thinking)", ""),
			UiOption::new("assistant", "Assistant messages", ""),
			UiOption::new("yield", "Final message only", "")
		]),
		None,
		Identity
	),
	ui!(
		"speech.voice",
		"cl_speech_voice",
		Providers,
		"Services",
		"Speech Vocalization Voice",
		"Kokoro voice used when speaking the assistant's output aloud",
		UiWidget::Submenu(&[
			UiOption::new("af_heart", "Heart (American female)", ""),
			UiOption::new("af_bella", "Bella (American female)", ""),
			UiOption::new("af_nicole", "Nicole (American female)", ""),
			UiOption::new("af_aoede", "Aoede (American female)", ""),
			UiOption::new("af_kore", "Kore (American female)", ""),
			UiOption::new("af_sarah", "Sarah (American female)", ""),
			UiOption::new("am_michael", "Michael (American male)", ""),
			UiOption::new("am_fenrir", "Fenrir (American male)", ""),
			UiOption::new("am_puck", "Puck (American male)", ""),
			UiOption::new("bf_emma", "Emma (British female)", ""),
			UiOption::new("bm_george", "George (British male)", ""),
			UiOption::new("bm_fable", "Fable (British male)", "")
		]),
		None,
		Identity
	),
	ui!(
		"searxng.endpoint",
		"ai_search_searxng_endpoint",
		Providers,
		"Services",
		"SearXNG Endpoint",
		"Base URL of a self-hosted SearXNG instance used for web search",
		UiWidget::Text { secret: false },
		None,
		Identity
	),
	ui!(
		"providers.fireworksTier",
		"ai_tier_fireworks",
		Providers,
		"Fireworks",
		"Fireworks Tier",
		"Serving path for Fireworks requests. Priority sends `service_tier: \"priority\"` for \
		 higher reliability during peak traffic at a higher price; Standard omits it. Fast \
		 (`-fast`) models ignore this — Fast is its own serving path.",
		UiWidget::Submenu(&[
			UiOption::new("standard", "Standard", "Default serving path (no service_tier)"),
			UiOption::new(
				"priority",
				"Priority",
				"Priority serving path: higher reliability, premium per-token pricing"
			)
		]),
		None,
		Identity
	),
	ui!(
		"providers.tinyModel",
		"ai_tiny_selector",
		Providers,
		"Tiny Model",
		"Tiny Model",
		"Session-title model: online (the TINY role from /models, else @smol) by default, or a \
		 local on-device model",
		UiWidget::Submenu(&[
			UiOption::new(
				"online",
				"Online (TINY role, else @smol)",
				"Online title generation: the TINY model role (set one in /models) when assigned, \
				 otherwise the online fallback (commit role, then @smol). No local download or \
				 on-device inference."
			),
			UiOption::new(
				"lfm2.5-230m",
				"LFM2.5 230M",
				"Recommended local model; fastest LFM2.5 option, about 214 MB cached."
			),
			UiOption::new(
				"lfm2.5-350m",
				"LFM2.5 350M",
				"Larger LFM2.5 option, about 292 MB cached; tends toward terse titles."
			),
			UiOption::new(
				"falcon-h1-90m",
				"Falcon H1 Tiny 90M",
				"Smallest option, about 147 MB cached; lower fidelity on complex prompts."
			)
		]),
		None,
		Identity
	),
	ui!(
		"providers.unexpectedStopModel",
		"ai_unexpected_stop_selector",
		Providers,
		"Tiny Model",
		"Unexpected Stop Model",
		"Classifier for Smart unexpected-stop detection: online (the TINY role from /models, else \
		 smol) by default, or a local on-device model.",
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
		Some(UiCondition::UnexpectedStopSmart),
		Identity
	),
	ui!(
		"providers.kimiApiFormat",
		"ai_kimi_api_format",
		Providers,
		"Protocol",
		"Kimi API Format",
		"API format for Kimi Code provider (auto follows live model metadata)",
		UiWidget::Submenu(&[
			UiOption::new("auto", "Auto", "Use the model's server-declared protocol"),
			UiOption::new("openai", "OpenAI", "api.kimi.com"),
			UiOption::new("anthropic", "Anthropic", "api.moonshot.ai")
		]),
		None,
		Identity
	),
	ui!(
		"providers.openaiWebsockets",
		"ai_openai_websockets",
		Providers,
		"Protocol",
		"OpenAI WebSockets",
		"Websocket policy for OpenAI Codex models (auto uses model defaults, on forces, off \
		 disables)",
		UiWidget::Submenu(&[
			UiOption::new("auto", "Auto", "Use model/provider default websocket behavior"),
			UiOption::new("off", "Off", "Disable websockets for OpenAI Codex models"),
			UiOption::new("on", "On", "Force websockets for OpenAI Codex models")
		]),
		None,
		Identity
	),
	ui!(
		"providers.cacheRetention",
		"ai_cache_retention",
		Providers,
		"Protocol",
		"Prompt Cache Retention",
		"Prompt-cache retention forwarded to providers that support it (Anthropic, Bedrock, \
		 OpenRouter, OpenAI)",
		UiWidget::Submenu(&[
			UiOption::new(
				"auto",
				"Auto",
				"Provider default — Anthropic uses 5m entries kept warm by idle keep-alive refreshes; \
				 PI_CACHE_RETENTION still applies"
			),
			UiOption::new(
				"short",
				"Short (5m)",
				"Cheapest cache writes; Anthropic keeps the entry warm with bounded keep-alive \
				 refreshes while idle"
			),
			UiOption::new(
				"long",
				"Long (1h)",
				"1h TTL where the provider supports it; pricier writes, no keep-alive refresh requests"
			),
			UiOption::new("none", "Off", "Disable prompt caching and cache-affinity routing")
		]),
		None,
		Identity
	),
	ui!(
		"providers.openrouterVariant",
		"ai_openrouter_variant",
		Providers,
		"Protocol",
		"OpenRouter Routing",
		"Default routing-variant suffix appended to OpenRouter model IDs (overridden when the \
		 selector already names a variant)",
		UiWidget::Submenu(&[
			UiOption::new("default", "Default", "No suffix; use OpenRouter's default routing"),
			UiOption::new("nitro", ":nitro", "Prioritize throughput / lowest latency"),
			UiOption::new("floor", ":floor", "Prioritize cheapest available provider"),
			UiOption::new("online", ":online", "Enable OpenRouter's web-search plugin"),
			UiOption::new(
				"exacto",
				":exacto",
				"Cherry-picked high-quality providers (only defined for select models)"
			)
		]),
		None,
		Identity
	),
];
