//! Multi-target structural search with stable pagination and hashline
//! locations.

use std::{fs, path::PathBuf, sync::Arc};

use async_stream::stream;
use bytes::Bytes;
use futures::Stream;
use omp_core::{Str, sf};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CallOutcome, CommitError, Constraint, DocEffects, Effects, Ev,
	IncomingParams, LiftedCall, ParamError, Part, PromptCaps, RecordedCall, Rev, Tool, ToolSpec,
	ToolTerminal,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const PAGE_LIMIT: usize = 50;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
/// Agent-supplied structural search arguments.
pub struct Params {
	/// Ast-grep structural pattern, including any metavariables to bind.
	pub pat:  Str,
	#[serde(default)]
	/// Semicolon-separated workspace-relative files, directories, or globs;
	/// defaults to `"."`.
	pub path: Option<Str>,
	#[serde(default)]
	/// Matches to skip before the page starts; defaults to `0`.
	pub skip: usize,
}

/// `ast_grep@1` argument shape, retained only to lift historical calls.
#[derive(Deserialize)]
struct ParamsV1 {
	pat:     Str,
	#[serde(default)]
	path:    Option<Str>,
	#[serde(default)]
	cursor:  usize,
	#[serde(default, rename = "limit")]
	_limit:  Option<usize>,
	#[serde(default)]
	i:       Option<Str>,
	#[serde(default)]
	notrunc: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// One structural source match returned to the agent.
pub struct Match {
	/// Workspace-relative path of the matched source file.
	pub path:       Str,
	/// One-based source line at which the matched node starts.
	pub line:       usize,
	/// One-based source column at which the matched node starts.
	pub column:     usize,
	/// One-based source line at which the matched node ends.
	pub end_line:   usize,
	/// One-based source column at which the matched node ends.
	pub end_column: usize,
	/// Exact source text covered by the matched AST node.
	pub text:       Str,
	/// Stable, display-ready metavariable bindings (`$A=value, $B=value`).
	pub bindings:   Str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// Non-fatal reason a targeted file could not be searched.
pub struct Advisory {
	/// Workspace-relative path of the skipped target.
	pub path:    Str,
	/// Language-resolution, pattern-compilation, or file-read explanation.
	pub message: Str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// Paginated structural-search result returned to the agent.
pub struct Payload {
	/// Current page of matches in stable path and source order.
	pub matches:    Vec<Match>,
	/// Per-file failures that did not prevent other targets from being searched.
	pub advisories: Vec<Advisory>,
	/// Number of matches across all targets before pagination.
	pub total:      usize,
	/// `skip` value that resumes at the next page, or `None` when this is the
	/// final page.
	pub next_skip:  Option<usize>,
	/// Files the search opened, including those that produced an advisory
	/// (pi `filesSearched`); lifted `@1` calls did not record it.
	#[serde(default)]
	pub files_searched: usize,
}

/// `ast_grep@1` payload shape, retained only to lift historical verdicts.
#[derive(Deserialize)]
struct PayloadV1 {
	matches:     Vec<Match>,
	advisories:  Vec<Advisory>,
	total:       usize,
	next_cursor: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// Empty update type because structural search emits only a terminal result.
pub enum Update {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
/// Terminal argument, target-discovery, or search failure.
pub struct Fault {
	message: Str,
}

/// Workspace-scoped structural-search tool exposed as `ast_grep`.
pub struct AstGrep {
	root: PathBuf,
	spec: ToolSpec,
}

/// Returns the host-free `ast_grep@2` specification.
pub fn spec() -> ToolSpec {
	ToolSpec {
		name:            sf!("ast_grep"),
		rev:             Rev { family: Default::default(), n: 2 },
		description:     sf!(
			"Searches multiple files structurally with ast-grep metavariables. `path` accepts \
			 semicolon-separated files, directories, and globs. Results use stable path/source \
			 ordering; `skip` resumes pagination past that many matches."
		),
		schema:          omp_tool::schema::<Params>(),
		constraint:      Constraint::Schema {
			priority:       100,
			on_unsupported: omp_tool::Fallback::Unspecified,
		},
		effects:         Effects {
			documents: Some(DocEffects { read: true, write_globs: Arc::default() }),
			exec:      None,
			inference: None,
			desktop:   None,
			subagents: 0,
		},
		projection_code: omp_tool::native_projection_code(
			env!("CARGO_PKG_NAME"),
			env!("CARGO_PKG_VERSION"),
			include_bytes!("ast_grep.rs"),
		)
		.into(),
	}
}

/// Builds an `ast_grep` tool whose relative files and globs resolve under
/// `root`.
pub fn tool(root: PathBuf) -> AstGrep {
	AstGrep { root, spec: spec() }
}

impl Tool for AstGrep {
	type Fault = Fault;
	type Params = Params;
	type Payload = Payload;
	type Update = Update;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut incoming: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<Update, Payload, Fault>> + Send + 'c {
		stream! {
			let params = match incoming.whole::<Params>().await { Ok(v) => v, Err(e) => { yield param_event(e); return; } };
			if params.pat.trim().is_empty() { yield done(Err(Fault { message: sf!("pat must not be empty") })); return; }
			if let Err(error) = incoming.interruptable().committed().await { yield commit_event(error); return; }
			let targets = params.path.as_deref().unwrap_or(".").split(';').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect::<Vec<_>>();
			let files = match omp_ast::ops::collect_matched_files(&self.root, &targets) { Ok(v) => v, Err(e) => { yield done(Err(Fault { message: Str::new(e.to_string()) })); return; } };
			let mut matches = Vec::new();
			let mut advisories = Vec::new();
			let files_searched = files.len();
			for file in files {
				let language = match omp_ast::ops::resolve_language(None, &file.absolute_path) { Ok(v) => v, Err(e) => { advisories.push(Advisory { path: file.relative_path, message: Str::new(e.to_string()) }); continue; } };
				let patterns = match omp_ast::ops::compile_search_patterns(&params.pat, language) { Ok(v) => v, Err(e) => { advisories.push(Advisory { path: file.relative_path, message: Str::new(e.to_string()) }); continue; } };
				let source = match fs::read_to_string(&file.absolute_path) { Ok(v) => v, Err(e) => { advisories.push(Advisory { path: file.relative_path, message: Str::new(e.to_string()) }); continue; } };
				for found in omp_ast::ops::collect_matches(&source, language, &patterns) {
					matches.push(Match { path: file.relative_path.clone(), line: found.line, column: found.column, end_line: found.end_line, end_column: found.end_column, text: found.text, bindings: render_bindings(&found.bindings) });
				}
			}
			matches.sort_unstable_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)).then(a.column.cmp(&b.column)));
			let total = matches.len();
			let start = params.skip.min(total);
			let end = start.saturating_add(PAGE_LIMIT).min(total);
			let page = matches.drain(start..end).collect();
			yield done(Ok(Payload { matches: page, advisories, total, next_skip: (end < total).then_some(end), files_searched }));
		}
	}

	fn lift(&self, from: &Rev, call: RecordedCall<'_>) -> Option<LiftedCall> {
		lift_cursor_to_skip(from, call)
	}

	fn prompt(&self, view: Result<&Payload, &Fault>, _: &PromptCaps) -> Vec<Part> {
		let text = match view {
			Err(e) => Str::new(e.to_string()),
			Ok(payload) => {
				let mut out = String::new();
				for found in &payload.matches {
					use std::fmt::Write as _;
					let _ =
						writeln!(out, "{}:{}:{}\n{}", found.path, found.line, found.column, found.text);
					if !found.bindings.is_empty() {
						let _ = writeln!(out, "  meta: {}", found.bindings);
					}
				}
				for advisory in &payload.advisories {
					use std::fmt::Write as _;
					let _ = writeln!(out, "[advisory {}] {}", advisory.path, advisory.message);
				}
				if let Some(skip) = payload.next_skip {
					use std::fmt::Write as _;
					let _ = writeln!(out, "[next skip: {skip}; total: {}]", payload.total);
				}
				Str::new(out)
			},
		};
		vec![Part::Text { text }]
	}
}
fn render_bindings(bindings: &[omp_ast::ops::AstBinding]) -> Str {
	Str::new(
		bindings
			.iter()
			.map(|binding| format!("{}={}", binding.name, binding.value))
			.collect::<Vec<_>>()
			.join(", "),
	)
}
fn done(result: Result<Payload, Fault>) -> Ev<Update, Payload, Fault> {
	Ev::Done(ToolTerminal::Done {
		useless: result.as_ref().is_ok_and(|p| p.matches.is_empty()),
		result,
	})
}
/// Lifts an `ast_grep@1` call (`cursor`/`limit` and `next_cursor`) to the
/// fixed-page `@2` wire (`skip`/`next_skip`); the resume offset is identical.
fn lift_cursor_to_skip(from: &Rev, call: RecordedCall<'_>) -> Option<LiftedCall> {
	if !from.family.is_empty() || from.n != 1 {
		return None;
	}
	let old = serde_json::from_slice::<ParamsV1>(call.raw_args).ok()?;
	let mut raw_args =
		serde_json::to_value(&Params { pat: old.pat, path: old.path, skip: old.cursor }).ok()?;
	if let Some(object) = raw_args.as_object_mut() {
		if let Some(intent) = old.i {
			object.insert("i".to_owned(), serde_json::Value::String(intent.to_string()));
		}
		if let Some(notrunc) = old.notrunc {
			object.insert("notrunc".to_owned(), serde_json::Value::Bool(notrunc));
		}
	}
	let raw_args = serde_json::to_vec(&raw_args).ok()?;
	let verdict = match serde_json::from_slice::<CallOutcome<PayloadV1, Fault>>(call.verdict).ok()? {
		CallOutcome::Ok(payload) => serde_json::to_vec(&CallOutcome::<Payload, Fault>::Ok(Payload {
			matches:    payload.matches,
			advisories: payload.advisories,
			total:      payload.total,
			next_skip:  payload.next_cursor,
			files_searched: 0,
		}))
		.ok()?,
		CallOutcome::Faulted(_) | CallOutcome::ArgsRejected(_) | CallOutcome::Aborted { .. } => {
			call.verdict.to_vec()
		},
	};
	Some(LiftedCall { raw_args: Bytes::from(raw_args), verdict: Bytes::from(verdict) })
}
#[cfg(test)]
mod tests {
	use futures::{StreamExt as _, executor::block_on};
	use omp_ast::ops::AstBinding;

	use super::*;

	#[test]
	fn renders_metavariable_bindings_in_stable_order() {
		let bindings = [AstBinding { name: sf!("$NAME"), value: sf!("answer") }, AstBinding {
			name:  sf!("$VALUE"),
			value: sf!("42"),
		}];
		assert_eq!(render_bindings(&bindings), "$NAME=answer, $VALUE=42");
	}

	#[test]
	fn revision_two_schema_is_the_skip_wire_contract() {
		let spec = spec();
		assert_eq!(spec.rev, Rev { family: Str::default(), n: 2 });
		let schema: serde_json::Value =
			serde_json::from_slice(&spec.schema).expect("ast_grep schema is JSON");
		let mut domain_properties = schema["properties"]
			.as_object()
			.expect("object properties")
			.keys()
			.filter(|name| !matches!(name.as_str(), "i" | "notrunc"))
			.map(String::as_str)
			.collect::<Vec<_>>();
		domain_properties.sort_unstable();
		assert_eq!(domain_properties, ["pat", "path", "skip"]);
		assert_eq!(schema["properties"]["skip"]["type"], "integer");
		assert_eq!(schema["properties"]["skip"]["default"], 0);
		assert!(schema["properties"].get("cursor").is_none());
		assert!(schema["properties"].get("limit").is_none());
		let required = schema["required"].as_array().expect("required fields");
		assert!(required.iter().any(|value| value == "i"));
		assert!(required.iter().any(|value| value == "pat"));
		assert!(!required.iter().any(|value| value == "skip"));
	}

	fn search(root: PathBuf, raw: &str) -> Payload {
		let tool = tool(root);
		let (feed, params) = IncomingParams::channel();
		feed
			.args_committed(Str::new(raw))
			.expect("invocation consumer remains live");
		let events = block_on(tool.call(params).collect::<Vec<_>>());
		let [Ev::Done(ToolTerminal::Done { result: Ok(payload), .. })] = events.as_slice() else {
			panic!("expected one successful ast_grep outcome: {events:?}");
		};
		payload.clone()
	}

	#[test]
	fn skip_resumes_fixed_size_pagination_past_the_first_matches() {
		let dir = tempfile::tempdir().expect("tempdir");
		let source = (0..55)
			.map(|index| format!("call{index}({index});\n"))
			.collect::<String>();
		fs::write(dir.path().join("calls.ts"), source).expect("write calls.ts");

		let first = search(dir.path().to_path_buf(), r#"{"pat":"$F($A)","path":"*.ts"}"#);
		assert_eq!(first.total, 55);
		assert_eq!(first.matches.len(), 50);
		assert_eq!(first.next_skip, Some(50));
		let first_texts = first
			.matches
			.iter()
			.map(|m| m.text.as_str())
			.collect::<Vec<_>>();

		let second = search(dir.path().to_path_buf(), r#"{"pat":"$F($A)","path":"*.ts","skip":50}"#);
		let second_texts = second
			.matches
			.iter()
			.map(|m| m.text.as_str())
			.collect::<Vec<_>>();
		assert_eq!(second.matches.len(), 5);
		assert_eq!(second.next_skip, None);
		for text in &first_texts {
			assert!(!second_texts.contains(text), "{text} reappeared on the second page");
		}
	}

	#[test]
	fn revision_one_cursor_and_limit_are_not_accepted_wire_fields() {
		for raw in [r#"{"pat":"$F($A)","cursor":2}"#, r#"{"pat":"$F($A)","limit":2}"#] {
			let tool = tool(PathBuf::from("."));
			let (feed, params) = IncomingParams::channel();
			feed
				.args_committed(Str::new(raw))
				.expect("invocation consumer remains live");
			let events = block_on(tool.call(params).collect::<Vec<_>>());
			assert!(
				matches!(events.as_slice(), [Ev::Args(_)]),
				"revision one pagination field must be rejected: {events:?}"
			);
		}
	}

	#[test]
	fn lifts_rev1_cursor_calls_onto_skip() {
		let tool = tool(PathBuf::from("."));
		let raw_args = br#"{"i":"Finding calls","notrunc":true,"pat":"$F($A)","cursor":7,"limit":3}"#;
		let verdict =
			br#"{"kind":"ok","value":{"matches":[],"advisories":[],"total":12,"next_cursor":10}}"#;
		let lifted = tool
			.lift(&Rev { family: Default::default(), n: 1 }, RecordedCall { raw_args, verdict })
			.expect("rev 1 lifts to rev 2");
		let params: Params = omp_tool::decode_params(
			std::str::from_utf8(&lifted.raw_args).expect("lifted arguments are UTF-8"),
		)
		.expect("lifted params");
		assert_eq!(params.skip, 7);
		let lifted_args: serde_json::Value =
			serde_json::from_slice(&lifted.raw_args).expect("lifted arguments are JSON");
		assert_eq!(lifted_args["i"], "Finding calls");
		assert_eq!(lifted_args["notrunc"], true);
		assert!(lifted_args.get("limit").is_none());
		let payload = match serde_json::from_slice::<CallOutcome<Payload, Fault>>(&lifted.verdict)
			.expect("lifted verdict")
		{
			CallOutcome::Ok(payload) => payload,
			other => panic!("expected ok verdict: {other:?}"),
		};
		assert_eq!(payload.next_skip, Some(10));
		assert_eq!(payload.total, 12);
		assert!(
			tool
				.lift(&Rev { family: Default::default(), n: 2 }, RecordedCall { raw_args, verdict })
				.is_none()
		);
	}
}
fn param_event(error: ParamError) -> Ev<Update, Payload, Fault> {
	match error {
		ParamError::Args(v) => Ev::Args(*v),
		ParamError::Interrupted(v) => Ev::Aborted(Abort::Interrupted { reason: v.reason }),
		ParamError::Protocol(v) => Ev::Args(issue(v)),
	}
}
fn commit_event(error: CommitError) -> Ev<Update, Payload, Fault> {
	match error {
		CommitError::Aborted => Ev::Aborted(Abort::InputDropped),
		CommitError::Interrupted(v) => Ev::Aborted(Abort::Interrupted { reason: v.reason }),
		CommitError::Protocol(v) => Ev::Args(issue(v)),
	}
}
fn issue(message: Str) -> ArgIssue {
	ArgIssue {
		path:     Vec::new(),
		expected: sf!("one committed JSON argument object"),
		kind:     ArgIssueKind::Protocol,
		example:  None,
		found:    Some(message),
	}
}
