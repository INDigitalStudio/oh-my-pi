//! Pure assembly of a child's `yield` calls into the payload that output-schema
//! validation consumes (pi `task/yield-assembly.ts`).
//!
//! An array-typed `type` contributes an incremental section and never decides
//! termination on its own; a string-typed `type` with an empty `result` makes
//! the child's last assistant turn the raw terminal result; any other terminal
//! yield contributes the complete payload verbatim. When the run ends with only
//! incremental sections, the accumulated sections are the result.

use omp_core::Str;
use omp_dom::{PropId, PropKey, Value};
use omp_session::Session;
use omp_tools::yield_tool::{Params as YieldParams, ResultEnvelope, YieldType};
use serde_json::{Map, Value as Json};

/// Standard prefix when a yield explicitly contains null data.
pub(crate) const WARNING_NULL_YIELD: &str =
	"[subagent null yield] no usable structured data was returned";

/// Terminal payload folded from every successful `yield` call of one run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Assembled {
	/// No yield decided the run.
	Missing,
	/// The run's complete structured (or raw-text) result.
	Data(Json),
	/// The child explicitly failed, or finalized without usable data.
	Error(String),
}

impl Assembled {
	/// Splits into the `(data, error)` pair the spawner reports.
	pub(crate) fn into_parts(self) -> (Option<Json>, Option<String>) {
		match self {
			Self::Missing => (None, None),
			Self::Data(data) => (Some(data), None),
			Self::Error(error) => (None, Some(error)),
		}
	}
}

/// Every `yield` call that settled `ok`, in journal order.
pub(crate) fn settled_yields(session: &Session) -> Vec<YieldParams> {
	let dom = session.dom();
	let yield_tag = omp_dom::Tag::Custom(Str::new_static("yield"));
	dom.handles()
		.filter_map(|handle| {
			let node = dom.get(handle)?;
			if node.tag != yield_tag
				|| node
					.prop(&PropKey::from(PropId::Status))
					.and_then(Value::as_str)
					!= Some("ok")
			{
				return None;
			}
			let input = dom.children(handle).iter().find_map(|child| {
				let node = dom.get(*child)?;
				(node.tag == omp_dom::Tag::Known(omp_dom::KnownTag::Input)).then_some(node)
			})?;
			let raw = input.content.as_deref().or_else(|| {
				input
					.prop(&PropKey::from(PropId::Text))
					.and_then(Value::as_str)
			})?;
			serde_json::from_str::<YieldParams>(raw).ok()
		})
		.collect()
}

/// Top-level output-schema property names declared as arrays. An incremental
/// section for such a label accumulates into a list even when the child emits
/// exactly one, so a single `type: ["findings"]` yield still validates.
pub(crate) fn array_valued_labels(schema: &Json) -> Vec<&str> {
	schema
		.get("properties")
		.and_then(Json::as_object)
		.map(|properties| {
			properties
				.iter()
				.filter(|(_, property)| is_array_typed(schema, property, 0))
				.map(|(name, _)| name.as_str())
				.collect()
		})
		.unwrap_or_default()
}

fn is_array_typed(root: &Json, schema: &Json, depth: u8) -> bool {
	const MAX_REF_DEPTH: u8 = 8;
	let Some(record) = schema.as_object() else {
		return false;
	};
	match record.get("type") {
		Some(Json::String(kind)) if kind == "array" => return true,
		Some(Json::Array(kinds)) if kinds.iter().any(|kind| kind.as_str() == Some("array")) => {
			return true;
		},
		_ => {},
	}
	if depth < MAX_REF_DEPTH
		&& let Some(reference) = record.get("$ref").and_then(Json::as_str)
		&& let Some(target) = resolve_local_ref(root, reference)
		&& is_array_typed(root, target, depth.saturating_add(1))
	{
		return true;
	}
	["anyOf", "oneOf", "allOf"].iter().any(|key| {
		record
			.get(*key)
			.and_then(Json::as_array)
			.is_some_and(|variants| {
				variants
					.iter()
					.any(|variant| is_array_typed(root, variant, depth.saturating_add(1)))
			})
	})
}

fn resolve_local_ref<'s>(root: &'s Json, reference: &str) -> Option<&'s Json> {
	let pointer = reference.strip_prefix('#')?;
	root.pointer(pointer)
}

/// Folds the run's yields (journal order) into its terminal payload.
///
/// `last_turn` is the child's final assistant text, used by a string-typed
/// terminal yield with an empty `result`. `array_labels` names the sections
/// that always accumulate into a list.
pub(crate) fn assemble(yields: &[YieldParams], last_turn: &str, array_labels: &[&str]) -> Assembled {
	let Some(last) = yields.last() else {
		return Assembled::Missing;
	};
	// pi `finalizeSubprocessOutput`: an aborting final yield ends the run
	// with its error regardless of what was accumulated before it.
	if let ResultEnvelope::Error { error } = &last.result {
		return Assembled::Error(error.to_string());
	}
	let terminal = yields
		.iter()
		.rev()
		.find(|params| !matches!(params.kind, Some(YieldType::Sections(_))));
	let mut sections = Map::new();
	let mut missing_data = false;
	for params in yields {
		let Some(YieldType::Sections(labels)) = &params.kind else {
			continue;
		};
		let value = match &params.result {
			ResultEnvelope::Data { data } => data.clone(),
			// Aborted sections are skipped; a data-less section reads the
			// last assistant turn.
			ResultEnvelope::Error { .. } => continue,
			ResultEnvelope::LastTurn {} => Json::String(last_turn.to_owned()),
		};
		missing_data |= value.is_null() || matches!(&value, Json::String(text) if text.is_empty());
		for label in labels {
			let label = label.as_str().trim();
			if label.is_empty() {
				continue;
			}
			append_section(&mut sections, label, value.clone(), array_labels.contains(&label));
		}
	}
	match terminal.map(|params| &params.result) {
		// An explicit terminal payload wins and is used verbatim, never
		// wrapped in a section.
		Some(ResultEnvelope::Data { data }) if data.is_null() => {
			Assembled::Error(WARNING_NULL_YIELD.to_owned())
		},
		Some(ResultEnvelope::Data { data }) => Assembled::Data(data.clone()),
		Some(ResultEnvelope::Error { error }) => Assembled::Error(error.to_string()),
		// A data-less terminal finalize keeps accumulated sections; only when
		// none exist does the last assistant turn become the raw result.
		_ if !sections.is_empty() => {
			if missing_data {
				Assembled::Error(WARNING_NULL_YIELD.to_owned())
			} else {
				Assembled::Data(Json::Object(sections))
			}
		},
		None => Assembled::Missing,
		Some(ResultEnvelope::LastTurn {}) if last_turn.is_empty() => {
			Assembled::Error(WARNING_NULL_YIELD.to_owned())
		},
		Some(ResultEnvelope::LastTurn {}) => Assembled::Data(Json::String(last_turn.to_owned())),
	}
}

fn append_section(sections: &mut Map<String, Json>, label: &str, value: Json, force_array: bool) {
	match sections.get_mut(label) {
		None => {
			let value = if force_array { Json::Array(vec![value]) } else { value };
			sections.insert(label.to_owned(), value);
		},
		Some(Json::Array(existing)) => existing.push(value),
		Some(existing) => {
			let first = std::mem::take(existing);
			*existing = Json::Array(vec![first, value]);
		},
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn params(value: Json) -> YieldParams {
		serde_json::from_value(value).expect("yield params")
	}

	#[test]
	fn incremental_sections_merge_into_a_data_less_terminal_yield() {
		let yields = [
			params(json!({"type": ["summary"], "result": {"data": "first pass"}})),
			params(json!({"type": ["findings"], "result": {"data": {"title": "a"}}})),
			params(json!({"type": ["findings"], "result": {"data": {"title": "b"}}})),
			params(json!({"type": "result", "result": {}})),
		];
		assert_eq!(
			assemble(&yields, "ignored last turn", &[]),
			Assembled::Data(json!({
				"summary": "first pass",
				"findings": [{"title": "a"}, {"title": "b"}],
			}))
		);
	}

	#[test]
	fn sections_alone_finalize_on_idle() {
		let yields = [params(json!({"type": ["notes"], "result": {"data": {"ok": true}}}))];
		assert_eq!(assemble(&yields, "", &[]), Assembled::Data(json!({"notes": {"ok": true}})));
	}

	#[test]
	fn array_valued_labels_accumulate_a_single_section_into_a_list() {
		let schema = json!({
			"type": "object",
			"properties": {
				"findings": {"$ref": "#/$defs/list"},
				"summary": {"type": "string"},
				"either": {"anyOf": [{"type": "null"}, {"type": ["array", "null"]}]},
			},
			"$defs": {"list": {"type": "array", "items": {"type": "object"}}},
		});
		let labels = array_valued_labels(&schema);
		assert_eq!(labels, ["findings", "either"]);
		let yields = [params(json!({"type": ["findings"], "result": {"data": {"title": "only"}}}))];
		assert_eq!(
			assemble(&yields, "", &labels),
			Assembled::Data(json!({"findings": [{"title": "only"}]}))
		);
	}

	#[test]
	fn explicit_terminal_data_wins_over_sections() {
		let yields = [
			params(json!({"type": ["findings"], "result": {"data": [1]}})),
			params(json!({"result": {"data": {"complete": true}}})),
		];
		assert_eq!(assemble(&yields, "", &[]), Assembled::Data(json!({"complete": true})));
	}

	#[test]
	fn last_turn_terminal_uses_assistant_text_only_without_sections() {
		let only_last_turn = [params(json!({"type": "result", "result": {}}))];
		assert_eq!(assemble(&only_last_turn, "final words", &[]), Assembled::Data(json!("final words")));
		assert_eq!(
			assemble(&only_last_turn, "", &[]),
			Assembled::Error(WARNING_NULL_YIELD.to_owned())
		);
	}

	#[test]
	fn a_failing_final_yield_ends_the_run_with_its_error() {
		let yields = [
			params(json!({"type": ["findings"], "result": {"data": [1]}})),
			params(json!({"result": {"error": "blocked"}})),
		];
		assert_eq!(assemble(&yields, "", &[]), Assembled::Error("blocked".to_owned()));
	}

	#[test]
	fn null_data_is_a_null_yield() {
		let yields = [params(json!({"result": {"data": null}}))];
		assert_eq!(assemble(&yields, "", &[]), Assembled::Error(WARNING_NULL_YIELD.to_owned()));
		assert_eq!(assemble(&[], "", &[]), Assembled::Missing);
	}
}
