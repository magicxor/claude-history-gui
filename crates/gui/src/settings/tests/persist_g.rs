//! G tests of the persistence runtime (`persist::install`, DESIGN §13.3,
//! PD17): the 500 ms debounce, the flush, the workspace/recents sync, window
//! bounds tracking. Writes are enabled per test (`PersistWrites(true)`); every
//! file lives in the test world's sandbox home.

use std::path::PathBuf;
use std::time::Duration;

use claude_history_core::WorkspaceScope;
use gpui_kit::{
    AppContext as _, BorrowAppContext as _, Bounds, TestAppContext, WindowBounds, point, px, size,
};

use crate::globals::{app_state, update_app_state};
use crate::model::LeftTab;
use crate::persist::{
    self, PersistWrites, RememberedSearchMode, RememberedToolDisplay, RememberedView, UiState,
    WindowKind, WindowPlacementKind,
};
use crate::test_support::{TestWorld, open_main};

fn state_path(world: &TestWorld) -> PathBuf {
    persist::state_file(&world.env).expect("state file")
}

fn written(world: &TestWorld) -> Option<UiState> {
    let text = std::fs::read_to_string(state_path(world)).ok()?;
    persist::parse(&text).ok()
}

fn enable_writes(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(PersistWrites(true)));
}

fn advance(cx: &mut TestAppContext, ms: u64) {
    cx.executor().advance_clock(Duration::from_millis(ms));
    cx.run_until_parked();
}

/// PD17: a change is written after the 500 ms debounce (not before), several
/// changes inside the window produce one write, and `flush` writes at once.
#[gpui_kit::test]
fn changes_are_saved_after_the_debounce_and_on_flush(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    enable_writes(cx);
    assert!(written(&world).is_none(), "nothing written at startup");

    cx.update(|cx| {
        persist::update_ui(cx, |ui| ui.left_tab = LeftTab::DeepSearch);
    });
    advance(cx, 300);
    assert!(written(&world).is_none(), "still inside the debounce");
    cx.update(|cx| {
        persist::update_ui(cx, |ui| ui.list_collapsed = true);
    });
    advance(cx, 300);
    assert!(
        written(&world).is_none(),
        "a later change restarts the debounce"
    );
    advance(cx, 300);
    let state = written(&world).expect("written after the debounce");
    assert_eq!(state.left_tab, LeftTab::DeepSearch);
    assert!(state.list_collapsed);

    // `flush` writes at once and cancels the pending save.
    cx.update(|cx| {
        persist::update_ui(cx, |ui| ui.deep_options_open = true);
        persist::flush(cx);
    });
    assert!(written(&world).expect("flushed").deep_options_open);

    // An unchanged state is not written again.
    let before = std::fs::metadata(state_path(&world))
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(Duration::from_millis(30));
    cx.update(persist::flush);
    assert_eq!(
        std::fs::metadata(state_path(&world))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

/// With writes disabled (tests, QA) nothing is ever written.
#[gpui_kit::test]
fn disabled_writes_leave_no_file(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        persist::update_ui(cx, |ui| ui.left_tab = LeftTab::DeepSearch);
        persist::flush(cx);
    });
    advance(cx, 1_000);
    assert!(written(&world).is_none());
}

/// C-040, C-041, C-042: the remembered values go to state.json.
#[gpui_kit::test]
fn remembered_values_are_written(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    enable_writes(cx);
    let export = world.home().join("exports");
    std::fs::create_dir_all(&export).unwrap();
    cx.update(|cx| {
        persist::remember_search_mode(RememberedSearchMode::Semantic, cx);
        persist::remember_view(
            RememberedView {
                tool_display: Some(RememberedToolDisplay::Full),
                show_thinking: Some(true),
                show_timing: Some(true),
            },
            cx,
        );
        persist::remember_export_dir(&export, cx);
        persist::remember_agent_panel(true, crate::model::AgentTab::Read, Some(40.0), cx);
        persist::remember_deep_options(true, cx);
        persist::flush(cx);
    });
    let state = written(&world).expect("written");
    assert_eq!(state.search_mode, Some(RememberedSearchMode::Semantic));
    assert_eq!(state.view.tool_display, Some(RememberedToolDisplay::Full));
    assert_eq!(state.view.show_thinking, Some(true));
    assert_eq!(state.view.show_timing, Some(true));
    assert_eq!(state.last_export_dir, Some(export));
    assert!(state.agent_panel.visible);
    assert_eq!(state.agent_panel.tab, crate::model::AgentTab::Read);
    assert_eq!(
        state.split.agent_panel_width_rem,
        persist::AGENT_WIDTH_MAX_REM,
        "the width is clamped"
    );
    assert!(state.deep_options_open);
}

/// C-039 / L-083: switching the workspace keeps `ui.workspace` and the
/// recents (newest first, capped, no duplicates) in step without the pane's
/// help.
#[gpui_kit::test]
fn the_workspace_and_the_recents_follow_the_app_state(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    enable_writes(cx);
    let dirs: Vec<PathBuf> = (0..12)
        .map(|n| {
            let dir = world.home().join(format!("ws-{n:02}"));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        })
        .collect();
    for dir in &dirs {
        let scope = WorkspaceScope::new(dir).expect("a valid workspace");
        cx.update(|cx| update_app_state(cx, |state, _| state.set_workspace(Some(scope))));
        cx.run_until_parked();
    }
    // Back to the first one, then none.
    let scope = WorkspaceScope::new(&dirs[5]).unwrap();
    cx.update(|cx| update_app_state(cx, |state, _| state.set_workspace(Some(scope))));
    cx.run_until_parked();
    let (workspace, recents) = cx.read(|cx| {
        let ui = &app_state(cx).ui;
        (ui.workspace.clone(), ui.recent_workspaces.clone())
    });
    assert_eq!(workspace.as_deref(), Some(dirs[5].as_path()));
    assert_eq!(recents.len(), persist::MAX_RECENT_WORKSPACES);
    assert_eq!(
        recents[0],
        WorkspaceScope::new(&dirs[5]).unwrap().dir().to_path_buf()
    );
    assert_eq!(
        recents.iter().filter(|dir| dir.ends_with("ws-05")).count(),
        1,
        "no duplicates"
    );
    cx.update(|cx| update_app_state(cx, |state, _| state.set_workspace(None)));
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| app_state(cx).ui.workspace.clone()), None);
    advance(cx, 600);
    let state = written(&world).expect("saved");
    assert_eq!(state.workspace, None);
    assert_eq!(
        state.recent_workspaces.len(),
        persist::MAX_RECENT_WORKSPACES
    );
    // The picker offers existing folders only.
    std::fs::remove_dir_all(&dirs[5]).unwrap();
    assert!(!state.visible_recents().iter().any(|d| d.ends_with("ws-05")));
}

/// C-037: the main window's bounds are tracked and saved with the next save.
#[gpui_kit::test]
fn the_main_windows_bounds_are_saved(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    enable_writes(cx);
    cx.update_window(handle, |_, window, cx| {
        window.bounds_changed(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(persist::flush);
    let state = written(&world).expect("written");
    let placement = state.main_window.expect("the main window's placement");
    assert_eq!(placement.kind, WindowPlacementKind::Windowed);
    assert!(placement.bounds.w > 0.0 && placement.bounds.h > 0.0);

    // A noted maximized placement is stored as such.
    cx.update(|cx| {
        persist::note_window_bounds(
            WindowKind::Main,
            WindowBounds::Maximized(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1500.), px(900.)),
            )),
            cx,
        );
        persist::flush(cx);
    });
    let state = written(&world).unwrap();
    assert_eq!(
        state.main_window.unwrap().kind,
        WindowPlacementKind::Maximized
    );
}

/// `restored_window_bounds` returns the clamped placement with
/// `restore_session` on and nothing when it is off.
#[gpui_kit::test]
fn restored_bounds_are_clamped_and_respect_restore_session(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    assert!(
        cx.read(|cx| persist::restored_window_bounds(WindowKind::Main, cx))
            .is_none(),
        "nothing remembered yet"
    );
    cx.update(|cx| {
        persist::update_ui(cx, |ui| {
            ui.main_window = Some(persist::WindowPlacement {
                kind: WindowPlacementKind::Windowed,
                bounds: persist::PlacementBounds {
                    x: 90_000.0,
                    y: 90_000.0,
                    w: 1000.0,
                    h: 700.0,
                },
            });
        })
    });
    let restored = cx
        .read(|cx| persist::restored_window_bounds(WindowKind::Main, cx))
        .expect("restored");
    let displays = cx.read(persist::display_rects);
    let bounds = restored.get_bounds();
    let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    assert!(
        displays
            .iter()
            .any(|d| x >= d.x && y >= d.y && x < d.x + d.w && y < d.y + d.h),
        "moved onto a display: {x},{y} not in {displays:?}"
    );
    // The window options take the remembered bounds over the defaults.
    let options = cx.read(|cx| {
        persist::with_restored_bounds(gpui_kit::WindowOptions::default(), WindowKind::Main, cx)
    });
    assert_eq!(
        options.window_bounds.map(|b| b.get_bounds()),
        Some(bounds),
        "with_restored_bounds applies the clamped placement"
    );
    cx.update(|cx| {
        cx.update_global::<crate::globals::AppState, _>(|state, _| {
            state.config.gui.restore_session = Some(false);
        });
    });
    assert!(
        cx.read(|cx| persist::restored_window_bounds(WindowKind::Main, cx))
            .is_none()
    );
    let options = cx.read(|cx| {
        persist::with_restored_bounds(gpui_kit::WindowOptions::default(), WindowKind::Main, cx)
    });
    assert!(options.window_bounds.is_none(), "restore_session off");
}

/// C-038: the agent panel's visibility, tab and width are remembered from
/// the UI (not only through the helper): opening a tab, closing it and
/// resizing it update `ui`; a new main window starts with the remembered
/// panel; the width is clamped to 18-28rem.
#[gpui_kit::test]
fn the_agent_panel_state_follows_the_ui_and_is_restored(cx: &mut TestAppContext) {
    use crate::model::AgentTab;
    let world = TestWorld::new();
    let (handle, ws) = open_main(cx, &world, &[]);
    let panel = cx.read(|cx| ws.read(cx).viewer().read(cx).agent_panel().clone());
    assert!(!cx.read(|cx| app_state(cx).ui.agent_panel.visible));

    cx.update_window(handle, |_, window, cx| {
        panel.update(cx, |panel, cx| panel.show_tab(AgentTab::Within, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let ui = &app_state(cx).ui;
        assert!(ui.agent_panel.visible);
        assert_eq!(ui.agent_panel.tab, AgentTab::Within);
    });

    // Dragging the handle: the width follows the pointer, clamped.
    cx.update(|cx| {
        panel.update(cx, |panel, cx| panel.set_width_rem(25.0, cx));
        assert_eq!(app_state(cx).ui.split.agent_panel_width_rem, 25.0);
        assert_eq!(crate::deep_search::panel::panel_width_rem(cx), 25.0);
        panel.update(cx, |panel, cx| panel.set_width_rem(100.0, cx));
        assert_eq!(
            app_state(cx).ui.split.agent_panel_width_rem,
            persist::AGENT_WIDTH_MAX_REM
        );
        panel.update(cx, |panel, cx| panel.set_width_rem(1.0, cx));
        assert_eq!(
            app_state(cx).ui.split.agent_panel_width_rem,
            persist::AGENT_WIDTH_MIN_REM
        );
        assert_eq!(
            crate::deep_search::panel::panel_width_rem(cx),
            persist::AGENT_WIDTH_MIN_REM,
            "18rem is reachable"
        );
    });

    // A second main window starts with the remembered panel.
    let (_handle2, ws2) = cx.update(|cx| {
        let (handle, view) = crate::workspace::open_main_window_with_view(cx);
        (handle, view)
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let panel2 = ws2.read(cx).viewer().read(cx).agent_panel().read(cx);
        assert!(panel2.is_visible());
        assert_eq!(panel2.tab(), AgentTab::Within);
    });

    cx.update(|cx| panel.update(cx, |panel, cx| panel.hide(cx)));
    cx.run_until_parked();
    assert!(!cx.read(|cx| app_state(cx).ui.agent_panel.visible));
}
