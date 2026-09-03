//! Typed card for GitHub operations.

use omp_core::Str;
use omp_dom::{Node, PropId};
use omp_tools::github::Operation;
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde_json::Value;
use strum::EnumMessage as _;

use super::{Card, CardView, Component, elapsed_badge, typed_fault, typed_input, typed_result};

/// Renders GitHub operation summaries and results.
pub struct GithubCard;

impl Card for GithubCard {
	fn tool(&self) -> &'static str {
		"github"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::github::Params>(view).unwrap_or(Value::Null);
		let op = args
			.get("op")
			.cloned()
			.and_then(|value| serde_json::from_value::<Operation>(value).ok());
		let mut heading = String::from("GitHub");
		if let Some(title) = op.and_then(|op| op.get_message()) {
			heading.push(' ');
			heading.push_str(title);
		}
		for (index, item) in operation_meta(op, &args).iter().enumerate() {
			heading.push_str(if index == 0 { " " } else { " · " });
			heading.push_str(item);
		}
		match view.status.as_str() {
			"ok" => {
				let title = format!("{} {heading}", ui.charset.icon_named("gh").unwrap_or_default());
				let output = result_value(view)
					.and_then(|value| {
						value
							.get("result")
							.and_then(|result| {
								result
									.get("output")
									.and_then(Value::as_str)
									.or_else(|| result.as_str())
							})
							.map(str::to_owned)
					})
					.unwrap_or_default();
				dom! { <box border=round title={title} title_pad=3 pad="0 1"><pre>{output}</pre></box> }
					.into_component()
			},
			"error" => {
				let title = format!("{} {heading}", ui.charset.icon_named("error").unwrap_or_default());
				let fault = failure(view);
				dom! { <box border=round title={title} title_pad=3 pad="0 1"><text>{fault}</text></box> }.into_component()
			},
			_ => dom! {
				<row gap=1><i:pending/><text>{heading}</text>
					if let Some(badge) = elapsed_badge(view) { {badge} }
				</row>
			}
			.into_component(),
		}
	}
}

/// Heading metadata per operation (pi `buildOpMeta`): the PR identifier or
/// branch for checkout/push, the query for searches, the path for file reads,
/// the title for PR creation, then the repository.
fn operation_meta(op: Option<Operation>, args: &Value) -> Vec<String> {
	let string = |key: &str| {
		args
			.get(key)
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|value| !value.is_empty())
			.map(str::to_owned)
	};
	let mut meta = Vec::with_capacity(3);
	match op {
		Some(Operation::PrCheckout | Operation::PrPush) => {
			if let Some(id) = pr_identifier(args.get("pr")) {
				meta.push(id);
			} else if let Some(branch) = string("branch") {
				meta.push(branch);
			}
			meta.extend(string("repo"));
		},
		Some(Operation::PrCreate) => {
			meta.extend(string("title"));
			if let Some(head) = string("head") {
				meta.push(match string("base") {
					Some(base) => format!("{head} -> {base}"),
					None => head,
				});
			}
			meta.extend(string("repo"));
		},
		Some(Operation::FileRead) => {
			meta.extend(string("path"));
			meta.extend(string("repo"));
			meta.extend(string("branch"));
		},
		Some(
			Operation::SearchIssues
			| Operation::SearchPrs
			| Operation::SearchCode
			| Operation::SearchCommits,
		) => {
			meta.extend(string("query"));
			meta.extend(string("repo"));
		},
		Some(Operation::SearchRepos) => meta.extend(string("query")),
		Some(Operation::RepoView) => {
			meta.extend(string("repo"));
			meta.extend(string("branch"));
		},
		Some(Operation::RunWatch) => {
			meta.extend(string("run"));
			meta.extend(string("branch"));
		},
		None => meta.extend(string("repo")),
	}
	meta
}

/// `#N` for a number or an issue/pull URL, else the selector itself; batches
/// list up to three (pi `formatPrIdentifier`).
fn pr_identifier(pr: Option<&Value>) -> Option<String> {
	let ids = match pr? {
		Value::String(one) => vec![issue_id(one)?],
		Value::Array(many) => many
			.iter()
			.filter_map(Value::as_str)
			.filter_map(issue_id)
			.collect(),
		_ => return None,
	};
	if ids.is_empty() {
		return None;
	}
	let mut text = ids.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
	if ids.len() > 3 {
		use std::fmt::Write as _;
		let _ = write!(text, ", +{} more", ids.len() - 3);
	}
	Some(text)
}

fn issue_id(value: &str) -> Option<String> {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return None;
	}
	if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
		return Some(format!("#{trimmed}"));
	}
	for marker in ["/issues/", "/pull/"] {
		if let Some((_, tail)) = trimmed.split_once(marker) {
			let digits = tail.split(|c: char| !c.is_ascii_digit()).next().unwrap_or_default();
			if !digits.is_empty() {
				return Some(format!("#{digits}"));
			}
		}
	}
	Some(trimmed.to_owned())
}
fn result_value(view: &CardView<'_>) -> Option<Value> {
	typed_result::<omp_tools::github::Payload>(view)
}
fn failure(view: &CardView<'_>) -> Str {
	if let Some(fault) = typed_fault::<omp_tools::github::Fault>(view) {
		return fault;
	}
	let raw = view.diag.and_then(node_text).unwrap_or_default();
	serde_json::from_str::<String>(raw.as_str())
		.map(Str::new)
		.unwrap_or(raw)
}
fn node_text(node: &Node) -> Option<Str> {
	node.content.clone().or_else(|| {
		node
			.prop(&PropId::Text.into())
			.and_then(|value| value.as_str())
			.map(Str::new)
	})
}
