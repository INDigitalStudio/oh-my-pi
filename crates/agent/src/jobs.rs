//! Runtime supervision index rebuilt from the authoritative `<meta><jobs>`
//! tree.
//!
//! The board deliberately stores no durable job state.  Identities, kinds and
//! lifecycle status live in the session DOM; this module only connects those
//! elements to kill boundaries owned by the runtime.

use std::{
	str::FromStr as _,
	sync::atomic::{AtomicUsize, Ordering},
};

use flume::Receiver;
use omp_core::{FastHashMap, Str};
use omp_dom::{Dom, Handle, KnownTag, Op, PropId, PropKey, Tag, Txn, Value};
use omp_journal::EntryId;
use omp_proto::toolhost::v1::HookEventId;
use omp_session::{LifecycleWork, Session};
use omp_tool::{InvocationFeed, RegistryError, ToolIdentity};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use strum::{Display, EnumString};
use tokio::{task::JoinHandle, time};
use tokio_util::sync::CancellationToken;

use crate::dispatch::{Committer, DispatchEvent, DispatchOptions, OutputStream};

/// The three execution shapes represented by the one job primitive.
#[derive(Clone, Copy, Debug, Deserialize, Display, EnumString, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum JobKind {
	/// A detached ordinary tool call.
	Tool,
	/// A child agent kernel.
	Subagent,
	/// A supervised process or daemon.
	Process,
}

/// Terminal result produced by one owned execution unit.
#[derive(Debug)]
pub struct JobSettlement {
	/// Durable terminal status (`completed`, `cancelled`, or `failed`).
	pub status: Str,
	/// Bounded typed output, when the job completed with a value.
	pub output: Option<Box<RawValue>>,
	/// Stable terminal diagnostic, when present.
	pub error:  Option<Str>,
}

/// Durable fields projected from one `<job>` or `<subagent>` element.
#[derive(Clone, Debug)]
pub struct JobRecord {
	/// Current DOM handle.
	pub handle:  Handle,
	/// Stable durable identity.
	pub id:      Str,
	/// Shared job kind.
	pub kind:    JobKind,
	/// Journal-derived lifecycle status.
	pub status:  Str,
	/// Owning session or agent identity, when present.
	pub owner:   Option<Str>,
	/// Start timestamp, when present.
	pub started: Option<Str>,
	/// Bounded serialized output projected from the DOM.
	pub output:  Option<Box<RawValue>>,
	/// Terminal diagnostic projected from the DOM.
	pub error:   Option<Str>,
}

/// Live execution state retained after a tool call stops blocking its turn.
///
/// Durable identity and status remain in the DOM; this value owns only the
/// receiver, task and invocation feed needed to keep the execution unit alive.
pub(crate) struct DetachedCall {
	pub(crate) committer: Committer,
	pub(crate) identity:  ToolIdentity,
	pub(crate) call_id:   Str,
	pub(crate) call:      EntryId,
	pub(crate) options:   DispatchOptions,
	pub(crate) events:    Receiver<DispatchEvent>,
	pub(crate) task:      Option<JoinHandle<Result<(), RegistryError>>>,
	pub(crate) feed:      Option<InvocationFeed>,
	pub(crate) output:    OutputStream,
	pub(crate) closed:    bool,
}

type JobFactory = dyn Fn(CancellationToken) -> JoinHandle<JobSettlement> + Send + Sync + 'static;

struct RuntimeJob {
	record:    JobRecord,
	cancel:    CancellationToken,
	task:      Option<JoinHandle<JobSettlement>>,
	_detached: Option<DetachedCall>,
	/// A running detached tool call whose execution unit no longer exists
	/// (re-derived by a forward rewind or a process restart): nothing can
	/// ever settle it, so [`JobBoard::poll`] journals it `failed`.
	orphaned:  bool,
}

/// Stable diagnostic journaled on a detached tool call that lost its
/// execution unit.
pub const ORPHANED_TOOL_JOB: &str =
	"detached tool execution was lost across a rewind or restart and cannot settle";

/// A disposable runtime index over the authoritative jobs subtree.
///
/// Rebuilding preserves a live execution unit by durable `id`, remaps it to
/// the newly-derived handle, and cancels units absent from the new tree.
pub struct JobBoard {
	jobs:         Mutex<FastHashMap<Handle, RuntimeJob>>,
	factories:    Mutex<FastHashMap<Str, std::sync::Arc<JobFactory>>>,
	hooks:        Mutex<Option<crate::LifecycleHooks>>,
	/// Largest settlement output published inline on a job element; larger
	/// outputs are spilled to the session blob store (ADR 0009: the DOM and
	/// every patch it emits stay bounded, the full result lives in the CAS).
	output_bound: AtomicUsize,
}

impl Default for JobBoard {
	fn default() -> Self {
		Self {
			jobs:         Mutex::default(),
			factories:    Mutex::default(),
			hooks:        Mutex::default(),
			output_bound: AtomicUsize::new(crate::DispatchPolicy::DEFAULT_MAX_OUTPUT_BYTES),
		}
	}
}

impl JobBoard {
	/// Creates an empty runtime index.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Aligns the inline settlement bound with the dispatcher's central
	/// `max_output_bytes`.
	pub fn set_output_bound(&self, bytes: usize) {
		self.output_bound.store(bytes, Ordering::Relaxed);
	}

	/// Installs the extension observer for `job_registered`/`job_settled`.
	pub fn set_lifecycle_hooks(&self, hooks: crate::LifecycleHooks) {
		*self.hooks.lock() = Some(hooks);
	}

	fn notify_registered(&self, record: &JobRecord) {
		let hooks = self.hooks.lock();
		let Some(hooks) = hooks.as_ref() else {
			return;
		};
		let _ = hooks.notify(
			HookEventId::HookEventJobRegistered,
			serde_json::json!({
				"job_id": record.id,
				"owner": record.owner.as_deref().unwrap_or("kernel"),
				"call_id": serde_json::Value::Null,
				"lifetime": "session",
				"expected_artifact": serde_json::Value::Null,
			}),
		);
	}

	fn notify_settled(&self, record: &JobRecord, settlement: &JobSettlement) {
		let hooks = self.hooks.lock();
		let Some(hooks) = hooks.as_ref() else {
			return;
		};
		let artifact = settlement.output.as_deref().and_then(|output| {
			serde_json::from_str::<serde_json::Value>(output.get())
				.ok()?
				.get("artifact")?
				.as_str()
				.map(str::to_owned)
		});
		let _ = hooks.notify(
			HookEventId::HookEventJobSettled,
			serde_json::json!({
				"job_id": record.id,
				"owner": record.owner.as_deref().unwrap_or("kernel"),
				"artifact": artifact,
				"failed": settlement.status.as_str() != "completed",
				"duration": duration_since(record.started.as_deref()),
			}),
		);
	}

	/// Attaches the runtime kill boundary for a job already present in the DOM.
	/// Returns false when `handle` is not a lifecycle-bearing job element.
	pub fn attach(&self, dom: &Dom, handle: Handle, cancel: CancellationToken) -> bool {
		let Some(record) = record(dom, handle) else {
			return false;
		};
		self.notify_registered(&record);
		self.jobs.lock().insert(handle, RuntimeJob {
			record,
			cancel,
			task: None,
			_detached: None,
			orphaned: false,
		});
		true
	}

	/// Adopts a timed-out tool execution already represented by a `<job>`
	/// element. Retaining its live receiver and task keeps the returned job
	/// reference observable and cancellable after the foreground turn resumes.
	pub(crate) fn adopt_tool_job(
		&self,
		session: &Session,
		id: &Str,
		cancel: CancellationToken,
		detached: DetachedCall,
	) -> bool {
		let Some(record) = records(session.dom())
			.into_iter()
			.find(|record| &record.id == id)
		else {
			return false;
		};
		self.notify_registered(&record);
		self.jobs.lock().insert(record.handle, RuntimeJob {
			record,
			cancel,
			task: None,
			_detached: Some(detached),
			orphaned: false,
		});
		true
	}

	/// Attaches an owned execution task to a DOM job.
	pub fn attach_task(
		&self,
		dom: &Dom,
		handle: Handle,
		cancel: CancellationToken,
		task: JoinHandle<JobSettlement>,
	) -> bool {
		let Some(record) = record(dom, handle) else {
			return false;
		};
		self.notify_registered(&record);
		self.jobs.lock().insert(handle, RuntimeJob {
			record,
			cancel,
			task: Some(task),
			_detached: None,
			orphaned: false,
		});
		true
	}

	/// Attaches a restartable execution factory for rewind/resume lifecycle.
	pub fn attach_restartable<F>(&self, dom: &Dom, handle: Handle, factory: F) -> bool
	where
		F: Fn(CancellationToken) -> JoinHandle<JobSettlement> + Send + Sync + 'static,
	{
		let Some(record) = record(dom, handle) else {
			return false;
		};
		let factory: std::sync::Arc<JobFactory> = std::sync::Arc::new(factory);
		let cancel = CancellationToken::new();
		let task = factory(cancel.clone());
		self.notify_registered(&record);
		self.factories.lock().insert(record.id.clone(), factory);
		self.jobs.lock().insert(handle, RuntimeJob {
			record,
			cancel,
			task: Some(task),
			_detached: None,
			orphaned: false,
		});
		true
	}

	/// Rebuilds the index after open or rewind. The DOM is the source of truth.
	pub fn rebuild(&self, session: &Session) {
		let records = records(session.dom());
		let mut jobs = self.jobs.lock();
		let mut by_id: FastHashMap<Str, RuntimeJob> = std::mem::take(&mut *jobs)
			.into_values()
			.map(|job| (job.record.id.clone(), job))
			.collect();
		for record in records {
			let mut job = by_id.remove(&record.id).unwrap_or_else(|| RuntimeJob {
				// A tool job only ever gains its execution unit at detachment
				// (`adopt_tool_job`); one that reappears without it is
				// unsettleable. Subagents and processes are re-derived from
				// their factories or supervised by the environment.
				orphaned:  record.kind == JobKind::Tool && is_live_status(record.status.as_str()),
				record:    record.clone(),
				cancel:    CancellationToken::new(),
				task:      None,
				_detached: None,
			});
			job.record = record.clone();
			jobs.insert(record.handle, job);
		}
		for job in by_id.into_values() {
			job.cancel.cancel();
		}
	}

	/// Commits every already-settled owned task to the authoritative job node.
	pub fn poll(&self, session: &mut Session) -> Result<Vec<JobRecord>, omp_session::SessionError> {
		let orphaned = {
			let mut jobs = self.jobs.lock();
			jobs
				.iter_mut()
				.filter(|(_, job)| job.orphaned)
				.map(|(handle, job)| {
					job.orphaned = false;
					*handle
				})
				.collect::<Vec<_>>()
		};
		for handle in orphaned {
			self.commit(session, handle, JobSettlement {
				status: Str::new_static("failed"),
				output: None,
				error:  Some(Str::new_static(ORPHANED_TOOL_JOB)),
			})?;
		}
		let detached = {
			let mut jobs = self.jobs.lock();
			jobs
				.iter_mut()
				.filter_map(|(handle, job)| {
					let detached = job._detached.as_mut()?;
					match detached.poll(session) {
						Ok(Some(report)) => {
							job._detached = None;
							Some((*handle, JobSettlement {
								status: Str::new_static(if report.is_error {
									"failed"
								} else {
									"completed"
								}),
								output: report.spilled.and_then(|blob| {
									serde_json::value::to_raw_value(&serde_json::json!({
										"artifact": format!("artifact://sha256/{}", blob.to_hex()),
									}))
									.ok()
								}),
								error:  None,
							}))
						},
						Ok(None) => None,
						Err(error) => {
							tracing::warn!(?error, "detached tool settlement failed");
							job._detached = None;
							Some((*handle, JobSettlement {
								status: Str::new_static("failed"),
								output: None,
								error:  Some(Str::new_static("detached tool settlement failed")),
							}))
						},
					}
				})
				.collect::<Vec<_>>()
		};
		for (handle, settlement) in detached {
			self.commit(session, handle, settlement)?;
		}
		let finished = {
			let mut jobs = self.jobs.lock();
			jobs
				.iter_mut()
				.filter(|(_, job)| job.task.as_ref().is_some_and(JoinHandle::is_finished))
				.filter_map(|(handle, job)| job.task.take().map(|task| (*handle, task)))
				.collect::<Vec<_>>()
		};
		for (handle, mut task) in finished {
			let settlement = futures::FutureExt::now_or_never(&mut task)
				.and_then(Result::ok)
				.unwrap_or_else(|| JobSettlement {
					status: Str::new_static("failed"),
					output: None,
					error:  Some(Str::new_static("job execution unit ended without a settlement")),
				});
			self.commit(session, handle, settlement)?;
		}
		self.rebuild(session);
		Ok(self.list())
	}

	/// Whether any owned execution unit has finished but not yet been
	/// committed to its DOM node (a cheap check the turn loop makes before
	/// deciding whether to poll).
	#[must_use]
	pub fn has_finished_units(&self) -> bool {
		self.jobs.lock().values().any(|job| {
			job.orphaned
				|| job.task.as_ref().is_some_and(JoinHandle::is_finished)
				|| job._detached.as_ref().is_some_and(|detached| {
					detached.closed
						|| !detached.events.is_empty()
						|| detached.task.as_ref().is_some_and(JoinHandle::is_finished)
				})
		})
	}

	/// Resolves once any owned execution unit finishes (never when none is
	/// live); used by hosts idling between turns.
	pub async fn any_finished(&self) {
		loop {
			if self.has_finished_units() {
				return;
			}
			let live = self.jobs.lock().values().any(|job| {
				job.task.is_some()
					|| job
						._detached
						.as_ref()
						.is_some_and(|detached| !detached.closed)
			});
			if !live {
				std::future::pending::<()>().await;
			}
			time::sleep(std::time::Duration::from_millis(25)).await;
		}
	}

	fn commit(
		&self,
		session: &mut Session,
		handle: Handle,
		settlement: JobSettlement,
	) -> Result<(), omp_session::SessionError> {
		if let Some(record) = record(session.dom(), handle) {
			self.notify_settled(&record, &settlement);
		}
		commit_settlement(session, handle, settlement, self.output_bound.load(Ordering::Relaxed))
	}

	/// Waits for the first selected job to settle while committing completions.
	pub async fn wait(
		&self,
		session: &mut Session,
		ids: Option<&[Str]>,
	) -> Result<Option<JobRecord>, omp_session::SessionError> {
		loop {
			let records = self.poll(session)?;
			if let Some(record) = records.into_iter().find(|record| {
				record.status.as_str() != "running"
					&& ids.is_none_or(|selected| {
						selected.is_empty() || selected.iter().any(|id| id == &record.id)
					})
			}) {
				return Ok(Some(record));
			}
			let selected_live = self.list().into_iter().any(|record| {
				record.status.as_str() == "running"
					&& ids.is_none_or(|selected| {
						selected.is_empty() || selected.iter().any(|id| id == &record.id)
					})
			});
			if !selected_live {
				return Ok(None);
			}
			time::sleep(std::time::Duration::from_millis(10)).await;
		}
	}

	/// Applies lifecycle work returned by `Session::rewind`, then remaps
	/// retained handles. Removed executions are cooperatively cancelled and
	/// their owned tasks are force-aborted after a bounded grace. Added
	/// records are re-derived from `session` rather than left invisible.
	pub fn apply_lifecycle(
		&self,
		session: &Session,
		work: &LifecycleWork,
	) -> impl Future<Output = ()> + Send + 'static {
		let mut terminated = Vec::new();
		{
			let mut jobs = self.jobs.lock();
			for handle in &work.terminate {
				if let Some(job) = jobs.remove(handle) {
					job.cancel.cancel();
					terminated.push(job);
				}
			}
			for (old, new) in &work.retained {
				if let Some(mut job) = jobs.remove(old) {
					job.record.handle = *new;
					jobs.insert(*new, job);
				}
			}
		}
		self.rebuild(session);
		for handle in &work.spawn {
			let Some(record) = record(session.dom(), *handle) else {
				continue;
			};
			let Some(factory) = self.factories.lock().get(&record.id).cloned() else {
				continue;
			};
			let cancel = CancellationToken::new();
			let task = factory(cancel.clone());
			self.jobs.lock().insert(*handle, RuntimeJob {
				record,
				cancel,
				task: Some(task),
				_detached: None,
				orphaned: false,
			});
		}
		async move {
			for job in terminated {
				let _ = terminate_runtime(job).await;
			}
		}
	}

	/// Terminates one execution unit and journals `cancelled` on its DOM node.
	pub async fn terminate(
		&self,
		session: &mut Session,
		handle: Handle,
	) -> Result<bool, omp_session::SessionError> {
		let Some(job) = self.jobs.lock().remove(&handle) else {
			return Ok(false);
		};
		let settlement = terminate_runtime(job).await;
		self.commit(session, handle, settlement)?;
		self.rebuild(session);
		Ok(true)
	}

	/// Returns the current DOM-derived roster.
	#[must_use]
	pub fn list(&self) -> Vec<JobRecord> {
		let mut records = self
			.jobs
			.lock()
			.values()
			.map(|job| job.record.clone())
			.collect::<Vec<_>>();
		records.sort_by(|left, right| left.id.cmp(&right.id));
		records
	}
}

async fn terminate_runtime(mut job: RuntimeJob) -> JobSettlement {
	job.cancel.cancel();
	if let Some(mut task) = job.task.take()
		&& time::timeout(std::time::Duration::from_secs(1), &mut task)
			.await
			.is_err()
	{
		task.abort();
		let _ = task.await;
	}
	if let Some(detached) = &mut job._detached {
		if let Some(feed) = &detached.feed {
			let _ = feed.interrupt(omp_tool::Interrupt {
				class:  Str::new_static(omp_tool::Interrupt::ESCAPE),
				reason: Str::new_static("job removed by session lifecycle diff"),
			});
		}
		if let Some(mut task) = detached.task.take()
			&& time::timeout(std::time::Duration::from_secs(1), &mut task)
				.await
				.is_err()
		{
			task.abort();
			let _ = task.await;
		}
	}
	JobSettlement { status: Str::new_static("cancelled"), output: None, error: None }
}

/// Inline shape of a settlement output that exceeded the board's bound: the
/// complete JSON is in the CAS at `artifact`, `text` keeps a bounded head of
/// the child's final text for actors and the async-result notice.
#[derive(Debug, Deserialize, Serialize)]
pub struct SpilledOutput {
	/// `artifact://sha256/<hex>` of the full settlement JSON.
	pub artifact: Str,
	/// Size of the full settlement JSON.
	pub byte_len: u64,
	/// Bounded head of the output's `text`, when it carried one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub text:     Option<Str>,
}

/// Bounds one settlement output for the DOM: inline when it fits, else the
/// full JSON is spilled and a [`SpilledOutput`] stands in for it.
fn bounded_output(
	session: &Session,
	output: Box<RawValue>,
	bound: usize,
) -> Result<Box<RawValue>, omp_session::SessionError> {
	let raw = output.get();
	if raw.len() <= bound {
		return Ok(output);
	}
	let blob = session.blobs().put(raw.as_bytes())?;
	let text = serde_json::from_str::<serde_json::Value>(raw)
		.ok()
		.and_then(|value| {
			value
				.get("text")
				.and_then(serde_json::Value::as_str)
				.map(|text| Str::new(crate::dispatch::utf8_prefix(text, bound / 4)))
		});
	Ok(serde_json::value::to_raw_value(&SpilledOutput {
		artifact: Str::new(format!("artifact://sha256/{}", blob.to_hex())),
		byte_len: u64::try_from(raw.len()).unwrap_or(u64::MAX),
		text,
	})?)
}

/// Resolves a job's settlement output, reading a spilled one back from the
/// session blob store.
pub fn resolve_output(
	session: &Session,
	output: &RawValue,
) -> Result<Option<Box<RawValue>>, omp_session::SessionError> {
	let Ok(spilled) = serde_json::from_str::<SpilledOutput>(output.get()) else {
		return Ok(Some(RawValue::from_string(output.get().to_owned())?));
	};
	let Some(hex) = spilled.artifact.as_str().strip_prefix("artifact://sha256/") else {
		return Ok(None);
	};
	let reference = omp_journal::blob::BlobRef::parse_hex(hex, spilled.byte_len)?;
	let bytes = session.blobs().get(&reference)?;
	let json = std::str::from_utf8(&bytes)
		.map_err(|source| omp_session::SessionError::JobOutputUtf8 { source })?;
	Ok(Some(RawValue::from_string(json.to_owned())?))
}

fn commit_settlement(
	session: &mut Session,
	handle: Handle,
	settlement: JobSettlement,
	bound: usize,
) -> Result<(), omp_session::SessionError> {
	let cause = session
		.head()
		.ok_or(omp_session::SessionError::NoActiveTurn)?;
	let mut ops = vec![Op::Set {
		h:     handle,
		prop:  PropId::Status.into(),
		value: Value::Str(settlement.status),
	}];
	if let Some(output) = settlement.output {
		let output = bounded_output(session, output, bound)?;
		ops.push(Op::Set { h: handle, prop: PropId::Data.into(), value: Value::Json(output) });
	}
	if let Some(error) = settlement.error {
		ops.push(Op::Set {
			h:     handle,
			prop:  PropKey::Custom(Str::new_static("error")),
			value: Value::Str(error),
		});
	}
	session.patch(Txn { cause, label: Some(Str::new_static("jobs.settle")), ops })?;
	Ok(())
}

/// Whether a job or subagent owned by this session is still running (pi
/// `#hasPendingAsyncWake`): its settlement will re-wake the loop with an
/// async-result follow-up, so a candidate yield is a scheduling pause.
#[must_use]
pub fn pending_wake(dom: &Dom) -> bool {
	records(dom)
		.iter()
		.any(|record| is_live_status(record.status.as_str()))
}

/// Settled jobs whose result has not yet been delivered to the model
/// (no `delivered` prop on the element), oldest first.
#[must_use]
pub fn undelivered(dom: &Dom) -> Vec<JobRecord> {
	let mut out = records(dom)
		.into_iter()
		.filter(|record| {
			!is_live_status(record.status.as_str())
				&& dom
					.get(record.handle)
					.and_then(|node| node.prop(&PropKey::Custom(Str::new_static(DELIVERED))))
					.is_none()
		})
		.collect::<Vec<_>>();
	out.sort_by(|left, right| {
		left
			.started
			.cmp(&right.started)
			.then(left.id.cmp(&right.id))
	});
	out
}

/// Marks settled jobs as delivered to the model in one `patch@1`.
pub fn mark_delivered(
	session: &mut Session,
	handles: &[Handle],
) -> Result<(), omp_session::SessionError> {
	if handles.is_empty() {
		return Ok(());
	}
	let cause = session
		.head()
		.ok_or(omp_session::SessionError::NoActiveTurn)?;
	session.patch(Txn {
		cause,
		label: Some(Str::new_static("jobs.delivered")),
		ops: handles
			.iter()
			.map(|handle| Op::Set {
				h:     *handle,
				prop:  PropKey::Custom(Str::new_static(DELIVERED)),
				value: Value::Bool(true),
			})
			.collect(),
	})?;
	Ok(())
}

/// Prop set on a settled job once its result reached the model.
pub const DELIVERED: &str = "delivered";

/// Human-readable elapsed time since an RFC 3339 `started` stamp (`"0s"`
/// when absent or unparsable).
fn duration_since(started: Option<&str>) -> String {
	let Some(started) = started else {
		return "0s".to_owned();
	};
	let Ok(started) = jiff::Timestamp::from_str(started) else {
		return "0s".to_owned();
	};
	let elapsed = jiff::Timestamp::now()
		.since(started)
		.map_or(0, |span| span.get_seconds().max(0));
	format!("{elapsed}s")
}

fn is_live_status(status: &str) -> bool {
	matches!(status, "running" | "starting")
}

fn records(dom: &Dom) -> Vec<JobRecord> {
	let Some(jobs) = dom.children(dom.meta()).iter().copied().find(|handle| {
		dom.get(*handle)
			.is_some_and(|node| node.tag == Tag::Known(KnownTag::Jobs))
	}) else {
		return Vec::new();
	};
	let mut out = Vec::new();
	collect(dom, jobs, &mut out);
	out
}

fn collect(dom: &Dom, parent: Handle, out: &mut Vec<JobRecord>) {
	for handle in dom.children(parent) {
		if let Some(record) = record(dom, *handle) {
			out.push(record);
		}
		collect(dom, *handle, out);
	}
}

fn record(dom: &Dom, handle: Handle) -> Option<JobRecord> {
	let node = dom.get(handle)?;
	let kind = match node.tag {
		Tag::Known(KnownTag::Job) => prop(node, PropId::Kind)
			.and_then(|value| value.parse().ok())
			.unwrap_or(JobKind::Tool),
		Tag::Known(KnownTag::Subagent) => JobKind::Subagent,
		_ => return None,
	};
	Some(JobRecord {
		handle,
		id: prop(node, PropId::Id)
			.map(Str::new)
			.unwrap_or_else(|| Str::new(handle.to_string())),
		kind,
		status: prop(node, PropId::Status)
			.map(Str::new)
			.unwrap_or_else(|| Str::new_static("running")),
		owner: custom(node, "owner").map(Str::new),
		started: custom(node, "started").map(Str::new),
		output: node
			.prop(&PropKey::from(PropId::Data))
			.and_then(|value| match value {
				Value::Json(raw) => RawValue::from_string(raw.get().to_owned()).ok(),
				_ => None,
			}),
		error: custom(node, "error").map(Str::new),
	})
}

fn prop(node: &omp_dom::Node, id: PropId) -> Option<&str> {
	node.prop(&PropKey::from(id)).and_then(Value::as_str)
}

fn custom<'a>(node: &'a omp_dom::Node, key: &'static str) -> Option<&'a str> {
	node
		.prop(&PropKey::Custom(Str::new_static(key)))
		.and_then(Value::as_str)
}
