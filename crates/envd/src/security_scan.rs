//! Environment-owned repository security scan authority.

use std::{
	collections::BTreeMap,
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_core::{CowBytes, Hash32, Str};
use omp_tools::{
	read::{
		Fault as ReadFault,
		resolver::{
			LineOffsetCache, Resolve, ResourceCompletion, ResourceEntry, ResourceList, fuzzy_score,
		},
		selector::ParsedSelector,
	},
	security_scan::{
		Action, Fault, Params, Payload, SecurityScanControl, TargetKind, ValidationEvidence,
		ValidationStatus,
	},
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_FILES: usize = 20_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Plan {
	id:            Str,
	fingerprint:   Str,
	target:        TargetKind,
	include_paths: Vec<Str>,
	exclude_paths: Vec<Str>,
	output_root:   Option<Str>,
	ref_supported: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Validation {
	status:   ValidationStatus,
	summary:  Str,
	evidence: Vec<ValidationEvidence>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Finding {
	id:         Str,
	path:       Str,
	line:       usize,
	rule:       Str,
	summary:    Str,
	validation: Option<Validation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Scan {
	id:       Str,
	plan_id:  Str,
	findings: Vec<Finding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Operation {
	id:            Str,
	scan_id:       Str,
	phase:         Str,
	finding_count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct State {
	plans:      BTreeMap<Str, Plan>,
	operations: BTreeMap<Str, Operation>,
	scans:      BTreeMap<Str, Scan>,
}

/// Live security authority scoped to one project environment.
#[derive(Clone)]
pub struct SecurityScanService {
	root:       Arc<PathBuf>,
	state_path: Arc<PathBuf>,
	state:      Arc<Mutex<Result<State, ()>>>,
	lines:      Arc<LineOffsetCache>,
}

impl SecurityScanService {
	/// Opens the project security authority under the environment state root.
	pub fn new(root: PathBuf, state_dir: &Path) -> Self {
		let workspace = Hash32::sum(root.to_string_lossy().as_bytes()).to_hex();
		let state_path = state_dir
			.join("security")
			.join(workspace.as_str())
			.join("state.json");
		let state = if !state_path.exists() {
			Ok(State::default())
		} else {
			fs::read(&state_path)
				.ok()
				.and_then(|bytes| serde_json::from_slice(&bytes).ok())
				.ok_or(())
		};
		Self {
			root:       Arc::new(root),
			state_path: Arc::new(state_path),
			state:      Arc::new(Mutex::new(state)),
			lines:      Arc::new(LineOffsetCache::default()),
		}
	}

	fn execute_sync(&self, params: Params) -> Result<Payload, Fault> {
		match params.action {
			Action::Preflight => self.preflight(params),
			Action::Start => self.start(params),
			Action::Status => self.status(params),
			Action::Cancel => self.cancel(params),
			Action::Validate => self.validate(params),
			Action::CloudScans | Action::CloudStart | Action::CloudStatus | Action::CloudPull => {
				Err(Fault::Unavailable)
			},
		}
	}

	fn preflight(&self, params: Params) -> Result<Payload, Fault> {
		let target = params.target_kind.unwrap_or_default();
		let include_paths = clean_paths(params.include_paths.unwrap_or_default())?;
		let exclude_paths = clean_paths(params.exclude_paths.unwrap_or_default())?;
		if target == TargetKind::ScopedPath && include_paths.is_empty() {
			return Err(Fault::InvalidArguments);
		}
		let ref_supported = match target {
			TargetKind::RefDiff => {
				params
					.base_revision
					.as_ref()
					.is_some_and(|value| !value.trim().is_empty())
					&& params
						.head_revision
						.as_ref()
						.is_some_and(|value| !value.trim().is_empty())
			},
			_ => true,
		};
		if target == TargetKind::RefDiff && !ref_supported {
			return Err(Fault::InvalidArguments);
		}
		let canonical = serde_json::to_vec(&json!({
			"target_kind": target,
			"include_paths": include_paths,
			"exclude_paths": exclude_paths,
			"base_revision": params.base_revision,
			"head_revision": params.head_revision,
			"knowledge_base_paths": params.knowledge_base_paths,
			"output_root": params.output_root,
			"archive_existing": params.archive_existing,
		}))
		.map_err(|_| Fault::Storage)?;
		let fingerprint = Str::from(Hash32::sum(&canonical).to_hex().as_str());
		let id = Str::from(format!("plan-{}", &fingerprint[..16]));
		let plan = Plan {
			id: id.clone(),
			fingerprint: fingerprint.clone(),
			target,
			include_paths,
			exclude_paths,
			output_root: params.output_root,
			ref_supported,
		};
		let mut state = self.state.lock();
		let state = state.as_mut().map_err(|()| Fault::Storage)?;
		state.plans.insert(id.clone(), plan);
		self.persist(state)?;
		Ok(Payload {
			action: Action::Preflight,
			output: Str::from(format!(
				"Security plan {id} is ready. Fingerprint: {fingerprint}. Start it with action=start \
				 and plan_id={id}."
			)),
			data:   json!({"plan": {"id": id, "fingerprint": fingerprint}}),
		})
	}

	fn start(&self, params: Params) -> Result<Payload, Fault> {
		let plan_id = required(params.plan_id)?;
		let plan = {
			let state = self.state.lock();
			state
				.as_ref()
				.map_err(|()| Fault::Storage)?
				.plans
				.get(&plan_id)
				.cloned()
				.ok_or(Fault::NotFound)?
		};
		if matches!(plan.target, TargetKind::RefDiff | TargetKind::WorkingTree) {
			return Err(Fault::Unavailable);
		}
		let findings = scan_workspace(&self.root, &plan)?;
		let scan_id = Str::from(format!("scan-{}", &plan.fingerprint[..16]));
		let operation_id = Str::from(format!("operation-{}", &plan.fingerprint[..16]));
		let scan = Scan { id: scan_id.clone(), plan_id: plan.id.clone(), findings };
		let operation = Operation {
			id:            operation_id.clone(),
			scan_id:       scan_id.clone(),
			phase:         Str::new_static("completed"),
			finding_count: scan.findings.len(),
		};
		let finding_count = operation.finding_count;
		let mut state = self.state.lock();
		let state = state.as_mut().map_err(|()| Fault::Storage)?;
		state.scans.insert(scan_id.clone(), scan.clone());
		state
			.operations
			.insert(operation_id.clone(), operation.clone());
		self.persist(state)?;
		if let Some(output_root) = plan.output_root {
			self.write_result(&output_root, &scan)?;
		}
		Ok(Payload {
			action: Action::Start,
			output: Str::from(format!(
				"Security scan {scan_id} completed as {operation_id}; {finding_count} finding(s)."
			)),
			data:   json!({"operation": operation, "scan": {"id": scan_id, "finding_count": finding_count}}),
		})
	}

	fn status(&self, params: Params) -> Result<Payload, Fault> {
		let operation_id = required(params.operation_id)?;
		let state = self.state.lock();
		let operation = state
			.as_ref()
			.map_err(|()| Fault::Storage)?
			.operations
			.get(&operation_id)
			.cloned()
			.ok_or(Fault::NotFound)?;
		Ok(Payload {
			action: Action::Status,
			output: Str::from(format!(
				"Security scan {}: {}; {} finding(s).",
				operation.scan_id, operation.phase, operation.finding_count
			)),
			data:   json!({"operation": operation}),
		})
	}

	fn cancel(&self, params: Params) -> Result<Payload, Fault> {
		let operation_id = required(params.operation_id)?;
		let mut state = self.state.lock();
		let state = state.as_mut().map_err(|()| Fault::Storage)?;
		let operation = state
			.operations
			.get_mut(&operation_id)
			.ok_or(Fault::NotFound)?;
		let cancelled = operation.phase == "running";
		if cancelled {
			operation.phase = Str::new_static("cancelled");
			self.persist(state)?;
		}
		Ok(Payload {
			action: Action::Cancel,
			output: Str::from(if cancelled {
				format!("Cancellation requested for {operation_id}.")
			} else {
				format!("No running operation {operation_id}.")
			}),
			data:   json!({"operation_id": operation_id, "cancelled": cancelled}),
		})
	}

	fn validate(&self, params: Params) -> Result<Payload, Fault> {
		let scan_id = required(params.scan_id)?;
		let finding_id = required(params.finding_id)?;
		let status = params.validation_status.ok_or(Fault::InvalidArguments)?;
		let summary = required(params.validation_summary)?;
		let evidence = params.validation_evidence.unwrap_or_default();
		if evidence.iter().any(|item| item.label.trim().is_empty()) {
			return Err(Fault::InvalidArguments);
		}
		let mut state = self.state.lock();
		let state = state.as_mut().map_err(|()| Fault::Storage)?;
		let scan = state.scans.get_mut(&scan_id).ok_or(Fault::NotFound)?;
		let finding = scan
			.findings
			.iter_mut()
			.find(|finding| finding.id == finding_id)
			.ok_or(Fault::NotFound)?;
		finding.validation = Some(Validation { status, summary, evidence });
		self.persist(state)?;
		Ok(Payload {
			action: Action::Validate,
			output: Str::from(format!("Finding {finding_id} validation is now {:?}.", status)),
			data:   json!({"finding": {"id": finding_id, "validation_status": status}}),
		})
	}

	fn persist(&self, state: &State) -> Result<(), Fault> {
		let parent = self.state_path.parent().ok_or(Fault::Storage)?;
		fs::create_dir_all(parent).map_err(|_| Fault::Storage)?;
		let bytes = serde_json::to_vec(state).map_err(|_| Fault::Storage)?;
		let temporary = self.state_path.with_extension("json.tmp");
		fs::write(&temporary, bytes).map_err(|_| Fault::Storage)?;
		fs::rename(temporary, self.state_path.as_ref()).map_err(|_| Fault::Storage)
	}

	fn write_result(&self, output_root: &str, scan: &Scan) -> Result<(), Fault> {
		let relative = checked_relative(output_root)?;
		let output = self.root.join(relative);
		fs::create_dir_all(&output).map_err(|_| Fault::Storage)?;
		let bytes = serde_json::to_vec_pretty(scan).map_err(|_| Fault::Storage)?;
		fs::write(output.join("security-scan.json"), bytes).map_err(|_| Fault::Storage)
	}

	fn render_resource(&self, resource: &str) -> Result<Vec<u8>, ReadFault> {
		let parts = security_parts(resource)?;
		let state = self.state.lock();
		let state = state.as_ref().map_err(|()| security_state_fault())?;
		let mut body = String::new();
		match parts.as_slice() {
			[] => {
				body.push_str(
					"# Security\n\nOMP-owned software-security scan reports and validated advisories. \
					 This namespace is read-only; use `dyn security_scan` for mutations.\n\n- \
					 `security://scans` — stored scans\n",
				);
			},
			["scans"] => {
				body.push_str("# Security scans\n\n");
				if state.scans.is_empty() {
					body.push_str("No scans are stored for this project.\n");
				} else {
					for scan in state.scans.values() {
						let phase = scan_phase(state, &scan.id);
						let _ = writeln!(
							body,
							"- `{}` — {}; {} finding(s)",
							scan.id,
							phase,
							scan.findings.len()
						);
					}
				}
			},
			["scans", scan_id] => {
				let scan = state
					.scans
					.get(*scan_id)
					.ok_or_else(|| unknown_scan(scan_id))?;
				let plan = state.plans.get(&scan.plan_id);
				let _ = writeln!(body, "# Security scan {}\n", scan.id);
				let _ = writeln!(body, "- Status: **{}**", scan_phase(state, &scan.id));
				let _ = writeln!(body, "- Findings: **{}**", scan.findings.len());
				if let Some(plan) = plan {
					let _ = writeln!(body, "- Target: **{:?}**", plan.target);
					let _ = writeln!(body, "- Plan: `{}`", plan.id);
				}
				body.push_str("\nResources: `manifest`, `findings`, and `report`.\n");
			},
			["scans", scan_id, "manifest"] => {
				let scan = state
					.scans
					.get(*scan_id)
					.ok_or_else(|| unknown_scan(scan_id))?;
				body = serde_json::to_string_pretty(scan).map_err(|_| security_state_fault())?;
				body.push('\n');
			},
			["scans", scan_id, "findings"] => {
				let scan = state
					.scans
					.get(*scan_id)
					.ok_or_else(|| unknown_scan(scan_id))?;
				render_findings_index(&mut body, scan);
			},
			["scans", scan_id, "findings", finding_id] => {
				let scan = state
					.scans
					.get(*scan_id)
					.ok_or_else(|| unknown_scan(scan_id))?;
				let finding = scan
					.findings
					.iter()
					.find(|finding| finding.id == *finding_id)
					.ok_or_else(|| unknown_finding(finding_id))?;
				render_finding(&mut body, finding);
			},
			["scans", scan_id, "report"] => {
				let scan = state
					.scans
					.get(*scan_id)
					.ok_or_else(|| unknown_scan(scan_id))?;
				let _ = writeln!(body, "# Security report {}\n", scan.id);
				let _ = writeln!(body, "- Status: **{}**", scan_phase(state, &scan.id));
				let _ = writeln!(body, "- Findings: **{}**\n", scan.findings.len());
				for finding in &scan.findings {
					render_finding(&mut body, finding);
				}
			},
			_ => return Err(unknown_resource(resource)),
		}
		Ok(body.into_bytes())
	}

	fn list_resource(
		&self,
		resource: &str,
		max_entries: usize,
		max_bytes: usize,
	) -> Result<ResourceList, ReadFault> {
		let parts = security_parts(resource)?;
		let state = self.state.lock();
		let state = state.as_ref().map_err(|()| security_state_fault())?;
		let mut candidates = Vec::new();
		match parts.as_slice() {
			[] => candidates.push(security_entry("scans", true, "scans")),
			["scans"] => {
				for scan in state.scans.values() {
					candidates.push(security_entry(&format!("scans/{}", scan.id), true, &scan.id));
				}
			},
			["scans", scan_id] => {
				if !state.scans.contains_key(*scan_id) {
					return Err(unknown_scan(scan_id));
				}
				for (name, directory) in [("manifest", false), ("findings", true), ("report", false)] {
					candidates.push(security_entry(&format!("scans/{scan_id}/{name}"), directory, name));
				}
			},
			["scans", scan_id, "findings"] => {
				let scan = state
					.scans
					.get(*scan_id)
					.ok_or_else(|| unknown_scan(scan_id))?;
				for finding in &scan.findings {
					candidates.push(security_entry(
						&format!("scans/{scan_id}/findings/{}", finding.id),
						false,
						&finding.id,
					));
				}
			},
			_ => return Err(unknown_resource(resource)),
		}
		let mut entries = Vec::new();
		let mut used = 0usize;
		let mut truncated = false;
		for entry in candidates {
			let bytes = entry.uri.len().saturating_add(entry.name.len());
			if entries.len() == max_entries || used.saturating_add(bytes) > max_bytes {
				truncated = true;
				break;
			}
			used += bytes;
			entries.push(entry);
		}
		Ok(ResourceList { entries, truncated })
	}

	fn complete_resource(
		&self,
		query: &str,
		max_results: usize,
	) -> Result<Vec<ResourceCompletion>, ReadFault> {
		let state = self.state.lock();
		let state = state.as_ref().map_err(|()| security_state_fault())?;
		let mut paths = vec![("scans".to_owned(), Str::new_static("stored security scans"))];
		for scan in state.scans.values() {
			let prefix = format!("scans/{}", scan.id);
			paths.push((
				prefix.clone(),
				Str::new(format!(
					"{}; {} finding(s)",
					scan_phase(state, &scan.id),
					scan.findings.len()
				)),
			));
			for child in ["manifest", "findings", "report"] {
				paths.push((format!("{prefix}/{child}"), Str::new_static("security scan resource")));
			}
			for finding in &scan.findings {
				paths.push((format!("{prefix}/findings/{}", finding.id), finding.summary.clone()));
			}
		}
		let query = query
			.trim()
			.strip_prefix("security://")
			.unwrap_or(query.trim())
			.trim_start_matches('/');
		let mut matches = paths
			.into_iter()
			.filter_map(|(path, description)| {
				let score = fuzzy_score(query, &path)?;
				Some(ResourceCompletion {
					value: Str::new(format!("security://{path}")),
					description,
					score,
				})
			})
			.collect::<Vec<_>>();
		matches.sort_unstable_by(|left, right| {
			right
				.score
				.cmp(&left.score)
				.then_with(|| left.value.cmp(&right.value))
		});
		matches.truncate(max_results);
		Ok(matches)
	}
}

impl SecurityScanControl for SecurityScanService {
	fn execute(&self, params: Params) -> impl Future<Output = Result<Payload, Fault>> + Send + '_ {
		let service = self.clone();
		async move {
			tokio::task::spawn_blocking(move || service.execute_sync(params))
				.await
				.map_err(|_| Fault::Storage)?
		}
	}
}

impl Resolve for SecurityScanService {
	async fn read<'a>(
		&'a self,
		resource: &'a str,
		selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, ReadFault> {
		let bytes = self.render_resource(resource)?;
		crate::tool_url::select_bytes(&self.lines, resource, CowBytes::from(bytes), selector)
	}

	async fn list(
		&self,
		resource: &str,
		max_entries: usize,
		max_bytes: usize,
	) -> Result<ResourceList, ReadFault> {
		self.list_resource(resource, max_entries, max_bytes)
	}

	async fn complete(
		&self,
		query: &str,
		max_results: usize,
	) -> Result<Vec<ResourceCompletion>, ReadFault> {
		self.complete_resource(query, max_results)
	}
}

fn security_parts(resource: &str) -> Result<Vec<&str>, ReadFault> {
	let resource = resource.trim_matches('/');
	if resource.is_empty() {
		return Ok(Vec::new());
	}
	let parts = resource.split('/').collect::<Vec<_>>();
	if parts
		.iter()
		.any(|part| part.is_empty() || matches!(*part, "." | "..") || part.contains('\\'))
	{
		return Err(ReadFault::Invalid {
			message: Str::new_static("Invalid or escaping security:// resource."),
		});
	}
	Ok(parts)
}

fn security_state_fault() -> ReadFault {
	ReadFault::Source {
		message: Str::new_static("Stored security scan state is corrupt or unavailable."),
	}
}

fn unknown_scan(scan_id: &str) -> ReadFault {
	ReadFault::Source {
		message: Str::new(format!(
			"Unknown security scan: {scan_id}. Read security://scans to list stored scans."
		)),
	}
}

fn unknown_finding(finding_id: &str) -> ReadFault {
	ReadFault::Source { message: Str::new(format!("Unknown security finding: {finding_id}.")) }
}

fn unknown_resource(resource: &str) -> ReadFault {
	ReadFault::Source {
		message: Str::new(format!(
			"Unknown security resource: security://{}. Read security:// for the index.",
			resource.trim_matches('/')
		)),
	}
}

fn scan_phase<'a>(state: &'a State, scan_id: &str) -> &'a str {
	state
		.operations
		.values()
		.find(|operation| operation.scan_id == scan_id)
		.map_or("stored", |operation| operation.phase.as_str())
}

fn render_findings_index(body: &mut String, scan: &Scan) {
	let _ = writeln!(body, "# Findings for {}\n", scan.id);
	if scan.findings.is_empty() {
		body.push_str("No findings.\n");
		return;
	}
	for finding in &scan.findings {
		let _ = writeln!(
			body,
			"- `{}` **{}** — {} (`{}:{}`)",
			finding.id, finding.rule, finding.summary, finding.path, finding.line
		);
	}
}

fn render_finding(body: &mut String, finding: &Finding) {
	let _ = writeln!(body, "## {}\n", finding.summary);
	let _ = writeln!(body, "- ID: `{}`", finding.id);
	let _ = writeln!(body, "- Rule: `{}`", finding.rule);
	let _ = writeln!(body, "- Location: `{}:{}`", finding.path, finding.line);
	if let Some(validation) = &finding.validation {
		let _ = writeln!(body, "- Validation: **{:?}**", validation.status);
		let _ = writeln!(body, "\n{}\n", validation.summary);
		if !validation.evidence.is_empty() {
			body.push_str("### Evidence\n\n");
			for evidence in &validation.evidence {
				let _ = writeln!(body, "- **{}** — {}", evidence.label, evidence.explanation);
			}
		}
	}
	body.push('\n');
}

fn security_entry(path: &str, directory: bool, name: &str) -> ResourceEntry {
	ResourceEntry {
		uri: Str::new(format!("security://{path}{}", if directory { "/" } else { "" })),
		name: Str::new(name),
		directory,
		size: 0,
	}
}

fn required(value: Option<Str>) -> Result<Str, Fault> {
	value
		.filter(|value| !value.trim().is_empty())
		.ok_or(Fault::InvalidArguments)
}

fn clean_paths(paths: Vec<Str>) -> Result<Vec<Str>, Fault> {
	paths
		.into_iter()
		.map(|path| {
			checked_relative(&path)?;
			Ok(path)
		})
		.collect()
}

fn checked_relative(path: &str) -> Result<&Path, Fault> {
	let path = Path::new(path);
	if path.as_os_str().is_empty()
		|| path.is_absolute()
		|| path
			.components()
			.any(|component| matches!(component, std::path::Component::ParentDir))
	{
		return Err(Fault::InvalidArguments);
	}
	Ok(path)
}

fn scan_workspace(root: &Path, plan: &Plan) -> Result<Vec<Finding>, Fault> {
	let mut paths = Vec::new();
	collect_files(root, root, &mut paths)?;
	let mut findings = Vec::new();
	for path in paths.into_iter().take(MAX_FILES) {
		let relative = path.strip_prefix(root).map_err(|_| Fault::Storage)?;
		let relative_text = relative.to_string_lossy();
		if !plan.include_paths.is_empty()
			&& !plan
				.include_paths
				.iter()
				.any(|include| relative.starts_with(include.as_str()))
		{
			continue;
		}
		if plan
			.exclude_paths
			.iter()
			.any(|exclude| relative.starts_with(exclude.as_str()))
		{
			continue;
		}
		let metadata = fs::metadata(&path).map_err(|_| Fault::Storage)?;
		if metadata.len() > MAX_FILE_BYTES {
			continue;
		}
		let bytes = fs::read(&path).map_err(|_| Fault::Storage)?;
		let Ok(text) = std::str::from_utf8(&bytes) else {
			continue;
		};
		for (index, line) in text.lines().enumerate() {
			let rule = if line.contains("-----BEGIN PRIVATE KEY-----") {
				Some(("private-key", "Private key material is committed to the repository"))
			} else if contains_aws_access_key(line) {
				Some(("aws-access-key", "AWS access key identifier is committed to the repository"))
			} else {
				None
			};
			let Some((rule, summary)) = rule else {
				continue;
			};
			let identity = format!("{relative_text}:{}:{rule}", index + 1);
			let digest = Hash32::sum(identity.as_bytes()).to_hex();
			findings.push(Finding {
				id:         Str::from(format!("finding-{}", &digest[..16])),
				path:       Str::from(relative_text.as_ref()),
				line:       index + 1,
				rule:       Str::new_static(rule),
				summary:    Str::new_static(summary),
				validation: None,
			});
		}
	}
	Ok(findings)
}

fn collect_files(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), Fault> {
	for entry in fs::read_dir(directory).map_err(|_| Fault::Storage)? {
		let entry = entry.map_err(|_| Fault::Storage)?;
		let path = entry.path();
		let relative = path.strip_prefix(root).map_err(|_| Fault::Storage)?;
		if relative.starts_with(".git") || relative.starts_with("target") {
			continue;
		}
		let kind = entry.file_type().map_err(|_| Fault::Storage)?;
		if kind.is_symlink() {
			continue;
		}
		if kind.is_dir() {
			collect_files(root, &path, paths)?;
		} else if kind.is_file() {
			paths.push(path);
		}
		if paths.len() >= MAX_FILES {
			break;
		}
	}
	Ok(())
}

fn contains_aws_access_key(line: &str) -> bool {
	let bytes = line.as_bytes();
	bytes.windows(20).any(|window| {
		window.starts_with(b"AKIA")
			&& window[4..]
				.iter()
				.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
	})
}

#[cfg(test)]
mod tests {
	use std::time::{SystemTime, UNIX_EPOCH};

	use super::*;

	fn fixture() -> (PathBuf, PathBuf) {
		let unique = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock")
			.as_nanos();
		let root = std::env::temp_dir().join(format!("omp-security-{unique}"));
		let state = std::env::temp_dir().join(format!("omp-security-state-{unique}"));
		fs::create_dir_all(&root).expect("fixture root");
		(root, state)
	}

	#[test]
	fn preflight_start_status_and_validate_use_persistent_authority_state() {
		let (root, state_dir) = fixture();
		fs::write(root.join("leak.txt"), "-----BEGIN PRIVATE KEY-----\n").expect("security fixture");
		let service = SecurityScanService::new(root.clone(), &state_dir);
		let preflight = service
			.preflight(Params {
				action: Action::Preflight,
				target_kind: Some(TargetKind::Repository),
				..empty_params(Action::Preflight)
			})
			.expect("preflight");
		let plan_id = preflight.data["plan"]["id"].as_str().expect("plan id");
		let started = service
			.start(Params { plan_id: Some(Str::new(plan_id)), ..empty_params(Action::Start) })
			.expect("start");
		assert_eq!(started.data["scan"]["finding_count"], 1);
		let operation_id = started.data["operation"]["id"]
			.as_str()
			.expect("operation id");
		let status = service
			.status(Params {
				operation_id: Some(Str::new(operation_id)),
				..empty_params(Action::Status)
			})
			.expect("status");
		assert_eq!(status.data["operation"]["phase"], "completed");
		let cancelled = service
			.cancel(Params {
				operation_id: Some(Str::new(operation_id)),
				..empty_params(Action::Cancel)
			})
			.expect("cancel completed operation");
		assert_eq!(cancelled.data["cancelled"], false);
		let scan_id = started.data["scan"]["id"].as_str().expect("scan id");
		let persisted = service.state.lock();
		let finding_id = persisted.as_ref().expect("state").scans[scan_id].findings[0]
			.id
			.clone();
		drop(persisted);
		let validated = service
			.validate(Params {
				scan_id: Some(Str::new(scan_id)),
				finding_id: Some(finding_id),
				validation_status: Some(ValidationStatus::Validated),
				validation_summary: Some(Str::new_static("confirmed fixture")),
				..empty_params(Action::Validate)
			})
			.expect("validate");
		assert_eq!(validated.data["finding"]["validation_status"], "validated");
		for action in [Action::CloudScans, Action::CloudStart, Action::CloudStatus, Action::CloudPull]
		{
			assert!(matches!(service.execute_sync(empty_params(action)), Err(Fault::Unavailable)));
		}
		let reopened = SecurityScanService::new(root.clone(), &state_dir);
		assert!(
			reopened
				.state
				.lock()
				.as_ref()
				.expect("reopened state")
				.scans
				.contains_key(scan_id)
		);
		fs::remove_dir_all(root).expect("remove fixture");
		fs::remove_dir_all(state_dir).expect("remove state");
	}

	#[tokio::test]
	async fn security_url_lists_reads_and_completes_real_scan_state() {
		let (root, state_dir) = fixture();
		fs::write(root.join("leak.txt"), "-----BEGIN PRIVATE KEY-----\n").expect("security fixture");
		let service = SecurityScanService::new(root.clone(), &state_dir);
		let preflight = service
			.preflight(Params {
				action: Action::Preflight,
				target_kind: Some(TargetKind::Repository),
				..empty_params(Action::Preflight)
			})
			.expect("preflight");
		let plan_id = preflight.data["plan"]["id"].as_str().expect("plan id");
		let started = service
			.start(Params { plan_id: Some(Str::new(plan_id)), ..empty_params(Action::Start) })
			.expect("start");
		let scan_id = started.data["scan"]["id"].as_str().expect("scan id");

		let index = service
			.read("scans", &ParsedSelector::None)
			.await
			.expect("security scan index");
		let index = std::str::from_utf8(&index).expect("UTF-8 index");
		assert!(index.contains(scan_id), "{index}");
		assert!(index.contains("1 finding(s)"), "{index}");

		let listing = service
			.list(&format!("scans/{scan_id}"), 10, 64 * 1024)
			.await
			.expect("scan resource listing");
		assert_eq!(
			listing
				.entries
				.iter()
				.map(|entry| entry.name.as_str())
				.collect::<Vec<_>>(),
			["manifest", "findings", "report"]
		);
		let report = service
			.read(&format!("scans/{scan_id}/report"), &ParsedSelector::None)
			.await
			.expect("security report");
		let report = std::str::from_utf8(&report).expect("UTF-8 report");
		assert!(report.contains("private-key"), "{report}");
		assert!(report.contains("leak.txt:1"), "{report}");

		let completions = service
			.complete(scan_id, 20)
			.await
			.expect("security completions");
		assert!(
			completions
				.iter()
				.any(|completion| completion.value == format!("security://scans/{scan_id}/report"))
		);

		fs::remove_dir_all(root).expect("remove fixture");
		fs::remove_dir_all(state_dir).expect("remove state");
	}

	fn empty_params(action: Action) -> Params {
		serde_json::from_value(json!({"action": action})).expect("empty params")
	}
}
