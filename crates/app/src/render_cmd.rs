//! Headless durable-session replay through the production transcript projection
//! and chat scene.

use std::{
	env, fs,
	io::{self, Write as _},
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, Instant},
};

use clap::Args;
use miette::{IntoDiagnostic as _, miette};
use omp_chat::{
	HostMailbox, HostOptions, ModelBadge, NativeHost, overlays::NoServices, welcome::WelcomeFacts,
};
use omp_core::Str;
use omp_dom::{KnownTag, PropId, Tag, Value};
use omp_tui::{Frame, Size, UiContext, frame_ansi, frame_text, slots::ResizePolicy};

/// Headless transcript replay and finalized-history rendering options.
#[derive(Clone, Debug, Args)]
pub struct RenderArgs {
	/// Session journal path or project-local session ID prefix.
	#[arg(value_name = "SESSION")]
	pub session: Option<Str>,
	/// Render width in terminal columns.
	#[arg(long, short = 'w')]
	pub width:   Option<u16>,
	/// Print phase timings and rendered row counts to standard error.
	#[arg(long, short = 't')]
	pub timing:  bool,
	/// Benchmark this many extra pure finalized-history batch renders.
	#[arg(long, value_name = "N")]
	pub repaint: Option<u32>,
	/// Strip ANSI styling from transcript output.
	#[arg(long)]
	pub plain:   bool,
	/// Suppress transcript output for timing-only runs.
	#[arg(long, short = 'q')]
	pub quiet:   bool,
}

/// Files produced by `omp --export <SESSION_OMS>`.
pub struct ExportedSession {
	/// Validated native journal copy.
	pub journal:    PathBuf,
	/// Pure transcript projection.
	pub transcript: PathBuf,
}

struct RenderOutput {
	path:          PathBuf,
	transcript:    String,
	source_bytes:  u64,
	items:         usize,
	rows:          u16,
	open:          Duration,
	project:       Duration,
	replay:        Duration,
	batch_render:  Duration,
	repaint_times: Vec<Duration>,
}

/// Exports a validated journal copy and its pure text projection.
pub fn export_session(
	selector: &Path,
	data_dir: &Path,
	cwd: &Path,
) -> miette::Result<ExportedSession> {
	let selector = selector.to_string_lossy();
	let source = resolve_target(Some(&selector), data_dir, cwd)?;
	let mut session =
		omp_session::Session::open(&source, omp_session::ComponentRegistry::standard())
			.into_diagnostic()?;
	let stem = source
		.file_stem()
		.and_then(|value| value.to_str())
		.unwrap_or("session");
	let journal = cwd.join(format!("{stem}.export.oms"));
	let transcript = cwd.join(format!("{stem}.txt"));
	if source != journal {
		fs::copy(&source, &journal).into_diagnostic()?;
	}
	fs::write(&transcript, production_transcript(&mut session, 100, true, cwd)?)
		.into_diagnostic()?;
	Ok(ExportedSession { journal, transcript })
}

/// Replays one session, writes its materialized transcript, and optionally
/// reports phase costs.
pub fn run(args: RenderArgs, data_dir: &Path) -> miette::Result<()> {
	if args.width == Some(0) {
		return Err(miette!("--width must be greater than zero"));
	}
	if args.repaint == Some(0) {
		return Err(miette!("--repaint must be a positive integer"));
	}
	let cwd = env::current_dir().into_diagnostic()?;
	let _ctx = crate::process_ctx(&cwd)?;
	let output = render_session(&args, data_dir, &cwd)?;
	if !args.quiet {
		let mut stdout = io::stdout().lock();
		stdout
			.write_all(output.transcript.as_bytes())
			.into_diagnostic()?;
		if !output.transcript.ends_with('\n') {
			stdout.write_all(b"\n").into_diagnostic()?;
		}
	}
	if args.timing || args.repaint.is_some() {
		eprintln!("{}", timing_report(&output));
	}
	Ok(())
}

fn render_session(args: &RenderArgs, data_dir: &Path, cwd: &Path) -> miette::Result<RenderOutput> {
	let open_start = Instant::now();
	let path = resolve_target(args.session.as_deref(), data_dir, cwd)?;
	let source_bytes = fs::metadata(&path).into_diagnostic()?.len();
	let open = open_start.elapsed();

	let replay_start = Instant::now();
	let mut session =
		omp_session::Session::open(&path, omp_session::ComponentRegistry::standard())
			.into_diagnostic()?;
	let replay = replay_start.elapsed();
	let items = omp_session::project_thread(session.dom()).len();
	let width = args.width.unwrap_or(100);

	let project_start = Instant::now();
	let host = production_host(&mut session, width, cwd)?;
	let project = project_start.elapsed();

	let batch_start = Instant::now();
	let transcript = rendered_transcript(&host, args.plain);
	let batch_render = batch_start.elapsed();
	let rows = u16::try_from(transcript.lines().count()).unwrap_or(u16::MAX);

	let mut repaint_times = Vec::with_capacity(args.repaint.unwrap_or(0) as usize);
	for _ in 0..args.repaint.unwrap_or(0) {
		let start = Instant::now();
		let _ = production_transcript(&mut session, width, args.plain, cwd)?;
		repaint_times.push(start.elapsed());
	}

	Ok(RenderOutput {
		path,
		transcript,
		source_bytes,
		items,
		rows,
		open,
		project,
		replay,
		batch_render,
		repaint_times,
	})
}

fn production_transcript(
	session: &mut omp_session::Session,
	width: u16,
	plain: bool,
	project: &Path,
) -> miette::Result<String> {
	let host = production_host(session, width, project)?;
	Ok(rendered_transcript(&host, plain))
}

fn production_host(
	session: &mut omp_session::Session,
	width: u16,
	project: &Path,
) -> miette::Result<NativeHost> {
	let (snapshot, dom_events) = session.subscribe();
	let (_, kernel_events) = flume::unbounded();
	let (commands, _) = flume::unbounded();
	let (up, _) = flume::unbounded();
	let con = Arc::new(HostMailbox::new().attach(omp_con::Ctx::builder()).build());
	con.run("cl_startup_quiet 1").into_diagnostic()?;
	let model = session
		.dom()
		.children(session.dom().body())
		.iter()
		.flat_map(|turn| session.dom().children(*turn))
		.find_map(|handle| {
			let node = session.dom().get(*handle)?;
			(node.tag == Tag::Known(KnownTag::Assistant))
				.then(|| node.prop(&PropId::Model.into()).and_then(Value::as_str))
				.flatten()
		})
		.unwrap_or("session");
	Ok(NativeHost::new(
		HostOptions {
			snapshot,
			dom_events,
			kernel_events,
			commands,
			up,
			con,
			models: Vec::new(),
			cycle: Vec::new(),
			resize_policy: ResizePolicy::Rebuild,
			model: ModelBadge::from_identifier(model),
			project: project.to_path_buf(),
			welcome: WelcomeFacts::default(),
			ui: UiContext::default(),
			services: Arc::new(NoServices),
			speech: None,
			resuming: true,
			initial_panel: None,
		},
		Size::new(width, 32),
	))
}

fn rendered_transcript(host: &NativeHost, plain: bool) -> String {
	let status_rows = host.status_frame().map_or(0, |frame| frame.size().height);
	let transcript_rows = host
		.frame()
		.size()
		.height
		.saturating_sub(status_rows)
		.saturating_sub(host.editor_rows());
	let mut transcript = Frame::new(Size::new(host.frame().size().width, transcript_rows));
	transcript.blit(host.frame(), 0, transcript_rows, 0, 0);
	if plain {
		frame_text(&transcript)
	} else {
		frame_ansi(&transcript)
	}
}

fn resolve_target(selector: Option<&str>, data_dir: &Path, cwd: &Path) -> miette::Result<PathBuf> {
	if let Some(selector) = selector {
		let candidate = Path::new(selector);
		if candidate.is_file() {
			return fs::canonicalize(candidate).into_diagnostic();
		}
		if candidate.components().count() > 1 || selector.ends_with(".oms") {
			return Err(miette!("session file not found: {}", candidate.display()));
		}
	}

	let root = fs::canonicalize(cwd).into_diagnostic()?;
	let sessions_dir = omp_env::project_state::directory(data_dir, &root)
		.into_diagnostic()?
		.join("sessions");
	let mut journals = fs::read_dir(&sessions_dir)
		.into_diagnostic()?
		.filter_map(Result::ok)
		.map(|entry| entry.path())
		.filter(|path| path.extension().is_some_and(|extension| extension == "oms"))
		.collect::<Vec<_>>();
	if let Some(selector) = selector {
		journals.retain(|path| {
			path
				.file_stem()
				.and_then(|name| name.to_str())
				.is_some_and(|name| name.starts_with(selector))
		});
		if journals.len() > 1 {
			return Err(miette!("session \"{selector}\" is ambiguous"));
		}
		return journals
			.pop()
			.ok_or_else(|| miette!("session \"{selector}\" not found"));
	}
	journals.sort_by_key(|path| {
		fs::metadata(path)
			.and_then(|metadata| metadata.modified())
			.ok()
	});
	journals
		.pop()
		.ok_or_else(|| miette!("no sessions found for {}", root.display()))
}

fn timing_report(output: &RenderOutput) -> String {
	let mut report = vec![
		format!("session  {}", output.path.display()),
		format!(
			"         {}, {} items, {} transcript rows",
			format_bytes(output.source_bytes),
			output.items,
			output.rows
		),
		format!("open     {}", format_duration(output.open)),
		format!("project  {}  (journal live-set projection)", format_duration(output.project)),
		format!("replay   {}  (production backend event projection)", format_duration(output.replay)),
		format!("batch    {}  (finalized-history render)", format_duration(output.batch_render),),
	];
	if !output.repaint_times.is_empty() {
		let total: Duration = output.repaint_times.iter().copied().sum();
		let average = total / output.repaint_times.len() as u32;
		report.push(format!(
			"repaint  {} avg over {} pure batch renders",
			format_duration(average),
			output.repaint_times.len(),
		));
	}
	report.join("\n")
}

fn format_duration(duration: Duration) -> String {
	format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
}

fn format_bytes(bytes: u64) -> String {
	if bytes >= 1024 * 1024 {
		format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
	} else if bytes >= 1024 {
		format!("{:.1} KiB", bytes as f64 / 1024.0)
	} else {
		format!("{bytes} B")
	}
}

#[cfg(test)]
mod tests {
	use omp_dom::{KnownTag, PropId, Tag};
	use serde_json::value::RawValue;
	use tempfile::tempdir;

	use super::*;

	#[test]
	fn fixture_replays_deterministically_through_the_chat_scene() {
		let scratch = tempdir().expect("scratch");
		let root = scratch.path().join("project");
		fs::create_dir(&root).expect("project");
		let path = scratch.path().join("fixture.oms");
		let mut session =
			omp_session::Session::create(&path, omp_session::ComponentRegistry::standard())
				.expect("fixture journal");
		session.begin_turn().expect("turn");
		session.user("hello fixture", Vec::new()).expect("user");
		session
			.assistant_start("fixture/model", "fixture", "fixture/model")
			.expect("assistant");
		let turn = *session
			.dom()
			.children(session.dom().body())
			.last()
			.expect("turn node");
		let assistant = session
			.dom()
			.children(turn)
			.iter()
			.copied()
			.find(|handle| {
				session
					.dom()
					.get(*handle)
					.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
			})
			.expect("assistant node");
		let stream = session
			.stream_open(assistant, PropId::Text.into())
			.expect("open text");
		session
			.stream_append(stream, "hello back")
			.expect("append text");
		session.stream_close(stream).expect("close text");
		session.assistant_end("tool_calls").expect("finish assistant");
		let call = session
			.call(
				"custom_tool",
				1,
				"call-render",
				None,
				Some(
					RawValue::from_string(
						r#"{"i":"Inspecting fixture","path":"a/very/long/fixture/path.txt"}"#
							.to_owned(),
					)
					.expect("args"),
				),
				None,
			)
			.expect("tool call");
		session
			.settle(
				call,
				RawValue::from_string(
					r#"{"content":[{"type":"text","text":"tool result body"}]}"#.to_owned(),
				)
				.expect("outcome"),
			)
			.expect("tool result");
		drop(session);
		let args = RenderArgs {
			session: Some(Str::from(path.to_string_lossy().as_ref())),
			width:   Some(80),
			timing:  true,
			repaint: Some(1),
			plain:   true,
			quiet:   false,
		};
		let first = render_session(&args, scratch.path(), &root).expect("first replay");
		let second = render_session(&args, scratch.path(), &root).expect("second replay");
		assert_eq!(first.transcript, second.transcript);
		assert!(first.transcript.contains("hello fixture"), "user block missing");
		assert!(first.transcript.contains("hello back"), "assistant block missing");
		assert!(first.transcript.contains("custom_tool"), "tool card missing");
		assert!(first.transcript.contains("tool result body"), "tool result missing");
		assert!(!first.transcript.contains('\u{1b}'), "--plain leaked ANSI");

		let mut narrow = args.clone();
		narrow.width = Some(24);
		let narrow = render_session(&narrow, scratch.path(), &root).expect("narrow replay");
		assert_ne!(first.transcript, narrow.transcript, "--width did not change layout");
		assert!(
			narrow
				.transcript
				.lines()
				.all(|line| omp_tui::cell_width(line) <= 24),
			"rendered line exceeded requested width",
		);

		let mut styled = args.clone();
		styled.plain = false;
		let styled = render_session(&styled, scratch.path(), &root).expect("styled replay");
		assert!(styled.transcript.contains('\u{1b}'), "styled render omitted ANSI");

		let timing = timing_report(&first);
		assert!(timing.contains("open") && timing.contains("project") && timing.contains("replay"));
		assert!(timing.contains("batch") && timing.contains("repaint"));
	}
}
