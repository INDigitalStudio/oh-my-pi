//! Session-owned `hub@1` operations over live kernel mailboxes and the DOM.

use std::{
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use omp_agent::{
	CallControl, JobBoard, JobSettlement, Received, SessionAuthority, SessionTool, SessionToolCx,
	SessionToolFuture, Up,
};
use omp_core::{EnvPath, Str, sf};
use omp_dom::{Handle, KnownTag, Op, PropId, PropKey, Tag, Txn, Value};
use omp_env::{EnvClient, ProcessAttachmentEvent};
use omp_proto::{
	SCHEMA_REV,
	env::v1::{
		AttachOutput, EnvironmentDelta, ListProcesses, ProcessInfo, ProcessSpec, ProcessState,
		PtySpec, ReadyLog, ReadyProbe, ReadyTcp, RestartPolicy as WireRestartPolicy, RestartProcess,
		RestartSpec, Script, SendInput, SignalProcess, StartProcess, StopProcess, ready_probe,
		send_input,
	},
};
use omp_session::components::jobs::{self, JobSpec};
use omp_tool::{CallOutcome, ToolSpec};
use omp_tools::hub::{
	Fault, HubBackend, Op as HubOp, Params, Request, Response, RestartPolicy,
};
use tokio_util::sync::CancellationToken;

/// Declaration-only backend; kernel session routing intercepts every call.
pub struct HubDeclarationBackend;

impl HubBackend for HubDeclarationBackend {
	async fn execute<'a>(
		&'a self,
		_caller_id: &'a str,
		_request: Request,
		_updates: &'a flume::Sender<Response>,
	) -> Result<Response, Fault> {
		Err(Fault { message: sf!("hub session dispatcher is unavailable") })
	}
}

/// Stateless host operations shared by the model-facing hub tool and native
/// embeddings.
pub struct SessionHub;

impl SessionHub {
	/// Sends one steering item through the target kernel mailbox.
	pub fn send(
		authority: &dyn SessionAuthority,
		to: &str,
		message: Str,
	) -> Result<Response, omp_agent::SessionToolError> {
		send_to(authority, to, message)
	}

	/// Reads or drains the caller's journal-backed steering inbox.
	pub fn inbox(
		session: &mut omp_session::Session,
		peek: bool,
	) -> Result<Response, omp_agent::SessionToolError> {
		inbox(session, peek)
	}
}

/// Session-authority hub implementation.
pub struct HubSessionTool {
	env:          EnvClient,
	project_root: PathBuf,
	caller_id:    Str,
	spec:         ToolSpec,
}

impl HubSessionTool {
	/// Creates the canonical session hub.
	#[must_use]
	pub fn new(env: EnvClient, project_root: PathBuf, caller_id: Str) -> Self {
		Self { env, project_root, caller_id, spec: omp_tools::hub::spec() }
	}
}

impl SessionTool for HubSessionTool {
	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'a>(
		&'a self,
		cx: SessionToolCx<'a>,
		args: Box<serde_json::value::RawValue>,
	) -> SessionToolFuture<'a> {
		Box::pin(async move {
			let mut value: serde_json::Value = serde_json::from_str(args.get())?;
			if let Some(object) = value.as_object_mut() {
				object.remove("i");
			}
			let params: Params = serde_json::from_value(value)?;
			let params = match omp_tools::hub::validate(params, self.caller_id.as_str()) {
				Ok(request) => request.params,
				Err(fault) => {
					let fault = serde_json::value::to_raw_value(&fault)?;
					return Ok(CallOutcome::Faulted(fault));
				},
			};
			cx.jobs.rebuild(cx.session);
			cx.jobs.poll(cx.session)?;
			let response = match params.op {
				HubOp::Send if params.name.is_some() => {
					process_send(&self.env, &params).await.map_err(fault_text)
				},
				HubOp::Send if params.await_reply => match send(cx.authority, &params) {
					Ok(_) => wait_peer(cx.session, cx.control, params.timeout_ms)
						.await
						.map_err(fault_text),
					Err(error) => Err(fault_text(error)),
				},
				HubOp::Send => send(cx.authority, &params).map_err(fault_text),
				HubOp::Inbox => inbox(cx.session, params.peek).map_err(fault_text),
				HubOp::Wait => {
					wait(cx.session, cx.jobs, cx.control, &self.env, &params)
						.await
						.map_err(fault_text)
				},
				HubOp::List => list(cx.authority, params.limit).map_err(fault_text),
				HubOp::Jobs => roster(cx.jobs).map_err(fault_text),
				HubOp::Cancel => {
					cancel(cx.session, cx.jobs, params.ids.as_deref().unwrap_or_default())
						.await
						.map_err(fault_text)
				},
				HubOp::Start => {
					process_start(
						cx.session,
						cx.jobs,
						&self.env,
						&self.project_root,
						self.caller_id.as_str(),
						&params,
					)
					.await
					.map_err(fault_text)
				},
				HubOp::Ps => process_list(cx.session, cx.jobs, &self.env).await.map_err(fault_text),
				HubOp::Logs => process_logs(&self.env, &params).await.map_err(fault_text),
				HubOp::Stop => {
					process_stop(cx.session, cx.jobs, &self.env, &params)
						.await
						.map_err(fault_text)
				},
				HubOp::Restart => {
					process_restart(
						cx.session,
						cx.jobs,
						&self.env,
						self.caller_id.as_str(),
						&params,
					)
					.await
					.map_err(fault_text)
				},
				HubOp::Describe => process_describe(&self.env, &params).await.map_err(fault_text),
			};
			match response {
				Ok(response) => {
					let payload = serde_json::value::to_raw_value(&response)?;
					Ok(CallOutcome::Ok(payload))
				},
				Err(fault) => {
					let fault = serde_json::value::to_raw_value(&fault)?;
					Ok(CallOutcome::Faulted(fault))
				},
			}
		})
	}
}

fn fault_text(error: impl std::fmt::Display) -> Fault {
	Fault { message: Str::new(error.to_string()) }
}

fn send(
	authority: Option<&dyn SessionAuthority>,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let authority = authority.ok_or_else(|| omp_agent::SessionToolError::Rejected {
		message: Str::new_static("live session authority is not attached"),
	})?;
	let target = params
		.to
		.as_deref()
		.ok_or_else(|| omp_agent::SessionToolError::Rejected {
			message: Str::new_static("hub send requires `to`"),
		})?;
	let message = params
		.message
		.clone()
		.ok_or_else(|| omp_agent::SessionToolError::Rejected {
			message: Str::new_static("hub send requires `message`"),
		})?;
	send_to(authority, target, message)
}

fn send_to(
	authority: &dyn SessionAuthority,
	target: &str,
	message: Str,
) -> Result<Response, omp_agent::SessionToolError> {
	let delivered = if target == "all" {
		authority
			.list()
			.into_iter()
			.filter(|endpoint| endpoint.up.send(Up::Peer(message.clone())).is_ok())
			.count()
	} else {
		usize::from(
			authority
				.lookup(target)
				.is_some_and(|endpoint| endpoint.up.send(Up::Peer(message)).is_ok()),
		)
	};
	if delivered == 0 {
		return Err(omp_agent::SessionToolError::Rejected {
			message: Str::new_static("target session is not live"),
		});
	}
	Ok(Response {
		text:    Str::new(serde_json::json!({ "delivered": delivered }).to_string()),
		useless: false,
	})
}

fn list(
	authority: Option<&dyn SessionAuthority>,
	limit: Option<u16>,
) -> Result<Response, omp_agent::SessionToolError> {
	let authority = authority.ok_or_else(|| omp_agent::SessionToolError::Rejected {
		message: Str::new_static("live session authority is not attached"),
	})?;
	let limit = usize::from(limit.unwrap_or(omp_tools::hub::DEFAULT_LIST_LIMIT as u16))
		.min(omp_tools::hub::MAX_LIST_LIMIT);
	let rows = authority
		.list()
		.into_iter()
		.take(limit)
		.map(|endpoint| serde_json::json!({ "id": endpoint.id, "name": endpoint.name }))
		.collect::<Vec<_>>();
	let useless = rows.is_empty();
	Ok(Response { text: Str::new(serde_json::json!({ "sessions": rows }).to_string()), useless })
}

fn inbox(
	session: &mut omp_session::Session,
	peek: bool,
) -> Result<Response, omp_agent::SessionToolError> {
	let steering = session
		.dom()
		.children(session.dom().queues())
		.iter()
		.copied()
		.find(|handle| {
			session
				.dom()
				.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Steering))
		})
		.ok_or_else(|| omp_agent::SessionToolError::Rejected {
			message: Str::new_static("session steering queue is absent"),
		})?;
	let messages = session
		.dom()
		.children(steering)
		.iter()
		.filter_map(|handle| session.dom().get(*handle)?.content.clone())
		.collect::<Vec<_>>();
	let useless = messages.is_empty();
	if !peek && !useless {
		let cause = session
			.head()
			.ok_or_else(|| omp_agent::SessionToolError::Rejected {
				message: Str::new_static("session has no journal head"),
			})?;
		let ops = session
			.dom()
			.children(steering)
			.iter()
			.copied()
			.map(Op::Rm)
			.collect();
		session
			.patch(Txn { cause, label: Some(Str::new_static("hub.inbox")), ops })
			.map_err(|_| omp_agent::SessionToolError::Rejected {
				message: Str::new_static("failed to journal inbox drain"),
			})?;
	}
	Ok(Response { text: Str::new(serde_json::json!({ "messages": messages }).to_string()), useless })
}

fn roster(jobs: &JobBoard) -> Result<Response, omp_agent::SessionToolError> {
	let rows = jobs
		.list()
		.into_iter()
		.map(|job| {
			serde_json::json!({
				"id": job.id,
				"kind": job.kind.to_string(),
				"status": job.status,
				"owner": job.owner,
				"started": job.started,
				"output": job.output,
				"error": job.error,
			})
		})
		.collect::<Vec<_>>();
	let useless = rows.is_empty();
	Ok(Response { text: Str::new(serde_json::json!({ "jobs": rows }).to_string()), useless })
}

async fn cancel(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	ids: &[Str],
) -> Result<Response, omp_agent::SessionToolError> {
	let handles = jobs
		.list()
		.into_iter()
		.filter(|job| ids.contains(&job.id))
		.map(|job| job.handle)
		.collect::<Vec<_>>();
	let mut cancelled = 0;
	for handle in handles {
		cancelled += usize::from(jobs.terminate(session, handle).await?);
	}
	Ok(Response {
		text:    Str::new(serde_json::json!({ "cancelled": cancelled }).to_string()),
		useless: false,
	})
}

async fn wait_peer(
	session: &mut omp_session::Session,
	control: Option<&CallControl>,
	timeout_ms: Option<u64>,
) -> Result<Response, omp_agent::SessionToolError> {
	let timeout = timeout_ms.unwrap_or(120_000);
	let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + Duration::from_millis(timeout));
	loop {
		if let Some(message) = pop_inbox_message(session)? {
			return Ok(Response {
				text: Str::new(serde_json::json!({ "messages": [message] }).to_string()),
				useless: false,
			});
		}
		let sleep = async {
			match deadline {
				Some(deadline) => tokio::time::sleep_until(deadline).await,
				None => std::future::pending().await,
			}
		};
		let Some(control) = control else {
			sleep.await;
			return Ok(Response { text: Str::new_static(r#"{"timeout":true}"#), useless: true });
		};
		tokio::select! {
			message = control.recv() => {
				if control.handle(session, message)? == Received::Cancelled {
					return Err(omp_agent::SessionToolError::Rejected {
						message: Str::new_static("hub wait was cancelled"),
					});
				}
			},
			() = sleep => {
				return Ok(Response { text: Str::new_static(r#"{"timeout":true}"#), useless: true });
			},
		}
	}
}

async fn wait(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	control: Option<&CallControl>,
	env: &EnvClient,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	if params.name.is_some() {
		return process_wait(session, control, env, params).await;
	}
	let timeout = params.timeout_ms.unwrap_or(120_000);
	let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + Duration::from_millis(timeout));
	let selected = params
		.ids
		.clone()
		.filter(|ids| !ids.is_empty())
		.unwrap_or_else(|| {
			jobs
				.list()
				.into_iter()
				.filter(|job| matches!(job.status.as_str(), "starting" | "running"))
				.map(|job| job.id)
				.collect()
		});
	loop {
		jobs.poll(session)?;
		if let Some(message) = pop_inbox_message(session)? {
			return Ok(Response {
				text: Str::new(serde_json::json!({ "messages": [message] }).to_string()),
				useless: false,
			});
		}
		if !selected.is_empty()
			&& let Some(job) = selected_settled_job(jobs, Some(&selected))
		{
			return Ok(Response {
				text: Str::new(
					serde_json::json!({
						"job": {
							"id": job.id,
							"kind": job.kind.to_string(),
							"status": job.status,
							"output": job.output,
							"error": job.error,
						}
					})
					.to_string(),
				),
				useless: false,
			});
		}
		let sleep = async {
			match deadline {
				Some(deadline) => {
					let tick = tokio::time::Instant::now() + Duration::from_millis(25);
					tokio::time::sleep_until(tick.min(deadline)).await;
				},
				None => tokio::time::sleep(Duration::from_millis(25)).await,
			}
		};
		if let Some(control) = control {
			tokio::select! {
				message = control.recv() => {
					let received = control.handle(session, message)?;
					if received == Received::Cancelled {
						return Err(omp_agent::SessionToolError::Rejected {
							message: Str::new_static("hub wait was cancelled"),
						});
					}
				},
				() = sleep => {},
			}
		} else {
			sleep.await;
		}
		if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
			return Ok(Response {
				text: Str::new_static(r#"{"timeout":true}"#),
				useless: true,
			});
		}
	}
}

fn selected_settled_job(
	jobs: &JobBoard,
	ids: Option<&[Str]>,
) -> Option<omp_agent::JobRecord> {
	jobs.list().into_iter().find(|job| {
		ids.is_none_or(|ids| ids.is_empty() || ids.contains(&job.id))
			&& !matches!(job.status.as_str(), "running" | "starting")
	})
}

fn pop_inbox_message(
	session: &mut omp_session::Session,
) -> Result<Option<Str>, omp_agent::SessionToolError> {
	let steering = session
		.dom()
		.children(session.dom().queues())
		.iter()
		.copied()
		.find(|handle| {
			session
				.dom()
				.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Steering))
		})
		.ok_or_else(|| omp_agent::SessionToolError::Rejected {
			message: Str::new_static("session steering queue is absent"),
		})?;
	let Some(message) = session
		.dom()
		.children(steering)
		.iter()
		.find_map(|handle| session.dom().get(*handle)?.content.clone())
	else {
		return Ok(None);
	};
	let handle = session
		.dom()
		.children(steering)
		.iter()
		.copied()
		.find(|handle| session.dom().get(*handle).and_then(|node| node.content.as_ref()) == Some(&message))
		.expect("message came from a steering child");
	let cause = session
		.head()
		.ok_or_else(|| omp_agent::SessionToolError::Rejected {
			message: Str::new_static("session has no journal head"),
		})?;
	session.patch(Txn {
		cause,
		label: Some(Str::new_static("hub.wait.message")),
		ops: vec![Op::Rm(handle)],
	})?;
	Ok(Some(message))
}

async fn process_start(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	env: &EnvClient,
	project_root: &Path,
	owner: &str,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let name = required_name(params)?;
	let application =
		params
			.application
			.as_deref()
			.ok_or_else(|| omp_agent::SessionToolError::Rejected {
				message: Str::new_static("hub start requires `application`"),
			})?;
	let cwd_path = params
		.cwd
		.as_deref()
		.map(PathBuf::from)
		.map_or_else(|| project_root.to_path_buf(), |path| {
			if path.is_absolute() {
				path
			} else {
				project_root.join(path)
			}
		});
	let cwd_url = url::Url::from_file_path(&cwd_path).map_err(|()| {
		omp_agent::SessionToolError::Rejected { message: Str::new_static("process cwd is invalid") }
	})?;
	let cwd = EnvPath::new(Str::new(cwd_url.as_str())).map_err(|_| {
		omp_agent::SessionToolError::Rejected {
			message: Str::new_static("process cwd is not an environment path"),
		}
	})?;
	if let Some(handle) = job_handle(session, name) {
		let _ = jobs.terminate(session, handle).await?;
	}
	let start = process_start_request(name, application, params);
	let started = env
		.start_process(
			&cwd,
			start,
		)
		.await
		.map_err(|error| omp_agent::SessionToolError::Rejected {
			message: Str::new(error.to_string()),
		})?;
	attach_process_job(session, jobs, env, owner, name)?;
	Ok(Response {
		text:    Str::new(
			serde_json::json!({
				"name": started.name,
				"generation": started.generation,
				"pid": started.identity.map(|identity| identity.pid),
				"endpoint": started.endpoint,
				"status": "ready",
			})
			.to_string(),
		),
		useless: false,
	})
}

fn process_start_request(name: &str, application: &str, params: &Params) -> StartProcess {
	let mut command = shell_quote(application);
	for argument in params.args.as_deref().unwrap_or_default() {
		command.push(' ');
		command.push_str(&shell_quote(argument));
	}
	let ready_timeout = params
		.ready
		.as_ref()
		.and_then(|ready| ready.timeout)
		.map_or(30_000, seconds_millis);
	let mut probes = Vec::new();
	if let Some(pattern) = params.ready.as_ref().and_then(|ready| ready.log.as_ref()) {
		probes.push(ReadyProbe {
			probe: Some(ready_probe::Probe::Log(ReadyLog {
				pattern: pattern.to_string(),
				props: None,
			})),
			timeout_ms: ready_timeout,
			props: None,
		});
	}
	if let Some(port) = params.ready.as_ref().and_then(|ready| ready.port) {
		probes.push(ReadyProbe {
			probe: Some(ready_probe::Probe::Tcp(ReadyTcp {
				host: params
					.ready
					.as_ref()
					.and_then(|ready| ready.host.as_ref())
					.map_or_else(|| String::from("127.0.0.1"), ToString::to_string),
				port: u32::from(port),
				props: None,
			})),
			timeout_ms: ready_timeout,
			props: None,
		});
	}
	let detached = params.detached;
	StartProcess {
		name: name.to_owned(),
		spec: Some(ProcessSpec {
			source: Some(Script { text: command, props: None }),
			env_delta: Some(EnvironmentDelta {
				set: params
					.env
					.clone()
					.unwrap_or_default()
					.into_iter()
					.map(|(name, value)| (name.to_string(), value.to_string()))
					.collect(),
				unset: Vec::new(),
				props: None,
			}),
			pty: (params.pty.unwrap_or(true) && !detached).then(|| PtySpec {
				rows: 24,
				columns: 120,
				terminal: String::from("xterm-256color"),
				props: None,
			}),
			restart: Some(RestartSpec {
				policy: match params.restart.unwrap_or(RestartPolicy::No) {
					RestartPolicy::No => WireRestartPolicy::Never as i32,
					RestartPolicy::OnFailure => WireRestartPolicy::OnFailure as i32,
					RestartPolicy::Always => WireRestartPolicy::Always as i32,
				},
				..RestartSpec::default()
			}),
			detached,
			persist: params.persist || detached,
			timeout_ms: params.timeout.map(seconds_millis).filter(|timeout| *timeout != 0),
			..ProcessSpec::default()
		}),
		ready: probes,
		props: None,
	}
}

fn attach_process_job(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	env: &EnvClient,
	owner: &str,
	name: &str,
) -> Result<(), omp_agent::SessionToolError> {
	let handle = job_handle(session, name);
	let handle = match handle {
		Some(handle) => {
			let cause = session.head().ok_or_else(|| omp_agent::SessionToolError::Rejected {
				message: Str::new_static("session has no journal head"),
			})?;
			session.patch(jobs::set_status(cause, handle, "running"))?;
			handle
		},
		None => {
			let cause = session.head().ok_or_else(|| omp_agent::SessionToolError::Rejected {
				message: Str::new_static("session has no journal head"),
			})?;
			let started = SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.map_err(|error| omp_agent::SessionToolError::Rejected {
					message: Str::new(error.to_string()),
				})?
				.as_millis()
				.to_string();
			session.patch(
				jobs::insert(session.dom(), cause, JobSpec {
					id: Str::new(name),
					kind: Str::new_static("process"),
					owner: Str::new(owner),
					started: Str::new(started),
					agent: None,
				})
				.ok_or_else(|| omp_agent::SessionToolError::Rejected {
					message: Str::new_static("session jobs component is absent"),
				})?,
			)?;
			job_handle(session, name).ok_or_else(|| omp_agent::SessionToolError::Rejected {
				message: Str::new_static("process job was not projected"),
			})?
		},
	};
	let env = env.clone();
	let name = name.to_owned();
	let first = Arc::new(AtomicBool::new(true));
	if !jobs.attach_restartable(session.dom(), handle, move |cancel| {
		let initial = first.swap(false, Ordering::AcqRel);
		spawn_process_task(env.clone(), name.clone(), initial, cancel)
	}) {
		return Err(omp_agent::SessionToolError::Rejected {
			message: Str::new_static("process job could not be attached"),
		});
	}
	Ok(())
}

fn spawn_process_task(
	env: EnvClient,
	name: String,
	initial: bool,
	cancel: CancellationToken,
) -> tokio::task::JoinHandle<JobSettlement> {
	tokio::spawn(async move {
		match monitor_process(&env, &name, initial, cancel).await {
			Ok(status) => JobSettlement { status, output: None, error: None },
			Err(error) => JobSettlement {
				status: Str::new_static("failed"),
				output: None,
				error: Some(Str::new(error.to_string())),
			},
		}
	})
}

async fn monitor_process(
	env: &EnvClient,
	name: &str,
	initial: bool,
	cancel: CancellationToken,
) -> Result<Str, omp_agent::SessionToolError> {
	let mut process = find_process(env, name).await?;
	if !initial && terminal_process(&process) {
		let started = env
			.restart_process(RestartProcess {
				name: name.to_owned(),
				generation: process.generation,
				wire_revision: SCHEMA_REV,
				props: None,
			})
			.await
			.map_err(env_error)?;
		process.generation = started.generation;
	}
	if terminal_process(&process) {
		return Ok(process_status(&process));
	}
	let mut attachment = env
		.attach_output(AttachOutput {
			name: name.to_owned(),
			after_sequence: process.log_end_offset,
			generation: process.generation,
			max_bytes: 1,
			terminal_text: false,
			terminal_columns: 1,
			terminal_rows: 1,
			props: None,
		})
		.await
		.map_err(env_error)?;
	loop {
		tokio::select! {
			() = cancel.cancelled() => {
				env.stop_process(StopProcess {
					name: name.to_owned(),
					grace_ms: 5_000,
					generation: process.generation,
					props: None,
				})
				.await
				.map_err(env_error)?;
				return Ok(Str::new_static("cancelled"));
			},
			event = attachment.next_event() => match event.map_err(env_error)? {
				Some(ProcessAttachmentEvent::State(state)) => {
					let Some(next) = state.process else { continue };
					if terminal_process(&next) {
						return Ok(process_status(&next));
					}
				},
				Some(ProcessAttachmentEvent::Attached(_) | ProcessAttachmentEvent::Output(_)) => {},
				None => {
					let current = find_process(env, name).await?;
					return Ok(process_status(&current));
				},
			}
		}
	}
}

fn process_status(process: &ProcessInfo) -> Str {
	match process.state() {
		ProcessState::Exited => Str::new_static("completed"),
		ProcessState::Stopped => Str::new_static("cancelled"),
		ProcessState::Failed => Str::new_static("failed"),
		ProcessState::Starting | ProcessState::Ready | ProcessState::Running
		| ProcessState::Unspecified => Str::new_static("running"),
	}
}

async fn process_list(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	env: &EnvClient,
) -> Result<Response, omp_agent::SessionToolError> {
	let processes = env
		.list_processes(ListProcesses::default())
		.await
		.map_err(env_error)?;
	sync_process_statuses(session, jobs, &processes.processes)?;
	let rows = processes.processes.iter().map(process_json).collect::<Vec<_>>();
	Ok(Response {
		useless: rows.is_empty(),
		text: Str::new(serde_json::json!({ "processes": rows }).to_string()),
	})
}

async fn process_describe(
	env: &EnvClient,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let process = find_process(env, required_name(params)?).await?;
	Ok(Response { text: Str::new(process_json(&process).to_string()), useless: false })
}

async fn process_restart(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	env: &EnvClient,
	owner: &str,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let name = required_name(params)?;
	let process = find_process(env, name).await?;
	if let Some(handle) = job_handle(session, name) {
		let _ = jobs.terminate(session, handle).await?;
	}
	let started = env
		.restart_process(RestartProcess {
			name: name.to_owned(),
			generation: process.generation,
			wire_revision: SCHEMA_REV,
			props: None,
		})
		.await
		.map_err(env_error)?;
	attach_process_job(session, jobs, env, owner, name)?;
	Ok(Response {
		text:    Str::new(
			serde_json::json!({
				"name": started.name,
				"generation": started.generation,
				"status": "restarted",
			})
			.to_string(),
		),
		useless: false,
	})
}

async fn process_stop(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	env: &EnvClient,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let name = required_name(params)?;
	let process = find_process(env, name).await?;
	let grace_ms = params.timeout.map_or(5_000, seconds_millis);
	env.stop_process(StopProcess {
		name: name.to_owned(),
		grace_ms,
		generation: process.generation,
		props: None,
	})
	.await
	.map_err(env_error)?;
	set_job_status(session, name, "stopped")?;
	jobs.rebuild(session);
	Ok(Response {
		text: Str::new(serde_json::json!({ "name": name, "status": "stopped" }).to_string()),
		useless: false,
	})
}

async fn process_send(
	env: &EnvClient,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let name = required_name(params)?;
	let process = find_process(env, name).await?;
	if let Some(signal) = params.signal {
		env.signal_process(SignalProcess {
			name: name.to_owned(),
			signal: signal_name(signal).to_owned(),
			generation: process.generation,
			props: None,
		})
		.await
		.map_err(env_error)?;
	} else {
		let mut text = params.text.as_deref().unwrap_or_default().to_owned();
		for key in params.keys.as_deref().unwrap_or_default() {
			text.push_str(control_key(key).ok_or_else(|| omp_agent::SessionToolError::Rejected {
				message: Str::new(format!("unsupported process key `{key}`")),
			})?);
		}
		if params.enter.unwrap_or(params.text.is_some()) {
			text.push('\n');
		}
		if text.is_empty() {
			return Err(omp_agent::SessionToolError::Rejected {
				message: Str::new_static("hub send requires process `text`, `keys`, or `signal`"),
			});
		}
		env.send_process_input(SendInput {
			name: name.to_owned(),
			input: Some(send_input::Input::Data(text.into_bytes().into())),
			generation: process.generation,
			props: None,
		})
		.await
		.map_err(env_error)?;
	}
	Ok(Response {
		text: Str::new(serde_json::json!({ "name": name, "accepted": true }).to_string()),
		useless: false,
	})
}

async fn process_logs(
	env: &EnvClient,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let name = required_name(params)?;
	let process = find_process(env, name).await?;
	let mut attachment = env
		.attach_output(AttachOutput {
			name: name.to_owned(),
			after_sequence: params.cursor.unwrap_or(0),
			generation: process.generation,
			max_bytes: 1024 * 1024,
			terminal_text: false,
			terminal_columns: 120,
			terminal_rows: u32::from(params.lines.unwrap_or(100)),
			props: None,
		})
		.await
		.map_err(env_error)?;
	let filter = params
		.grep
		.as_deref()
		.map(regex::Regex::new)
		.transpose()
		.map_err(|error| omp_agent::SessionToolError::Rejected {
			message: Str::new(error.to_string()),
		})?;
	let timeout = params.timeout.map_or(Duration::from_secs(30), Duration::from_secs_f64);
	let mut bytes = Vec::new();
	let mut cursor = params.cursor.unwrap_or(0);
	loop {
		let next = tokio::time::timeout(
			if params.follow {
				timeout
			} else if bytes.is_empty() {
				Duration::from_secs(1)
			} else {
				Duration::from_millis(50)
			},
			attachment.next_event(),
		)
		.await;
		let event = match next {
			Ok(Ok(Some(event))) => event,
			Ok(Ok(None)) | Err(_) => break,
			Ok(Err(error)) => return Err(env_error(error)),
		};
		match event {
			ProcessAttachmentEvent::Output(output) => {
				cursor = cursor.max(output.sequence);
				if bytes.len() < 1024 * 1024 {
					let remaining = 1024 * 1024 - bytes.len();
					bytes.extend_from_slice(&output.data[..output.data.len().min(remaining)]);
				}
			},
			ProcessAttachmentEvent::State(state)
				if state.process.as_ref().is_some_and(terminal_process) =>
			{
				break;
			},
			ProcessAttachmentEvent::Attached(_) | ProcessAttachmentEvent::State(_) => {},
		}
		if !params.follow && !bytes.is_empty() {
			continue;
		}
	}
	let mut lines = String::from_utf8_lossy(&bytes)
		.lines()
		.filter(|line| filter.as_ref().is_none_or(|filter| filter.is_match(line)))
		.map(ToOwned::to_owned)
		.collect::<Vec<_>>();
	let limit = usize::from(params.lines.unwrap_or(100));
	if lines.len() > limit {
		if params.head {
			lines.truncate(limit);
		} else {
			lines.drain(..lines.len() - limit);
		}
	}
	Ok(Response {
		text:    Str::new(
			serde_json::json!({
				"name": name,
				"generation": process.generation,
				"cursor": cursor,
				"logs": lines,
			})
			.to_string(),
		),
		useless: lines.is_empty(),
	})
}

async fn process_wait(
	session: &mut omp_session::Session,
	control: Option<&CallControl>,
	env: &EnvClient,
	params: &Params,
) -> Result<Response, omp_agent::SessionToolError> {
	let name = required_name(params)?;
	let process = find_process(env, name).await?;
	let lifecycle = params.wait_for.as_deref().unwrap_or("exit");
	if process_matches_wait(&process, lifecycle) {
		return Ok(Response {
			text: Str::new(serde_json::json!({ "process": process_json(&process) }).to_string()),
			useless: false,
		});
	}
	let pattern = params
		.pattern
		.as_deref()
		.map(regex::Regex::new)
		.transpose()
		.map_err(|error| omp_agent::SessionToolError::Rejected {
			message: Str::new(error.to_string()),
		})?;
	let mut attachment = env
		.attach_output(AttachOutput {
			name: name.to_owned(),
			after_sequence: process.log_end_offset,
			generation: process.generation,
			max_bytes: 1024 * 1024,
			terminal_text: false,
			terminal_columns: 120,
			terminal_rows: 100,
			props: None,
		})
		.await
		.map_err(env_error)?;
	let timeout = params.timeout.map_or(30.0, |seconds| seconds);
	let deadline =
		(timeout != 0.0).then(|| tokio::time::Instant::now() + Duration::from_secs_f64(timeout));
	loop {
		let next = attachment.next_event();
		tokio::pin!(next);
		let sleep = async {
			match deadline {
				Some(deadline) => tokio::time::sleep_until(deadline).await,
				None => std::future::pending().await,
			}
		};
		let event = if let Some(control) = control {
			tokio::select! {
				event = &mut next => Some(event.map_err(env_error)?),
				message = control.recv() => {
					let received = control.handle(session, message)?;
					if received == Received::Cancelled {
						return Err(omp_agent::SessionToolError::Rejected {
							message: Str::new_static("hub wait was cancelled"),
						});
					}
					None
				},
				() = sleep => {
					return Ok(Response { text: Str::new_static(r#"{"timeout":true}"#), useless: true });
				},
			}
		} else {
			tokio::select! {
				event = &mut next => Some(event.map_err(env_error)?),
				() = sleep => {
					return Ok(Response { text: Str::new_static(r#"{"timeout":true}"#), useless: true });
				},
			}
		};
		let Some(event) = event.flatten() else {
			if let Some(message) = pop_inbox_message(session)? {
				return Ok(Response {
					text: Str::new(serde_json::json!({ "messages": [message] }).to_string()),
					useless: false,
				});
			}
			continue;
		};
		match event {
			ProcessAttachmentEvent::Output(output)
				if pattern
					.as_ref()
					.is_some_and(|pattern| pattern.is_match(&String::from_utf8_lossy(&output.data))) =>
			{
				return Ok(Response {
					text: Str::new(
						serde_json::json!({
							"name": name,
							"generation": output.generation,
							"matched": String::from_utf8_lossy(&output.data),
							"cursor": output.sequence,
						})
						.to_string(),
					),
					useless: false,
				});
			},
			ProcessAttachmentEvent::State(state) => {
				let Some(info) = state.process else { continue };
				if info.generation != process.generation {
					return Ok(Response {
						text: Str::new(
							serde_json::json!({
								"name": name,
								"generation": process.generation,
								"status": "replaced",
							})
							.to_string(),
						),
						useless: false,
					});
				}
				if pattern.is_none() && process_matches_wait(&info, lifecycle) {
					return Ok(Response {
						text: Str::new(
							serde_json::json!({ "process": process_json(&info) }).to_string(),
						),
						useless: false,
					});
				}
			},
			ProcessAttachmentEvent::Attached(_) | ProcessAttachmentEvent::Output(_) => {},
		}
	}
}

async fn find_process(
	env: &EnvClient,
	name: &str,
) -> Result<ProcessInfo, omp_agent::SessionToolError> {
	env.list_processes(ListProcesses::default())
		.await
		.map_err(env_error)?
		.processes
		.into_iter()
		.find(|process| process.name == name)
		.ok_or_else(|| omp_agent::SessionToolError::Rejected {
			message: Str::new(format!("unknown process `{name}`")),
		})
}

fn required_name(params: &Params) -> Result<&str, omp_agent::SessionToolError> {
	params
		.name
		.as_deref()
		.ok_or_else(|| omp_agent::SessionToolError::Rejected {
			message: Str::new_static("process operation requires `name`"),
		})
}

fn process_json(process: &ProcessInfo) -> serde_json::Value {
	serde_json::json!({
		"name": process.name,
		"generation": process.generation,
		"state": process.state().as_str_name().to_ascii_lowercase(),
		"pid": process.identity.as_ref().map(|identity| identity.pid),
		"logStart": process.log_start_offset,
		"logEnd": process.log_end_offset,
		"readyMatch": process.ready_match,
		"readyPending": process.ready_pending,
		"restartCount": process.restart_count,
		"consecutiveFailures": process.consecutive_failures,
		"endpoint": process.endpoint,
	})
}

fn terminal_process(process: &ProcessInfo) -> bool {
	matches!(
		process.state(),
		ProcessState::Exited | ProcessState::Stopped | ProcessState::Failed
	)
}

fn process_matches_wait(process: &ProcessInfo, lifecycle: &str) -> bool {
	match lifecycle {
		"ready" => process.state() == ProcessState::Ready,
		"exit" => terminal_process(process),
		_ => false,
	}
}

fn sync_process_statuses(
	session: &mut omp_session::Session,
	jobs: &JobBoard,
	processes: &[ProcessInfo],
) -> Result<(), omp_agent::SessionToolError> {
	for process in processes {
		let status = match process.state() {
			ProcessState::Starting => "starting",
			ProcessState::Ready | ProcessState::Running => "running",
			ProcessState::Exited => "completed",
			ProcessState::Stopped => "cancelled",
			ProcessState::Failed => "failed",
			ProcessState::Unspecified => continue,
		};
		set_job_status(session, process.name.as_str(), status)?;
	}
	jobs.rebuild(session);
	Ok(())
}

fn set_job_status(
	session: &mut omp_session::Session,
	id: &str,
	status: &str,
) -> Result<(), omp_agent::SessionToolError> {
	let Some(handle) = job_handle(session, id) else {
		return Ok(());
	};
	let current = session
		.dom()
		.get(handle)
		.and_then(|node| node.prop(&PropKey::from(PropId::Status)))
		.and_then(Value::as_str);
	if current == Some(status) {
		return Ok(());
	}
	let cause = session.head().ok_or_else(|| omp_agent::SessionToolError::Rejected {
		message: Str::new_static("session has no journal head"),
	})?;
	session.patch(jobs::set_status(cause, handle, status))?;
	Ok(())
}

fn job_handle(session: &omp_session::Session, id: &str) -> Option<Handle> {
	let root = jobs::jobs_handle(session.dom())?;
	session.dom().children(root).iter().copied().find(|handle| {
		session
			.dom()
			.get(*handle)
			.and_then(|node| node.prop(&PropKey::from(PropId::Id)))
			.and_then(Value::as_str)
			== Some(id)
	})
}

fn env_error(error: omp_env::ClientError) -> omp_agent::SessionToolError {
	omp_agent::SessionToolError::Rejected { message: Str::new(error.to_string()) }
}

fn seconds_millis(seconds: f64) -> u64 {
	if !seconds.is_finite() || seconds <= 0.0 {
		0
	} else {
		Duration::from_secs_f64(seconds)
			.as_millis()
			.try_into()
			.unwrap_or(u64::MAX)
	}
}

fn shell_quote(value: &str) -> String {
	format!("'{}'", value.replace('\'', "'\\''"))
}

fn signal_name(signal: omp_tools::hub::Signal) -> &'static str {
	match signal {
		omp_tools::hub::Signal::Sigint => "SIGINT",
		omp_tools::hub::Signal::Sigterm => "SIGTERM",
		omp_tools::hub::Signal::Sighup => "SIGHUP",
		omp_tools::hub::Signal::Sigquit => "SIGQUIT",
		omp_tools::hub::Signal::Sigkill => "SIGKILL",
	}
}

fn control_key(key: &str) -> Option<&'static str> {
	match key {
		"ENTER" => Some("\r"),
		"TAB" => Some("\t"),
		"ESCAPE" => Some("\u{1b}"),
		"CTRL_C" => Some("\u{3}"),
		"CTRL_D" => Some("\u{4}"),
		"UP" => Some("\u{1b}[A"),
		"DOWN" => Some("\u{1b}[B"),
		"LEFT" => Some("\u{1b}[D"),
		"RIGHT" => Some("\u{1b}[C"),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn params(value: serde_json::Value) -> Params {
		let params: Params = serde_json::from_value(value).expect("hub params");
		omp_tools::hub::validate(params, "Main")
			.expect("valid hub params")
			.params
	}

	#[test]
	fn start_request_preserves_both_readiness_probes_and_detached_policy() {
		let params = params(serde_json::json!({
			"op": "start",
			"name": "worker",
			"application": "printf",
			"args": ["hello world"],
			"ready": {"log": "hello", "port": 4321, "timeout": 2.5},
			"restart": "on-failure",
			"detached": true
		}));
		let start = process_start_request("worker", "printf", &params);
		assert_eq!(start.ready.len(), 2);
		assert!(start.ready.iter().all(|probe| probe.timeout_ms == 2_500));
		let spec = start.spec.expect("process spec");
		assert!(spec.pty.is_none());
		assert!(spec.persist);
		assert!(spec.detached);
		assert_eq!(
			spec.restart.expect("restart").policy,
			WireRestartPolicy::OnFailure as i32
		);
		assert_eq!(
			spec.source.expect("source").text,
			"'printf' 'hello world'"
		);
	}

	#[test]
	fn process_wait_classifies_ready_and_every_terminal_state() {
		let process = |state| ProcessInfo { state: state as i32, ..ProcessInfo::default() };
		assert!(process_matches_wait(&process(ProcessState::Ready), "ready"));
		for state in [ProcessState::Exited, ProcessState::Stopped, ProcessState::Failed] {
			assert!(process_matches_wait(&process(state), "exit"));
		}
		assert!(!process_matches_wait(&process(ProcessState::Running), "exit"));
	}

	#[test]
	fn process_input_keys_map_to_pty_control_sequences() {
		assert_eq!(control_key("CTRL_C"), Some("\u{3}"));
		assert_eq!(control_key("UP"), Some("\u{1b}[A"));
		assert_eq!(control_key("not-a-key"), None);
	}

	#[test]
	fn shell_arguments_are_single_quote_safe() {
		assert_eq!(shell_quote("a'b"), "'a'\\''b'");
	}
}
