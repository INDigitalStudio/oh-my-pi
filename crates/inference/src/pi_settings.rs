//! Literal pi-setting convars not otherwise owned by a narrower runtime module.

use omp_core::Str;

omp_con::var! {
	/// pi `advisor.enabled` (boolean, default: false).
	pub static AI_ADVISOR_ENABLED = ai_advisor_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `prewalk.enabled` (boolean, default: false).
	pub static AI_PREWALK_ENABLED = ai_prewalk_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `advisor.syncBacklog` (enum, default: "off").
	pub static AI_ADVISOR_SYNC_BACKLOG = ai_advisor_sync_backlog: Str {
		default: Str::new_static("off"),
		flags: archive,
	};
	/// pi `advisor.immuneTurns` (number, default: 3).
	pub static AI_ADVISOR_IMMUNE_TURNS = ai_advisor_immune_turns: i64 {
		default: 3,
		flags: archive,
	};
	/// pi `git.enabled` (boolean, default: true).
	pub static AI_GIT_ENABLED = ai_git_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `omitThinking` (boolean, default: false).
	pub static AI_OMIT_THINKING = ai_omit_thinking: bool {
		default: false,
		flags: archive,
	};
	/// pi `externalThinking` (boolean, default: false).
	pub static AI_EXTERNAL_THINKING = ai_external_thinking: bool {
		default: false,
		flags: archive,
	};
	/// pi `model.loopGuard.enabled` (boolean, default: true).
	pub static AI_MODEL_LOOP_GUARD_ENABLED = ai_model_loop_guard_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `model.loopGuard.checkAssistantContent` (boolean, default: true).
	pub static AI_MODEL_LOOP_GUARD_CHECK_ASSISTANT_CONTENT = ai_model_loop_guard_check_assistant_content: bool {
		default: true,
		flags: archive,
	};
	/// pi `model.loopGuard.toolCallReminder` (boolean, default: true).
	pub static AI_MODEL_LOOP_GUARD_TOOL_CALL_REMINDER = ai_model_loop_guard_tool_call_reminder: bool {
		default: true,
		flags: archive,
	};
	/// pi `model.toolCallLoopGuard.enabled` (boolean, default: true).
	pub static AI_MODEL_TOOL_CALL_LOOP_GUARD_ENABLED = ai_model_tool_call_loop_guard_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `model.toolCallLoopGuard.threshold` (number, default: 5).
	pub static AI_MODEL_TOOL_CALL_LOOP_GUARD_THRESHOLD = ai_model_tool_call_loop_guard_threshold: i64 {
		default: 5,
		flags: archive,
	};
	/// pi `model.toolCallLoopGuard.exemptTools` (array, default: DEFAULT_TOOL_CALL_LOOP_EXEMPT_TOOLS).
	pub static AI_MODEL_TOOL_CALL_LOOP_GUARD_EXEMPT_TOOLS = ai_model_tool_call_loop_guard_exempt_tools: Vec<Str> {
		default: vec![Str::new_static("hub")],
		flags: archive,
	};
	/// pi `contextPromotion.enabled` (boolean, default: false).
	pub static AI_CONTEXT_PROMOTION_ENABLED = ai_context_promotion_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `compaction.enabled` (boolean, default: true).
	pub static AI_COMPACTION_ENABLED = ai_compaction_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `compaction.midTurnEnabled` (boolean, default: true).
	pub static AI_COMPACTION_MID_TURN_ENABLED = ai_compaction_mid_turn_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `compaction.methodOrder` (array, default: [...DEFAULT_COMPACTION_METHOD_ORDER]).
	pub static AI_COMPACTION_METHOD_ORDER = ai_compaction_method_order: Vec<Str> {
		default: vec![Str::new_static("remote"), Str::new_static("snapcompact"), Str::new_static("handoff"), Str::new_static("shake"), Str::new_static("soft")],
		flags: archive,
	};
	/// pi `compaction.thresholdTokens` (number, default: -1).
	pub static AI_COMPACTION_THRESHOLD_TOKENS = ai_compaction_threshold_tokens: i64 {
		default: -1,
		flags: archive,
	};
	/// pi `compaction.handoffSaveToDisk` (boolean, default: false).
	pub static AI_COMPACTION_HANDOFF_SAVE_TO_DISK = ai_compaction_handoff_save_to_disk: bool {
		default: false,
		flags: archive,
	};
	/// pi `compaction.remoteStreamingV2Enabled` (boolean, default: true).
	pub static AI_COMPACTION_REMOTE_STREAMING_V2_ENABLED = ai_compaction_remote_streaming_v2_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `compaction.asyncEnabled` (boolean, default: true).
	pub static AI_COMPACTION_ASYNC_ENABLED = ai_compaction_async_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `compaction.reserveTokens` (number, default: undefined).
	pub static AI_COMPACTION_RESERVE_TOKENS = ai_compaction_reserve_tokens: f64 {
		default: -1.0,
		flags: archive,
	};
	/// pi `compaction.keepRecentTokens` (number, default: 20000).
	pub static AI_COMPACTION_KEEP_RECENT_TOKENS = ai_compaction_keep_recent_tokens: i64 {
		default: 20000,
		flags: archive,
	};
	/// pi `compaction.autoContinue` (boolean, default: true).
	pub static AI_COMPACTION_AUTO_CONTINUE = ai_compaction_auto_continue: bool {
		default: true,
		flags: archive,
	};
	/// pi `compaction.remoteEndpoint` (string, default: undefined).
	pub static AI_COMPACTION_REMOTE_ENDPOINT = ai_compaction_remote_endpoint: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `compaction.v2RetainedMessageBudget` (number, default: 64000).
	pub static AI_COMPACTION_V2_RETAINED_MESSAGE_BUDGET = ai_compaction_v2_retained_message_budget: i64 {
		default: 64000,
		flags: archive,
	};
	/// pi `compaction.idleEnabled` (boolean, default: false).
	pub static AI_COMPACTION_IDLE_ENABLED = ai_compaction_idle_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `compaction.idleThresholdTokens` (number, default: 200000).
	pub static AI_COMPACTION_IDLE_THRESHOLD_TOKENS = ai_compaction_idle_threshold_tokens: i64 {
		default: 200000,
		flags: archive,
	};
	/// pi `compaction.idleTimeoutSeconds` (number, default: 300).
	pub static AI_COMPACTION_IDLE_TIMEOUT_SECONDS = ai_compaction_idle_timeout_seconds: i64 {
		default: 300,
		flags: archive,
	};
	/// pi `compaction.supersedeReads` (boolean, default: true).
	pub static AI_COMPACTION_SUPERSEDE_READS = ai_compaction_supersede_reads: bool {
		default: true,
		flags: archive,
	};
	/// pi `compaction.dropUseless` (boolean, default: true).
	pub static AI_COMPACTION_DROP_USELESS = ai_compaction_drop_useless: bool {
		default: true,
		flags: archive,
	};
	/// pi `snapcompact.systemPrompt` (enum, default: "none").
	pub static AI_SNAPCOMPACT_SYSTEM_PROMPT = ai_snapcompact_system_prompt: Str {
		default: Str::new_static("none"),
		flags: archive,
	};
	/// pi `snapcompact.toolResults` (boolean, default: false).
	pub static AI_SNAPCOMPACT_TOOL_RESULTS = ai_snapcompact_tool_results: bool {
		default: false,
		flags: archive,
	};
	/// pi `snapcompact.shape` (enum, default: "auto").
	pub static AI_SNAPCOMPACT_SHAPE = ai_snapcompact_shape: Str {
		default: Str::new_static("auto"),
		flags: archive,
	};
	/// pi `branchSummary.enabled` (boolean, default: false).
	pub static AI_BRANCH_SUMMARY_ENABLED = ai_branch_summary_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `branchSummary.reserveTokens` (number, default: 16384).
	pub static AI_BRANCH_SUMMARY_RESERVE_TOKENS = ai_branch_summary_reserve_tokens: i64 {
		default: 16384,
		flags: archive,
	};
	/// pi `memories.enabled` (boolean, default: false).
	pub static AI_MEMORIES_ENABLED = ai_memories_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `memories.maxRolloutsPerStartup` (number, default: 64).
	pub static AI_MEMORIES_MAX_ROLLOUTS_PER_STARTUP = ai_memories_max_rollouts_per_startup: i64 {
		default: 64,
		flags: archive,
	};
	/// pi `memories.maxRolloutAgeDays` (number, default: 30).
	pub static AI_MEMORIES_MAX_ROLLOUT_AGE_DAYS = ai_memories_max_rollout_age_days: i64 {
		default: 30,
		flags: archive,
	};
	/// pi `memories.minRolloutIdleHours` (number, default: 12).
	pub static AI_MEMORIES_MIN_ROLLOUT_IDLE_HOURS = ai_memories_min_rollout_idle_hours: i64 {
		default: 12,
		flags: archive,
	};
}

omp_con::var! {
	/// pi `memories.threadScanLimit` (number, default: 300).
	pub static AI_MEMORIES_THREAD_SCAN_LIMIT = ai_memories_thread_scan_limit: i64 {
		default: 300,
		flags: archive,
	};
	/// pi `memories.maxRawMemoriesForGlobal` (number, default: 200).
	pub static AI_MEMORIES_MAX_RAW_MEMORIES_FOR_GLOBAL = ai_memories_max_raw_memories_for_global: i64 {
		default: 200,
		flags: archive,
	};
	/// pi `memories.stage1Concurrency` (number, default: 8).
	pub static AI_MEMORIES_STAGE1_CONCURRENCY = ai_memories_stage1_concurrency: i64 {
		default: 8,
		flags: archive,
	};
	/// pi `memories.stage1LeaseSeconds` (number, default: 120).
	pub static AI_MEMORIES_STAGE1_LEASE_SECONDS = ai_memories_stage1_lease_seconds: i64 {
		default: 120,
		flags: archive,
	};
	/// pi `memories.stage1RetryDelaySeconds` (number, default: 120).
	pub static AI_MEMORIES_STAGE1_RETRY_DELAY_SECONDS = ai_memories_stage1_retry_delay_seconds: i64 {
		default: 120,
		flags: archive,
	};
	/// pi `memories.phase2LeaseSeconds` (number, default: 180).
	pub static AI_MEMORIES_PHASE2_LEASE_SECONDS = ai_memories_phase2_lease_seconds: i64 {
		default: 180,
		flags: archive,
	};
	/// pi `memories.phase2RetryDelaySeconds` (number, default: 180).
	pub static AI_MEMORIES_PHASE2_RETRY_DELAY_SECONDS = ai_memories_phase2_retry_delay_seconds: i64 {
		default: 180,
		flags: archive,
	};
	/// pi `memories.phase2HeartbeatSeconds` (number, default: 30).
	pub static AI_MEMORIES_PHASE2_HEARTBEAT_SECONDS = ai_memories_phase2_heartbeat_seconds: i64 {
		default: 30,
		flags: archive,
	};
	/// pi `memories.rolloutPayloadPercent` (number, default: 0.7).
	pub static AI_MEMORIES_ROLLOUT_PAYLOAD_PERCENT = ai_memories_rollout_payload_percent: f64 {
		default: 0.7,
		flags: archive,
	};
	/// pi `memories.phase1InputTokenLimit` (number, default: 4000).
	pub static AI_MEMORIES_PHASE1_INPUT_TOKEN_LIMIT = ai_memories_phase1_input_token_limit: i64 {
		default: 4000,
		flags: archive,
	};
	/// pi `memories.fallbackTokenLimit` (number, default: 16000).
	pub static AI_MEMORIES_FALLBACK_TOKEN_LIMIT = ai_memories_fallback_token_limit: i64 {
		default: 16000,
		flags: archive,
	};
	/// pi `memories.summaryInjectionTokenLimit` (number, default: 5000).
	pub static AI_MEMORIES_SUMMARY_INJECTION_TOKEN_LIMIT = ai_memories_summary_injection_token_limit: i64 {
		default: 5000,
		flags: archive,
	};
	/// pi `sharpshooter.model` (string, default: undefined).
	pub static AI_SHARPSHOOTER_MODEL = ai_sharpshooter_model: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `sharpshooter.intervalMinutes` (number, default: 5).
	pub static AI_SHARPSHOOTER_INTERVAL_MINUTES = ai_sharpshooter_interval_minutes: i64 {
		default: 5,
		flags: archive,
	};
	/// pi `sharpshooter.injectionTokenLimit` (number, default: 15000).
	pub static AI_SHARPSHOOTER_INJECTION_TOKEN_LIMIT = ai_sharpshooter_injection_token_limit: i64 {
		default: 15000,
		flags: archive,
	};
	/// pi `mnemopi.dbPath` (string, default: undefined).
	pub static AI_MNEMOPI_DB_PATH = ai_mnemopi_db_path: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `mnemopi.bank` (string, default: undefined).
	pub static AI_MNEMOPI_BANK = ai_mnemopi_bank: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `mnemopi.embeddingVariant` (enum, default: "en").
	pub static AI_MNEMOPI_EMBEDDING_VARIANT = ai_mnemopi_embedding_variant: Str {
		default: Str::new_static("en"),
		flags: archive,
	};
	/// pi `mnemopi.autoRecall` (boolean, default: true).
	pub static AI_MNEMOPI_AUTO_RECALL = ai_mnemopi_auto_recall: bool {
		default: true,
		flags: archive,
	};
	/// pi `mnemopi.autoRetain` (boolean, default: true).
	pub static AI_MNEMOPI_AUTO_RETAIN = ai_mnemopi_auto_retain: bool {
		default: true,
		flags: archive,
	};
	/// pi `mnemopi.polyphonicRecall` (boolean, default: false).
	pub static AI_MNEMOPI_POLYPHONIC_RECALL = ai_mnemopi_polyphonic_recall: bool {
		default: false,
		flags: archive,
	};
	/// pi `mnemopi.enhancedRecall` (boolean, default: false).
	pub static AI_MNEMOPI_ENHANCED_RECALL = ai_mnemopi_enhanced_recall: bool {
		default: false,
		flags: archive,
	};
	/// pi `mnemopi.proactiveLinking` (boolean, default: false).
	pub static AI_MNEMOPI_PROACTIVE_LINKING = ai_mnemopi_proactive_linking: bool {
		default: false,
		flags: archive,
	};
	/// pi `mnemopi.noEmbeddings` (boolean, default: false).
	pub static AI_MNEMOPI_NO_EMBEDDINGS = ai_mnemopi_no_embeddings: bool {
		default: false,
		flags: archive,
	};
	/// pi `mnemopi.embeddingModel` (string, default: undefined).
	pub static AI_MNEMOPI_EMBEDDING_MODEL = ai_mnemopi_embedding_model: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `mnemopi.embeddingApiUrl` (string, default: undefined).
	pub static AI_MNEMOPI_EMBEDDING_API_URL = ai_mnemopi_embedding_api_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `mnemopi.embeddingApiKey` (string, default: undefined).
	pub static AI_MNEMOPI_EMBEDDING_API_KEY = ai_mnemopi_embedding_api_key: Str {
		default: Str::new_static(""),
	};
	/// pi `mnemopi.llmMode` (enum, default: "smol").
	pub static AI_MNEMOPI_LLM_MODE = ai_mnemopi_llm_mode: Str {
		default: Str::new_static("smol"),
		flags: archive,
	};
	/// pi `mnemopi.llmBaseUrl` (string, default: undefined).
	pub static AI_MNEMOPI_LLM_BASE_URL = ai_mnemopi_llm_base_url: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `mnemopi.llmApiKey` (string, default: undefined).
	pub static AI_MNEMOPI_LLM_API_KEY = ai_mnemopi_llm_api_key: Str {
		default: Str::new_static(""),
	};
	/// pi `mnemopi.llmModel` (string, default: undefined).
	pub static AI_MNEMOPI_LLM_MODEL = ai_mnemopi_llm_model: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `mnemopi.retainEveryNTurns` (number, default: 4).
	pub static AI_MNEMOPI_RETAIN_EVERY_NTURNS = ai_mnemopi_retain_every_nturns: i64 {
		default: 4,
		flags: archive,
	};
	/// pi `mnemopi.recallLimit` (number, default: 8).
	pub static AI_MNEMOPI_RECALL_LIMIT = ai_mnemopi_recall_limit: i64 {
		default: 8,
		flags: archive,
	};
	/// pi `mnemopi.recallContextTurns` (number, default: 3).
	pub static AI_MNEMOPI_RECALL_CONTEXT_TURNS = ai_mnemopi_recall_context_turns: i64 {
		default: 3,
		flags: archive,
	};
	/// pi `mnemopi.recallMaxQueryChars` (number, default: 4000).
	pub static AI_MNEMOPI_RECALL_MAX_QUERY_CHARS = ai_mnemopi_recall_max_query_chars: i64 {
		default: 4000,
		flags: archive,
	};
	/// pi `mnemopi.injectionTokenLimit` (number, default: 5000).
	pub static AI_MNEMOPI_INJECTION_TOKEN_LIMIT = ai_mnemopi_injection_token_limit: i64 {
		default: 5000,
		flags: archive,
	};
	/// pi `mnemopi.debug` (boolean, default: false).
	pub static AI_MNEMOPI_DEBUG = ai_mnemopi_debug: bool {
		default: false,
		flags: archive,
	};
	/// pi `hindsight.apiUrl` (string, default: "http://localhost:8888").
	pub static AI_HINDSIGHT_API_URL = ai_hindsight_api_url: Str {
		default: Str::new_static("http://localhost:8888"),
		flags: archive,
	};
	/// pi `hindsight.apiToken` (string, default: undefined).
	pub static AI_HINDSIGHT_API_TOKEN = ai_hindsight_api_token: Str {
		default: Str::new_static(""),
	};
	/// pi `hindsight.bankId` (string, default: undefined).
	pub static AI_HINDSIGHT_BANK_ID = ai_hindsight_bank_id: Str {
		default: Str::new_static(""),
		flags: archive,
	};
}

omp_con::var! {
	/// pi `hindsight.bankIdPrefix` (string, default: undefined).
	pub static AI_HINDSIGHT_BANK_ID_PREFIX = ai_hindsight_bank_id_prefix: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `hindsight.scoping` (enum, default: "per-project-tagged").
	pub static AI_HINDSIGHT_SCOPING = ai_hindsight_scoping: Str {
		default: Str::new_static("per-project-tagged"),
		flags: archive,
	};
	/// pi `hindsight.bankMission` (string, default: undefined).
	pub static AI_HINDSIGHT_BANK_MISSION = ai_hindsight_bank_mission: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `hindsight.retainMission` (string, default: undefined).
	pub static AI_HINDSIGHT_RETAIN_MISSION = ai_hindsight_retain_mission: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `hindsight.autoRecall` (boolean, default: true).
	pub static AI_HINDSIGHT_AUTO_RECALL = ai_hindsight_auto_recall: bool {
		default: true,
		flags: archive,
	};
	/// pi `hindsight.autoRetain` (boolean, default: true).
	pub static AI_HINDSIGHT_AUTO_RETAIN = ai_hindsight_auto_retain: bool {
		default: true,
		flags: archive,
	};
	/// pi `hindsight.retainMode` (enum, default: "full-session").
	pub static AI_HINDSIGHT_RETAIN_MODE = ai_hindsight_retain_mode: Str {
		default: Str::new_static("full-session"),
		flags: archive,
	};
	/// pi `hindsight.retainEveryNTurns` (number, default: 3).
	pub static AI_HINDSIGHT_RETAIN_EVERY_NTURNS = ai_hindsight_retain_every_nturns: i64 {
		default: 3,
		flags: archive,
	};
	/// pi `hindsight.retainOverlapTurns` (number, default: 2).
	pub static AI_HINDSIGHT_RETAIN_OVERLAP_TURNS = ai_hindsight_retain_overlap_turns: i64 {
		default: 2,
		flags: archive,
	};
	/// pi `hindsight.retainContext` (string, default: "omp").
	pub static AI_HINDSIGHT_RETAIN_CONTEXT = ai_hindsight_retain_context: Str {
		default: Str::new_static("omp"),
		flags: archive,
	};
	/// pi `hindsight.recallBudget` (enum, default: "mid").
	pub static AI_HINDSIGHT_RECALL_BUDGET = ai_hindsight_recall_budget: Str {
		default: Str::new_static("mid"),
		flags: archive,
	};
	/// pi `hindsight.recallMaxTokens` (number, default: 1024).
	pub static AI_HINDSIGHT_RECALL_MAX_TOKENS = ai_hindsight_recall_max_tokens: i64 {
		default: 1024,
		flags: archive,
	};
	/// pi `hindsight.recallContextTurns` (number, default: 1).
	pub static AI_HINDSIGHT_RECALL_CONTEXT_TURNS = ai_hindsight_recall_context_turns: i64 {
		default: 1,
		flags: archive,
	};
	/// pi `hindsight.recallMaxQueryChars` (number, default: 800).
	pub static AI_HINDSIGHT_RECALL_MAX_QUERY_CHARS = ai_hindsight_recall_max_query_chars: i64 {
		default: 800,
		flags: archive,
	};
	/// pi `hindsight.recallTypes` (array, default: HINDSIGHT_RECALL_TYPES_DEFAULT).
	pub static AI_HINDSIGHT_RECALL_TYPES = ai_hindsight_recall_types: Vec<Str> {
		default: vec![Str::new_static("world"), Str::new_static("experience")],
		flags: archive,
	};
	/// pi `hindsight.debug` (boolean, default: false).
	pub static AI_HINDSIGHT_DEBUG = ai_hindsight_debug: bool {
		default: false,
		flags: archive,
	};
	/// pi `hindsight.requestTimeoutMs` (number, default: 30_000).
	pub static AI_HINDSIGHT_REQUEST_TIMEOUT_MS = ai_hindsight_request_timeout_ms: i64 {
		default: 30000,
		flags: archive,
	};
	/// pi `hindsight.reflectTimeoutMs` (number, default: 120_000).
	pub static AI_HINDSIGHT_REFLECT_TIMEOUT_MS = ai_hindsight_reflect_timeout_ms: i64 {
		default: 120000,
		flags: archive,
	};
	/// pi `hindsight.recallTimeoutMs` (number, default: 30_000).
	pub static AI_HINDSIGHT_RECALL_TIMEOUT_MS = ai_hindsight_recall_timeout_ms: i64 {
		default: 30000,
		flags: archive,
	};
	/// pi `hindsight.retainTimeoutMs` (number, default: 60_000).
	pub static AI_HINDSIGHT_RETAIN_TIMEOUT_MS = ai_hindsight_retain_timeout_ms: i64 {
		default: 60000,
		flags: archive,
	};
	/// pi `hindsight.mentalModelsEnabled` (boolean, default: true).
	pub static AI_HINDSIGHT_MENTAL_MODELS_ENABLED = ai_hindsight_mental_models_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `hindsight.mentalModelAutoSeed` (boolean, default: true).
	pub static AI_HINDSIGHT_MENTAL_MODEL_AUTO_SEED = ai_hindsight_mental_model_auto_seed: bool {
		default: true,
		flags: archive,
	};
	/// pi `hindsight.mentalModelRefreshIntervalMs` (number, default: 5 * 60 * 1000).
	pub static AI_HINDSIGHT_MENTAL_MODEL_REFRESH_INTERVAL_MS = ai_hindsight_mental_model_refresh_interval_ms: i64 {
		default: 300000,
		flags: archive,
	};
	/// pi `hindsight.mentalModelMaxRenderChars` (number, default: 16_000).
	pub static AI_HINDSIGHT_MENTAL_MODEL_MAX_RENDER_CHARS = ai_hindsight_mental_model_max_render_chars: i64 {
		default: 16000,
		flags: archive,
	};
	/// pi `ttsr.enabled` (boolean, default: true).
	pub static AI_TTSR_ENABLED = ai_ttsr_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `ttsr.contextMode` (enum, default: "discard").
	pub static AI_TTSR_CONTEXT_MODE = ai_ttsr_context_mode: Str {
		default: Str::new_static("discard"),
		flags: archive,
	};
	/// pi `ttsr.interruptMode` (enum, default: "always").
	pub static AI_TTSR_INTERRUPT_MODE = ai_ttsr_interrupt_mode: Str {
		default: Str::new_static("always"),
		flags: archive,
	};
	/// pi `ttsr.repeatMode` (enum, default: "once").
	pub static AI_TTSR_REPEAT_MODE = ai_ttsr_repeat_mode: Str {
		default: Str::new_static("once"),
		flags: archive,
	};
	/// pi `ttsr.repeatGap` (number, default: 10).
	pub static AI_TTSR_REPEAT_GAP = ai_ttsr_repeat_gap: i64 {
		default: 10,
		flags: archive,
	};
	/// pi `ttsr.builtinRules` (boolean, default: true).
	pub static AI_TTSR_BUILTIN_RULES = ai_ttsr_builtin_rules: bool {
		default: true,
		flags: archive,
	};
	/// pi `ttsr.disabledRules` (array, default: [] as string[]).
	pub static AI_TTSR_DISABLED_RULES = ai_ttsr_disabled_rules: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `web_search.enabled` (boolean, default: true).
	pub static AI_WEB_SEARCH_ENABLED = ai_web_search_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `security.enabled` (boolean, default: false).
	pub static AI_SECURITY_ENABLED = ai_security_enabled: bool {
		default: false,
		flags: archive,
	};
	/// pi `irc.timeoutMs` (number, default: 120_000).
	pub static AI_IRC_TIMEOUT_MS = ai_irc_timeout_ms: i64 {
		default: 120000,
		flags: archive,
	};
	/// pi `plan.enabled` (boolean, default: true).
	pub static AI_PLAN_ENABLED = ai_plan_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `plan.defaultOnStartup` (boolean, default: false).
	pub static AI_PLAN_DEFAULT_ON_STARTUP = ai_plan_default_on_startup: bool {
		default: false,
		flags: archive,
	};
	/// pi `providers.ollama-cloud.maxConcurrency` (number, default: 3).
	pub static AI_PROVIDERS_OLLAMA_CLOUD_MAX_CONCURRENCY = ai_providers_ollama_cloud_max_concurrency: i64 {
		default: 3,
		flags: archive,
	};
	/// pi `providers.imageOrder` (array, default: [] as ImageProvider[]).
	pub static AI_PROVIDERS_IMAGE_ORDER = ai_providers_image_order: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// pi `providers.tinyModelDevice` (enum, default: TINY_MODEL_DEVICE_DEFAULT).
	pub static AI_PROVIDERS_TINY_MODEL_DEVICE = ai_providers_tiny_model_device: Str {
		default: Str::new_static(""),
		flags: archive,
	};
	/// pi `providers.tinyModelDtype` (enum, default: TINY_MODEL_DTYPE_DEFAULT).
	pub static AI_PROVIDERS_TINY_MODEL_DTYPE = ai_providers_tiny_model_dtype: Str {
		default: Str::new_static(""),
		flags: archive,
	};
}

omp_con::var! {
	/// pi `providers.autoThinkingMaxEffort` (enum, default: "xhigh").
	pub static AI_PROVIDERS_AUTO_THINKING_MAX_EFFORT = ai_providers_auto_thinking_max_effort: Str {
		default: Str::new_static("xhigh"),
		flags: archive,
	};
	/// pi `features.unexpectedStopDetection` (enum, default: "mechanical").
	pub static AI_FEATURES_UNEXPECTED_STOP_DETECTION = ai_features_unexpected_stop_detection: Str {
		default: Str::new_static("mechanical"),
		flags: archive,
	};
	/// pi `providers.streamFirstEventTimeoutSeconds` (number, default: -1).
	pub static AI_PROVIDERS_STREAM_FIRST_EVENT_TIMEOUT_SECONDS = ai_providers_stream_first_event_timeout_seconds: i64 {
		default: -1,
		flags: archive,
	};
	/// pi `providers.streamIdleTimeoutSeconds` (number, default: -1).
	pub static AI_PROVIDERS_STREAM_IDLE_TIMEOUT_SECONDS = ai_providers_stream_idle_timeout_seconds: i64 {
		default: -1,
		flags: archive,
	};
	/// pi `providers.fetch` (enum, default: "auto").
	pub static AI_PROVIDERS_FETCH = ai_providers_fetch: Str {
		default: Str::new_static("auto"),
		flags: archive,
	};
	/// pi `provider.appendOnlyContext` (enum, default: "auto").
	pub static AI_PROVIDER_APPEND_ONLY_CONTEXT = ai_provider_append_only_context: Str {
		default: Str::new_static("auto"),
		flags: archive,
	};
	/// pi `commit.mapReduceEnabled` (boolean, default: true).
	pub static AI_COMMIT_MAP_REDUCE_ENABLED = ai_commit_map_reduce_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `commit.mapReduceThreshold` (number, default: 5000).
	pub static AI_COMMIT_MAP_REDUCE_THRESHOLD = ai_commit_map_reduce_threshold: i64 {
		default: 5000,
		flags: archive,
	};
	/// pi `commit.mapBatchTokenBudget` (number, default: 16000).
	pub static AI_COMMIT_MAP_BATCH_TOKEN_BUDGET = ai_commit_map_batch_token_budget: i64 {
		default: 16000,
		flags: archive,
	};
	/// pi `commit.cacheEnabled` (boolean, default: true).
	pub static AI_COMMIT_CACHE_ENABLED = ai_commit_cache_enabled: bool {
		default: true,
		flags: archive,
	};
	/// pi `commit.cacheTtlDays` (number, default: 14).
	pub static AI_COMMIT_CACHE_TTL_DAYS = ai_commit_cache_ttl_days: i64 {
		default: 14,
		flags: archive,
	};
	/// pi `commit.changelogMaxDiffChars` (number, default: 120000).
	pub static AI_COMMIT_CHANGELOG_MAX_DIFF_CHARS = ai_commit_changelog_max_diff_chars: i64 {
		default: 120000,
		flags: archive,
	};
	/// pi `extensionHandlers.toolCallTimeoutMs` (number, default: 30_000).
	pub static AI_EXTENSION_HANDLERS_TOOL_CALL_TIMEOUT_MS = ai_extension_handlers_tool_call_timeout_ms: i64 {
		default: 30000,
		flags: archive,
	};
	/// pi `gc.blobs` (boolean, default: true).
	pub static AI_GC_BLOBS = ai_gc_blobs: bool {
		default: true,
		flags: archive,
	};
	/// pi `gc.archive` (boolean, default: true).
	pub static AI_GC_ARCHIVE = ai_gc_archive: bool {
		default: true,
		flags: archive,
	};
	/// pi `gc.wal` (boolean, default: true).
	pub static AI_GC_WAL = ai_gc_wal: bool {
		default: true,
		flags: archive,
	};
	/// pi `gc.coldArchiveAfterDays` (number, default: 30).
	pub static AI_GC_COLD_ARCHIVE_AFTER_DAYS = ai_gc_cold_archive_after_days: i64 {
		default: 30,
		flags: archive,
	};
	/// pi `gc.retainNewestGlobal` (number, default: 20).
	pub static AI_GC_RETAIN_NEWEST_GLOBAL = ai_gc_retain_newest_global: i64 {
		default: 20,
		flags: archive,
	};
	/// pi `gc.retainNewestPerCwd` (number, default: 10).
	pub static AI_GC_RETAIN_NEWEST_PER_CWD = ai_gc_retain_newest_per_cwd: i64 {
		default: 10,
		flags: archive,
	};
}

/// Exact pi setting keys and their command-stream convar names.
pub const LEGACY_CONVAR_MAPPINGS: &[(&str, &str)] = &[
	("advisor.enabled", "ai_advisor_enabled"),
	("prewalk.enabled", "ai_prewalk_enabled"),
	("advisor.syncBacklog", "ai_advisor_sync_backlog"),
	("advisor.immuneTurns", "ai_advisor_immune_turns"),
	("git.enabled", "ai_git_enabled"),
	("omitThinking", "ai_omit_thinking"),
	("externalThinking", "ai_external_thinking"),
	("model.loopGuard.enabled", "ai_model_loop_guard_enabled"),
	("model.loopGuard.checkAssistantContent", "ai_model_loop_guard_check_assistant_content"),
	("model.loopGuard.toolCallReminder", "ai_model_loop_guard_tool_call_reminder"),
	("model.toolCallLoopGuard.enabled", "ai_model_tool_call_loop_guard_enabled"),
	("model.toolCallLoopGuard.threshold", "ai_model_tool_call_loop_guard_threshold"),
	("model.toolCallLoopGuard.exemptTools", "ai_model_tool_call_loop_guard_exempt_tools"),
	("contextPromotion.enabled", "ai_context_promotion_enabled"),
	("compaction.enabled", "ai_compaction_enabled"),
	("compaction.midTurnEnabled", "ai_compaction_mid_turn_enabled"),
	("compaction.methodOrder", "ai_compaction_method_order"),
	("compaction.thresholdTokens", "ai_compaction_threshold_tokens"),
	("compaction.handoffSaveToDisk", "ai_compaction_handoff_save_to_disk"),
	("compaction.remoteStreamingV2Enabled", "ai_compaction_remote_streaming_v2_enabled"),
	("compaction.asyncEnabled", "ai_compaction_async_enabled"),
	("compaction.reserveTokens", "ai_compaction_reserve_tokens"),
	("compaction.keepRecentTokens", "ai_compaction_keep_recent_tokens"),
	("compaction.autoContinue", "ai_compaction_auto_continue"),
	("compaction.remoteEndpoint", "ai_compaction_remote_endpoint"),
	("compaction.v2RetainedMessageBudget", "ai_compaction_v2_retained_message_budget"),
	("compaction.idleEnabled", "ai_compaction_idle_enabled"),
	("compaction.idleThresholdTokens", "ai_compaction_idle_threshold_tokens"),
	("compaction.idleTimeoutSeconds", "ai_compaction_idle_timeout_seconds"),
	("compaction.supersedeReads", "ai_compaction_supersede_reads"),
	("compaction.dropUseless", "ai_compaction_drop_useless"),
	("snapcompact.systemPrompt", "ai_snapcompact_system_prompt"),
	("snapcompact.toolResults", "ai_snapcompact_tool_results"),
	("snapcompact.shape", "ai_snapcompact_shape"),
	("branchSummary.enabled", "ai_branch_summary_enabled"),
	("branchSummary.reserveTokens", "ai_branch_summary_reserve_tokens"),
	("memories.enabled", "ai_memories_enabled"),
	("memories.maxRolloutsPerStartup", "ai_memories_max_rollouts_per_startup"),
	("memories.maxRolloutAgeDays", "ai_memories_max_rollout_age_days"),
	("memories.minRolloutIdleHours", "ai_memories_min_rollout_idle_hours"),
	("memories.threadScanLimit", "ai_memories_thread_scan_limit"),
	("memories.maxRawMemoriesForGlobal", "ai_memories_max_raw_memories_for_global"),
	("memories.stage1Concurrency", "ai_memories_stage1_concurrency"),
	("memories.stage1LeaseSeconds", "ai_memories_stage1_lease_seconds"),
	("memories.stage1RetryDelaySeconds", "ai_memories_stage1_retry_delay_seconds"),
	("memories.phase2LeaseSeconds", "ai_memories_phase2_lease_seconds"),
	("memories.phase2RetryDelaySeconds", "ai_memories_phase2_retry_delay_seconds"),
	("memories.phase2HeartbeatSeconds", "ai_memories_phase2_heartbeat_seconds"),
	("memories.rolloutPayloadPercent", "ai_memories_rollout_payload_percent"),
	("memories.phase1InputTokenLimit", "ai_memories_phase1_input_token_limit"),
	("memories.fallbackTokenLimit", "ai_memories_fallback_token_limit"),
	("memories.summaryInjectionTokenLimit", "ai_memories_summary_injection_token_limit"),
	("sharpshooter.model", "ai_sharpshooter_model"),
	("sharpshooter.intervalMinutes", "ai_sharpshooter_interval_minutes"),
	("sharpshooter.injectionTokenLimit", "ai_sharpshooter_injection_token_limit"),
	("mnemopi.dbPath", "ai_mnemopi_db_path"),
	("mnemopi.bank", "ai_mnemopi_bank"),
	("mnemopi.embeddingVariant", "ai_mnemopi_embedding_variant"),
	("mnemopi.autoRecall", "ai_mnemopi_auto_recall"),
	("mnemopi.autoRetain", "ai_mnemopi_auto_retain"),
	("mnemopi.polyphonicRecall", "ai_mnemopi_polyphonic_recall"),
	("mnemopi.enhancedRecall", "ai_mnemopi_enhanced_recall"),
	("mnemopi.proactiveLinking", "ai_mnemopi_proactive_linking"),
	("mnemopi.noEmbeddings", "ai_mnemopi_no_embeddings"),
	("mnemopi.embeddingModel", "ai_mnemopi_embedding_model"),
	("mnemopi.embeddingApiUrl", "ai_mnemopi_embedding_api_url"),
	("mnemopi.embeddingApiKey", "ai_mnemopi_embedding_api_key"),
	("mnemopi.llmMode", "ai_mnemopi_llm_mode"),
	("mnemopi.llmBaseUrl", "ai_mnemopi_llm_base_url"),
	("mnemopi.llmApiKey", "ai_mnemopi_llm_api_key"),
	("mnemopi.llmModel", "ai_mnemopi_llm_model"),
	("mnemopi.retainEveryNTurns", "ai_mnemopi_retain_every_nturns"),
	("mnemopi.recallLimit", "ai_mnemopi_recall_limit"),
	("mnemopi.recallContextTurns", "ai_mnemopi_recall_context_turns"),
	("mnemopi.recallMaxQueryChars", "ai_mnemopi_recall_max_query_chars"),
	("mnemopi.injectionTokenLimit", "ai_mnemopi_injection_token_limit"),
	("mnemopi.debug", "ai_mnemopi_debug"),
	("hindsight.apiUrl", "ai_hindsight_api_url"),
	("hindsight.apiToken", "ai_hindsight_api_token"),
	("hindsight.bankId", "ai_hindsight_bank_id"),
	("hindsight.bankIdPrefix", "ai_hindsight_bank_id_prefix"),
	("hindsight.scoping", "ai_hindsight_scoping"),
	("hindsight.bankMission", "ai_hindsight_bank_mission"),
	("hindsight.retainMission", "ai_hindsight_retain_mission"),
	("hindsight.autoRecall", "ai_hindsight_auto_recall"),
	("hindsight.autoRetain", "ai_hindsight_auto_retain"),
	("hindsight.retainMode", "ai_hindsight_retain_mode"),
	("hindsight.retainEveryNTurns", "ai_hindsight_retain_every_nturns"),
	("hindsight.retainOverlapTurns", "ai_hindsight_retain_overlap_turns"),
	("hindsight.retainContext", "ai_hindsight_retain_context"),
	("hindsight.recallBudget", "ai_hindsight_recall_budget"),
	("hindsight.recallMaxTokens", "ai_hindsight_recall_max_tokens"),
	("hindsight.recallContextTurns", "ai_hindsight_recall_context_turns"),
	("hindsight.recallMaxQueryChars", "ai_hindsight_recall_max_query_chars"),
	("hindsight.recallTypes", "ai_hindsight_recall_types"),
	("hindsight.debug", "ai_hindsight_debug"),
	("hindsight.requestTimeoutMs", "ai_hindsight_request_timeout_ms"),
	("hindsight.reflectTimeoutMs", "ai_hindsight_reflect_timeout_ms"),
	("hindsight.recallTimeoutMs", "ai_hindsight_recall_timeout_ms"),
	("hindsight.retainTimeoutMs", "ai_hindsight_retain_timeout_ms"),
	("hindsight.mentalModelsEnabled", "ai_hindsight_mental_models_enabled"),
	("hindsight.mentalModelAutoSeed", "ai_hindsight_mental_model_auto_seed"),
	("hindsight.mentalModelRefreshIntervalMs", "ai_hindsight_mental_model_refresh_interval_ms"),
	("hindsight.mentalModelMaxRenderChars", "ai_hindsight_mental_model_max_render_chars"),
	("ttsr.enabled", "ai_ttsr_enabled"),
	("ttsr.contextMode", "ai_ttsr_context_mode"),
	("ttsr.interruptMode", "ai_ttsr_interrupt_mode"),
	("ttsr.repeatMode", "ai_ttsr_repeat_mode"),
	("ttsr.repeatGap", "ai_ttsr_repeat_gap"),
	("ttsr.builtinRules", "ai_ttsr_builtin_rules"),
	("ttsr.disabledRules", "ai_ttsr_disabled_rules"),
	("web_search.enabled", "ai_web_search_enabled"),
	("security.enabled", "ai_security_enabled"),
	("irc.timeoutMs", "ai_irc_timeout_ms"),
	("plan.enabled", "ai_plan_enabled"),
	("plan.defaultOnStartup", "ai_plan_default_on_startup"),
	("providers.ollama-cloud.maxConcurrency", "ai_providers_ollama_cloud_max_concurrency"),
	("providers.imageOrder", "ai_providers_image_order"),
	("providers.tinyModelDevice", "ai_providers_tiny_model_device"),
	("providers.tinyModelDtype", "ai_providers_tiny_model_dtype"),
	("providers.autoThinkingMaxEffort", "ai_providers_auto_thinking_max_effort"),
	("features.unexpectedStopDetection", "ai_features_unexpected_stop_detection"),
	("providers.streamFirstEventTimeoutSeconds", "ai_providers_stream_first_event_timeout_seconds"),
	("providers.streamIdleTimeoutSeconds", "ai_providers_stream_idle_timeout_seconds"),
	("providers.fetch", "ai_providers_fetch"),
	("provider.appendOnlyContext", "ai_provider_append_only_context"),
	("commit.mapReduceEnabled", "ai_commit_map_reduce_enabled"),
	("commit.mapReduceThreshold", "ai_commit_map_reduce_threshold"),
	("commit.mapBatchTokenBudget", "ai_commit_map_batch_token_budget"),
	("commit.cacheEnabled", "ai_commit_cache_enabled"),
	("commit.cacheTtlDays", "ai_commit_cache_ttl_days"),
	("commit.changelogMaxDiffChars", "ai_commit_changelog_max_diff_chars"),
	("extensionHandlers.toolCallTimeoutMs", "ai_extension_handlers_tool_call_timeout_ms"),
	("gc.blobs", "ai_gc_blobs"),
	("gc.archive", "ai_gc_archive"),
	("gc.wal", "ai_gc_wal"),
	("gc.coldArchiveAfterDays", "ai_gc_cold_archive_after_days"),
	("gc.retainNewestGlobal", "ai_gc_retain_newest_global"),
	("gc.retainNewestPerCwd", "ai_gc_retain_newest_per_cwd"),
];
