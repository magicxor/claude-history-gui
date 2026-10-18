//! G tests of Debug search and Semantic debug search (T-011, T-012, T-013).

use claude_history_core::maintenance::{debug_search_report, load_sorted_conversations};
use claude_history_core::semantic::embed::SemanticEmbedder;
use claude_history_core::semantic_ops::{semantic_debug_report, semantic_search_report};
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::main_window;
use crate::globals::app_state;
use crate::model::SemanticReportMode;
use crate::test_support::fixtures_list::{
    FakeEmbedderControl, ListFixture, SessionSpec, fake_factory, install_fake_embedder,
};
use crate::test_support::fixtures_tools::{settle, wait_for};
use crate::test_support::{TestWorld, recording, synth};
use crate::tools::common::Scope;
use crate::tools::debug_search::{
    CorpusState, DebugSearchDialog, current_dialog, open_debug_search,
};
use crate::tools::semantic_report::{
    RunState, SemanticReportDialog, current_dialog as current_semantic, open_semantic_debug,
};

fn open_search(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    query: Option<&str>,
    local: bool,
) -> Entity<DebugSearchDialog> {
    let query = query.map(str::to_string);
    cx.update_window(handle, |_, window, cx| {
        open_debug_search(query, local, window, cx)
    })
    .unwrap();
    wait_for(cx, "the corpus", |cx| {
        current_dialog(cx)
            .is_some_and(|d| matches!(d.read(cx).corpus_state(), CorpusState::Ready(_)))
    });
    settle(cx, handle);
    cx.read(current_dialog).expect("the dialog is open")
}

/// The synthetic corpus has equal-timestamp conversations whose relative order
/// differs between two loads; compare the report lines as a sorted set with
/// the rank numbers dropped.
fn tie_insensitive(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| match line.find("chunk-heavy #") {
            Some(at) => line[..at].to_string() + line[at..].split_once(": ").map_or("", |t| t.1),
            None => strip_rank(line),
        })
        .collect();
    lines.sort();
    lines
}

/// `# 3 hybrid=… | project-04 …` -> `hybrid=… | project-04 …`.
fn strip_rank(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('#') {
        let rest = rest.trim_start();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 {
            return rest[digits..].trim_start().to_string();
        }
    }
    line.to_string()
}

fn fresh_corpus(cx: &TestAppContext) -> Vec<claude_history_core::history::Conversation> {
    cx.read(|cx| {
        let state = app_state(cx);
        load_sorted_conversations(
            &state.env,
            state.settings.show_last,
            None,
            &state.launch.time_filter,
        )
        .expect("fresh load")
    })
}

/// T-011: the report equals core's on a fresh load with the launch filter.
#[gpui_kit::test]
fn debug_search_reports_on_a_fresh_load(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 12);
    let (_f, handle) = main_window(cx, &world, &["--since", "400d"]);
    let dialog = open_search(cx, handle, Some("synthetic"), false);
    wait_for(cx, "the report", |cx| dialog.read(cx).report().is_some());
    let expected =
        debug_search_report(&fresh_corpus(cx), "synthetic", chrono::Local::now(), None).to_text();
    let text = cx.read(|cx| dialog.read(cx).report_text()).unwrap();
    // Tied rows may come back in another order from the second load.
    assert_eq!(tie_insensitive(&text), tie_insensitive(&expected));
    assert!(cx.read(|cx| dialog.read(cx).report().unwrap().rows.len()) > 0);
    // "Copy as text" copies exactly that.
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.copy(window, cx))
    })
    .unwrap();
    assert_eq!(recording(cx).clipboard(), Some(text));
}

/// T-011: sessions outside the launch time filter are not in the dialog's
/// corpus even though they exist on disk.
#[gpui_kit::test]
fn debug_search_corpus_honours_the_launch_filter(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 5);
    // Conversations are dated by their files: make them a week old.
    for entry in std::fs::read_dir(world.projects_root()).unwrap().flatten() {
        for file in std::fs::read_dir(entry.path()).unwrap().flatten() {
            crate::test_support::fixtures_tools::age_file(&file.path(), 7);
        }
    }
    let (_f, handle) = main_window(cx, &world, &["--since", "1d"]);
    let dialog = open_search(cx, handle, Some("synthetic"), false);
    wait_for(cx, "the report", |cx| dialog.read(cx).report().is_some());
    assert_eq!(
        cx.read(|cx| dialog.read(cx).corpus_state().clone()),
        CorpusState::Ready(0)
    );
    assert_eq!(
        cx.read(|cx| dialog.read(cx).report().unwrap().rows.len()),
        0
    );
    let line = cx.read(|cx| crate::tools::common::corpus_line(0, cx));
    assert!(
        line.contains("since 1d (launch filter)") && line.ends_with("0 conversations"),
        "{line}"
    );
}

/// C-014 / T-011: `local` selects the Workspace scope.
#[gpui_kit::test]
fn debug_search_workspace_scope(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let proj = world.home().join("work").join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    for n in 0..3 {
        crate::test_support::fixtures_list::SessionSpec::new("proj", 200 + n)
            .cwd(proj.clone())
            .summary(&format!("proj token {n}"))
            .start(chrono::Utc::now())
            .write(&world);
    }
    synth::corpus(&world, 4);
    let ws = proj.display().to_string();
    let (_f, handle) = main_window(
        cx,
        &world,
        &["--workspace", &ws, "--local", "--since", "400d"],
    );
    let dialog = open_search(cx, handle, Some("token"), true);
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
    wait_for(cx, "the report", |cx| dialog.read(cx).report().is_some());
    let workspace = cx
        .read(|cx| app_state(cx).workspace.clone())
        .expect("workspace");
    let expected = debug_search_report(
        &fresh_corpus(cx),
        "token",
        chrono::Local::now(),
        Some(&workspace),
    )
    .to_text();
    assert_eq!(
        tie_insensitive(&cx.read(|cx| dialog.read(cx).report_text()).unwrap()),
        tie_insensitive(&expected)
    );
}

/// A workspace `proj` with three `token` sessions and an `api` project with
/// two more; returns the `proj` directory.
fn token_world(world: &TestWorld) -> std::path::PathBuf {
    let proj = world.home().join("work").join("proj");
    let api = world.home().join("work").join("api");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&api).unwrap();
    for n in 0..3 {
        SessionSpec::new("proj", 300 + n)
            .cwd(proj.clone())
            .summary(&format!("proj token {n}"))
            .start(chrono::Utc::now())
            .write(world);
    }
    for n in 0..2 {
        SessionSpec::new("api", 310 + n)
            .cwd(api.clone())
            .summary(&format!("api token {n}"))
            .start(chrono::Utc::now())
            .write(world);
    }
    proj
}

/// T-011: changing the scope after a run runs the query again, so the table,
/// the summary and "Copy as text" never show the previous scope's report.
#[gpui_kit::test]
fn changing_the_scope_reruns_the_query(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let proj = token_world(&world);
    let ws = proj.display().to_string();
    let (_f, handle) = main_window(cx, &world, &["--workspace", &ws, "--since", "400d"]);
    let dialog = open_search(cx, handle, Some("token"), false);
    wait_for(cx, "the report", |cx| dialog.read(cx).report().is_some());
    let rows = |cx: &TestAppContext| cx.read(|cx| dialog.read(cx).report().unwrap().rows.len());
    assert_eq!(rows(cx), 5, "All: both projects");
    let all_text = cx.read(|cx| dialog.read(cx).report_text()).unwrap();
    // Workspace: the report is replaced by the workspace's.
    dialog.update(cx, |d, cx| d.set_scope(Scope::Workspace, cx));
    wait_for(cx, "the workspace report", |cx| {
        let d = dialog.read(cx);
        !d.is_running() && d.report().is_some_and(|r| r.rows.len() == 3)
    });
    let workspace = cx
        .read(|cx| app_state(cx).workspace.clone())
        .expect("workspace");
    let expected = debug_search_report(
        &fresh_corpus(cx),
        "token",
        chrono::Local::now(),
        Some(&workspace),
    )
    .to_text();
    let text = cx.read(|cx| dialog.read(cx).report_text()).unwrap();
    assert_eq!(tie_insensitive(&text), tie_insensitive(&expected));
    assert_ne!(tie_insensitive(&text), tie_insensitive(&all_text));
    // Back to All.
    dialog.update(cx, |d, cx| d.set_scope(Scope::All, cx));
    wait_for(cx, "the all report", |cx| {
        let d = dialog.read(cx);
        !d.is_running() && d.report().is_some_and(|r| r.rows.len() == 5)
    });
    // Choosing the scope already chosen changes nothing.
    dialog.update(cx, |d, cx| d.set_scope(Scope::All, cx));
    assert!(!cx.read(|cx| dialog.read(cx).is_running()));
}

/// T-011: the results table has the headers of the §5.12 mockup and every row
/// shows the rank, score, freshness, project, short session id and age.
#[gpui_kit::test]
fn the_results_table_has_headers_and_cells(cx: &mut TestAppContext) {
    use crate::tools::debug_search::{COLUMNS, row_cells};
    let world = TestWorld::new();
    synth::corpus(&world, 6);
    let (_f, handle) = main_window(cx, &world, &["--since", "400d"]);
    let dialog = open_search(cx, handle, Some("synthetic"), false);
    wait_for(cx, "the report", |cx| dialog.read(cx).report().is_some());
    settle(cx, handle);
    assert_eq!(
        COLUMNS,
        ["#", "Score", "Fresh", "Project", "Session", "Age"]
    );
    let rows = cx.read(|cx| dialog.read(cx).report().unwrap().rows.clone());
    assert!(!rows.is_empty());
    for row in &rows {
        let cells = row_cells(row);
        assert_eq!(cells[0], format!("{:02}", row.rank));
        assert_eq!(cells[1], format!("{:.2}", row.score));
        assert_eq!(cells[2], format!("{:.2}", row.freshness));
        assert_eq!(cells[3], row.project);
        assert_eq!(cells[4], crate::model::short_session_id(&row.session));
        assert_eq!(cells[5], row.age_label);
    }
}

/// T-011: a conversation outside the launch filter that is in the list (a
/// UUID jump) is not in the dialog's corpus: the report is exactly core's on a
/// fresh filtered load, in order and with ranks, and never the list's corpus.
#[gpui_kit::test]
fn a_uuid_jump_row_stays_out_of_the_debug_search(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 3);
    let recent = SessionSpec::new("proj", 71)
        .summary("zebra recent")
        .start(chrono::Utc::now())
        .write(&world);
    let old = SessionSpec::new("proj", 70)
        .summary("zebra old")
        .start(chrono::Utc::now() - chrono::Duration::days(400))
        .write(&world);
    crate::test_support::fixtures_tools::age_file(&old, 400);
    let (f, handle) = main_window(cx, &world, &["--since", "1d"]);
    // The jump puts the old conversation into the list.
    f.type_text(cx, &synth::uuid(70));
    f.wait_until(cx, "the jump row", |list, _| {
        list.filtered_paths() == vec![old.clone()]
    });
    let dialog = open_search(cx, handle, Some("zebra"), false);
    wait_for(cx, "the report", |cx| dialog.read(cx).report().is_some());
    // The corpus is the filtered one: the old file is not in it.
    let filtered = fresh_corpus(cx);
    assert!(filtered.iter().all(|c| c.path != old), "guard");
    assert!(filtered.iter().any(|c| c.path == recent), "guard");
    assert_eq!(
        cx.read(|cx| dialog.read(cx).corpus_state().clone()),
        CorpusState::Ready(filtered.len())
    );
    let everything = cx.read(|cx| {
        let state = app_state(cx);
        load_sorted_conversations(
            &state.env,
            state.settings.show_last,
            None,
            &claude_history_core::time_filter::TimeFilter::default(),
        )
        .expect("unfiltered load")
    });
    assert_eq!(
        everything.len(),
        filtered.len() + 1,
        "guard: one is excluded"
    );
    let rows = cx.read(|cx| dialog.read(cx).report().unwrap().rows.clone());
    assert_eq!(rows.len(), 1, "only the recent conversation matches");
    assert_eq!(rows[0].path, recent);
    // The text is core's, row order and ranks included (one row: no ties).
    let expected = debug_search_report(&filtered, "zebra", chrono::Local::now(), None).to_text();
    let text = cx.read(|cx| dialog.read(cx).report_text()).unwrap();
    assert_eq!(text, expected);
    // Had the list's corpus (with the jump row) been used, there would be 2 rows.
    let with_jump = debug_search_report(&everything, "zebra", chrono::Local::now(), None)
        .rows
        .len();
    assert_eq!(with_jump, 2, "guard: the old row matches too");
}

fn open_report(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    query: &str,
    mode: SemanticReportMode,
) -> Entity<SemanticReportDialog> {
    let query = query.to_string();
    cx.update_window(handle, |_, window, cx| {
        open_semantic_debug(Some(query), mode, false, window, cx)
    })
    .unwrap();
    wait_for(cx, "the semantic report", |cx| {
        current_semantic(cx)
            .is_some_and(|d| matches!(d.read(cx).state(), RunState::Done | RunState::Failed(_)))
    });
    cx.read(current_semantic).expect("the dialog is open")
}

fn core_factory(
    control: std::sync::Arc<FakeEmbedderControl>,
) -> impl FnMut() -> claude_history_core::Result<Box<dyn SemanticEmbedder>> {
    let factory = fake_factory(control);
    move || factory().map(|model| model as Box<dyn SemanticEmbedder>)
}

/// T-012: the search report (fake embedder).
#[gpui_kit::test]
fn the_semantic_search_report_is_core_text(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 8);
    ListFixture::init_app(cx, &world, &["--since", "400d"]);
    let _control = install_fake_embedder(cx);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open_report(
        cx,
        handle,
        "synthetic",
        SemanticReportMode::Search { top: 20 },
    );
    assert_eq!(
        cx.read(|cx| dialog.read(cx).state().clone()),
        RunState::Done
    );
    let text = cx.read(|cx| dialog.read(cx).output_text(cx));
    assert!(text.contains("Semantic search: searching"), "{text}");
    let cx_env = cx.read(|cx| app_state(cx).env.clone());
    let control = std::sync::Arc::new(FakeEmbedderControl::default());
    let report = semantic_search_report(
        &cx_env,
        "synthetic",
        &fresh_corpus(cx),
        20,
        None,
        &mut core_factory(control),
    )
    .expect("report");
    assert_eq!(
        tie_insensitive(&text),
        tie_insensitive(report.text.trim_end())
    );
}

/// T-012: changing the scope after a run runs the report again.
#[gpui_kit::test]
fn changing_the_scope_reruns_the_semantic_report(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let proj = token_world(&world);
    let ws = proj.display().to_string();
    ListFixture::init_app(cx, &world, &["--since", "400d", "--workspace", &ws]);
    let _control = install_fake_embedder(cx);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open_report(cx, handle, "token", SemanticReportMode::Search { top: 20 });
    let all = cx.read(|cx| dialog.read(cx).output_text(cx));
    assert!(all.contains("from 5 recent conversation(s)"), "{all}");
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.set_scope(Scope::Workspace, window, cx))
    })
    .unwrap();
    wait_for(cx, "the workspace report", |cx| {
        dialog.read(cx).state() == &RunState::Done
            && dialog
                .read(cx)
                .output_text(cx)
                .contains("from 3 recent conversation(s)")
    });
    let text = cx.read(|cx| dialog.read(cx).output_text(cx));
    assert_ne!(text, all);
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
}

/// T-012: a query matching nothing ends with `No semantic matches found.`.
#[gpui_kit::test]
fn a_query_without_matches_says_so(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 8);
    ListFixture::init_app(cx, &world, &["--since", "400d"]);
    let _control = install_fake_embedder(cx);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open_report(
        cx,
        handle,
        "\"no-such-literal-anywhere\"",
        SemanticReportMode::Search { top: 20 },
    );
    let text = cx.read(|cx| dialog.read(cx).output_text(cx));
    assert!(text.ends_with("No semantic matches found."), "{text}");
}

/// T-013: the debug dump equals `semantic_debug_report(..).text`.
#[gpui_kit::test]
fn the_debug_dump_is_core_text(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 8);
    ListFixture::init_app(cx, &world, &["--since", "400d"]);
    let _control = install_fake_embedder(cx);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open_report(cx, handle, "synthetic", SemanticReportMode::DebugDump);
    assert!(cx.read(|cx| dialog.read(cx).is_debug_dump()));
    let text = cx.read(|cx| dialog.read(cx).output_text(cx));
    let env = cx.read(|cx| app_state(cx).env.clone());
    let control = std::sync::Arc::new(FakeEmbedderControl::default());
    let report = semantic_debug_report(
        &env,
        "synthetic",
        &fresh_corpus(cx),
        None,
        &mut core_factory(control),
    )
    .expect("report");
    assert_eq!(
        tie_insensitive(&text),
        tie_insensitive(report.text.trim_end())
    );
}

/// A run is refused while Generate/Clear holds the semantic gate.
#[gpui_kit::test]
fn a_report_is_refused_while_the_gate_is_held(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 4);
    ListFixture::init_app(cx, &world, &["--since", "400d"]);
    let _control = install_fake_embedder(cx);
    let (_f, handle) = main_window(cx, &world, &[]);
    assert!(
        cx.update(|cx| crate::globals::SemanticGate::enter_exclusive(
            crate::model::SemanticOp::Clear,
            cx
        ))
    );
    let dialog = open_report(
        cx,
        handle,
        "synthetic",
        SemanticReportMode::Search { top: 20 },
    );
    let state = cx.read(|cx| dialog.read(cx).state().clone());
    assert!(
        matches!(state, RunState::Failed(ref error) if error.contains("busy")),
        "{state:?}"
    );
    cx.update(crate::globals::SemanticGate::leave);
}
