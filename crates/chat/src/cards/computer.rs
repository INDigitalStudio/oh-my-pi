//! Typed card for `computer@1`.

use omp_core::Str;
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde_json::Value;

use super::{
	Card, CardStatus, CardView, Component, elapsed_badge, result_image, typed_fault, typed_input,
	typed_result,
};

/// Native-desktop status card.
pub struct ComputerCard;

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
		let fault = typed_fault::<omp_tools::computer::Fault>(view);
		dom! {
			<col>
				<row gap=1>
					match view.status {
						CardStatus::StreamingArgs | CardStatus::InProgress => <spinner kind=status/>,
						CardStatus::Done => <i:success/>,
						CardStatus::Failed => <i:error/>,
					}
					<text bold>{"Computer"}</text>
					if let Some(badge) = elapsed_badge(view) { {badge} }
				</row>
				if expanded {
					if !code.is_empty() { <pre pad-x=2>{code}</pre> }
					if let Some(output) = output { <pre pad-x=2>{output}</pre> }
					{artifacts}
				}
				if let Some(fault) = fault { <text pad-x=2 fg=err>{fault}</text> }
			</col>
		}
		.into_component()
	}
}
