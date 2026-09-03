//! Literal pi-setting convars not otherwise owned by a narrower runtime module.

use omp_core::Str;

omp_con::var! {
	/// pi `worktree.clone` (boolean, default: true).
	pub static SV_WORKTREE_CLONE = sv_worktree_clone: bool {
		default: true,
		flags: archive,
	};
	/// pi `task.isolation.commits` (enum, default: "generic").
	pub static SV_TASK_ISOLATION_COMMITS = sv_task_isolation_commits: Str {
		default: Str::new_static("generic"),
		flags: archive,
	};
	/// pi `task.batch` (boolean, default: true).
	pub static SV_TASK_BATCH = sv_task_batch: bool {
		default: true,
		flags: archive,
	};
	/// pi `task.enableEffort` (boolean, default: false).
	pub static SV_TASK_ENABLE_EFFORT = sv_task_enable_effort: bool {
		default: false,
		flags: archive,
	};
	/// pi `task.prewalk` (boolean, default: false).
	pub static SV_TASK_PREWALK = sv_task_prewalk: bool {
		default: false,
		flags: archive,
	};
	/// pi `tasks.todoClearDelay` (number, default: 60).
	pub static SV_TASKS_TODO_CLEAR_DELAY = sv_tasks_todo_clear_delay: i64 {
		default: 60,
		flags: archive,
	};
	/// pi `skills.enableSkillCommands` (boolean, default: true).
	pub static SV_SKILLS_ENABLE_SKILL_COMMANDS = sv_skills_enable_skill_commands: bool {
		default: true,
		flags: archive,
	};
	/// pi `skills.enableCodexUser` (boolean, default: false).
	pub static SV_SKILLS_ENABLE_CODEX_USER = sv_skills_enable_codex_user: bool {
		default: false,
		flags: archive,
	};
	/// pi `skills.enableClaudeUser` (boolean, default: false).
	pub static SV_SKILLS_ENABLE_CLAUDE_USER = sv_skills_enable_claude_user: bool {
		default: false,
		flags: archive,
	};
	/// pi `skills.enableClaudeProject` (boolean, default: true).
	pub static SV_SKILLS_ENABLE_CLAUDE_PROJECT = sv_skills_enable_claude_project: bool {
		default: true,
		flags: archive,
	};
	/// pi `skills.enablePiUser` (boolean, default: true).
	pub static SV_SKILLS_ENABLE_PI_USER = sv_skills_enable_pi_user: bool {
		default: true,
		flags: archive,
	};
	/// pi `skills.enablePiProject` (boolean, default: true).
	pub static SV_SKILLS_ENABLE_PI_PROJECT = sv_skills_enable_pi_project: bool {
		default: true,
		flags: archive,
	};
	/// pi `skills.enableAgentsUser` (boolean, default: true).
	pub static SV_SKILLS_ENABLE_AGENTS_USER = sv_skills_enable_agents_user: bool {
		default: true,
		flags: archive,
	};
	/// pi `skills.enableAgentsProject` (boolean, default: true).
	pub static SV_SKILLS_ENABLE_AGENTS_PROJECT = sv_skills_enable_agents_project: bool {
		default: true,
		flags: archive,
	};
	/// pi `secrets.enabled` (boolean, default: false).
	pub static SV_SECRETS_ENABLED = sv_secrets_enabled: bool {
		default: false,
		flags: archive,
	};
}

/// Exact pi setting keys and their command-stream convar names.
pub const LEGACY_CONVAR_MAPPINGS: &[(&str, &str)] = &[
	("worktree.clone", "sv_worktree_clone"),
	("task.isolation.commits", "sv_task_isolation_commits"),
	("task.batch", "sv_task_batch"),
	("task.enableEffort", "sv_task_enable_effort"),
	("task.prewalk", "sv_task_prewalk"),
	("tasks.todoClearDelay", "sv_tasks_todo_clear_delay"),
	("skills.enableSkillCommands", "sv_skills_enable_skill_commands"),
	("skills.enableCodexUser", "sv_skills_enable_codex_user"),
	("skills.enableClaudeUser", "sv_skills_enable_claude_user"),
	("skills.enableClaudeProject", "sv_skills_enable_claude_project"),
	("skills.enablePiUser", "sv_skills_enable_pi_user"),
	("skills.enablePiProject", "sv_skills_enable_pi_project"),
	("skills.enableAgentsUser", "sv_skills_enable_agents_user"),
	("skills.enableAgentsProject", "sv_skills_enable_agents_project"),
	("secrets.enabled", "sv_secrets_enabled"),
];
