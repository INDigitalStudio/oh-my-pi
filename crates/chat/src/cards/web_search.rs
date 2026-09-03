//! Typed card for web-search answers and citations.

use std::time::{SystemTime, UNIX_EPOCH};

use omp_core::{Str, sf};
use omp_tui::{Border, IntoComponent as _, UiContext, dom};
use serde_json::Value;

use super::{
	Card, CardStatus, CardView, Component, elapsed_badge, typed_fault, typed_input, typed_result,
};

/// Renders a web-search answer, source list, provider metadata, or fault.
pub struct WebSearchCard;

impl Card for WebSearchCard {
	fn tool(&self) -> &'static str {
		"web_search"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::web_search::Params>(view).unwrap_or(Value::Null);
		let query = args
			.get("query")
			.and_then(Value::as_str)
			.unwrap_or_default();
		if view.status == CardStatus::Failed {
			let fault = view.fault::<omp_tools::web_search::Fault>();
			let provider = fault
				.as_ref()
				.and_then(|fault| serde_json::to_value(fault).ok())
				.and_then(|value| {
					value
						.get("provider")
						.and_then(Value::as_str)
						.map(provider_name)
				})
				.filter(|provider| !provider.is_empty());
			let error = typed_fault::<omp_tools::web_search::Fault>(view)
				.unwrap_or_else(|| omp_core::Str::new_static("search failed"));
			let title = format!(
				"{} Web Search{}",
				ui.charset.icon_named("error").unwrap_or_default(),
				provider.map_or_else(String::new, |provider| format!(": {provider}")),
			);
			return dom! { <box border=round title={title} title_pad=3 pad="0 1"><text>{format!("Error: {error}")}</text></box> }.into_component();
		}
		let Some(_typed) = typed_result::<omp_tools::web_search::Payload>(view) else {
			return dom! {
				<row gap=1><i:pending/><text>{format!("Web Search: {query}")}</text>
					if let Some(badge) = elapsed_badge(view) { {badge} }
				</row>
			}
			.into_component();
		};
		let payload = view.outcome_json().unwrap_or(Value::Null);
		let result = payload.get("response").unwrap_or(&Value::Null);
		let provider = provider_name(
			result
				.get("engine")
				.and_then(Value::as_str)
				.unwrap_or("web"),
		);
		let sources = result
			.get("sources")
			.and_then(Value::as_array)
			.cloned()
			.unwrap_or_default();
		let title = format!(
			"{} Web Search: {provider} {} sources",
			ui.charset.icon_named("web-search").unwrap_or_default(),
			sources.len()
		);
		let answer = result
			.get("answer")
			.and_then(Value::as_str)
			.unwrap_or_default()
			.replace("<br>\n", "\n");
		let usage = result.get("usage").unwrap_or(&Value::Null);
		let usage_text = format!(
			"Usage: in {} · out {} · total {} · search {}",
			usage
				.get("input_tokens")
				.and_then(Value::as_u64)
				.unwrap_or_default(),
			usage
				.get("output_tokens")
				.and_then(Value::as_u64)
				.unwrap_or_default(),
			usage
				.get("total_tokens")
				.and_then(Value::as_u64)
				.unwrap_or_default(),
			usage
				.get("search_requests")
				.and_then(Value::as_u64)
				.unwrap_or_default(),
		);
		let (branch, last, _) = ui.charset.guides(Border::Square);
		// pi `renderTreeList` (`MAX_COLLAPSED_ITEMS`): a collapsed card lists
		// the first eight sources and a `… N more sources` tail row; expanded
		// lists them all.
		let shown = if expanded {
			sources.len()
		} else {
			sources.len().min(COLLAPSED_SOURCES)
		};
		let hidden = sources.len() - shown;
		let mut source_rows = Vec::with_capacity(shown + usize::from(hidden > 0));
		for (index, source) in sources.iter().take(shown).enumerate() {
			let prefix = if index + 1 == shown && hidden == 0 {
				last
			} else {
				branch
			};
			let name = source
				.get("title")
				.and_then(Value::as_str)
				.filter(|title| !title.trim().is_empty())
				.or_else(|| source.get("url").and_then(Value::as_str))
				.unwrap_or("Untitled");
			let domain = source
				.get("url")
				.and_then(Value::as_str)
				.map(domain_of)
				.filter(|domain| !domain.is_empty());
			let head = match domain {
				Some(domain) => sf!("{prefix} {name} ({domain})"),
				None => sf!("{prefix} {name}"),
			};
			let age = source_age(source).map(|age| match age {
				SourceAge::Relative(ms) => {
					dom! { <row gap=1 fg=muted><text>{"·"}</text><time kind="relative" ms={ms}/></row> }
						.into_component()
				},
				SourceAge::Literal(date) => {
					dom! { <row gap=1 fg=muted><text>{"·"}</text><text>{date}</text></row> }
						.into_component()
				},
			});
			source_rows.push(
				dom! {
					<row gap=1>
						<text>{head}</text>
						if let Some(age) = age { {age} }
					</row>
				}
				.into_component(),
			);
		}
		if hidden > 0 {
			let more = sf!("{last} … {hidden} more source{}", if hidden == 1 { "" } else { "s" });
			source_rows.push(dom! { <text fg=muted>{more}</text> }.into_component());
		}
		if source_rows.is_empty() {
			source_rows.push(dom! { <text fg=muted>{"No sources returned"}</text> }.into_component());
		}
		let answer = if answer.trim().is_empty() {
			Str::new_static("No answer text returned")
		} else {
			Str::new(answer)
		};
		let query = format!("Query: {query}");
		let provider_line = format!(
			"Provider: {} @ {provider} (API)",
			result
				.get("model")
				.and_then(Value::as_str)
				.unwrap_or_default(),
		);
		dom! {
			<box border=round title={title} title_pad=3 pad="0 1">
				<col>
					<text>{query}</text>
					<hr title="Answer" title_pad=3/>
					<pre>{answer}</pre>
					<hr title="Sources" title_pad=3/>
					{source_rows}
					<hr title="Metadata" title_pad=3/>
					<text>{provider_line}</text>
					<text>{usage_text}</text>
				</col>
			</box>
		}
		.into_component()
	}
}

/// pi `PREVIEW_LIMITS.COLLAPSED_ITEMS`: sources listed before the collapsed
/// card folds the rest into a `… N more sources` row.
const COLLAPSED_SOURCES: usize = 8;

/// How a source's publication time is painted (pi `formatAge(src.ageSeconds)
/// || src.publishedDate`).
enum SourceAge {
	/// Age in milliseconds for a live `<time kind=relative>` badge.
	Relative(u64),
	/// The engine's own date text, shown verbatim.
	Literal(Str),
}

/// The source's age: a relative badge when the engine reported an
/// `age_seconds` or the facade encoded `published_at` as Unix seconds
/// (`omp_serve::inference`), the date text verbatim when it is an ISO date,
/// nothing when unknown. Never invented.
fn source_age(source: &Value) -> Option<SourceAge> {
	if let Some(age) = source
		.get("age_seconds")
		.or_else(|| source.get("ageSeconds"))
		.and_then(Value::as_u64)
		.filter(|age| *age > 0)
	{
		return Some(SourceAge::Relative(age.saturating_mul(1000)));
	}
	let published = source
		.get("published_at")
		.or_else(|| source.get("published_date"))
		.or_else(|| source.get("publishedDate"))
		.and_then(Value::as_str)
		.map(str::trim)
		.filter(|text| !text.is_empty())?;
	match published.parse::<u64>() {
		Ok(secs) if secs > 0 => {
			let now = SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.map_or(0, |elapsed| elapsed.as_secs());
			Some(SourceAge::Relative(now.saturating_sub(secs).saturating_mul(1000)))
		},
		Ok(_) => None,
		Err(_) => Some(SourceAge::Literal(Str::new(published))),
	}
}

fn provider_name(value: &str) -> String {
	let mut chars = value.chars();
	chars
		.next()
		.map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}
fn domain_of(url: &str) -> String {
	url.split_once("://")
		.map_or(url, |(_, rest)| rest)
		.split('/')
		.next()
		.unwrap_or_default()
		.trim_start_matches("www.")
		.to_owned()
}
