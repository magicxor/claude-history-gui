//! G tests of Show projects directory and Open JSONL file (T-016, T-019).

use gpui_kit::{AppContext as _, TestAppContext};

use super::main_window;
use crate::actions::ShowProjectsDirectory;
use crate::globals::{app_state, last_toast};
use crate::model::OpenPathsOptions;
use crate::services::ServiceCall;
use crate::test_support::fixtures_tools::wait_for;
use crate::test_support::{TestWorld, recording, synth};
use crate::tools::open_file::picker_options;
use crate::tools::projects_dir::{copy_path, projects_directory, reveal_path, toast_text};

/// T-016: with a workspace the toast names the Claude projects directory;
/// Copy and Reveal act on it, and the command is available (menu, palette).
#[gpui_kit::test]
fn show_projects_directory_with_a_workspace(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let ws = world.home().join("work").join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let (_f, handle) = main_window(cx, &world, &["--workspace", &ws.display().to_string()]);
    let dir = cx.read(projects_directory).expect("the directory resolves");
    cx.update_window(handle, |_, window, cx| {
        window.refresh();
        assert!(window.is_action_available(&ShowProjectsDirectory, cx));
    })
    .unwrap();
    // Through the window's listener, as the menu and the palette dispatch it.
    cx.dispatch_action(handle, ShowProjectsDirectory);
    cx.run_until_parked();
    assert!(cx.read(last_toast).starts_with("Projects directory: "));
    assert_eq!(
        dir,
        world.projects_root().join(synth::encoded_project_dir(&ws))
    );
    cx.update_window(handle, |_, window, cx| {
        crate::tools::show_projects_directory(window, cx)
    })
    .unwrap();
    // The folder does not exist yet.
    assert_eq!(cx.read(last_toast).as_ref(), toast_text(&dir, false));
    assert!(cx.read(last_toast).ends_with("(does not exist yet)"));
    std::fs::create_dir_all(&dir).unwrap();
    cx.update_window(handle, |_, window, cx| {
        crate::tools::show_projects_directory(window, cx)
    })
    .unwrap();
    assert_eq!(
        cx.read(last_toast).as_ref(),
        format!("Projects directory: {}", dir.display())
    );
    // The buttons' actions.
    cx.update_window(handle, |_, window, cx| copy_path(&dir, window, cx))
        .unwrap();
    assert_eq!(recording(cx).clipboard(), Some(dir.display().to_string()));
    cx.update(|cx| reveal_path(&dir, cx));
    assert!(recording(cx).calls().contains(&ServiceCall::Reveal(dir)));
}

/// T-016: without a workspace the command is unavailable — disabled in the
/// menus and the palette, a dispatch does nothing; a workspace chosen later
/// enables it. The `--show-dir` startup tool still runs it and shows core's
/// error instead of nothing (PD16).
#[gpui_kit::test]
fn show_projects_directory_needs_a_workspace(cx: &mut TestAppContext) {
    use crate::palette::palette_groups_for;

    let world = TestWorld::new();
    let (f, handle) = main_window(cx, &world, &[]);
    let available = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.refresh();
            window.is_action_available(&ShowProjectsDirectory, cx)
        })
        .unwrap()
    };
    assert!(!available(cx), "disabled without a workspace");
    let palette_disabled = cx
        .update_window(handle, |_, window, cx| {
            let focus = window.focused(cx);
            palette_groups_for(focus.as_ref(), window, cx)
                .into_iter()
                .flat_map(|group| group.entries)
                .find(|entry| entry.label.as_ref() == "Show projects directory")
                .map(|entry| entry.disabled)
        })
        .unwrap();
    assert_eq!(palette_disabled, Some(true), "muted in the palette");
    cx.dispatch_action(handle, ShowProjectsDirectory);
    cx.run_until_parked();
    assert_eq!(cx.read(last_toast).as_ref(), "", "a dispatch does nothing");
    // The startup tool path: core's error, never silence.
    let error = cx.read(projects_directory).unwrap_err();
    assert_eq!(
        error,
        claude_history_core::maintenance::WORKSPACE_REQUIRED_SHOW_DIR
    );
    cx.update_window(handle, |_, window, cx| {
        crate::tools::show_projects_directory(window, cx)
    })
    .unwrap();
    assert_eq!(cx.read(last_toast).as_ref(), error);
    // Choosing a workspace enables it.
    let ws = world.home().join("work").join("later");
    std::fs::create_dir_all(&ws).unwrap();
    let scope = claude_history_core::WorkspaceScope::new(&ws).unwrap();
    cx.update(|cx| {
        crate::globals::update_app_state(cx, |state, _| state.set_workspace(Some(scope)))
    });
    cx.run_until_parked();
    f.wait_idle(cx);
    assert!(available(cx), "enabled once a workspace is set");
}

/// T-019: the picker's answer opens one single-file window per file.
#[gpui_kit::test]
fn open_jsonl_file_opens_a_window_per_file(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = world.standard_corpus();
    let (_f, handle) = main_window(cx, &world, &[]);
    let before = cx.update(|cx| cx.windows().len());
    recording(cx).answer_open(Ok(Some(vec![corpus[0].clone(), corpus[1].clone()])));
    cx.update_window(handle, |_, window, cx| {
        crate::tools::open_jsonl_file_dialog(window, cx)
    })
    .unwrap();
    wait_for(cx, "two windows", |cx| cx.windows().len() == before + 2);
    let calls = recording(cx).calls();
    assert!(
        calls.contains(&ServiceCall::PromptOpen(picker_options())),
        "{calls:?}"
    );
    assert_eq!(
        picker_options(),
        OpenPathsOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: "Open".into()
        }
    );
}

/// A cancelled picker opens nothing; a failing one says so.
#[gpui_kit::test]
fn open_jsonl_file_cancel_and_error(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let before = cx.update(|cx| cx.windows().len());
    recording(cx).answer_open(Ok(None));
    cx.update_window(handle, |_, window, cx| {
        crate::tools::open_jsonl_file_dialog(window, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), before);
    recording(cx).answer_open(Err("no picker".to_string()));
    cx.update_window(handle, |_, window, cx| {
        crate::tools::open_jsonl_file_dialog(window, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read(last_toast).as_ref(),
        "Couldn't open the file picker: no picker"
    );
}

/// T-019: in picker mode the action is unavailable.
#[gpui_kit::test]
fn open_jsonl_file_is_unavailable_in_picker_mode(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &["--show-path"]);
    assert!(cx.read(|cx| app_state(cx).is_picker()));
    cx.update_window(handle, |_, window, cx| {
        crate::tools::open_jsonl_file_dialog(window, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        !recording(cx)
            .calls()
            .iter()
            .any(|call| matches!(call, ServiceCall::PromptOpen(_)))
    );
}
