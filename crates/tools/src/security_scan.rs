//! Long-tail security scan device contract.

use std::{future::Future, sync::Arc};

use async_stream::stream;
use futures::Stream;
use omp_core::{Str, sf};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CommitError, Constraint, DocEffects, Effects, Ev, IncomingParams,
	ParamError, Part, PromptCaps, Rev, Tool, ToolSpec, ToolTerminal,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Security scan operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
	/// Validate and freeze a scan plan.
	Preflight,
	/// Run a frozen plan.
	Start,
	/// Inspect an operation.
	Status,
	/// Cancel a running operation.
	Cancel,
	/// Record a finding validation.
	Validate,
	/// List cloud scan configurations.
	CloudScans,
	/// Start a cloud scan.
	CloudStart,
	/// Inspect a cloud scan.
	CloudStatus,
	/// Import cloud findings.
	CloudPull,
}

/// Repository slice selected for a scan.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
	/// Scan the repository.
	#[default]
	Repository,
	/// Scan explicit include paths.
	ScopedPath,
	/// Scan a revision range.
	RefDiff,
	/// Scan working-tree changes.
	WorkingTree,
}

/// Human validation verdict for one finding.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
	/// No validation has happened.
	Unvalidated,
	/// The finding is confirmed.
	Validated,
	/// The finding is rejected.
	Rejected,
	/// Only part of the finding was confirmed.
	Partial,
	/// Validation could not complete.
	Error,
}

/// Evidence attached to a finding validation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationEvidence {
	/// Non-empty evidence label.
	#[schemars(length(min = 1))]
	pub label:       Str,
	/// Evidence explanation.
	pub explanation: Str,
}

/// Cloud scan lookback selector.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
pub enum LookbackDays {
	/// Positive day count.
	Days(#[schemars(range(min = 1))] u64),
	/// All available history.
	All(AllHistory),
}

/// Literal all-history selector.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
pub enum AllHistory {
	/// Select all available history.
	#[serde(rename = "all")]
	All,
}

/// Full `security_scan@1` operation schema.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
	/// Operation discriminator.
	pub action:                   Action,
	/// Frozen plan identifier.
	#[serde(default)]
	pub plan_id:                  Option<Str>,
	/// Running operation identifier.
	#[serde(default)]
	pub operation_id:             Option<Str>,
	/// Repository slice kind.
	#[serde(default)]
	pub target_kind:              Option<TargetKind>,
	/// Included workspace-relative paths.
	#[serde(default)]
	pub include_paths:            Option<Vec<Str>>,
	/// Excluded workspace-relative paths.
	#[serde(default)]
	pub exclude_paths:            Option<Vec<Str>>,
	/// Base revision for a ref diff.
	#[serde(default)]
	pub base_revision:            Option<Str>,
	/// Head revision for a ref diff.
	#[serde(default)]
	pub head_revision:            Option<Str>,
	/// Additional knowledge-base paths.
	#[serde(default)]
	pub knowledge_base_paths:     Option<Vec<Str>>,
	/// Optional output directory.
	#[serde(default)]
	pub output_root:              Option<Str>,
	/// Archive an existing output directory.
	#[serde(default)]
	pub archive_existing:         Option<bool>,
	/// Authentication-registry credential id.
	#[serde(default)]
	#[schemars(range(min = 1))]
	pub credential_id:            Option<u64>,
	/// Scan identifier containing a finding.
	#[serde(default)]
	pub scan_id:                  Option<Str>,
	/// Finding identifier to validate.
	#[serde(default)]
	pub finding_id:               Option<Str>,
	/// Finding validation verdict.
	#[serde(default)]
	pub validation_status:        Option<ValidationStatus>,
	/// Finding validation summary.
	#[serde(default)]
	pub validation_summary:       Option<Str>,
	/// Finding validation evidence.
	#[serde(default)]
	pub validation_evidence:      Option<Vec<ValidationEvidence>>,
	/// Cloud scan configuration id.
	#[serde(default)]
	pub cloud_configuration_id:   Option<Str>,
	/// Cloud repository id.
	#[serde(default)]
	pub repository_id:            Option<Str>,
	/// Cloud repository URL.
	#[serde(default)]
	pub repository_url:           Option<Str>,
	/// Cloud environment id.
	#[serde(default)]
	pub environment_id:           Option<Str>,
	/// Cloud lookback in days, or `all`.
	#[serde(default)]
	pub lookback_days:             Option<LookbackDays>,
}

/// Durable security operation result.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Payload {
	/// Completed action.
	pub action: Action,
	/// Human-readable result.
	pub output: Str,
	/// Structured operation data.
	pub data:   Value,
}

/// Security device does not stream updates.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Update {}

/// Security authority failure.
#[derive(Clone, Debug, Deserialize, Serialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fault {
	/// Required or mutually dependent arguments are invalid.
	#[error("invalid security scan arguments")]
	InvalidArguments,
	/// The requested plan, operation, scan, or finding does not exist.
	#[error("security scan resource was not found")]
	NotFound,
	/// The requested external backend is not configured.
	#[error("security scan backend is unavailable")]
	Unavailable,
	/// Workspace security state could not be read or persisted.
	#[error("security scan storage failed")]
	Storage,
}

/// Environment-owned security scan authority.
pub trait SecurityScanControl: Clone + Send + Sync + 'static {
	/// Executes one validated operation.
	fn execute(
		&self,
		params: Params,
	) -> impl Future<Output = Result<Payload, Fault>> + Send + '_;
}

/// Frozen security device binding.
pub struct SecurityScan<C> {
	control: C,
	spec:    ToolSpec,
}

/// Returns the host-free `security_scan@1` device specification.
pub fn spec() -> ToolSpec {
	ToolSpec {
		name:            sf!("security_scan"),
		rev:             Rev { family: Str::default(), n: 1 },
		description:     sf!(
			"Plan, run, inspect, cancel, and validate repository security scans; cloud operations are available only when configured."
		),
		schema:          omp_tool::schema::<Params>(),
		constraint:      Constraint::Schema {
			priority:       100,
			on_unsupported: omp_tool::Fallback::Unspecified,
		},
		effects:         Effects {
			documents: Some(DocEffects {
				read:        true,
				write_globs: [sf!("**")].into_iter().collect::<Arc<[_]>>(),
			}),
			exec:      None,
			inference: None,
			desktop:   None,
			subagents: 0,
		},
		projection_code: omp_tool::native_projection_code(
			env!("CARGO_PKG_NAME"),
			env!("CARGO_PKG_VERSION"),
			include_bytes!("security_scan.rs"),
		)
		.into(),
	}
}

/// Constructs the long-tail security scan device.
pub fn tool<C: SecurityScanControl>(control: C) -> SecurityScan<C> {
	SecurityScan { control, spec: spec() }
}

impl<C: SecurityScanControl> Tool for SecurityScan<C> {
	type Fault = Fault;
	type Params = Params;
	type Payload = Payload;
	type Update = Update;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut incoming: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<Update, Payload, Fault>> + Send + 'c {
		stream! {
			let params = match incoming.whole::<Params>().await {
				Ok(params) => params,
				Err(error) => { yield param_event(error); return; },
			};
			if let Err(error) = incoming.interruptable().committed().await {
				yield commit_event(error);
				return;
			}
			yield Ev::Done(ToolTerminal::Done {
				result: self.control.execute(params).await,
				useless: false,
			});
		}
	}

	fn prompt(&self, view: Result<&Payload, &Fault>, _: &PromptCaps) -> Vec<Part> {
		vec![Part::Text {
			text: match view {
				Ok(payload) => payload.output.clone(),
				Err(fault) => Str::from(fault.to_string()),
			},
		}]
	}
}

fn param_event(error: ParamError) -> Ev<Update, Payload, Fault> {
	match error {
		ParamError::Args(issue) => Ev::Args(*issue),
		ParamError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		ParamError::Protocol(message) => Ev::Args(protocol_issue(message)),
	}
}

fn commit_event(error: CommitError) -> Ev<Update, Payload, Fault> {
	match error {
		CommitError::Aborted => Ev::Aborted(Abort::InputDropped),
		CommitError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		CommitError::Protocol(message) => Ev::Args(protocol_issue(message)),
	}
}

fn protocol_issue(message: Str) -> ArgIssue {
	ArgIssue {
		path: Vec::new(),
		expected: sf!("one committed JSON argument object"),
		kind: ArgIssueKind::Protocol,
		example: None,
		found: Some(message),
	}
}

#[cfg(test)]
mod tests {
	use std::{future, sync::{Arc, Mutex}};

	use futures::StreamExt as _;

	use super::*;

	#[derive(Clone, Default)]
	struct Recording(Arc<Mutex<Option<Action>>>);

	impl SecurityScanControl for Recording {
		fn execute(
			&self,
			params: Params,
		) -> impl Future<Output = Result<Payload, Fault>> + Send + '_ {
			self.0.lock().expect("recording").replace(params.action);
			future::ready(Ok(Payload {
				action: params.action,
				output: sf!("ok"),
				data: serde_json::json!({}),
			}))
		}
	}

	#[test]
	fn schema_contains_the_complete_pi_operation_surface() {
		let schema: Value = serde_json::from_slice(&spec().schema).expect("security schema");
		assert_eq!(schema["additionalProperties"], false);
		assert_eq!(schema["required"], serde_json::json!(["i", "action"]));
		assert_eq!(
			schema["properties"].as_object().expect("properties").keys()
				.map(String::as_str)
				.collect::<std::collections::BTreeSet<_>>(),
			[
				"action", "archive_existing", "base_revision", "cloud_configuration_id",
				"credential_id", "environment_id", "exclude_paths", "finding_id", "head_revision",
				"i", "include_paths", "knowledge_base_paths", "lookback_days", "notrunc",
				"operation_id", "output_root", "plan_id", "repository_id", "repository_url",
				"scan_id", "target_kind", "validation_evidence", "validation_status",
				"validation_summary",
			]
			.into_iter()
			.collect()
		);
		assert!(schema["properties"]["action"].is_object());
		for action in [
			"preflight",
			"start",
			"status",
			"cancel",
			"validate",
			"cloud_scans",
			"cloud_start",
			"cloud_status",
			"cloud_pull",
		] {
			assert!(serde_json::from_value::<Action>(serde_json::json!(action)).is_ok());
		}
	}

	#[tokio::test]
	async fn device_routes_committed_operations_to_the_authority() {
		let control = Recording::default();
		let security = tool(control.clone());
		let raw = r#"{"action":"preflight","target_kind":"repository"}"#;
		let (feed, incoming) = IncomingParams::channel();
		feed.arg_text(raw.into()).expect("stream args");
		feed.args_committed(raw.into()).expect("commit args");
		let events = security.call(incoming).collect::<Vec<_>>().await;
		assert!(matches!(
			events.last(),
			Some(Ev::Done(ToolTerminal::Done { result: Ok(_), .. }))
		));
		assert_eq!(*control.0.lock().expect("recording"), Some(Action::Preflight));
	}
}
