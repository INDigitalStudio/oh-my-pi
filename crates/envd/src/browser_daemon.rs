//! Supervised named-tab browser daemon over `omp-webview` automation.

use std::{collections::HashMap, sync::Arc, thread, time::Duration};

use async_trait::async_trait;
use flume::Receiver;
use omp_con::Ctx;
use omp_core::{Str, sf};
use omp_tools::browser::{Action, BrowserHost, Fault, Params, Payload, mode_name};
use omp_webview::{Engine, FrameConfig, SurfaceKind, WebView, WebViewBuilder, WindowConfig};
use serde_json::json;

use crate::blobs::BlobHost;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_TIMEOUT: Duration = Duration::from_mins(5);

omp_con::var! {
	/// Enable the browser tool for scripted web automation.
	pub static SV_BROWSER_ENABLED = sv_browser_enabled: bool {
		default: true,
		flags: archive,
	};
	/// Run browser automation offscreen instead of showing a browser window.
	pub static SV_BROWSER_HEADLESS = sv_browser_headless: bool {
		default: true,
		flags: archive,
	};
}

/// Browser-tool availability and presentation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowserSettings {
	/// Enables the browser automation tool.
	pub enabled:  bool,
	/// Uses an offscreen frame surface instead of an engine-owned window.
	pub headless: bool,
}

impl BrowserSettings {
	/// Resolves browser policy from the process control context.
	#[must_use]
	pub fn from_con(ctx: &Ctx) -> Self {
		Self { enabled: SV_BROWSER_ENABLED.get(ctx), headless: SV_BROWSER_HEADLESS.get(ctx) }
	}
}

impl Default for BrowserSettings {
	fn default() -> Self {
		Self { enabled: true, headless: true }
	}
}

#[cfg(test)]
mod settings_tests {
	use super::*;

	#[test]
	fn browser_settings_project_from_con() {
		let ctx = Ctx::new();
		SV_BROWSER_ENABLED.set(&ctx, false).expect("set enabled");
		SV_BROWSER_HEADLESS.set(&ctx, false).expect("set headless");
		assert_eq!(BrowserSettings::from_con(&ctx), BrowserSettings {
			enabled:  false,
			headless: false,
		});
	}
}

enum Request {
	Execute { params: Params, reply: flume::Sender<Result<Payload, Fault>> },
	Restart { headless: bool, reply: flume::Sender<Result<(), Fault>> },
}

/// Process-local browser supervisor. One actor owns every non-`Send` webview
/// handle and tears the complete tab set down when its request channel closes.
pub(crate) struct BrowserDaemon {
	requests: flume::Sender<Request>,
}

impl BrowserDaemon {
	/// Starts one daemon actor with content-addressed artifact storage and its
	/// initial typed surface-mode projection.
	pub(crate) fn start(blobs: BlobHost, settings: BrowserSettings) -> Arc<Self> {
		let (requests, receiver) = flume::unbounded::<Request>();
		thread::Builder::new()
			.name("omp-browser-daemon".to_owned())
			.spawn(move || run(receiver, blobs, settings.headless))
			.expect("browser daemon actor starts");
		Arc::new(Self { requests })
	}
}

#[async_trait]
impl BrowserHost for BrowserDaemon {
	#[tracing::instrument(
		name = "browser_request",
		level = "debug",
		skip_all,
		fields(action = ?params.action, tab = ?params.name),
	)]
	async fn execute(&self, params: Params) -> Result<Payload, Fault> {
		let (reply, response) = flume::bounded(1);
		self
			.requests
			.send_async(Request::Execute { params, reply })
			.await
			.map_err(|_| daemon_closed())?;
		response.recv_async().await.map_err(|_| daemon_closed())?
	}

	async fn restart_for_mode_change(&self, headless: bool) -> Result<(), Fault> {
		let (reply, response) = flume::bounded(1);
		self
			.requests
			.send_async(Request::Restart { headless, reply })
			.await
			.map_err(|_| daemon_closed())?;
		response.recv_async().await.map_err(|_| daemon_closed())?
	}
}

fn run(receiver: Receiver<Request>, blobs: BlobHost, mut headless: bool) {
	tracing::info!(headless, "browser daemon started");
	let mut tabs = HashMap::<Str, WebView>::new();
	while let Ok(request) = receiver.recv() {
		match request {
			Request::Execute { params, reply } => {
				let result = execute(&mut tabs, &blobs, headless, params);
				let _ = reply.send(result);
			},
			Request::Restart { headless: next, reply } => {
				let tabs_closed = tabs.len();
				tabs.clear();
				tracing::info!(
					previous_headless = headless,
					headless = next,
					tabs_closed,
					"browser daemon restarted for mode change",
				);
				headless = next;
				let _ = reply.send(Ok(()));
			},
		}
	}
	tracing::info!(tabs_closed = tabs.len(), "browser daemon stopped");
}

fn execute(
	tabs: &mut HashMap<Str, WebView>,
	_blobs: &BlobHost,
	headless: bool,
	params: Params,
) -> Result<Payload, Fault> {
	validate_supported(&params)?;
	let name = params.name.clone().unwrap_or_else(|| sf!("main"));
	match params.action {
		Action::Open => open(tabs, name, headless, params),
		Action::Close => {
			if params.all {
				tabs.clear();
			} else if tabs.remove(&name).is_none() {
				return Err(not_found(&name));
			}
			Ok(Payload {
				action: Action::Close,
				name,
				url: None,
				title: None,
				display: Vec::new(),
				result: Some(json!({ "remaining_tabs": tabs.len() })),
				artifacts: Vec::new(),
				browser: Some(mode_name(headless)),
			})
		},
		Action::Run => run_tab(tabs, name, headless, params),
	}
}

fn open(
	tabs: &mut HashMap<Str, WebView>,
	name: Str,
	headless: bool,
	params: Params,
) -> Result<Payload, Fault> {
	let surface = if headless {
		SurfaceKind::Frames
	} else {
		SurfaceKind::Window
	};
	let engine = Engine::find(surface).map_err(webview_fault)?;
	let mut builder = WebViewBuilder::new(engine).incognito(true);
	if let Some(url) = params.url.as_ref() {
		builder = builder.url(url.clone());
	}
	let width = params
		.viewport
		.map_or(1280, |viewport| viewport.width)
		.clamp(320, 4096);
	let height = params
		.viewport
		.map_or(800, |viewport| viewport.height)
		.clamp(240, 4096);
	let view = if headless {
		builder
			.build_frames(FrameConfig {
				width,
				height,
				scale: params
					.viewport
					.and_then(|viewport| viewport.scale)
					.unwrap_or(1.0)
					.clamp(0.5, 4.0),
				..FrameConfig::default()
			})
			.map_err(webview_fault)?
	} else {
		builder
			.build_window(WindowConfig { width, height })
			.map_err(webview_fault)?
	};
	let timeout = timeout(&params);
	view
		.automation()
		.wait_for_navigation(timeout)
		.map_err(webview_fault)?;
	let url = view.url();
	let title = view.title();
	tabs.insert(name.clone(), view);
	Ok(Payload {
		action: Action::Open,
		name,
		url: Some(url),
		title: Some(title),
		display: Vec::new(),
		result: None,
		artifacts: Vec::new(),
		browser: Some(mode_name(headless)),
	})
}

fn run_tab(
	tabs: &mut HashMap<Str, WebView>,
	name: Str,
	headless: bool,
	params: Params,
) -> Result<Payload, Fault> {
	let view = tabs.get(&name).ok_or_else(|| not_found(&name))?;
	let tab = view.automation();
	let timeout = timeout(&params);
	if let Some(url) = params.url.as_ref() {
		tab
			.goto(url, timeout)
			.map_err(webview_fault)
			.map_err(|fault| tab_fault(fault, &name, view, headless))?;
	}
	let code = required(params.code.as_deref(), "run requires `code`")
		.map_err(|fault| tab_fault(fault, &name, view, headless))?;
	let evaluated = tab
		.evaluate(&scoped_script(code), timeout)
		.map_err(webview_fault)
		.map_err(|fault| tab_fault(fault, &name, view, headless))?;
	let display = evaluated
		.get("display")
		.and_then(serde_json::Value::as_array)
		.cloned()
		.unwrap_or_default();
	let result = evaluated.get("result").cloned();
	Ok(Payload {
		action: Action::Run,
		name,
		url: Some(view.url()),
		title: Some(view.title()),
		display,
		result,
		artifacts: Vec::new(),
		browser: Some(mode_name(headless)),
	})
}

fn scoped_script(code: &str) -> String {
	format!(
		"(async () => {{ const __ompDisplay = []; const display = value => {{ \
		 __ompDisplay.push(value); }}; const __ompResult = await (async () => {{ {code}\n }})(); \
		 return {{ display: __ompDisplay, result: __ompResult ?? null }}; }})()"
	)
}

fn validate_supported(params: &Params) -> Result<(), Fault> {
	if params.dialogs.is_some() {
		return Err(invalid("this browser backend does not support `dialogs`"));
	}
	if params.kill {
		return Err(invalid("this browser backend does not spawn an external application"));
	}
	if let Some(app) = params.app.as_ref()
		&& (app.path.is_some()
			|| app.cdp_url.is_some()
			|| app.relay == Some(true)
			|| app.args.is_some()
			|| app.target.is_some())
	{
		return Err(invalid(
			"this browser backend does not support `app`; omit it to use the embedded browser",
		));
	}
	Ok(())
}

fn timeout(params: &Params) -> Duration {
	Duration::from_secs_f64(
		params
			.timeout
			.unwrap_or(DEFAULT_TIMEOUT.as_secs_f64())
			.clamp(0.001, MAX_TIMEOUT.as_secs_f64()),
	)
}

fn required<'a>(value: Option<&'a str>, field: &'static str) -> Result<&'a str, Fault> {
	value.ok_or_else(|| invalid(field))
}

fn invalid(message: &'static str) -> Fault {
	Fault {
		code: sf!("invalid_browser_request"),
		message: Str::new_static(message),
		name: None,
		url: None,
		title: None,
		browser: None,
	}
}

fn not_found(name: &str) -> Fault {
	Fault {
		code: sf!("browser_tab_not_found"),
		message: sf!("browser tab `{name}` is not open"),
		name: Some(Str::new(name)),
		url: None,
		title: None,
		browser: None,
	}
}

fn daemon_closed() -> Fault {
	Fault {
		code: sf!("browser_daemon_closed"),
		message: sf!("browser daemon is not available"),
		name: None,
		url: None,
		title: None,
		browser: None,
	}
}

fn webview_fault(error: omp_webview::Error) -> Fault {
	Fault {
		code: sf!("browser_automation_failed"),
		message: Str::new(error.to_string()),
		name: None,
		url: None,
		title: None,
		browser: None,
	}
}

fn tab_fault(mut fault: Fault, name: &Str, view: &WebView, headless: bool) -> Fault {
	fault.name = Some(name.clone());
	fault.url = Some(view.url());
	fault.title = Some(view.title());
	fault.browser = Some(mode_name(headless));
	fault
}
