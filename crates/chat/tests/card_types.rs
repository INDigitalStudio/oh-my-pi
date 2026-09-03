//! Compile-time and rendering checks tying native cards to tool contracts.

use omp_chat::cards::{CardRegistry, CardStatus, CardView};
use omp_core::Str;
use omp_dom::{KnownTag, Node, PropId, Tag, Value as DomValue};
use omp_tui::{Ui, UiContext, frame_text};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn node(tag: KnownTag, content: String) -> Node {
	Node {
		tag:     Tag::Known(tag),
		props:   Default::default(),
		kids:    Vec::new(),
		content: Some(Str::new(content)),
	}
}

fn update_is_typed<U>()
where
	U: DeserializeOwned + Serialize,
{
}

fn renders_typed<P, T, F>(tool: &str, fixture: Value) -> String
where
	P: DeserializeOwned,
	T: DeserializeOwned + Serialize,
	F: DeserializeOwned,
{
	let payload: T = serde_json::from_value(fixture)
		.unwrap_or_else(|error| panic!("{tool} typed payload fixture must deserialize: {error}"));
	let encoded = serde_json::to_value(&payload).expect("typed payload fixture must serialize");
	let mut result =
		node(KnownTag::Result, r#"{"results":[{"error":"projection decoy"}]}"#.to_owned());
	result.props.push((
		PropId::Outcome.into(),
		DomValue::Json(
			serde_json::value::to_raw_value(&json!({"kind": "ok", "value": encoded}))
				.expect("outcome JSON is valid"),
		),
	));
	result.props.push((
		PropId::Data.into(),
		DomValue::Json(
			serde_json::value::to_raw_value(&json!([
				{"kind": "text", "text": "{\"results\":[{\"error\":\"projection decoy\"}]}"}
			]))
			.expect("projection JSON is valid"),
		),
	));
	let input = node(KnownTag::Input, "{}".to_owned());
	let view = CardView {
		input:   &input,
		result:  Some(&result),
		diag:    None,
		usage:   None,
		status:  CardStatus::Done,
		output:  None,
		started: None,
	};
	let registry = CardRegistry::standard();
	assert!(registry.contains(tool), "{tool} must not fall back to GenericCard");
	let _params: Option<P> = view.input();
	assert!(view.result::<T>().is_some());
	let _fault: Option<F> = view.fault();
	let ui = UiContext::default();
	let component = registry.render(tool, &view, false, &ui);
	let rendered = Ui::from_root(component, 100, ui);
	let text = frame_text(rendered.frame());
	assert!(!text.trim().is_empty(), "{tool} card rendered no content");
	text
}

#[test]
fn every_native_registered_card_accepts_its_tool_contract() {
	use omp_tools as tools;

	update_is_typed::<tools::ask::Update>();
	update_is_typed::<tools::ast_edit::Update>();
	update_is_typed::<tools::ast_grep::Update>();
	update_is_typed::<tools::shell::Update>();
	update_is_typed::<tools::browser::Update>();
	update_is_typed::<tools::computer::Update>();
	update_is_typed::<tools::debug::Update>();
	update_is_typed::<tools::edit::EditUpdate>();
	update_is_typed::<tools::eval::Update>();
	update_is_typed::<tools::github::Update>();
	update_is_typed::<tools::glob::Update>();
	update_is_typed::<tools::goal::Update>();
	update_is_typed::<tools::grep::Update>();
	update_is_typed::<tools::hub::Response>();
	update_is_typed::<tools::lsp::Update>();
	update_is_typed::<tools::memory::Update>();
	update_is_typed::<tools::read::Update>();
	update_is_typed::<tools::task::Update>();
	update_is_typed::<tools::think::Update>();
	update_is_typed::<tools::todo::Update>();
	update_is_typed::<tools::web_search::Update>();
	update_is_typed::<tools::write::Update>();

	renders_typed::<tools::ask::Params, tools::ask::Payload, tools::ask::Fault>(
		"ask",
		json!({"answers": [], "headless": false}),
	);
	renders_typed::<
		tools::edit::apply_patch::FreeformEditParams,
		tools::edit::Payload,
		tools::edit::Fault,
	>("apply_patch", json!({"sections": []}));
	renders_typed::<tools::ast_edit::Params, tools::ast_edit::Payload, tools::ast_edit::Fault>(
		"ast_edit",
		json!({"files": [], "advisories": [], "recovery_root": null, "pending_proposal": null}),
	);
	renders_typed::<tools::ast_grep::Params, tools::ast_grep::Payload, tools::ast_grep::Fault>(
		"ast_grep",
		json!({"matches": [], "advisories": [], "total": 0, "next_cursor": null}),
	);
	renders_typed::<tools::shell::Params, tools::shell::Payload, tools::shell::Fault>(
		"bash",
		json!({
			"session_id": [], "exec_id": [], "command": "", "transcript": [], "adjustments": [],
			"status": {"outcome": "exited", "exit_code": 0, "signal": null, "wall_clock_ms": 0,
				"spilled_output": null, "aborted": false, "effects_unknown": false,
				"final_cwd_uri": null, "final_cwd_revision": 0}
		}),
	);
	renders_typed::<tools::browser::Params, tools::browser::Payload, tools::browser::Fault>(
		"browser",
		json!({"action": "open", "name": "main", "url": null, "title": null, "result": null, "artifacts": []}),
	);
	renders_typed::<tools::computer::Params, tools::computer::Payload, tools::computer::Fault>(
		"computer",
		json!({"code": "return await desktop.capabilities()", "results": [{}], "artifacts": []}),
	);
	renders_typed::<tools::debug::Params, tools::debug::Payload, tools::debug::Fault>(
		"debug",
		json!({"action": "launch", "session": null, "revision": null, "output": "", "data": {}}),
	);
	renders_typed::<tools::edit::Params, tools::edit::Payload, tools::edit::Fault>(
		"edit",
		json!({"sections": []}),
	);
	renders_typed::<tools::eval::Params, tools::eval::Payload, tools::eval::Fault>(
		"eval",
		json!({
			"session_id": [], "cell_id": [], "language": "py", "title": null, "code": "", "reset": false,
			"had_output": false, "result": null, "display_outputs": [],
			"status": {"outcome": "complete", "exit_code": 0, "duration_ms": 0, "exception": null}
		}),
	);
	renders_typed::<tools::github::Params, tools::github::Payload, tools::github::Fault>(
		"github",
		json!({"op": "repo_view", "result": {}, "rate_limit_remaining": null, "rate_limit_reset": null}),
	);
	renders_typed::<tools::glob::Params, tools::glob::Payload, tools::glob::Fault>(
		"glob",
		json!({"matches": [], "missing_paths": [], "timed_out": false, "truncated": false,
			"result_limit_reached": null, "partial_match_count": 0, "timeout_ms": 0, "projected_text": "",
			"output_blob": null, "output_artifact_uri": null, "output_shown_lines": 0, "output_total_lines": 0}),
	);
	renders_typed::<tools::goal::Params, tools::goal::Payload, tools::goal::Fault>(
		"goal",
		json!({"op": "get", "goal": null, "remaining_tokens": null, "completion_report": null}),
	);
	renders_typed::<tools::grep::Params, tools::grep::Payload, tools::grep::Fault>(
		"grep",
		json!({"files": [], "total_files": 0, "total_files_lower_bound": false, "multi_scope": false,
			"skip": 0, "file_limit_reached": false, "per_file_limit_reached": false, "notes": [],
			"projected_text": "", "output_blob": null, "output_artifact_uri": null,
			"output_shown_lines": 0, "output_total_lines": 0}),
	);
	renders_typed::<tools::hub::Params, tools::hub::Response, tools::hub::Fault>(
		"hub",
		json!({"text": "ok", "useless": false}),
	);
	renders_typed::<tools::lsp::Params, tools::lsp::Payload, tools::lsp::Fault>(
		"lsp",
		json!({"action": "diagnostics", "servers": [], "output": "", "data": {}}),
	);
	renders_typed::<tools::memory::RecallParams, tools::memory::RecallPayload, tools::memory::Fault>(
		"recall",
		json!({"query": "", "items": []}),
	);
	renders_typed::<tools::memory::ReflectParams, tools::memory::ReflectPayload, tools::memory::Fault>(
		"reflect",
		json!({"answer": "", "recalled": 0}),
	);
	renders_typed::<tools::memory::RetainParams, tools::memory::RetainPayload, tools::memory::Fault>(
		"retain",
		json!({"ids": []}),
	);
	renders_typed::<tools::read::Params, tools::read::Payload, tools::read::Fault>(
		"read",
		json!({"parts": [], "artifacts": []}),
	);
	renders_typed::<tools::task::Params, tools::task::Payload, tools::task::Fault>(
		"task",
		json!({"children": []}),
	);
	renders_typed::<tools::think::Params, tools::think::Payload, tools::think::Fault>(
		"think",
		json!({"recorded": true}),
	);
	renders_typed::<tools::todo::Params, tools::todo::Payload, tools::todo::Fault>(
		"todo",
		json!({"op":"view","phases": [], "completed_tasks": []}),
	);
	renders_typed::<tools::web_search::Params, tools::web_search::Payload, tools::web_search::Fault>(
		"web_search",
		json!({"response": {}}),
	);
	renders_typed::<tools::write::Params, tools::write::Payload, tools::write::Fault>(
		"write",
		json!({"resolved_path": "x", "display_path": "x", "canonical_recovery": null,
			"byte_len": 0, "reported_len": 0, "disposition": "created", "stripped_wrapper": false,
			"made_executable": false, "snapshot_tag": null, "operation": {"kind": "plain"}}),
	);
	renders_typed::<
		tools::checkpoint::CheckpointParams,
		tools::checkpoint::CheckpointPayload,
		tools::checkpoint::Fault,
	>("checkpoint", json!({"token":"checkpoint-1","goal":"explore parser","started_at":1}));
	renders_typed::<
		tools::checkpoint::RewindParams,
		tools::checkpoint::RewindPayload,
		tools::checkpoint::Fault,
	>(
		"rewind",
		json!({"token":"checkpoint-1","report":"parser is sound","receipt":"r1","scheduled":true}),
	);
	renders_typed::<tools::yield_tool::Params, tools::yield_tool::Payload, tools::yield_tool::Fault>(
		"yield",
		json!({"incremental":false,"use_last_turn":false}),
	);
	renders_typed::<tools::learn::Params, tools::learn::LearnOutcome, tools::learn::Fault>(
		"learn",
		json!({"memory_id":"m1","skill":{"status":"not_requested"},"partial":false}),
	);
	renders_typed::<
		tools::manage_skill::Params,
		tools::manage_skill::MutationOutcome,
		tools::manage_skill::Fault,
	>("manage_skill", json!({"action":"create","name":"rust","path":"rust/SKILL.md","revision":1}));
	let registry = CardRegistry::standard();
	for identity in tools::builtin_tool_identities() {
		assert!(registry.contains(identity.name), "{} must have a dedicated card", identity.name);
	}
}

#[test]
fn typed_outcomes_are_not_shadowed_by_projection_json() {
	use omp_tools as tools;

	let task = renders_typed::<tools::task::Params, tools::task::Payload, tools::task::Fault>(
		"task",
		json!({"children":[{
			"id":"Child","agent":"task","text":"completed successfully","session_path":"child.oms",
			"tokens_in":12,"tokens_out":3,"output":null,"workspace":null,"error":null
		}]}),
	);
	assert!(task.contains("Child"), "{task}");
	assert!(!task.contains("operation failed"), "{task}");

	let ask = renders_typed::<tools::ask::Params, tools::ask::Payload, tools::ask::Fault>(
		"ask",
		json!({"answers":[{"id":"db","selected":["Postgres"],"timed_out":false}],"headless":false}),
	);
	assert!(!ask.contains("operation failed"), "{ask}");

	let hub = renders_typed::<tools::hub::Params, tools::hub::Response, tools::hub::Fault>(
		"hub",
		json!({"text":"{\"peers\":[{\"id\":\"Worker\",\"status\":\"idle\",\"unread\":0}]}","useless":false}),
	);
	assert!(hub.contains("Worker"), "{hub}");

	let search = renders_typed::<
		tools::web_search::Params,
		tools::web_search::Payload,
		tools::web_search::Fault,
	>(
		"web_search",
		json!({"response":{
			"engine":"perplexity","answer":"production answer",
			"sources":[{"url":"https://example.com","title":"Primary source","snippet":"proof"}]
		}}),
	);
	assert!(search.contains("production answer"), "{search}");
	assert!(search.contains("Primary source"), "{search}");

	let debug = renders_typed::<tools::debug::Params, tools::debug::Payload, tools::debug::Fault>(
		"debug",
		json!({"action":"stack_trace","session":null,"revision":null,"output":"",
			"data":{"session":{"path":"src/main.rs","line":7,"col":2},"frames":[{"name":"main","path":"src/main.rs","line":7,"col":2}]}}),
	);
	assert!(debug.contains("main"), "{debug}");

	let lsp = renders_typed::<tools::lsp::Params, tools::lsp::Payload, tools::lsp::Fault>(
		"lsp",
		json!({"action":"references","servers":[],"output":"",
			"data":{"references":[{"path":"src/main.rs","locations":[{"line":7,"col":2}]}]}}),
	);
	assert!(lsp.contains("1 found"), "{lsp}");

	let goal = renders_typed::<tools::goal::Params, tools::goal::Payload, tools::goal::Fault>(
		"goal",
		json!({"op":"get","goal":{"id":"g1","objective":"ship","status":"active",
			"token_budget":1000,"tokens_used":100,"time_used_secs":60},
			"remaining_tokens":900,"completion_report":null}),
	);
	assert!(goal.contains("1K left"), "{goal}");

	let github = renders_typed::<tools::github::Params, tools::github::Payload, tools::github::Fault>(
		"github",
		json!({"op":"search_prs","result":{"output":"#42 production result"},
			"rate_limit_remaining":null,"rate_limit_reset":null}),
	);
	assert!(github.contains("#42 production result"), "{github}");
}
