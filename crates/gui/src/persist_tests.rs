//! P tests of `persist.rs` (DESIGN §17.5): roundtrip, corrupt, clamp, v1
//! upgrade, recents, window placement. Hermetic: every file lives in a temp
//! sandbox home.

use std::path::{Path, PathBuf};

use claude_history_core::HistoryEnv;

use super::*;
use crate::model::{AgentTab, LeftTab};

fn sandbox() -> (tempfile::TempDir, HistoryEnv) {
    let tmp = tempfile::Builder::new()
        .prefix("chg-persist-")
        .tempdir()
        .expect("temp dir");
    let env = HistoryEnv::sandbox(tmp.path());
    (tmp, env)
}

fn full_state(base: &Path) -> UiState {
    let proj = base.join("proj");
    let api = base.join("api");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&api).unwrap();
    UiState {
        version: STATE_VERSION,
        main_window: Some(WindowPlacement {
            kind: WindowPlacementKind::Maximized,
            bounds: PlacementBounds {
                x: 10.0,
                y: 20.0,
                w: 1280.0,
                h: 820.0,
            },
        }),
        single_file_window: Some(WindowPlacement {
            kind: WindowPlacementKind::Windowed,
            bounds: PlacementBounds {
                x: 50.0,
                y: 60.0,
                w: 1000.0,
                h: 800.0,
            },
        }),
        split: SplitWidths {
            list_width_rem: 28.0,
            deep_width_rem: 36.0,
            agent_panel_width_rem: 20.0,
        },
        list_collapsed: true,
        agent_panel: AgentPanelState {
            visible: true,
            tab: AgentTab::Read,
        },
        left_tab: LeftTab::DeepSearch,
        deep_options_open: true,
        workspace: Some(proj.clone()),
        recent_workspaces: vec![proj, api],
        search_mode: Some(RememberedSearchMode::Semantic),
        view: RememberedView {
            tool_display: Some(RememberedToolDisplay::Full),
            show_thinking: Some(true),
            show_timing: Some(true),
        },
        last_export_dir: Some(base.to_path_buf()),
    }
}

/// C-036, C-038..C-042: every field roundtrips through state.json.
#[test]
fn every_field_roundtrips() {
    let (tmp, env) = sandbox();
    let state = full_state(tmp.path());
    let path = state_file(&env).expect("state file");
    write_state(&path, &state).unwrap();
    let report = load_with(&env, true);
    assert_eq!(report.warning, None);
    assert_eq!(report.state, state);
}

/// V-032: `view.show_timing` defaults to false (never remembered) and
/// roundtrips when set.
#[test]
fn show_timing_defaults_off_and_roundtrips() {
    assert_eq!(UiState::default().view.show_timing, None);
    let (_tmp, env) = sandbox();
    let path = state_file(&env).unwrap();
    let mut state = UiState::default();
    state.view.show_timing = Some(true);
    write_state(&path, &state).unwrap();
    assert_eq!(load_with(&env, true).state.view.show_timing, Some(true));
    state.view.show_timing = Some(false);
    write_state(&path, &state).unwrap();
    assert_eq!(load_with(&env, true).state.view.show_timing, Some(false));
}

/// C-036: a corrupt file is renamed to `state.json.bad`, defaults load, and
/// one warning line names both files; a second corrupt file replaces the
/// first `.bad`.
#[test]
fn a_corrupt_file_is_moved_to_bad() {
    let (_tmp, env) = sandbox();
    let path = state_file(&env).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    let report = load_with(&env, true);
    assert_eq!(report.state, UiState::default());
    let bad = bad_file(&env).unwrap();
    assert_eq!(report.moved_to.as_deref(), Some(bad.as_path()));
    assert!(!path.exists());
    assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
    let warning = report.warning.expect("a warning for the debug log");
    assert!(warning.contains("state.json") && warning.contains("state.json.bad"));

    std::fs::write(&path, "[1, 2").unwrap();
    let again = load_with(&env, true);
    assert_eq!(again.state, UiState::default());
    assert_eq!(std::fs::read_to_string(&bad).unwrap(), "[1, 2");

    // A missing file is not an error.
    let report = load_with(&env, true);
    assert_eq!(report.warning, None);
    assert_eq!(report.state, UiState::default());
}

/// C-036: a version-1 file loads; its time filter is ignored.
#[test]
fn a_v1_file_loads_and_ignores_the_time_filter() {
    let (_tmp, env) = sandbox();
    let path = state_file(&env).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{ "version": 1, "time_filter_text": "2d", "left_tab": "deep_search",
             "split": { "list_width_rem": 26.0 }, "view": { "show_thinking": true } }"#,
    )
    .unwrap();
    let report = load_with(&env, true);
    assert_eq!(report.warning, None);
    assert_eq!(report.state.version, STATE_VERSION);
    assert_eq!(report.state.left_tab, LeftTab::DeepSearch);
    assert_eq!(report.state.split.list_width_rem, 26.0);
    assert_eq!(report.state.split.deep_width_rem, 34.0);
    assert_eq!(report.state.view.show_thinking, Some(true));
    // The v1 field is dropped when the file is written again.
    write_state(&path, &report.state).unwrap();
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("time_filter")
    );
}

/// C-038: restored widths are clamped to their ranges; NaN and infinity
/// fall back to the minimum.
#[test]
fn restored_widths_are_clamped() {
    let state = UiState {
        split: SplitWidths {
            list_width_rem: 90.0,
            deep_width_rem: 3.0,
            agent_panel_width_rem: f32::NAN,
        },
        ..UiState::default()
    }
    .sanitized();
    assert_eq!(
        state.split.list_width_rem,
        crate::workspace::LIST_WIDTH_MAX_REM
    );
    assert_eq!(
        state.split.deep_width_rem,
        crate::workspace::DEEP_WIDTH_MIN_REM
    );
    assert_eq!(state.split.agent_panel_width_rem, AGENT_WIDTH_MIN_REM);

    let mut state = UiState::default();
    state.split.agent_panel_width_rem = f32::INFINITY;
    state.split.list_width_rem = f32::NEG_INFINITY;
    let state = state.sanitized();
    assert_eq!(state.split.agent_panel_width_rem, AGENT_WIDTH_MIN_REM);
    assert_eq!(
        state.split.list_width_rem,
        crate::workspace::LIST_WIDTH_MIN_REM
    );

    // A loaded file is clamped too.
    let (_tmp, env) = sandbox();
    let path = state_file(&env).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{ "split": { "list_width_rem": 500, "agent_panel_width_rem": 1 } }"#,
    )
    .unwrap();
    let loaded = load_with(&env, true).state;
    assert_eq!(
        loaded.split.list_width_rem,
        crate::workspace::LIST_WIDTH_MAX_REM
    );
    assert_eq!(loaded.split.agent_panel_width_rem, AGENT_WIDTH_MIN_REM);
}

/// L-083: 12 workspaces keep the 10 newest, newest first; a deleted folder
/// is not offered.
#[test]
fn recents_keep_ten_newest_first_and_hide_missing_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs: Vec<PathBuf> = (0..12)
        .map(|n| {
            let dir = tmp.path().join(format!("ws-{n:02}"));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        })
        .collect();
    let mut recents = Vec::new();
    for dir in &dirs {
        add_recent(&mut recents, dir);
    }
    assert_eq!(recents.len(), MAX_RECENT_WORKSPACES);
    assert_eq!(recents[0], dirs[11]);
    assert_eq!(recents[9], dirs[2]);
    assert!(!recents.contains(&dirs[0]) && !recents.contains(&dirs[1]));

    // Re-adding an old entry moves it to the front without duplicating it.
    add_recent(&mut recents, &dirs[5]);
    assert_eq!(recents[0], dirs[5]);
    assert_eq!(recents.iter().filter(|dir| **dir == dirs[5]).count(), 1);
    assert_eq!(recents.len(), MAX_RECENT_WORKSPACES);

    // A deleted folder is hidden.
    std::fs::remove_dir_all(&dirs[11]).unwrap();
    let visible = visible_recents(&recents);
    assert!(!visible.contains(&dirs[11]));
    assert_eq!(visible.len(), MAX_RECENT_WORKSPACES - 1);
    assert_eq!(visible[0], dirs[5]);

    // A long stored list is capped on load and duplicates collapse.
    let state = UiState {
        recent_workspaces: dirs.iter().rev().chain(dirs.iter()).cloned().collect(),
        ..UiState::default()
    }
    .sanitized();
    assert_eq!(state.recent_workspaces.len(), MAX_RECENT_WORKSPACES);
    assert_eq!(state.recent_workspaces[0], dirs[11]);
}

/// C-039: workspace + recents roundtrip; a missing workspace folder is not
/// restored.
#[test]
fn a_missing_workspace_folder_is_not_restored() {
    let (tmp, env) = sandbox();
    let state = full_state(tmp.path());
    let path = state_file(&env).unwrap();
    write_state(&path, &state).unwrap();
    assert_eq!(load_with(&env, true).state.workspace, state.workspace);

    std::fs::remove_dir_all(state.workspace.as_ref().unwrap()).unwrap();
    let loaded = load_with(&env, true).state;
    assert_eq!(loaded.workspace, None);
    assert_eq!(loaded.visible_recents().len(), 1);
}

/// C-042: the export folder roundtrips; a missing one falls back to
/// Downloads, then home.
#[test]
fn a_missing_export_folder_falls_back_to_downloads_then_home() {
    let tmp = tempfile::tempdir().unwrap();
    let downloads = tmp.path().join("Downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let home = tmp.path().to_path_buf();
    let gone = tmp.path().join("gone");
    assert_eq!(
        export_dir_or_default(Some(&downloads), Some(&downloads), Some(&home)),
        Some(downloads.clone())
    );
    assert_eq!(
        export_dir_or_default(Some(&gone), Some(&downloads), Some(&home)),
        Some(downloads.clone())
    );
    assert_eq!(
        export_dir_or_default(Some(&gone), Some(&gone), Some(&home)),
        Some(home.clone())
    );
    assert_eq!(export_dir_or_default(None, None, None), None);

    let mut state = UiState {
        last_export_dir: Some(gone),
        ..UiState::default()
    };
    assert_eq!(state.export_dir(), None);
    state.last_export_dir = Some(downloads.clone());
    assert_eq!(state.export_dir(), Some(downloads.as_path()));
}

/// `restore_session = false`: only the export folder and the recents are used.
#[test]
fn restore_session_off_keeps_only_export_dir_and_recents() {
    let (tmp, env) = sandbox();
    let state = full_state(tmp.path());
    write_state(&state_file(&env).unwrap(), &state).unwrap();
    let loaded = load_with(&env, false).state;
    assert_eq!(loaded.recent_workspaces, state.recent_workspaces);
    assert_eq!(loaded.last_export_dir, state.last_export_dir);
    assert_eq!(loaded.main_window, None);
    assert_eq!(loaded.workspace, None);
    assert_eq!(loaded.left_tab, LeftTab::Conversations);
    assert!(!loaded.list_collapsed);
    assert_eq!(loaded.view, RememberedView::default());
    assert_eq!(loaded.search_mode, None);
}

/// `load` reads `[gui] restore_session` from gui.toml itself.
#[test]
fn load_follows_restore_session_in_gui_toml() {
    let (tmp, env) = sandbox();
    let state = full_state(tmp.path());
    write_state(&state_file(&env).unwrap(), &state).unwrap();
    let gui = env.gui_config_file().unwrap();
    std::fs::create_dir_all(gui.parent().unwrap()).unwrap();
    std::fs::write(&gui, "[gui]\nrestore_session = false\n").unwrap();
    let loaded = load(&env);
    assert_eq!(loaded.left_tab, LeftTab::Conversations);
    assert_eq!(loaded.last_export_dir, state.last_export_dir);
    std::fs::write(&gui, "[gui]\nrestore_session = true\n").unwrap();
    assert_eq!(load(&env).left_tab, LeftTab::DeepSearch);
}

fn placement(x: f32, y: f32, w: f32, h: f32) -> WindowPlacement {
    WindowPlacement {
        kind: WindowPlacementKind::Windowed,
        bounds: PlacementBounds { x, y, w, h },
    }
}

/// C-037: bounds outside every display move onto the primary display;
/// oversized bounds are shrunk; partly hidden bounds move inside.
#[test]
fn window_bounds_are_clamped_to_the_displays() {
    let primary = DisplayRect::new(0.0, 0.0, 1920.0, 1080.0);
    let second = DisplayRect::new(1920.0, 0.0, 1280.0, 1024.0);
    let displays = [primary, second];

    // Inside: unchanged.
    let inside = placement(100.0, 80.0, 1280.0, 820.0);
    assert_eq!(clamp_placement(inside, &displays), inside);

    // On the second display: stays there.
    let on_second = placement(2000.0, 100.0, 1000.0, 700.0);
    assert_eq!(clamp_placement(on_second, &displays), on_second);

    // Outside every display (the second monitor was unplugged): onto the
    // primary display, same size, centered.
    let gone = placement(5000.0, 4000.0, 1000.0, 700.0);
    let moved = clamp_placement(gone, &[primary]);
    assert_eq!(moved.bounds.w, 1000.0);
    assert_eq!(moved.bounds.h, 700.0);
    assert_eq!(moved.bounds.x, 460.0);
    assert_eq!(moved.bounds.y, 190.0);

    // Oversized: shrunk to the display.
    let huge = placement(0.0, 0.0, 6000.0, 4000.0);
    let shrunk = clamp_placement(huge, &[primary]);
    assert_eq!((shrunk.bounds.w, shrunk.bounds.h), (1920.0, 1080.0));
    assert_eq!((shrunk.bounds.x, shrunk.bounds.y), (0.0, 0.0));

    // Partly off the right/bottom edge: moved fully inside.
    let edge = placement(1500.0, 900.0, 900.0, 600.0);
    let fitted = clamp_placement(edge, &[primary]);
    assert_eq!((fitted.bounds.x, fitted.bounds.y), (1020.0, 480.0));
    assert_eq!((fitted.bounds.w, fitted.bounds.h), (900.0, 600.0));

    // Partly off the left/top edge.
    let corner = placement(-300.0, -100.0, 800.0, 600.0);
    let fitted = clamp_placement(corner, &[primary]);
    assert_eq!((fitted.bounds.x, fitted.bounds.y), (0.0, 0.0));

    // The kind is kept; no displays: unchanged.
    let maximized = WindowPlacement {
        kind: WindowPlacementKind::Maximized,
        ..gone
    };
    assert_eq!(
        clamp_placement(maximized, &[primary]).kind,
        WindowPlacementKind::Maximized
    );
    assert_eq!(clamp_placement(gone, &[]), gone);
}

/// A tiny stored window never shrinks the window below the minimum size
/// (when the display is large enough).
#[test]
fn tiny_windows_grow_to_the_minimum() {
    let primary = DisplayRect::new(0.0, 0.0, 1920.0, 1080.0);
    let tiny = placement(10.0, 10.0, 100.0, 100.0);
    let fitted = clamp_placement(tiny, &[primary]);
    assert_eq!((fitted.bounds.w, fitted.bounds.h), MIN_WINDOW_SIZE);
}

/// Unusable stored placements (zero size, NaN) are dropped on load.
#[test]
fn unusable_placements_are_dropped() {
    let state = UiState {
        main_window: Some(placement(0.0, 0.0, 0.0, 0.0)),
        single_file_window: Some(placement(f32::NAN, 0.0, 800.0, 600.0)),
        ..UiState::default()
    }
    .sanitized();
    assert_eq!(state.main_window, None);
    assert_eq!(state.single_file_window, None);
}

#[test]
fn placements_convert_to_window_bounds_and_back() {
    let stored = WindowPlacement {
        kind: WindowPlacementKind::Fullscreen,
        bounds: PlacementBounds {
            x: 5.0,
            y: 6.0,
            w: 700.0,
            h: 500.0,
        },
    };
    assert_eq!(placement_of(window_bounds_of(&stored)), stored);
}

/// Writes are atomic: no temp file is left behind and an existing file is replaced.
#[test]
fn writes_replace_the_file_and_leave_no_temp_file() {
    let (_tmp, env) = sandbox();
    let path = state_file(&env).unwrap();
    let mut state = UiState::default();
    write_state(&path, &state).unwrap();
    state.left_tab = LeftTab::DeepSearch;
    write_state(&path, &state).unwrap();
    assert_eq!(
        parse(&std::fs::read_to_string(&path).unwrap()).unwrap(),
        state
    );
    let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|name| name.ends_with(".tmp")),
        "{names:?}"
    );
}
