//! Deterministic, journal-derived tool-card gallery.

use omp_core::Str;
use omp_dom::{Handle, KnownTag, Node, PropId, Snapshot, Tag, Value as DomValue};
use omp_session::{ComponentRegistry, Session};
use omp_tool::Part;
use omp_tui::{Charset, Frame, IntoComponent as _, Ui, UiContext, dom};
use serde_json::{Value, value::RawValue};
use thiserror::Error;

use crate::cards::{CardRegistry, CardStatus, CardView, fixtures::CardFixture};

/// Tool lifecycle states rendered by the gallery, in display order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GalleryState {
	/// Arguments are still streaming.
	StreamingArgs,
	/// The call is executing.
	InProgress,
	/// The call settled successfully.
	Done,
	/// The call faulted or returned an error-shaped outcome.
	Failed,
}

impl GalleryState {
	/// All states in reference-gallery order.
	pub const ALL: [Self; 4] = [Self::StreamingArgs, Self::InProgress, Self::Done, Self::Failed];

	/// Human-readable state label used by the captured references.
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::StreamingArgs => "streaming args",
			Self::InProgress => "in progress",
			Self::Done => "done",
			Self::Failed => "failed",
		}
	}

	const fn index(self) -> usize {
		match self {
			Self::StreamingArgs => 0,
			Self::InProgress => 1,
			Self::Done => 2,
			Self::Failed => 3,
		}
	}
}

/// One rendered fixture state.
pub struct GallerySection {
	/// Gallery fixture identity.
	pub tool:  &'static str,
	/// Human-readable fixture title.
	pub title: &'static str,
	/// Lifecycle state represented by this frame.
	pub state: GalleryState,
	/// Fully laid-out card frame.
	pub frame: Frame,
}

/// Failure to materialize or render a gallery fixture.
#[derive(Debug, Error)]
pub enum GalleryError {
	/// A fixture payload was not valid complete JSON for its lifecycle state.
	#[error("gallery fixture JSON is invalid")]
	Json(#[from] serde_json::Error),
	/// A temporary journal could not be created.
	#[error("gallery temporary journal failed")]
	Temp(#[from] std::io::Error),
	/// The journal-to-DOM fold failed.
	#[error("gallery session fold failed")]
	Session(#[from] omp_session::SessionError),
	/// The folded call element or one of its mandatory children is absent.
	#[error("gallery fixture did not materialize {0}")]
	Missing(&'static str),
}

/// Returns gallery fixture names in stable reference order.
#[must_use]
pub fn fixture_names() -> Vec<&'static str> {
	let mut names = crate::cards::fixtures::all()
		.into_iter()
		.map(|fixture| fixture.tool)
		.collect::<Vec<_>>();
	names.sort_unstable();
	names
}

/// Materializes and renders selected card fixtures through real sessions.
///
/// `tool = None` renders every fixture in stable reference order.
pub fn render_sections(
	tool: Option<&str>,
	states: &[GalleryState],
	width: u16,
	expanded: bool,
) -> Result<Vec<GallerySection>, GalleryError> {
	let mut fixtures = crate::cards::fixtures::all();
	fixtures.sort_unstable_by_key(|fixture| fixture.tool);
	let registry = CardRegistry::standard();
	let mut sections = Vec::with_capacity(fixtures.len().saturating_mul(states.len()));
	for fixture in fixtures {
		if tool.is_some_and(|wanted| wanted != fixture.tool) {
			continue;
		}
		for &state in states {
			sections.push(render_fixture(&registry, fixture, state, width, expanded)?);
		}
	}
	Ok(sections)
}

fn render_fixture(
	registry: &CardRegistry,
	fixture: &'static CardFixture,
	state: GalleryState,
	width: u16,
	expanded: bool,
) -> Result<GallerySection, GalleryError> {
	let directory = tempfile::tempdir()?;
	let journal = directory.path().join("gallery.oms");
	let mut session = Session::create(journal, ComponentRegistry::standard())?;
	session.begin_turn()?;
	let state_fixture = fixture.states[state.index()];
	let call_id = format!("gallery-{}-{}", fixture.tool, state.index());
	let call = if state == GalleryState::StreamingArgs {
		let (call, sid) =
			session.call_streaming(card_tool(fixture.tool), 1, call_id.as_str(), None)?;
		if !state_fixture.args.is_empty() {
			session.stream_append(sid, state_fixture.args)?;
		}
		call
	} else {
		session.call(
			card_tool(fixture.tool),
			1,
			call_id.as_str(),
			None,
			Some(raw(state_fixture.args)?),
			None,
		)?
	};
	if state != GalleryState::StreamingArgs {
		if let Some(update) = state_fixture.update {
			session.call_update(call, raw(update)?)?;
		}
		match state {
			GalleryState::StreamingArgs | GalleryState::InProgress => {},
			GalleryState::Done => {
				let payload = fixture_payload(
					card_tool(fixture.tool),
					state_fixture.args,
					state_fixture.result.unwrap_or("null"),
				)?;
				let parts = projected_parts(card_tool(fixture.tool), &payload)?;
				session.settle_projected(call, outcome_value("ok", payload)?, raw_parts(parts)?)?;
			},
			GalleryState::Failed => {
				let raw_fault = state_fixture
					.fault
					.map(serde_json::from_str)
					.transpose()?
					.unwrap_or_else(|| {
						serde_json::json!({
							"message": state_fixture
								.result
								.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
								.and_then(|value| value.get("error").and_then(Value::as_str).map(str::to_owned))
								.unwrap_or_else(|| "operation failed".to_owned())
						})
					});
				let fault = fixture_fault(card_tool(fixture.tool), raw_fault);
				let parts = projected_parts(card_tool(fixture.tool), &fault)?;
				session.fail_projected(call, outcome_value("faulted", fault)?, raw_parts(parts)?)?;
			},
		}
	}
	let snapshot = session.dom().snapshot();
	let tool = find_snapshot_call(&snapshot, call_id.as_str())
		.ok_or(GalleryError::Missing("tool element"))?;
	let node = snapshot
		.get(tool)
		.ok_or(GalleryError::Missing("tool element"))?;
	let input =
		child(&snapshot, tool, KnownTag::Input).ok_or(GalleryError::Missing("input element"))?;
	let status = node
		.prop(&PropId::Status.into())
		.and_then(DomValue::as_str)
		.map_or(CardStatus::InProgress, CardStatus::from_dom);
	if status == CardStatus::Done {
		let result =
			child(&snapshot, tool, KnownTag::Result).ok_or(GalleryError::Missing("result element"))?;
		if result.prop(&PropId::Outcome.into()).is_none()
			|| result.prop(&PropId::Data.into()).is_none()
		{
			return Err(GalleryError::Missing("projected result truth"));
		}
	} else if status == CardStatus::Failed {
		let diag =
			child(&snapshot, tool, KnownTag::Diag).ok_or(GalleryError::Missing("diag element"))?;
		if diag.prop(&PropId::Fault.into()).is_none() || diag.prop(&PropId::Data.into()).is_none() {
			return Err(GalleryError::Missing("projected fault truth"));
		}
	}
	let result = child(&snapshot, tool, KnownTag::Result);
	let output = result.and_then(node_text);
	let view = CardView {
		input,
		result,
		diag: child(&snapshot, tool, KnownTag::Diag),
		usage: child(&snapshot, tool, KnownTag::Usage),
		status,
		output,
		started: None,
	};
	let mut ui_context = UiContext::default();
	ui_context.charset = Charset::NerdFont;
	let card = registry.render(card_tool(fixture.tool), &view, expanded, &ui_context);
	// Pi captures tool blocks inside the transcript, where every block
	// carries a one-row vertical margin; the gallery paints the same block.
	let component = dom! { <col pad="1 0">{card}</col> }.into_component();
	let ui = Ui::from_root(component, width, ui_context);
	Ok(GallerySection { tool: fixture.tool, title: fixture.title, state, frame: ui.frame().clone() })
}

fn raw(text: &str) -> Result<Box<RawValue>, serde_json::Error> {
	let value: serde_json::Value = serde_json::from_str(text)?;
	serde_json::value::to_raw_value(&value)
}

/// Wraps a fixture payload in the `CallOutcome` envelope the kernel journals
/// (`{"kind":"ok"|"faulted","value":…}`), so cards read the gallery exactly
/// like a live session.
fn outcome_value(kind: &str, value: serde_json::Value) -> Result<Box<RawValue>, serde_json::Error> {
	serde_json::value::to_raw_value(&serde_json::json!({ "kind": kind, "value": value }))
}

fn raw_parts(parts: Vec<Part>) -> Result<Box<RawValue>, serde_json::Error> {
	serde_json::value::to_raw_value(&parts)
}

/// Produces the exact typed durable shape used by the live tool, rather than
/// letting an old gallery-only object masquerade as the payload.
fn fixture_payload(
	tool: &str,
	args: &str,
	text: &str,
) -> Result<serde_json::Value, serde_json::Error> {
	let value: serde_json::Value = serde_json::from_str(text)?;
	let args: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
	Ok(match tool {
		"hub" => serde_json::json!({ "text": serde_json::to_string(&value)?, "useless": false }),
		"web_search" => {
			let mut response = value.as_object().cloned().unwrap_or_default();
			if let Some(provider) = response.remove("provider") {
				response.entry("engine".to_owned()).or_insert(provider);
			}
			serde_json::json!({ "response": response })
		},
		"debug" => serde_json::json!({
			"action": args.get("action").cloned().unwrap_or_else(|| serde_json::json!("output")),
			"session": null,
			"revision": null,
			"output": "",
			"data": value,
		}),
		"lsp" => serde_json::json!({
			"action": args.get("action").cloned().unwrap_or_else(|| serde_json::json!("diagnostics")),
			"servers": [],
			"output": "",
			"data": value,
		}),
		"github" => serde_json::json!({
			"op": args.get("op").cloned().unwrap_or_else(|| serde_json::json!("repo_view")),
			"result": value,
			"rate_limit_remaining": null,
			"rate_limit_reset": null,
		}),
		"goal" => {
			let goal = value.get("goal").map(|goal| serde_json::json!({
				"id": goal.get("id").cloned().unwrap_or_else(|| serde_json::json!("goal")),
				"objective": goal.get("objective").cloned().unwrap_or_else(|| serde_json::json!("")),
				"status": goal.get("status").cloned().unwrap_or_else(|| serde_json::json!("active")),
				"token_budget": goal.get("token_budget").or_else(|| goal.get("tokenBudget")).cloned(),
				"tokens_used": goal.get("tokens_used").or_else(|| goal.get("tokensUsed")).cloned().unwrap_or_else(|| serde_json::json!(0)),
				"time_used_secs": goal.get("time_used_secs").or_else(|| goal.get("timeUsedSeconds")).cloned().unwrap_or_else(|| serde_json::json!(0)),
			}));
			serde_json::json!({
				"op": value.get("op").cloned().unwrap_or_else(|| serde_json::json!("get")),
				"goal": goal,
				"remaining_tokens": value.get("remaining_tokens").or_else(|| value.get("remainingTokens")).cloned(),
				"completion_report": value.get("completion_report").or_else(|| value.get("completionBudgetReport")).cloned(),
			})
		},
		"ask" => {
			let answers = value
				.get("answers")
				.and_then(serde_json::Value::as_array)
				.into_iter()
				.flatten()
				.map(|answer| serde_json::json!({
					"id": answer.get("id").or_else(|| answer.get("question")).cloned().unwrap_or_default(),
					"selected": answer.get("selected").or_else(|| answer.get("options")).cloned().unwrap_or_else(|| serde_json::json!([])),
					"customInput": answer.get("customInput").cloned(),
					"note": answer.get("note").cloned(),
					"timed_out": false,
				}))
				.collect::<Vec<_>>();
			serde_json::json!({ "answers": answers, "headless": false })
		},
		"bash" => {
			let projection = value
				.get("transcript")
				.and_then(Value::as_array)
				.into_iter()
				.flatten()
				.filter_map(|frame| frame.get("data").and_then(Value::as_str))
				.collect::<String>();
			serde_json::json!({
				"session_id": [],
				"exec_id": [],
				"command": args.get("command").cloned().unwrap_or_else(|| serde_json::json!("")),
				"transcript": [],
				"attachments": [],
				"adjustments": [],
				"status": {
					"outcome": "exited",
					"exit_code": value.pointer("/status/exit_code").cloned(),
					"signal": null,
					"wall_clock_ms": value.pointer("/status/wall_clock_ms").cloned().unwrap_or_else(|| serde_json::json!(0)),
					"spilled_output": null,
					"aborted": false,
					"effects_unknown": false,
					"final_cwd_uri": null,
					"final_cwd_revision": 0
				},
				"_projection": projection
			})
		},
		"eval" => {
			let projection = value
				.get("frames")
				.and_then(Value::as_array)
				.into_iter()
				.flatten()
				.filter_map(|frame| frame.get("data").and_then(Value::as_str))
				.collect::<String>();
			serde_json::json!({
				"session_id": [],
				"cell_id": [],
				"language": args.get("language").cloned().unwrap_or_else(|| serde_json::json!("py")),
				"title": args.get("title").cloned(),
				"code": args.get("code").cloned().unwrap_or_else(|| serde_json::json!("")),
				"reset": args.get("reset").cloned().unwrap_or_else(|| serde_json::json!(false)),
				"had_output": !projection.is_empty(),
				"result": value.get("result").cloned(),
				"display_outputs": value.get("display_outputs").cloned().unwrap_or_else(|| serde_json::json!([])),
				"status": {
					"outcome": "complete",
					"exit_code": 0,
					"duration_ms": value.pointer("/status/duration_ms").cloned().unwrap_or_else(|| serde_json::json!(0)),
					"exception": null
				},
				"_projection": projection
			})
		},
		"glob" => {
			let matches = value
				.get("matches")
				.or_else(|| value.get("files"))
				.and_then(Value::as_array)
				.into_iter()
				.flatten()
				.map(|entry| {
					serde_json::json!({
						"path": entry.get("path").cloned().unwrap_or_else(|| entry.clone()),
						"modified_ms": 0,
						"is_dir": false
					})
				})
				.collect::<Vec<_>>();
			let count = value
				.get("partial_match_count")
				.or_else(|| value.get("file_count"))
				.cloned()
				.unwrap_or_else(|| serde_json::json!(matches.len()));
			serde_json::json!({
				"matches": matches,
				"missing_paths": [],
				"timed_out": false,
				"truncated": false,
				"result_limit_reached": null,
				"partial_match_count": count.clone(),
				"timeout_ms": 0,
				"projected_text": "",
				"output_blob": null,
				"output_artifact_uri": null,
				"output_shown_lines": count.clone(),
				"output_total_lines": count
			})
		},
		"grep" => {
			let mut files = serde_json::Map::new();
			for row in value
				.get("matches")
				.and_then(Value::as_array)
				.into_iter()
				.flatten()
			{
				let path = row.get("path").and_then(Value::as_str).unwrap_or_default();
				files
					.entry(path.to_owned())
					.or_insert_with(|| serde_json::json!([]))
					.as_array_mut()
					.expect("inserted array")
					.push(serde_json::json!({
						"line_number": row.get("line").cloned().unwrap_or_else(|| serde_json::json!(0)),
						"line": row.get("text").cloned().unwrap_or_else(|| serde_json::json!("")),
						"truncated": false,
						"context_before": [],
						"context_after": []
					}));
			}
			let groups = files
				.into_iter()
				.map(|(path, matches)| {
					serde_json::json!({
						"path": path.clone(),
						"source_key": path,
						"snapshot_tag": null,
						"matches": matches
					})
				})
				.collect::<Vec<_>>();
			serde_json::json!({
				"total_files": groups.len(),
				"files": groups,
				"total_files_lower_bound": false,
				"multi_scope": true,
				"skip": 0,
				"file_limit_reached": false,
				"per_file_limit_reached": false,
				"notes": [],
				"projected_text": "",
				"output_blob": null,
				"output_artifact_uri": null,
				"output_shown_lines": 0,
				"output_total_lines": 0
			})
		},
		"ast_grep" => {
			let matches = value
				.get("matches")
				.and_then(Value::as_array)
				.into_iter()
				.flatten()
				.map(|entry| {
					let line = entry.get("line").cloned().unwrap_or_else(|| serde_json::json!(1));
					let bindings = match entry.get("bindings") {
						Some(Value::Object(fields)) => fields
							.iter()
							.map(|(key, value)| format!("${key}={}", value.as_str().map_or_else(|| value.to_string(), str::to_owned)))
							.collect::<Vec<_>>()
							.join(", "),
						Some(Value::String(text)) => text.clone(),
						_ => String::new(),
					};
					serde_json::json!({
						"path": entry.get("path").cloned().unwrap_or_else(|| serde_json::json!("")),
						"line": line,
						"column": entry.get("column").cloned().unwrap_or_else(|| serde_json::json!(1)),
						"end_line": entry.get("end_line").cloned().unwrap_or(line),
						"end_column": entry.get("end_column").cloned().unwrap_or_else(|| serde_json::json!(1)),
						"text": entry.get("text").cloned().unwrap_or_else(|| serde_json::json!("")),
						"bindings": bindings
					})
				})
				.collect::<Vec<_>>();
			let total = value
				.get("match_count")
				.or_else(|| value.get("total"))
				.cloned()
				.unwrap_or_else(|| serde_json::json!(matches.len()));
			serde_json::json!({
				"matches": matches,
				"advisories": [],
				"total": total,
				"next_skip": null,
				"files_searched": value.get("files_searched").cloned().unwrap_or_else(|| serde_json::json!(0))
			})
		},
		"todo" => {
			let phases = value
				.get("phases")
				.and_then(Value::as_array)
				.into_iter()
				.flatten()
				.map(|phase| {
					let tasks = phase
						.get("tasks")
						.or_else(|| phase.get("items"))
						.and_then(Value::as_array)
						.into_iter()
						.flatten()
						.map(|task| serde_json::json!({
							"content": task.get("content").or_else(|| task.get("text")).cloned().unwrap_or_else(|| serde_json::json!("")),
							"status": task.get("status").cloned().unwrap_or_else(|| serde_json::json!("pending")),
							"blocker": task.get("blocker").cloned()
						}))
						.collect::<Vec<_>>();
					serde_json::json!({
						"name": phase.get("name").or_else(|| phase.get("phase")).cloned().unwrap_or_else(|| serde_json::json!("")),
						"tasks": tasks
					})
				})
				.collect::<Vec<_>>();
			serde_json::json!({
				"op": args.get("op").cloned().unwrap_or_else(|| serde_json::json!("view")),
				"phases": phases,
				"completed_tasks": value.get("completed_tasks").cloned().unwrap_or_else(|| serde_json::json!([]))
			})
		},
		"browser" => serde_json::json!({
			"action": value.get("action").or_else(|| args.get("action")).cloned().unwrap_or_else(|| serde_json::json!("run")),
			"name": value.get("name").or_else(|| args.get("name")).cloned().unwrap_or_else(|| serde_json::json!("main")),
			"url": value.get("url").cloned(),
			"title": value.get("title").cloned(),
			"result": value.get("result").cloned().or_else(|| value.get("display").cloned()),
			"artifacts": value.get("artifacts").cloned().unwrap_or_else(|| serde_json::json!([])),
			"browser": value.get("browser").cloned()
		}),
		"computer" => serde_json::json!({
			"code": args.get("code").cloned().unwrap_or_else(|| serde_json::json!("")),
			"results": value.get("results").cloned().unwrap_or_else(|| serde_json::json!([])),
			"artifacts": value.get("artifacts").cloned().unwrap_or_else(|| serde_json::json!([]))
		}),
		"task" => {
			let children = value
				.get("children")
				.or_else(|| value.get("results"))
				.and_then(serde_json::Value::as_array)
				.into_iter()
				.flatten()
				.map(|child| serde_json::json!({
					"id": child.get("id").or_else(|| child.get("job")).cloned().unwrap_or_else(|| serde_json::json!("agent")),
					"agent": child.get("agent").cloned().unwrap_or_else(|| serde_json::json!("task")),
					"text": child.get("text").or_else(|| child.get("output")).cloned().unwrap_or_else(|| serde_json::json!("")),
					"session_path": child.get("session_path").cloned().unwrap_or_else(|| serde_json::json!("")),
					"tokens_in": child.get("tokens_in").or_else(|| child.get("context_tokens")).cloned().unwrap_or_else(|| serde_json::json!(0)),
					"tokens_out": child.get("tokens_out").cloned().unwrap_or_else(|| serde_json::json!(0)),
					"output": null,
					"workspace": null,
					"error": child.get("error").cloned(),
				}))
				.collect::<Vec<_>>();
			serde_json::json!({ "children": children })
		},
		"recall" => {
			let items = value
				.get("items")
				.and_then(Value::as_array)
				.into_iter()
				.flatten()
				.map(|item| {
					serde_json::json!({
						"memory": {
							"id": item.get("id").cloned().unwrap_or_else(|| serde_json::json!("memory")),
							"bank": item.get("bank").cloned().unwrap_or_else(|| serde_json::json!("global")),
							"tier": "working",
							"content": item.get("content").cloned().unwrap_or_else(|| serde_json::json!("")),
							"source": null,
							"session_id": "gallery",
							"timestamp": "2026-01-01T00:00:00Z",
							"importance": 0.5,
							"veracity": "observed",
							"memory_type": "fact",
							"metadata": {
								"context": item.get("context").cloned().unwrap_or(serde_json::Value::Null)
							},
							"superseded_by": null
						},
						"score": item.get("score").cloned().unwrap_or_else(|| serde_json::json!(0.0)),
						"voice_scores": {"vector":0.0,"graph":0.0,"episodic":0.0,"working":0.0},
						"broadened": false
					})
				})
				.collect::<Vec<_>>();
			serde_json::json!({
				"query": value.get("query").cloned().unwrap_or_else(|| serde_json::json!("")),
				"items": items
			})
		},
		"read" => {
			let text = value
				.get("preview_text")
				.and_then(serde_json::Value::as_str)
				.unwrap_or_default();
			serde_json::json!({ "parts": [{ "kind": "text", "text": text }] })
		},
		"write" => {
			let path = args
				.get("path")
				.and_then(serde_json::Value::as_str)
				.unwrap_or_default();
			let content = args
				.get("content")
				.and_then(serde_json::Value::as_str)
				.unwrap_or_default();
			serde_json::json!({
				"resolved_path": path,
				"display_path": path,
				"canonical_recovery": null,
				"byte_len": content.len(),
				"reported_len": content.encode_utf16().count(),
				"disposition": value.get("disposition").cloned().unwrap_or_else(|| serde_json::json!("created")),
				"stripped_wrapper": false,
				"made_executable": false,
				"snapshot_tag": null,
				"operation": { "kind": "plain" },
			})
		},
		_ => value,
	})
}

fn fixture_fault(tool: &str, value: serde_json::Value) -> serde_json::Value {
	if tool != "web_search" {
		return value;
	}
	let message = value
		.get("message")
		.or_else(|| value.get("error"))
		.and_then(Value::as_str)
		.or_else(|| value.as_str())
		.unwrap_or("search failed");
	serde_json::json!({ "kind": "search", "code": "gallery", "message": message })
}

/// Model-facing parts are persisted beside the outcome exactly as production
/// dispatch does. Cards must ignore this bounded projection and decode the
/// typed outcome; wrapper tools explicitly unwrap their projection contract.
fn projected_parts(tool: &str, value: &serde_json::Value) -> Result<Vec<Part>, serde_json::Error> {
	let text = match tool {
		"hub" => value
			.get("text")
			.and_then(Value::as_str)
			.unwrap_or_default()
			.to_owned(),
		"bash" | "eval" => value
			.get("_projection")
			.and_then(Value::as_str)
			.unwrap_or_default()
			.to_owned(),
		"web_search" => web_projection(value.pointer("/response").unwrap_or(&Value::Null)),
		"task" => value
			.get("children")
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
			.filter_map(|child| child.get("text").and_then(Value::as_str))
			.collect::<Vec<_>>()
			.join("\n"),
		"read" => value
			.get("parts")
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
			.filter_map(|part| part.get("text").and_then(Value::as_str))
			.collect::<Vec<_>>()
			.join("\n"),
		_ => value
			.as_str()
			.or_else(|| {
				value
					.get("projected_text")
					.or_else(|| value.get("output"))
					.or_else(|| value.get("message"))
					.and_then(Value::as_str)
			})
			.map(str::to_owned)
			.unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default()),
	};
	Ok(vec![Part::Text { text: Str::new(text) }])
}

fn web_projection(response: &Value) -> String {
	use std::fmt::Write as _;
	let mut text = response
		.get("answer")
		.and_then(Value::as_str)
		.unwrap_or_default()
		.to_owned();
	let sources = response
		.get("sources")
		.and_then(Value::as_array)
		.map(Vec::as_slice)
		.unwrap_or_default();
	if !sources.is_empty() {
		if !text.is_empty() {
			text.push_str("\n\n");
		}
		text.push_str("## Sources\n\n");
		for (index, source) in sources.iter().enumerate() {
			let _ = write!(
				text,
				"{}. [{}]({})",
				index + 1,
				source
					.get("title")
					.and_then(Value::as_str)
					.unwrap_or_default(),
				source
					.get("url")
					.and_then(Value::as_str)
					.unwrap_or_default(),
			);
			if let Some(snippet) = source.get("snippet").and_then(Value::as_str)
				&& !snippet.is_empty()
			{
				let _ = write!(text, " — {snippet}");
			}
			text.push('\n');
		}
	}
	text
}

fn find_snapshot_call(snapshot: &Snapshot, call_id: &str) -> Option<Handle> {
	snapshot.handles().find(|handle| {
		snapshot.get(*handle).is_some_and(|node| {
			matches!(&node.tag, Tag::Custom(_))
				&& node
					.prop(&PropId::Id.into())
					.and_then(DomValue::as_str)
					.is_some_and(|id| id == call_id)
		})
	})
}

fn child(snapshot: &Snapshot, parent: Handle, tag: KnownTag) -> Option<&Node> {
	snapshot
		.children(parent)
		.iter()
		.filter_map(|handle| snapshot.get(*handle))
		.find(|node| node.tag == Tag::Known(tag))
}

fn node_text(node: &Node) -> Option<&str> {
	node
		.prop(&PropId::Text.into())
		.and_then(DomValue::as_str)
		.filter(|text| !text.is_empty())
		.or(node.content.as_deref())
}

fn card_tool(tool: &str) -> &str {
	match tool {
		"read_group" => "read",
		"edit_delete" | "edit_move" => "edit",
		"report_tool_issue" => "report_issue",
		"hub_inbox" | "hub_jobs" | "hub_list" | "hub_logs" | "hub_send" | "hub_start"
		| "hub_wait" => "hub",
		"custom" => "Custom Tool",
		other => other,
	}
}

#[cfg(test)]
mod tests {
	use omp_tui::frame_text;

	use super::{GalleryState, fixture_names, render_sections};

	#[test]
	fn gallery_fixture_inventory_is_complete() {
		assert_eq!(fixture_names(), [
			"apply_patch",
			"ask",
			"ast_edit",
			"ast_grep",
			"bash",
			"browser",
			"computer",
			"context_gauge",
			"custom",
			"debug",
			"edit",
			"edit_delete",
			"edit_move",
			"eval",
			"github",
			"glob",
			"goal",
			"grep",
			"hub",
			"hub_inbox",
			"hub_jobs",
			"hub_list",
			"hub_logs",
			"hub_send",
			"hub_start",
			"hub_wait",
			"inspect_image",
			"lsp",
			"read",
			"read_group",
			"recall",
			"reflect",
			"reject",
			"report_tool_issue",
			"resolve",
			"retain",
			"task",
			"think",
			"todo",
			"vibe_kill",
			"vibe_list",
			"vibe_send",
			"vibe_spawn",
			"vibe_wait",
			"web_search",
			"write",
		]);
	}

	#[test]
	fn gallery_materializes_every_read_lifecycle_through_session() {
		let sections = render_sections(Some("read"), &GalleryState::ALL, 100, false)
			.expect("read fixtures should fold and render");
		assert_eq!(sections.len(), GalleryState::ALL.len());
		for (section, state) in sections.iter().zip(GalleryState::ALL) {
			assert_eq!(section.state, state);
			assert_eq!(section.frame.size().width, 100);
			assert!(!frame_text(&section.frame).trim().is_empty());
		}
	}

	#[test]
	fn all_46_fixtures_use_projected_production_settlement() {
		let sections = render_sections(None, &GalleryState::ALL, 100, false)
			.expect("every fixture should fold through settle_projected/fail_projected");
		assert_eq!(fixture_names().len(), 46);
		assert_eq!(sections.len(), 46 * GalleryState::ALL.len());
		assert!(
			sections
				.iter()
				.all(|section| !frame_text(&section.frame).trim().is_empty()),
			"every lifecycle frame must carry meaningful presentation"
		);
	}
}
