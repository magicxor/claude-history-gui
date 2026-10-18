//! G tests of loading, empty/error states, refresh, reloads and the UUID
//! jump (L-001…L-010, L-038, L-048, L-050, L-072…L-076, S-015, S-016, S-018,
//! L-079, C-003).

use chrono::{Duration, Utc};
use claude_history_core::browser::LoadEvent;
use claude_history_core::debug::DebugLevel;
use claude_history_core::error::AppError;
use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;

use crate::exit;
use crate::list::ListPaneEvent;
use crate::list::states::{StateAction, StateKind};
use crate::notify::ToastKind;
use crate::test_support::fixtures_list::{ListFixture, SessionSpec, listed_corpus};
use crate::test_support::{TestWorld, global_lock, recording, synth};

/// L-001, L-002: rows are listed while the load runs; the first row is
/// selected; the suffix reads `Loading… <n>`; afterwards the count is 8.
#[gpui_kit::test]
fn rows_stream_in_while_loading(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_paused(cx, &world, &[]);
    assert!(f.read(cx, |list, _| list.is_loading()));
    assert_eq!(
        f.read(cx, |list, _| list.search_status().text),
        "Loading… 0"
    );
    f.step_until(cx, "the first batch", |list| {
        !list.loading_snapshots().is_empty()
    });
    let snapshot = f.read(cx, |list, _| list.loading_snapshots()[0].clone());
    assert!(snapshot.is_loading, "{snapshot:?}");
    assert!(snapshot.rows >= 1, "{snapshot:?}");
    assert_eq!(snapshot.selected, Some(0));
    assert_eq!(snapshot.status, format!("Loading… {}", snapshot.rows));
    f.resume(cx);
    f.wait_loaded(cx);
    assert_eq!(f.paths(cx).len(), 8);
    assert_eq!(f.read(cx, |list, _| list.search_status().text), "1/8");
}

/// L-003: after `Loaded` the rows are newest first.
#[gpui_kit::test]
fn loaded_rows_are_newest_first(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let times: Vec<_> = f.read(cx, |list, _| {
        (0..8)
            .map(|pos| list.browser().row(pos).unwrap().timestamp)
            .collect()
    });
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]), "{times:?}");
    assert_eq!(f.selected_position(cx), Some(0));
}

/// L-004, L-003: a query typed during the load is kept and not dispatched;
/// after the load only the matching rows remain.
#[gpui_kit::test]
fn a_query_typed_while_loading_is_applied_at_the_end(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    SessionSpec::new("proj", 40)
        .summary("Alpha")
        .messages(&[("user", "alpha rollout plan"), ("assistant", "ok")])
        .write(&world);
    let f = ListFixture::open_paused(cx, &world, &[]);
    f.type_text(cx, "alpha");
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).as_ref(), "alpha");
    assert_eq!(
        f.read(cx, |list, _| list.browser().query().to_string()),
        "alpha"
    );
    f.step_until(cx, "the first batch", |list| {
        !list.loading_snapshots().is_empty()
    });
    let received = f.read(cx, |list, _| list.loading_snapshots()[0].rows);
    // The query is not applied mid-load: every received row is listed.
    assert!(received >= 1);
    assert_eq!(
        f.read(cx, |list, _| list.browser().filtered().len()),
        received
    );
    f.resume(cx);
    f.wait_loaded(cx);
    assert_eq!(f.paths(cx).len(), 1, "only the alpha row remains");
    assert_eq!(f.titles(cx), vec!["Alpha".to_string()]);
}

/// L-005: an empty history shows the no-history state with Refresh; with a
/// time filter that hides every file the text mentions the range and Clear
/// time filter reloads unfiltered.
#[gpui_kit::test]
fn empty_history_offers_refresh_and_clear_time_filter(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let state = f
        .read(cx, |list, cx| list.empty_state(cx))
        .expect("an empty state");
    assert_eq!(
        state.title,
        "No conversation history found in selected scope"
    );
    assert_eq!(state.kind, StateKind::Info);
    assert_eq!(state.actions, vec![StateAction::Refresh]);
}

/// L-005 (time range variant) in its own app.
#[gpui_kit::test]
fn time_filter_hides_everything_with_a_clear_button(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .start(Utc::now() - Duration::days(400))
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--since", "1d"]);
    let state = f
        .read(cx, |list, cx| list.empty_state(cx))
        .expect("an empty state");
    assert_eq!(
        state.title,
        "No conversation history found in selected scope within the requested time range"
    );
    assert_eq!(
        state.actions,
        vec![StateAction::Refresh, StateAction::ClearTimeFilter]
    );
    // L-048: the chip says `Since 1d`.
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().time_label)
            .as_ref(),
        "Since 1d"
    );
    f.with_window(cx, |list, window, cx| {
        list.run_state_action(StateAction::ClearTimeFilter, window, cx)
    });
    f.wait_loaded(cx);
    assert_eq!(f.paths(cx).len(), 1, "the old session is listed unfiltered");
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().time_label)
            .as_ref(),
        "Any time"
    );
}

/// L-006: a missing projects directory is an error view with Retry; Retry
/// starts a load.
#[gpui_kit::test]
fn a_missing_projects_dir_is_an_error_with_retry(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    std::fs::remove_dir_all(world.projects_root()).unwrap();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let state = f
        .read(cx, |list, cx| list.empty_state(cx))
        .expect("an error state");
    assert_eq!(state.kind, StateKind::Error);
    assert_eq!(state.title, "Couldn't load history");
    assert!(
        state
            .description
            .as_deref()
            .is_some_and(|text| text.starts_with("Claude projects directory not found at ")),
        "{state:?}"
    );
    assert_eq!(
        state.actions,
        vec![StateAction::Retry, StateAction::ShowDebugLog]
    );
    f.with_window(cx, |list, window, cx| {
        list.run_state_action(StateAction::Retry, window, cx)
    });
    assert!(
        f.read(cx, |list, _| list.is_loading()),
        "Retry starts a load"
    );
    f.wait_loaded(cx);
    // Still failing: the error view is back, not a refresh toast.
    assert_eq!(
        f.read(cx, |list, cx| list.empty_state(cx)).unwrap().title,
        "Couldn't load history"
    );
}

/// L-009: project errors of the initial load warn with a Show debug log action.
#[gpui_kit::test]
fn project_errors_warn(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.with_window(cx, |list, window, cx| {
        list.inject_load_event(
            LoadEvent::Loaded {
                conversations: 5,
                project_errors: 2,
            },
            window,
            cx,
        )
    });
    let record = f.read(cx, |list, _| list.toasts().last().cloned()).unwrap();
    assert_eq!(record.kind, ToastKind::Warning);
    assert_eq!(
        record.text,
        "2 project(s) could not be loaded. See Tools → Debug log."
    );
    assert_eq!(record.buttons, vec!["Show debug log".to_string()]);
}

/// L-010: with `--debug info` the loader's lines reach the DebugBuffer.
#[gpui_kit::test]
fn debug_lines_reach_the_buffer(cx: &mut TestAppContext) {
    let _guard = global_lock();
    let world = TestWorld::new();
    listed_corpus(&world);
    ListFixture::init_app(cx, &world, &["--debug", "info"]);
    cx.update(|cx| crate::debug_sink::install(cx, Some(DebugLevel::Info)));
    let f = ListFixture::open_loaded(cx, &world, &["--debug", "info"]);
    let lines = cx.read(|cx| cx.global::<crate::globals::DebugBufferHandle>().0.lines());
    claude_history_core::debug::reset_sink();
    assert!(
        lines
            .iter()
            .any(|line| line.text.starts_with("Total conversations loaded:")),
        "{lines:#?}"
    );
    drop(f);
}

/// L-072: Refresh during the initial load starts nothing new; a running
/// refresh shows the persistent indicator and pushes no toast at its start.
#[gpui_kit::test]
fn refresh_is_ignored_while_loading_and_indicates_itself(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_paused(cx, &world, &[]);
    f.with_window(cx, |list, _, cx| list.refresh(cx));
    assert!(
        !f.read(cx, |list, _| list.is_refreshing()),
        "nothing new started"
    );
    f.resume(cx);
    f.wait_loaded(cx);

    f.with_window(cx, |list, _, cx| list.refresh(cx));
    assert!(f.read(cx, |list, _| list.is_refreshing()));
    let model = cx
        .update_window(f.handle, |_, window, cx| {
            f.list.read(cx).footer_model(window, cx)
        })
        .unwrap();
    assert!(model.refreshing, "the footer shows the refresh indicator");
    assert!(model.chips.is_empty(), "the indicator replaces the hints");
    assert!(
        f.read(cx, |list, _| list.toasts().is_empty()),
        "no start toast"
    );
    f.wait_loaded(cx);
}

/// L-073, L-038: a new file shows `Refreshed conversations: 8 → 9`; two
/// refreshes in a row leave the latest text; the selection by path survives
/// (L-074).
#[gpui_kit::test]
fn refresh_reports_and_keeps_the_selection(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.with_window(cx, |list, _, cx| {
        for _ in 0..4 {
            list.select_by(1, cx);
        }
    });
    let selected = f.selected_path(cx).unwrap();
    assert_eq!(f.selected_position(cx), Some(4));

    SessionSpec::new("proj", 50)
        .summary("Brand new")
        .write(&world);
    f.with_window(cx, |list, _, cx| list.refresh(cx));
    f.wait_loaded(cx);
    let last = f.read(cx, |list, _| list.toasts().last().cloned()).unwrap();
    assert_eq!(last.text, "Refreshed conversations: 8 → 9");
    assert_eq!(last.kind, ToastKind::Success);
    assert_eq!(f.selected_path(cx), Some(selected), "selection by path");
    assert_eq!(f.selected_position(cx), Some(5), "one newer row above");

    SessionSpec::new("proj", 51)
        .summary("Another")
        .write(&world);
    f.with_window(cx, |list, _, cx| list.refresh(cx));
    f.wait_loaded(cx);
    let texts: Vec<String> = f.read(cx, |list, _| {
        list.toasts()
            .iter()
            .map(|toast| toast.text.clone())
            .collect()
    });
    assert_eq!(
        texts,
        vec![
            "Refreshed conversations: 8 → 9".to_string(),
            "Refreshed conversations: 9 → 10".to_string()
        ]
    );
    assert_eq!(
        cx.read(crate::globals::last_toast).as_ref(),
        "Refreshed conversations: 9 → 10"
    );
    // L-038: the second toast replaced the first: one notification shown.
    let shown = cx
        .update_window(f.handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.notifications(cx).len()
        })
        .unwrap();
    assert_eq!(shown, 1, "exactly one refresh toast");
}

/// L-073: incomplete, failed and interrupted refreshes keep the list and say so.
#[gpui_kit::test]
fn failed_refreshes_keep_the_list_with_recovery_actions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.with_window(cx, |list, window, cx| {
        list.inject_load_event(LoadEvent::RefreshIncomplete, window, cx);
        list.inject_load_event(
            LoadEvent::RefreshFailed(AppError::SessionNotFound("x".to_string())),
            window,
            cx,
        );
        list.inject_load_event(LoadEvent::RefreshInterrupted, window, cx);
    });
    let toasts = f.read(cx, |list, _| list.toasts().to_vec());
    assert_eq!(toasts.len(), 3);
    assert_eq!(
        toasts[0].text,
        "Refresh incomplete; keeping the current list"
    );
    assert_eq!(toasts[0].kind, ToastKind::Warning);
    assert_eq!(toasts[0].buttons, vec!["Show debug log".to_string()]);
    assert_eq!(toasts[1].text, "Refresh failed: Session not found: x");
    assert_eq!(toasts[1].kind, ToastKind::Error);
    assert_eq!(toasts[1].buttons, vec!["Retry".to_string()]);
    assert_eq!(toasts[2].text, "Refresh failed; keeping the current list");
    assert_eq!(f.paths(cx).len(), 8, "the list is kept");
}

/// L-076: a refresh that finds nothing reports `8 → 0` and shows the empty
/// state; the window stays open.
#[gpui_kit::test]
fn refresh_to_zero_rows_shows_the_empty_state(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let written = listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    for path in &written {
        let _ = std::fs::remove_file(path);
    }
    f.with_window(cx, |list, _, cx| list.refresh(cx));
    f.wait_loaded(cx);
    let last = f.read(cx, |list, _| list.toasts().last().cloned()).unwrap();
    assert_eq!(last.text, "Refreshed conversations: 8 → 0");
    let state = f.read(cx, |list, cx| list.empty_state(cx)).unwrap();
    assert_eq!(
        state.title,
        "No conversation history found in selected scope"
    );
    assert_eq!(cx.read(exit::quit_requested), None, "the app keeps running");
}

/// L-050, S-018: a pasted session ID shows exactly that row even when its
/// project is excluded; clearing the query hides it again.
#[gpui_kit::test]
fn uuid_jump_bypasses_exclusions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let hidden = SessionSpec::new("secret", 60)
        .summary("Hidden session")
        .write(&world);
    world.write_config(None, Some("[tui]\nexclude_projects = [\"secret\"]\n"));
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(f.paths(cx).len(), 8, "the excluded project is not listed");
    f.type_text(cx, &synth::uuid(60));
    f.wait_idle(cx);
    assert_eq!(f.paths(cx), vec![hidden.clone()]);
    f.press(cx, "escape");
    f.wait_idle(cx);
    assert_eq!(f.paths(cx).len(), 8, "the excluded row is hidden again");
}

/// S-015, S-016: a UUID outside the loaded corpus is looked up on disk in the
/// background (Pending right after the paste); an unknown UUID keeps the
/// previous rows and shows `Session not found: <uuid>`.
#[gpui_kit::test]
fn uuid_jump_loads_from_disk_in_the_background(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let old = SessionSpec::new("proj", 70)
        .summary("Old session")
        .start(Utc::now() - Duration::days(400))
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--since", "1d"]);
    let before = f.paths(cx);
    assert!(
        !before.contains(&old),
        "the old file is outside the time filter"
    );
    let events = f.record_events(cx);

    recording(cx).set_clipboard(Some(synth::uuid(70)));
    // Paste without parking: the lookup is pending right after the paste.
    cx.dispatch_keystroke(f.handle, gpui_kit::Keystroke::parse("ctrl-v").unwrap());
    assert!(
        matches!(
            f.read(cx, |list, _| list.uuid_lookup().cloned()),
            Some(claude_history_core::browser::UuidLookup::Pending { .. })
        ),
        "pending right after the paste"
    );
    f.wait_idle(cx);
    f.wait_until(cx, "the disk lookup", |list, _| {
        list.filtered_paths() == vec![old.clone()]
    });
    assert!(events.count_matching(|e| *e == ListPaneEvent::CorpusChanged) >= 1);

    // An unknown id keeps the rows and toasts.
    f.press(cx, "escape");
    f.wait_idle(cx);
    let rows_before = f.paths(cx);
    let unknown = synth::uuid(999_999);
    f.type_text(cx, &unknown);
    f.wait_until(cx, "the not-found toast", |list, _| {
        list.toasts()
            .iter()
            .any(|toast| toast.text == format!("Session not found: {unknown}"))
    });
    assert_eq!(f.paths(cx), rows_before, "the previous rows stay");
    let toast = f.read(cx, |list, _| list.toasts().last().cloned()).unwrap();
    assert_eq!(toast.kind, ToastKind::Error);
}

/// L-079: a workspace without conversations + Workspace scope shows the
/// scope state; Show all switches to All.
#[gpui_kit::test]
fn an_empty_scope_offers_show_all(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let empty_ws = world.home().join("empty-workspace");
    std::fs::create_dir_all(&empty_ws).unwrap();
    let f = ListFixture::open_loaded(
        cx,
        &world,
        &["--workspace", &empty_ws.display().to_string(), "--local"],
    );
    assert!(f.read(cx, |list, _| list.scope_is_workspace()));
    f.wait_idle(cx);
    let state = f.read(cx, |list, cx| list.empty_state(cx)).unwrap();
    assert_eq!(state.title, "No conversations in this scope");
    assert_eq!(
        state.description.as_deref(),
        Some("Switch to All or change the workspace.")
    );
    assert_eq!(state.actions, vec![StateAction::ShowAll]);
    f.with_window(cx, |list, window, cx| {
        list.run_state_action(StateAction::ShowAll, window, cx)
    });
    f.wait_idle(cx);
    assert!(!f.read(cx, |list, _| list.scope_is_workspace()));
    assert_eq!(f.paths(cx).len(), 8);
}

/// 17.3: preparing the corpus does not block the UI thread: every poll of
/// a load of 1,500 sessions returns quickly (debug build; the bound is generous because CI runners run the other GUI tests in parallel: a poll took 336 ms on loaded Linux),
/// and rows are listed before the load ends (K1: the install is a later poll).
#[gpui_kit::test]
fn polls_stay_short_while_the_corpus_is_prepared(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 1_500);
    let f = ListFixture::open_paused(cx, &world, &[]);
    let mut slowest = std::time::Duration::ZERO;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while f.read(cx, |list, _| list.is_loading()) {
        let started = std::time::Instant::now();
        f.step(cx);
        slowest = slowest.max(started.elapsed());
        cx.run_until_parked();
        assert!(
            std::time::Instant::now() < deadline,
            "the load did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        slowest < std::time::Duration::from_millis(1_000),
        "the slowest poll took {slowest:?}"
    );
    assert!(
        !f.read(cx, |list, _| list.loading_snapshots().is_empty()),
        "rows were listed before the load ended"
    );
    assert_eq!(f.paths(cx).len(), 1_500);
}

/// L-006: a retry after a failed initial load that ends `RefreshIncomplete`
/// shows the error view again instead of staying in Loading.
#[gpui_kit::test]
fn an_incomplete_retry_returns_to_the_error_view(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    std::fs::remove_dir_all(world.projects_root()).unwrap();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.with_window(cx, |list, window, cx| {
        list.run_state_action(StateAction::Retry, window, cx);
        list.inject_load_event(LoadEvent::RefreshIncomplete, window, cx);
    });
    assert!(!f.read(cx, |list, _| list.is_loading()));
    assert_eq!(
        f.read(cx, |list, cx| list.empty_state(cx)).unwrap().title,
        "Couldn't load history"
    );
}
