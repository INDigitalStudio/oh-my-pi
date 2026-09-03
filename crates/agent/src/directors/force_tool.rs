//! Forced-tool Director and its bounded escalation ladder.

use omp_core::Str;
use omp_dom::{Dom, Node};
use omp_inference::{ChatRequest, ForcedCall, Setting, ToolChoice};

use crate::director::{
	BindValue, Director, DirectorCx, DirectorEffect, ForceUntil, StateUpdate, TurnView, Verdict,
	state_bool, state_int, state_str, turn_called,
};

/// Requires a named tool call before its parent may inspect the yield.
pub struct ForceTool {
	name:     Str,
	until:    ForceUntil,
	reminder: Option<Str>,
	retries:   u32,
	attempts:  u32,
	satisfied: bool,
}

impl ForceTool {
	/// Creates a bounded forced-call engagement.
	#[must_use]
	pub fn new(
		name: impl Into<Str>,
		until: ForceUntil,
		reminder: Option<Str>,
		retries: u32,
	) -> Self {
		Self { name: name.into(), until, reminder, retries, attempts: 0, satisfied: false }
	}

	/// Reconstructs a forced-call engagement from DOM properties.
	#[must_use]
	pub fn from_node(node: &Node) -> Self {
		let name = state_str(node, "tool").unwrap_or_else(|| Str::new_static("required"));
		let until = match state_str(node, "until").as_deref() {
			Some("*") | None => ForceUntil::AnyToolCall,
			Some(tool) => ForceUntil::ToolCalled(Str::new(tool)),
		};
		let reminder = state_str(node, "reminder").filter(|value| !value.is_empty());
		let retries = state_int(node, "retries")
			.and_then(|value| u32::try_from(value).ok())
			.unwrap_or(3);
		let attempts = state_int(node, "attempts")
			.and_then(|value| u32::try_from(value).ok())
			.unwrap_or(0);
		let satisfied = state_bool(node, "satisfied").unwrap_or(false);
		Self { name, until, reminder, retries, attempts, satisfied }
	}

	fn turn_satisfies(&self, dom: &Dom, turn: &TurnView) -> bool {
		match &self.until {
			ForceUntil::AnyToolCall => turn.had_tool_calls,
			ForceUntil::ToolCalled(tool) => turn_called(dom, turn.turn, tool),
		}
	}
}

impl Director for ForceTool {
	fn id(&self) -> &'static str {
		"force_tool"
	}

	fn claims(&self) -> &'static [crate::director::Slot] {
		&[crate::director::Slot::ToolChoice]
	}

	fn state(&self) -> Vec<(Str, BindValue)> {
		vec![
			(Str::new_static("tool"), BindValue::Str(self.name.clone())),
			(
				Str::new_static("until"),
				BindValue::Str(match &self.until {
					ForceUntil::AnyToolCall => Str::new_static("*"),
					ForceUntil::ToolCalled(tool) => tool.clone(),
				}),
			),
			(Str::new_static("reminder"), BindValue::Str(self.reminder.clone().unwrap_or_default())),
			(Str::new_static("retries"), BindValue::Int(i64::from(self.retries))),
			(Str::new_static("attempts"), BindValue::Int(i64::from(self.attempts))),
			(Str::new_static("satisfied"), BindValue::Bool(self.satisfied)),
		]
	}

	fn prepare_inference(&self, _cx: &DirectorCx<'_>, req: &mut ChatRequest) {
		// This is semantic intent only. Inference owns the soft/native/costly
		// translation and receipts each rung (ADRs 0016 and 0019).
		req.tool_choice = Setting::Require(ToolChoice::Named(self.name.clone()));
		req.forced_call = Some(ForcedCall {
			non_compliant_turns: u8::try_from(self.attempts).unwrap_or(u8::MAX),
			escalations_left:    u8::from(self.attempts >= self.retries),
		});
	}

	fn observe_turn(&self, dom: &Dom, _cx: &DirectorCx<'_>, turn: &TurnView) -> Vec<StateUpdate> {
		if self.satisfied || !self.turn_satisfies(dom, turn) {
			return Vec::new();
		}
		vec![StateUpdate::new("satisfied", BindValue::Bool(true))]
	}

	fn evaluate(&self, dom: &Dom, _cx: &DirectorCx<'_>, turn: &TurnView) -> DirectorEffect {
		if self.satisfied || self.turn_satisfies(dom, turn) {
			return DirectorEffect::new(Verdict::Done);
		}
		if self.attempts >= self.retries {
			return DirectorEffect::new(Verdict::Fail(Str::new(format!(
				"model did not call required tool {} after {} retries",
				self.name, self.retries
			))));
		}
		let reminder = self.reminder.clone().or_else(|| {
			Some(Str::new(format!("Call {} now; do not answer without using it.", self.name)))
		});
		DirectorEffect {
			verdict: Verdict::Continue { reminder },
			updates: vec![StateUpdate::new(
				"attempts",
				BindValue::Int(i64::from(self.attempts.saturating_add(1))),
			)],
			asides:  Vec::new(),
		}
	}
}
