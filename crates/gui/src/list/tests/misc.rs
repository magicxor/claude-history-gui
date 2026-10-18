//! G tests of the remaining small rows: the relative-time ticker (L-022),
//! the prompt (L-031), the status suffix (L-032), the no-match state (L-078),
//! the QA probes and the window-level wiring.

use std::time::Duration;

use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use crate::list::states::{StateAction, StateKind};
use crate::list::toolbar::NO_WORKSPACE_TOOLTIP;
use crate::test_support::fixtures_list::{
    ListFixture, SessionSpec, install_fake_embedder, listed_corpus, workspace_corpus,
};
use crate::test_support::{TestWorld, synth};

/// L-022: a row reading `just now` reads `1 min ago` after the clock moved
/// 61 s (the 30 s ticker re-renders it).
#[gpui_kit::test]
fn relative_times_follow_the_ticker(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .messages(&[("user", "question")])
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let timestamp = |cx: &TestAppContext| {
        f.read(cx, |list, _| list.row_models(0..1, 480.0))[0]
            .timestamp
            .clone()
    };
    assert_eq!(timestamp(cx), "just now");
    cx.executor().advance_clock(Duration::from_secs(61));
    cx.run_until_parked();
    assert_eq!(timestamp(cx), "1 min ago");
    cx.executor().advance_clock(Duration::from_secs(3 * 60));
    cx.run_until_parked();
    assert_eq!(timestamp(cx), "4 min ago");
}

/// L-031: the prompt is `❯`, `Project ❯` with the Workspace scope.
#[gpui_kit::test]
fn the_prompt_follows_the_scope(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (proj, _, _) = workspace_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--workspace", &proj.display().to_string()]);
    assert_eq!(f.read(cx, |list, _| list.prompt_text()), "❯");
    f.press(cx, "tab");
    assert_eq!(f.read(cx, |list, _| list.prompt_text()), "Project ❯");
    f.press(cx, "tab");
    assert_eq!(f.read(cx, |list, _| list.prompt_text()), "❯");
}

/// L-032: the status suffix reads `Loading… 3`, `1/8`, `0/0`, `sem 1/8`.
#[gpui_kit::test]
fn the_status_suffix_has_its_states(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    ListFixture::init_app(cx, &world, &[]);
    let _control = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(f.read(cx, |list, _| list.search_status().text), "1/8");
    f.press(cx, "ctrl-t");
    f.wait_idle(cx);
    assert_eq!(f.read(cx, |list, _| list.search_status().text), "sem 1/8");
    f.press(cx, "ctrl-t");
    f.type_text(cx, "qqqqzzzz");
    f.wait_idle(cx);
    assert_eq!(f.read(cx, |list, _| list.search_status().text), "0/0");

    let empty = TestWorld::new();
    let mut cx2 = cx.new_app();
    let f2 = ListFixture::open_loaded(&mut cx2, &empty, &[]);
    assert_eq!(f2.read(&cx2, |list, _| list.search_status().text), "0/0");
}

/// L-078: no matches → `No matching conversations` + Clear search.
#[gpui_kit::test]
fn no_matches_offer_clear_search(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 6);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "zzzz");
    f.wait_idle(cx);
    let state = f.read(cx, |list, cx| list.empty_state(cx)).unwrap();
    assert_eq!(state.title, "No matching conversations");
    assert_eq!(state.kind, StateKind::Info);
    assert_eq!(state.actions, vec![StateAction::ClearSearch]);
    f.with_window(cx, |list, window, cx| {
        list.run_state_action(StateAction::ClearSearch, window, cx)
    });
    f.wait_idle(cx);
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).as_ref(), "");
    assert_eq!(f.paths(cx).len(), 6);
}

/// L-084: the tooltip text of the disabled Workspace segment.
#[gpui_kit::test]
fn the_workspace_segment_explains_itself(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 2);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let model = f.read(cx, |list, _| list.toolbar_model());
    assert!(!model.workspace_enabled);
    assert_eq!(
        model.workspace_tooltip,
        "Choose a workspace to filter by project"
    );
    assert_eq!(model.workspace_tooltip, NO_WORKSPACE_TOOLTIP);
}

/// L-011, L-091: every painted row is an element of its own with an
/// accessibility label; the QA probes report counts and timings.
#[gpui_kit::test]
fn painted_rows_and_probes(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 12);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let paths = f.paths(cx);
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let first = window.find(format!("row:{}", paths[0].display()));
        assert!(
            first
                .label()
                .is_some_and(|label| label.contains("Synthetic conversation"))
        );
    })
    .unwrap();
    let probe = |key: &str, cx: &TestAppContext| f.read(cx, |list, cx| list.qa_probe(key, cx));
    assert_eq!(probe("list.count", cx).as_deref(), Some("12"));
    assert_eq!(probe("list.total", cx).as_deref(), Some("12"));
    assert_eq!(probe("list.loaded", cx).as_deref(), Some("true"));
    assert_eq!(probe("list.mode", cx).as_deref(), Some("lex"));
    assert_eq!(probe("list.scope", cx).as_deref(), Some("all"));
    assert_eq!(probe("list.time", cx).as_deref(), Some("Any time"));
    assert!(probe("list.frame_p95", cx).unwrap().parse::<f64>().is_ok());
    assert!(
        probe("list.ui_busy_max", cx)
            .unwrap()
            .parse::<f64>()
            .is_ok()
    );
    assert_eq!(probe("list.nonexistent", cx), None);
}
