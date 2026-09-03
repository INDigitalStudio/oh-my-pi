//! Typed card for parallel subagent task batches.

use omp_core::{Str, sf};
use omp_dom::{Node, PropId};
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde_json::Value;

use super::{
	Card, CardStatus, CardView, Component, elapsed_badge, partial_string, typed_input, typed_result,
};

/// Agent rows a collapsed batch call shows; the rest fold into one
/// `… N more agents` line (pi `COLLAPSED_AGENT_LIMIT`).
const COLLAPSED_AGENT_LIMIT: usize = 4;
/// Characters of the assignment's first line a call row previews (pi
/// `previewLine(brief, 64)`).
const BRIEF_CHARS: usize = 64;

/// The call arguments as far as they have arrived (pi `repairTaskParams`):
/// the parsed object once complete, else the string fields a torn object
/// already names, so the live card can show the dispatched agent and its
/// brief while the provider is still streaming.
struct CallArgs {
	agent:    Option<Str>,
	name:     Option<Str>,
	task:     Option<Str>,
	context:  Option<Str>,
	isolated: bool,
	tasks:    Vec<Value>,
}

impl CallArgs {
	fn read(view: &CardView<'_>) -> Self {
		let Some(args) = typed_input::<omp_tools::task::Params>(view) else {
			let text = view.args_text().unwrap_or_default();
			return Self {
				agent:    partial_string(text, "agent"),
				name:     partial_string(text, "name"),
				task:     partial_string(text, "task"),
				context:  partial_string(text, "context"),
				isolated: false,
				tasks:    Vec::new(),
			};
		};
		let string = |key: &str| {
			args
				.get(key)
				.and_then(Value::as_str)
				.map(str::trim)
				.filter(|text| !text.is_empty())
				.map(Str::new)
		};
		Self {
			agent:    string("agent"),
			name:     string("name"),
			task:     string("task"),
			context:  string("context"),
			isolated: args.get("isolated").and_then(Value::as_bool) == Some(true),
			tasks:    args
				.get("tasks")
				.and_then(Value::as_array)
				.cloned()
				.unwrap_or_default(),
		}
	}

	/// The assignment a settled child was given: the flat form's `task`, else
	/// the batch item named like the child, else the item at its index.
	fn assignment_for(&self, index: usize, id: &str) -> Option<Str> {
		if self.tasks.is_empty() {
			return self.task.clone();
		}
		let string = |item: &Value, key: &str| item.get(key).and_then(Value::as_str).map(Str::new);
		self
			.tasks
			.iter()
			.find(|item| string(item, "name").is_some_and(|name| task_id(&name) == id))
			.or_else(|| self.tasks.get(index))
			.and_then(|item| string(item, "task"))
	}

	/// The frame's leading sections (pi `createContextSectionRenderer` +
	/// `createAssignmentSectionRenderer`): the shared batch context, then the
	/// assignment brief, as muted markdown.
	fn sections(&self) -> Vec<Component> {
		[&self.context, &self.task]
			.into_iter()
			.flatten()
			.map(|text| dom! { <md fg=muted>{text.clone()}</md> }.into_component())
			.collect()
	}
}

/// Parallel subagent task card.
pub struct TaskCard;

impl Card for TaskCard {
	fn tool(&self) -> &'static str {
		"task"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
		let _fault = view.fault::<omp_tools::task::Fault>();
		match view.status {
			CardStatus::StreamingArgs | CardStatus::InProgress => render_live(view),
			CardStatus::Done | CardStatus::Failed => render_settled(view, expanded, ui),
		}
	}
}

/// The call frame while arguments stream or the children run (pi
/// `renderCall`): `Task: <agent>` for the flat form, the context and
/// assignment markdown, then a divider and one `• name: brief ⟨agent⟩` row
/// per dispatched agent.
fn render_live(view: &CardView<'_>) -> Component {
	let args = CallArgs::read(view);
	let title = args
		.agent
		.as_deref()
		.map_or_else(|| Str::new_static("Task"), |agent| sf!("Task: {agent}"));
	let sections = args.sections();
	let rows = call_rows(&args);
	dom! {
		<box border=round title_pad=3 pad="0 1">
			<row kind=title gap=1 bold>
				<i:task/><text bold>{title}</text>
				if args.isolated { <text fg=muted>{"isolated"}</text> }
				if let Some(badge) = elapsed_badge(view) { {badge} }
			</row>
			{sections}
			if !rows.is_empty() {
				<hr/>
				{rows}
			}
		</box>
	}
	.into_component()
}

/// pi `renderTaskCallLines`: the flat form's single `• name: brief ⟨agent⟩`
/// row, then the batch items (`#N` when unnamed, `[isolated]` when so),
/// capped at [`COLLAPSED_AGENT_LIMIT`] with a `… N more agents` fold.
fn call_rows(args: &CallArgs) -> Vec<Component> {
	let mut rows = Vec::with_capacity(args.tasks.len().min(COLLAPSED_AGENT_LIMIT) + 2);
	let brief = args.task.as_deref().and_then(first_line);
	if args.name.is_some() || brief.is_some() {
		let label = args
			.name
			.as_deref()
			.map_or_else(|| Str::new_static("agent"), task_id);
		rows.push(call_row(label, brief, args.agent.as_deref(), false));
	}
	let shown = args.tasks.len().min(COLLAPSED_AGENT_LIMIT);
	for (index, item) in args.tasks.iter().take(shown).enumerate() {
		let label = item
			.get("name")
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|name| !name.is_empty())
			.map_or_else(|| sf!("#{}", index + 1), task_id);
		let brief = item
			.get("task")
			.and_then(Value::as_str)
			.and_then(first_line);
		let agent = item.get("agent").and_then(Value::as_str);
		let isolated = item.get("isolated").and_then(Value::as_bool) == Some(true);
		rows.push(call_row(label, brief, agent, isolated));
	}
	if shown < args.tasks.len() {
		let more = sf!("… {} more agents", args.tasks.len() - shown);
		rows.push(
			dom! { <row gap=1><text fg=muted>{"•"}</text><text fg=muted>{more}</text></row> }
				.into_component(),
		);
	}
	rows
}

fn call_row(label: Str, brief: Option<Str>, agent: Option<&str>, isolated: bool) -> Component {
	let name = brief
		.as_ref()
		.map_or_else(|| label.clone(), |_| sf!("{label}:"));
	let badge = agent
		.map(str::trim)
		.filter(|agent| !agent.is_empty() && *agent != "task")
		.map(|agent| sf!("⟨{agent}⟩"));
	dom! {
		<row gap=1>
			<text fg=muted>{"•"}</text>
			<text bold fg=accent>{name}</text>
			if let Some(brief) = brief { <text fg=muted>{brief}</text> }
			if let Some(badge) = badge { <text fg=muted>{badge}</text> }
			if isolated { <text fg=muted>{"[isolated]"}</text> }
		</row>
	}
	.into_component()
}

/// pi `taskFirstLine` + `previewLine(brief, 64)`: the first line of the
/// assignment with whitespace runs collapsed, cut to [`BRIEF_CHARS`] with an
/// ellipsis.
fn first_line(task: &str) -> Option<Str> {
	let line = task.trim().lines().next()?;
	let mut collapsed = String::with_capacity(line.len());
	for word in line.split_whitespace() {
		if !collapsed.is_empty() {
			collapsed.push(' ');
		}
		collapsed.push_str(word);
	}
	(!collapsed.is_empty()).then(|| preview(&collapsed, BRIEF_CHARS))
}

/// pi `formatTaskId`: nesting levels (`Anna.Bob`) render as a `>` breadcrumb.
fn task_id(name: &str) -> Str {
	if name.contains('.') {
		Str::from(name.replace('.', ">"))
	} else {
		Str::new(name)
	}
}

fn render_settled(view: &CardView<'_>, expanded: bool, ui: &UiContext) -> Component {
	let result = typed_result::<omp_tools::task::Payload>(view).unwrap_or(Value::Null);
	let args = CallArgs::read(view);
	let sections = args.sections();
	let sections_empty = sections.is_empty();
	if let Some(jobs) = result.get("jobs").and_then(Value::as_array) {
		return render_started(jobs, sections, ui);
	}
	let rows = result
		.get("results")
		.or_else(|| result.get("children"))
		.and_then(Value::as_array)
		.map(Vec::as_slice)
		.unwrap_or_default();
	if rows.is_empty() {
		let fault = diag_text(view.diag).unwrap_or_else(|| Str::new_static("operation failed"));
		let title = sf!("{} Task 1 agent", ui.charset.icon_named("error").unwrap_or("[!!]"));
		return dom! {
			<box border=round title={title} title_pad=3 pad="0 1">
				{sections}
				if !sections_empty { <hr/> }
				<text fg=err pad-x=2>{fault}</text>
			</box>
		}
		.into_component();
	}
	let failed = rows.iter().any(row_failed);
	let count = rows.len();
	let agent_word = if count == 1 { "agent" } else { "agents" };
	let title = sf!(
		"{} Task {count} {agent_word}",
		ui.charset
			.icon_named(if failed { "error" } else { "done" })
			.unwrap_or(if failed { "[!!]" } else { "*" })
	);
	let mut rendered_rows = Vec::with_capacity(rows.len());
	for (index, row) in rows.iter().enumerate() {
		let job = row
			.get("job")
			.or_else(|| row.get("id"))
			.and_then(Value::as_str)
			.map_or_else(|| Str::new_static("agent"), task_id);
		// pi's row is `id: description`; omp's child request carries no
		// description, so the brief of its assignment stands in, as pi's own
		// running rows do without one.
		let assignment = row
			.get("assignment")
			.and_then(Value::as_str)
			.map(Str::new)
			.or_else(|| args.assignment_for(index, &job));
		let desc = row
			.get("description")
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|text| !text.is_empty())
			.map(|text| preview(text, BRIEF_CHARS))
			.or_else(|| assignment.as_deref().and_then(first_line));
		let ok = !row_failed(row);
		let state = if ok { "⟨done⟩" } else { "⟨failed⟩" };
		let badge = row
			.get("agent")
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|agent| !agent.is_empty() && *agent != "task")
			.map(|agent| sf!("⟨{agent}⟩"));
		let detail = task_detail(row);
		let assignment = assignment
			.as_deref()
			.filter(|text| !text.trim().is_empty())
			.map(|text| preview(text, 70));
		// pi `renderOutputSection`: the child's final text, three rows
		// collapsed, ten expanded, each cut at 70 cells.
		let output = row
			.get("output")
			.and_then(Value::as_str)
			.or_else(|| row.get("text").and_then(Value::as_str))
			.map(str::trim_end)
			.filter(|text| !text.is_empty())
			.map(|text| output_preview(text, if expanded { 10 } else { 3 }));
		let error = row
			.get("error")
			.and_then(Value::as_str)
			.filter(|text| !text.is_empty())
			.map(|text| preview(text, 70));
		rendered_rows.push(
			dom! {
				<col>
					<row gap=1>
						if ok { <i:done/> } else { <i:error/> }
						if desc.is_some() { <text bold>{sf!("{job}:")}</text> } else { <text bold>{job}</text> }
						if let Some(desc) = desc { <text>{desc}</text> }
						<text fg=muted>{state}</text>
						if let Some(badge) = badge { <text fg=muted>{badge}</text> }
						if let Some(detail) = detail {
							<text fg=muted>{"·"}</text><text fg=muted>{detail}</text>
						}
					</row>
					if expanded {
						if let Some(assignment) = assignment {
							<text fg=muted pad-x=2>{"Task"}</text><pre pad-x=4>{assignment}</pre>
						}
					}
					if let Some(output) = output {
						<text fg=muted pad-x=2>{"Output"}</text><pre pad-x=4>{output}</pre>
					}
					if let Some(error) = error { <text fg=err pad-x=2>{error}</text> }
				</col>
			}
			.into_component(),
		);
	}
	let requests: u64 = rows
		.iter()
		.filter_map(|row| row.pointer("/stats/requests")?.as_u64())
		.sum();
	let duration_ms = result
		.get("duration_ms")
		.and_then(Value::as_u64)
		.unwrap_or_else(|| {
			rows
				.iter()
				.filter_map(|row| row.pointer("/stats/duration_ms")?.as_u64())
				.sum()
		});
	let status = if failed { "failed" } else { "succeeded" };
	let summary = sf!("⟨{count} {status} · {requests} req · {:.1}s⟩", duration_ms as f64 / 1_000.0);
	dom! {
		<box border=round title={title} title_pad=3 pad="0 1">
			{sections}
			if !sections_empty { <hr/> }
			{rendered_rows}
			<text fg=muted>{summary}</text>
		</box>
	}
	.into_component()
}

/// `Payload::Started`: every child was admitted as a detached runtime job
/// (ADR 0010) and settles later through `<meta><jobs>`. pi
/// (`task/render.ts` `renderAgentProgress`) keeps such rows static — the
/// same dot finished rows use, the id, the agent badge, and the job state —
/// never an error panel: the spawn itself succeeded.
fn render_started(jobs: &[Value], sections: Vec<Component>, ui: &UiContext) -> Component {
	let sections_empty = sections.is_empty();
	let count = jobs.len();
	let agent_word = if count == 1 { "agent" } else { "agents" };
	let title = sf!("{} Task {count} {agent_word}", ui.charset.icon_named("pending").unwrap_or("…"));
	let mut rendered_rows = Vec::with_capacity(count);
	for job in jobs {
		let id = Str::new(job.get("id").and_then(Value::as_str).unwrap_or("agent"));
		let agent = job
			.get("agent")
			.and_then(Value::as_str)
			.filter(|agent| !agent.is_empty() && *agent != "task")
			.map(|agent| sf!("⟨{agent}⟩"));
		let state = sf!(
			"⟨{}⟩",
			job.get("status")
				.and_then(Value::as_str)
				.unwrap_or("started")
		);
		let session = job
			.get("session_path")
			.and_then(Value::as_str)
			.filter(|path| !path.is_empty())
			.map(Str::new);
		rendered_rows.push(
			dom! {
				<row gap=1>
					<i:done/>
					<text bold>{sf!("{id}:")}</text>
					if let Some(agent) = agent { <text fg=muted>{agent}</text> }
					<text fg=muted>{state}</text>
					if let Some(session) = session {
						<text fg=muted>{"·"}</text><text fg=muted>{session}</text>
					}
				</row>
			}
			.into_component(),
		);
	}
	let summary = sf!("⟨{count} started · detached⟩");
	dom! {
		<box border=round title={title} title_pad=3 pad="0 1">
			{sections}
			if !sections_empty { <hr/> }
			{rendered_rows}
			<text fg=muted>{summary}</text>
		</box>
	}
	.into_component()
}

fn row_failed(row: &Value) -> bool {
	row.get("error").is_some_and(|value| !value.is_null())
		|| row
			.get("exit")
			.and_then(Value::as_i64)
			.is_some_and(|exit| exit != 0)
}

fn task_detail(row: &Value) -> Option<Str> {
	let stats = row.get("stats")?;
	let requests = stats.get("requests").and_then(Value::as_u64)?;
	let context = stats
		.get("context_tokens")
		.and_then(Value::as_u64)
		.unwrap_or_default();
	let window = stats
		.get("context_window")
		.and_then(Value::as_u64)
		.unwrap_or_default();
	let cost = stats
		.get("cost_nano_usd")
		.and_then(Value::as_u64)
		.unwrap_or_default();
	let duration = stats
		.get("duration_ms")
		.and_then(Value::as_u64)
		.unwrap_or_default();
	let window_label = if window >= 1_000 && window.is_multiple_of(1_000) {
		sf!("{}K", window / 1_000)
	} else {
		sf!("{window}")
	};
	let percent = if window == 0 {
		0.0
	} else {
		context as f64 * 100.0 / window as f64
	};
	Some(sf!(
		"{requests} req · {percent:.1}%/{window_label} · ${:.2} · {:.1}s",
		cost as f64 / 1_000_000_000.0,
		duration as f64 / 1_000.0
	))
}

/// The first `limit` output lines, each cut at 70 cells, and pi's
/// `… N more lines` fold when more follow.
fn output_preview(text: &str, limit: usize) -> Str {
	let total = text.lines().count();
	let mut out = text
		.lines()
		.take(limit)
		.map(|line| preview(line, 70))
		.collect::<Vec<_>>()
		.join("\n");
	if total > limit {
		out.push_str(&sf!("\n… {} more lines", total - limit));
	}
	Str::from(out)
}

fn preview(text: &str, max_chars: usize) -> Str {
	let lines = text
		.lines()
		.map(|line| {
			if line.chars().count() <= max_chars {
				line.to_owned()
			} else {
				let mut cut: String = line.chars().take(max_chars.saturating_sub(1)).collect();
				cut.push('…');
				cut
			}
		})
		.collect::<Vec<_>>()
		.join("\n");
	Str::new(lines)
}

fn diag_text(node: Option<&Node>) -> Option<Str> {
	node.and_then(|node| {
		node.content.clone().or_else(|| {
			node
				.prop(&PropId::Text.into())
				.and_then(|value| value.as_str())
				.map(Str::new)
		})
	})
}
