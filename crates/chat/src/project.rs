//! Pure projection from an actor-owned session DOM replica to transcript
//! blocks.

use omp_core::{Str, StrMut, sf};
use omp_dom::{Dom, Handle, KnownTag, Node, PropId, Tag, Value};
use omp_journal::data::Attachment;
use omp_tui::{Charset, Icon, IntoComponent, UiContext, dom, slots::Mode};

use crate::{
	cards::{CardRegistry, CardStatus, CardView, Component},
	notices::{cache, custom, divider, error, misc, usage},
	reaction, thinking,
	transcript::{Local, REVEAL_HORIZON, StreamHead},
};

/// Semantic transcript block class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
	/// Host-owned welcome banner shown before the first turn.
	Welcome,
	/// User-authored message.
	User,
	/// Assistant reasoning, controlled by the observer-local reveal setting.
	Thinking,
	/// Visible assistant answer.
	Assistant,
	/// Tool element rendered by the card registry.
	Tool,
	/// Controller notice.
	Notice,
	/// Turn receipt.
	Usage,
	/// Maintenance divider: compaction, handoff, branch summary, or a
	/// prompt-cache miss marker.
	Divider,
}

/// Test- and status-facing description of one projected block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockView {
	/// Stable observer-local identity derived from the DOM handle and block
	/// kind.
	pub key:       u64,
	/// Semantic block class.
	pub kind:      BlockKind,
	/// Plain semantic text represented by this block.
	pub text:      Str,
	/// Slot update mode.
	pub mode:      Mode,
	/// Whether the block may retire into history.
	pub finalized: bool,
}

/// One rendered block ready for admission to the slot engine.
pub(crate) struct RenderedBlock {
	pub view:      BlockView,
	pub component: Component,
	/// Streamed text owned by the component's [`omp_tui::slots::STREAM_ID`]
	/// child. A later projection whose stream extends this one is applied in
	/// place, keeping the reveal cursor and animation phase.
	pub stream:    Option<Str>,
}

/// Observer-local switches the projection reads (never DOM state).
#[derive(Clone, Copy)]
pub struct Options<'a> {
	/// Reveal reasoning text (`cl_showthinking`).
	pub show_thinking: bool,
	/// Expand tool cards (`cl_tools_expanded`).
	pub expanded:      bool,
	/// Type streamed text out at the reveal cadence (`cl_smooth_streaming`).
	pub smooth:        bool,
	/// Collapse fenced code in reasoning to an ellipsis
	/// (`cl_thinking_prose_only`).
	pub prose_only:    bool,
	/// Tool start instants, speed gauge, and reset banner.
	pub local:         &'a Local,
}

impl<'a> Options<'a> {
	/// pi's defaults: thinking shown, cards collapsed, smooth streaming and
	/// prose-only reasoning on.
	#[must_use]
	pub const fn new(local: &'a Local) -> Self {
		Self { show_thinking: true, expanded: false, smooth: true, prose_only: true, local }
	}
}

/// Projects descriptors without constructing terminal components.
#[must_use]
pub fn block_views(dom: &Dom, show_thinking: bool) -> Vec<BlockView> {
	let local = Local::default();
	let options = Options { show_thinking, ..Options::new(&local) };
	project(dom, &CardRegistry::standard(), &UiContext::default(), &options)
		.into_iter()
		.map(|block| block.view)
		.collect()
}

pub(crate) fn project(
	dom: &Dom,
	cards: &CardRegistry,
	ui: &UiContext,
	options: &Options<'_>,
) -> Vec<RenderedBlock> {
	let mut blocks = Vec::new();
	if let Some(banner) = options.local.banner() {
		blocks.push(banner_block(banner.key, banner.text.clone()));
	}
	let turns = dom.children(dom.body());
	let cache_misses = cache::cache_invalidations(dom);
	// pi `pickReactionTarget`: the nearest preceding user bubble, looking
	// past notices and tool cards but never past an earlier reply.
	let mut reaction_target: Option<ReactionTarget> = None;
	for (index, turn) in turns.iter().enumerate() {
		let Some(turn_node) = dom.get(*turn) else {
			continue;
		};
		if turn_node.tag != Tag::Known(KnownTag::Turn) {
			continue;
		}
		let start = blocks.len();
		let last_turn = index + 1 == turns.len();
		for handle in dom.children(*turn) {
			let Some(node) = dom.get(*handle) else {
				continue;
			};
			match &node.tag {
				Tag::Known(KnownTag::User) => {
					let raw = node.content.clone().unwrap_or_default();
					// Display-only collapse before any branch: guest and synthetic
					// rows show the same chips as the plain bubble.
					let text = collapse_image_markers(&raw, ui.charset);
					let chips = attachment_chips(node, raw.as_str(), ui.charset);
					let component: Component = if crate::notices::prop_bool(node, PropId::Synthetic) {
						reaction_target = None;
						with_attachments(misc::synthetic_row(text.as_str(), options.expanded), &chips)
					} else if let Some(author) = crate::notices::prop_text(node, PropId::Author) {
						reaction_target = None;
						with_attachments(misc::guest_bubble(author.as_str(), text.clone()), &chips)
					} else {
						reaction_target = Some(ReactionTarget {
							key:   block_key(*handle, BlockKind::User),
							text:  text.clone(),
							chips: chips.clone(),
						});
						user_bubble(text, None, &chips)
					};
					blocks.push(rendered(
						*handle,
						BlockKind::User,
						raw,
						Mode::Mutable,
						true,
						component,
					));
				},
				Tag::Known(KnownTag::Assistant) => {
					assistant_blocks(dom, *handle, node, options, &mut blocks, &mut reaction_target);
				},
				Tag::Known(KnownTag::Notice) => {
					let kind = prop_text(node, PropId::Kind).unwrap_or_else(|| Str::new_static("info"));
					// pi `custom_message` / `hookMessage` entries: framed
					// Markdown boxes journaled by the kernel (`EnvEvent::Notice`).
					if let Ok(framed) = kind.as_str().parse::<custom::CustomKind>() {
						let component = custom::custom_message_card(framed, node, options.expanded);
						let text = custom::framed_text(node);
						blocks.push(rendered(
							*handle,
							BlockKind::Notice,
							text,
							Mode::Mutable,
							true,
							component,
						));
						continue;
					}
					let text = node.content.clone().unwrap_or_default();
					// ERR-06: while the identical error is pinned above the editor the
					// inline copy is suppressed; ctrl+o draws it in full anyway.
					if !options.expanded && error::suppressed_inline(dom, *handle) {
						continue;
					}
					let component = misc::custom_notice(kind.as_str(), node).unwrap_or_else(|| {
						error::notice_card(kind.as_str(), text.clone(), options.expanded)
					});
					blocks.push(rendered(
						*handle,
						BlockKind::Notice,
						text,
						Mode::Mutable,
						true,
						component,
					));
				},
				Tag::Known(KnownTag::Usage) => {
					let facts = usage::usage_facts(dom, *handle);
					let text = usage::usage_line(&facts, ui);
					blocks.push(rendered(
						*handle,
						BlockKind::Usage,
						text,
						Mode::Mutable,
						true,
						usage::usage_block(&facts, ui),
					));
					// CSH-01: the cache-miss marker trails the turn whose request
					// lost the prompt cache.
					if let Some((_, miss)) = cache_misses.iter().find(|(usage, _)| usage == handle) {
						blocks.push(rendered(
							*handle,
							BlockKind::Divider,
							Str::new(format!("cache miss · {} tokens", miss.reprocessed_tokens)),
							Mode::Mutable,
							true,
							cache::cache_miss_marker(miss),
						));
					}
				},
				Tag::Custom(tool) => {
					if let Some(block) = tool_block(dom, *handle, node, tool, cards, ui, options) {
						blocks.push(block);
					}
				},
				_ => {},
			}
		}
		displace_cards(dom, &mut blocks, start, last_turn);
		group_reads(dom, cards, ui, options, &mut blocks, start);
		// CMP-01..06: maintenance dividers land after the turn holding their
		// boundary entry.
		for (compaction, component) in divider::compaction_dividers(dom, *turn, options.expanded) {
			let label = dom
				.get(compaction)
				.map(|node| divider::SummaryDivider::compaction(node, options.expanded).label)
				.unwrap_or_default();
			blocks.push(rendered(
				compaction,
				BlockKind::Divider,
				label,
				Mode::Mutable,
				true,
				component,
			));
		}
	}
	blocks
}

/// Assistant reasoning and answer blocks (pi `AssistantMessageComponent`).
///
/// Reasoning shown: an append-only Markdown head (`<md reveal>`, pi
/// `new Markdown(text, 1, 0, …, { italic: true })`) whose stable prefix may
/// retire into scrollback mid-stream; reasoning hidden while the model is
/// still reasoning: the breathing starburst pulse with the speed badge; the
/// answer: a mutable markdown block typed out through the reveal cursor,
/// snapped to its full text once a tool call starts (a transcript order
/// boundary) or the message ends.
fn assistant_blocks(
	dom: &Dom,
	handle: Handle,
	node: &Node,
	options: &Options<'_>,
	blocks: &mut Vec<RenderedBlock>,
	reaction_target: &mut Option<ReactionTarget>,
) {
	let finalized = node.prop(&PropId::StopReason.into()).is_some();
	let raw_thinking = live_text(dom, handle, node, PropId::Thinking).unwrap_or_default();
	let full_text = live_text(dom, handle, node, PropId::Text).unwrap_or_default();
	let text = apply_reaction(&full_text, finalized, reaction_target.take(), blocks);
	let thinking = thinking::display_thinking(&raw_thinking, options.prose_only);
	let thinking = Str::new(thinking.as_str().trim());
	let has_thinking = thinking::is_displayable(raw_thinking.as_str(), thinking.as_str());
	let tool_started = has_tool_calls(dom, handle);
	let reveal = options.smooth && !finalized && !tool_started;
	if options.show_thinking && has_thinking {
		let component = if reveal {
			dom! { <md id={omp_tui::slots::STREAM_ID} reveal={REVEAL_HORIZON_PROP} fg=muted italic pad-x=1>{thinking.clone()}</md> }
		} else {
			dom! { <md id={omp_tui::slots::STREAM_ID} fg=muted italic pad-x=1>{thinking.clone()}</md> }
		};
		let mut block = rendered(
			handle,
			BlockKind::Thinking,
			thinking.clone(),
			Mode::AppendOnly,
			finalized,
			component,
		);
		block.stream = Some(thinking.clone());
		blocks.push(block);
	} else if !options.show_thinking
		&& !finalized
		&& !tool_started
		&& thinking::has_content(raw_thinking.as_str())
		&& reasoning_is_head(options.local, text.as_str())
	{
		let local = options.local;
		let pulse = omp_tui::components::Pulse::new()
			.label(" Thinking")
			.count(local.thinking_tokens())
			.gauge(local.gauge().clone(), "toks/s")
			.with(omp_tui::Prop::Fg, "secondary");
		blocks.push(rendered(
			handle,
			BlockKind::Thinking,
			Str::new_static("Thinking"),
			Mode::Mutable,
			false,
			dom! { <row pad-x=1>{pulse}</row> },
		));
	}
	if !text.is_empty() || finalized {
		let component = if reveal {
			dom! { <md id={omp_tui::slots::STREAM_ID} reveal={REVEAL_HORIZON_PROP} pad-x=1>{text.clone()}</md> }
		} else {
			dom! { <md id={omp_tui::slots::STREAM_ID} pad-x=1>{text.clone()}</md> }
		};
		let mut block =
			rendered(handle, BlockKind::Assistant, text.clone(), Mode::Mutable, finalized, component);
		block.stream = Some(text);
		blocks.push(block);
	}
}

/// The user bubble a reply may react to (pi `ReactionTarget`): its block
/// key plus the facts needed to redraw it with the badge.
struct ReactionTarget {
	key:   u64,
	text:  Str,
	chips: Vec<Str>,
}

/// pi `#displayMessage`: the reply's display text with the reaction line
/// handled — stripped and badged onto the target bubble once resolved,
/// withheld entirely while a streaming prefix could still become one, and
/// left verbatim when there is no target. A reply consumes the target
/// either way: a continuation after tool calls has nothing to react to.
fn apply_reaction(
	text: &Str,
	finalized: bool,
	target: Option<ReactionTarget>,
	blocks: &mut [RenderedBlock],
) -> Str {
	let Some(target) = target else {
		return text.clone();
	};
	let split = reaction::split_reaction(text.as_str());
	match split.emoji {
		Some(emoji) => {
			if let Some(block) = blocks.iter_mut().rev().find(|block| block.view.key == target.key) {
				block.component = user_bubble(target.text, Some(Str::new(emoji)), &target.chips);
			}
			Str::new(split.body)
		},
		None if split.pending && !finalized => Str::default(),
		None => text.clone(),
	}
}

/// pi `#shouldAnimateThinking`: the pulse shows while the model is reasoning
/// right now — the newest delta was reasoning — so a second reasoning phase
/// after visible text pulses again. An observer that has seen no delta (a
/// replica fed by patches alone) falls back to the only order the DOM
/// keeps: answer text having started means reasoning stopped.
fn reasoning_is_head(local: &Local, text: &str) -> bool {
	match local.stream_head() {
		Some(head) => head == StreamHead::Thinking,
		None => !thinking::has_content(text),
	}
}

/// `reveal` prop spelling of [`REVEAL_HORIZON`].
const REVEAL_HORIZON_PROP: &str = "264ms";
const _: () = assert!(REVEAL_HORIZON.as_millis() == 264);

/// Whether a tool element follows this assistant message in its turn (pi
/// `splitAssistantMessageToolTimeline().hasToolCalls`).
fn has_tool_calls(dom: &Dom, assistant: Handle) -> bool {
	let Some(turn) = dom.parent(assistant) else {
		return false;
	};
	let siblings = dom.children(turn);
	let Some(position) = siblings.iter().position(|handle| *handle == assistant) else {
		return false;
	};
	siblings[position + 1..]
		.iter()
		.filter_map(|handle| dom.get(*handle))
		.any(|node| matches!(node.tag, Tag::Custom(_)))
}

/// Observer-local transcript row after a session reset (pi `present([new
/// Spacer(1), new Text(success + label, 1, 1)])`).
fn banner_block(key: u64, text: Str) -> RenderedBlock {
	let component = dom! {
		<row gap=1 pad-x=1 fg=accent><icon name="success"/><text>{text.clone()}</text></row>
	};
	RenderedBlock {
		view:      BlockView {
			key,
			kind: BlockKind::Notice,
			text,
			mode: Mode::Mutable,
			finalized: true,
		},
		component: component.into_component(),
		stream:    None,
	}
}

/// pi `displaceableByToolName`: a waiting `hub` poll card is displaced by
/// the next `hub` call, and a `todo` snapshot card by the next `todo` call
/// or the next user prompt, so the transcript keeps one live copy.
fn displace_cards(dom: &Dom, blocks: &mut Vec<RenderedBlock>, start: usize, last_turn: bool) {
	let displaceable = |block: &RenderedBlock| -> Option<&'static str> {
		if block.view.kind != BlockKind::Tool {
			return None;
		}
		let handle = Handle::new(block.view.key / 8)?;
		let node = dom.get(handle)?;
		let Tag::Custom(tool) = &node.tag else {
			return None;
		};
		if matches!(
			node.prop(&PropId::Status.into()).and_then(Value::as_str),
			Some("error" | "cancelled" | "aborted")
		) {
			return None;
		}
		match tool.as_str() {
			"todo" => Some("todo"),
			"hub" if hub_is_wait(dom, handle, node) => Some("hub"),
			_ => None,
		}
	};
	let mut keep = vec![true; blocks.len()];
	let mut latest: [Option<usize>; 2] = [None, None];
	for index in start..blocks.len() {
		let Some(name) = displaceable(&blocks[index]) else {
			continue;
		};
		let slot = usize::from(name == "hub");
		if let Some(previous) = latest[slot].replace(index) {
			keep[previous] = false;
		}
	}
	// A todo snapshot is also displaced by the next user prompt: only the
	// newest turn keeps its last one.
	if !last_turn && let Some(index) = latest[0] {
		keep[index] = false;
	}
	let mut position = 0;
	blocks.retain(|_| {
		let kept = keep[position];
		position += 1;
		kept
	});
}

/// Whether a `hub` call is a waiting poll (`op` = `wait`).
fn hub_is_wait(dom: &Dom, handle: Handle, _node: &Node) -> bool {
	child(dom, handle, KnownTag::Input)
		.and_then(|input| {
			let raw = match input.prop(&PropId::Data.into()) {
				Some(Value::Json(value)) => value.get().to_owned(),
				_ => input
					.prop(&PropId::Text.into())
					.and_then(Value::as_str)?
					.to_owned(),
			};
			serde_json::from_str::<serde_json::Value>(&raw).ok()
		})
		.and_then(|args| {
			args
				.get("op")
				.and_then(serde_json::Value::as_str)
				.map(str::to_owned)
		})
		.is_some_and(|op| op == "wait")
}

/// pi `read-tool-group.ts`: consecutive `read` calls in one turn collapse
/// into one compact tree block, and when the turn contains only reads the
/// turn's usage row attaches to the group instead of standing alone.
fn group_reads(
	dom: &Dom,
	cards: &CardRegistry,
	ui: &UiContext,
	options: &Options<'_>,
	blocks: &mut Vec<RenderedBlock>,
	start: usize,
) {
	let is_read = |block: &RenderedBlock| {
		block.view.kind == BlockKind::Tool
			&& Handle::new(block.view.key / 8).is_some_and(|handle| read_is_groupable(dom, handle))
	};
	let reads_only = blocks[start..]
		.iter()
		.all(|block| is_read(block) || matches!(block.view.kind, BlockKind::User | BlockKind::Usage));
	let mut index = start;
	while index < blocks.len() {
		if !is_read(&blocks[index]) {
			index += 1;
			continue;
		}
		let mut end = index + 1;
		while end < blocks.len() && is_read(&blocks[end]) {
			end += 1;
		}
		if end - index < 2 {
			index = end;
			continue;
		}
		let handles = blocks[index..end]
			.iter()
			.filter_map(|block| Handle::new(block.view.key / 8))
			.collect::<Vec<_>>();
		let usage = if reads_only {
			blocks[end..]
				.iter()
				.position(|block| block.view.kind == BlockKind::Usage)
				.map(|offset| end + offset)
		} else {
			None
		};
		let usage_line = usage.map(|at| blocks[at].view.text.clone());
		let group = read_group_block(dom, cards, ui, options, &handles, usage_line);
		let mut text = StrMut::new("");
		for block in &blocks[index..end] {
			text.push_str(block.view.text.as_str());
			text.push_str("\n");
		}
		let finalized = blocks[index..end].iter().all(|block| block.view.finalized);
		let view = BlockView {
			key: blocks[index].view.key,
			kind: BlockKind::Tool,
			text: text.freeze(),
			mode: Mode::Mutable,
			finalized,
		};
		if let Some(at) = usage {
			blocks.remove(at);
		}
		blocks.splice(index..end, [RenderedBlock { view, component: group, stream: None }]);
		index += 1;
	}
}

/// Only ordinary local-file reads collapse into a compact group. Internal
/// resources (`artifact://`, `skill://`, `agent://`, URLs, and other schemes)
/// keep their full card because their result body is the useful surface.
fn read_is_groupable(dom: &Dom, handle: Handle) -> bool {
	let Some(node) = dom.get(handle) else {
		return false;
	};
	if !matches!(&node.tag, Tag::Custom(tool) if tool.as_str() == "read") {
		return false;
	}
	let Some(input) = child(dom, handle, KnownTag::Input) else {
		return false;
	};
	let raw = match input.prop(&PropId::Data.into()) {
		Some(Value::Json(value)) => value.get(),
		_ => input
			.prop(&PropId::Text.into())
			.and_then(Value::as_str)
			.or(input.content.as_deref())
			.unwrap_or_default(),
	};
	let Ok(args) = serde_json::from_str::<serde_json::Value>(raw) else {
		return true;
	};
	args
		.get("path")
		.and_then(serde_json::Value::as_str)
		.is_some_and(|path| !path.contains("://"))
}

fn read_group_block(
	dom: &Dom,
	cards: &CardRegistry,
	ui: &UiContext,
	options: &Options<'_>,
	handles: &[Handle],
	usage: Option<Str>,
) -> Component {
	let views = handles
		.iter()
		.filter_map(|handle| {
			let node = dom.get(*handle)?;
			card_view(dom, *handle, node, options)
		})
		.collect::<Vec<_>>();
	let _ = cards;
	crate::cards::read::render_calls_group(&views, options.expanded, usage, ui)
}

fn card_view<'a>(
	dom: &'a Dom,
	handle: Handle,
	node: &'a Node,
	options: &Options<'_>,
) -> Option<CardView<'a>> {
	let input = child(dom, handle, KnownTag::Input)?;
	let status = node
		.prop(&PropId::Status.into())
		.and_then(Value::as_str)
		.unwrap_or("running");
	let card_status = CardStatus::from_dom(status);
	let started = (card_status == CardStatus::InProgress)
		.then(|| options.local.started(block_key(handle, BlockKind::Tool)))
		.flatten();
	let result = dom.children(handle).iter().copied().find(|child| {
		dom.get(*child)
			.is_some_and(|node| node.tag == Tag::Known(KnownTag::Result))
	});
	Some(CardView {
		input,
		result: result.and_then(|handle| dom.get(handle)),
		diag: child(dom, handle, KnownTag::Diag),
		usage: child(dom, handle, KnownTag::Usage),
		status: card_status,
		output: result.and_then(|handle| dom.stream_text(handle, &PropId::Text.into())),
		started,
	})
}

/// User message: pi renders the text as Markdown on the `userMessageBg`
/// tint with one cell of padding on every side (`new Markdown(text, 1, 1,
/// …)` in `user-message.ts`: a tinted blank row above and below) and no
/// border; the chrome brackets an OSC 133 prompt zone. An agent reaction
/// replaces the top padding row with the emoji right-aligned inside the
/// horizontal padding (`#reactionRow`); journaled attachments the text does
/// not already reference add a chip row under the prose.
fn user_bubble(text: Str, reaction: Option<Str>, chips: &[Str]) -> Component {
	if reaction.is_none() && chips.is_empty() {
		return dom! { <md zone=prompt bg=surface pad="1 1">{text}</md> }.into_component();
	}
	let chips = chips.to_vec();
	dom! {
		<col zone=prompt bg=surface>
			if let Some(emoji) = reaction {
				<row h=1 justify=end pad-x=1><text>{emoji}</text></row>
			} else {
				<spacer h=1/>
			}
			<md pad-x=1>{text}</md>
			if !chips.is_empty() {
				<row h=1 gap=2 pad-x=1>
					for chip in chips { <text bold fg=accent>{chip}</text> }
				</row>
			}
			<spacer h=1/>
		</col>
	}
	.into_component()
}

/// A guest or synthetic user row followed by its attachment chip row, when
/// the journaled attachment set has entries the text does not reference.
fn with_attachments(component: Component, chips: &[Str]) -> Component {
	if chips.is_empty() {
		return component;
	}
	let chips = chips.to_vec();
	dom! {
		<col>
			{component}
			<row h=1 gap=2 pad-x=1 bg=surface>
				for chip in chips { <text bold fg=accent>{chip}</text> }
			</row>
		</col>
	}
	.into_component()
}

/// Chips for the user node's journaled attachments (`data` = the fold's
/// `Vec<Attachment>`, addressed as `attachment://N` by ordinal) that the
/// text does not already carry as a `[Image #N]` / `[Video #N]` marker or
/// an `attachment://N` reference: `<paperclip> #N · <size>`. A reference
/// knows only its digest, size, and MIME, so the chip names the ordinal the
/// model and the `read` tool use.
fn attachment_chips(node: &Node, text: &str, charset: Charset) -> Vec<Str> {
	let Some(Value::Json(raw)) = node.prop(&PropId::Data.into()) else {
		return Vec::new();
	};
	let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(raw.get()) else {
		return Vec::new();
	};
	let icon = charset.icon(Icon::Paperclip);
	attachments
		.iter()
		.enumerate()
		.filter_map(|(index, attachment)| {
			let ordinal = index + 1;
			(!text_references_attachment(text, ordinal)).then(|| {
				sf!(
					"{icon} #{ordinal} · {}",
					misc::format_bytes(usize::try_from(attachment.blob.size).unwrap_or(usize::MAX))
				)
			})
		})
		.collect()
}

/// Whether `text` already shows attachment `ordinal`: a vision marker
/// (`[Image #N`, `[Video #N`) or an `attachment://N` reference.
fn text_references_attachment(text: &str, ordinal: usize) -> bool {
	let digits = ordinal.to_string();
	let follows = |prefix: &str| {
		text.match_indices(prefix).any(|(at, _)| {
			text[at + prefix.len()..]
				.strip_prefix(digits.as_str())
				.is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
		})
	};
	follows("[Image #") || follows("[Video #") || follows("attachment://")
}

/// pi `collapseImageMarkers` (`composer-attachments.ts`, called with an
/// unbounded image count from `user-message.ts`): the stored text carries
/// bracketed `[Image #N, WxH]` / `[Video #N]` markers, optionally followed
/// by their ` attachment://N` reference, but the transcript shows the same
/// compact `<icon> #N` chip the composer used. Runs before Markdown layout
/// so wrapping and bubble padding are computed on the visible text.
fn collapse_image_markers(text: &Str, charset: Charset) -> Str {
	if !text.contains("[Image #") && !text.contains("[Video #") {
		return text.clone();
	}
	let mut out = StrMut::with_capacity(text.len());
	let mut rest = text.as_str();
	while let Some(start) = rest.find('[') {
		out.push_str(&rest[..start]);
		let candidate = &rest[start..];
		match parse_vision_marker(candidate) {
			Some((icon, ordinal, consumed)) => {
				out.push_str(charset.icon(icon));
				out.push_str(" #");
				out.push_str(ordinal);
				rest = &candidate[consumed..];
			},
			None => {
				out.push_str("[");
				rest = &candidate[1..];
			},
		}
	}
	out.push_str(rest);
	out.freeze()
}

/// Parses one leading vision marker: `[Image #N]`, `[Image #N, WxH]`, or
/// `[Video #N…]`, each optionally followed by ` attachment://N` naming the
/// same ordinal. Returns the chip icon, the ordinal digits, and the byte
/// length consumed.
fn parse_vision_marker(candidate: &str) -> Option<(Icon, &str, usize)> {
	let (icon, body) = if let Some(body) = candidate.strip_prefix("[Image #") {
		(Icon::Image, body)
	} else {
		(Icon::Video, candidate.strip_prefix("[Video #")?)
	};
	let digits = body.bytes().take_while(u8::is_ascii_digit).count();
	if digits == 0 || body.as_bytes()[0] == b'0' {
		return None;
	}
	let ordinal = &body[..digits];
	let tail = &body[digits..];
	let close = match *tail.as_bytes().first()? {
		b']' => 0,
		b',' => tail
			.find(|c: char| c == ']' || c == '\n')
			.filter(|at| tail.as_bytes()[*at] == b']')?,
		_ => return None,
	};
	let mut consumed = candidate.len() - tail.len() + close + 1;
	let reference = &candidate[consumed..];
	if let Some(after) = reference.strip_prefix(" attachment://")
		&& after
			.strip_prefix(ordinal)
			.is_some_and(|next| !next.starts_with(|c: char| c.is_ascii_digit()))
	{
		consumed += " attachment://".len() + ordinal.len();
	}
	Some((icon, ordinal, consumed))
}

fn rendered(
	handle: Handle,
	kind: BlockKind,
	text: Str,
	mode: Mode,
	finalized: bool,
	component: impl IntoComponent,
) -> RenderedBlock {
	RenderedBlock {
		view:      BlockView { key: block_key(handle, kind), kind, text, mode, finalized },
		component: component.into_component(),
		stream:    None,
	}
}

/// Stable observer-local block identity: the DOM handle times eight plus a
/// kind suffix, so the handle is recoverable as `key / 8`.
pub(crate) const fn block_key(handle: Handle, kind: BlockKind) -> u64 {
	let suffix = match kind {
		BlockKind::Welcome | BlockKind::User => 0,
		BlockKind::Thinking => 1,
		BlockKind::Assistant => 2,
		BlockKind::Tool => 3,
		BlockKind::Notice => 4,
		BlockKind::Usage => 5,
		BlockKind::Divider => 6,
	};
	handle.get().saturating_mul(8).saturating_add(suffix)
}

fn tool_block(
	dom: &Dom,
	handle: Handle,
	node: &Node,
	tool: &Str,
	cards: &CardRegistry,
	ui: &UiContext,
	options: &Options<'_>,
) -> Option<RenderedBlock> {
	let view = card_view(dom, handle, node, options)?;
	let status = prop_text(node, PropId::Status).unwrap_or_else(|| Str::new_static("running"));
	let component = cards.render(tool.as_str(), &view, options.expanded, ui);
	let mut text = StrMut::new(tool.as_str());
	text.push_str(" ");
	text.push_str(status.as_str());
	if let Some(result) = view
		.result
		.and_then(node_text)
		.filter(|text| !text.is_empty())
	{
		text.push_str("\n");
		text.push_str(result.as_str());
	}
	if let Some(diag) = view
		.diag
		.and_then(node_text)
		.filter(|text| !text.is_empty())
	{
		text.push_str("\n");
		text.push_str(diag.as_str());
	}
	let finalized = matches!(status.as_str(), "ok" | "error" | "cancelled" | "aborted");
	Some(rendered(handle, BlockKind::Tool, text.freeze(), Mode::Mutable, finalized, component))
}

fn child(dom: &Dom, parent: Handle, tag: KnownTag) -> Option<&Node> {
	dom.children(parent)
		.iter()
		.filter_map(|handle| dom.get(*handle))
		.find(|node| node.tag == Tag::Known(tag))
}

/// The property's text, preferring an open stream buffer so streaming
/// content projects before the stream closes.
fn live_text(dom: &Dom, handle: Handle, node: &Node, prop: PropId) -> Option<Str> {
	let key: omp_dom::PropKey = prop.into();
	match dom.stream_text(handle, &key) {
		Some(text) => Some(Str::new(text)),
		None => prop_text(node, prop),
	}
}

fn prop_text(node: &Node, prop: PropId) -> Option<Str> {
	node
		.prop(&prop.into())
		.and_then(Value::as_str)
		.map(Str::new)
}

fn node_text(node: &Node) -> Option<Str> {
	node
		.content
		.clone()
		.or_else(|| prop_text(node, PropId::Text))
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use omp_agent::KernelEvent;
	use omp_session::{ComponentRegistry, Session};
	use omp_tui::{Ui, frame_text};

	use super::*;

	fn empty_session() -> Session {
		let directory = tempfile::tempdir().expect("temp directory");
		let path = directory.keep().join("project.oms");
		Session::create(path, ComponentRegistry::standard()).expect("session")
	}

	/// A session whose newest assistant is still streaming: reasoning, then
	/// answer text when `text` is non-empty — none of it finalized.
	fn streaming(thinking: &str, text: &str) -> Session {
		let mut session = empty_session();
		session.begin_turn().expect("turn");
		session.user("hi", Vec::new()).expect("user");
		session
			.assistant_start("test/model", "test", "test/model")
			.expect("assistant");
		let turn = *session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn");
		let assistant = session
			.dom()
			.children(turn)
			.iter()
			.copied()
			.find(|handle| {
				session
					.dom()
					.get(*handle)
					.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
			})
			.expect("assistant handle");
		let sid = session
			.stream_open(assistant, PropId::Thinking.into())
			.expect("thinking stream");
		session
			.stream_append(sid, thinking)
			.expect("thinking delta");
		if !text.is_empty() {
			let sid = session
				.stream_open(assistant, PropId::Text.into())
				.expect("text stream");
			session.stream_append(sid, text).expect("text delta");
		}
		session
	}

	fn render(component: Component, width: u16) -> String {
		let ui = Ui::from_root(component, width, UiContext::default());
		frame_text(ui.frame())
	}

	fn projected(session: &Session, options: &Options<'_>) -> Vec<RenderedBlock> {
		project(session.dom(), &CardRegistry::standard(), &UiContext::default(), options)
	}

	/// pi `user-message.ts`: `new Markdown(text, 1, 1, …)` on the tinted
	/// background — inline emphasis renders, fences render as code, and a
	/// padded blank row sits above and below the text.
	#[test]
	fn user_bubble_renders_markdown_with_padding_rows() {
		let local = Local::default();
		let options = Options::new(&local);
		let mut session = empty_session();
		session.begin_turn().expect("turn");
		session
			.user("run **exactly** this:\n\n```sh\necho pong\n```", Vec::new())
			.expect("user");
		let block = projected(&session, &options)
			.into_iter()
			.find(|block| block.view.kind == BlockKind::User)
			.expect("user block");
		let text = render(block.component, 40);
		let rows: Vec<&str> = text.split('\n').collect();
		assert!(rows.len() > 2, "{text}");
		assert!(rows.first().is_some_and(|row| row.trim().is_empty()), "top pad row:\n{text}");
		assert!(rows.last().is_some_and(|row| row.trim().is_empty()), "bottom pad row:\n{text}");
		assert!(text.contains("run exactly this:"), "emphasis markers must not leak:\n{text}");
		assert!(!text.contains("**"), "{text}");
		assert!(
			text.contains("  echo pong"),
			"fenced code renders as an indented code block:\n{text}"
		);
		assert!(
			rows
				.iter()
				.all(|row| row.is_empty() || row.starts_with(' ')),
			"one cell of left padding:\n{text}"
		);
	}

	/// pi `assistant-message.ts`: the reasoning trace is a Markdown block
	/// (`new Markdown(text, 1, 0, …, { italic: true })`), so list bullets and
	/// emphasis in the trace render instead of leaking their markers.
	#[test]
	fn reasoning_trace_renders_as_markdown() {
		let local = Local::default();
		let options = Options { smooth: false, ..Options::new(&local) };
		let session = streaming("- **first** step\n- second step", "");
		let block = projected(&session, &options)
			.into_iter()
			.find(|block| block.view.kind == BlockKind::Thinking)
			.expect("thinking block");
		assert_eq!(block.view.mode, Mode::AppendOnly);
		assert_eq!(block.stream.as_deref(), Some("- **first** step\n- second step"));
		let text = render(block.component, 40);
		assert!(!text.contains("**"), "emphasis markers must not leak:\n{text}");
		assert!(text.contains("- first step") && text.contains("- second step"), "{text}");
	}

	/// pi `#shouldAnimateThinking`: with reasoning hidden, the pulse shows
	/// while the model's newest delta is reasoning — including a second
	/// reasoning phase after visible text — and ends once text is the tail.
	#[test]
	fn hidden_thinking_pulse_follows_the_streaming_head_not_prior_text() {
		let session = streaming("considering", "partial answer");
		let has_pulse = |local: &Local| {
			let options = Options { show_thinking: false, ..Options::new(local) };
			projected(&session, &options)
				.iter()
				.any(|block| block.view.kind == BlockKind::Thinking && block.view.text == "Thinking")
		};
		let mut local = Local::default();
		assert!(!local.on_kernel_event(&KernelEvent::InferenceStarted, Duration::ZERO));
		assert!(local.on_kernel_event(&KernelEvent::ThinkingDelta("c".into()), Duration::ZERO));
		assert!(!local.on_kernel_event(&KernelEvent::ThinkingDelta("o".into()), Duration::ZERO));
		assert!(has_pulse(&local), "reasoning is the head");
		assert!(local.on_kernel_event(&KernelEvent::TextDelta("p".into()), Duration::ZERO));
		assert!(!has_pulse(&local), "text is the head");
		assert!(local.on_kernel_event(&KernelEvent::ThinkingDelta("more".into()), Duration::ZERO));
		assert!(has_pulse(&local), "a later reasoning phase pulses again despite prior text");
		assert!(!local.on_kernel_event(&KernelEvent::InferenceStarted, Duration::ZERO));
		assert_eq!(local.stream_head(), None);
		assert!(!has_pulse(&local), "without a delta observed, started text means reasoning stopped");
		let fresh = streaming("considering", "");
		let options = Options { show_thinking: false, ..Options::new(&local) };
		assert!(
			projected(&fresh, &options)
				.iter()
				.any(|block| block.view.text == "Thinking"),
			"without a delta observed, reasoning with no text pulses"
		);
	}

	/// The handle of the last `<user>` in the newest turn.
	fn last_user(session: &Session) -> Handle {
		let turn = *session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn");
		session
			.dom()
			.children(turn)
			.iter()
			.copied()
			.rev()
			.find(|handle| {
				session
					.dom()
					.get(*handle)
					.is_some_and(|node| node.tag == Tag::Known(KnownTag::User))
			})
			.expect("user handle")
	}

	fn set_prop(session: &mut Session, handle: Handle, prop: PropId, value: Value) {
		session
			.patch(omp_dom::Txn {
				cause: session.head().expect("head"),
				label: None,
				ops:   vec![omp_dom::Op::Set { h: handle, prop: prop.into(), value }],
			})
			.expect("patch");
	}

	/// A finalized reply of `text` after the user prompt in the newest turn.
	fn reply(session: &mut Session, text: &str) {
		session
			.assistant_start("test/model", "test", "test/model")
			.expect("assistant");
		let turn = *session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn");
		let assistant = *session.dom().children(turn).last().expect("assistant");
		let sid = session
			.stream_open(assistant, PropId::Text.into())
			.expect("text stream");
		session.stream_append(sid, text).expect("text delta");
		session.stream_close(sid).expect("close");
		session.assistant_end("stop").expect("end");
	}

	/// The rendered user row and the first reply block, consumed from a
	/// projection.
	fn user_and_assistant(blocks: Vec<RenderedBlock>) -> (String, Option<RenderedBlock>) {
		let mut user = None;
		let mut assistant = None;
		for block in blocks {
			match block.view.kind {
				BlockKind::User if user.is_none() => user = Some(block),
				BlockKind::Assistant if assistant.is_none() => assistant = Some(block),
				_ => {},
			}
		}
		let user = user.expect("user block");
		(render(user.component, 40), assistant)
	}

	fn user_and_assistant_text(blocks: Vec<RenderedBlock>) -> (String, Option<Str>) {
		let (user, assistant) = user_and_assistant(blocks);
		(user, assistant.map(|block| block.view.text))
	}

	/// Journaled attachments (`<user data=[BlobRef…]>`) the text does not
	/// reference render as `<paperclip> #N · size` chips under the prompt,
	/// while an attachment the text already shows as a vision marker is not
	/// repeated; the chips ride guest and synthetic rows too.
	#[test]
	fn journaled_attachments_render_as_chips_under_the_prompt() {
		let local = Local::default();
		let options = Options::new(&local);
		let blob = |size: u64| Attachment {
			blob: omp_journal::blob::BlobRef { hash: omp_core::Hash32::new([7; 32]), size },
			mime: Str::new_static("image/png"),
		};
		let mut session = empty_session();
		session.begin_turn().expect("turn");
		session
			.user("look at [Image #1, 640x480] attachment://1 please", vec![blob(2048), blob(300)])
			.expect("user");
		let (text, _) = user_and_assistant_text(projected(&session, &options));
		let clip = Charset::default().icon(Icon::Paperclip);
		let image = Charset::default().icon(Icon::Image);
		assert!(text.contains(&format!("{image} #1")), "vision marker collapses:\n{text}");
		assert!(text.contains(&format!("{clip} #2 · 300B")), "unreferenced attachment chip:\n{text}");
		assert!(!text.contains(&format!("{clip} #1")), "referenced attachment is not repeated:\n{text}");

		let user = last_user(&session);
		set_prop(&mut session, user, PropId::Author, Value::Str(Str::new_static("ada")));
		let (guest, _) = user_and_assistant_text(projected(&session, &options));
		assert!(guest.contains("«ada» ›"), "{guest}");
		assert!(guest.contains(&format!("{image} #1")), "guest bubble collapses markers:\n{guest}");
		assert!(guest.contains(&format!("{clip} #2 · 300B")), "guest bubble keeps chips:\n{guest}");

		set_prop(&mut session, user, PropId::Author, Value::Null);
		set_prop(&mut session, user, PropId::Synthetic, Value::Bool(true));
		let (synthetic, _) =
			user_and_assistant_text(projected(&session, &Options { expanded: true, ..options }));
		assert!(synthetic.contains("Synthetic input"), "{synthetic}");
		assert!(synthetic.contains(&format!("{image} #1")), "synthetic row collapses markers:\n{synthetic}");
		assert!(!synthetic.contains("[Image #1"), "{synthetic}");
		assert!(synthetic.contains(&format!("{clip} #2 · 300B")), "synthetic row keeps chips:\n{synthetic}");
	}

	/// pi `reaction.ts` + `#reactionRow`: a reply opening with a lone emoji
	/// line badges the preceding user bubble (right-aligned in its top
	/// padding row) and the emoji leaves the prose; the badge survives a
	/// re-projection because it derives from the journaled text.
	#[test]
	fn leading_emoji_line_badges_the_user_bubble_and_leaves_the_prose() {
		let local = Local::default();
		let options = Options { smooth: false, ..Options::new(&local) };
		let mut session = empty_session();
		session.begin_turn().expect("turn");
		session.user("ship it", Vec::new()).expect("user");
		reply(&mut session, "🎉\nShipped.");
		let (user, assistant) = user_and_assistant(projected(&session, &options));
		let rows: Vec<&str> = user.split('\n').collect();
		assert!(rows[0].trim_end().ends_with("🎉"), "badge in the top padding row:\n{user}");
		assert!(rows[0].starts_with(' '), "badge sits inside the horizontal padding:\n{user}");
		assert!(user.contains("ship it"), "{user}");
		assert!(rows.last().is_some_and(|row| row.trim().is_empty()), "bottom pad row:\n{user}");
		let assistant = assistant.expect("assistant block");
		assert_eq!(assistant.view.text, "Shipped.", "the emoji line leaves the prose");
		assert_eq!(assistant.stream.as_deref(), Some("Shipped."));
		assert!(!render(assistant.component, 40).contains("🎉"));
	}

	/// No target, no reaction: a reply after tool calls (a continuation)
	/// keeps a leading emoji line verbatim, and a synthetic prompt takes no
	/// badge. While streaming, an emoji-only opening run is withheld until
	/// it proves to be a reaction or ordinary text.
	#[test]
	fn reactions_need_a_user_bubble_target_and_are_withheld_while_pending() {
		let local = Local::default();
		let options = Options { smooth: false, ..Options::new(&local) };
		let mut session = empty_session();
		session.begin_turn().expect("turn");
		session.user("do two things", Vec::new()).expect("user");
		reply(&mut session, "First.");
		reply(&mut session, "👍\nSecond.");
		let blocks = projected(&session, &options);
		let second = blocks
			.iter()
			.filter(|block| block.view.kind == BlockKind::Assistant)
			.nth(1)
			.expect("second reply");
		assert_eq!(second.view.text, "👍\nSecond.", "a continuation has nothing to react to");
		let (user, _) = user_and_assistant_text(blocks);
		assert!(!user.contains("👍"), "{user}");

		let mut session = empty_session();
		session.begin_turn().expect("turn");
		session.user("# Session update\nstate", Vec::new()).expect("user");
		let user = last_user(&session);
		set_prop(&mut session, user, PropId::Synthetic, Value::Bool(true));
		reply(&mut session, "👍\nNoted.");
		let (row, assistant) = user_and_assistant_text(projected(&session, &options));
		assert!(!row.contains("👍"), "synthetic rows take no badge:\n{row}");
		assert_eq!(assistant.as_deref(), Some("👍\nNoted."), "left verbatim without a target");

		let live = streaming("", "👍");
		let blocks = projected(&live, &options);
		assert!(
			!blocks.iter().any(|block| block.view.kind == BlockKind::Assistant),
			"an emoji-only opening run is withheld while it may still become a reaction"
		);
		let live = streaming("", "👍 sure");
		let (_, assistant) = user_and_assistant_text(projected(&live, &options));
		assert_eq!(assistant.as_deref(), Some("👍 sure"), "proven prose streams through");
	}

	/// pi `collapseImageMarkers`: bracketed vision markers (and their paired
	/// `attachment://N` reference) become the composer's `<icon> #N` chip;
	/// malformed markers and ordinary brackets stay verbatim.
	#[test]
	fn image_markers_collapse_into_attachment_chips() {
		let image = Charset::Unicode.icon(Icon::Image);
		let video = Charset::Unicode.icon(Icon::Video);
		let collapse = |text: &str| collapse_image_markers(&Str::new(text), Charset::Unicode);
		assert_eq!(
			collapse("see [Image #1, 640x480] attachment://1 and [Video #12] now"),
			format!("see {image} #1 and {video} #12 now")
		);
		assert_eq!(collapse("[Image #2] attachment://21"), format!("{image} #2 attachment://21"));
		assert_eq!(
			collapse("[Image #0] [Image #] [Image #1, a\nb] [x] [Image #3"),
			"[Image #0] [Image #] [Image #1, a\nb] [x] [Image #3"
		);
		assert_eq!(collapse("plain [brackets]"), "plain [brackets]");
		assert_eq!(collapse("[Image #1]"), format!("{image} #1"));
		assert_eq!(
			collapse_image_markers(&Str::new("[Image #1]"), Charset::Ascii),
			format!("{} #1", Charset::Ascii.icon(Icon::Image))
		);
	}
}
