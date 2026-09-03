//! Typed cards for checkpointing, structured yield, memory maintenance, skills,
//! and media.

use omp_core::{Str, sf};
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
	Card, CardStatus, CardView, Component, elapsed_badge, result_image, typed_fault, typed_input,
	typed_result,
};

/// Durable checkpoint creation card.
pub struct CheckpointCard;
/// Scheduled rewind card.
pub struct RewindCard;
/// Structured subagent-yield card.
pub struct YieldCard;
/// Scoped memory mutation card.
pub struct MemoryEditCard;
/// Durable lesson card.
pub struct LearnCard;
/// Managed-skill mutation card.
pub struct ManageSkillCard;
/// Image-generation card.
pub struct ImageGenCard;
/// Speech-generation card.
pub struct TtsCard;
/// Security analysis card.
pub struct SecurityScanCard;

impl Card for CheckpointCard {
	fn tool(&self) -> &'static str {
		"checkpoint"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, _ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::checkpoint::CheckpointParams>(view);
		let result = typed_result::<omp_tools::checkpoint::CheckpointPayload>(view);
		let goal = result
			.as_ref()
			.and_then(|value| value.get("goal"))
			.and_then(Value::as_str)
			.or_else(|| args.as_ref()?.get("goal")?.as_str())
			.unwrap_or_default();
		let token = result
			.as_ref()
			.and_then(|value| value.get("token"))
			.and_then(Value::as_str);
		semantic_row("checkpoint", "Checkpoint", goal, token, view)
	}
}

impl Card for RewindCard {
	fn tool(&self) -> &'static str {
		"rewind"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, _ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::checkpoint::RewindParams>(view);
		let result = typed_result::<omp_tools::checkpoint::RewindPayload>(view);
		let report = result
			.as_ref()
			.and_then(|value| value.get("report"))
			.and_then(Value::as_str)
			.or_else(|| args.as_ref()?.get("report")?.as_str())
			.unwrap_or_default();
		let receipt = result
			.as_ref()
			.and_then(|value| value.get("receipt"))
			.and_then(Value::as_str);
		semantic_row("rewind", "Rewind", report, receipt, view)
	}
}

impl Card for YieldCard {
	fn tool(&self) -> &'static str {
		"yield"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, _ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::yield_tool::Params>(view);
		let result = typed_result::<omp_tools::yield_tool::Payload>(view);
		let incremental = result
			.as_ref()
			.and_then(|value| value.get("incremental"))
			.and_then(Value::as_bool)
			.unwrap_or(false);
		let detail = if incremental {
			"incremental section"
		} else {
			"terminal result"
		};
		let kind = args
			.as_ref()
			.and_then(|value| value.get("type"))
			.map(compact_json)
			.unwrap_or_default();
		semantic_row(
			"output",
			"Submit result",
			detail,
			(!kind.is_empty()).then_some(kind.as_str()),
			view,
		)
	}
}

#[derive(Deserialize, Serialize)]
struct MemoryEditOutcome {
	operation: Value,
	status:    Value,
	id:        Str,
	bank:      Option<Value>,
}

impl Card for MemoryEditCard {
	fn tool(&self) -> &'static str {
		"memory_edit"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, _ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::memory_edit::Params>(view);
		let result = typed_result::<MemoryEditOutcome>(view);
		let operation = result
			.as_ref()
			.and_then(|value| value.get("operation"))
			.and_then(Value::as_str)
			.or_else(|| args.as_ref()?.get("op")?.as_str())
			.unwrap_or("edit");
		let id = result
			.as_ref()
			.and_then(|value| value.get("id"))
			.and_then(Value::as_str)
			.or_else(|| args.as_ref()?.get("id")?.as_str())
			.unwrap_or_default();
		let status = result
			.as_ref()
			.and_then(|value| value.get("status"))
			.and_then(Value::as_str);
		semantic_row("memory-tool", "Memory", &sf!("{operation} {id}"), status, view)
	}
}

impl Card for LearnCard {
	fn tool(&self) -> &'static str {
		"learn"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, _ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::learn::Params>(view);
		let result = typed_result::<omp_tools::learn::LearnOutcome>(view);
		let memory = args
			.as_ref()
			.and_then(|value| value.get("memory"))
			.and_then(Value::as_str)
			.unwrap_or_default();
		let id = result
			.as_ref()
			.and_then(|value| value.get("memory_id"))
			.and_then(Value::as_str);
		let body = if expanded {
			memory
		} else {
			memory.lines().next().unwrap_or_default()
		};
		semantic_row("memory-tool", "Learn", body, id, view)
	}
}

impl Card for ManageSkillCard {
	fn tool(&self) -> &'static str {
		"manage_skill"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, _ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::manage_skill::Params>(view);
		let result = typed_result::<omp_tools::manage_skill::MutationOutcome>(view);
		let action = result
			.as_ref()
			.and_then(|value| value.get("action"))
			.and_then(Value::as_str)
			.or_else(|| args.as_ref()?.get("action")?.as_str())
			.unwrap_or("manage");
		let name = result
			.as_ref()
			.and_then(|value| value.get("name"))
			.and_then(Value::as_str)
			.or_else(|| args.as_ref()?.get("name")?.as_str())
			.unwrap_or_default();
		let path = result
			.as_ref()
			.and_then(|value| value.get("path"))
			.and_then(Value::as_str);
		semantic_row("skill", "Skill", &sf!("{action} {name}"), path, view)
	}
}

#[derive(Deserialize, Serialize)]
struct MediaParams {
	prompt:      Option<Value>,
	text:        Option<Str>,
	provider:    Option<Str>,
	output_path: Option<Str>,
}

#[derive(Deserialize, Serialize)]
struct MediaPayload {
	artifact_id: Str,
	media_type:  Str,
	output_path: Option<Str>,
}

#[derive(Deserialize, Serialize)]
struct MediaFault {
	code:    Str,
	backend: Str,
	message: Str,
}

impl Card for ImageGenCard {
	fn tool(&self) -> &'static str {
		"image_gen"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
		render_media(view, expanded, ui, false)
	}
}

impl Card for TtsCard {
	fn tool(&self) -> &'static str {
		"tts"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
		render_media(view, expanded, ui, true)
	}
}

impl Card for SecurityScanCard {
	fn tool(&self) -> &'static str {
		"security_scan"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, _ui: &UiContext) -> Component {
		let result = view.outcome_json();
		let summary = result
			.as_ref()
			.and_then(|value| value.get("summary").or_else(|| value.get("message")))
			.and_then(Value::as_str)
			.unwrap_or("repository security analysis");
		let detail = expanded
			.then(|| result.as_ref().map(compact_json))
			.flatten();
		let fault = view.diag.and_then(|node| node.content.clone());
		dom! {
			<col>
				<row gap=1>
					match view.status {
						CardStatus::Failed => <i:error/>,
						CardStatus::Done => <i:success/>,
						CardStatus::StreamingArgs | CardStatus::InProgress => <spinner kind=status/>,
					}
					<text bold>{"Security scan"}</text><text>{summary}</text>
					if let Some(badge) = elapsed_badge(view) { {badge} }
				</row>
				if let Some(detail) = detail { <pre pad-x=2>{detail}</pre> }
				if let Some(fault) = fault { <text pad-x=2 fg=err>{fault}</text> }
			</col>
		}
		.into_component()
	}
}

fn render_media(view: &CardView<'_>, expanded: bool, ui: &UiContext, speech: bool) -> Component {
	let args = typed_input::<MediaParams>(view);
	let result = typed_result::<MediaPayload>(view);
	let fault = typed_fault::<MediaFault>(view);
	let label = if speech { "Speech" } else { "Image" };
	let prompt = args
		.as_ref()
		.and_then(|value| value.get("prompt"))
		.map(compact_json);
	let description = if speech {
		args
			.as_ref()
			.and_then(|value| value.get("text"))
			.and_then(Value::as_str)
	} else {
		prompt.as_deref()
	};
	let artifact = result
		.as_ref()
		.and_then(|value| value.get("artifact_id"))
		.and_then(Value::as_str)
		.map(Str::new);
	let mime = result
		.as_ref()
		.and_then(|value| value.get("media_type"))
		.and_then(Value::as_str)
		.unwrap_or(if speech { "audio/*" } else { "image/*" });
	let output_path = result
		.as_ref()
		.and_then(|value| value.get("output_path"))
		.and_then(Value::as_str)
		.map(Str::new);
	let image = (!speech)
		.then(|| {
			artifact
				.as_ref()
				.map(|src| result_image(src, mime, output_path.as_deref(), ui))
		})
		.flatten();
	dom! {
		<col>
			<row gap=1>
				match view.status {
					CardStatus::Failed => <i:error/>,
					CardStatus::Done => <i:success/>,
					CardStatus::StreamingArgs | CardStatus::InProgress => <spinner kind=status/>,
				}
				<text bold>{label}</text>
				if let Some(path) = output_path.clone() { <text>{path}</text> }
				if let Some(badge) = elapsed_badge(view) { {badge} }
			</row>
			if let Some(description) = description { <text pad-x=2>{description}</text> }
			if let Some(image) = image { {image} }
			if speech && expanded { if let Some(artifact) = artifact { <text pad-x=2>{artifact}</text> } }
			if let Some(fault) = fault { <text pad-x=2 fg=err>{fault}</text> }
		</col>
	}
	.into_component()
}

fn semantic_row(
	icon: &'static str,
	title: &'static str,
	detail: &str,
	receipt: Option<&str>,
	view: &CardView<'_>,
) -> Component {
	let fault = view.diag.and_then(|node| node.content.clone());
	dom! {
		<col>
			<row gap=1>
				match view.status {
					CardStatus::Failed => <i:error/>,
					CardStatus::Done => <icon name={icon}/>,
					CardStatus::StreamingArgs | CardStatus::InProgress => <spinner kind=status/>,
				}
				<text bold>{title}</text><text>{Str::new(detail)}</text>
				if let Some(receipt) = receipt { <text fg=muted>{Str::new(receipt)}</text> }
				if let Some(badge) = elapsed_badge(view) { {badge} }
			</row>
			if let Some(fault) = fault { <text pad-x=2 fg=err>{fault}</text> }
		</col>
	}
	.into_component()
}

fn compact_json(value: &Value) -> String {
	match value {
		Value::String(text) => text.clone(),
		other => serde_json::to_string(other).unwrap_or_default(),
	}
}
