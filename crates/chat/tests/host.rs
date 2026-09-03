//! Session-DOM projection laws for the interactive chat actor.

use std::sync::Arc;

use omp_agent::{ApprovalBook, ApprovalScope, ApprovalSpec};
use omp_chat::{
	BlockKind, CtrlCAction, HostCommand, HostOptions, NativeEffect, NativeHost, block_views,
	ctrl_c_action, overlays::Overlays,
};
use omp_dom::{Dom, Event, KnownTag, PropId, Tag};
use omp_session::{ComponentRegistry, Session};
use omp_tui::{Key, Size, UiContext, slots::ResizePolicy};
use tempfile::tempdir;

fn fixture() -> (Session, omp_journal::EntryId) {
	let directory = tempdir().expect("temp directory");
	let path = directory.keep().join("fixture.oms");
	let mut session = Session::create(path, ComponentRegistry::standard()).expect("create session");
	let genesis = session.head().expect("genesis");
	session.begin_turn().expect("begin turn");
	session.user("hello", Vec::new()).expect("user");
	session
		.assistant_start("test/model", "test", "test/model")
		.expect("assistant start");
	let turn = *session
		.dom()
		.children(session.dom().body())
		.last()
		.expect("turn");
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
		.expect("assistant");
	let thinking = session
		.stream_open(assistant, PropId::Thinking.into())
		.expect("thinking stream");
	session
		.stream_append(thinking, "considering")
		.expect("thinking delta");
	session.stream_close(thinking).expect("thinking close");
	let text = session
		.stream_open(assistant, PropId::Text.into())
		.expect("text stream");
	session.stream_append(text, "answer").expect("text delta");
	session.stream_close(text).expect("text close");
	// A settled fixture: the host now reads turn activity from the snapshot
	// at boot (a `tool_calls` stop would mean another inference follows).
	session.assistant_end("stop").expect("assistant end");
	let args =
		serde_json::value::to_raw_value(&serde_json::json!({"path":"note.txt"})).expect("args");
	let call = session
		.call("read", 1, "call-1", Some("read fixture".into()), Some(args), None)
		.expect("tool call");
	let outcome = serde_json::value::to_raw_value(&serde_json::json!({"text":"hello from fixture"}))
		.expect("outcome");
	session.settle(call, outcome).expect("tool result");
	session
		.receipt(omp_journal::data::TurnReceipt::tokens(12, 7, 0))
		.expect("receipt");
	(session, genesis)
}

#[test]
fn fixture_session_projects_expected_block_sequence() {
	let (session, _) = fixture();
	let blocks = block_views(session.dom(), true);
	assert_eq!(blocks.iter().map(|block| block.kind).collect::<Vec<_>>(), [
		BlockKind::User,
		BlockKind::Thinking,
		BlockKind::Assistant,
		BlockKind::Tool,
		BlockKind::Usage,
	]);
	assert_eq!(blocks[0].text, "hello");
	assert_eq!(blocks[1].text, "considering");
	assert_eq!(blocks[2].text, "answer");
	assert!(blocks[3].text.contains("hello from fixture"));
	// pi usage row: `YYYY-MM-DD HH:mm:ss  ⏱Δ<wait>  ⤵ <in>  ⤴ <out>` (USG-01..03).
	let usage = blocks[4].text.as_str();
	let mut parts = usage.split("  ");
	let stamp = parts.next().expect("timestamp part");
	assert_eq!(stamp.len(), 19, "{usage}");
	assert!(stamp.as_bytes()[4] == b'-' && stamp.as_bytes()[10] == b' ', "{usage}");
	let rest = parts.collect::<Vec<_>>();
	assert!(rest.iter().any(|part| part.contains('Δ')), "{usage}");
	assert!(rest.iter().any(|part| part.ends_with(" 12")), "{usage}");
	assert!(rest.iter().any(|part| part.ends_with(" 7")), "{usage}");
}

#[test]
fn reset_after_rewind_rebuilds_actor_blocks() {
	let (mut session, genesis) = fixture();
	let (snapshot, events) = session.subscribe();
	let mut replica = Dom::from_snapshot(&snapshot);
	assert!(!block_views(&replica, true).is_empty());

	session.rewind(genesis).expect("rewind");
	let event = events.recv().expect("reset event");
	assert!(matches!(event, Event::Reset { .. }));
	replica.apply_event(&event).expect("apply reset");
	assert!(block_views(&replica, true).is_empty());
}

#[test]
fn ctrl_c_interrupts_active_turn_and_quits_when_idle_or_repeated() {
	assert_eq!(ctrl_c_action(true, false), CtrlCAction::Interrupt);
	assert_eq!(ctrl_c_action(false, false), CtrlCAction::Quit);
	assert_eq!(ctrl_c_action(true, true), CtrlCAction::Quit);
}

#[test]
fn ctrl_c_during_an_active_turn_emits_one_interrupt_and_stays_open() {
	let (mut host, commands) = bound_host(vec![row("test/model", &[])]);
	host.key(Key::Char('g')).expect("type");
	host.key(Key::Char('o')).expect("type");
	assert_eq!(host.key(Key::Enter).expect("enter"), NativeEffect::Consumed);
	assert!(matches!(commands.recv().expect("submit"), HostCommand::Submit(text) if text == "go"));
	assert_ne!(host.key(Key::Ctrl('c')).expect("ctrl+c"), NativeEffect::Quit);
	assert!(matches!(commands.recv().expect("interrupt"), HostCommand::Interrupt));
	assert!(commands.try_recv().is_err(), "one ctrl+c posts exactly one interrupt");
	assert_eq!(host.key(Key::Char('!')).expect("type"), NativeEffect::Consumed);
	// Idle ctrl+c (no active turn) still quits, per `ctrl_c_action`.
	let (mut idle, idle_commands) = bound_host(vec![row("test/model", &[])]);
	assert_eq!(idle.key(Key::Ctrl('c')).expect("ctrl+c"), NativeEffect::Quit);
	assert!(idle_commands.try_recv().is_err());
}

#[test]
fn pending_approval_projects_overlay_and_hotkeys() {
	let directory = tempdir().expect("temp directory");
	let path = directory.path().join("approval.oms");
	let mut session =
		Session::create(path, ComponentRegistry::standard()).expect("create approval session");
	let ticket = ApprovalBook::default()
		.open(&mut session, ApprovalSpec {
			title:         "Run command".into(),
			body:          "The command changes the project.".into(),
			subject:       "cargo fix".into(),
			kind:          "exec".into(),
			scopes:        vec!["once".into()],
			default:       None,
			route:         "user".into(),
			approver:      None,
			timeout_ms:    0,
			unreachable:   "deny".into(),
			require_human: true,
			pattern:       None,
			evidence:      Vec::new(),
		})
		.expect("open approval");
	let mut overlays = Overlays::default();
	overlays.sync_approval(session.dom());
	let approval = overlays.approval().expect("approval overlay");
	assert_eq!(approval.id, ticket.ticket_id);
	assert_eq!(approval.title, "Run command");
	assert!(!approval.decision('n').expect("deny").approved);
	assert_eq!(approval.decision('a').expect("session approval").scope, ApprovalScope::Session);
	assert!(approval.decision('y').expect("approve").approved);

	let (snapshot, dom_events) = session.subscribe();
	let (_, kernel_events) = flume::unbounded();
	let (commands, command_rx) = flume::unbounded();
	let (up, _) = flume::unbounded();
	let mut host = NativeHost::new(
		HostOptions {
			model: omp_chat::ModelBadge::from_identifier("test/model"),
			snapshot,
			dom_events,
			kernel_events,
			commands,
			up,
			con: Arc::new(omp_con::Ctx::new()),
			models: Vec::new(),
			cycle: Vec::new(),
			resize_policy: ResizePolicy::Rebuild,
			project: std::path::PathBuf::new(),
			welcome: omp_chat::welcome::WelcomeFacts::default(),
			ui: UiContext::default(),
			services: Arc::new(omp_chat::overlays::NoServices),
			speech: None,
			resuming: false,
			initial_panel: None,
		},
		Size::new(80, 24),
	);
	assert_eq!(host.key(Key::Char('x')).expect("non-choice approval key"), NativeEffect::Consumed);
	assert_eq!(host.composer_text(), "", "non-choice keys never reach the hidden composer");
	assert!(command_rx.try_recv().is_err(), "non-choice key does not decide");
	assert_eq!(host.key(Key::Esc).expect("deny approval"), NativeEffect::Consumed);
	match command_rx.recv().expect("approval command") {
		HostCommand::Approve { id, decision } => {
			assert_eq!(id, ticket.ticket_id);
			assert!(!decision.approved);
			assert_eq!(decision.scope, ApprovalScope::Once);
		},
		other => panic!("unexpected host command: {other:?}"),
	}
	assert!(!host.overlay_open(), "Escape resolves instead of merely hiding approval");
}

fn bound_host(models: Vec<omp_chat::ModelRow>) -> (NativeHost, flume::Receiver<HostCommand>) {
	let (host, commands, _) = bound_host_with_session(models);
	(host, commands)
}

fn bound_host_with_session(
	models: Vec<omp_chat::ModelRow>,
) -> (NativeHost, flume::Receiver<HostCommand>, Session) {
	let (mut session, _) = fixture();
	let (snapshot, dom_events) = session.subscribe();
	let (_, kernel_events) = flume::unbounded();
	let (commands, command_rx) = flume::unbounded();
	let (up, _) = flume::unbounded();
	let con = Arc::new(
		omp_chat::HostMailbox::new()
			.attach(omp_con::Ctx::builder())
			.build(),
	);
	con.run(
		r#"bind alt+p "cl_model_select session"; bind shift+tab cl_thinking_cycle; bind ctrl+r cl_history_search; bind escape cl_interrupt"#,
	)
	.expect("binds");
	let host = NativeHost::new(
		HostOptions {
			model: omp_chat::ModelBadge::from_identifier("test/model"),
			snapshot,
			dom_events,
			kernel_events,
			commands,
			up,
			con,
			models,
			cycle: Vec::new(),
			resize_policy: ResizePolicy::Rebuild,
			project: std::path::PathBuf::new(),
			welcome: omp_chat::welcome::WelcomeFacts::default(),
			ui: UiContext::default(),
			services: Arc::new(omp_chat::overlays::NoServices),
			speech: None,
			resuming: false,
			initial_panel: None,
		},
		Size::new(100, 30),
	);
	(host, command_rx, session)
}

#[test]
fn ctrl_c_reads_turn_activity_from_the_tree_through_receipts_and_notices() {
	use omp_dom::{NodeSpec, Op, Txn, Value};
	let (mut host, commands, mut session) = bound_host_with_session(vec![row("test/model", &[])]);
	// A second turn: the assistant stopped for tool calls, the bash call is
	// running, and the per-inference receipt already landed after it.
	session.begin_turn().expect("begin turn");
	session.user("interrupt me", Vec::new()).expect("user");
	session
		.assistant_start("test/model", "test", "test/model")
		.expect("assistant start");
	let args =
		serde_json::value::to_raw_value(&serde_json::json!({"command":"sleep 30"})).expect("args");
	let call = session
		.call("bash", 1, "slow-shell", None, Some(args), None)
		.expect("tool call");
	session.assistant_end("tool_calls").expect("assistant end");
	session
		.receipt(omp_journal::data::TurnReceipt::tokens(0, 0, 0))
		.expect("receipt");
	let turn = *session
		.dom()
		.children(session.dom().body())
		.last()
		.expect("turn");
	let cause = session.head().expect("head");
	session
		.patch(Txn {
			cause,
			label: None,
			ops: vec![Op::Ins {
				parent: turn,
				after:  session.dom().children(turn).last().copied(),
				node:   NodeSpec::new(KnownTag::Notice)
					.with_prop(PropId::Kind, Value::Str(omp_core::Str::new_static("info")))
					.with_content(omp_core::Str::new_static("still working")),
			}],
		})
		.expect("informational notice");
	host.poll().expect("apply dom events");
	assert_ne!(
		host.key(Key::Ctrl('c')).expect("ctrl+c"),
		NativeEffect::Quit,
		"an informational notice after an open tool is transparent to lifecycle"
	);
	assert!(matches!(commands.recv().expect("interrupt"), HostCommand::Interrupt));
	assert!(commands.try_recv().is_err());

	// The kernel settles the call as aborted and ends the turn with a notice:
	// the tree now says the turn is over, so ctrl+c quits.
	let fault = serde_json::value::to_raw_value(&serde_json::json!({
		"kind":"aborted","value":{"abort":{"kind":"interrupted","reason":"cancelled"},"kind":"cancelled"}
	}))
	.expect("fault");
	session.fail(call, fault).expect("aborted result");
	let turn = *session
		.dom()
		.children(session.dom().body())
		.last()
		.expect("turn");
	let cause = session.head().expect("head");
	session
		.patch(Txn {
			cause,
			label: None,
			ops: vec![Op::Ins {
				parent: turn,
				after:  session.dom().children(turn).last().copied(),
				node:   NodeSpec::new(KnownTag::Notice)
					.with_prop(PropId::Kind, Value::Str(omp_core::Str::new_static("warn")))
					.with_content(omp_core::Str::new_static("Turn interrupted")),
			}],
		})
		.expect("interrupt notice");
	host.poll().expect("apply dom events");
	std::thread::sleep(std::time::Duration::from_millis(1100));
	assert_eq!(host.key(Key::Ctrl('c')).expect("ctrl+c"), NativeEffect::Quit);
}

fn empty_host(resuming: bool, quiet: bool) -> NativeHost {
	let directory = tempdir().expect("temp directory");
	let mut session =
		Session::create(directory.path().join("empty.oms"), ComponentRegistry::standard())
			.expect("empty session");
	let (snapshot, dom_events) = session.subscribe();
	let (_, kernel_events) = flume::unbounded();
	let (commands, _) = flume::unbounded();
	let (up, _) = flume::unbounded();
	let con = Arc::new(omp_con::Ctx::new());
	if quiet {
		con.run("cl_startup_quiet 1").expect("quiet");
	}
	NativeHost::new(
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
			model: omp_chat::ModelBadge::from_identifier("test/model"),
			project: std::path::PathBuf::new(),
			welcome: omp_chat::welcome::WelcomeFacts::default(),
			ui: UiContext::default(),
			services: Arc::new(omp_chat::overlays::NoServices),
			speech: None,
			resuming,
			initial_panel: None,
		},
		Size::new(80, 24),
	)
}

#[test]
fn explicit_resume_suppresses_intro_even_for_an_empty_journal() {
	let fresh = empty_host(false, false);
	let fresh_welcome = fresh
		.blocks()
		.into_iter()
		.find(|block| block.kind == BlockKind::Welcome)
		.expect("fresh welcome");
	assert!(!fresh_welcome.finalized, "fresh launch begins the mutable intro");

	let resumed = empty_host(true, false);
	let resumed_welcome = resumed
		.blocks()
		.into_iter()
		.find(|block| block.kind == BlockKind::Welcome)
		.expect("resumed welcome");
	assert!(resumed_welcome.finalized, "resume rests immediately without inspecting body history");

	let quiet = empty_host(false, true);
	assert!(
		quiet
			.blocks()
			.into_iter()
			.all(|block| block.kind != BlockKind::Welcome),
		"quiet startup omits the welcome"
	);
}

/// A host over the fixture journal, opened as pi's `--continue`/`--resume`
/// path does (`resuming: true`) or as a fresh launch.
fn fixture_host(resuming: bool) -> NativeHost {
	let (mut session, _) = fixture();
	let (snapshot, dom_events) = session.subscribe();
	let (_, kernel_events) = flume::unbounded();
	let (commands, _) = flume::unbounded();
	let (up, _) = flume::unbounded();
	NativeHost::new(
		HostOptions {
			model: omp_chat::ModelBadge::from_identifier("test/model"),
			snapshot,
			dom_events,
			kernel_events,
			commands,
			up,
			con: Arc::new(omp_con::Ctx::new()),
			models: Vec::new(),
			cycle: Vec::new(),
			resize_policy: ResizePolicy::Rebuild,
			project: std::path::PathBuf::new(),
			welcome: omp_chat::welcome::WelcomeFacts::default(),
			ui: UiContext::default(),
			services: Arc::new(omp_chat::overlays::NoServices),
			speech: None,
			resuming,
			initial_panel: None,
		},
		Size::new(80, 24),
	)
}

/// pi `setHistoryStorage` at `interactive-mode.ts:968`: a resumed session's
/// prompts are Up-arrow history from the first keypress.
#[test]
fn resumed_session_seeds_up_arrow_history_from_the_journal() {
	let mut resumed = fixture_host(true);
	assert_eq!(resumed.composer_text(), "");
	resumed.key(Key::Up).expect("up");
	assert_eq!(resumed.composer_text(), "hello", "the journal's user prompt is recalled");
	resumed.key(Key::End).expect("end");
	resumed.key(Key::Down).expect("down");
	assert_eq!(resumed.composer_text(), "", "Down returns to the empty draft");

	let mut fresh = fixture_host(false);
	fresh.key(Key::Up).expect("up");
	assert_eq!(fresh.composer_text(), "", "a fresh launch has nothing to recall");
}

/// Services stub exposing only the session's `local://` artifacts.
struct LocalArtifacts;

impl omp_chat::overlays::Services for LocalArtifacts {
	fn list_local(
		&self,
		suffix: &str,
	) -> omp_chat::overlays::services::ServiceResult<Vec<omp_core::Str>> {
		Ok(["local://omp2-plan.md", "local://notes.txt"]
			.into_iter()
			.filter(|url| url.ends_with(suffix))
			.map(omp_core::Str::new_static)
			.collect())
	}
}

fn host_with_services(
	services: Arc<dyn omp_chat::overlays::Services>,
) -> (NativeHost, Session, tempfile::TempDir) {
	let directory = tempdir().expect("temp directory");
	let mut session =
		Session::create(directory.path().join("urls.oms"), ComponentRegistry::standard())
			.expect("session");
	let (snapshot, dom_events) = session.subscribe();
	let (_, kernel_events) = flume::unbounded();
	let (commands, _) = flume::unbounded();
	let (up, _) = flume::unbounded();
	let host = NativeHost::new(
		HostOptions {
			model: omp_chat::ModelBadge::from_identifier("test/model"),
			snapshot,
			dom_events,
			kernel_events,
			commands,
			up,
			con: Arc::new(omp_con::Ctx::new()),
			models: Vec::new(),
			cycle: Vec::new(),
			resize_policy: ResizePolicy::Rebuild,
			project: std::path::PathBuf::new(),
			welcome: omp_chat::welcome::WelcomeFacts::default(),
			ui: UiContext::default(),
			services,
			speech: None,
			resuming: false,
			initial_panel: None,
		},
		Size::new(100, 30),
	);
	(host, session, directory)
}

fn type_text(host: &mut NativeHost, text: &str) {
	for character in text.chars() {
		host.key(Key::Char(character)).expect("type");
	}
}

/// pi `internal-url-autocomplete.ts`: typing `scheme://` offers the
/// resources the host can name — `local://` artifacts from the services
/// seam and `agent://` ids from the live `<meta><jobs>` roster, which
/// follows spawns as the replica changes.
#[test]
fn internal_url_tokens_complete_local_artifacts_and_live_agents() {
	use omp_session::components::jobs::{self, JobSpec};
	let (mut host, mut session, _dir) = host_with_services(Arc::new(LocalArtifacts));
	type_text(&mut host, "see local://pl");
	host.key(Key::Tab).expect("accept");
	assert_eq!(host.composer_text(), "see local://omp2-plan.md ");
	host.act(omp_chat::HostAction::Clear).expect("clear draft");
	assert_eq!(host.composer_text(), "");

	// No agents yet: `agent://` declines rather than offering stale rows.
	type_text(&mut host, "agent://");
	host.key(Key::Tab).expect("tab with nothing to accept");
	assert_eq!(host.composer_text(), "agent://");
	host.act(omp_chat::HostAction::Clear).expect("clear draft");

	let cause = session.head().expect("head");
	let txn = jobs::insert(session.dom(), cause, JobSpec {
		id:      "Fx2Composer".into(),
		kind:    "subagent".into(),
		owner:   "Main".into(),
		started: "0".into(),
		agent:   Some("task".into()),
	})
	.expect("jobs component");
	session.patch(txn).expect("spawn agent");
	host.poll().expect("apply spawn");
	type_text(&mut host, "agent://fx2");
	host.key(Key::Tab).expect("accept");
	assert_eq!(host.composer_text(), "agent://Fx2Composer ");

	// An unknown scheme never opens a dropdown, so Tab has nothing to take.
	host.act(omp_chat::HostAction::Clear).expect("clear draft");
	type_text(&mut host, "https://exa");
	host.key(Key::Tab).expect("tab");
	assert_eq!(host.composer_text(), "https://exa");
}

/// pi `applySpellingSettings`: the `cl_spelling_*` convars reach the live
/// editor on the next status sync, not only at boot.
#[test]
fn spelling_convars_reach_the_composer_editor() {
	let mut host = empty_host(false, false);
	assert_eq!(host.spelling_features(), omp_tui::SpellingFeatures::default());
	host
		.console(
			"cl_spelling_typo_detection 0; cl_spelling_autocomplete 0; cl_spelling_autocorrect 1",
		)
		.expect("spelling convars");
	host.key(Key::Char('a')).expect("type");
	assert_eq!(host.spelling_features(), omp_tui::SpellingFeatures {
		typo_detection: false,
		autocomplete:   false,
		autocorrect:    true,
	});
}

fn row(key: &'static str, efforts: &[&'static str]) -> omp_chat::ModelRow {
	omp_chat::ModelRow {
		key:         key.into(),
		name:        key.into(),
		provider_id: "test".into(),
		provider:    "Test".into(),
		context:     Some(200_000),
		input_mtok:  None,
		output_mtok: None,
		efforts:     efforts
			.iter()
			.map(|effort| omp_core::Str::new_static(effort))
			.collect(),
	}
}

#[test]
fn alt_p_opens_the_model_picker_and_enter_sets_ai_model_for_the_session() {
	let mut other = row("other/second", &["medium"]);
	other.name = "Second Model".into();
	other.provider_id = "other".into();
	other.provider = "Other Provider".into();
	other.context = Some(321_000);
	let (mut host, commands) = bound_host(vec![row("test/model", &["low", "high"]), other]);
	assert!(!host.overlay_open());
	assert_eq!(host.key(Key::Alt('p')).expect("alt+p"), NativeEffect::Consumed);
	assert!(host.overlay_open(), "alt+p opens the picker");
	let frame = host.picker_frame().expect("picker frame");
	assert!(omp_tui::frame_text(&frame).contains("Switch Model"));
	assert!(matches!(commands.recv().expect("overlay open"), HostCommand::Overlay {
		open: true,
		..
	}));
	host.key(Key::Down).expect("down");
	host.key(Key::Enter).expect("enter");
	assert!(!host.overlay_open(), "picking closes the picker");
	assert_eq!(host.notice(), Some("Session model: Second Model"));
	let badge = host.model_badge();
	assert_eq!(badge.identifier, "other/second");
	assert_eq!(badge.name, "Second Model");
	assert_eq!(badge.provider, "other");
	assert_eq!(badge.context_window, Some(321_000));
	assert!(badge.reasoning);
	assert!(matches!(commands.recv().expect("overlay close"), HostCommand::Overlay {
		open: false,
		..
	}));
	assert!(commands.try_recv().is_err(), "a session-only pick never reaches the controller");
}

#[test]
fn escape_dismisses_the_picker_before_anything_else() {
	let (mut host, _commands) = bound_host(vec![row("test/model", &[])]);
	host.key(Key::Alt('p')).expect("alt+p");
	assert!(host.overlay_open());
	host.key(Key::Esc).expect("esc");
	assert!(!host.overlay_open());
}

#[test]
fn shift_tab_cycles_ai_thinking_through_the_model_efforts_then_off() {
	let (mut host, _commands) = bound_host(vec![row("test/model", &["low", "high"])]);
	let mut seen = Vec::new();
	for _ in 0..3 {
		host.key(Key::BackTab).expect("shift+tab");
		seen.push(host.notice().expect("thinking notice").to_owned());
	}
	assert_eq!(
		seen,
		["Thinking: off", "Thinking: low", "Thinking: high"],
		"the declared default is high, then the cycle wraps through off"
	);
	let (mut host, _commands) = bound_host(vec![row("test/model", &[])]);
	host.key(Key::BackTab).expect("shift+tab");
	assert_eq!(host.notice(), Some("Current model does not support thinking"));
}

#[test]
fn ctrl_r_recalls_a_prior_prompt_into_the_composer() {
	let (mut host, _commands) = bound_host(vec![row("test/model", &[])]);
	host.key(Key::Ctrl('r')).expect("ctrl+r");
	assert!(host.overlay_open(), "history picker opens over the fixture's prompt");
	host.key(Key::Enter).expect("enter");
	assert!(!host.overlay_open());
	assert_eq!(host.key(Key::Char('!')).expect("type"), NativeEffect::Consumed);
}

#[test]
fn slash_and_unknown_commands_surface_as_notices_not_host_errors() {
	let (mut host, _commands) = bound_host(Vec::new());
	assert_eq!(host.console("no_such_command").expect("console"), NativeEffect::Consumed);
	assert!(
		host
			.notice()
			.is_some_and(|text| text.contains("no_such_command"))
	);
	assert_eq!(host.console("cl_model_select").expect("console"), NativeEffect::Consumed);
	assert_eq!(host.notice(), Some("No models are available to switch to"));
}

#[test]
fn thinking_toggle_changes_projection_without_touching_dom() {
	let (session, _) = fixture();
	let ctx = omp_con::Ctx::new();
	let before = session.dom().snapshot();
	let shown = block_views(session.dom(), omp_con::CL_SHOWTHINKING.get(&ctx));
	ctx.exec("toggle cl_showthinking", omp_con::Source::Console)
		.expect("toggle command");
	let hidden = block_views(session.dom(), omp_con::CL_SHOWTHINKING.get(&ctx));
	let after = session.dom().snapshot();

	assert!(shown.iter().any(|block| block.kind == BlockKind::Thinking));
	assert!(!hidden.iter().any(|block| block.kind == BlockKind::Thinking));
	assert_eq!(before, after);
}

/// A host over a fresh fixture session with a live kernel-event feed and
/// pi's default retry/interrupt binds.
fn kernel_host()
-> (NativeHost, flume::Receiver<HostCommand>, flume::Sender<omp_agent::KernelEvent>, Session) {
	let (mut session, _) = fixture();
	let (snapshot, dom_events) = session.subscribe();
	let (kernel_tx, kernel_events) = flume::unbounded();
	let (commands, command_rx) = flume::unbounded();
	let (up, _) = flume::unbounded();
	let con = Arc::new(
		omp_chat::HostMailbox::new()
			.attach(omp_con::Ctx::builder())
			.build(),
	);
	con.run(r#"bind f5 cl_retry; bind escape cl_interrupt"#)
		.expect("binds");
	let host = NativeHost::new(
		HostOptions {
			model: omp_chat::ModelBadge::from_identifier("test/model"),
			snapshot,
			dom_events,
			kernel_events,
			commands,
			up,
			con,
			models: Vec::new(),
			cycle: Vec::new(),
			resize_policy: ResizePolicy::Rebuild,
			project: std::path::PathBuf::new(),
			welcome: omp_chat::welcome::WelcomeFacts::default(),
			ui: UiContext::default(),
			services: Arc::new(omp_chat::overlays::NoServices),
			speech: None,
			resuming: false,
			initial_panel: None,
		},
		Size::new(100, 30),
	);
	(host, command_rx, kernel_tx, session)
}

fn last_turn(session: &Session) -> omp_dom::Handle {
	*session
		.dom()
		.children(session.dom().body())
		.last()
		.expect("turn")
}

/// Appends `<notice kind=K>` to the last turn exactly as the kernel does.
fn append_notice(session: &mut Session, kind: &'static str, text: &'static str) {
	use omp_dom::{NodeSpec, Op, Txn, Value};
	let turn = last_turn(session);
	let cause = session.head().expect("head");
	session
		.patch(Txn {
			cause,
			label: None,
			ops: vec![Op::Ins {
				parent: turn,
				after:  session.dom().children(turn).last().copied(),
				node:   NodeSpec::new(KnownTag::Notice)
					.with_prop(PropId::Kind, Value::Str(omp_core::Str::new_static(kind)))
					.with_content(omp_core::Str::new_static(text)),
			}],
		})
		.expect("notice");
}

/// A second turn that died on a provider error notice.
fn failed_turn(session: &mut Session, error: &'static str) {
	session.begin_turn().expect("begin turn");
	session.user("again", Vec::new()).expect("user");
	session
		.assistant_start("test/model", "test", "test/model")
		.expect("assistant start");
	session.assistant_end("error").expect("assistant end");
	append_notice(session, "error", error);
}

fn text_of(frame: &omp_tui::Frame) -> String {
	omp_tui::frame_text(frame)
}

#[test]
fn retry_loader_appears_on_inference_retry_and_clears_on_the_next_inference_start() {
	use omp_agent::KernelEvent;
	let (mut host, commands, kernel, mut session) = kernel_host();
	// The failed attempt: a call the kernel settled synthetically (no
	// output), then the provider error that ended it.
	session.begin_turn().expect("begin turn");
	session.user("again", Vec::new()).expect("user");
	session
		.assistant_start("test/model", "test", "test/model")
		.expect("assistant start");
	let args = serde_json::value::to_raw_value(&serde_json::json!({"path":"x.txt"})).expect("args");
	let call = session
		.call("read", 1, "call-2", None, Some(args), None)
		.expect("tool call");
	session.assistant_end("error").expect("assistant end");
	let fault =
		serde_json::value::to_raw_value(&serde_json::json!({"kind":"error"})).expect("fault");
	let parts = serde_json::value::to_raw_value(&serde_json::json!([])).expect("parts");
	session
		.fail_projected(call, fault, parts)
		.expect("synthetic failure");
	append_notice(&mut session, "error", "http 529 overloaded");
	host.poll().expect("apply dom events");
	assert!(host.retrying().is_none());
	assert!(host.status_frame().is_none(), "no status row before the retry");
	let call_handle = *session
		.dom()
		.children(last_turn(&session))
		.iter()
		.find(|handle| {
			session
				.dom()
				.get(**handle)
				.is_some_and(|node| node.tag == Tag::Custom("read".into()))
		})
		.expect("failed call element");
	let failed_card = |host: &NativeHost| {
		host
			.blocks()
			.iter()
			.any(|block| block.kind == BlockKind::Tool && block.key / 8 == call_handle.get())
	};
	assert!(failed_card(&host), "the failed attempt's synthetic card is projected");
	assert!(host.banner_frame().is_some(), "the error is pinned");

	kernel
		.send(KernelEvent::InferenceRetry {
			attempt:      1,
			max_attempts: 3,
			delay:        std::time::Duration::from_secs(2),
			reason:       "http 529 overloaded".into(),
		})
		.expect("retry event");
	assert_eq!(host.poll().expect("poll"), NativeEffect::Consumed);
	let state = host.retrying().expect("retry scheduled");
	assert_eq!((state.attempt, state.max_attempts), (1, 3));
	let row = text_of(&host.status_frame().expect("retry loader row"));
	assert!(row.contains("Retrying (1/3) in 2.0s…"), "{row}");
	assert!(row.contains("(esc to cancel)"), "{row}");
	// The native window paints the same status row above its editor band
	// (ADR 0005 peer views) and repaints it on the clock alone: the
	// loader's spinner phase turns over well inside a quarter second.
	assert!(text_of(host.frame()).contains("Retrying (1/3) in"), "{}", text_of(host.frame()));
	std::thread::sleep(std::time::Duration::from_millis(250));
	assert_eq!(host.poll().expect("poll"), NativeEffect::Consumed, "countdown wake repaints");
	// ERR-08: the superseded failure card and its pinned error leave the view.
	assert!(!failed_card(&host), "the retry retracts the previous attempt's synthetic card");
	assert!(host.banner_frame().is_none(), "the retry retracts the superseded error");
	// Esc while the retry counts down cancels the turn (rung 3).
	host.key(Key::Esc).expect("escape");
	assert!(matches!(commands.recv().expect("interrupt"), HostCommand::Interrupt));

	kernel
		.send(KernelEvent::InferenceStarted)
		.expect("inference started");
	assert_eq!(host.poll().expect("poll"), NativeEffect::Consumed);
	assert!(host.retrying().is_none(), "the next inference start clears the loader");
	assert!(host.status_frame().is_none());
}

#[test]
fn error_banner_pins_above_the_editor_and_clears_on_the_next_submit() {
	let (mut host, commands, _kernel, mut session) = kernel_host();
	assert!(host.banner_frame().is_none());
	failed_turn(&mut session, "Provider exploded\nsecond line");
	host.poll().expect("apply dom events");
	let banner = text_of(&host.banner_frame().expect("pinned banner"));
	assert!(banner.contains("Provider exploded"), "{banner}");
	assert!(banner.contains("Dismissed when you send your next message."), "{banner}");
	// The banner sits in the editor band of the composed frame.
	let frame = text_of(host.frame());
	assert!(frame.contains("Provider exploded"), "{frame}");
	// ERR-06: the identical inline error card is suppressed while pinned.
	assert!(
		!host
			.blocks()
			.iter()
			.any(|block| block.kind == BlockKind::Notice && block.text.contains("exploded")),
		"inline notice suppressed while the banner shows it"
	);

	for key in "go".chars() {
		host.key(Key::Char(key)).expect("type");
	}
	assert_eq!(host.key(Key::Enter).expect("enter"), NativeEffect::Consumed);
	assert!(matches!(commands.recv().expect("submit"), HostCommand::Submit(text) if text == "go"));
	assert!(host.banner_frame().is_none(), "sending the next message dismisses the banner");
	assert!(!text_of(host.frame()).contains("Provider exploded"));
}

#[test]
fn idle_retry_hint_shows_after_a_turn_died_on_a_tool_call() {
	let (mut host, commands, _kernel, mut session) = kernel_host();
	session.begin_turn().expect("begin turn");
	session.user("run it", Vec::new()).expect("user");
	session
		.assistant_start("test/model", "test", "test/model")
		.expect("assistant start");
	let args =
		serde_json::value::to_raw_value(&serde_json::json!({"command":"sleep 30"})).expect("args");
	let call = session
		.call("bash", 1, "slow-shell", None, Some(args), None)
		.expect("tool call");
	session.assistant_end("tool_calls").expect("assistant end");
	host.poll().expect("apply dom events");
	assert!(host.status_frame().is_none(), "no hint while the call runs");
	let fault = serde_json::value::to_raw_value(&serde_json::json!({
		"kind":"aborted","value":{"abort":{"kind":"interrupted","reason":"cancelled"},"kind":"cancelled"}
	}))
	.expect("fault");
	// The dispatcher journals a tool's `Ev::Aborted` as the `CallOutcome`
	// JSON in both the fault and its projected text, then the kernel ends
	// the turn with the interrupt notice.
	let parts = serde_json::value::to_raw_value(&serde_json::json!([
		{"kind":"text","text":fault.get()}
	]))
	.expect("parts");
	session
		.fail_projected(call, fault, parts)
		.expect("aborted result");
	append_notice(&mut session, "warn", "Turn interrupted");
	host.poll().expect("apply dom events");
	let row = text_of(&host.status_frame().expect("retry hint row"));
	assert!(row.contains("f5 to Retry"), "{row}");
	assert!(text_of(host.frame()).contains("f5 to Retry"), "the native frame shows the hint");
	// The advertised key runs the same predicate and hands the replay to
	// the controller (pi `viewSession.retry()`), never a prompt resubmit.
	host.key(Key::Function(5)).expect("retry key");
	assert!(
		matches!(commands.try_recv(), Ok(HostCommand::Retry)),
		"the retry hint's key must emit HostCommand::Retry"
	);
	assert!(commands.try_recv().is_err(), "no resubmitted prompt");
	assert_eq!(host.notice(), Some("Retrying the interrupted tool calls"));
}

/// Journals `<notice kind=K name=N>` under the last turn exactly as the
/// kernel does for `EnvEvent::Notice` (pi `custom_message` / `hookMessage`).
fn append_named_notice(session: &mut Session, kind: &'static str, name: &'static str, body: &str) {
	use omp_dom::{NodeSpec, Op, Txn, Value};
	let turn = last_turn(session);
	let cause = session.head().expect("head");
	session
		.patch(Txn {
			cause,
			label: None,
			ops: vec![Op::Ins {
				parent: turn,
				after:  session.dom().children(turn).last().copied(),
				node:   NodeSpec::new(KnownTag::Notice)
					.with_prop(PropId::Kind, Value::Str(omp_core::Str::new_static(kind)))
					.with_prop(PropId::Name, Value::Str(omp_core::Str::new_static(name)))
					.with_content(omp_core::Str::new(body)),
			}],
		})
		.expect("notice");
}

#[test]
fn hook_message_is_a_journaled_notice_that_replays_rewinds_and_copies() {
	let (mut host, _commands, _kernel, mut session) = kernel_host();
	let before = session.head().expect("head");
	append_named_notice(&mut session, "hook", "pre-commit", "Ran **3** checks\n\n- lint ok");
	assert_eq!(host.poll().expect("apply dom events"), NativeEffect::Consumed);
	let blocks = host.blocks();
	let hook = blocks.last().expect("hook block last");
	assert_eq!(hook.kind, BlockKind::Notice);
	assert_eq!(hook.text, "[pre-commit]\nRan **3** checks\n\n- lint ok");
	let frame = text_of(host.frame());
	assert!(frame.contains("pre-commit") && frame.contains("lint ok"), "{frame}");
	// The block is a pure function of the tree: a byte-for-byte journal copy
	// replays into the same block while the live writer retains its required
	// exclusive lock.
	let replay_dir = tempfile::tempdir().expect("replay directory");
	let replay_path = replay_dir.path().join("replay.oms");
	std::fs::copy(session.journal_path(), &replay_path).expect("copy journal");
	let replayed =
		Session::open(replay_path, ComponentRegistry::standard()).expect("reopen copied journal");
	let replayed_blocks = block_views(replayed.dom(), true);
	assert_eq!(replayed_blocks.last().map(|block| block.text.as_str()), Some(hook.text.as_str()));
	// The copy selector offers it as pi's `message` outline target.
	let targets = omp_chat::overlays::copy::collect_targets(session.dom(), true);
	let message = targets.last().expect("copy target");
	assert_eq!(message.label, "message");
	assert_eq!(message.content, "Ran **3** checks\n\n- lint ok");
	// Rewinding past the notice removes it from every observer.
	session.rewind(before).expect("rewind");
	host.poll().expect("apply reset");
	assert!(
		!host
			.blocks()
			.iter()
			.any(|block| block.text.contains("lint ok"))
	);
	assert!(!text_of(host.frame()).contains("lint ok"));
}

#[test]
fn extension_message_folds_only_when_it_is_a_hook() {
	let (mut host, _commands, _kernel, mut session) = kernel_host();
	let body = "l1\nl2\nl3\nl4\nl5\nl6\nl7";
	append_named_notice(&mut session, "custom", "irc:incoming", body);
	append_named_notice(&mut session, "hook", "audit", body);
	host.poll().expect("apply dom events");
	let frame = text_of(host.frame());
	// One `l7` from the extension message (never folded); the hook shows
	// its first five lines then pi's `…` fold.
	assert_eq!(frame.matches("l7").count(), 1, "{frame}");
	assert!(frame.contains('…'), "{frame}");
	assert!(frame.contains("irc:incoming") && frame.contains("audit"), "{frame}");
}

#[test]
fn ask_call_waiting_on_the_user_earns_one_toast() {
	let (mut host, _commands, _kernel, mut session) = kernel_host();
	session.begin_turn().expect("begin turn");
	session.user("pick", Vec::new()).expect("user");
	session
		.assistant_start("test/model", "test", "test/model")
		.expect("assistant start");
	let args = serde_json::value::to_raw_value(&serde_json::json!({
		"questions":[{"id":"region","question":"Which region?","options":[{"label":"us"},{"label":"eu"}]}]
	}))
	.expect("args");
	session
		.call("ask", 1, "ask-1", None, Some(args), None)
		.expect("ask call");
	session.assistant_end("tool_calls").expect("assistant end");
	host.poll().expect("apply dom events");
	let toasts = host.take_notifications();
	assert_eq!(toasts.len(), 1, "{toasts:?}");
	assert_eq!(toasts[0].body.as_deref(), Some("Which region?"));
	host.poll().expect("poll again");
	assert!(host.take_notifications().is_empty(), "one toast per waiting call");
}

/// Journals a running `ask` call with two questions and returns the host
/// showing its dialog.
fn ask_host() -> (NativeHost, flume::Receiver<HostCommand>, Session) {
	let (mut host, commands, _kernel, mut session) = kernel_host();
	session.begin_turn().expect("begin turn");
	session.user("pick", Vec::new()).expect("user");
	session
		.assistant_start("test/model", "test", "test/model")
		.expect("assistant start");
	let args = serde_json::value::to_raw_value(&serde_json::json!({
		"questions":[
			{"id":"region","question":"Which region?","options":[{"label":"us"},{"label":"eu","description":"Frankfurt"}],"recommended":1},
			{"id":"tier","header":"Tier","question":"Which tiers?","multi":true,"options":[{"label":"free"},{"label":"pro"}]}
		]
	}))
	.expect("args");
	session
		.call("ask", 1, "ask-7", None, Some(args), None)
		.expect("ask call");
	session.assistant_end("tool_calls").expect("assistant end");
	host.poll().expect("apply dom events");
	host.take_notifications();
	assert_eq!(host.overlay_id(), Some("ask"), "the running ask element opens the dialog");
	(host, commands, session)
}

#[test]
fn ask_dialog_answers_round_trip_to_the_call_with_the_recommended_default_first() {
	let (mut host, commands, _session) = ask_host();
	let frame = text_of(host.picker_frame().as_ref().expect("dialog frame"));
	assert!(frame.contains("Which region?"), "{frame}");
	assert!(frame.contains("eu (Recommended)"), "{frame}");
	assert!(frame.contains("Submit"), "tab bar has the Submit tab: {frame}");
	assert!(frame.contains("Enter select · n note"), "{frame}");
	// Enter takes the highlighted (recommended) option and advances.
	host.key(Key::Enter).expect("select region");
	let frame = text_of(host.picker_frame().as_ref().expect("dialog frame"));
	assert!(frame.contains("Which tiers?"), "{frame}");
	assert!(frame.contains("Space toggle · Enter next"), "{frame}");
	host.key(Key::Space).expect("toggle free");
	host.key(Key::Down).expect("move to pro");
	host.key(Key::Space).expect("toggle pro");
	host.key(Key::Enter).expect("advance to review");
	let frame = text_of(host.picker_frame().as_ref().expect("dialog frame"));
	assert!(frame.contains("Review answers"), "{frame}");
	assert!(frame.contains("free, pro"), "{frame}");
	assert!(frame.contains("1. Q1:") && frame.contains("2. Tier:"), "{frame}");
	host.key(Key::Enter).expect("submit");
	assert_eq!(host.overlay_id(), None, "the dialog closes on submit");
	let answer = commands
		.try_iter()
		.find_map(|command| match command {
			HostCommand::AskAnswer { id, answers } => Some((id, answers)),
			_ => None,
		})
		.expect("answer command");
	assert_eq!(answer.0, "ask-7");
	let answers = answer.1.expect("answers, not a cancel");
	assert_eq!(answers.len(), 2);
	assert_eq!(answers[0].id, "region");
	assert_eq!(answers[0].selected, [omp_core::Str::new_static("eu")]);
	assert_eq!(answers[1].selected, [
		omp_core::Str::new_static("free"),
		omp_core::Str::new_static("pro")
	]);
	assert!(!answers[0].timed_out);
	// The element is still `running` until the tool folds the reply; the
	// dialog must not reopen for it.
	host.poll().expect("poll");
	assert_eq!(host.overlay_id(), None);
}

#[test]
fn ask_dialog_escape_cancels_the_call() {
	let (mut host, commands, _session) = ask_host();
	host.key(Key::Esc).expect("esc");
	assert_eq!(host.overlay_id(), None);
	let cancelled = commands.try_iter().any(
		|command| matches!(command, HostCommand::AskAnswer { ref id, answers: None } if id == "ask-7"),
	);
	assert!(cancelled, "Esc sends the cancel reply");
}

/// A real terminal delivers Escape as a physical chord bound to
/// `cl_interrupt`; the ask dialog still answers `None` so the blocked tool
/// call settles instead of hanging behind a silently dismissed overlay.
#[test]
fn ask_dialog_escape_chord_through_the_interrupt_bind_still_cancels_the_call() {
	let (mut host, commands, _session) = ask_host();
	host
		.chord(omp_tui::KeyEvent {
			chord:   omp_tui::Chord::plain(Key::Esc),
			key:     Some(Key::Esc),
			pressed: true,
		})
		.expect("esc chord");
	assert_eq!(host.overlay_id(), None);
	let cancelled = commands.try_iter().any(
		|command| matches!(command, HostCommand::AskAnswer { ref id, answers: None } if id == "ask-7"),
	);
	assert!(cancelled, "the bound Esc edge reaches the dialog's cancel");
}

#[test]
fn bash_and_eval_prefixes_run_locally_instead_of_prompting_the_model() {
	let (mut host, commands) = bound_host(vec![row("test/model", &[])]);
	for character in "!echo hi".chars() {
		host.key(Key::Char(character)).expect("type");
	}
	host.key(Key::Enter).expect("submit");
	match commands.recv().expect("command") {
		HostCommand::RunLocal { input, draft } => {
			assert_eq!(input.mode, omp_chat::composer::PrefixMode::Bash);
			assert_eq!(input.code, "echo hi");
			assert!(!input.exclude);
			assert_eq!(draft, "!echo hi", "the verbatim line travels with the run");
		},
		other => panic!("expected a local run, got {other:?}"),
	}
	for character in "$$ 1+1".chars() {
		host.key(Key::Char(character)).expect("type");
	}
	host.key(Key::Enter).expect("submit");
	match commands.recv().expect("command") {
		HostCommand::RunLocal { input, .. } => {
			assert_eq!(input.mode, omp_chat::composer::PrefixMode::Eval);
			assert_eq!(input.code, "1+1");
			assert!(input.exclude);
		},
		other => panic!("expected a local run, got {other:?}"),
	}
	assert!(commands.try_recv().is_err(), "nothing was submitted to the model");
}

fn type_line(host: &mut NativeHost, line: &str) {
	for character in line.chars() {
		host.key(Key::Char(character)).expect("type");
	}
}

/// pi `#submitToFocusedSession`: `!` / `$` lines never run while a subagent
/// is focused; the draft stays put and the status names the way back.
#[test]
fn local_prefixes_are_refused_while_a_subagent_is_focused_and_keep_the_draft() {
	let (mut host, commands) = bound_host(vec![row("test/model", &[])]);
	host
		.act(omp_chat::HostAction::FocusAgent(Some("worker-1".into())))
		.expect("focus");
	commands.try_iter().for_each(drop);
	for line in ["!echo hi", "$ 1+1"] {
		type_line(&mut host, line);
		host.key(Key::Enter).expect("submit");
		assert_eq!(host.composer_text(), line, "the draft is preserved");
		assert!(!host.turn_active(), "no optimistic activity edge");
		assert_eq!(
			host.notice(),
			Some("Commands run in the main session — press ←← to return first")
		);
		assert!(commands.try_recv().is_err(), "nothing runs in the main session");
		host.console("cl_clear").expect("clear draft");
		commands.try_iter().for_each(drop);
	}
}

/// pi `piSegment`: while a subagent is focused the band's brand slot holds
/// the ghost and the agent id, so the target of every submit stays visible
/// after the transient notice has gone; leaving restores the brand glyph.
#[test]
fn focused_subagent_is_named_in_the_status_band_brand_slot() {
	let (mut host, commands) = bound_host(vec![row("test/model", &[])]);
	let band = |host: &NativeHost| {
		text_of(host.frame())
			.lines()
			.find(|line| line.contains("📁 ") && line.contains(" ▶"))
			.map(str::to_owned)
			.expect("status band row")
	};
	assert!(band(&host).starts_with(" π  >"), "{}", band(&host));
	host
		.act(omp_chat::HostAction::FocusAgent(Some("worker-1".into())))
		.expect("focus");
	commands.try_iter().for_each(drop);
	assert!(band(&host).starts_with(" 👻 worker-1  >"), "{}", band(&host));
	// The notice clears on the next keystroke; the brand slot does not.
	host.key(Key::Char('x')).expect("type");
	assert!(band(&host).starts_with(" 👻 worker-1  >"), "{}", band(&host));
	host
		.act(omp_chat::HostAction::FocusAgent(None))
		.expect("unfocus");
	assert!(band(&host).starts_with(" π  >"), "{}", band(&host));
}

/// pi `handleSubmit` collab guest branch: local execution is host-only; the
/// line is consumed with a status and nothing is sent.
#[test]
fn local_prefixes_are_refused_for_a_collab_guest() {
	let (mut host, commands) = bound_host(vec![row("test/model", &[])]);
	host
		.act(omp_chat::HostAction::CollabGuest(true))
		.expect("guest");
	type_line(&mut host, "!rm -rf build");
	host.key(Key::Enter).expect("submit");
	assert!(commands.try_recv().is_err(), "the guest never runs a local tool");
	assert!(!host.turn_active());
	assert_eq!(host.notice(), Some("Local execution is host-only during a collab session"));
	assert_eq!(host.composer_text(), "", "pi clears the consumed line");
}

/// pi: "A bash command is already running. Press Esc to cancel it first."
/// followed by `editor.setText(text)` — the second command is handed back.
#[test]
fn a_second_local_command_while_one_runs_is_handed_back_to_the_composer() {
	let (mut host, commands, mut session) = bound_host_with_session(vec![row("test/model", &[])]);
	session.begin_turn().expect("begin turn");
	let args =
		serde_json::value::to_raw_value(&serde_json::json!({"command":"sleep 30"})).expect("args");
	session
		.call("bash", 1, "local-1", None, Some(args), None)
		.expect("running local call");
	host.poll().expect("sync replica");
	type_line(&mut host, "!echo second");
	host.key(Key::Enter).expect("submit");
	assert!(commands.try_recv().is_err(), "no second run while one is active");
	assert_eq!(host.composer_text(), "!echo second", "the draft comes back");
	assert_eq!(
		host.notice(),
		Some("A bash command is already running. Press Esc to cancel it first.")
	);
	// Ctrl+Q / Alt+Enter goes through the same door and must not wipe the
	// restored draft afterwards.
	host.console("cl_followup").expect("follow up");
	assert!(commands.try_recv().is_err());
	assert_eq!(host.composer_text(), "!echo second");
}

/// The controller refused the run (paused): the draft returns and the
/// optimistic activity edge rolls back so the composer reads idle again.
#[test]
fn a_refused_local_run_restores_the_draft_and_rolls_back_activity() {
	let (mut host, commands) = bound_host(vec![row("test/model", &[])]);
	type_line(&mut host, "!echo hi");
	host.key(Key::Enter).expect("submit");
	let draft = match commands.recv().expect("command") {
		HostCommand::RunLocal { draft, .. } => draft,
		other => panic!("expected a local run, got {other:?}"),
	};
	assert!(host.turn_active(), "submit is optimistic");
	assert_eq!(host.composer_text(), "");
	host
		.act(omp_chat::HostAction::LocalRefused {
			draft,
			reason: "Paused: resume before running local commands".into(),
		})
		.expect("refusal");
	assert!(!host.turn_active(), "the activity edge rolled back");
	assert_eq!(host.composer_text(), "!echo hi");
	assert_eq!(host.notice(), Some("Paused: resume before running local commands"));
}

/// Dropping or pasting an image stages a `#1` chip; Enter submits the draft
/// with pi's positional `[Image #1, WxH]` marker and the image bytes typed
/// from their header, the controller content-addresses them beside the
/// journaled prompt, and the transcript bubble shows the same compact chip
/// the composer used.
#[test]
fn pasted_image_chip_submits_attachments_and_the_bubble_shows_the_chip() {
	let (mut host, commands, mut session) = bound_host_with_session(vec![row("test/model", &[])]);
	let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x04\0\0\0\x03";
	let dir = tempdir().expect("image directory");
	let path = dir.path().join("shot.png");
	std::fs::write(&path, png).expect("write png");
	let image_icon = omp_tui::Charset::default().icon(omp_tui::Icon::Image);

	assert_eq!(host.paste(path.to_str().expect("utf-8 path")), NativeEffect::Consumed);
	let composer = text_of(host.frame());
	assert!(composer.contains(&format!("{image_icon} #1")), "chip staged:\n{composer}");
	assert!(!composer.contains("[Image #1"), "the draft shows the chip, not the wire marker");
	type_text(&mut host, "what is this?");
	assert_eq!(host.key(Key::Enter).expect("enter"), NativeEffect::Consumed);

	let (text, attachments) = match commands.recv().expect("submit") {
		HostCommand::SubmitWithAttachments { text, attachments } => (text, attachments),
		other => panic!("image chips submit attachments, got {other:?}"),
	};
	assert_eq!(text, "[Image #1, 4x3] what is this?");
	assert_eq!(attachments.len(), 1);
	assert_eq!(attachments[0].mime, "image/png");
	assert_eq!(attachments[0].bytes.as_ref(), png);
	assert!(host.take_clipboard().is_none());

	// The controller's side of the seam: content-address and journal.
	let stored = session
		.store_attachments(attachments)
		.expect("attachments store");
	assert_eq!(session.blobs().get(&stored[0].blob).expect("blob").as_ref(), png);
	session.begin_turn().expect("turn");
	session.user(text, stored).expect("user");
	assert_eq!(host.poll().expect("apply dom events"), NativeEffect::Consumed);
	let user = host
		.blocks()
		.into_iter()
		.filter(|block| block.kind == BlockKind::User)
		.last()
		.expect("user block");
	assert_eq!(user.text, "[Image #1, 4x3] what is this?", "the block carries the wire text");
	// The painted bubble collapses the marker back into the composer's chip
	// (pi `collapseImageMarkers` in `user-message.ts`).
	let frame = text_of(host.frame());
	assert!(frame.contains(&format!("{image_icon} #1")), "bubble shows the chip:\n{frame}");
	assert!(!frame.contains("[Image #1"), "the marker collapses:\n{frame}");
	assert!(frame.contains("what is this?"), "{frame}");
	let paperclip = omp_tui::Charset::default().icon(omp_tui::Icon::Paperclip);
	assert!(!frame.contains(&format!("{paperclip} #1")), "a referenced image is not repeated:\n{frame}");
}
