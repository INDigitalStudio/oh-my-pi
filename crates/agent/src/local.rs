//! One tool call outside a model turn: the interactive `!` (bash) and `$`
//! (eval) prefix modes.
//!
//! pi runs those locally through the same tool the model would call and
//! never asks the provider (`command-controller.ts handleBashCommand` /
//! `handlePythonCommand`). Here the run is a journaled `<turn>` holding only
//! the tool element (ADR 0008: the element state stream is the record; ADR
//! 0028: the in-process `bash` tool is the executor), so the transcript card,
//! replay, and rewind treat it exactly like a model-issued call, and the
//! thread projection shows the model what ran (pi `bashExecutionToText`).

use omp_core::{Str, Ulid};
use omp_dom::{Op, PropKey, Txn, Value};
use omp_session::{
	Session,
	projection::{LOCAL_CONTEXT_EXCLUDED, LOCAL_CONTEXT_PROP},
};
use omp_tool::RegistryError;
use serde_json::value::RawValue;

use crate::{
	Inference, Kernel, KernelError, KernelEvent, RunControl, TurnOutcome, TurnStop,
	loop_::{ReadyCall, cancelled_outcome, current_turn, outcome},
};

/// One tool invocation the host asked to run without inference.
#[derive(Clone, Debug)]
pub struct LocalRun {
	/// Registered tool name (`bash`, `eval`).
	pub name:    Str,
	/// Canonical JSON arguments.
	pub args:    Box<RawValue>,
	/// Intent shown on the card (`i`).
	pub intent:  Option<Str>,
	/// Keep the run out of the model's context.
	pub exclude: bool,
}

impl<C: Inference> Kernel<C> {
	/// Runs one tool call as its own turn: no user message, no inference,
	/// just the journaled element and its outcome. Interrupts and session
	/// cancellation reach the tool through the same scopes a model-issued
	/// call observes.
	pub async fn run_local(
		&mut self,
		session: &mut Session,
		run: LocalRun,
		control: RunControl,
	) -> Result<TurnOutcome, KernelError> {
		if control.is_expired() || self.cancel.is_session_cancelled() {
			return Ok(cancelled_outcome());
		}
		let identity = self
			.dispatcher
			.registry()
			.resolved_identity(run.name.as_str())
			.ok_or_else(|| RegistryError::UnknownTool(run.name.clone()))?;
		let turn_cancel = self.cancel.begin_turn();
		session.begin_turn()?;
		self.apply_live_components(session)?;
		let call_id = Str::new(format!("local-{}", Ulid::generate()));
		let entry = session.call(
			run.name.clone(),
			crate::journal_revision(&identity.rev),
			call_id.clone(),
			run.intent,
			Some(run.args.clone()),
			None,
		)?;
		self.apply_live_components(session)?;
		if run.exclude {
			let turn = current_turn(session)?;
			let Some(element) = session.dom().children(turn).last().copied() else {
				return Err(KernelError::MissingResponseStart);
			};
			session.patch(Txn {
				cause: entry,
				label: Some(Str::new_static("local.exclude")),
				ops:   vec![Op::Set {
					h:     element,
					prop:  PropKey::Custom(Str::new_static(LOCAL_CONTEXT_PROP)),
					value: Value::Str(Str::new_static(LOCAL_CONTEXT_EXCLUDED)),
				}],
			})?;
		}
		self.events.publish(KernelEvent::ToolReady {
			call_id: call_id.clone(),
			name:    identity.name.clone(),
		});
		let mut steering = Vec::new();
		let dispatched = self
			.dispatch_call(
				session,
				ReadyCall { entry, identity, call_id, args: run.args },
				&turn_cancel,
				&control,
				&mut steering,
			)
			.await;
		// Steering typed while a local command runs has no inference to
		// land in; it goes back to the mailbox for the next model turn.
		for (text, attachments) in steering {
			let _ = self.mailbox_tx.send(crate::Up::Steer { text, attachments });
		}
		let stop = match dispatched {
			Ok(_) if turn_cancel.is_turn_cancelled() => TurnStop::Cancelled,
			Ok(_) => TurnStop::Completed,
			Err(_) => TurnStop::Failed,
		};
		self.events.publish(KernelEvent::TurnEnded { stop });
		dispatched.map(|_| outcome(stop, String::new(), 0, 0))
	}
}
