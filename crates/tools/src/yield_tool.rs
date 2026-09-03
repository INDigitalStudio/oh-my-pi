//! Subagent terminal and incremental structured-output submission.

use async_stream::stream;
use futures::Stream;
use omp_core::{Str, sf};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CommitError, Constraint, Effects, Ev, IncomingParams, ParamError,
	Part, PromptCaps, ProtocolSchemaError, Rev, Tool, ToolSpec, ToolTerminal,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::output_schema::{self, OutputStatus, SchemaError, SchemaMode, SchemaViolation};

/// Arguments accepted by `yield@2`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
	/// Terminal label or non-empty incremental section path.
	#[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
	pub kind:   Option<YieldType>,
	/// Success/failure envelope.
	pub result: ResultEnvelope,
}

/// Terminal label or incremental section path.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum YieldType {
	/// Named terminal result.
	Terminal(Str),
	/// Non-empty incremental section path.
	Sections(Vec<Str>),
}

/// Structured success or failure.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ResultEnvelope {
	/// Successful structured output.
	Data {
		/// Caller-schema-bound structured value.
		data: Value,
	},
	/// Terminal failure description.
	Error {
		/// Human-readable failure.
		error: Str,
	},
	/// Typed terminal success which uses the child's last assistant turn.
	LastTurn {},
}

/// Durable yield acknowledgement. The caller consumes the original argument
/// bytes for schema validation; this payload never substitutes for them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Payload {
	/// Whether this is an incremental section.
	pub incremental:   bool,
	/// Whether finalization must consume the child's last assistant turn.
	pub use_last_turn: bool,
	/// Immediate terminal validation verdict when a caller schema is installed.
	pub validation:    Option<OutputStatus>,
}

/// Yield does not stream updates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Update {}

/// Invalid yield envelope or caller-schema result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum Fault {
	/// Incremental section labels were empty.
	#[error("type sections must be non-empty strings")]
	EmptySections,
	/// Last-turn extraction was requested without a terminal type.
	#[error("an empty result requires a terminal string type")]
	LastTurnWithoutTerminalType,
	/// Terminal data violated the installed caller schema.
	#[error(transparent)]
	SchemaViolation(#[from] SchemaViolation),
}

/// Failure to construct a caller-specific yield contract.
#[derive(Debug, thiserror::Error)]
pub enum SchemaContractError {
	/// The caller's output schema is malformed.
	#[error(transparent)]
	Schema(#[from] SchemaError),
	/// The generated parameter schema could not receive protocol fields.
	#[error(transparent)]
	Protocol(#[from] ProtocolSchemaError),
	/// The generated schema could not be encoded.
	#[error("generated yield schema could not be encoded")]
	Json(#[from] serde_json::Error),
}

/// Yield executor. A child-specific instance validates terminal data before
/// the call settles, allowing strict mode to reprompt inside the child rather
/// than reporting a late parent-side failure.
pub struct Yield {
	spec:   ToolSpec,
	schema: Option<Value>,
	mode:   SchemaMode,
}

/// Creates unconstrained `yield@2`.
pub fn tool() -> Yield {
	Yield {
		spec: yield_spec(loose_record_schema_value(), SchemaMode::Permissive)
			.expect("the built-in loose yield schema is valid"),
		schema: None,
		mode: SchemaMode::Permissive,
	}
}

/// Creates `yield@2` with one child's effective output contract.
///
/// `null` selects the unconstrained contract. String schemas are parsed using
/// the same normalization as task settlement.
pub fn tool_for_schema(
	raw_schema: &Value,
	mode: SchemaMode,
) -> Result<Yield, SchemaContractError> {
	let schema = output_schema::normalize(raw_schema)?;
	let data_schema = schema.clone().unwrap_or_else(loose_record_schema_value);
	Ok(Yield { spec: yield_spec(data_schema, mode)?, schema, mode })
}

fn yield_spec(data_schema: Value, mode: SchemaMode) -> Result<ToolSpec, SchemaContractError> {
	let schema = yield_parameter_schema(data_schema);
	let encoded = serde_json::to_vec(&schema)?;
	let schema = omp_tool::inject_protocol_schema(&encoded)?;
	Ok(ToolSpec {
			name:            sf!("yield"),
			rev:             Rev { family: Default::default(), n: 2 },
			description:     sf!(
				"Submits terminal or incremental subagent output. Structured success uses \
				 `result.data`; failure uses `result.error`. A terminal typed yield may pass an \
				 empty `result` object to use the last assistant turn.",
			),
			schema,
			constraint:      if mode == SchemaMode::Strict {
				Constraint::Schema {
					priority: 100,
					on_unsupported: omp_tool::Fallback::Unspecified,
				}
			} else {
				Constraint::None
			},
			effects:         Effects::empty(),
			projection_code: omp_tool::native_projection_code(
				env!("CARGO_PKG_NAME"),
				env!("CARGO_PKG_VERSION"),
				include_bytes!("yield_tool.rs"),
			)
			.into(),
		})
}

fn loose_record_schema_value() -> Value {
	serde_json::json!({
		"type": "object",
		"additionalProperties": true
	})
}

fn yield_parameter_schema(mut data_schema: Value) -> Value {
	let mut root_defs = serde_json::Map::new();
	if let Some(object) = data_schema.as_object_mut() {
		for key in ["$defs", "definitions"] {
			if let Some(value) = object.remove(key) {
				root_defs.insert(key.to_owned(), value);
			}
		}
	}
	let data_schema = with_section_variants(data_schema);
	let mut root = serde_json::json!({
		"type": "object",
		"description": "Submit terminal or incremental child output.",
		"properties": {
			"type": {
				"oneOf": [
					{"type": "string", "minLength": 1},
					{
						"type": "array",
						"minItems": 1,
						"items": {"type": "string", "minLength": 1}
					}
				]
			},
			"result": {
				"oneOf": [
					{
						"type": "object",
						"properties": {"data": data_schema},
						"required": ["data"],
						"additionalProperties": false
					},
					{
						"type": "object",
						"properties": {"error": {"type": "string"}},
						"required": ["error"],
						"additionalProperties": false
					},
					{
						"type": "object",
						"properties": {},
						"additionalProperties": false
					}
				]
			}
		},
		"required": ["result"],
		"additionalProperties": false
	});
	if let Some(object) = root.as_object_mut() {
		object.extend(root_defs);
	}
	root
}

fn with_section_variants(schema: Value) -> Value {
	let Some(object) = schema.as_object() else {
		return schema;
	};
	if object.get("type").and_then(Value::as_str) != Some("object") {
		return schema;
	}
	let Some(properties) = object.get("properties").and_then(Value::as_object) else {
		return schema;
	};
	let mut branches = vec![schema.clone()];
	for property in properties.values() {
		if !branches.contains(property) {
			branches.push(property.clone());
		}
		if let Some(items) = property
			.as_object()
			.filter(|property| property.get("type").and_then(Value::as_str) == Some("array"))
			.and_then(|property| property.get("items"))
			&& !branches.contains(items)
		{
			branches.push(items.clone());
		}
	}
	if branches.len() == 1 {
		schema
	} else {
		serde_json::json!({"anyOf": branches})
	}
}

impl Tool for Yield {
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
			let params = match incoming.whole::<Params>().await {
				Ok(value) => value,
				Err(error) => { yield param_event(error); return; }
			};
			let incremental = matches!(&params.kind, Some(YieldType::Sections(_)));
			if let Some(YieldType::Sections(parts)) = &params.kind
				&& (parts.is_empty() || parts.iter().any(|part| part.trim().is_empty()))
			{
				yield done(Err(Fault::EmptySections));
				return;
			}
			let use_last_turn = matches!(&params.result, ResultEnvelope::LastTurn {});
			if use_last_turn
				&& (params.kind.is_none() || incremental)
			{
				yield done(Err(Fault::LastTurnWithoutTerminalType));
				return;
			}
			let validation = match validate_terminal(
				self.schema.as_ref(),
				self.mode,
				incremental,
				&params.result,
			) {
				Ok(validation) => validation,
				Err(fault) => {
					yield done(Err(fault));
					return;
				},
			};
			if let Err(error) = incoming.interruptable().committed().await { yield commit_event(error); return; }
			yield done(Ok(Payload { incremental, use_last_turn, validation }));
		}
	}

	fn prompt(&self, view: Result<&Payload, &Fault>, _: &PromptCaps) -> Vec<Part> {
		vec![Part::Text {
			text: match view {
				Ok(payload) if payload.incremental => sf!("Incremental section accepted."),
				Ok(Payload { validation: Some(OutputStatus::Invalid), .. }) => {
					sf!("Result accepted with a schema warning.")
				},
				Ok(_) => sf!("Result accepted."),
				Err(fault) => Str::new(fault.to_string()),
			},
		}]
	}
}

fn validate_terminal(
	schema: Option<&Value>,
	mode: SchemaMode,
	incremental: bool,
	result: &ResultEnvelope,
) -> Result<Option<OutputStatus>, Fault> {
	let Some(schema) = schema else {
		return Ok(None);
	};
	let ResultEnvelope::Data { data } = result else {
		return Ok(None);
	};
	if incremental {
		return Ok(None);
	}
	match output_schema::validate(schema, data) {
		Ok(Ok(())) => Ok(Some(OutputStatus::Valid)),
		Ok(Err(violation)) if mode == SchemaMode::Strict => Err(violation.into()),
		Ok(Err(_)) => Ok(Some(OutputStatus::Invalid)),
		// `tool_for_schema` normalized the schema, but defects such as broken
		// local references are discovered only when traversed. Treat those as
		// unavailable in permissive mode and as a violation in strict mode by
		// using one stable root diagnostic.
		Err(_) if mode == SchemaMode::Permissive => Ok(Some(OutputStatus::Unavailable)),
		Err(_) => Err(SchemaViolation {
			pointer: Str::new_static(""),
			reason:  Str::new_static("the installed output schema is not traversable"),
		}
		.into()),
	}
}

const fn done(result: Result<Payload, Fault>) -> Ev<Update, Payload, Fault> {
	Ev::Done(ToolTerminal::Done { result, useless: false })
}
fn param_event(error: ParamError) -> Ev<Update, Payload, Fault> {
	match error {
		ParamError::Args(issue) => Ev::Args(*issue),
		ParamError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		ParamError::Protocol(message) => Ev::Args(protocol_issue(message)),
	}
}
fn commit_event(error: CommitError) -> Ev<Update, Payload, Fault> {
	match error {
		CommitError::Aborted => Ev::Aborted(Abort::InputDropped),
		CommitError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		CommitError::Protocol(message) => Ev::Args(protocol_issue(message)),
	}
}
fn protocol_issue(message: Str) -> ArgIssue {
	ArgIssue {
		path:     Vec::new(),
		expected: sf!("one committed JSON argument object"),
		kind:     ArgIssueKind::Protocol,
		example:  Some(sf!(r#"{{"result":{{"data":{{}}}}}}"#)),
		found:    Some(message),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn accepts_terminal_incremental_and_last_turn_envelopes() {
		let terminal: Params =
			serde_json::from_value(serde_json::json!({"result":{"data":{"ok":true}}})).unwrap();
		assert!(matches!(terminal.result, ResultEnvelope::Data { .. }));
		let incremental: Params =
			serde_json::from_value(serde_json::json!({"type":["findings"],"result":{"data":[1,2]}}))
				.unwrap();
		assert!(matches!(incremental.kind, Some(YieldType::Sections(_))));
		let fallback: Params =
			serde_json::from_value(serde_json::json!({"type":"result","result":{}})).unwrap();
		assert!(matches!(fallback.result, ResultEnvelope::LastTurn {}));
		assert!(serde_json::from_value::<Params>(serde_json::json!({"type":"result"})).is_err());
	}

	#[test]
	fn envelope_rejects_unknown_fields() {
		assert!(
			serde_json::from_value::<Params>(
				serde_json::json!({"result":{"data":1},"schemaOverridden":true})
			)
			.is_err()
		);
	}
	#[test]
	fn caller_schema_is_installed_in_the_wire_contract() {
		let yield_tool = tool_for_schema(
			&serde_json::json!({
				"type": "object",
				"properties": {"answer": {"type": "integer"}},
				"required": ["answer"],
				"additionalProperties": false
			}),
			SchemaMode::Strict,
		)
		.unwrap();
		assert!(matches!(yield_tool.spec().constraint, Constraint::Schema { .. }));
		assert_eq!(yield_tool.spec().rev.n, 2);
		let schema: Value = serde_json::from_slice(&yield_tool.spec().schema).unwrap();
		let data = &schema["properties"]["result"]["oneOf"][0]["properties"]["data"];
		assert_eq!(data["anyOf"][0]["properties"]["answer"]["type"], "integer");
		assert_eq!(data["anyOf"][1]["type"], "integer");
		assert_eq!(schema["required"][0], "i");
	}

	#[test]
	fn strict_rejects_and_permissive_reports_invalid_terminal_data() {
		let schema = serde_json::json!({
			"type": "object",
			"properties": {"answer": {"type": "integer"}},
			"required": ["answer"],
			"additionalProperties": false
		});
		let invalid = ResultEnvelope::Data {
			data: serde_json::json!({"answer": "not-an-integer"}),
		};
		let strict = validate_terminal(Some(&schema), SchemaMode::Strict, false, &invalid)
			.expect_err("strict validation rejects");
		assert!(matches!(strict, Fault::SchemaViolation(_)));
		let encoded = serde_json::to_string(&strict).unwrap();
		let decoded: Fault = serde_json::from_str(&encoded).unwrap();
		assert_eq!(decoded, strict);
		assert_eq!(
			validate_terminal(Some(&schema), SchemaMode::Permissive, false, &invalid).unwrap(),
			Some(OutputStatus::Invalid)
		);
		let valid = ResultEnvelope::Data { data: serde_json::json!({"answer": 7}) };
		assert_eq!(
			validate_terminal(Some(&schema), SchemaMode::Strict, false, &valid).unwrap(),
			Some(OutputStatus::Valid)
		);
	}

	#[test]
	fn local_refs_remain_valid_after_wrapping() {
		let yield_tool = tool_for_schema(
			&serde_json::json!({
				"$defs": {"answer": {"type": "integer"}},
				"$ref": "#/$defs/answer"
			}),
			SchemaMode::Strict,
		)
		.unwrap();
		let schema: Value = serde_json::from_slice(&yield_tool.spec().schema).unwrap();
		assert_eq!(schema["$defs"]["answer"]["type"], "integer");
		assert_eq!(
			schema["properties"]["result"]["oneOf"][0]["properties"]["data"]["$ref"],
			"#/$defs/answer"
		);
	}
}
