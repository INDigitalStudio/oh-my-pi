//! Typed card for `eval@1`.
//!
//! pi `eval-render.ts`: a framed code cell titled `<lang icon> <status>
//! <title> · (<duration>ms)`, the cell's stdout under an `Output` rule, and —
//! after a blank row — every `display()` value as a JSON tree
//! (`json-tree.ts` `renderJsonTreeLines`). A Python exception is not a tool
//! fault: the cell settles `Ok(Payload)` with `CellOutcome::Error` and the
//! traceback in `CellStatus::exception`, and paints as failed.

use omp_tools::eval::{CellOutcome, CellStatus, DisplayOutput, Params, Payload};
use omp_tui::{IntoComponent as _, UiContext, components::hr::truncate_to_width, dom};
use serde_json::Value;

use super::{Card, CardStatus, CardView, Component, elapsed_badge, typed_fault, typed_input};

/// Persistent Python-kernel cell card.
pub struct EvalCard;

/// pi `JSON_TREE_MAX_DEPTH_{COLLAPSED,EXPANDED}`.
const TREE_DEPTH: (usize, usize) = (2, 6);
/// pi `JSON_TREE_MAX_LINES_{COLLAPSED,EXPANDED}`.
const TREE_LINES: (usize, usize) = (6, 200);
/// pi `JSON_TREE_SCALAR_LEN_{COLLAPSED,EXPANDED}`.
const TREE_SCALAR: (usize, usize) = (60, 2000);

impl Card for EvalCard {
	fn tool(&self) -> &'static str {
		"eval"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
		let args = typed_input::<Params>(view);
		let payload = view.result::<Payload>();
		let code = args
			.as_ref()
			.and_then(|value| value.get("code"))
			.and_then(Value::as_str)
			.map(str::to_owned)
			.or_else(|| partial_string(view.args_text().unwrap_or_default(), "code"))
			.or_else(|| payload.as_ref().map(|payload| payload.code.to_string()))
			.unwrap_or_default();
		let title = args
			.as_ref()
			.and_then(|value| value.get("title"))
			.and_then(Value::as_str)
			.map(str::to_owned)
			.or_else(|| partial_string(view.args_text().unwrap_or_default(), "title"))
			.or_else(|| {
				payload
					.as_ref()
					.and_then(|payload| payload.title.as_ref().map(ToString::to_string))
			})
			.unwrap_or_default();
		let live = matches!(view.status, CardStatus::StreamingArgs | CardStatus::InProgress);
		// stdout is streamed on the `<result>` text (never retained in the
		// payload): the open stream while running, the settled text after.
		let status = payload.as_ref().map(|payload| &payload.status);
		let had_output = payload.as_ref().is_none_or(|payload| payload.had_output);
		let stdout = view
			.output
			.or_else(|| had_output.then(|| view.result_text()).flatten())
			.map(str::to_owned)
			.unwrap_or_default();
		let failed = view.status == CardStatus::Failed
			|| status.is_some_and(|status| status.outcome != CellOutcome::Complete);
		let mut output = output_preview(&stdout, expanded);
		if let Some(text) = status.and_then(exception_text) {
			if !output.is_empty() {
				output.push('\n');
			}
			output.push_str(&text);
		}
		if let Some(fault) = typed_fault::<omp_tools::eval::Fault>(view) {
			if !output.is_empty() {
				output.push('\n');
			}
			output.push_str(&fault);
		}
		let duration = status.map(|status| format!("· ({}ms)", status.duration_ms));
		let state = match (view.status, failed) {
			(CardStatus::StreamingArgs | CardStatus::InProgress, _) => "running",
			(_, true) => icon(ui, "error"),
			(_, false) => icon(ui, "done"),
		};
		let tree = payload
			.as_ref()
			.filter(|_| !failed)
			.map(|payload| display_tree(&payload.display_outputs, expanded, ui))
			.unwrap_or_default();
		dom! {
			<col>
				<box border=round pad-x=1 title_pad=3>
					<row kind=title gap=1 bold>
						<i:python/>
						if live { <spinner kind=status/> }
						<text bold>{state}</text>
						if !title.is_empty() { <text bold>{title}</text> }
						if let Some(duration) = duration { <text bold>{duration}</text> }
						if let Some(badge) = elapsed_badge(view) { {badge} }
					</row>
					if !code.is_empty() {
						<pre>{code}</pre>
					}
					if !output.is_empty() {
						<hr title="Output" title_pad=3/>
						<pre>{output}</pre>
					}
				</box>
				if !tree.is_empty() {
					<spacer h=1/>
					<col>
						for line in tree { <text>{line}</text> }
					</col>
				}
			</col>
		}
		.into_component()
	}
}

fn icon<'a>(ui: &'a UiContext, name: &'a str) -> &'a str {
	ui.charset.icon_named(name).unwrap_or(name)
}

fn output_preview(output: &str, expanded: bool) -> String {
	let output = output.trim_end();
	if expanded {
		return output.to_owned();
	}
	let lines = output.lines().collect::<Vec<_>>();
	let skipped = lines.len().saturating_sub(20);
	let tail = lines
		.into_iter()
		.skip(skipped)
		.collect::<Vec<_>>()
		.join("\n");
	if skipped == 0 {
		tail
	} else {
		format!("… ({skipped} earlier lines)\n{tail}")
	}
}

/// The traceback the eval resource retained for a raised exception, in
/// Python order; `Name: message` when the resource kept no frames.
fn exception_text(status: &CellStatus) -> Option<String> {
	let exception = status.exception.as_ref()?;
	Some(if exception.traceback.is_empty() {
		format!("{}: {}", exception.name, exception.message)
	} else {
		exception
			.traceback
			.iter()
			.map(|line| line.trim_end())
			.collect::<Vec<_>>()
			.join("\n")
	})
}

/// pi `eval-render.ts` `jsonLines`: every `display()` JSON value as a tree,
/// labelled `display[N]` when there is more than one.
fn display_tree(outputs: &[DisplayOutput], expanded: bool, ui: &UiContext) -> Vec<String> {
	let values = outputs
		.iter()
		.filter_map(|output| match output {
			DisplayOutput::Json { data } => Some(data),
			_ => None,
		})
		.collect::<Vec<_>>();
	let labelled = values.len() > 1;
	let mut lines = Vec::new();
	for (index, value) in values.into_iter().enumerate() {
		if labelled {
			lines.push(format!("display[{}]", index + 1));
		}
		let mut tree = JsonTree::new(expanded, ui);
		tree.render_root(value);
		if tree.truncated {
			tree.lines.push("…".to_owned());
		}
		lines.extend(tree.lines);
	}
	lines
}

/// pi `json-tree.ts` `renderJsonTreeLines`.
struct JsonTree<'a> {
	lines:      Vec<String>,
	truncated:  bool,
	max_depth:  usize,
	max_lines:  usize,
	max_scalar: usize,
	ui:         &'a UiContext,
}

impl<'a> JsonTree<'a> {
	fn new(expanded: bool, ui: &'a UiContext) -> Self {
		let pick = |pair: (usize, usize)| if expanded { pair.1 } else { pair.0 };
		Self {
			lines: Vec::new(),
			truncated: false,
			max_depth: pick(TREE_DEPTH),
			max_lines: pick(TREE_LINES),
			max_scalar: pick(TREE_SCALAR),
			ui,
		}
	}

	fn push(&mut self, line: String) -> bool {
		if self.lines.len() >= self.max_lines {
			self.truncated = true;
			return false;
		}
		self.lines.push(line);
		true
	}

	fn render_root(&mut self, value: &Value) {
		match value {
			Value::Object(map) => {
				let keys = map
					.keys()
					.filter(|key| key.as_str() != "i")
					.collect::<Vec<_>>();
				for key in keys {
					self.render_node(&map[key], Some(key.as_str()), &mut Vec::new(), true, 1);
					if self.lines.len() >= self.max_lines {
						self.truncated = true;
						break;
					}
				}
			},
			Value::Array(items) => {
				for (index, item) in items.iter().enumerate() {
					self.render_node(
						item,
						Some(&format!("[{index}]")),
						&mut Vec::new(),
						index + 1 == items.len(),
						1,
					);
					if self.lines.len() >= self.max_lines {
						self.truncated = true;
						break;
					}
				}
			},
			_ => self.render_node(value, None, &mut Vec::new(), true, 0),
		}
	}

	fn prefix(&self, ancestors: &[bool]) -> String {
		let vertical = icon(self.ui, "tree-vertical");
		ancestors
			.iter()
			.map(|has_next| {
				if *has_next {
					format!("{vertical}  ")
				} else {
					"   ".to_owned()
				}
			})
			.collect()
	}

	fn render_node(
		&mut self,
		value: &Value,
		key: Option<&str>,
		ancestors: &mut Vec<bool>,
		is_last: bool,
		depth: usize,
	) {
		if self.lines.len() >= self.max_lines {
			self.truncated = true;
			return;
		}
		let connector = icon(self.ui, if is_last { "tree-last" } else { "tree-branch" });
		let prefix = format!("{}{connector} ", self.prefix(ancestors));
		ancestors.push(!is_last);
		match value {
			Value::Array(items) => {
				let header = key.unwrap_or("array");
				self.push(format!("{prefix}{} {header}", icon(self.ui, "package")));
				if items.is_empty() {
					self.push(format!("{}{} []", self.prefix(ancestors), icon(self.ui, "tree-last")));
				} else if depth >= self.max_depth {
					self.push(format!("{}{} …", self.prefix(ancestors), icon(self.ui, "tree-last")));
				} else {
					for (index, item) in items.iter().enumerate() {
						self.render_node(
							item,
							Some(&format!("[{index}]")),
							ancestors,
							index + 1 == items.len(),
							depth + 1,
						);
						if self.lines.len() >= self.max_lines {
							self.truncated = true;
							break;
						}
					}
				}
			},
			Value::Object(map) => {
				let header = key.unwrap_or("object");
				self.push(format!("{prefix}{} {header}", icon(self.ui, "folder")));
				if depth >= self.max_depth {
					self.push(format!("{}{} …", self.prefix(ancestors), icon(self.ui, "tree-last")));
				} else if map.is_empty() {
					self.push(format!("{}{} {{}}", self.prefix(ancestors), icon(self.ui, "tree-last")));
				} else {
					let count = map.len();
					for (index, (child_key, child)) in map.iter().enumerate() {
						self.render_node(
							child,
							Some(child_key),
							ancestors,
							index + 1 == count,
							depth + 1,
						);
						if self.lines.len() >= self.max_lines {
							self.truncated = true;
							break;
						}
					}
				}
			},
			_ => {
				let label = key.unwrap_or("value");
				let scalar_icon = icon(self.ui, "file");
				match value.as_str().filter(|text| text.contains('\n')) {
					Some(text) => {
						let rows = text.split('\n').collect::<Vec<_>>();
						let budget = self.max_lines.saturating_sub(self.lines.len() + 1).max(1);
						let shown = rows.len().min(budget);
						let continue_prefix = self.prefix(ancestors);
						self.push(format!(
							"{prefix}{scalar_icon} {label}: \"{}",
							clip(rows[0], self.max_scalar)
						));
						for row in rows.iter().take(shown).skip(1) {
							if !self.push(format!("{continue_prefix}    {}", clip(row, self.max_scalar))) {
								break;
							}
						}
						if rows.len() > shown {
							self.truncated = true;
							self.push(format!(
								"{continue_prefix}    …({} more lines)\"",
								rows.len() - shown
							));
						} else if let Some(last) = self.lines.last_mut() {
							last.push('"');
						}
					},
					None => {
						self.push(format!(
							"{prefix}{scalar_icon} {label}: {}",
							format_scalar(value, self.max_scalar)
						));
					},
				}
			},
		}
		ancestors.pop();
	}
}

/// pi `json-tree.ts` `formatScalar`.
fn format_scalar(value: &Value, max_len: usize) -> String {
	match value {
		Value::Null => "null".to_owned(),
		Value::Bool(flag) => flag.to_string(),
		Value::Number(number) => number.to_string(),
		Value::String(text) => {
			format!("\"{}\"", clip(&text.replace('\n', "\\n").replace('\t', "\\t"), max_len))
		},
		Value::Array(items) => format!("[{} items]", items.len()),
		Value::Object(map) => format!("{{{} keys}}", map.len()),
	}
}

/// pi `truncateToWidth` on a scalar: the first `max` columns with an ellipsis.
fn clip(text: &str, max: usize) -> String {
	let clipped = truncate_to_width(text, u16::try_from(max).unwrap_or(u16::MAX));
	if clipped.ellipsis {
		format!("{}…", clipped.text)
	} else {
		clipped.text.to_owned()
	}
}

fn partial_string(raw: &str, key: &str) -> Option<String> {
	let start = raw.find(&format!("\"{key}\""))?;
	let value = raw[start..].find(':')? + start + 1;
	let quote = raw[value..].find('"')? + value + 1;
	let bytes = raw.as_bytes();
	let mut escaped = false;
	for index in quote..bytes.len() {
		match (bytes[index], escaped) {
			(b'"', false) => return serde_json::from_str(&raw[quote - 1..=index]).ok(),
			(b'\\', false) => escaped = true,
			_ => escaped = false,
		}
	}
	Some(raw[quote..].replace("\\n", "\n").replace("\\\"", "\""))
}
