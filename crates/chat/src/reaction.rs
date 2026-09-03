//! Agent reactions (pi `modes/components/reaction.ts`). A reply whose text
//! opens with a lone emoji line (`<emoji>\n`) is reacting to the transcript
//! block before it: the emoji is lifted out of the prose and shown as a badge
//! on that block instead. While the reply streams, the opening run is
//! withheld until it either completes a reaction line or proves to be
//! ordinary text, so the emoji never flashes inside the reply.
//!
//! Reactions are derived from the journaled assistant text, never stored, so
//! a rebuilt transcript reproduces them exactly (ADR 0005).

use std::sync::LazyLock;

use regex::Regex;

/// Longest emoji grapheme (UTF-16 units, pi's measure) still worth
/// withholding for.
const MAX_REACTION_UNITS: usize = 32;

/// One emoji grapheme (pi `REACTION_RE`, `\p{RGI_Emoji}` spelled out as the
/// RGI sequence grammar): a pictograph, a flag (two regional indicators), or
/// a keycap, followed by any run of presentation selectors, skin tones, tag
/// letters, and ZWJ-joined pictographs.
static REACTION: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(
		r"(?x)^
		(?:\p{Extended_Pictographic}|\p{Regional_Indicator}\p{Regional_Indicator}|[0-9\#*]\x{FE0F}?\x{20E3})
		(?:\x{FE0F}|\x{20E3}|\p{Emoji_Modifier}|[\x{E0020}-\x{E007F}]
		  |\x{200D}\p{Extended_Pictographic}(?:\x{FE0F}|\p{Emoji_Modifier})?)*
		$",
	)
	.expect("reaction grammar")
});

/// A still-streaming run that can only ever be an emoji grapheme plus
/// trailing blanks (pi `REACTION_PREFIX_RE`).
static REACTION_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(
		r"(?x)^
		(?:[\p{Extended_Pictographic}\p{Emoji_Modifier}\p{Regional_Indicator}\x{E0020}-\x{E007F}]
		  |\x{FE0F}|\x{200D}|\x{20E3})*
		[\ \t]*$",
	)
	.expect("reaction prefix grammar")
});

/// The reaction line split off the front of assistant text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReactionSplit<'a> {
	/// The reaction emoji when the text opens with `<emoji>\n`.
	pub emoji:   Option<&'a str>,
	/// Text with the reaction line removed; the input when there is none.
	pub body:    &'a str,
	/// True while the (newline-less) text could still grow into a reaction
	/// line.
	pub pending: bool,
}

/// Splits a reaction line off the front of assistant text (pi
/// `splitReaction`). Leading whitespace and trailing blanks on the emoji
/// line are tolerated; anything else on that line makes it ordinary prose.
#[must_use]
pub fn split_reaction(text: &str) -> ReactionSplit<'_> {
	let start = text.len() - text.trim_start().len();
	let Some(newline) = text[start..].find('\n').map(|at| at + start) else {
		let head = &text[start..];
		return ReactionSplit {
			emoji:   None,
			body:    text,
			pending: units(head) <= MAX_REACTION_UNITS && REACTION_PREFIX.is_match(head),
		};
	};
	let head = text[start..newline].trim_end();
	if head.is_empty() || units(head) > MAX_REACTION_UNITS || !REACTION.is_match(head) {
		return ReactionSplit { emoji: None, body: text, pending: false };
	}
	ReactionSplit { emoji: Some(head), body: &text[newline + 1..], pending: false }
}

/// UTF-16 code units, pi's length measure.
fn units(text: &str) -> usize {
	text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
	use super::*;

	/// pi `splitReaction`: a lone emoji line lifts off as the reaction and
	/// the body is what follows it; emoji sequences (flags, skin tones,
	/// keycaps, ZWJ families) count as one grapheme.
	#[test]
	fn lone_emoji_lines_split_off_as_reactions() {
		assert_eq!(split_reaction("👍\nDone."), ReactionSplit {
			emoji:   Some("👍"),
			body:    "Done.",
			pending: false,
		});
		assert_eq!(split_reaction("  🎉  \n\nParty"), ReactionSplit {
			emoji:   Some("🎉"),
			body:    "\nParty",
			pending: false,
		});
		for emoji in ["🇺🇸", "👍🏽", "1️⃣", "👨‍👩‍👧", "👩🏽‍❤️‍👨🏻", "❤️", "🏴󠁧󠁢󠁳󠁣󠁴󠁿"] {
			let text = format!("{emoji}\nbody");
			assert_eq!(split_reaction(&text).emoji, Some(emoji), "{emoji}");
		}
	}

	/// Anything else on the first line makes it prose: words, two emoji,
	/// punctuation, or an over-long run.
	#[test]
	fn prose_first_lines_are_not_reactions() {
		for text in ["Sure 👍\nDone.", "👍👍\nDone.", "👍!\nDone.", "hello\nworld", "\nDone."] {
			let split = split_reaction(text);
			assert_eq!(split.emoji, None, "{text:?}");
			assert_eq!(split.body, text);
			assert!(!split.pending);
		}
		let long = format!("{}\nbody", "👍".repeat(17));
		assert_eq!(split_reaction(&long).emoji, None, "over the 32-unit budget");
	}

	/// While streaming (no newline yet), an emoji-only run is pending and
	/// withheld; a run that proves to be text is not.
	#[test]
	fn newline_less_emoji_runs_are_pending_until_they_prove_otherwise() {
		assert!(split_reaction("👍").pending);
		assert!(split_reaction("  👍 ").pending);
		assert!(split_reaction("").pending, "an empty stream may still open with a reaction");
		assert!(!split_reaction("👍 yes").pending);
		assert!(!split_reaction("Sure").pending);
		assert_eq!(split_reaction("👍").body, "👍");
	}
}
