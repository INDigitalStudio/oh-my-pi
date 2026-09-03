//! Typed card for whole-file writes.

use omp_core::{Str, sf};
use omp_dom::{Node, PropId};
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde_json::Value;

use super::{
	Card, CardStatus, CardView, Component, elapsed_badge, typed_fault, typed_input, typed_result,
};

/// Card for `write` calls.
pub struct WriteCard;

impl Card for WriteCard {
	fn tool(&self) -> &'static str {
		"write"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::write::Params>(view).unwrap_or(Value::Null);
		let path = string_at(&args, "path")
			.or_else(|| partial_string(view.args_text().unwrap_or_default(), "path"))
			.unwrap_or_default();
		let content = string_at(&args, "content").unwrap_or_default();
		match view.status {
			CardStatus::StreamingArgs => render_streaming(path, content, expanded, ui),
			CardStatus::InProgress => render_progress(view, path, content, expanded, ui),
			CardStatus::Done => render_done(view, path, content, expanded, ui),
			CardStatus::Failed => render_failed(view, path, ui),
		}
	}
}

/// Collapsed streaming previews follow the edge with a bounded tail window
/// (pi `WRITE_STREAMING_PREVIEW_LINES`); `@expanded` lifts the cap.
const STREAMING_PREVIEW_LINES: usize = 12;

/// Numbers every segment of the streamed content the way pi's
/// `formatStreamingContent` does: a trailing newline yields a numbered empty
/// row, and the gutter keeps counting past the fixture's two lines.
fn render_streaming(path: &str, content: &str, expanded: bool, ui: &UiContext) -> Component {
	let total = content.split('\n').count();
	let start = if expanded {
		0
	} else {
		total.saturating_sub(STREAMING_PREVIEW_LINES)
	};
	let mut body = String::new();
	if start > 0 {
		let noun = if start == 1 { "line" } else { "lines" };
		use std::fmt::Write as _;
		let _ = writeln!(body, "… ({start} earlier {noun})");
	}
	if !content.is_empty() {
		body.push_str(&number_segments(content.split('\n').skip(start), start + 1));
	}
	let title = sf!("Write: {} {path}", icon(ui, "typescript"));
	dom! {
		<box border=round title={title} title_pad=3>
			if !body.is_empty() { <pre pad-x=1>{body}</pre> }
			<row pad-x=1 gap=1>
				<spinner kind=status/>
				<text fg=muted>{"… (streaming)"}</text>
			</row>
		</box>
	}
	.into_component()
}

fn render_progress(
	view: &CardView<'_>,
	path: &str,
	content: &str,
	expanded: bool,
	ui: &UiContext,
) -> Component {
	let lines: Vec<&str> = content.lines().collect();
	let full = number_lines(&lines.join("\n"), 1);
	let skipped = lines.len().saturating_sub(12);
	let middle = number_lines(
		&lines
			.iter()
			.skip(skipped)
			.copied()
			.collect::<Vec<_>>()
			.join("\n"),
		skipped + 1,
	);
	let line_count = lines.len();
	let title = sf!("Write: {} {path}", icon(ui, "typescript"));
	dom! {
		<box border=round title_pad=3>
			<row kind=title gap=1 bold>
				<text bold>{title}</text>
				if let Some(badge) = elapsed_badge(view) { {badge} }
			</row>
			if expanded {
				<pre pad-x=1>{full}</pre>
				<row pad-x=2><text fg=muted>{sf!("{line_count}")}</text></row>
			} else {
				if skipped > 0 { <row pad-x=1><text fg=muted>{sf!("… ({skipped} earlier lines)")}</text></row> }
				<pre pad-x=1>{middle}</pre>
				<row pad-x=2><text fg=muted>{sf!("{line_count}")}</text></row>
			}
			<row pad-x=1><text fg=muted>{"… (streaming)"}</text></row>
		</box>
	}
	.into_component()
}

fn render_done(
	view: &CardView<'_>,
	path: &str,
	content: &str,
	expanded: bool,
	ui: &UiContext,
) -> Component {
	let _result = typed_result::<omp_tools::write::Payload>(view).unwrap_or(Value::Null);
	let lines: Vec<&str> = content.lines().collect();
	let line_count = lines.len();
	let full = number_lines(&lines.join("\n"), 1);
	let head = number_lines(&lines.iter().take(6).copied().collect::<Vec<_>>().join("\n"), 1);
	let title =
		sf!("{} Write: {} {path} · {line_count} lines", icon(ui, "write"), icon(ui, "typescript"));
	dom! {
		<box border=round title={title} title_pad=3>
			if expanded {
				<pre pad-x=1>{full}</pre>
				<row pad-x=2><text fg=muted>{sf!("{line_count}")}</text></row>
			} else {
				<pre pad-x=1>{head}</pre>
				if line_count > 6 {
					<row pad-x=1><text fg=muted>{sf!("… {} more lines ⟨Ctrl+O: Expand⟩", line_count - 6)}</text></row>
				}
			}
		</box>
	}
	.into_component()
}

fn render_failed(view: &CardView<'_>, path: &str, ui: &UiContext) -> Component {
	let fault = typed_fault::<omp_tools::write::Fault>(view)
		.or_else(|| diag_text(view.diag))
		.unwrap_or_else(|| Str::new_static("write failed"));
	let title = sf!("{} Write: {} {path}", icon(ui, "error"), icon(ui, "typescript"));
	dom! {
		<box border=round bc=err title={title} title_pad=3>
			<text pad-x=3 fg=err wrap=word>{fault}</text>
		</box>
	}
	.into_component()
}

fn number_lines(text: &str, start: usize) -> Str {
	Str::new(number_segments(text.lines(), start))
}

fn number_segments<'a>(lines: impl Iterator<Item = &'a str>, start: usize) -> String {
	let mut out = String::new();
	for (offset, line) in lines.enumerate() {
		if offset > 0 {
			out.push('\n');
		}
		use std::fmt::Write as _;
		let _ = write!(out, "{:>3} {}", start + offset, line.replace('\t', "   "));
	}
	out
}

fn icon<'a>(ui: &'a UiContext, name: &str) -> &'a str {
	ui.charset.icon_named(name).unwrap_or_default()
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
	value.get(key).and_then(Value::as_str)
}

fn partial_string<'a>(json: &'a str, key: &str) -> Option<&'a str> {
	let marker = sf!("\"{key}\":\"");
	let rest = json.get(json.find(marker.as_str())? + marker.len()..)?;
	Some(rest.split('"').next().unwrap_or(rest))
}

fn diag_text(node: Option<&Node>) -> Option<Str> {
	let raw = node.and_then(|node| {
		node.content.as_deref().or_else(|| {
			node
				.prop(&PropId::Text.into())
				.and_then(omp_dom::Value::as_str)
		})
	})?;
	let value: Value = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into()));
	value
		.as_str()
		.or_else(|| string_at(&value, "message"))
		.map(Str::new)
}
