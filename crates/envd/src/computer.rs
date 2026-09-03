//! Admission-routed native desktop session host.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use omp_core::{ArtifactUrl, Str, sf};
use omp_desktop::{
	AxNode, AxQuery, AxSnapshotOptions, CaptureCaps, DesktopPoint, DesktopSession, Target,
};
use omp_tools::computer::{Action, ComputerHost, Fault, NativeParams, Params, Payload};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::blobs::{BlobError, BlobHost};

/// Persistent native desktop owner shared by every `computer` invocation in a
/// session-scoped Environment registry.
pub(crate) struct ComputerSessionHost {
	session: DesktopSession,
	blobs:   BlobHost,
	state:   Mutex<Map<String, Value>>,
}

impl ComputerSessionHost {
	pub(crate) fn new(blobs: BlobHost) -> Arc<Self> {
		Arc::new(Self { session: DesktopSession::new(None), blobs, state: Mutex::new(Map::new()) })
	}
}

#[async_trait]
impl ComputerHost for ComputerSessionHost {
	async fn execute(&self, params: Params) -> Result<Payload, Fault> {
		let timeout = Duration::from_secs_f64(params.timeout.unwrap_or(20.0).clamp(0.001, 300.0));
		let code = params.code.clone();
		let program = parse_program(&params.code)?;
		let (results, artifacts) = tokio::time::timeout(
			timeout,
			self.execute_program(program, params.read_only),
		)
		.await
		.map_err(|_| Fault {
			code:    sf!("desktop_timeout"),
			message: sf!("computer program exceeded its timeout"),
		})??;
		Ok(Payload { code, results, artifacts })
	}
}

impl ComputerSessionHost {
	async fn execute_program(
		&self,
		program: Vec<Statement>,
		read_only: bool,
	) -> Result<(Vec<Value>, Vec<Str>), Fault> {
		let mut results = Vec::new();
		let mut artifacts = Vec::new();
		for statement in program {
			match statement {
				Statement::Desktop { bind, params } => {
					if read_only && params.required_effects().input {
						return Err(Fault {
							code:    sf!("desktop_read_only"),
							message: sf!(
								"read_only computer programs cannot perform input or focus mutation"
							),
						});
					}
					let (result, created) = self.execute_native(params).await?;
					if let Some(name) = bind {
						self.state.lock().insert(name.to_string(), result.clone());
					}
					results.push(result);
					artifacts.extend(created);
				},
				Statement::Wait(duration) => tokio::time::sleep(duration).await,
				Statement::Assert { expression, message } => {
					if !evaluate_assertion(&expression, &self.state.lock()) {
						return Err(Fault {
							code:    sf!("desktop_assertion_failed"),
							message: message.unwrap_or_else(|| sf!("computer assertion failed")),
						});
					}
					results.push(Value::Bool(true));
				},
			}
		}
		Ok((results, artifacts))
	}

	async fn execute_native(&self, params: NativeParams) -> Result<(Value, Vec<Str>), Fault> {
		let action = params.action;
		let mut artifacts = Vec::new();
		let result = match action {
			Action::Capabilities => {
				capabilities(self.session.capabilities().await.map_err(native_fault)?)
			},
			Action::ListDisplays => Value::Array(
				self
					.session
					.list_displays()
					.await
					.map_err(native_fault)?
					.into_iter()
					.map(|display| {
						json!({
							"id": display.id,
							"name": display.name,
							"x": display.x,
							"y": display.y,
							"width": display.width,
							"height": display.height,
							"scale": display.scale,
							"pixel_x": display.pixel_x,
							"pixel_y": display.pixel_y,
							"pixel_width": display.pixel_width,
							"pixel_height": display.pixel_height,
							"primary": display.is_primary,
						})
					})
					.collect(),
			),
			Action::ListWindows => Value::Array(
				self
					.session
					.list_windows()
					.await
					.map_err(native_fault)?
					.into_iter()
					.map(|window| {
						json!({
							"id": window.id,
							"title": window.title,
							"app": window.app,
							"pid": window.pid,
							"x": window.x,
							"y": window.y,
							"width": window.width,
							"height": window.height,
							"focused": window.focused,
						})
					})
					.collect(),
			),
			Action::Capture => {
				let capture = self
					.session
					.capture(target(&params), CaptureCaps {
						max_width:  params.max_width,
						max_height: params.max_height,
					})
					.await
					.map_err(native_fault)?;
				let id = self.blobs.put(&capture.data).map_err(blob_fault)?;
				let artifact = Str::new(ArtifactUrl::from_digest(id.hash).as_str());
				artifacts.push(artifact.clone());
				json!({
					"artifact": artifact,
					"bytes": id.size,
					"width": capture.width,
					"height": capture.height,
					"source_width": capture.source_width,
					"source_height": capture.source_height,
					"target": capture.target,
					"backend": capture.backend,
					"display_server": capture.display_server,
				})
			},
			Action::Click => {
				self
					.session
					.click(target(&params), number(params.x, "x")?, number(params.y, "y")?, None)
					.await
					.map_err(native_fault)?;
				Value::Bool(true)
			},
			Action::MoveMouse => {
				self
					.session
					.move_mouse(target(&params), number(params.x, "x")?, number(params.y, "y")?, None)
					.await
					.map_err(native_fault)?;
				Value::Bool(true)
			},
			Action::Drag => {
				let points = params
					.points
					.as_ref()
					.ok_or_else(|| invalid("drag requires `points`"))?
					.iter()
					.map(|point| DesktopPoint { x: point[0], y: point[1] })
					.collect();
				self
					.session
					.drag(target(&params), points, None)
					.await
					.map_err(native_fault)?;
				Value::Bool(true)
			},
			Action::Scroll => {
				self
					.session
					.scroll(
						target(&params),
						number(params.x, "x")?,
						number(params.y, "y")?,
						number(params.dx, "dx")?,
						number(params.dy, "dy")?,
						None,
					)
					.await
					.map_err(native_fault)?;
				Value::Bool(true)
			},
			Action::Type => {
				self
					.session
					.type_text(target(&params), text(&params)?.to_owned(), None)
					.await
					.map_err(native_fault)?;
				Value::Bool(true)
			},
			Action::KeyChord => {
				let keys = text(&params)?
					.split('+')
					.map(str::trim)
					.filter(|key| !key.is_empty())
					.map(str::to_owned)
					.collect::<Vec<_>>();
				self
					.session
					.key_chord(target(&params), &keys, None)
					.await
					.map_err(native_fault)?;
				Value::Bool(true)
			},
			Action::RaiseWindow => {
				self
					.session
					.raise_window(
						required(
							params.reference.as_deref().or(params.window.as_deref()),
							"raise_window requires `reference` or `window`",
						)?
						.to_owned(),
					)
					.await
					.map_err(native_fault)?;
				Value::Bool(true)
			},
			Action::AxSnapshot => {
				let snapshot = self
					.session
					.ax_snapshot(target(&params), AxSnapshotOptions {
						max_depth: params.max_depth,
						max_nodes: params.limit,
						all:       None,
					})
					.await
					.map_err(native_fault)?;
				json!({ "text": snapshot.text, "node_count": snapshot.node_count, "truncated": snapshot.truncated })
			},
			Action::AxQuery => Value::Array(
				self
					.session
					.ax_query(target(&params), AxQuery {
						role:  params.value.as_ref().map(ToString::to_string),
						title: None,
						value: None,
						limit: params.limit,
					})
					.await
					.map_err(native_fault)?
					.into_iter()
					.map(node)
					.collect(),
			),
			Action::AxElementAt => self
				.session
				.ax_element_at(target(&params), number(params.x, "x")?, number(params.y, "y")?)
				.await
				.map_err(native_fault)?
				.map(node)
				.unwrap_or(Value::Null),
			Action::AxFocused => self
				.session
				.ax_focused()
				.await
				.map_err(native_fault)?
				.map(node)
				.unwrap_or(Value::Null),
			Action::AxNode => node(
				self
					.session
					.ax_node(
						required(params.reference.as_deref(), "ax_node requires `reference`")?.to_owned(),
					)
					.await
					.map_err(native_fault)?,
			),
			Action::AxAttributes => Value::Array(
				self
					.session
					.ax_attributes(
						required(params.reference.as_deref(), "ax_attributes requires `reference`")?
							.to_owned(),
					)
					.await
					.map_err(native_fault)?
					.into_iter()
					.map(|(name, value)| json!({ "name": name, "value": value }))
					.collect(),
			),
		};
		Ok((result, artifacts))
	}
}

enum Statement {
	Desktop { bind: Option<Str>, params: NativeParams },
	Wait(Duration),
	Assert { expression: Str, message: Option<Str> },
}

fn parse_program(code: &str) -> Result<Vec<Statement>, Fault> {
	let mut program = Vec::new();
	for statement in split_statements(code)? {
		let statement = statement
			.trim()
			.strip_prefix("return ")
			.unwrap_or(statement.trim())
			.trim();
		let (bind, statement) = parse_assignment(statement)?;
		let statement = statement.strip_prefix("await ").unwrap_or(statement).trim();
		if statement.is_empty() {
			continue;
		}
		let (callee, raw_arguments) = parse_call(statement)?;
		if callee == "assert" {
			let (expression, message) = parse_assertion(raw_arguments)?;
			program.push(Statement::Assert { expression, message });
			continue;
		}
		let arguments = parse_arguments(raw_arguments)?;
		if callee == "wait" {
			let millis = arguments
				.first()
				.and_then(Value::as_f64)
				.ok_or_else(|| invalid("wait requires a millisecond number"))?;
			if !millis.is_finite() || millis < 0.0 {
				return Err(invalid("wait requires a finite non-negative duration"));
			}
			program.push(Statement::Wait(Duration::from_secs_f64(millis / 1_000.0)));
		} else if let Some(method) = callee.strip_prefix("desktop.") {
			program.push(Statement::Desktop {
				bind,
				params: parse_desktop_call(method, &arguments)?,
			});
		} else {
			return Err(invalid(
				"computer code may call only `desktop`, `wait`, and `assert`",
			));
		}
	}
	if program.is_empty() {
		return Err(invalid("computer code must contain at least one call"));
	}
	Ok(program)
}

fn parse_assignment(statement: &str) -> Result<(Option<Str>, &str), Fault> {
	let declaration = ["const ", "let ", "var "]
		.into_iter()
		.find_map(|prefix| statement.strip_prefix(prefix));
	let Some(declaration) = declaration else {
		return Ok((None, statement));
	};
	let (name, expression) = declaration
		.split_once('=')
		.ok_or_else(|| invalid("computer variable declarations require `=`"))?;
	let name = name.trim();
	if name.is_empty()
		|| !name
			.bytes()
			.enumerate()
			.all(|(index, byte)| byte == b'_' || byte.is_ascii_alphanumeric() && (index != 0 || !byte.is_ascii_digit()))
	{
		return Err(invalid("computer variable name is invalid"));
	}
	Ok((Some(Str::new(name)), expression.trim()))
}

fn parse_assertion(arguments: &str) -> Result<(Str, Option<Str>), Fault> {
	let mut depth = 0_u32;
	let mut quote = None;
	let mut escaped = false;
	let mut separator = None;
	for (offset, character) in arguments.char_indices() {
		if let Some(active_quote) = quote {
			if escaped {
				escaped = false;
			} else if character == '\\' {
				escaped = true;
			} else if character == active_quote {
				quote = None;
			}
			continue;
		}
		match character {
			'"' | '\'' => quote = Some(character),
			'(' | '[' | '{' => depth = depth.saturating_add(1),
			')' | ']' | '}' => {
				depth = depth
					.checked_sub(1)
					.ok_or_else(|| invalid("assert has an unmatched delimiter"))?;
			},
			',' if depth == 0 => {
				separator = Some(offset);
				break;
			},
			_ => {},
		}
	}
	let (expression, message) = separator.map_or((arguments, None), |offset| {
		(&arguments[..offset], Some(arguments[offset + 1..].trim()))
	});
	let expression = expression.trim();
	if expression.is_empty() {
		return Err(invalid("assert requires a condition"));
	}
	let message = message
		.map(|message| serde_json::from_str::<Str>(message))
		.transpose()
		.map_err(|_| invalid("assert message must be a JSON string"))?;
	Ok((Str::new(expression), message))
}

fn evaluate_assertion(expression: &str, state: &Map<String, Value>) -> bool {
	for operator in [">=", "<=", "===", "!==", "==", "!=", ">", "<"] {
		if let Some((left, right)) = expression.split_once(operator) {
			let Some(left) = expression_value(left.trim(), state) else {
				return false;
			};
			let Some(right) = expression_value(right.trim(), state) else {
				return false;
			};
			return match operator {
				"==" | "===" => left == right,
				"!=" | "!==" => left != right,
				">" => compare_numbers(&left, &right, |left, right| left > right),
				"<" => compare_numbers(&left, &right, |left, right| left < right),
				">=" => compare_numbers(&left, &right, |left, right| left >= right),
				"<=" => compare_numbers(&left, &right, |left, right| left <= right),
				_ => false,
			};
		}
	}
	expression_value(expression.trim(), state).is_some_and(truthy)
}

fn expression_value(expression: &str, state: &Map<String, Value>) -> Option<Value> {
	if let Ok(literal) = serde_json::from_str(expression) {
		return Some(literal);
	}
	let mut segments = expression.split('.');
	let first = segments.next()?;
	let mut value = state.get(first)?.clone();
	for segment in segments {
		value = if segment == "length" {
			let length = match &value {
				Value::Array(values) => values.len(),
				Value::Object(values) => values.len(),
				Value::String(value) => value.chars().count(),
				_ => return None,
			};
			Value::from(u64::try_from(length).ok()?)
		} else if let Ok(index) = segment.parse::<usize>() {
			value.as_array()?.get(index)?.clone()
		} else {
			value.as_object()?.get(segment)?.clone()
		};
	}
	Some(value)
}

fn truthy(value: Value) -> bool {
	match value {
		Value::Null => false,
		Value::Bool(value) => value,
		Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
		Value::String(value) => !value.is_empty(),
		Value::Array(_) | Value::Object(_) => true,
	}
}

fn compare_numbers(left: &Value, right: &Value, compare: impl FnOnce(f64, f64) -> bool) -> bool {
	left
		.as_f64()
		.zip(right.as_f64())
		.is_some_and(|(left, right)| compare(left, right))
}

fn split_statements(code: &str) -> Result<Vec<&str>, Fault> {
	let mut statements = Vec::new();
	let mut start = 0;
	let mut depth = 0_u32;
	let mut quote = None;
	let mut escaped = false;
	for (offset, character) in code.char_indices() {
		if let Some(active_quote) = quote {
			if escaped {
				escaped = false;
			} else if character == '\\' {
				escaped = true;
			} else if character == active_quote {
				quote = None;
			}
			continue;
		}
		match character {
			'"' | '\'' => quote = Some(character),
			'(' | '[' | '{' => depth = depth.saturating_add(1),
			')' | ']' | '}' => {
				depth = depth
					.checked_sub(1)
					.ok_or_else(|| invalid("computer code has an unmatched closing delimiter"))?;
			},
			';' | '\n' if depth == 0 => {
				statements.push(&code[start..offset]);
				start = offset + character.len_utf8();
			},
			_ => {},
		}
	}
	if quote.is_some() || depth != 0 {
		return Err(invalid("computer code has an unterminated string or delimiter"));
	}
	statements.push(&code[start..]);
	Ok(statements)
}

fn parse_call(statement: &str) -> Result<(&str, &str), Fault> {
	let open = statement
		.find('(')
		.ok_or_else(|| invalid("computer statements must be function calls"))?;
	if !statement.ends_with(')') {
		return Err(invalid("computer statements must end after the function call"));
	}
	Ok((statement[..open].trim(), &statement[open + 1..statement.len() - 1]))
}

fn parse_arguments(arguments: &str) -> Result<Vec<Value>, Fault> {
	if arguments.trim().is_empty() {
		return Ok(Vec::new());
	}
	serde_json::from_str(&format!("[{arguments}]"))
		.map_err(|_| invalid("computer call arguments must be JSON-compatible JavaScript values"))
}

fn parse_desktop_call(method: &str, arguments: &[Value]) -> Result<NativeParams, Fault> {
	if method == "execute" {
		let operation = arguments
			.first()
			.cloned()
			.ok_or_else(|| invalid("desktop.execute requires an operation object"))?;
		return serde_json::from_value(operation)
			.map_err(|_| invalid("desktop.execute received an invalid operation object"));
	}
	let action = match method {
		"capabilities" => Action::Capabilities,
		"displays" => Action::ListDisplays,
		"windows" => Action::ListWindows,
		"screenshot" => Action::Capture,
		"click" => Action::Click,
		"move" => Action::MoveMouse,
		"drag" => Action::Drag,
		"scroll" => Action::Scroll,
		"type" => Action::Type,
		"press" => Action::KeyChord,
		"raise" => Action::RaiseWindow,
		"ax" => Action::AxSnapshot,
		"find" => Action::AxQuery,
		"elementAt" => Action::AxElementAt,
		"focusedElement" => Action::AxFocused,
		"ref" => Action::AxNode,
		"attributes" => Action::AxAttributes,
		_ => return Err(invalid("unknown `desktop` method")),
	};
	let mut params = NativeParams {
		action,
		read_only: false,
		window: None,
		reference: None,
		value: None,
		x: None,
		y: None,
		dx: None,
		dy: None,
		points: None,
		max_width: None,
		max_height: None,
		max_depth: None,
		limit: None,
	};
	match action {
		Action::Capabilities | Action::ListDisplays | Action::ListWindows | Action::AxFocused => {
			require_arity(arguments, 0)?;
		},
		Action::Capture | Action::AxSnapshot | Action::AxQuery => {
			require_arity_at_most(arguments, 1)?;
			if let Some(options) = arguments.first() {
				apply_options(&mut params, options)?;
			}
		},
		Action::Click | Action::MoveMouse | Action::AxElementAt => {
			require_arity_range(arguments, 2, 3)?;
			params.x = arguments.first().and_then(Value::as_f64);
			params.y = arguments.get(1).and_then(Value::as_f64);
			if params.x.is_none() || params.y.is_none() {
				return Err(invalid("desktop coordinates must be numbers"));
			}
			if let Some(options) = arguments.get(2) {
				apply_options(&mut params, options)?;
			}
		},
		Action::Drag => {
			require_arity_range(arguments, 1, 2)?;
			params.points = Some(
				serde_json::from_value(arguments[0].clone())
					.map_err(|_| invalid("desktop.drag requires an array of [x, y] points"))?,
			);
			if let Some(options) = arguments.get(1) {
				apply_options(&mut params, options)?;
			}
		},
		Action::Scroll => {
			require_arity_range(arguments, 2, 3)?;
			params.x = arguments.first().and_then(Value::as_f64);
			params.y = arguments.get(1).and_then(Value::as_f64);
			if params.x.is_none() || params.y.is_none() {
				return Err(invalid("desktop coordinates must be numbers"));
			}
			if let Some(options) = arguments.get(2) {
				apply_options(&mut params, options)?;
			}
			params.dx.get_or_insert(0.0);
			params.dy.get_or_insert(0.0);
		},
		Action::Type | Action::KeyChord => {
			require_arity_range(arguments, 1, 2)?;
			params.value = arguments
				.first()
				.and_then(Value::as_str)
				.map(Str::new);
			if params.value.is_none() {
				return Err(invalid("desktop text and key chords must be strings"));
			}
			if let Some(options) = arguments.get(1) {
				apply_options(&mut params, options)?;
			}
		},
		Action::RaiseWindow | Action::AxNode | Action::AxAttributes => {
			require_arity(arguments, 1)?;
			params.reference = arguments
				.first()
				.and_then(Value::as_str)
				.map(Str::new);
			if params.reference.is_none() {
				return Err(invalid("desktop references must be strings"));
			}
		},
	}
	Ok(params)
}

fn apply_options(params: &mut NativeParams, value: &Value) -> Result<(), Fault> {
	let options = value
		.as_object()
		.ok_or_else(|| invalid("desktop options must be an object"))?;
	params.window = string_option(options, "window")?;
	params.reference = string_option(options, "reference")?.or(params.reference.take());
	params.value = string_option(options, "role")?.or(params.value.take());
	params.dx = number_option(options, "dx")?;
	params.dy = number_option(options, "dy")?;
	params.max_width = integer_option(options, "maxWidth")?;
	params.max_height = integer_option(options, "maxHeight")?;
	params.max_depth = integer_option(options, "maxDepth")?;
	params.limit = integer_option(options, "limit")?;
	Ok(())
}

fn string_option(
	options: &serde_json::Map<String, Value>,
	name: &'static str,
) -> Result<Option<Str>, Fault> {
	options
		.get(name)
		.map(|value| {
			value
				.as_str()
				.map(Str::new)
				.ok_or_else(|| invalid("desktop string option has the wrong type"))
		})
		.transpose()
}

fn number_option(
	options: &serde_json::Map<String, Value>,
	name: &'static str,
) -> Result<Option<f64>, Fault> {
	options
		.get(name)
		.map(|value| {
			value
				.as_f64()
				.ok_or_else(|| invalid("desktop number option has the wrong type"))
		})
		.transpose()
}

fn integer_option(
	options: &serde_json::Map<String, Value>,
	name: &'static str,
) -> Result<Option<u32>, Fault> {
	options
		.get(name)
		.map(|value| {
			value
				.as_u64()
				.and_then(|value| u32::try_from(value).ok())
				.ok_or_else(|| invalid("desktop integer option has the wrong type"))
		})
		.transpose()
}

fn require_arity(arguments: &[Value], expected: usize) -> Result<(), Fault> {
	if arguments.len() == expected {
		Ok(())
	} else {
		Err(invalid("desktop method received the wrong number of arguments"))
	}
}

fn require_arity_at_most(arguments: &[Value], maximum: usize) -> Result<(), Fault> {
	if arguments.len() <= maximum {
		Ok(())
	} else {
		Err(invalid("desktop method received too many arguments"))
	}
}

fn require_arity_range(arguments: &[Value], minimum: usize, maximum: usize) -> Result<(), Fault> {
	if (minimum..=maximum).contains(&arguments.len()) {
		Ok(())
	} else {
		Err(invalid("desktop method received the wrong number of arguments"))
	}
}

fn target(params: &NativeParams) -> Target {
	params
		.window
		.as_deref()
		.map_or(Target::Desktop, Target::parse)
}

fn capabilities(value: omp_desktop::DesktopCapabilities) -> Value {
	json!({
		"backend": value.backend,
		"display_server": value.display_server,
		"capture": value.capture,
		"input": value.input,
		"ax": value.ax,
		"background_window_input": value.background_window_input,
		"delivery_modes": value.delivery_modes,
		"capture_permission": value.capture_permission,
		"input_permission": value.input_permission,
		"ax_permission": value.ax_permission,
		"display_count": value.display_count,
	})
}

fn node(value: AxNode) -> Value {
	json!({
		"ref": value.ref_,
		"role": value.role,
		"native_role": value.native_role,
		"title": value.title,
		"value": value.value,
		"description": value.description,
		"enabled": value.enabled,
		"focused": value.focused,
		"x": value.x,
		"y": value.y,
		"width": value.width,
		"height": value.height,
		"actions": value.actions,
		"child_count": value.child_count,
	})
}

fn text(params: &NativeParams) -> Result<&str, Fault> {
	required(params.value.as_deref(), "operation requires `value`")
}

fn number(value: Option<f64>, field: &'static str) -> Result<f64, Fault> {
	value.ok_or_else(|| invalid(field))
}

fn required<'a>(value: Option<&'a str>, message: &'static str) -> Result<&'a str, Fault> {
	value.ok_or_else(|| invalid(message))
}

fn invalid(message: &'static str) -> Fault {
	Fault { code: sf!("invalid_desktop_request"), message: Str::new_static(message) }
}

fn native_fault(error: omp_desktop::DesktopError) -> Fault {
	Fault { code: sf!("desktop_operation_failed"), message: Str::new(error.to_string()) }
}

fn blob_fault(error: BlobError) -> Fault {
	Fault { code: sf!("desktop_artifact_failed"), message: Str::new(error.to_string()) }
}

#[cfg(test)]
mod tests {
	use super::{Action, Statement, evaluate_assertion, parse_program};
	use serde_json::{Map, json};

	#[test]
	fn computer_program_composes_desktop_wait_and_assert() {
		let program = parse_program(
			"const windows = await desktop.windows();\n\
			 await wait(5);\n\
			 assert(windows.length > 0, \"a desktop window is required\");\n\
			 await desktop.screenshot({\"maxWidth\":1280,\"maxHeight\":896});",
		)
		.expect("parse program");
		assert_eq!(program.len(), 4);
		assert!(matches!(
			&program[0],
			Statement::Desktop { bind: Some(name), params }
				if name == "windows" && matches!(params.action, Action::ListWindows)
		));
		assert!(matches!(program[1], Statement::Wait(_)));
		assert!(matches!(program[2], Statement::Assert { .. }));
		assert!(matches!(
			&program[3],
			Statement::Desktop { params, .. }
				if matches!(params.action, Action::Capture)
					&& params.max_width == Some(1280)
					&& params.max_height == Some(896)
		));

		let mut state = Map::new();
		state.insert("windows".to_owned(), json!([{"id":"w1"}]));
		assert!(evaluate_assertion("windows.length > 0", &state));
		assert!(evaluate_assertion("windows.0.id == \"w1\"", &state));
		assert!(!evaluate_assertion("windows.length == 0", &state));
	}

	#[test]
	fn computer_program_rejects_non_surface_calls() {
		assert!(parse_program("process.exit(0)").is_err());
		assert!(parse_program("await desktop.click(\"x\", 2)").is_err());
		assert!(parse_program("assert(missing.length > 0)").is_ok());
	}
}
