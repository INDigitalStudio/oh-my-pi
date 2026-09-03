//! Command-stream configuration migration and persistence contracts.

use std::fs;

use omp_app::{
	cli::ConfigScope,
	config_cmd::{migrate_settings, set_persisted},
};

#[test]
fn config_migrate_is_idempotent_and_maps_every_schema_key() {
	let registry = omp_con::Ctx::new();
	for &(legacy, name) in [
		omp_catalog::settings::LEGACY_CONVAR_MAPPINGS,
		omp_catalog::pi_settings::LEGACY_CONVAR_MAPPINGS,
		omp_inference::settings::LEGACY_CONVAR_MAPPINGS,
		omp_inference::pi_settings::LEGACY_CONVAR_MAPPINGS,
		omp_tools::settings::LEGACY_CONVAR_MAPPINGS,
		omp_tools::pi_settings::LEGACY_CONVAR_MAPPINGS,
		omp_envd::LEGACY_CONVAR_MAPPINGS,
		omp_envd::pi_settings::LEGACY_CONVAR_MAPPINGS,
		omp_driver::settings::LEGACY_CONVAR_MAPPINGS,
		omp_driver::pi_settings::LEGACY_CONVAR_MAPPINGS,
		omp_app::settings::LEGACY_CONVAR_MAPPINGS,
		omp_chat::settings::LEGACY_CONVAR_MAPPINGS,
		omp_app::voice::settings::LEGACY_CONVAR_MAPPINGS,
	]
	.into_iter()
	.flatten()
	{
		assert!(
			matches!(registry.find(name), Some(omp_con::RegItem::Var(_))),
			"legacy key {legacy} maps to missing convar {name}",
		);
	}
	let data = tempfile::tempdir().expect("data directory");
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: nextest runs each test in its own process; nothing else reads the
	// variable concurrently.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	fs::write(data.path().join("config.toml"), "[stt]\nenabled = true\nmodelName = \"turbo\"\n")
		.expect("legacy TOML");

	let path = migrate_settings(data.path(), project.path()).expect("first migration");
	let first = fs::read(&path).expect("first config.cfg");
	migrate_settings(data.path(), project.path()).expect("second migration");
	let second = fs::read(&path).expect("second config.cfg");

	assert_eq!(second, first);
	let script = String::from_utf8(first).expect("UTF-8 cfg");
	assert!(script.contains("cl_voice_stt_enabled true"));
	assert!(script.contains("cl_stt_model turbo"));
}

#[test]
fn config_migrate_keeps_project_values_out_of_the_user_cfg() {
	let data = tempfile::tempdir().expect("data directory");
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: see above.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	fs::create_dir_all(project.path().join(".omp")).expect(".omp");
	fs::write(data.path().join("config.toml"), "[stt]\nenabled = true\n").expect("user TOML");
	fs::write(project.path().join(".omp/config.toml"), "[stt]\nmodelName = \"turbo\"\n")
		.expect("project TOML");

	let user = migrate_settings(data.path(), project.path()).expect("migration");
	let user_script = fs::read_to_string(&user).expect("user config.cfg");
	assert!(user_script.contains("cl_voice_stt_enabled true"));
	assert!(!user_script.contains("cl_stt_model"), "project value leaked into the user scope");
	let project_script =
		fs::read_to_string(project.path().join(".omp/config.cfg")).expect("project config.cfg");
	assert!(project_script.contains("cl_stt_model turbo"));
	assert!(!project_script.contains("cl_voice_stt_enabled"));
	let ctx = omp_app::process_ctx(project.path()).expect("reload context");
	assert_eq!(
		ctx.get_typed::<omp_app::voice::settings::SttModel>("cl_stt_model").expect("convar"),
		omp_app::voice::settings::SttModel::Turbo
	);
	assert!(ctx.get_typed::<bool>("cl_voice_stt_enabled").expect("convar"));
}

#[test]
fn profile_selects_its_own_config_cfg() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: see above.
	unsafe {
		std::env::set_var("OMP_CONFIG_DIR", config.path());
		std::env::set_var("OMP_PROFILE", "work");
	}
	let project = tempfile::tempdir().expect("project directory");
	set_persisted(project.path(), ConfigScope::Global, "cl_showthinking", "false")
		.expect("set archived convar");
	set_persisted(project.path(), ConfigScope::Global, "sv_worktree_base", "/tmp/omp-worktrees")
		.expect("set worktree root");
	assert!(config.path().join("profiles/work/config.cfg").is_file());
	assert!(!config.path().join("config.cfg").exists());
	let ctx = omp_app::process_ctx(project.path()).expect("reload context");
	assert!(!ctx.get_typed::<bool>("cl_showthinking").expect("convar"));
	assert_eq!(
		omp_driver::settings::current().expect("driver settings").worktree.base.as_deref(),
		Some(std::path::Path::new("/tmp/omp-worktrees"))
	);
}

#[test]
fn exec_and_writecfg_use_the_installed_cfg_files() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: see above.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	fs::create_dir_all(project.path().join(".omp")).expect(".omp");
	fs::write(config.path().join("focus.cfg"), "cl_showthinking false\n").expect("user profile");
	fs::write(project.path().join(".omp/focus.cfg"), "ai_fastmode true\n").expect("project overlay");
	let ctx = omp_app::process_ctx(project.path()).expect("context");
	ctx.run("exec focus").expect("exec resolves through the installed loader");
	assert!(!ctx.get_typed::<bool>("cl_showthinking").expect("convar"));
	assert!(ctx.get_typed::<bool>("ai_fastmode").expect("convar"));
	ctx.run("writecfg").expect("writecfg resolves through the installed saver");
	let script = fs::read_to_string(config.path().join("config.cfg")).expect("config.cfg");
	assert!(script.contains("cl_showthinking false"));
	assert!(script.contains("ai_fastmode true"));
}

#[test]
fn config_set_persists_and_get_reads_back() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: see above.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	set_persisted(project.path(), ConfigScope::Global, "cl_showthinking", "false")
		.expect("set archived convar");

	let script = fs::read_to_string(config.path().join("config.cfg")).expect("config.cfg");
	assert!(script.contains("cl_showthinking false"));
	let ctx = omp_app::process_ctx(project.path()).expect("reload context");
	assert_eq!(ctx.get_typed::<bool>("cl_showthinking").expect("convar"), false);
}
