//! Journal-first child-kernel spawn composition.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, SystemTime, SystemTimeError, UNIX_EPOCH},
};

use omp_agent::{
	BackgroundToolCancellation, JobBoard, JobSettlement, RunControl, SessionTool, SessionToolCx,
	SessionToolFuture, TurnInput, TurnStop,
};
use omp_con::{CfgLoader, ConError, Ctx};
use omp_core::{Str, Ulid};
use omp_dom::{PropId, PropKey, Value};
use omp_env::EnvClient;
use omp_proto::env::v1::{CreateWorktree, DestroyWorktree, MergeMode, MergeWorktree};
use omp_session::{
	Session, SessionError,
	components::jobs::{self, JobSpec},
};
use omp_tool::{CallOutcome, ToolSpec};
use omp_tools::{
	output_schema::{self, OutputStatus, SchemaMode},
	task::{
		ChildRequest, ChildResult, Fault as TaskFault, Params as TaskParams, Payload as TaskPayload,
		StartedChild, StructuredOutput, SubagentSpawner, TaskEffort, Update as TaskUpdate,
		WorkspaceOutcome,
	},
	yield_tool::{Params as YieldParams, ResultEnvelope, YieldType},
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::settings::{
	SV_TASK_RECURSION_DEPTH, TaskEffortCeiling, TaskIsolationMerge, TaskSettings, child_ctx,
};
use crate::headless::{
	HeadlessError,
	kernel::{KernelOptions, compose_kernel},
};

/// Declaration-only spawner used to place `task@1` in the frozen registry.
///
/// Dispatcher session routing intercepts the call before this value can run.
pub struct TaskDeclarationSpawner;

impl SubagentSpawner for TaskDeclarationSpawner {
	async fn spawn<'a>(
		&'a self,
		_owner: &'a str,
		_request: TaskParams,
		_updates: &'a flume::Sender<TaskUpdate>,
	) -> Result<TaskPayload, TaskFault> {
		Err(TaskFault { message: Str::new_static("task session dispatcher is unavailable") })
	}
}

/// Concrete driver-owned implementation of the tools crate's spawn seam.
///
/// The parent session mutex is an integration boundary, not durable state: all
/// lifecycle truth is committed to its journal and DOM by [`spawn_child`].
pub struct DriverSubagentSpawner {
	/// Parent journal controller.
	pub parent:       Arc<tokio::sync::Mutex<Session>>,
	/// Production data root.
	pub data_dir:     PathBuf,
	/// Parent or isolated project root.
	pub project_root: PathBuf,
	/// Parent sessions directory.
	pub sessions_dir: PathBuf,
	/// Shared live-session routing authority.
	pub sessions:     Arc<crate::sessions::SessionRegistry>,
	/// Parent effective console context.
	pub parent_ctx:   Arc<Ctx>,
	/// Runtime job index paired with the parent DOM.
	pub jobs:         Arc<JobBoard>,
	/// Environment authority for isolated whole-workspace views.
	pub env:          EnvClient,
	/// Configuration script loader.
	pub cfg:          Arc<dyn CfgLoader>,
	/// Model selector used unless a driver policy resolves another route.
	pub model:        Str,
}

impl SubagentSpawner for DriverSubagentSpawner {
	async fn spawn<'a>(
		&'a self,
		owner: &'a str,
		request: TaskParams,
		updates: &'a flume::Sender<TaskUpdate>,
	) -> Result<TaskPayload, TaskFault> {
		let request = request.into_batch();
		admit_batch(&self.parent_ctx, &self.jobs, &request.tasks)
			.map_err(|source| TaskFault { message: Str::new(source.to_string()) })?;
		let mut pending = Vec::with_capacity(request.tasks.len());
		for child in request.tasks {
			let announced = child
				.name
				.clone()
				.unwrap_or_else(|| Str::new_static("pending"));
			let _ = updates
				.send_async(TaskUpdate { id: announced, status: Str::new_static("starting") })
				.await;
			let cancel = CancellationToken::new();
			let mut parent = self.parent.lock().await;
			let prepared = prepare_child(&mut parent, SpawnRequest {
				data_dir: &self.data_dir,
				project_root: &self.project_root,
				sessions_dir: &self.sessions_dir,
				sessions: &self.sessions,
				parent_ctx: &self.parent_ctx,
				cfg: self.cfg.as_ref(),
				jobs: &self.jobs,
				env: &self.env,
				cancel: BackgroundToolCancellation::from_token_for_host(cancel.clone()),
				owner,
				context: request.context.as_str(),
				model: self.model.as_str(),
				child,
			})
			.map_err(|source| TaskFault { message: Str::new(source.to_string()) })?;
			let handle = prepared.handle;
			let id = prepared.id.clone();
			let fallback = ChildResult {
				id: id.clone(),
				agent: prepared.agent.clone(),
				text: Str::default(),
				session_path: Str::new(prepared.session_path.to_string_lossy()),
				tokens_in: 0,
				tokens_out: 0,
				output: None,
				workspace: None,
				error: None,
			};
			let factory = move |cancel| spawn_child_task(prepared.clone(), cancel);
			if !self.jobs.attach_restartable(parent.dom(), handle, factory) {
				return Err(TaskFault {
					message: Str::new_static("subagent job could not be attached"),
				});
			}
			pending.push((id, fallback));
		}
		let mut children = Vec::with_capacity(pending.len());
		for (id, mut fallback) in pending {
			let record = {
				let mut parent = self.parent.lock().await;
				self.jobs
					.wait(&mut parent, Some(std::slice::from_ref(&id)))
					.await
					.map_err(|source| TaskFault { message: Str::new(source.to_string()) })?
			};
			let Some(record) = record else {
				fallback.error = Some(Str::new_static("subagent job disappeared before settlement"));
				children.push(fallback);
				continue;
			};
			let result = record
				.output
				.as_deref()
				.and_then(|output| serde_json::from_str::<ChildResult>(output.get()).ok())
				.unwrap_or_else(|| {
					fallback.error = record.error.clone();
					fallback
				});
			let _ = updates
				.send_async(TaskUpdate { id, status: record.status })
				.await;
			children.push(result);
		}
		Ok(TaskPayload::Settled { children })
	}
}

/// Session-owned `task@1` implementation composed by the driver.
pub struct TaskSessionTool {
	data_dir:     PathBuf,
	project_root: PathBuf,
	sessions_dir: PathBuf,
	sessions:     Arc<crate::sessions::SessionRegistry>,
	parent_ctx:   Arc<Ctx>,
	cfg:          Arc<dyn CfgLoader>,
	env:          EnvClient,
	owner:        Str,
	model:        Str,
	spec:         ToolSpec,
}

impl TaskSessionTool {
	/// Creates the task tool using host-owned child composition inputs.
	#[must_use]
	pub fn new(
		data_dir: PathBuf,
		project_root: PathBuf,
		sessions_dir: PathBuf,
		sessions: Arc<crate::sessions::SessionRegistry>,
		parent_ctx: Arc<Ctx>,
		cfg: Arc<dyn CfgLoader>,
		env: EnvClient,
		owner: Str,
		model: Str,
	) -> Self {
		Self {
			data_dir,
			project_root,
			sessions_dir,
			sessions,
			parent_ctx,
			cfg,
			env,
			owner,
			model,
			spec: omp_tools::task::spec(),
		}
	}
}

impl SessionTool for TaskSessionTool {
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
			let request: TaskParams = serde_json::from_value(value)?;
			let request = request.into_batch();
			if request.tasks.is_empty() {
				let fault = serde_json::value::to_raw_value(&TaskFault {
					message: Str::new_static("task requires at least one child"),
				})?;
				return Ok(CallOutcome::Faulted(fault));
			}
			if let Err(source) = admit_batch(&self.parent_ctx, cx.jobs, &request.tasks) {
				let fault = serde_json::value::to_raw_value(&TaskFault {
					message: Str::new(source.to_string()),
				})?;
				return Ok(CallOutcome::Faulted(fault));
			}
			let mut jobs = Vec::with_capacity(request.tasks.len());
			for child in request.tasks {
				let cancel = cx.cancel.token().child_token();
				let prepared = match prepare_child(cx.session, SpawnRequest {
					data_dir: &self.data_dir,
					project_root: &self.project_root,
					sessions_dir: &self.sessions_dir,
					sessions: &self.sessions,
					parent_ctx: &self.parent_ctx,
					cfg: self.cfg.as_ref(),
					jobs: cx.jobs,
					env: &self.env,
					cancel: BackgroundToolCancellation::from_token_for_host(cancel.clone()),
					owner: self.owner.as_str(),
					context: request.context.as_str(),
					model: self.model.as_str(),
					child,
				}) {
					Ok(prepared) => prepared,
					Err(source) => {
						let fault = serde_json::value::to_raw_value(&TaskFault {
							message: Str::new(source.to_string()),
						})?;
						return Ok(CallOutcome::Faulted(fault));
					},
				};
				let handle = prepared.handle;
				let id = prepared.id.clone();
				let agent = prepared.agent.clone();
				let session_path = prepared.session_path.clone();
				let factory = move |cancel| spawn_child_task(prepared.clone(), cancel);
				if !cx.jobs.attach_restartable(cx.session.dom(), handle, factory) {
					let fault = serde_json::value::to_raw_value(&TaskFault {
						message: Str::new_static("subagent job could not be attached"),
					})?;
					return Ok(CallOutcome::Faulted(fault));
				}
				jobs.push(StartedChild {
					id,
					agent,
					session_path: Str::new(session_path.to_string_lossy()),
					status: Str::new_static("running"),
				});
			}
			let payload = serde_json::value::to_raw_value(&TaskPayload::Started { jobs })?;
			Ok(CallOutcome::Ok(payload))
		})
	}
}

/// Failure to configure, journal, compose, or run one child kernel.
#[derive(Debug, Error)]
pub enum SpawnError {
	/// Child convar seeding or cfg execution failed.
	#[error("child console configuration failed")]
	Con(#[from] ConError),
	/// Parent job-tree update failed.
	#[error("parent job projection failed")]
	Session(#[from] SessionError),
	/// Production kernel composition failed.
	#[error("child kernel composition failed")]
	Headless(#[from] HeadlessError),
	/// Child turn failed.
	#[error("child turn failed")]
	Kernel(#[from] omp_agent::KernelError),
	/// Environment isolation or merge failed.
	#[error("subagent workspace operation failed")]
	Environment(#[from] omp_env::ClientError),
	/// Environment returned an invalid isolated workspace.
	#[error("subagent workspace response was invalid: {message}")]
	Workspace {
		/// Stable protocol defect.
		message: Str,
	},
	/// System clock is unavailable.
	#[error("system clock predates the Unix epoch")]
	Clock(#[from] SystemTimeError),
	/// The parent session has no journal head.
	#[error("parent session has no journal head")]
	MissingParentHead,
	/// The standard jobs component is absent.
	#[error("parent session has no jobs component")]
	MissingJobs,
	/// The selected agent is disabled by child policy.
	#[error("subagent `{agent}` is disabled by policy")]
	DisabledAgent {
		/// Rejected agent class.
		agent: Str,
	},
	/// The configured concurrent child ceiling is full.
	#[error("subagent concurrency limit {maximum} is already full")]
	Concurrency {
		/// Configured live-child ceiling.
		maximum: usize,
	},
	/// The configured recursive child depth has been reached.
	#[error("subagent recursion depth {depth} reaches configured maximum {maximum}")]
	RecursionDepth {
		/// Current parent depth.
		depth:   u32,
		/// Configured maximum depth.
		maximum: i32,
	},
}

/// Host-owned inputs for one child run.
pub struct SpawnRequest<'a> {
	/// Data root used by production composition and artifact storage.
	pub data_dir:     &'a Path,
	/// Parent project root (or its isolated whole-workspace view).
	pub project_root: &'a Path,
	/// Directory in which the child's `.oms` is created.
	pub sessions_dir: &'a Path,
	/// Shared live-session routing authority.
	pub sessions:     &'a Arc<crate::sessions::SessionRegistry>,
	/// Parent's effective convar context at spawn time.
	pub parent_ctx:   &'a Ctx,
	/// User/project cfg loader.
	pub cfg:          &'a dyn CfgLoader,
	/// Runtime index paired with the parent DOM.
	pub jobs:         &'a JobBoard,
	/// Environment authority used for isolated workspace views.
	pub env:          &'a EnvClient,
	/// Kill boundary for this child.
	pub cancel:       BackgroundToolCancellation,
	/// Parent job owner identity.
	pub owner:        &'a str,
	/// Shared batch context prepended to the child assignment.
	pub context:      &'a str,
	/// Requested model selector.
	pub model:        &'a str,
	/// Typed child request.
	pub child:        ChildRequest,
}

/// Journals a `<subagent>`, runs one independently configured child kernel,
/// then settles the parent element and returns the ordinary task payload row.
pub async fn spawn_child(
	parent: &mut Session,
	request: SpawnRequest<'_>,
) -> Result<ChildResult, SpawnError> {
	let jobs = request.jobs;
	let prepared = prepare_child(parent, request)?;
	let handle = prepared.handle;
	let id = prepared.id.clone();
	let factory = move |cancel| spawn_child_task(prepared.clone(), cancel);
	if !jobs.attach_restartable(parent.dom(), handle, factory) {
		return Err(SpawnError::MissingJobs);
	}
	let record = jobs
		.wait(parent, Some(std::slice::from_ref(&id)))
		.await?
		.ok_or_else(|| SpawnError::Workspace {
			message: Str::new_static("subagent job disappeared before settlement"),
		})?;
	if let Some(output) = record.output {
		return serde_json::from_str(output.get()).map_err(|source| SpawnError::Workspace {
			message: Str::new(source.to_string()),
		});
	}
	Err(SpawnError::Workspace {
		message: record
			.error
			.unwrap_or_else(|| Str::new_static("subagent job settled without output")),
	})
}

#[derive(Clone)]
struct PreparedChild {
	data_dir:     PathBuf,
	project_root: PathBuf,
	sessions_dir: PathBuf,
	sessions:     Arc<crate::sessions::SessionRegistry>,
	env:          EnvClient,
	ctx:          Arc<Ctx>,
	settings:     TaskSettings,
	cancel:       BackgroundToolCancellation,
	context:      Str,
	child:        ChildRequest,
	id:           Str,
	agent:        Str,
	session_path: PathBuf,
	handle:       omp_dom::Handle,
}

struct ChildExecution {
	status: Str,
	result: ChildResult,
}

fn admit_batch(
	parent_ctx: &Ctx,
	jobs: &JobBoard,
	children: &[ChildRequest],
) -> Result<(), SpawnError> {
	let settings = TaskSettings::from_con(parent_ctx);
	let depth = SV_TASK_RECURSION_DEPTH.get(parent_ctx);
	if settings.max_recursion_depth >= 0
		&& depth >= u32::try_from(settings.max_recursion_depth).unwrap_or(u32::MAX)
	{
		return Err(SpawnError::RecursionDepth {
			depth,
			maximum: i32::from(settings.max_recursion_depth),
		});
	}
	if let Some(agent) = children.iter().find_map(|child| {
		let agent = child.agent.as_deref().unwrap_or("task");
		settings
			.disabled_agents
			.iter()
			.any(|disabled| disabled.as_str().eq_ignore_ascii_case(agent))
			.then(|| Str::new(agent))
	}) {
		return Err(SpawnError::DisabledAgent { agent });
	}
	let active = jobs
		.list()
		.into_iter()
		.filter(|job| {
			job.kind == omp_agent::JobKind::Subagent
				&& matches!(job.status.as_str(), "starting" | "running")
		})
		.count();
	if settings.max_concurrency != 0
		&& active.saturating_add(children.len()) > settings.max_concurrency
	{
		return Err(SpawnError::Concurrency { maximum: settings.max_concurrency });
	}
	Ok(())
}

fn prepare_child(
	parent: &mut Session,
	request: SpawnRequest<'_>,
) -> Result<PreparedChild, SpawnError> {
	let parent_settings = TaskSettings::from_con(request.parent_ctx);
	let parent_depth = SV_TASK_RECURSION_DEPTH.get(request.parent_ctx);
	if parent_settings.max_recursion_depth >= 0
		&& parent_depth >= u32::try_from(parent_settings.max_recursion_depth).unwrap_or(u32::MAX)
	{
		return Err(SpawnError::RecursionDepth {
			depth: parent_depth,
			maximum: i32::from(parent_settings.max_recursion_depth),
		});
	}
	let agent = request
		.child
		.agent
		.clone()
		.unwrap_or_else(|| Str::new_static("task"));
	if parent_settings
		.disabled_agents
		.iter()
		.any(|disabled| disabled.as_str().eq_ignore_ascii_case(agent.as_str()))
	{
		return Err(SpawnError::DisabledAgent { agent });
	}
	let active = request
		.jobs
		.list()
		.into_iter()
		.filter(|job| {
			job.kind == omp_agent::JobKind::Subagent
				&& matches!(job.status.as_str(), "starting" | "running")
		})
		.count();
	if parent_settings.max_concurrency != 0 && active >= parent_settings.max_concurrency {
		return Err(SpawnError::Concurrency { maximum: parent_settings.max_concurrency });
	}
	let requested_id = request
		.child
		.name
		.clone()
		.unwrap_or_else(|| Str::new(Ulid::generate().to_string()));
	let id = allocate_id(parent, normalize_id(requested_id));
	let session_path = child_session_path(request.sessions_dir, &id);
	let ctx = Arc::new(child_ctx(request.parent_ctx, request.cfg, agent.as_str())?);
	SV_TASK_RECURSION_DEPTH
		.set(&ctx, parent_depth.saturating_add(1))
		.map_err(SpawnError::Con)?;
	let settings = TaskSettings::from_con(&ctx);
	configure_child_route(&ctx, &settings, agent.as_str(), request.child.effort)?;
	if omp_con::AI_MODEL.get(&ctx).is_empty() {
		omp_con::AI_MODEL
			.set(&ctx, Str::new(request.model))
			.map_err(SpawnError::Con)?;
	}
	let started = SystemTime::now()
		.duration_since(UNIX_EPOCH)?
		.as_millis()
		.to_string();
	let cause = parent.head().ok_or(SpawnError::MissingParentHead)?;
	let txn = jobs::insert(parent.dom(), cause, JobSpec {
		id: id.clone(),
		kind: Str::new_static("subagent"),
		owner: Str::new(request.owner),
		started: Str::new(started),
		agent: Some(agent.clone()),
	})
	.ok_or(SpawnError::MissingJobs)?;
	parent.patch(txn)?;
	let handle = parent
		.dom()
		.select(&format!("jobs subagent[id={id}]"))
		.ok()
		.and_then(|mut values| values.next())
		.ok_or(SpawnError::MissingJobs)?;
	Ok(PreparedChild {
		data_dir: request.data_dir.to_path_buf(),
		project_root: request.project_root.to_path_buf(),
		sessions_dir: request.sessions_dir.to_path_buf(),
		sessions: Arc::clone(request.sessions),
		env: request.env.clone(),
		ctx,
		settings,
		cancel: request.cancel,
		context: Str::new(request.context),
		child: request.child,
		id,
		agent,
		session_path,
		handle,
	})
}

fn spawn_child_task(
	mut prepared: PreparedChild,
	cancel: CancellationToken,
) -> tokio::task::JoinHandle<JobSettlement> {
	prepared.cancel = BackgroundToolCancellation::from_token_for_host(cancel);
	tokio::spawn(async move {
		match run_child(prepared).await {
			Ok(execution) => JobSettlement {
				status: execution.status,
				error: execution.result.error.clone(),
				output: serde_json::value::to_raw_value(&execution.result).ok(),
			},
			Err(source) => JobSettlement {
				status: Str::new_static("failed"),
				output: None,
				error: Some(Str::new(source.to_string())),
			},
		}
	})
}

async fn run_child(prepared: PreparedChild) -> Result<ChildExecution, SpawnError> {
	let selected_model = omp_con::AI_MODEL.get(&prepared.ctx);
	// Every composed child receives mutation-capable tools, so ADR 0007
	// requires isolation even when a caller attempts to opt out.
	let isolation = Some(create_isolation(&prepared.env, &prepared.id).await?);
	let run_root = isolation
		.as_ref()
		.map_or_else(|| prepared.project_root.clone(), |isolation| isolation.root.clone());
	let run = async {
		let options = KernelOptions {
			session: Some(prepared.session_path.clone()),
			sessions_dir: Some(prepared.sessions_dir.clone()),
			sessions: Some(Arc::clone(&prepared.sessions)),
			session_name: prepared.child.name.clone().or_else(|| Some(prepared.id.clone())),
			model_override: true,
			output_schema: prepared.child.output_schema.clone(),
			schema_mode: prepared.child.schema_mode,
			..KernelOptions::default()
		};
		let (mut kernel, mut child_session, _) = compose_kernel(
			&prepared.data_dir,
			&run_root,
			selected_model.as_str(),
			Arc::clone(&prepared.ctx),
			options,
		)
		.await?;
		let deadline = (prepared.settings.max_runtime_ms != 0).then(|| {
			std::time::Instant::now() + Duration::from_millis(prepared.settings.max_runtime_ms)
		});
		let mut prompt = format!("{}\n\n{}", prepared.context, prepared.child.task);
		if let Some(schema) = prepared.child.output_schema.as_ref() {
			prompt.push_str("\n\n");
			prompt.push_str(&crate::prompt_templates::schema::render(schema));
		}
		let turn = kernel
			.run_turn(
				&mut child_session,
				TurnInput { text: Str::new(prompt), attachments: Vec::new() },
				RunControl::new(prepared.cancel.token(), deadline)
					.with_request_budget(prepared.settings.soft_request_budget)
					.with_request_budget_notice(prepared.settings.soft_request_budget_notice),
			)
			.await?;
		Ok::<_, SpawnError>((turn, child_session))
	}
	.await;
	schedule_idle_park(
		Arc::clone(&prepared.sessions),
		crate::sessions::SessionId::new(prepared.id.clone()),
		prepared.settings.agent_idle_ttl_ms,
	);
	let (turn, child_session) = match run {
		Ok(run) => run,
		Err(source) => {
			if let Some(isolation) = &isolation {
				let _ = destroy_isolation(&prepared.env, isolation.id.as_str()).await;
			}
			return Err(source);
		},
	};
	let (output, schema_error) =
		structured_output(&prepared.child, &child_session, turn.assistant_text.as_str());
	let cancelled = (turn.stop == TurnStop::Cancelled)
		.then(|| Str::new_static("subagent was cancelled"));
	let error = cancelled.or(schema_error);
	let workspace = match isolation {
		Some(isolation) if error.is_some() => {
			Some(discard_isolation(&prepared.env, isolation).await?)
		},
		Some(isolation) => Some(
			finish_isolation(&prepared.env, isolation, &prepared.settings).await?,
		),
		None => None,
	};
	let status = child_status(turn.stop, error.as_ref());
	Ok(ChildExecution {
		status,
		result: ChildResult {
			id: prepared.id,
			agent: prepared.agent,
			text: turn.assistant_text,
			session_path: Str::new(prepared.session_path.to_string_lossy()),
			tokens_in: turn.tokens_in,
			tokens_out: turn.tokens_out,
			output,
			workspace,
			error,
		},
	})
}

struct IsolationRun {
	id:   Str,
	root: PathBuf,
}

async fn create_isolation(env: &EnvClient, id: &Str) -> Result<IsolationRun, SpawnError> {
	let result = env
		.create_worktree(CreateWorktree {
			name: format!("subagent-{id}"),
			base: None,
			paths: Vec::new(),
			owner_pid: std::process::id(),
			props: None,
		})
		.await?;
	let worktree = result.worktree.ok_or_else(|| SpawnError::Workspace {
		message: Str::new_static("create omitted worktree metadata"),
	})?;
	let url = url::Url::parse(&worktree.root_uri).map_err(|_| SpawnError::Workspace {
		message: Str::new_static("worktree root is not a URL"),
	})?;
	let root = url.to_file_path().map_err(|()| SpawnError::Workspace {
		message: Str::new_static("worktree root is not a local filesystem URL"),
	})?;
	Ok(IsolationRun { id: Str::new(worktree.id), root })
}

async fn finish_isolation(
	env: &EnvClient,
	isolation: IsolationRun,
	settings: &TaskSettings,
) -> Result<WorkspaceOutcome, SpawnError> {
	let mode = match settings.isolation.merge {
		TaskIsolationMerge::Patch => MergeMode::Patch,
		TaskIsolationMerge::Branch => MergeMode::Branch,
	};
	let result = env
		.merge_worktree(MergeWorktree {
			id: isolation.id.to_string(),
			dry_run: !settings.isolation.apply,
			mode: mode as i32,
			props: None,
		})
		.await?;
	let patch = (!result.artifact_hash.is_empty()).then(|| {
		let digest = result
			.artifact_hash
			.iter()
			.map(|byte| format!("{byte:02x}"))
			.collect::<String>();
		Str::new(format!("artifact://sha256/{digest}"))
	});
	let conflicts = result
		.conflicts
		.into_iter()
		.map(|conflict| Str::new(conflict.path))
		.collect::<Vec<_>>();
	let applied = settings.isolation.apply
		&& settings.isolation.merge == TaskIsolationMerge::Patch
		&& conflicts.is_empty();
	let branch = result.branch.map(Str::new);
	if settings.isolation.merge == TaskIsolationMerge::Patch {
		destroy_isolation(env, isolation.id.as_str()).await?;
	}
	Ok(WorkspaceOutcome {
		worktree: isolation.id,
		patch,
		branch,
		applied,
		conflicts,
	})
}

async fn discard_isolation(
	env: &EnvClient,
	isolation: IsolationRun,
) -> Result<WorkspaceOutcome, SpawnError> {
	destroy_isolation(env, isolation.id.as_str()).await?;
	Ok(WorkspaceOutcome {
		worktree: isolation.id,
		patch: None,
		branch: None,
		applied: false,
		conflicts: Vec::new(),
	})
}

async fn destroy_isolation(env: &EnvClient, id: &str) -> Result<(), SpawnError> {
	env.destroy_worktree(DestroyWorktree {
		id: id.to_owned(),
		force: true,
		props: None,
	})
	.await?;
	Ok(())
}

fn configure_child_route(
	ctx: &Ctx,
	settings: &TaskSettings,
	agent: &str,
	effort: Option<TaskEffort>,
) -> Result<(), SpawnError> {
	if let Some(effort) = effort {
		let thinking = match effort {
			TaskEffort::Lo => "low",
			TaskEffort::Med => "medium",
			TaskEffort::Hi => "high",
		};
		omp_con::AI_THINKING
			.set(ctx, Str::new_static(thinking))
			.map_err(SpawnError::Con)?;
	}
	clamp_effort(ctx, settings.max_effort)?;
	if let Some(model) = settings
		.agent_model_overrides
		.iter()
		.find(|(name, _)| name.as_str().eq_ignore_ascii_case(agent))
		.map(|(_, model)| model.clone())
	{
		omp_con::AI_MODEL.set(ctx, model).map_err(SpawnError::Con)?;
	} else {
		let task_model = omp_con::AI_TASK_MODEL.get(ctx);
		if !task_model.is_empty() {
			omp_con::AI_MODEL
				.set(ctx, task_model)
				.map_err(SpawnError::Con)?;
		}
	}
	Ok(())
}

fn child_status(stop: TurnStop, error: Option<&Str>) -> Str {
	if stop == TurnStop::Cancelled {
		Str::new_static("cancelled")
	} else if error.is_some() {
		Str::new_static("failed")
	} else {
		Str::new_static("completed")
	}
}

fn clamp_effort(ctx: &Ctx, ceiling: TaskEffortCeiling) -> Result<(), SpawnError> {
	let current = omp_con::AI_THINKING.get(ctx);
	let rank = |value: &str| match value {
		"off" => 0,
		"minimal" => 1,
		"low" => 2,
		"medium" => 3,
		"high" => 4,
		"xhigh" => 5,
		"max" => 6,
		_ => 4,
	};
	let maximum: &'static str = ceiling.into();
	if rank(current.as_str()) > rank(maximum) {
		omp_con::AI_THINKING
			.set(ctx, Str::new_static(maximum))
			.map_err(SpawnError::Con)?;
	}
	Ok(())
}

fn structured_output(
	request: &ChildRequest,
	session: &Session,
	last_turn: &str,
) -> (Option<StructuredOutput>, Option<Str>) {
	let Some(raw_schema) = request.output_schema.as_ref() else {
		return (None, terminal_yield_error(session));
	};
	let mode = request.schema_mode.unwrap_or_default();
	let schema = match output_schema::normalize(raw_schema) {
		Ok(Some(schema)) => schema,
		Ok(None) => return (None, terminal_yield_error(session)),
		Err(error) => {
			let error = Str::new(error.to_string());
			return (
				Some(StructuredOutput {
					mode,
					status: OutputStatus::Unavailable,
					data: None,
					error: Some(error.clone()),
				}),
				(mode == SchemaMode::Strict).then_some(error),
			);
		},
	};
	let (data, explicit_error) = terminal_yield(session, last_turn);
	if let Some(error) = explicit_error {
		let failure = Str::new(error);
		return (
			Some(StructuredOutput {
				mode,
				status: OutputStatus::Invalid,
				data,
				error: Some(failure.clone()),
			}),
			(mode == SchemaMode::Strict).then_some(failure),
		);
	}
	let Some(data) = data else {
		let failure = Str::new_static(super::yield_driver::WARNING_MISSING_YIELD);
		return (
			Some(StructuredOutput {
				mode,
				status: OutputStatus::Invalid,
				data: None,
				error: Some(failure.clone()),
			}),
			(mode == SchemaMode::Strict).then_some(failure),
		);
	};
	match output_schema::validate(&schema, &data) {
		Ok(Ok(())) => (
			Some(StructuredOutput {
				mode,
				status: OutputStatus::Valid,
				data: Some(data),
				error: None,
			}),
			None,
		),
		Ok(Err(violation)) => {
			let error = Str::new(violation.to_string());
			(
				Some(StructuredOutput {
					mode,
					status: OutputStatus::Invalid,
					data: Some(data),
					error: Some(error.clone()),
				}),
				(mode == SchemaMode::Strict).then_some(error),
			)
		},
		Err(source) => {
			let error = Str::new(source.to_string());
			(
				Some(StructuredOutput {
					mode,
					status: OutputStatus::Unavailable,
					data: Some(data),
					error: Some(error.clone()),
				}),
				(mode == SchemaMode::Strict).then_some(error),
			)
		},
	}
}

fn terminal_yield(session: &Session, last_turn: &str) -> (Option<serde_json::Value>, Option<String>) {
	let mut terminal = None;
	for handle in session.dom().handles() {
		let Some(node) = session.dom().get(handle) else {
			continue;
		};
		if node.tag != omp_dom::Tag::Custom(Str::new_static("yield"))
			|| node
				.prop(&PropKey::from(PropId::Status))
				.and_then(Value::as_str)
				!= Some("ok")
		{
			continue;
		}
		let Some(input) = session.dom().children(handle).iter().find_map(|child| {
			let node = session.dom().get(*child)?;
			(node.tag == omp_dom::Tag::Known(omp_dom::KnownTag::Input)).then_some(node)
		}) else {
			continue;
		};
		let raw = input
			.content
			.as_deref()
			.or_else(|| input.prop(&PropKey::from(PropId::Text)).and_then(Value::as_str));
		let Some(raw) = raw else { continue };
		let Ok(params) = serde_json::from_str::<YieldParams>(raw) else {
			continue;
		};
		if matches!(params.kind, Some(YieldType::Sections(_))) {
			continue;
		}
		terminal = Some(match params.result {
			ResultEnvelope::Data { data } => (Some(data), None),
			ResultEnvelope::Error { error } => (None, Some(error.to_string())),
			ResultEnvelope::LastTurn {} if !last_turn.is_empty() => {
				(Some(serde_json::Value::String(last_turn.to_owned())), None)
			},
			ResultEnvelope::LastTurn {} => (
				None,
				Some(super::yield_driver::WARNING_NULL_YIELD.to_owned()),
			),
		});
	}
	terminal.unwrap_or((None, None))
}

fn terminal_yield_error(session: &Session) -> Option<Str> {
	let (_, error) = terminal_yield(session, "");
	error.map(Str::new)
}

fn idle_park_delay(ttl_ms: u64) -> Option<Duration> {
	(ttl_ms != 0).then(|| Duration::from_millis(ttl_ms))
}

fn schedule_idle_park(
	sessions: Arc<crate::sessions::SessionRegistry>,
	id: crate::sessions::SessionId,
	ttl_ms: u64,
) {
	let Some(delay) = idle_park_delay(ttl_ms) else {
		return;
	};
	tokio::spawn(async move {
		tokio::time::sleep(delay).await;
		sessions.remove(&id);
	});
}

fn normalize_id(requested: Str) -> Str {
	let value = requested
		.as_str()
		.chars()
		.filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
		.take(32)
		.collect::<String>();
	if value.is_empty() {
		Str::new(Ulid::generate().to_string())
	} else {
		Str::new(value)
	}
}

fn allocate_id(parent: &Session, requested: Str) -> Str {
	let Some(jobs) = jobs::jobs_handle(parent.dom()) else {
		return requested;
	};
	let exists = |candidate: &str| {
		parent.dom().children(jobs).iter().any(|handle| {
			parent
				.dom()
				.get(*handle)
				.and_then(|node| node.prop(&PropKey::from(PropId::Id)))
				.and_then(Value::as_str)
				.is_some_and(|id| id == candidate)
		})
	};
	if !exists(requested.as_str()) {
		return requested;
	}
	for suffix in 2_u32.. {
		let candidate = Str::new(format!("{requested}-{suffix}"));
		if !exists(candidate.as_str()) {
			return candidate;
		}
	}
	unreachable!("u32 job-name suffix space exhausted")
}

fn child_session_path(sessions_dir: &Path, id: &Str) -> PathBuf {
	let safe = id
		.as_str()
		.chars()
		.map(|ch| {
			if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
				ch
			} else {
				'_'
			}
		})
		.collect::<String>();
	sessions_dir.join(format!("{safe}.oms"))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn request_with_schema(mode: SchemaMode) -> ChildRequest {
		ChildRequest {
			task: Str::new_static("return an object"),
			name: None,
			agent: None,
			effort: None,
			output_schema: Some(serde_json::json!({
				"type": "object",
				"required": ["ok"],
				"properties": {"ok": {"type": "boolean"}},
			})),
			schema_mode: Some(mode),
			isolated: None,
		}
	}

	#[test]
	fn batch_admission_rejects_disabled_agents_before_spawning_any_child() {
		let ctx = Ctx::new();
		super::super::settings::SV_TASK_DISABLED_AGENTS
			.set(&ctx, vec![Str::new_static("review")])
			.expect("disabled agents");
		let children = vec![ChildRequest {
			task: Str::new_static("work"),
			name: None,
			agent: Some(Str::new_static("Review")),
			effort: None,
			output_schema: None,
			schema_mode: None,
			isolated: None,
		}];
		assert!(matches!(
			admit_batch(&ctx, &JobBoard::new(), &children),
			Err(SpawnError::DisabledAgent { .. })
		));
	}

	#[test]
	fn batch_admission_enforces_the_whole_concurrency_request_atomically() {
		let ctx = Ctx::new();
		super::super::settings::SV_TASK_MAX_CONCURRENCY
			.set(&ctx, 1)
			.expect("concurrency");
		let child = ChildRequest {
			task: Str::new_static("work"),
			name: None,
			agent: None,
			effort: None,
			output_schema: None,
			schema_mode: None,
			isolated: None,
		};
		assert!(matches!(
			admit_batch(&ctx, &JobBoard::new(), &[child.clone(), child]),
			Err(SpawnError::Concurrency { maximum: 1 })
		));
	}

	#[test]
	fn strict_schema_turn_without_yield_is_a_failed_child() {
		let temp = tempfile::tempdir().expect("temporary directory");
		let session = Session::create(
			temp.path().join("child.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("child session");
		let (output, error) =
			structured_output(&request_with_schema(SchemaMode::Strict), &session, "plain text");
		assert_eq!(output.expect("schema verdict").status, OutputStatus::Invalid);
		assert!(error.is_some());
	}

	#[test]
	fn permissive_schema_turn_keeps_invalid_verdict_without_failing_child() {
		let temp = tempfile::tempdir().expect("temporary directory");
		let session = Session::create(
			temp.path().join("child.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("child session");
		let (output, error) = structured_output(
			&request_with_schema(SchemaMode::Permissive),
			&session,
			"plain text",
		);
		assert_eq!(output.expect("schema verdict").status, OutputStatus::Invalid);
		assert!(error.is_none());
	}

	#[test]
	fn agent_model_override_wins_over_task_model_and_effort_is_clamped() {
		let ctx = Ctx::new();
		omp_con::AI_TASK_MODEL
			.set(&ctx, Str::new_static("task/model"))
			.expect("task model");
		omp_con::AI_THINKING
			.set(&ctx, Str::new_static("xhigh"))
			.expect("thinking");
		let mut settings = TaskSettings {
			max_effort: TaskEffortCeiling::Low,
			..TaskSettings::default()
		};
		settings
			.agent_model_overrides
			.insert(Str::new_static("Review"), Str::new_static("agent/model"));
		configure_child_route(&ctx, &settings, "review", Some(TaskEffort::Hi))
			.expect("child route");
		assert_eq!(omp_con::AI_MODEL.get(&ctx).as_str(), "agent/model");
		assert_eq!(omp_con::AI_THINKING.get(&ctx).as_str(), "low");
	}

	#[tokio::test]
	async fn idle_ttl_zero_keeps_child_live_and_nonzero_reaps_after_boundary() {
		let temp = tempfile::tempdir().expect("tempdir");
		let session = Session::create(
			temp.path().join("idle.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.expect("session");
		let registry = Arc::new(crate::sessions::SessionRegistry::new());
		let (up, _) = flume::unbounded();
		let register = |id: &'static str| {
			registry.register(
				Str::new_static(id),
				crate::sessions::KernelHandle {
					id: crate::sessions::SessionId::new(Str::new_static(id)),
					name: Str::new_static(id),
					up: up.clone(),
					snapshot: Arc::new(parking_lot::RwLock::new(session.dom().snapshot())),
				},
			);
		};
		register("kept");
		schedule_idle_park(
			Arc::clone(&registry),
			crate::sessions::SessionId::new(Str::new_static("kept")),
			0,
		);
		register("reaped");
		schedule_idle_park(
			Arc::clone(&registry),
			crate::sessions::SessionId::new(Str::new_static("reaped")),
			1,
		);
		tokio::time::sleep(Duration::from_millis(10)).await;
		assert!(registry.lookup(crate::sessions::SessionId::from_ref("kept")).is_some());
		assert!(registry.lookup(crate::sessions::SessionId::from_ref("reaped")).is_none());
		assert_eq!(idle_park_delay(420_000), Some(Duration::from_secs(420)));
	}

	#[test]
	fn cancelled_child_never_classifies_as_completed() {
		assert_eq!(child_status(TurnStop::Cancelled, None).as_str(), "cancelled");
		assert_eq!(
			child_status(TurnStop::Completed, Some(&Str::new_static("failure"))).as_str(),
			"failed"
		);
		assert_eq!(child_status(TurnStop::Completed, None).as_str(), "completed");
	}
}
