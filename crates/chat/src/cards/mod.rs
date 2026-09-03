//! Typed tool-card registry over materialized tool element state.

pub mod apply_patch;
pub mod ask;
pub mod ast_edit;
pub mod ast_grep;
pub mod bash;
pub mod browser;
pub mod computer;
pub mod context_gauge;
pub mod debug;
pub mod edit;
pub mod eval;
pub(crate) mod fixtures;
mod generic;
pub mod github;
pub mod glob;
pub mod goal;
pub mod grep;
pub mod hub;
pub mod inspect_image;
pub mod lsp;
pub mod memory;
pub mod read;
pub mod report_issue;
pub mod resolve;
pub mod task;
pub mod think;
pub mod todo;
pub mod utility;
pub mod vibe;
pub mod web_search;
pub mod write;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

pub use generic::GenericCard;
use omp_core::{Str, sf};
use omp_dom::{Node, PropId};
use omp_tool::{ArgPath, CallOutcome};
use omp_tui::{Graphics, IntoComponent as _, UiContext, dom};
use serde::de::DeserializeOwned;

/// A boxed retained TUI component.
pub type Component = Box<dyn omp_tui::Component>;

/// Inline tool-result image column cap (pi `tui.maxInlineImageColumns`
/// default); the layout further bounds it by the card's width.
pub(crate) const INLINE_IMAGE_MAX_COLS: u16 = 100;
/// Inline tool-result image row cap (pi `tui.maxInlineImageRows` default,
/// the explicit bound pi takes when it is tighter than 60% of the
/// viewport).
pub(crate) const INLINE_IMAGE_MAX_ROWS: u16 = 20;

/// Whether the terminal renders real inline images (pi
/// `TERMINAL.imageProtocol`): Kitty, Sixel, or iTerm2 graphics.
pub(crate) const fn inline_images(ui: &UiContext) -> bool {
	!matches!(ui.graphics, Graphics::Cells)
}

/// pi `imageFallback`: `[Image: <name> [<mime>] <WxH>]`, the text stand-in
/// for a result image the terminal cannot draw.
pub(crate) fn image_placeholder(
	mime: &str,
	dimensions: Option<(u32, u32)>,
	filename: Option<&str>,
) -> Str {
	let mut text = String::from("[Image:");
	if let Some(name) = filename.filter(|name| !name.is_empty()) {
		text.push(' ');
		text.push_str(name);
	}
	text.push_str(" [");
	text.push_str(mime);
	text.push(']');
	if let Some((width, height)) = dimensions {
		text.push_str(&sf!(" {width}x{height}"));
	}
	text.push(']');
	Str::new(text)
}

/// A tool-result image (pi `tool-execution.ts` image blocks): the image
/// itself through `<img>` when the terminal supports a graphics protocol,
/// else pi's text placeholder in the tool-output color.
pub(crate) fn result_image(
	src: &Str,
	mime: &str,
	filename: Option<&str>,
	ui: &UiContext,
) -> Component {
	if inline_images(ui) {
		dom! { <img src={src.clone()} w={INLINE_IMAGE_MAX_COLS} max-rows={INLINE_IMAGE_MAX_ROWS}/> }
			.into_component()
	} else {
		dom! { <text fg=muted>{image_placeholder(mime, None, filename)}</text> }.into_component()
	}
}

/// Tool lifecycle state derived from the tool element's `status` property.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardStatus {
	/// The provider is still streaming tool arguments.
	StreamingArgs,
	/// The tool is executing.
	InProgress,
	/// The tool settled successfully.
	Done,
	/// The tool faulted or was aborted.
	Failed,
}

impl CardStatus {
	/// Derives a card status from the session-DOM lifecycle spelling.
	#[must_use]
	pub fn from_dom(status: &str) -> Self {
		match status.as_bytes() {
			b"arguments" => Self::StreamingArgs,
			b"ok" => Self::Done,
			b"error" | b"cancelled" | b"aborted" => Self::Failed,
			_ => Self::InProgress,
		}
	}

	/// Returns the canonical session-DOM spelling.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::StreamingArgs => "arguments",
			Self::InProgress => "running",
			Self::Done => "ok",
			Self::Failed => "error",
		}
	}
}

/// Borrowed state of one tool element and its standard child elements.
pub struct CardView<'a> {
	/// Tool input state.
	pub input:   &'a Node,
	/// Successful result state, when present.
	pub result:  Option<&'a Node>,
	/// Diagnostic state, when present.
	pub diag:    Option<&'a Node>,
	/// Usage state, when present.
	pub usage:   Option<&'a Node>,
	/// Tool lifecycle status.
	pub status:  CardStatus,
	/// Accumulated ordered output of a running call: the open stream the
	/// dispatcher binds to the `<result>` text (ADR 0008 tool output
	/// streaming; `Dom::stream_text`). `None` once the stream closes and the
	/// settled result materializes into `result`.
	pub output:  Option<&'a str>,
	/// Presentation-clock instant the observer first saw the call executing
	/// (pi `executionStartedAtNow`); `None` while streaming arguments or once
	/// settled. Cards paint a live elapsed badge against
	/// [`omp_tui::PaintCtx::now`] from it.
	pub started: Option<Duration>,
}

impl CardView<'_> {
	/// Returns the streamed or committed argument text.
	#[must_use]
	pub fn args_text(&self) -> Option<&str> {
		node_text(self.input)
	}

	/// Deserializes the streamed or committed arguments into the tool's
	/// canonical parameter type.
	#[must_use]
	pub fn input<P: DeserializeOwned>(&self) -> Option<P> {
		let raw = node_data(self.input).or_else(|| self.args_text())?;
		let mut value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
		value.as_object_mut()?.remove("i");
		serde_json::from_value(value).ok()
	}

	/// Parses the streamed or committed arguments as JSON.
	#[must_use]
	pub fn args_json(&self) -> Option<serde_json::Value> {
		serde_json::from_str(self.args_text()?).ok()
	}

	/// Returns the successful result's model-facing text.
	#[must_use]
	pub fn result_text(&self) -> Option<&str> {
		self.result.and_then(node_text)
	}

	/// Deserializes the successful result into the tool's canonical payload
	/// type: the journaled `CallOutcome::Ok` truth (ADR 0008: the element
	/// carries the payload). The bounded projection (`data` / text — the
	/// once-bounded prompt parts, ADR 0009) is only consulted when the
	/// element carries no typed outcome (a foreign or extension tool whose
	/// result is its projection), never as an override of one.
	#[must_use]
	pub fn result<T: DeserializeOwned>(&self) -> Option<T> {
		let node = self.result?;
		match node_outcome(node, PropId::Outcome) {
			Some(raw) => outcome_value::<T>(raw, "ok"),
			None => parse_either::<T>(node),
		}
	}

	/// The successful result's raw journaled payload as untyped JSON.
	///
	/// Dedicated cards should prefer [`Self::result`] with their concrete
	/// payload. This seam is for extension and dynamic-device cards whose
	/// payload type is not linked into `omp-chat`.
	#[must_use]
	pub fn outcome_json(&self) -> Option<serde_json::Value> {
		let node = self.result?;
		outcome_value(node_outcome(node, PropId::Outcome)?, "ok")
	}

	/// The successful result's model-facing projection parsed as JSON: the
	/// bounded text when it is JSON, else the settled payload. Wrapper
	/// tools whose payload embeds the JSON their cards read (`hub`
	/// `Response::text`) decode the typed payload and unwrap it themselves;
	/// this is the untyped fallback for tools without a card contract.
	#[must_use]
	pub fn result_json(&self) -> Option<serde_json::Value> {
		let node = self.result?;
		self
			.result_text()
			.and_then(|text| serde_json::from_str(text).ok())
			.or_else(|| outcome_value(node_outcome(node, PropId::Outcome)?, "ok"))
	}

	/// Deserializes the terminal diagnostic into the tool's canonical fault
	/// type: the settled `CallOutcome::Faulted` truth, else a bare fault in
	/// `data` or the text for elements without a journaled outcome.
	#[must_use]
	pub fn fault<F: DeserializeOwned>(&self) -> Option<F> {
		let node = self.diag?;
		match node_outcome(node, PropId::Fault) {
			Some(raw) => outcome_value::<F>(raw, "faulted"),
			None => parse_either::<F>(node),
		}
	}
}

/// Live elapsed badge for a running call: pi's dim ` Ns` after a muted ` · `
/// (tool-execution.ts `#renderCompact`), counting whole seconds from
/// [`CardView::started`] on the shared clock. Absent unless the call is
/// executing and the projection recorded when it started, so gallery and
/// settled cards paint no badge.
pub(crate) fn elapsed_badge(view: &CardView<'_>) -> Option<Component> {
	if view.status != CardStatus::InProgress {
		return None;
	}
	let since = u64::try_from(view.started?.as_millis()).unwrap_or(u64::MAX);
	Some(
		dom! { <row gap=1><text fg=muted>{"·"}</text><time kind=elapsed dim ms={since}/></row> }
			.into_component(),
	)
}

/// pi `previewLines` / `renderCodeCell` `*MaxLines`: the first `limit`
/// lines and a `… N more lines` marker when more follow.
pub(crate) fn preview_lines(text: &str, limit: usize) -> Str {
	let total = text.lines().count();
	if total <= limit {
		return Str::new(text);
	}
	let mut out = text.lines().take(limit).collect::<Vec<_>>().join("\n");
	out.push_str(&sf!("\n… {} more lines", total - limit));
	Str::new(out)
}

pub(crate) fn typed_input<P>(view: &CardView<'_>) -> Option<serde_json::Value>
where
	P: DeserializeOwned + serde::Serialize,
{
	view
		.input::<P>()
		.and_then(|value| serde_json::to_value(value).ok())
		.or_else(|| view.args_json())
}

/// The typed payload re-encoded as JSON for cards that read it by field.
///
/// This intentionally never falls back to projection JSON. Typed cards consume
/// the journaled outcome; wrapper cards that deliberately consume a textual
/// projection call [`CardView::result_json`] themselves.
pub(crate) fn typed_result<T>(view: &CardView<'_>) -> Option<serde_json::Value>
where
	T: DeserializeOwned + serde::Serialize,
{
	view
		.result::<T>()
		.and_then(|value| serde_json::to_value(value).ok())
}

/// Parses `data`, then the text, independently: live `data` is the
/// prompt-part array, which is never a payload, while the text may be one.
fn parse_either<T: DeserializeOwned>(node: &Node) -> Option<T> {
	node_data(node)
		.and_then(|raw| serde_json::from_str(raw).ok())
		.or_else(|| node_text(node).and_then(|raw| serde_json::from_str(raw).ok()))
}

/// Human-readable text for a failed call: the tool fault's `message` (else
/// its JSON), or the harness-owned prose for an abort / rejected argument
/// (`Abort::render`), read from the journaled `CallOutcome` envelope.
pub(crate) fn typed_fault<F>(view: &CardView<'_>) -> Option<Str>
where
	F: DeserializeOwned + serde::Serialize,
{
	if let Some(raw) = view.diag.and_then(|node| node_outcome(node, PropId::Fault)) {
		if let Ok(outcome) = serde_json::from_str::<CallOutcome<serde_json::Value, F>>(raw) {
			return Some(match outcome {
				// A tool fault: its `message`, else the fold's bounded human
				// text (the prompt-parts projection), never the raw fault JSON
				// when a bounded rendering exists.
				CallOutcome::Faulted(fault) => {
					let value = serde_json::to_value(fault).ok()?;
					match value.get("message").and_then(serde_json::Value::as_str) {
						Some(message) => Str::new(message),
						None => view
							.diag
							.and_then(node_text)
							.filter(|text| !text.is_empty())
							.map_or_else(|| fault_message(&value), Str::new),
					}
				},
				CallOutcome::Aborted { abort, .. } => abort.render(),
				CallOutcome::ArgsRejected(issue) => sf!(
					"invalid argument{}: expected {}",
					issue
						.path
						.iter()
						.map(|segment| match segment {
							ArgPath::Key(key) => format!(".{key}"),
							ArgPath::Index(index) => format!("[{index}]"),
						})
						.collect::<String>(),
					issue.expected
				),
				CallOutcome::Ok(_) => return None,
			});
		}
	}
	let value = serde_json::to_value(view.fault::<F>()?).ok()?;
	Some(fault_message(&value))
}

fn fault_message(value: &serde_json::Value) -> Str {
	let text = value
		.get("message")
		.and_then(serde_json::Value::as_str)
		.map(str::to_owned)
		.unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default());
	Str::new(text)
}

/// The journaled `CallOutcome` envelope (`{"kind":…,"value":…}`) the fold
/// stores on a settled `<result>` (`outcome`) or `<diag>` (`fault`).
fn node_outcome(node: &Node, prop: PropId) -> Option<&str> {
	match node.prop(&prop.into())? {
		omp_dom::Value::Json(value) => Some(value.get()),
		_ => None,
	}
}

/// Unwraps the `value` of a `CallOutcome` envelope whose `kind` is `kind`.
///
/// Cards apply no size limit of their own (ADR 0009: output is bounded once,
/// by dispatch, which spills over-limit outcomes to the CAS as
/// `CallOutcomeDetails` and journals the `<diag kind=truncated>` address);
/// whatever the element carries inline is what the card renders.
fn outcome_value<T: DeserializeOwned>(raw: &str, kind: &str) -> Option<T> {
	#[derive(serde::Deserialize)]
	struct Envelope<'a> {
		kind:  &'a str,
		#[serde(default)]
		value: Option<Box<serde_json::value::RawValue>>,
	}
	let envelope: Envelope<'_> = serde_json::from_str(raw).ok()?;
	if envelope.kind != kind {
		return None;
	}
	serde_json::from_str(envelope.value?.get()).ok()
}

fn node_data(node: &Node) -> Option<&str> {
	match node.prop(&PropId::Data.into())? {
		omp_dom::Value::Json(value) => Some(value.get()),
		_ => None,
	}
}

fn node_text(node: &Node) -> Option<&str> {
	node
		.prop(&PropId::Text.into())
		.and_then(omp_dom::Value::as_str)
		.filter(|text| !text.is_empty())
		.or(node.content.as_deref())
}

/// One typed renderer for a tool identity.
pub trait Card: Send + Sync {
	/// Tool name handled by this renderer.
	fn tool(&self) -> &'static str;

	/// Builds retained semantic markup for the current element state.
	fn render(&self, el: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component;
}

/// Tool-identity keyed card renderer registry with a generic fallback.
#[derive(Clone)]
pub struct CardRegistry {
	cards:    BTreeMap<&'static str, Arc<dyn Card>>,
	fallback: Arc<GenericCard>,
}

impl CardRegistry {
	/// Builds the standard registry. Tool-specific cards extend this seam.
	#[must_use]
	pub fn standard() -> Self {
		let mut registry = Self { cards: BTreeMap::new(), fallback: Arc::new(GenericCard) };
		registry.register(apply_patch::ApplyPatchCard);
		registry.register(ask::AskCard);
		registry.register(ast_edit::AstEditCard);
		registry.register(ast_grep::AstGrepCard);
		registry.register(bash::BashCard);
		registry.register(browser::BrowserCard);
		registry.register(computer::ComputerCard);
		registry.register(context_gauge::ContextGaugeCard);
		registry.register(debug::DebugCard);
		registry.register(edit::EditCard);
		registry.register(eval::EvalCard);
		registry.register(github::GithubCard);
		registry.register(glob::GlobCard);
		registry.register(goal::GoalCard);
		registry.register(grep::GrepCard);
		registry.register(hub::HubCard);
		registry.register(inspect_image::InspectImageCard);
		registry.register(lsp::LspCard);
		registry.register(memory::RecallCard);
		registry.register(memory::ReflectCard);
		registry.register(memory::RetainCard);
		registry.register(read::ReadCard);
		registry.register(report_issue::ReportIssueCard);
		registry.register(resolve::RejectCard);
		registry.register(resolve::ResolveCard);
		registry.register(task::TaskCard);
		registry.register(think::ThinkCard);
		registry.register(todo::TodoCard);
		registry.register(utility::CheckpointCard);
		registry.register(utility::ImageGenCard);
		registry.register(utility::LearnCard);
		registry.register(utility::ManageSkillCard);
		registry.register(utility::MemoryEditCard);
		registry.register(utility::RewindCard);
		registry.register(utility::SecurityScanCard);
		registry.register(utility::TtsCard);
		registry.register(utility::YieldCard);
		registry.register(vibe::VibeCard::new());
		for card in vibe::VibeCard::identities() {
			registry.register(card);
		}
		registry.register(web_search::WebSearchCard);
		registry.register(write::WriteCard);
		registry
	}

	/// Registers or replaces one typed card.
	pub fn register<C: Card + 'static>(&mut self, card: C) {
		self.cards.insert(card.tool(), Arc::new(card));
	}

	/// Returns whether a tool identity has a dedicated typed card.
	#[must_use]
	pub fn contains(&self, tool: &str) -> bool {
		self.cards.contains_key(tool)
	}

	/// Renders one tool, falling back to the generic element-state card.
	#[must_use]
	pub fn render(
		&self,
		tool: &str,
		view: &CardView<'_>,
		expanded: bool,
		ui: &UiContext,
	) -> Component {
		self.cards.get(tool).map_or_else(
			|| self.fallback.render_named(tool, view, expanded, ui),
			|card| card.render(view, expanded, ui),
		)
	}
}

impl Default for CardRegistry {
	fn default() -> Self {
		Self::standard()
	}
}
