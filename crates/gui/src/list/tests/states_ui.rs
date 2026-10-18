//! G tests that render the list area's non-row states and click their
//! recovery buttons (L-005, L-006, L-076, L-078, L-079).

use chrono::{Duration, Utc};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

use crate::test_support::fixtures_list::{ListFixture, SessionSpec, listed_corpus};
use crate::test_support::{TestWorld, recording};

/// Render a frame and return the labels of the visible state buttons (every
/// state under test has at least one), or `None` when none is drawn.
fn rendered_state(f: &ListFixture, cx: &mut TestAppContext) -> Option<Vec<String>> {
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        let buttons = (0..4usize)
            .map_while(|ix| window.try_find(("list-state-action", ix)))
            .filter(|button| button.visible())
            .map(|button| button.label().unwrap_or_default().to_string())
            .collect();
        let buttons: Vec<String> = buttons;
        (!buttons.is_empty()).then_some(buttons)
    })
    .unwrap()
}

fn click_state_button(f: &ListFixture, cx: &mut TestAppContext, ix: usize) {
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(("list-state-action", ix), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn has_dialog(f: &ListFixture, cx: &mut TestAppContext) -> bool {
    cx.update_window(f.handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

/// L-006: the error view is drawn with Retry and Show debug log; clicking
/// Retry starts a load, clicking Show debug log opens the log.
#[gpui_kit::test]
fn the_error_view_is_drawn_and_its_buttons_work(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    std::fs::remove_dir_all(world.projects_root()).unwrap();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(
        rendered_state(&f, cx),
        Some(vec!["Retry".to_string(), "Show debug log".to_string()])
    );
    // The failing reload ends at once on its loader thread: pause the poll
    // loop so the Loading state cannot be consumed before it is observed.
    f.list.update(cx, |list, _| list.set_poll_paused(true));
    click_state_button(&f, cx, 0);
    assert!(
        f.read(cx, |list, _| list.is_loading()),
        "clicking Retry starts a load"
    );
    f.resume(cx);
    f.wait_loaded(cx);
    assert!(
        rendered_state(&f, cx).is_some(),
        "still failing: still drawn"
    );
    assert!(!has_dialog(&f, cx));
    click_state_button(&f, cx, 1);
    assert!(has_dialog(&f, cx), "Show debug log opens the log");
}

/// L-005: the no-history state is drawn; Refresh reloads.
#[gpui_kit::test]
fn the_no_history_states_are_drawn_with_working_buttons(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(rendered_state(&f, cx), Some(vec!["Refresh".to_string()]));
    click_state_button(&f, cx, 0);
    f.wait_loaded(cx);
    assert!(rendered_state(&f, cx).is_some());
}

/// L-005 (time range variant): Clear time filter lists the old session.
#[gpui_kit::test]
fn the_time_range_state_is_drawn_with_working_buttons(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .start(Utc::now() - Duration::days(400))
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--since", "1d"]);
    assert_eq!(
        rendered_state(&f, cx),
        Some(vec!["Refresh".to_string(), "Clear time filter".to_string()])
    );
    click_state_button(&f, cx, 1);
    f.wait_loaded(cx);
    assert_eq!(f.paths(cx).len(), 1);
    assert_eq!(rendered_state(&f, cx), None, "rows replace the state");
}

/// L-079: the scope state is drawn; Show all lists the other projects.
#[gpui_kit::test]
fn the_scope_state_is_drawn_and_show_all_works(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let empty_ws = world.home().join("empty-workspace");
    std::fs::create_dir_all(&empty_ws).unwrap();
    let f = ListFixture::open_loaded(
        cx,
        &world,
        &["--workspace", &empty_ws.display().to_string(), "--local"],
    );
    f.wait_idle(cx);
    assert_eq!(rendered_state(&f, cx), Some(vec!["Show all".to_string()]));
    click_state_button(&f, cx, 0);
    f.wait_idle(cx);
    assert_eq!(f.paths(cx).len(), 8);
    assert_eq!(rendered_state(&f, cx), None);
}

/// L-078: the no-match state is drawn; Clear search restores the rows.
#[gpui_kit::test]
fn the_no_match_state_is_drawn_and_clear_search_works(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "zzzzqqqq-no-such-text");
    f.wait_idle(cx);
    assert_eq!(
        rendered_state(&f, cx),
        Some(vec!["Clear search".to_string()])
    );
    click_state_button(&f, cx, 0);
    f.wait_idle(cx);
    assert_eq!(f.paths(cx).len(), 8);
}

/// L-079: when `exclude_projects` hides every conversation the state names
/// the cause and offers Settings instead of a Show all that cannot help.
#[gpui_kit::test]
fn exclusions_hiding_everything_are_named(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("solo", 1)
        .summary("Only one")
        .write(&world);
    world.write_config(None, Some("[tui]\nexclude_projects = [\"solo\"]\n"));
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.wait_idle(cx);
    assert!(f.paths(cx).is_empty());
    let state = f.read(cx, |list, cx| list.empty_state(cx)).unwrap();
    assert_eq!(state.title, "Every conversation is in an excluded project");
    assert!(
        state
            .description
            .as_deref()
            .is_some_and(|d| d.contains("solo")),
        "{state:?}"
    );
    assert_eq!(
        rendered_state(&f, cx),
        Some(vec!["Open Settings".to_string()])
    );
    click_state_button(&f, cx, 0);
    assert!(has_dialog(&f, cx), "Open Settings opens the dialog");
    let _ = recording(cx);
}
