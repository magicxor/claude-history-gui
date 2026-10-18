//! G tests of the review fixes: single-file windows, per-window dialogs, the
//! live Debug log, the list's capture level, the Generate gate and the
//! Delete-empty scope.

use std::sync::Arc;

use claude_history_core::debug::DebugLevel;
use gpui_kit::{AppContext as _, Entity, TestAppContext};

use super::main_window;
use crate::actions::ShowDebugLog;
use crate::globals::{SemanticGate, app_state, debug_buffer, update_app_state};
use crate::model::DebugLogTab;
use crate::test_support::fixtures_list::{ListFixture, install_fake_embedder, listed_corpus};
use crate::test_support::fixtures_tools::{settle, wait_for};
use crate::test_support::{TestWorld, global_lock, open_single_file, synth};
use crate::tools::common::{Scope, current_window};
use crate::tools::debug_log::{DebugLogDialog, current_dialog, open_debug_log};
use crate::tools::delete_empty::{DeleteEmptyDialog, ScanState, open_delete_empty};
use crate::tools::semantic_cache::{
    GenerateDialog, GenerateState, current_generate, open_generate_semantic_cache,
};

/// T-015: Tools -> Debug log (menu, shortcut, palette) works in a single-file
/// window; before the fix the window root was read while it was leased.
#[gpui_kit::test]
fn show_debug_log_works_in_a_single_file_window(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = listed_corpus(&world).remove(0);
    let handle = open_single_file(cx, &world, &path);
    cx.dispatch_action(handle, ShowDebugLog);
    settle(cx, handle);
    assert!(cx.read(current_dialog).is_some(), "the dialog opened");
    assert_eq!(cx.read(current_window::<DebugLogDialog>), Some(handle));
}

/// T-015: a Tools dialog opens in the invoking window, never reusing the one
/// of another window.
#[gpui_kit::test]
fn a_dialog_opens_in_the_invoking_window(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.standard_corpus();
    let (f, main) = main_window(cx, &world, &[]);
    cx.update_window(main, |_, window, cx| {
        open_debug_log(DebugLogTab::Messages, window, cx)
    })
    .unwrap();
    settle(cx, main);
    assert_eq!(cx.read(current_window::<DebugLogDialog>), Some(main));
    let path = f.paths(cx).remove(0);
    let single = open_single_file(cx, &world, &path);
    cx.update_window(single, |_, window, cx| {
        open_debug_log(DebugLogTab::Messages, window, cx)
    })
    .unwrap();
    settle(cx, single);
    assert_eq!(cx.read(current_window::<DebugLogDialog>), Some(single));
    assert!(cx.read(current_dialog).is_some());
}

/// T-015: the Messages tab keeps updating once the 5,000-line ring is full.
#[gpui_kit::test]
fn the_messages_tab_updates_with_a_full_ring(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    cx.update(|cx| {
        let buffer = debug_buffer(cx);
        buffer.clear();
        for n in 0..crate::globals::DEBUG_BUFFER_LINES + 10 {
            buffer.push(DebugLevel::Info, format!("filler {n}"));
        }
    });
    cx.update_window(handle, |_, window, cx| {
        open_debug_log(DebugLogTab::Messages, window, cx)
    })
    .unwrap();
    settle(cx, handle);
    let dialog = cx.read(current_dialog).expect("open");
    cx.update(|cx| debug_buffer(cx).push(DebugLevel::Info, "brand-new-line"));
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.tick(window, cx))
    })
    .unwrap();
    assert!(
        cx.read(|cx| dialog.read(cx).text(cx))
            .contains("brand-new-line")
    );
    cx.update(|cx| debug_buffer(cx).clear());
}

/// T-022: the level set in the dialog reaches the list's own next refresh.
#[gpui_kit::test]
fn the_level_reaches_the_lists_refresh(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    world.standard_corpus();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    cx.update(|cx| crate::debug_sink::install(cx, None));
    cx.update(|cx| debug_buffer(cx).clear());
    cx.update(|cx| debug_buffer(cx).set_min_level(DebugLevel::Debug));
    f.list.update(cx, |list, cx| list.refresh(cx));
    f.wait_loaded(cx);
    let debug_lines = cx.read(|cx| {
        debug_buffer(cx)
            .lines()
            .iter()
            .filter(|line| line.level == DebugLevel::Debug)
            .count()
    });
    cx.update(|cx| debug_buffer(cx).set_min_level(DebugLevel::Info));
    assert!(debug_lines > 0, "the list's refresh captured Debug lines");
}

/// T-008 / T-024: a Generate worker that dies leaves the gate and fails the
/// dialog.
#[gpui_kit::test]
fn a_dead_generate_worker_leaves_the_gate(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 4);
    ListFixture::init_app(cx, &world, &[]);
    let _control = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let handle = f.handle;
    cx.update(|cx| {
        crate::embedder::install_fake(
            cx,
            Arc::new(|| -> claude_history_core::Result<_> { panic!("worker dies (test)") }),
        )
    });
    cx.update_window(handle, |_, window, cx| {
        open_generate_semantic_cache(false, window, cx)
    })
    .unwrap();
    settle(cx, handle);
    let dialog: Entity<GenerateDialog> = cx.read(current_generate).expect("open");
    dialog.update(cx, |d, cx| d.start(cx));
    wait_for(cx, "the failed run", |cx| {
        matches!(dialog.read(cx).state(), GenerateState::Failed(_))
    });
    assert!(cx.read(SemanticGate::is_open), "the gate is left");
}

/// T-004: "Current workspace" never widens silently when the workspace goes.
#[gpui_kit::test]
fn delete_empty_does_not_widen_the_scope_silently(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let proj = world.home().join("work").join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let ws = proj.display().to_string();
    let (_f, handle) = main_window(cx, &world, &["--workspace", &ws, "--local"]);
    cx.update_window(handle, |_, window, cx| open_delete_empty(true, window, cx))
        .unwrap();
    wait_for(cx, "the scan", |cx| {
        crate::tools::delete_empty::current_dialog(cx)
            .is_some_and(|d| d.read(cx).state() == &ScanState::Ready)
    });
    let dialog: Entity<DeleteEmptyDialog> =
        cx.read(crate::tools::delete_empty::current_dialog).unwrap();
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
    cx.update(|cx| update_app_state(cx, |state, _| state.workspace = None));
    assert!(cx.read(|cx| app_state(cx).workspace.is_none()));
    dialog.update(cx, |d, cx| d.rescan(cx));
    assert_eq!(
        cx.read(|cx| dialog.read(cx).scope()),
        Scope::All,
        "the control shows the scope that is scanned"
    );
}
