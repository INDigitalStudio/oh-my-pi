//! Typed card for `computer@1`.

use omp_core::Str;
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde_json::Value;

use super::{
	Card, CardStatus, CardView, Component, elapsed_badge, preview_lines, result_image, typed_fault,
	typed_input, typed_result,
};

/// Native-desktop status card.
pub struct ComputerCard;

/// pi `PREVIEW_LIMITS.COMPUTER_CODE_COLLAPSED`: script lines a collapsed
/// card shows; expanded shows the whole script.
const CODE_COLLAPSED: usize = 10;
/// pi `PREVIEW_LIMITS.OUTPUT_COLLAPSED` / `OUTPUT_EXPANDED`: output lines
/// shown collapsed and expanded.
const OUTPUT_COLLAPSED: usize = 3;
const OUTPUT_EXPANDED: usize = 10;

impl Card for ComputerCard {
	fn tool(&self) -> &'static str {
		"computer"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
		let input = typed_input::<omp_tools::computer::Params>(view);
		let result = typed_result::<omp_tools::computer::Payload>(view);
		let code = result
			.as_ref()
			.and_then(|value| value.get("code"))
			.and_then(Value::as_str)
			.or_else(|| input.as_ref()?.get("code")?.as_str())
			.unwrap_or_default();
		let output = result
			.as_ref()
			.and_then(|value| value.get("results"))
			.filter(|value| !value.as_array().is_some_and(Vec::is_empty))
			.map(|value| serde_json::to_string_pretty(value).unwrap_or_default());
		let artifacts = result
			.as_ref()
			.and_then(|value| value.get("artifacts"))
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
			.filter_map(Value::as_str)
			.map(|artifact| result_image(&Str::new(artifact), "image/png", None, ui))
			.collect::<Vec<_>>();
		let fault = typed_fault::<omp_tools::computer::Fault>(view).or_else(|| {
			view
				.diag
				.and_then(|node| {
					node.content.clone().or_else(|| {
						node
							.prop(&omp_dom::PropId::Text.into())
							.and_then(omp_dom::Value::as_str)
							.map(Str::new)
					})
				})
				.map(|raw| {
					serde_json::from_str::<String>(raw.as_str())
						.map(Str::new)
						.unwrap_or(raw)
				})
		});
		// pi `statusSuffix`: the header names the error state.
		let title = if view.status == CardStatus::Failed {
			Str::new_static("Computer: error")
		} else {
			Str::new_static("Computer")
		};
		// pi shows a bounded script and output preview in both states; only
		// the bounds change with `@expanded`. A call without a script (old
		// persisted `{window, actions}` calls) stays a bare header.
		let code = (!code.is_empty()).then(|| {
			if expanded {
				Str::new(code)
			} else {
				preview_lines(code, CODE_COLLAPSED)
			}
		});
		let output = output.map(|output| {
			preview_lines(
				output.as_str(),
				if expanded {
					OUTPUT_EXPANDED
				} else {
					OUTPUT_COLLAPSED
				},
			)
		});
		// pi: `if (code === undefined) return new Text(header)` — no body at
		// all, the error section included.
		let fault = fault.filter(|_| code.is_some());
		dom! {
			<col>
				<row gap=1>
					match view.status {
						CardStatus::StreamingArgs | CardStatus::InProgress => <i:pending/>,
						CardStatus::Done => <i:success/>,
						CardStatus::Failed => <i:error/>,
					}
					<text bold>{title}</text>
					if let Some(badge) = elapsed_badge(view) { {badge} }
				</row>
				if let Some(code) = code {
					<text pad-x=2 fg=muted>{"Code"}</text>
					<pre pad-x=2>{code}</pre>
				}
				if let Some(output) = output {
					<text pad-x=2 fg=muted>{"Output"}</text>
					<pre pad-x=2>{output}</pre>
				}
				if expanded { {artifacts} }
				if let Some(fault) = fault { <text pad-x=2 fg=err>{fault}</text> }
			</col>
		}
		.into_component()
	}
}
