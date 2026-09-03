//! Stateful browser automation over a harness-owned supervised daemon.

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::Stream;
use omp_core::{Str, sf};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CommitError, Constraint, Effects, Ev, ExecEffects,
	IncomingParams, ParamError, Part, PromptCaps, Rev, Tool, ToolSpec, ToolTerminal,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Browser lifecycle operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Action {
	/// Create or replace a named tab.
	Open,
	/// Execute one automation operation in a named tab.
	Run,
	/// Close one named tab or every tab.
	Close,
}

/// Browser application attachment or launch configuration.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct App {
	/// Binary path to spawn.
	pub path:    Option<Str>,
	/// Existing Chrome DevTools Protocol endpoint.
	pub cdp_url: Option<Str>,
	/// Drive the user's own browser through the relay.
	pub relay:   Option<bool>,
	/// Extra application arguments.
	pub args:    Option<Vec<Str>>,
	/// Window title or URL substring used to select a target.
	pub target:  Option<Str>,
}

/// Browser viewport configuration.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
	/// Width in CSS pixels.
	pub width:  u32,
	/// Height in CSS pixels.
	pub height: u32,
	/// Device scale factor.
	pub scale:  Option<f64>,
}

/// Navigation completion condition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum WaitUntil {
	/// Window load event.
	Load,
	/// DOM content loaded event.
	Domcontentloaded,
	/// No active network requests.
	Networkidle0,
	/// At most two active network requests.
	Networkidle2,
}

/// JavaScript dialog handling policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Dialogs {
	/// Accept dialogs.
	Accept,
	/// Dismiss dialogs.
	Dismiss,
}

/// Browser tool arguments.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
	/// Lifecycle action.
	pub action:                  Action,
	/// Stable tab name; defaults to `main`.
	pub name:                    Option<Str>,
	/// Initial or navigated URL.
	pub url:                     Option<Str>,
	/// Browser process, CDP, or relay configuration for `open`.
	pub app:                     Option<App>,
	/// Viewport dimensions for `open`.
	pub viewport:                Option<Viewport>,
	/// Navigation completion condition.
	pub wait_until:              Option<WaitUntil>,
	/// Automatic JavaScript dialog handling.
	pub dialogs:                 Option<Dialogs>,
	/// JavaScript body evaluated by `run` against the persistent named tab.
	pub code:                    Option<Str>,
	/// Bounded operation timeout in seconds.
	pub timeout:                 Option<f64>,
	/// Close every managed tab.
	#[serde(default)]
	pub all:                     bool,
	/// Also terminate spawned browser processes while closing.
	#[serde(default)]
	pub kill:                    bool,
	/// Private host-control signal used by `/browser` after persisting a mode
	/// change. This is intentionally absent from the model-facing schema.
	#[serde(default)]
	#[schemars(skip)]
	#[doc(hidden)]
	pub restart_for_mode_change: Option<bool>,
}

/// Browser operation result.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Payload {
	/// Completed lifecycle action.
	pub action:    Action,
	/// Stable tab name.
	pub name:      Str,
	/// Current committed URL, when a tab remains open.
	pub url:       Option<Str>,
	/// Current document title, when available.
	pub title:     Option<Str>,
	/// Values explicitly emitted through the run scope's `display(value)`.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub display:   Vec<Value>,
	/// JSON value returned by the run scope.
	pub result:    Option<Value>,
	/// Content-addressed artifacts created by the operation.
	pub artifacts: Vec<Str>,
	/// Backend mode the tab runs under (`headless` or `window`); pi's
	/// `describeBrowser` meta. Absent on payloads journaled before it existed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub browser:   Option<Str>,
}

/// Human name of a backend mode for [`Payload::browser`].
#[must_use]
pub fn mode_name(headless: bool) -> Str {
	if headless {
		Str::new_static("headless")
	} else {
		Str::new_static("window")
	}
}

/// Browser daemon failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct Fault {
	/// Stable failure category.
	pub code:    Str,
	/// Secret-free diagnostic.
	pub message: Str,
	/// Stable tab name when failure happened after tab lookup.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name:    Option<Str>,
	/// Current committed URL when available.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub url:     Option<Str>,
	/// Current document title when available.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub title:   Option<Str>,
	/// Backend mode when known.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub browser: Option<Str>,
}

/// Browser operations currently settle as one bounded result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Update {}

/// Harness-owned browser daemon contract.
#[async_trait]
pub trait BrowserHost: Send + Sync + 'static {
	/// Execute one lifecycle operation.
	async fn execute(&self, params: Params) -> Result<Payload, Fault>;
	/// Drop live browser surfaces and apply a new headless/windowed mode.
	async fn restart_for_mode_change(&self, headless: bool) -> Result<(), Fault>;
}

/// Browser tool routed to one supervised daemon.
pub struct Browser {
	host: Arc<dyn BrowserHost>,
	spec: ToolSpec,
}

/// Builds the host-free `browser@2` declaration.
pub fn spec() -> ToolSpec {
	ToolSpec {
		name:            sf!("browser"),
		rev:             Rev { family: Str::default(), n: 2 },
		description:     sf!(
			"Controls named tabs through the supervised embedded browser daemon. Use open before run \
			 and close when finished."
		),
		schema:          omp_tool::schema::<Params>(),
		constraint:      Constraint::Schema {
			priority:       100,
			on_unsupported: omp_tool::Fallback::Unspecified,
		},
		effects:         Effects {
			documents: None,
			exec:      Some(ExecEffects { commands: Arc::default(), network: true }),
			inference: None,
			desktop:   None,
			subagents: 0,
		},
		projection_code: omp_tool::native_projection_code(
			env!("CARGO_PKG_NAME"),
			env!("CARGO_PKG_VERSION"),
			include_bytes!("browser.rs"),
		)
		.into(),
	}
}

/// Creates `browser@2`.
pub fn tool(host: Arc<dyn BrowserHost>) -> Browser {
	Browser { host, spec: spec() }
}

impl Tool for Browser {
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
			if let Some(headless) = params.restart_for_mode_change {
				let name = params.name.clone().unwrap_or_else(|| sf!("main"));
				let result = self.host.restart_for_mode_change(headless).await.map(|()| Payload {
					action: Action::Close,
					name,
					url: None,
					title: None,
					display: Vec::new(),
					result: Some(json!({ "headless": headless })),
					artifacts: Vec::new(),
					browser: Some(mode_name(headless)),
				});
				yield Ev::Done(ToolTerminal::Done { result, useless: false });
				return;
			}
			yield Ev::Done(ToolTerminal::Done { result: self.host.execute(params).await, useless: false });
		}
	}

	fn prompt(&self, view: Result<&Payload, &Fault>, _: &PromptCaps) -> Vec<Part> {
		vec![Part::Text {
			text: match view {
				Ok(payload) => {
					Str::new(serde_json::to_string(payload).expect("browser payload serializes"))
				},
				Err(fault) => fault.message.clone(),
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
		path:     Vec::new(),
		expected: sf!("one committed browser argument object"),
		kind:     ArgIssueKind::Protocol,
		example:  None,
		found:    Some(message),
	}
}

#[cfg(test)]
mod tests {
	use serde_json::{Value, json};

	use super::{Action, Params, spec};

	#[test]
	fn browser_schema_keeps_only_open_run_close_code_surface() {
		let schema: Value = serde_json::from_slice(&spec().schema).expect("browser schema");
		let properties = schema["properties"].as_object().expect("object properties");
		let mut domain = properties
			.keys()
			.filter(|name| !matches!(name.as_str(), "i" | "notrunc"))
			.map(String::as_str)
			.collect::<Vec<_>>();
		domain.sort_unstable();
		assert_eq!(domain, [
			"action",
			"all",
			"app",
			"code",
			"dialogs",
			"kill",
			"name",
			"timeout",
			"url",
			"viewport",
			"wait_until",
		]);
		assert!(!properties.contains_key("operation"));
		assert!(!properties.contains_key("selector"));
		assert!(!properties.contains_key("full_page"));
		assert!(properties["action"].is_object());
		for action in ["open", "run", "close"] {
			assert!(serde_json::from_value::<Action>(json!(action)).is_ok());
		}
	}

	#[test]
	fn browser_code_schema_accepts_reference_oracle_arguments() {
		let params: Params = serde_json::from_value(json!({
			"action": "open",
			"name": "main",
			"url": "https://example.test",
			"app": {
				"path": "/Applications/Browser.app/Contents/MacOS/Browser",
				"relay": false,
				"args": ["--incognito"],
				"target": "Example"
			},
			"viewport": { "width": 1280, "height": 800, "scale": 2.0 },
			"wait_until": "networkidle2",
			"dialogs": "dismiss",
			"timeout": 10.5,
			"all": false,
			"kill": false
		}))
		.expect("reference browser arguments");
		assert_eq!(params.action, Action::Open);
		assert_eq!(params.viewport.expect("viewport").width, 1280);
	}
}
