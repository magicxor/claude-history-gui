//! G tests of rename (A-017…A-020, A-024, A-028 rename races).

use std::path::Path;

use gpui_kit::{AppContext as _, Entity, TestAppContext};

use super::{dialog_id, has_dialog, render, toast};
use crate::ops::rename::{DIALOG_ID, LastRename, NOT_IN_LIST, RenameDialog};
use crate::test_support::TestWorld;
use crate::test_support::fault;
use crate::test_support::fixtures_list::SessionSpec;
use crate::test_support::fixtures_ops::{
    OpsFixture, claude_session, omp_session, pi_session, project_dir,
};

fn dialog(cx: &TestAppContext) -> Entity<RenameDialog> {
    cx.read(|cx| {
        cx.global::<LastRename>()
            .0
            .upgrade()
            .expect("a rename dialog was opened")
    })
}

fn input_value(cx: &TestAppContext, dialog: &Entity<RenameDialog>) -> String {
    cx.read(|cx| dialog.read(cx).input().read(cx).value().to_string())
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn open_rename(cx: &mut TestAppContext, f: &OpsFixture, path: &Path) -> Entity<RenameDialog> {
    f.select(cx, path);
    f.press(cx, "f2");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "F2 opens the rename dialog");
    assert_eq!(dialog_id(cx), DIALOG_ID);
    render(cx, f.handle());
    dialog(cx)
}

/// A-017, A-019, A-020: F2 on a list row prefills the custom title; typing and
/// Enter save it (the file gets `custom-title` + `agent-name`), the row title
/// changes and no toast appears.
#[gpui_kit::test]
fn rename_from_the_list_saves_and_updates_the_row(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = project_dir(&world, "proj");
    let path = SessionSpec::new("proj", 1)
        .cwd(cwd)
        .summary("Fix login bug")
        .custom_title("Old title")
        .write(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let toast_before = toast(cx);
    let dialog = open_rename(cx, &f, &path);
    assert_eq!(
        input_value(cx, &dialog),
        "Old title",
        "prefilled with the custom title"
    );
    f.press(cx, "ctrl-u");
    assert_eq!(input_value(cx, &dialog), "");
    cx.simulate_input(f.handle(), "New title");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()), "closed after the save");
    let written = lines(&path);
    let [.., custom, agent] = written.as_slice() else {
        panic!("{written:?}")
    };
    assert!(
        custom.contains("custom-title") && custom.contains("New title"),
        "{custom}"
    );
    assert!(
        agent.contains("agent-name") && agent.contains("New title"),
        "{agent}"
    );
    assert_eq!(
        f.target(cx, &path).title.as_ref(),
        "New title",
        "the row updated in place"
    );
    assert_eq!(toast(cx), toast_before, "success shows no toast (PD16)");
}

/// A-017: F2 in the viewer renames the viewer's conversation even though the
/// list selection moved (A-028).
#[gpui_kit::test]
fn rename_from_the_viewer_uses_the_viewers_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let shown = claude_session(&world, "proj", 1, "Shown in the viewer");
    let other = claude_session(&world, "proj", 2, "Selected in the list");
    let other_before = std::fs::read(&other).unwrap();
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &shown);
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&other, cx))
    );
    f.press(cx, "f2");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    render(cx, f.handle());
    let dialog = dialog(cx);
    assert_eq!(cx.read(|cx| dialog.read(cx).target().path.clone()), shown);
    cx.simulate_input(f.handle(), "Viewer rename");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(
        lines(&shown)
            .iter()
            .any(|line| line.contains("Viewer rename"))
    );
    assert_eq!(
        std::fs::read(&other).unwrap(),
        other_before,
        "the selected row is untouched"
    );
}

/// A-018: Ctrl+U clears, Ctrl+E goes to the end, Home goes to the start, Esc
/// closes without writing.
#[gpui_kit::test]
fn rename_input_keys_and_escape(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = project_dir(&world, "proj");
    let path = SessionSpec::new("proj", 1)
        .cwd(cwd)
        .custom_title("Old")
        .write(&world);
    let before = std::fs::read(&path).unwrap();
    let f = OpsFixture::open(cx, &world, &[]);
    let dialog = open_rename(cx, &f, &path);
    f.press(cx, "ctrl-u");
    cx.simulate_input(f.handle(), "abc");
    f.press(cx, "home");
    cx.simulate_input(f.handle(), "X");
    assert_eq!(input_value(cx, &dialog), "Xabc", "Home moves to the start");
    f.press(cx, "ctrl-e");
    cx.simulate_input(f.handle(), "Y");
    assert_eq!(input_value(cx, &dialog), "XabcY", "Ctrl+E moves to the end");
    f.press(cx, "escape");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(std::fs::read(&path).unwrap(), before, "Esc writes nothing");
}

/// A-019: Pi appends `session_info`, OMP rewrites the title slot; an empty
/// title clears the custom title.
#[gpui_kit::test]
fn rename_writes_each_sources_own_format(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = project_dir(&world, "native");
    let pi = pi_session(&world, &cwd);
    let omp = omp_session(&world, &cwd);
    let claude = SessionSpec::new("proj", 1)
        .cwd(project_dir(&world, "proj"))
        .custom_title("Has a title")
        .write(&world);
    let f = OpsFixture::open(cx, &world, &[]);

    open_rename(cx, &f, &pi);
    cx.simulate_input(f.handle(), "Pi renamed");
    f.press(cx, "enter");
    f.settle(cx);
    let pi_lines = lines(&f.target(cx, &pi).path);
    assert!(
        pi_lines.last().unwrap().contains("session_info"),
        "{pi_lines:?}"
    );
    assert!(pi_lines.last().unwrap().contains("Pi renamed"));

    open_rename(cx, &f, &omp);
    f.press(cx, "ctrl-u");
    cx.simulate_input(f.handle(), "Omp renamed");
    f.press(cx, "enter");
    f.settle(cx);
    let omp_lines = lines(&f.target(cx, &omp).path);
    assert!(
        omp_lines[0].contains("Omp renamed"),
        "the title slot: {}",
        omp_lines[0]
    );

    // An empty title clears the custom title.
    open_rename(cx, &f, &claude);
    f.press(cx, "ctrl-u");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()), "an empty title is allowed");
    let target = f.target(cx, &claude);
    assert_eq!(target.custom_title, None);
}

/// A-020: an unwritable file → inline `Failed to rename: …`, the dialog stays
/// open with the input kept.
#[gpui_kit::test]
fn rename_error_is_inline_and_keeps_the_dialog(cx: &mut TestAppContext) {
    if fault::running_as_root() {
        eprintln!("skipped: running as root, permissions do not apply");
        return;
    }
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    let dialog = open_rename(cx, &f, &path);
    cx.simulate_input(f.handle(), " typed");
    let guard = fault::unwritable(&path);
    f.press(cx, "enter");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "the dialog stays open");
    let error = cx.read(|cx| dialog.read(cx).error().map(str::to_string));
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.starts_with("Failed to rename: ")),
        "{error:?}"
    );
    assert_eq!(input_value(cx, &dialog), " typed", "the input is kept");
    assert!(!cx.read(|cx| dialog.read(cx).is_saving()));
    drop(guard);
    // Retrying after the fault is gone works.
    f.press(cx, "enter");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
}

/// A-024: the OMP rewrite surfaces its error inline too.
#[gpui_kit::test]
fn omp_rename_failure_is_inline(cx: &mut TestAppContext) {
    if fault::running_as_root() {
        eprintln!("skipped: running as root, permissions do not apply");
        return;
    }
    let world = TestWorld::new();
    let omp = omp_session(&world, &project_dir(&world, "native"));
    let f = OpsFixture::open(cx, &world, &[]);
    let dialog = open_rename(cx, &f, &omp);
    cx.simulate_input(f.handle(), "x");
    let real = f.target(cx, &omp).path;
    // The OMP rewrite replaces the file atomically (temp file + rename): the
    // Windows read-only attribute blocks that, a Unix file mode does not (the
    // directory's does).
    let _guard = if cfg!(unix) {
        fault::undeletable(&real)
    } else {
        fault::unwritable(&real)
    };
    f.press(cx, "enter");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    let error = cx.read(|cx| dialog.read(cx).error().map(str::to_string));
    assert!(error.is_some_and(|e| e.starts_with("Failed to rename: ")));
}

/// A-028: the dialog captured its conversation; the list selection moving (and
/// a refresh) before Save changes only the captured conversation.
#[gpui_kit::test]
fn rename_acts_on_the_captured_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let first = claude_session(&world, "proj", 1, "First");
    let second = claude_session(&world, "proj", 2, "Second");
    let second_before = std::fs::read(&second).unwrap();
    let f = OpsFixture::open(cx, &world, &[]);
    open_rename(cx, &f, &first);
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&second, cx))
    );
    f.list_pane().update(cx, |list, cx| list.refresh(cx));
    cx.simulate_input(f.handle(), " renamed");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(lines(&first).iter().any(|line| line.contains("renamed")));
    assert_eq!(std::fs::read(&second).unwrap(), second_before);
}

/// The conversation left the list before Save: inline notice, nothing written.
#[gpui_kit::test]
fn rename_of_a_conversation_that_left_the_list_is_refused(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let before = std::fs::read(&path).unwrap();
    let f = OpsFixture::open(cx, &world, &[]);
    let dialog = open_rename(cx, &f, &path);
    f.list_pane().update(cx, |list, cx| {
        list.remove_paths(std::slice::from_ref(&path), cx)
    });
    cx.simulate_input(f.handle(), "x");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    assert_eq!(
        cx.read(|cx| dialog.read(cx).error().map(str::to_string))
            .as_deref(),
        Some(NOT_IN_LIST)
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// A-017: a second F2 in the viewer pre-fills the title as it is now (the
/// viewer's target predates the first rename); Enter without editing keeps it.
#[gpui_kit::test]
fn a_second_rename_in_the_viewer_prefills_the_new_title(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = project_dir(&world, "proj");
    let path = SessionSpec::new("proj", 1)
        .cwd(cwd)
        .summary("Fix login bug")
        .custom_title("Old title")
        .write(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &path);
    f.press(cx, "f2");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "ctrl-u");
    cx.simulate_input(f.handle(), "New title");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));

    f.press(cx, "f2");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    render(cx, f.handle());
    let dialog = dialog(cx);
    assert_eq!(input_value(cx, &dialog), "New title");
    assert!(
        !cx.read(|cx| dialog.read(cx).target().describe())
            .contains("Old title"),
        "the target line names the new title"
    );
    f.press(cx, "enter");
    f.settle(cx);
    let last = lines(&path)
        .into_iter()
        .rev()
        .find(|l| l.contains("custom-title"));
    assert!(last.is_some_and(|l| l.contains("New title")));
}

/// A-017: a target built from an agent hit has no custom title; the dialog
/// still pre-fills the conversation's current one.
#[gpui_kit::test]
fn a_target_without_a_custom_title_prefills_from_the_corpus(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = project_dir(&world, "proj");
    let path = SessionSpec::new("proj", 1)
        .cwd(cwd)
        .summary("Fix login bug")
        .custom_title("Current title")
        .write(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let mut target = f.target(cx, &path);
    target.custom_title = None;
    cx.update_window(f.handle(), |_, window, cx| {
        crate::ops::rename::open_rename_dialog(target, window, cx)
    })
    .unwrap();
    f.settle(cx);
    render(cx, f.handle());
    assert_eq!(input_value(cx, &dialog(cx)), "Current title");
}

/// The Cancel button releases the dialog's bookkeeping, so a later deletion of
/// the path neither closes an unrelated dialog nor reports a rename dialog.
#[gpui_kit::test]
fn cancel_button_releases_the_registration(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let alpha = claude_session(&world, "proj", 1, "Alpha");
    let beta = claude_session(&world, "proj", 2, "Beta");
    let f = OpsFixture::open(cx, &world, &[]);
    let dialog = open_rename(cx, &f, &alpha);
    cx.update_window(f.handle(), |_, window, cx| {
        dialog.update(cx, |dialog, cx| dialog.cancel(window, cx))
    })
    .unwrap();
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(cx.read(crate::globals::OpenTargets::len), 0);
    assert_eq!(dialog_id(cx), "", "the QA dialog id is cleared");

    // Beta's rename dialog stays open when Alpha is deleted elsewhere.
    open_rename(cx, &f, &beta);
    cx.update_window(f.handle(), |_, _, cx| {
        crate::globals::CorpusBus::broadcast(
            crate::globals::CorpusNotice::Deleted(vec![alpha.clone()]),
            cx,
        )
    })
    .unwrap();
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "an unrelated dialog survives");
}

/// A rename dialog closed by a deletion clears the QA dialog id.
#[gpui_kit::test]
fn a_deletion_closing_the_rename_dialog_clears_the_dialog_id(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let alpha = claude_session(&world, "proj", 1, "Alpha");
    let f = OpsFixture::open(cx, &world, &[]);
    open_rename(cx, &f, &alpha);
    cx.update_window(f.handle(), |_, _, cx| {
        crate::globals::CorpusBus::broadcast(
            crate::globals::CorpusNotice::Deleted(vec![alpha.clone()]),
            cx,
        )
    })
    .unwrap();
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(dialog_id(cx), "");
    assert_eq!(toast(cx), "Closed: \"Alpha\" was deleted");
}

/// A-028: a viewer rename survives the selection moving and a refresh.
#[gpui_kit::test]
fn viewer_rename_survives_selection_move_and_refresh(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let shown = claude_session(&world, "proj", 1, "Shown");
    let other = claude_session(&world, "proj", 2, "Other");
    let other_before = std::fs::read(&other).unwrap();
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &shown);
    f.press(cx, "f2");
    f.settle(cx);
    render(cx, f.handle());
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&other, cx))
    );
    f.list_pane().update(cx, |list, cx| list.refresh(cx));
    cx.simulate_input(f.handle(), " renamed");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(lines(&shown).iter().any(|line| line.contains("renamed")));
    assert_eq!(std::fs::read(&other).unwrap(), other_before);
}
