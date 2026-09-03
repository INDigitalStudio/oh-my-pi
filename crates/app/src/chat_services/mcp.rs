//! `/mcp`: pi's `MCPCommandController` over the Environment's MCP
//! authorities — the persisted config stores (`~/.o2/mcp.json`,
//! `.omp/mcp.json`, `.mcp.json`) for `add`/`remove`/`enable`/`disable`,
//! the live manager for `list`/`test`/`reconnect`/`reload`/`resources`/
//! `prompts`/`notifications`, and the OAuth authority for
//! `reauth`/`unauth`. Every operation settles a report line on a pending
//! receiver so the host's loader panel never blocks.

use std::{fmt::Write as _, path::Path, time::Duration};

use omp_chat::overlays::services::{McpAdd, McpOp, McpRun, McpScope, ServiceError, ServiceResult};
use omp_core::{Str, dirs::DataDirError, sf};
use omp_envd::mcp::{
	McpConfigPaths,
	config::{McpServerConfig, TransportKind},
	config_store::{McpConfigStore, set_server_enabled},
	manager::{McpInspectorHealth, McpInspectorSnapshot},
};

use super::ServiceState;

/// pi waits this long for `/mcp test` before giving up on a server.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Poll cadence while a reconnecting server settles.
const TEST_POLL: Duration = Duration::from_millis(200);
/// pi lists tool names only up to this many.
const LISTED_TOOLS: usize = 10;

fn failed(error: impl std::fmt::Display) -> ServiceError {
	ServiceError::failed(error)
}

/// The three MCP config files `/mcp` reads and mutates: the same
/// [`McpConfigPaths`] the Environment and `omp config mcp` address, rooted at
/// the user configuration root (`~/.o2`, profile-aware) — never the data
/// directory.
///
/// # Errors
///
/// Returns [`DataDirError::HomeUnset`] when no home directory is set.
pub fn mcp_config_paths(project: &Path) -> Result<McpConfigPaths, DataDirError> {
	Ok(McpConfigPaths::new(&omp_core::dirs::user_config_root()?, project))
}

pub(super) fn stores(
	state: &ServiceState,
) -> ServiceResult<(McpConfigStore, McpConfigStore, McpConfigStore)> {
	let paths = mcp_config_paths(&state.project).map_err(failed)?;
	Ok((
		McpConfigStore::new(paths.user),
		McpConfigStore::new(paths.project),
		McpConfigStore::new(paths.root),
	))
}

fn store_for(state: &ServiceState, scope: McpScope) -> ServiceResult<McpConfigStore> {
	let paths = mcp_config_paths(&state.project).map_err(failed)?;
	Ok(McpConfigStore::new(match scope {
		McpScope::User => paths.user,
		McpScope::Project => paths.project,
	}))
}

/// Runs one operation; synchronous config edits settle immediately, live
/// manager operations run on the app runtime.
pub(super) fn run(state: &ServiceState, op: McpOp) -> ServiceResult<McpRun> {
	let (tx, rx) = flume::bounded(1);
	match op {
		McpOp::List => {
			let _ = tx.send(list(state));
		},
		McpOp::Add(add) => {
			let _ = tx.send(add_server(state, &add));
		},
		McpOp::Remove(name, scope) => {
			let _ = tx.send(remove_server(state, &name, scope));
		},
		McpOp::SetEnabled(name, enabled) => {
			let _ = tx.send(set_enabled(state, &name, enabled));
		},
		McpOp::Resources => {
			let _ = tx.send(Ok(resources(state)));
		},
		McpOp::Prompts => {
			let _ = tx.send(Ok(prompts(state)));
		},
		McpOp::Notifications => {
			let _ = tx.send(Ok(notifications(state)));
		},
		McpOp::Test(name) => {
			let (cancel_tx, cancel_rx) = flume::bounded::<()>(1);
			let mcp = state.mcp.clone();
			let declared = declared_config(state, &name);
			state.runtime.spawn(async move {
				let test = test_server(&mcp, &name, declared);
				let cancelled = cancel_rx.recv_async();
				let result = tokio::select! {
					result = test => result,
					_ = cancelled => Err(ServiceError::Failed(sf!("Cancelled MCP test for \"{name}\""))),
				};
				let _ = tx.send(result);
			});
			return Ok(McpRun { done: rx, cancel: Some(cancel_tx) });
		},
		McpOp::Reconnect(name) => {
			let mcp = state.mcp.clone();
			state.runtime.spawn(async move {
				let result = match mcp.reconnect(&name).await {
					Ok(()) => Ok(sf!("Reconnected to \"{name}\".")),
					Err(error) => Err(ServiceError::Failed(sf!(
						"Failed to reconnect to \"{name}\": {error}. Check server status and logs."
					))),
				};
				let _ = tx.send(result);
			});
		},
		McpOp::Reload => {
			let mcp = state.mcp.clone();
			state.runtime.spawn(async move {
				let result = match mcp.reload().await {
					Ok(snapshot) => {
						let connected = mcp
							.snapshots()
							.iter()
							.filter(|server| server.health == McpInspectorHealth::Connected)
							.count();
						let _ = snapshot;
						Ok(sf!("MCP reload complete\n  Connected servers: {connected}"))
					},
					Err(error) => Err(ServiceError::Failed(sf!("Failed to reload MCP: {error}"))),
				};
				let _ = tx.send(result);
			});
		},
		McpOp::Reauth(name) => {
			let mcp = state.mcp.clone();
			let con = std::sync::Arc::clone(&state.con);
			let declared = declared_config(state, &name);
			state.runtime.spawn(async move {
				let result = match declared {
					None => Err(ServiceError::Failed(sf!("Server \"{name}\" not found."))),
					Some(config) if !config.enabled => Err(ServiceError::Failed(sf!(
						"Server \"{name}\" is disabled. Run /mcp enable {name} first."
					))),
					Some(_) => match mcp
						.reauthorize(&name, |url| {
							// The URL reaches the actor through the console reply sink
							// (a status notice) while the grant waits for the browser.
							con.reply(
								omp_con::Severity::Info,
								&format!("Authorize \"{name}\" in your browser: {url}"),
							);
						})
						.await
					{
						Ok(true) => Ok(sf!("Reauthorized \"{name}\".")),
						Ok(false) => Ok(sf!("Server \"{name}\" does not use OAuth.")),
						Err(error) => {
							Err(ServiceError::Failed(sf!("Failed to reauthorize server: {error}")))
						},
					},
				};
				let _ = tx.send(result);
			});
		},
		McpOp::Unauth(name) => {
			let mcp = state.mcp.clone();
			let declared = declared_config(state, &name);
			state.runtime.spawn(async move {
				let result = match declared {
					None => Err(ServiceError::Failed(sf!("Server \"{name}\" not found."))),
					Some(_) => match mcp.clear_authorization(&name).await {
						Ok(true) => Ok(sf!("Cleared auth for \"{name}\".")),
						Ok(false) => Ok(sf!("No stored auth for \"{name}\".")),
						Err(error) => Err(ServiceError::Failed(sf!("Failed to clear auth: {error}"))),
					},
				};
				let _ = tx.send(result);
			});
		},
	}
	Ok(McpRun { done: rx, cancel: None })
}

/// The declaration for `name` from the first store that has it.
fn declared_config(state: &ServiceState, name: &str) -> Option<McpServerConfig> {
	let (user, project, root) = stores(state).ok()?;
	[project, root, user]
		.iter()
		.find_map(|store| store.get(name).ok().flatten())
}

/// pi `#handleList`: user-level, project-level, then discovered servers,
/// each with its connection state.
fn list(state: &ServiceState) -> ServiceResult<Str> {
	let (user, project, root) = stores(state)?;
	let live = state.mcp.snapshots();
	let health = |name: &str| {
		live
			.iter()
			.find(|server| server.server == name)
			.map(|server| server.health)
	};
	let mut out = String::new();
	let mut declared = std::collections::BTreeSet::new();
	for (label, store) in
		[("User level", &user), ("Project level", &project), ("Project root", &root)]
	{
		let Ok(file) = store.read() else { continue };
		if file.mcp_servers.is_empty() {
			continue;
		}
		let _ = writeln!(out, "{label} ({}):", shorten(store.path(), &state.project));
		for (name, config) in &file.mcp_servers {
			declared.insert(name.clone());
			let kind = match config.resolved_transport() {
				TransportKind::Stdio => "stdio",
				_ => "http",
			};
			let status = if !config.enabled {
				"◌ inactive"
			} else {
				status_label(health(name))
			};
			let _ = writeln!(out, "  {name} {status} [{kind}]");
		}
		out.push('\n');
	}
	let discovered = live
		.iter()
		.filter(|server| !declared.contains(&server.server))
		.collect::<Vec<_>>();
	if !discovered.is_empty() {
		out.push_str("Discovered (extension-mounted):\n");
		for server in discovered {
			let _ = writeln!(out, "  {} {}", server.server, status_label(Some(server.health)));
		}
		out.push('\n');
	}
	if out.is_empty() {
		return Ok(Str::new_static(
			"No MCP servers configured. Add one with /mcp add <name> -- <command>.",
		));
	}
	Ok(Str::new(out.trim_end()))
}

const fn status_label(health: Option<McpInspectorHealth>) -> &'static str {
	match health {
		Some(McpInspectorHealth::Connected) => "● connected",
		Some(McpInspectorHealth::Connecting) => "◌ connecting",
		Some(McpInspectorHealth::Failed) => "○ failed",
		Some(McpInspectorHealth::Disconnected) | None => "○ not connected",
	}
}

fn shorten(path: &Path, project: &Path) -> String {
	path
		.strip_prefix(project)
		.map_or_else(|_| path.display().to_string(), |rest| rest.display().to_string())
}

/// pi `#handleAdd` (non-interactive form): validate, write, report.
fn add_server(state: &ServiceState, add: &McpAdd) -> ServiceResult<Str> {
	let store = store_for(state, add.scope)?;
	let mut config = McpServerConfig {
		transport:         None,
		enabled:           true,
		command:           None,
		args:              Vec::new(),
		env:               Default::default(),
		env_policy:        None,
		cwd:               None,
		url:               None,
		headers:           Default::default(),
		header_policy:     None,
		timeout:           None,
		request_id_format: None,
		auth:              None,
		oauth:             None,
		protocol_versions: Vec::new(),
	};
	if let Some(url) = &add.url {
		config.url = Some(url.clone());
	} else if let Some((command, args)) = add.command.split_first() {
		config.command = Some(command.clone());
		config.args = args.to_vec();
	}
	store
		.add(&add.name, config)
		.map_err(|error| ServiceError::Failed(sf!("Failed to add server: {error}")))?;
	schedule_reload(state);
	Ok(sf!(
		"Added MCP server \"{}\" to {} config ({}).",
		add.name,
		add.scope,
		shorten(store.path(), &state.project)
	))
}

/// pi `#handleRemove`.
fn remove_server(state: &ServiceState, name: &str, scope: McpScope) -> ServiceResult<Str> {
	let store = store_for(state, scope)?;
	if store.get(name).map_err(failed)?.is_none() {
		return Err(ServiceError::Failed(sf!("Server \"{name}\" not found in {scope} config.")));
	}
	store
		.remove(name)
		.map_err(|error| ServiceError::Failed(sf!("Failed to remove server: {error}")))?;
	schedule_reload(state);
	Ok(sf!("Removed MCP server \"{name}\" from {scope} config."))
}

/// pi `#handleSetEnabled`.
fn set_enabled(state: &ServiceState, name: &str, enabled: bool) -> ServiceResult<Str> {
	let known = declared_config(state, name).is_some()
		|| state
			.mcp
			.snapshots()
			.iter()
			.any(|server| server.server == name);
	if !known {
		return Err(ServiceError::Failed(sf!("Server \"{name}\" not found.")));
	}
	let (user, project, root) = stores(state)?;
	set_server_enabled(&user, &project, Some((&root, true)), name, enabled).map_err(|error| {
		ServiceError::Failed(sf!(
			"Failed to {} server: {error}",
			if enabled { "enable" } else { "disable" }
		))
	})?;
	schedule_reload(state);
	Ok(sf!("{} MCP server \"{name}\".", if enabled { "Enabled" } else { "Disabled" }))
}

/// Config edits become live through a background reload.
fn schedule_reload(state: &ServiceState) {
	let mcp = state.mcp.clone();
	state.runtime.spawn(async move {
		if let Err(error) = mcp.reload().await {
			tracing::warn!(%error, "MCP reload after config edit failed");
		}
	});
}

/// pi `#handleTest`: reconnect and wait for the catalog, then report the
/// server and its tools.
async fn test_server(
	mcp: &omp_envd::McpInspectorHandle,
	name: &str,
	declared: Option<McpServerConfig>,
) -> ServiceResult<Str> {
	if let Some(config) = &declared
		&& !config.enabled
	{
		return Err(ServiceError::Failed(sf!(
			"Server \"{name}\" is disabled. Run /mcp enable {name} first."
		)));
	}
	let mounted = mcp.snapshots().iter().any(|server| server.server == name);
	if declared.is_none() && !mounted {
		return Err(ServiceError::Failed(sf!(
			"Server \"{name}\" not found.\n\nTip: Run /mcp list to see available servers."
		)));
	}
	if let Err(error) = mcp.reconnect(name).await {
		return Err(ServiceError::Failed(sf!(
			"Failed to connect to \"{name}\": {error}{}",
			tip(&error.to_string())
		)));
	}
	let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
	loop {
		let snapshot = mcp
			.snapshots()
			.into_iter()
			.find(|server| server.server == name);
		match snapshot {
			Some(server) if server.health == McpInspectorHealth::Connected => {
				return Ok(test_report(name, &server));
			},
			Some(server) if server.health == McpInspectorHealth::Failed => {
				return Err(ServiceError::Failed(sf!(
					"Failed to connect to \"{name}\": the server reported a failure. Check its logs."
				)));
			},
			_ if tokio::time::Instant::now() >= deadline => {
				return Err(ServiceError::Failed(sf!(
					"Failed to connect to \"{name}\": timeout\n\nTip: The server may be slow or \
					 unresponsive. Try increasing the timeout."
				)));
			},
			_ => tokio::time::sleep(TEST_POLL).await,
		}
	}
}

/// pi's helpful error suffixes.
fn tip(message: &str) -> &'static str {
	if message.contains("ENOENT") || message.contains("not found") {
		"\n\nTip: Check that the command or URL is correct."
	} else if message.contains("EACCES") {
		"\n\nTip: Check file/command permissions."
	} else if message.contains("ECONNREFUSED") {
		"\n\nTip: Check that the server is running and the URL/port is correct."
	} else if message.contains("timeout") {
		"\n\nTip: The server may be slow or unresponsive. Try increasing the timeout."
	} else if message.contains("401") || message.contains("403") {
		"\n\nTip: Check your authentication credentials."
	} else {
		""
	}
}

fn test_report(name: &str, server: &McpInspectorSnapshot) -> Str {
	let mut out = format!(
		"✓ Successfully connected to \"{name}\"\n\n  Server: {} v{}\n  Tools: {}",
		server.implementation.as_deref().unwrap_or(name),
		server.version.as_deref().unwrap_or("?"),
		server.tools.len()
	);
	if !server.tools.is_empty() && server.tools.len() <= LISTED_TOOLS {
		out.push_str("\n\n  Available tools:");
		for tool in server.tools.iter() {
			if let Some(tool) = tool.get("name").and_then(|name| name.as_str()) {
				let _ = write!(out, "\n    • {tool}");
			}
		}
	}
	Str::new(out)
}

/// pi `#handleResources`.
fn resources(state: &ServiceState) -> Str {
	let mut out = String::new();
	for server in state.mcp.snapshots() {
		if server.resources.is_empty() {
			continue;
		}
		let _ = writeln!(out, "{}:", server.server);
		for resource in server.resources.iter() {
			let _ = writeln!(out, "  {} — {}", resource.uri, resource.name);
		}
	}
	if out.is_empty() {
		return Str::new_static("No resources available from connected servers.");
	}
	Str::new(out.trim_end())
}

/// pi `#handlePrompts`.
fn prompts(state: &ServiceState) -> Str {
	let mut out = String::new();
	for server in state.mcp.snapshots() {
		if server.prompts.is_empty() {
			continue;
		}
		let _ = writeln!(out, "{}:", server.server);
		for prompt in server.prompts.iter() {
			match &prompt.description {
				Some(description) => {
					let _ = writeln!(out, "  {} — {description}", prompt.name);
				},
				None => {
					let _ = writeln!(out, "  {}", prompt.name);
				},
			}
		}
	}
	if out.is_empty() {
		return Str::new_static("No prompts available from connected servers.");
	}
	Str::new(out.trim_end())
}

/// pi `#handleNotifications`: per-server capability summary.
fn notifications(state: &ServiceState) -> Str {
	let mut out = String::new();
	for server in state.mcp.snapshots() {
		let _ = writeln!(
			out,
			"{} — {} · {} tools · {} resources · {} prompts",
			server.server,
			status_label(Some(server.health)),
			server.tools.len(),
			server.resources.len(),
			server.prompts.len()
		);
	}
	if out.is_empty() {
		return Str::new_static("No connected MCP servers.");
	}
	Str::new(out.trim_end())
}
