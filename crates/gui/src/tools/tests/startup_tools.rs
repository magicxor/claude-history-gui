//! G tests of the one-shot flags' dialogs (C-013, C-014, C-015): each startup
//! tool opens the right dialog prefilled.

use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

use super::main_window;
use crate::globals::last_toast;
use crate::model::StartupTool;
use crate::test_support::fixtures_list::{ListFixture, install_fake_embedder};
use crate::test_support::fixtures_tools::{
    settle, two_copy_session, wait_for, write_empty_transcript,
};
use crate::test_support::{TestWorld, recording, synth};
use crate::tools::common::Scope;
use crate::tools::semantic_cache::{GenerateState, current_generate};
use crate::tools::{debug_search, delete_by_id, delete_empty, semantic_report, updates};
use crate::workspace::startup::{last_dialog, run_startup_tool};

fn run(cx: &mut TestAppContext, handle: AnyWindowHandle, tool: StartupTool) {
    cx.update_window(handle, |_, window, cx| run_startup_tool(tool, window, cx))
        .unwrap();
    cx.run_until_parked();
}

/// A world with a real workspace folder and some sessions.
fn workspace_world() -> (TestWorld, String) {
    let world = TestWorld::new();
    let proj = world.home().join("work").join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    synth::corpus(&world, 4);
    for n in 0..2 {
        crate::test_support::fixtures_list::SessionSpec::new("proj", 300 + n)
            .cwd(proj.clone())
            .summary(&format!("proj token {n}"))
            .start(chrono::Utc::now())
            .write(&world);
    }
    let ws = proj.display().to_string();
    (world, ws)
}

/// C-013: `--delete ID` opens the dialog with the id and the found copies;
/// nothing is deleted.
#[gpui_kit::test]
fn delete_session_opens_the_dialog_with_the_copies(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (first, second, _dir) = two_copy_session(&world, 51);
    let id = synth::uuid(51);
    let (_f, handle) = main_window(cx, &world, &[]);
    run(cx, handle, StartupTool::DeleteSession(id.clone()));
    wait_for(cx, "Find", |cx| {
        delete_by_id::current_dialog(cx).is_some_and(|d| !d.read(cx).copies().is_empty())
    });
    let dialog = cx.read(delete_by_id::current_dialog).unwrap();
    assert_eq!(cx.read(|cx| dialog.read(cx).id_text(cx)), id);
    assert_eq!(cx.read(|cx| dialog.read(cx).copies().len()), 2);
    assert!(first.exists() && second.exists());
    assert_eq!(cx.read(|cx| last_dialog(cx).to_string()), "delete-by-id");
}

/// C-014: `--debug-search Q -L` opens the dialog on the Workspace scope and
/// runs on a fresh load.
#[gpui_kit::test]
fn debug_search_opens_prefilled_and_runs(cx: &mut TestAppContext) {
    let (world, ws) = workspace_world();
    let (_f, handle) = main_window(
        cx,
        &world,
        &["--workspace", &ws, "--local", "--since", "400d"],
    );
    run(
        cx,
        handle,
        StartupTool::DebugSearch {
            query: "token".to_string(),
            local: true,
        },
    );
    wait_for(cx, "the report", |cx| {
        debug_search::current_dialog(cx).is_some_and(|d| d.read(cx).report().is_some())
    });
    let dialog = cx.read(debug_search::current_dialog).unwrap();
    assert_eq!(cx.read(|cx| dialog.read(cx).query_text(cx)), "token");
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
    let rows = cx.read(|cx| dialog.read(cx).report().unwrap().rows.len());
    assert_eq!(rows, 2, "only the workspace's two sessions match");
}

/// C-015: the hidden semantic flags open the report dialog with the query,
/// top and scope prefilled.
#[gpui_kit::test]
fn semantic_search_and_dump_open_prefilled(cx: &mut TestAppContext) {
    let (world, ws) = workspace_world();
    ListFixture::init_app(
        cx,
        &world,
        &["--workspace", &ws, "--local", "--since", "400d"],
    );
    let _control = install_fake_embedder(cx);
    let (_f, handle) = main_window(cx, &world, &[]);
    run(
        cx,
        handle,
        StartupTool::SemanticSearch {
            query: "token".to_string(),
            top: 7,
            local: true,
        },
    );
    wait_for(cx, "the report", |cx| {
        semantic_report::current_dialog(cx).is_some_and(|d| {
            matches!(
                d.read(cx).state(),
                semantic_report::RunState::Done | semantic_report::RunState::Failed(_)
            )
        })
    });
    let dialog = cx.read(semantic_report::current_dialog).unwrap();
    assert_eq!(cx.read(|cx| dialog.read(cx).query_text(cx)), "token");
    assert_eq!(cx.read(|cx| dialog.read(cx).top_text(cx)), "7");
    assert!(!cx.read(|cx| dialog.read(cx).is_debug_dump()));
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
    assert!(
        cx.read(|cx| dialog.read(cx).output_text(cx))
            .contains("Semantic search: searching")
    );
    // The dump flag reuses the open dialog in dump mode.
    run(
        cx,
        handle,
        StartupTool::SemanticDebug {
            query: "token".to_string(),
            local: true,
        },
    );
    assert!(cx.read(|cx| dialog.read(cx).is_debug_dump()));
}

/// C-015: Generate opens idle (never auto-started) on the Workspace scope.
#[gpui_kit::test]
fn generate_opens_idle(cx: &mut TestAppContext) {
    let (world, ws) = workspace_world();
    ListFixture::init_app(cx, &world, &["--workspace", &ws, "--local"]);
    let control = install_fake_embedder(cx);
    let (_f, handle) = main_window(cx, &world, &[]);
    run(
        cx,
        handle,
        StartupTool::GenerateSemanticCache { local: true },
    );
    let dialog = cx
        .read(current_generate)
        .expect("the Generate dialog is open");
    assert_eq!(
        cx.read(|cx| dialog.read(cx).state().clone()),
        GenerateState::Idle
    );
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
    assert_eq!(control.loads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// C-015: Clear asks first and deletes nothing until confirmed.
#[gpui_kit::test]
fn clear_asks_first(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let dir = world.env.semantic_cache_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("embeddings-v1.bin"), "cache").unwrap();
    let (_f, handle) = main_window(cx, &world, &[]);
    run(cx, handle, StartupTool::ClearSemanticCache);
    settle(cx, handle);
    assert_eq!(
        cx.read(|cx| last_dialog(cx).to_string()),
        "clear-semantic-cache"
    );
    assert!(
        dir.join("embeddings-v1.bin").exists(),
        "nothing is deleted before the confirmation"
    );
    cx.simulate_keystrokes(handle, "n");
    cx.run_until_parked();
    assert!(dir.join("embeddings-v1.bin").exists(), "n cancels");
}

/// C-015 / T-001: delete-empty with `local` preselects the workspace.
#[gpui_kit::test]
fn delete_empty_opens_on_the_workspace(cx: &mut TestAppContext) {
    let (world, ws) = workspace_world();
    write_empty_transcript(
        &world,
        &world.home().join("work").join("proj"),
        61,
        Some("x"),
    );
    let (_f, handle) = main_window(cx, &world, &["--workspace", &ws, "--local"]);
    run(cx, handle, StartupTool::DeleteEmpty { local: true });
    wait_for(cx, "the scan", |cx| {
        delete_empty::current_dialog(cx)
            .is_some_and(|d| d.read(cx).state() == &delete_empty::ScanState::Ready)
    });
    let dialog = cx.read(delete_empty::current_dialog).unwrap();
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
    assert_eq!(cx.read(|cx| dialog.read(cx).candidates().len()), 1);
}

/// `update` and `--show-dir`.
#[gpui_kit::test]
fn update_and_show_dir(cx: &mut TestAppContext) {
    let (world, ws) = workspace_world();
    let (_f, handle) = main_window(cx, &world, &["--workspace", &ws]);
    recording(cx).set_release(Err("offline".to_string()));
    run(cx, handle, StartupTool::CheckForUpdates);
    wait_for(cx, "the check", |cx| {
        updates::current_dialog(cx)
            .is_some_and(|d| !matches!(d.read(cx).state(), updates::CheckState::Checking))
    });
    run(cx, handle, StartupTool::ShowDir);
    assert!(cx.read(last_toast).starts_with("Projects directory: "));
}
