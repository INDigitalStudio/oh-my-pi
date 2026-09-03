//! Extension and hook messages (pi `custom-message.ts` / `hook-message.ts`
//! over `message-frame.ts`): `<notice kind=custom|hook name=<type>>` elements
//! the kernel journals into a `<turn>` (`EnvEvent::Notice`), so they replay
//! on resume, vanish on rewind, and reach every peer actor (ADR 0005).

use omp_core::{Str, sf};
use omp_dom::{Node, PropId};
use omp_tui::{IntoComponent as _, dom};

use super::prop_text;
use crate::cards::Component;

/// pi `HOOK_COLLAPSED_LINES`: Markdown body lines a hook message shows
/// before the `…` fold while the transcript is collapsed.
const HOOK_COLLAPSED_LINES: usize = 5;

/// Which framed-message flavor a `<notice>` kind selects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display, strum::EnumString)]
#[strum(serialize_all = "kebab-case")]
pub enum CustomKind {
	/// Extension-injected message (pi `custom_message`): package glyph,
	/// never folded.
	Custom,
	/// Hook-authored message (pi `hookMessage`): hook glyph, folded to
	/// [`HOOK_COLLAPSED_LINES`] unless expanded.
	Hook,
}

/// Renders a journaled `<notice kind=custom|hook>` element: pi
/// `renderFramedMessage` — a rounded, muted-bordered box with one cell of
/// padding, a bold `<icon> <name>` header row followed by a blank row when
/// the element names its type, and the Markdown body.
#[must_use]
pub fn custom_message_card(kind: CustomKind, node: &Node, expanded: bool) -> Component {
	let name = prop_text(node, PropId::Name);
	let body = node.content.clone().unwrap_or_default();
	framed_message(kind, name, body, expanded)
}

/// [`custom_message_card`] over explicit fields.
#[must_use]
pub fn framed_message(kind: CustomKind, name: Option<Str>, body: Str, expanded: bool) -> Component {
	let icon = match kind {
		CustomKind::Custom => "package",
		CustomKind::Hook => "hook",
	};
	let body = if kind == CustomKind::Hook && !expanded {
		fold_lines(body, HOOK_COLLAPSED_LINES)
	} else {
		body
	};
	if let Some(name) = name {
		dom! {
			<box border=round bc=border bg=surface pad="1 1">
				<row gap=1>
					<icon name={icon} fg=accent/>
					<text bold fg=accent>{name}</text>
				</row>
				<spacer/>
				<md>{body}</md>
			</box>
		}
		.into_component()
	} else {
		// pi's unnamed custom message is a compact three-row box: border,
		// body, border. Vertical padding belongs only to the named frame.
		dom! {
			<box border=round bc=border bg=surface pad-x=1>
				<md>{body}</md>
			</box>
		}
		.into_component()
	}
}

/// The header the copy selector and block descriptors carry for a framed
/// message: `[<name>]` on its own line above the body, when named.
#[must_use]
pub fn framed_text(node: &Node) -> Str {
	let body = node.content.clone().unwrap_or_default();
	match prop_text(node, PropId::Name) {
		Some(name) => sf!("[{name}]\n{body}"),
		None => body,
	}
}

/// pi `collapseAfterLines`: the first `keep` lines then `…`.
fn fold_lines(body: Str, keep: usize) -> Str {
	let mut lines = body.as_str().split('\n');
	let mut folded = String::with_capacity(body.len());
	for (index, line) in lines.by_ref().take(keep).enumerate() {
		if index != 0 {
			folded.push('\n');
		}
		folded.push_str(line);
	}
	if lines.next().is_none() {
		return body;
	}
	folded.push_str("\n…");
	Str::new(folded)
}

#[cfg(test)]
mod tests {
	use omp_core::Str;
	use omp_dom::{KnownTag, PropId, Tag, Value};
	use omp_tui::{Ui, UiContext, frame_text};

	use super::*;

	fn rows(component: Component, width: u16) -> Vec<String> {
		let ui = Ui::from_root(component, width, UiContext::default());
		frame_text(ui.frame())
			.lines()
			.map(|row| row.trim_end().to_owned())
			.collect()
	}

	fn notice(kind: &'static str, name: Option<&'static str>, body: &'static str) -> Node {
		let mut node = Node {
			tag:     Tag::Known(KnownTag::Notice),
			props:   Default::default(),
			kids:    Vec::new(),
			content: Some(Str::new_static(body)),
		};
		node
			.props
			.push((PropId::Kind.into(), Value::Str(Str::new_static(kind))));
		if let Some(name) = name {
			node
				.props
				.push((PropId::Name.into(), Value::Str(Str::new_static(name))));
		}
		node
	}

	#[test]
	fn hook_box_renders_name_header_and_markdown() {
		let node = notice("hook", Some("pre-commit"), "Ran **3** checks\n\n- lint ok");
		let rows = rows(custom_message_card(CustomKind::Hook, &node, false), 40);
		assert!(rows[0].starts_with('╭') && rows[0].ends_with('╮'), "{rows:?}");
		assert!(rows.last().is_some_and(|row| row.starts_with('╰')), "{rows:?}");
		let header = rows
			.iter()
			.position(|row| row.contains("pre-commit"))
			.expect("name row");
		assert!(rows[header].contains(omp_tui::Charset::default().icon(omp_tui::Icon::Hook)));
		assert!(
			rows[header + 1]
				.trim_matches(|c| c == '│' || c == ' ')
				.is_empty(),
			"blank row after the header"
		);
		let body = rows
			.iter()
			.position(|row| row.contains("Ran 3 checks"))
			.expect("markdown body");
		assert!(body > header, "{rows:?}");
		assert!(rows.iter().any(|row| row.contains("lint ok")), "{rows:?}");
		assert_eq!(CustomKind::Hook.to_string(), "hook");
		assert_eq!("custom".parse::<CustomKind>(), Ok(CustomKind::Custom));
		assert_eq!(framed_text(&node), "[pre-commit]\nRan **3** checks\n\n- lint ok");
	}

	#[test]
	fn hook_body_folds_after_five_lines_unless_expanded() {
		let node = notice("hook", Some("audit"), "l1\nl2\nl3\nl4\nl5\nl6\nl7");
		let collapsed = rows(custom_message_card(CustomKind::Hook, &node, false), 30);
		assert!(collapsed.iter().any(|row| row.contains("l5")), "{collapsed:?}");
		assert!(!collapsed.iter().any(|row| row.contains("l6")), "{collapsed:?}");
		assert!(collapsed.iter().any(|row| row.contains('…')), "{collapsed:?}");
		let expanded = rows(custom_message_card(CustomKind::Hook, &node, true), 30);
		assert!(expanded.iter().any(|row| row.contains("l7")), "{expanded:?}");
		assert!(!expanded.iter().any(|row| row.contains('…')), "{expanded:?}");
		// Extension messages never fold.
		let custom = rows(custom_message_card(CustomKind::Custom, &node, false), 30);
		assert!(custom.iter().any(|row| row.contains("l7")), "{custom:?}");
		assert!(
			custom
				.iter()
				.any(|row| { row.contains(omp_tui::Charset::default().icon(omp_tui::Icon::Package)) })
		);
	}

	#[test]
	fn unnamed_message_has_no_header_row() {
		let node = notice("custom", None, "plain body");
		let rows = rows(custom_message_card(CustomKind::Custom, &node, false), 30);
		assert_eq!(rows.len(), 3, "{rows:?}");
		assert!(rows[1].contains("plain body"), "{rows:?}");
		assert_eq!(framed_text(&node), "plain body");
	}
}
