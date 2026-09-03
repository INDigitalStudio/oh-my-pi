//! `scheme://` internal URL references (pi `internal-url-autocomplete.ts`).
//!
//! A `skill://`, `rule://`, `local://`, `omp://`, `memory://`, `agent://`,
//! `artifact://`, … token ending at the cursor offers the resources the
//! application's resolver table can complete for that scheme, fuzzy-ranked
//! by the text typed after the slashes; acceptance inserts the full URL
//! plus a trailing space (like `@` file references).
//!
//! The application supplies candidates through [`UrlCompleter`]; the
//! provider asks it once per scheme while one token is being typed (the
//! editor re-queries on every keystroke, and the roster behind a scheme
//! does not change mid-word).

use std::sync::Arc;

use omp_core::{Str, sf};
use omp_tui::{EditorCompletion, Icon, Suggestion, Suggestions};

use super::fuzzy_score;

/// Upper bound on rows surfaced in the dropdown (pi `MAX_URL_SUGGESTIONS`).
const MAX_ROWS: usize = 25;

/// One completable resource under a scheme.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UrlCandidate {
	/// The full URL inserted on acceptance, e.g. `skill://humanizer`.
	pub value:       Str,
	/// Explanatory text shown beside the label.
	pub description: Option<Str>,
}

/// Application-supplied candidate source: every resource under `scheme`
/// (lowercased, without `://`), or `None` when the scheme has no
/// completion-capable resolver.
pub type UrlCompleter = Arc<dyn Fn(&str) -> Option<Vec<UrlCandidate>> + Send + Sync>;

/// A `scheme://query` token ending at the cursor (pi `InternalUrlContext`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UrlContext<'a> {
	/// Byte offset of the token start.
	pub start:  usize,
	/// Lowercased scheme, e.g. `local`.
	pub scheme: Str,
	/// Text typed after the slashes so far (host + path); may be empty.
	pub query:  &'a str,
}

/// Whether `character` may continue a URL token (pi `URL_TOKEN_RE` rest
/// class: anything but whitespace, quotes, parentheses, and angle brackets).
const fn is_url_char(character: char) -> bool {
	!(character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '(' | ')' | '<' | '>'))
}

/// Whether `character` may precede a URL token (pi `URL_TOKEN_RE` boundary
/// class).
const fn is_url_boundary(character: char) -> bool {
	character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '(' | '<' | '=')
}

/// Whether `scheme` spells a URL scheme: `[a-z][a-z0-9+.-]*`, any case.
fn is_scheme(scheme: &str) -> bool {
	let mut characters = scheme.chars();
	characters
		.next()
		.is_some_and(|first| first.is_ascii_alphabetic())
		&& characters
			.all(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-'))
}

/// Parses the internal URL token ending at `cursor`, if any: a scheme, a
/// colon, one or two slashes, and the partial resource.
#[must_use]
pub fn url_context(text: &str, cursor: usize) -> Option<UrlContext<'_>> {
	let before = text.get(..cursor)?;
	let start = before
		.char_indices()
		.rev()
		.find(|(_, character)| !is_url_char(*character))
		.map_or(0, |(at, character)| at + character.len_utf8());
	if start > 0
		&& !before[..start]
			.chars()
			.next_back()
			.is_some_and(is_url_boundary)
	{
		return None;
	}
	// `=` is both a boundary and a token character (`a=omp://`, and
	// `omp://k=v` alike); pi's regex takes the leftmost boundary whose
	// remainder parses as `scheme:/…`, so try each `=` from the left.
	let token = &before[start..];
	let starts = std::iter::once(start).chain(
		token
			.match_indices('=')
			.map(|(at, equals)| start + at + equals.len()),
	);
	for start in starts {
		let token = &before[start..];
		let Some((scheme, rest)) = token.split_once(':') else {
			continue;
		};
		if !is_scheme(scheme) {
			continue;
		}
		let Some(query) = rest.strip_prefix("//").or_else(|| rest.strip_prefix('/')) else {
			continue;
		};
		return Some(UrlContext { start, scheme: Str::new(scheme.to_ascii_lowercase()), query });
	}
	None
}

/// Decodes `%XX` escapes for matching (pi `decodeUrlCompletionValue`);
/// the raw value is returned untouched when the encoding is malformed.
fn percent_decode(value: &str) -> Str {
	if !value.contains('%') {
		return Str::new(value);
	}
	let bytes = value.as_bytes();
	let mut decoded = Vec::with_capacity(bytes.len());
	let mut index = 0;
	while index < bytes.len() {
		if bytes[index] == b'%'
			&& let Some(pair) = bytes.get(index + 1..index + 3)
			&& let Some(byte) = u8::from_str_radix(str::from_utf8(pair).unwrap_or("zz"), 16).ok()
		{
			decoded.push(byte);
			index += 3;
		} else {
			decoded.push(bytes[index]);
			index += 1;
		}
	}
	String::from_utf8(decoded).map_or_else(|_| Str::new(value), Str::new)
}

/// Type-indicator icon for a scheme's rows.
fn scheme_icon(scheme: &str) -> Icon {
	match scheme {
		"skill" => Icon::Skill,
		"rule" => Icon::RuleExtension,
		"memory" => Icon::Memory,
		"agent" | "history" => Icon::Agents,
		"artifact" | "local" | "attachment" => Icon::File,
		_ => Icon::Link,
	}
}

/// The resource part of a full URL: everything after `scheme:` and its
/// slashes.
fn resource_of<'a>(value: &'a str, scheme: &str) -> &'a str {
	value
		.get(scheme.len()..)
		.and_then(|rest| rest.strip_prefix(':'))
		.map_or(value, |rest| rest.trim_start_matches('/'))
}

/// Internal URL completion over an application-supplied candidate source.
pub struct InternalUrls {
	completer: UrlCompleter,
	/// Candidates fetched for the scheme of the token being typed.
	cached:    Option<(Str, Option<Vec<UrlCandidate>>)>,
}

impl InternalUrls {
	/// Builds the provider over `completer`.
	#[must_use]
	pub fn new(completer: UrlCompleter) -> Self {
		Self { completer, cached: None }
	}

	fn candidates(&mut self, scheme: &Str) -> Option<&[UrlCandidate]> {
		if self
			.cached
			.as_ref()
			.is_none_or(|(cached, _)| cached != scheme)
		{
			self.cached = Some((scheme.clone(), (self.completer)(scheme)));
		}
		self
			.cached
			.as_ref()
			.and_then(|(_, candidates)| candidates.as_deref())
	}
}

impl EditorCompletion for InternalUrls {
	fn suggest(&mut self, text: &str, cursor: usize) -> Option<Suggestions> {
		let Some(context) = url_context(text, cursor) else {
			// The token ended: the next one asks for a fresh roster.
			self.cached = None;
			return None;
		};
		let query = context.query.to_ascii_lowercase();
		let scheme = context.scheme.clone();
		let candidates = self.candidates(&scheme)?;
		let mut scored: Vec<(u16, usize, &UrlCandidate)> = candidates
			.iter()
			.enumerate()
			.filter_map(|(index, candidate)| {
				let target =
					percent_decode(resource_of(&candidate.value, &scheme)).to_ascii_lowercase();
				fuzzy_score(&query, &target).map(|score| (score, index, candidate))
			})
			.collect();
		if scored.is_empty() {
			return None;
		}
		scored.sort_by_key(|(score, index, _)| (std::cmp::Reverse(*score), *index));
		let icon = scheme_icon(&scheme);
		let items = scored
			.into_iter()
			.take(MAX_ROWS)
			.map(|(_, _, candidate)| {
				let mut row = Suggestion::new(sf!("{} ", candidate.value), candidate.value.clone())
					.with_icon(icon);
				if let Some(description) = &candidate.description {
					row = row.with_description(description.clone());
				}
				row
			})
			.collect();
		Some(Suggestions { range: context.start..cursor, items })
	}

	fn accepted(&mut self, _replaced: &str, _suggestion: &Suggestion) {
		self.cached = None;
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;

	fn candidate(value: &'static str, description: Option<&'static str>) -> UrlCandidate {
		UrlCandidate {
			value:       Str::new_static(value),
			description: description.map(Str::new_static),
		}
	}

	fn provider(calls: Arc<AtomicUsize>) -> InternalUrls {
		InternalUrls::new(Arc::new(move |scheme: &str| {
			calls.fetch_add(1, Ordering::Relaxed);
			match scheme {
				"skill" => Some(vec![
					candidate("skill://humanizer", Some("Humanize prose")),
					candidate("skill://local-plan", None),
					candidate("skill://pyo3", Some("PyO3 boundary rules")),
				]),
				"local" => Some(vec![candidate("local://omp2-plan.md", None)]),
				"ssh" => Some(vec![candidate("ssh://alice%40prod", Some("prod"))]),
				_ => None,
			}
		}))
	}

	fn labels(suggestions: &Suggestions) -> Vec<&str> {
		suggestions
			.items
			.iter()
			.map(|item| match item.display() {
				omp_tui::SuggestionDisplay::Text(label) => label.as_str(),
				omp_tui::SuggestionDisplay::Emoji { .. } => unreachable!(),
			})
			.collect()
	}

	#[test]
	fn token_detection_mirrors_pi_url_token_re() {
		assert_eq!(
			url_context("skill://", 8),
			Some(UrlContext { start: 0, scheme: Str::new_static("skill"), query: "" })
		);
		assert_eq!(
			url_context("read Skill:/hum", 15),
			Some(UrlContext { start: 5, scheme: Str::new_static("skill"), query: "hum" })
		);
		assert_eq!(url_context("(local://x", 10).map(|context| context.start), Some(1));
		assert_eq!(url_context("a=omp://", 8).map(|context| context.start), Some(2));
		assert_eq!(
			url_context("x=skill://a=b", 13),
			Some(UrlContext { start: 2, scheme: Str::new_static("skill"), query: "a=b" })
		);
		// Mid-word, no slashes, or a non-scheme word never form a token.
		assert!(url_context("foo/skill://x", 13).is_none());
		assert!(url_context("skill:x", 7).is_none());
		assert!(url_context("1st://x", 7).is_none());
		assert!(url_context("plain text", 10).is_none());
		// The cursor mid-token completes the part before it.
		assert_eq!(url_context("skill://hum tail", 11).map(|context| context.query), Some("hum"));
	}

	#[test]
	fn rows_are_fuzzy_ranked_and_insert_the_full_url_with_a_space() {
		let calls = Arc::new(AtomicUsize::new(0));
		let mut urls = provider(Arc::clone(&calls));
		let text = "see skill://";
		let all = urls.suggest(text, text.len()).expect("every skill");
		assert_eq!(all.range, 4..text.len());
		assert_eq!(labels(&all), ["skill://humanizer", "skill://local-plan", "skill://pyo3"]);
		assert_eq!(all.items[0].value(), "skill://humanizer ");
		assert_eq!(all.items[0].description(), Some("Humanize prose"));
		assert_eq!(all.items[0].icon(), Some(Icon::Skill));
		let text = "see skill://lp";
		let fuzzy = urls.suggest(text, text.len()).expect("subsequence match");
		assert_eq!(labels(&fuzzy), ["skill://local-plan"]);
		let text = "see skill://PY";
		let prefix = urls
			.suggest(text, text.len())
			.expect("case-insensitive prefix");
		assert_eq!(labels(&prefix), ["skill://pyo3"]);
		assert!(urls.suggest("see skill://zzz", 15).is_none(), "no match closes");
		// One fetch served the whole token.
		assert_eq!(calls.load(Ordering::Relaxed), 1);
	}

	#[test]
	fn unknown_schemes_decline_and_scheme_switches_refetch() {
		let calls = Arc::new(AtomicUsize::new(0));
		let mut urls = provider(Arc::clone(&calls));
		assert!(urls.suggest("https://exa", 11).is_none());
		assert!(urls.suggest("https://exam", 12).is_none());
		assert_eq!(calls.load(Ordering::Relaxed), 1, "an unknown scheme is asked once");
		// Leaving the token and coming back asks again (the roster may have
		// changed while the user typed elsewhere).
		assert!(urls.suggest("https://exam ", 13).is_none());
		assert!(urls.suggest("https://exam h", 14).is_none());
		assert_eq!(calls.load(Ordering::Relaxed), 1, "no URL token, no fetch");
		assert!(urls.suggest("https://", 8).is_none());
		assert_eq!(calls.load(Ordering::Relaxed), 2, "a new token fetches again");
		let rows = urls.suggest("local://", 8).expect("local rows");
		assert_eq!(labels(&rows), ["local://omp2-plan.md"]);
		assert_eq!(calls.load(Ordering::Relaxed), 3, "a scheme switch fetches");
		// Acceptance forgets the roster so the next token sees fresh data.
		let accepted = rows.items[0].clone();
		urls.accepted("local://", &accepted);
		let rows = urls.suggest("local://o", 9).expect("local rows again");
		assert_eq!(rows.items.len(), 1);
		assert_eq!(calls.load(Ordering::Relaxed), 4);
	}

	#[test]
	fn percent_encoded_values_match_their_decoded_form() {
		let mut urls = provider(Arc::new(AtomicUsize::new(0)));
		let text = "ssh://alice@";
		let rows = urls
			.suggest(text, text.len())
			.expect("decoded host matches");
		assert_eq!(rows.items[0].value(), "ssh://alice%40prod ");
		assert_eq!(percent_decode("a%zz"), "a%zz");
	}
}
