//! Typed cards for approval resolution tools.
//!
//! `resolve` applies and `reject` discards the latest staged proposal
//! (`envd::devices_host` `finalize_proposal`); both take one `reason`
//! argument. pi (`tools/resolve.ts` `resolveRenderer`) paints the verb from
//! the action — `Accept` / `Discard`, `Failed` for an apply that errored —
//! then the proposal label and the reason the caller gave.

use omp_core::{Str, sf};
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde_json::Value;
use strum::EnumMessage as _;

use super::{Card, CardStatus, CardView, Component, elapsed_badge};

/// Renders accepted approval resolutions.
pub struct ResolveCard;
/// Renders rejected approval resolutions.
pub struct RejectCard;

impl Card for ResolveCard {
	fn tool(&self) -> &'static str {
		"resolve"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, ui: &UiContext) -> Component {
		render_resolution(view, Action::Apply, ui)
	}
}
impl Card for RejectCard {
	fn tool(&self) -> &'static str {
		"reject"
	}

	fn render(&self, view: &CardView<'_>, _expanded: bool, ui: &UiContext) -> Component {
		render_resolution(view, Action::Discard, ui)
	}
}

/// pi `ResolveAction`: what the resolution device does to the proposal. The
/// string form is pi's `renderCall` description; the `message` is its badge,
/// the staged → settled transition.
#[derive(Clone, Copy, Eq, PartialEq, strum::EnumMessage, strum::IntoStaticStr)]
enum Action {
	#[strum(serialize = "apply", message = "⟨proposed -> resolved⟩")]
	Apply,
	#[strum(serialize = "reject", message = "⟨proposed -> rejected⟩")]
	Discard,
}

impl Action {
	/// pi `renderCall` badge: the staged → settled transition.
	fn badge(self) -> &'static str {
		self.get_message().unwrap_or_default()
	}

	/// pi `ResolveAction`, the `renderCall` description.
	fn name(self) -> &'static str {
		self.into()
	}
}

/// The caller's one-sentence reason, from the arguments (the device input)
/// else the settled payload, trimmed; pi's `No reason provided` otherwise.
fn reason(view: &CardView<'_>) -> Option<Str> {
	let from = |value: Option<Value>| {
		value
			.as_ref()
			.and_then(|value| value.get("reason"))
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|reason| !reason.is_empty())
			.map(Str::new)
	};
	from(view.args_json())
		.or_else(|| from(view.result_json()))
		.or_else(|| {
			// Streaming arguments: the reason string may be open.
			let raw = view.args_text()?;
			let start = raw.find("\"reason\":\"")? + "\"reason\":\"".len();
			let rest = raw.get(start..)?;
			let reason = rest.split('"').next().unwrap_or(rest).trim();
			(!reason.is_empty()).then(|| Str::new(reason))
		})
}

/// The proposal's label (`<source tool>: <summary>` in pi) when the settled
/// payload names it; pi's `pending action` otherwise.
fn label(view: &CardView<'_>) -> Str {
	view
		.result_json()
		.as_ref()
		.and_then(|value| value.get("label"))
		.and_then(Value::as_str)
		.map(str::trim)
		.filter(|label| !label.is_empty())
		.map_or_else(|| Str::new_static("pending action"), Str::new)
}

fn render_resolution(view: &CardView<'_>, action: Action, _ui: &UiContext) -> Component {
	match view.status {
		CardStatus::StreamingArgs | CardStatus::InProgress => {
			let reason = reason(view);
			dom! {
				<row gap=0><i:pending fg=output/><text>{" "}</text><text fg=accent>{"Resolve"}</text><text>{":"}</text>
					<text fg=output wrap=pre>{format!(" {}", action.name())}</text><text>{" "}</text>
					<text fg={if action == Action::Apply { "ok" } else { "warn" }}>{action.badge()}</text>
					if let Some(reason) = reason { <text fg=output>{sf!(" {reason}")}</text> }
					if let Some(badge) = elapsed_badge(view) { {badge} }
				</row>
			}
			.into_component()
		},
		CardStatus::Done | CardStatus::Failed => {
			let failed = view.status == CardStatus::Failed;
			let verb = match (action, failed) {
				(Action::Apply, false) => "Accept:",
				(Action::Apply, true) => "Failed:",
				(Action::Discard, _) => "Discard:",
			};
			let header = sf!("{verb} {}", label(view));
			let reason = reason(view).unwrap_or_else(|| Str::new_static("No reason provided"));
			// pi's block color: success for an apply, error for a failed
			// apply, warning for a discard.
			let color = match (action, failed) {
				(Action::Apply, false) => "success",
				(_, true) => "error",
				(Action::Discard, false) => "warning",
			};
			// pi frames the block as five inverse rows: blank, header,
			// blank, reason, blank.
			dom! {
				<col fg={color}>
					<spacer h=1/>
					<row gap=1 pad-x=1>
						if action == Action::Apply && !failed { <i:resolve fg={color}/> } else { <i:error fg={color}/> }
						<text>{header}</text>
					</row>
					<spacer h=1/>
					<text pad-x=1>{reason}</text>
					<spacer h=1/>
				</col>
			}
			.into_component()
		},
	}
}
