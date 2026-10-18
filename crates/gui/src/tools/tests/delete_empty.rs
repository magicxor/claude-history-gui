//! G tests of Delete empty transcripts (T-001, T-003…T-007).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use claude_history_core::maintenance::format_delete_empty_report;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::main_window;
use crate::globals::{CorpusBus, CorpusNotice};
use crate::test_support::fixtures_tools::{
    age_file, settle, tree_digest, wait_for, write_empty_transcript,
};
use crate::test_support::{TestWorld, recording, synth};
use crate::tools::common::{Scope, ShownConfirm};
use crate::tools::delete_empty::{DeleteEmptyDialog, ScanState, current_dialog, open_delete_empty};

fn open(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    local: bool,
) -> Entity<DeleteEmptyDialog> {
    cx.update_window(handle, |_, window, cx| open_delete_empty(local, window, cx))
        .unwrap();
    wait_for(cx, "the scan", |cx| {
        current_dialog(cx).is_some_and(|d| d.read(cx).state() == &ScanState::Ready)
    });
    settle(cx, handle);
    cx.read(current_dialog).expect("the dialog is open")
}

/// Two old empty transcripts; the first has a session directory.
fn two_empty(world: &TestWorld) -> (PathBuf, PathBuf, PathBuf) {
    let base = synth::cwd_base();
    let a = write_empty_transcript(world, &base.join("proj"), 11, Some("/status"));
    let b = write_empty_transcript(world, &base.join("api"), 12, None);
    let dir = a.parent().unwrap().join(synth::uuid(11));
    std::fs::create_dir_all(dir.join("subagents")).unwrap();
    std::fs::write(dir.join("subagents").join("x.jsonl"), "{}").unwrap();
    age_file(&a, 2);
    age_file(&b, 5);
    (a, b, dir)
}

fn paths_of(cx: &TestAppContext, dialog: &Entity<DeleteEmptyDialog>) -> Vec<PathBuf> {
    cx.read(|cx| {
        dialog
            .read(cx)
            .candidates()
            .iter()
            .map(|c| c.path().to_path_buf())
            .collect()
    })
}

/// Collect every `CorpusNotice` of the app.
fn record_notices(cx: &mut TestAppContext) -> Rc<RefCell<Vec<CorpusNotice>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|cx| {
        let bus = CorpusBus::entity(cx);
        cx.subscribe(&bus, move |_, notice: &CorpusNotice, _| {
            sink.borrow_mut().push(notice.clone());
        })
        .detach();
    });
    seen
}

/// Open the delete confirmation and return what it shows.
fn open_confirm(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    dialog: &Entity<DeleteEmptyDialog>,
) -> ShownConfirm {
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.request_delete(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    settle(cx, handle);
    cx.read(|cx| dialog.read(cx).shown_confirm().cloned())
        .expect("the delete confirmation is open")
}

fn delete_with_y(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    dialog: &Entity<DeleteEmptyDialog>,
) {
    open_confirm(cx, handle, dialog);
    cx.simulate_keystrokes(handle, "y");
    wait_for(cx, "the deletion to finish", |cx| {
        let d = dialog.read(cx);
        !d.is_deleting() && d.state() == &ScanState::Ready && !d.result_lines().is_empty()
    });
    cx.run_until_parked();
}

/// T-003: rows, the found line and "Copy as text" (CLI report).
#[gpui_kit::test]
fn lists_empty_transcripts_and_copies_the_cli_report(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (a, b, _) = two_empty(&world);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    let paths = paths_of(cx, &dialog);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&a) && paths.contains(&b));
    let (found, text, expected) = cx.read(|cx| {
        let d = dialog.read(cx);
        let transcripts: Vec<_> = d
            .candidates()
            .iter()
            .map(|c| c.transcript.clone())
            .collect();
        (
            d.found_line(),
            d.report_text(),
            format_delete_empty_report(&transcripts, None),
        )
    });
    assert_eq!(found, "Found 2 empty transcript(s).");
    assert_eq!(text, expected);
    assert!(text.contains("(no user messages)") && text.contains("/status"));
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.copy_report(window, cx))
    })
    .unwrap();
    assert_eq!(recording(cx).clipboard(), Some(expected));
}

/// T-001: Current workspace lists only that project's candidates; without a
/// workspace the option is unavailable.
#[gpui_kit::test]
fn the_workspace_scope_lists_only_that_project(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let proj = world.home().join("work").join("proj");
    let api = world.home().join("work").join("api");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&api).unwrap();
    let in_proj = write_empty_transcript(&world, &proj, 21, Some("a"));
    let in_api = write_empty_transcript(&world, &api, 22, Some("b"));
    age_file(&in_proj, 3);
    age_file(&in_api, 3);
    let (_f, handle) = main_window(cx, &world, &["--workspace", &proj.display().to_string()]);
    let dialog = open(cx, handle, false);
    assert_eq!(
        paths_of(cx, &dialog).len(),
        2,
        "All is the default without -L"
    );
    dialog.update(cx, |d, cx| d.set_scope(Scope::Workspace, cx));
    wait_for(cx, "the rescan", |cx| {
        let d = dialog.read(cx);
        d.state() == &ScanState::Ready && d.candidates().len() == 1
    });
    assert_eq!(paths_of(cx, &dialog), vec![in_proj]);
    // `local` preselects the workspace.
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.close(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    let dialog = open(cx, handle, true);
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::Workspace);
}

#[gpui_kit::test]
fn without_a_workspace_the_workspace_scope_is_unavailable(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    two_empty(&world);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, true);
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::All);
    dialog.update(cx, |d, cx| d.set_scope(Scope::Workspace, cx));
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::All);
}

/// T-004 + T-007: confirm with `y` deletes the files and session
/// directories, reports the summary and broadcasts exactly the deleted paths.
#[gpui_kit::test]
fn confirming_deletes_transcripts_and_session_dirs(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (a, b, dir) = two_empty(&world);
    let (_f, handle) = main_window(cx, &world, &[]);
    let notices = record_notices(cx);
    let dialog = open(cx, handle, false);
    delete_with_y(cx, handle, &dialog);
    assert!(!a.exists() && !b.exists(), "both transcripts are gone");
    assert!(!dir.exists(), "the session directory is gone");
    let lines = cx.read(|cx| dialog.read(cx).result_lines().to_vec());
    assert_eq!(lines[0], "Summary: deleted 2 empty transcript(s).");
    let seen = notices.borrow().clone();
    assert_eq!(seen.len(), 1);
    let CorpusNotice::Deleted(mut paths) = seen[0].clone();
    paths.sort();
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(paths, expected);
    assert!(paths_of(cx, &dialog).is_empty(), "the dialog rescanned");
}

/// T-004: a candidate that changed after the scan is skipped and listed; a
/// failing delete is reported.
#[gpui_kit::test]
fn a_candidate_modified_after_the_scan_is_skipped(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (a, b, _) = two_empty(&world);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    // Appending a line changes the size (and so the transcript) after the scan.
    let mut text = std::fs::read_to_string(&b).unwrap();
    text.push_str("{\"type\":\"summary\",\"summary\":\"late\",\"leafUuid\":\"x\"}\n");
    std::fs::write(&b, text).unwrap();
    delete_with_y(cx, handle, &dialog);
    assert!(!a.exists());
    assert!(b.exists(), "the changed candidate is kept");
    let lines = cx.read(|cx| dialog.read(cx).result_lines().to_vec());
    assert_eq!(lines[0], "Summary: deleted 1 empty transcript(s).");
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("Skipped (") && line.contains(&b.display().to_string())),
        "{lines:?}"
    );
}

#[gpui_kit::test]
fn a_failing_delete_reports_the_failure(cx: &mut TestAppContext) {
    if crate::test_support::fault::running_as_root() {
        eprintln!("skipped: permissions do not apply to root");
        return;
    }
    let world = TestWorld::new();
    let (a, _b, _) = two_empty(&world);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    let guard = crate::test_support::fault::undeletable(&a);
    delete_with_y(cx, handle, &dialog);
    drop(guard);
    let lines = cx.read(|cx| dialog.read(cx).result_lines().to_vec());
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("Failed to delete ")),
        "{lines:?}"
    );
    assert!(a.exists(), "the undeletable transcript is still there");
}

/// T-005: a candidate touched now is marked, excluded from `Delete N…` until
/// the box is checked.
#[gpui_kit::test]
fn recent_candidates_are_excluded_until_the_box_is_checked(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_a, _b, _) = two_empty(&world);
    let recent = write_empty_transcript(&world, &synth::cwd_base().join("live"), 13, Some("hello"));
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    let (all, eligible, recent_flags) = cx.read(|cx| {
        let d = dialog.read(cx);
        (
            d.candidates().len(),
            d.eligible().len(),
            d.candidates().iter().filter(|c| d.is_recent(c)).count(),
        )
    });
    assert_eq!((all, eligible, recent_flags), (3, 2, 1));
    dialog.update(cx, |d, cx| d.set_include_recent(true, cx));
    assert_eq!(cx.read(|cx| dialog.read(cx).eligible().len()), 3);
    dialog.update(cx, |d, cx| d.set_include_recent(false, cx));
    delete_with_y(cx, handle, &dialog);
    assert!(
        recent.exists(),
        "the live transcript is never deleted by default"
    );
}

/// T-006: Pi and OMP trees are never touched.
#[gpui_kit::test]
fn pi_and_omp_conversations_are_untouched(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    two_empty(&world);
    world.add_fixture(
        ".pi/agent/sessions/--tmp-pi-project--/2024-03-01T00-00-00-000Z_01912345-6789-7abc-8def-0123456789ab.jsonl",
        synth::PI_SESSION.as_bytes(),
    );
    world.add_fixture(
        ".omp/agent/sessions/--tmp-omp-project--/2026-01-02T03-04-05-000Z_omp_session_custom_id.jsonl",
        synth::OMP_SESSION.as_bytes(),
    );
    let pi = tree_digest(&world.home().join(".pi"));
    let omp = tree_digest(&world.home().join(".omp"));
    assert!(!pi.is_empty() && !omp.is_empty());
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    delete_with_y(cx, handle, &dialog);
    assert_eq!(tree_digest(&world.home().join(".pi")), pi);
    assert_eq!(tree_digest(&world.home().join(".omp")), omp);
}

/// T-007: after deleting, the list refreshed and no longer shows the files.
#[gpui_kit::test]
fn the_list_refreshes_after_a_delete(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (a, b, _) = two_empty(&world);
    let (f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    delete_with_y(cx, handle, &dialog);
    f.wait_loaded(cx);
    let listed = f.paths(cx);
    assert!(!listed.contains(&a) && !listed.contains(&b));
}

/// T-004: the confirmation names the count and lists both transcripts and the
/// session folder about to be removed; nothing is deleted before `y`, `n`
/// cancels without deleting, and the confirmation is gone afterwards.
#[gpui_kit::test]
fn the_confirmation_lists_what_will_be_deleted(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (a, b, dir) = two_empty(&world);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    assert!(cx.read(|cx| dialog.read(cx).shown_confirm().is_none()));
    let shown = open_confirm(cx, handle, &dialog);
    assert_eq!(shown.title, "Delete 2 empty transcripts?");
    let display = |p: &PathBuf| p.display().to_string();
    assert_eq!(shown.paths.len(), 3, "{:?}", shown.paths);
    for expected in [&a, &b] {
        assert!(
            shown.paths.iter().any(|p| p.contains(&display(expected))),
            "{expected:?} in {:?}",
            shown.paths
        );
    }
    assert!(
        shown.paths.iter().any(|p| p.contains(&display(&dir))),
        "the session folder in {:?}",
        shown.paths
    );
    assert!(a.exists() && b.exists() && dir.exists());
    // `n` cancels: nothing is deleted and the confirmation is gone.
    cx.simulate_keystrokes(handle, "n");
    cx.run_until_parked();
    settle(cx, handle);
    assert!(cx.read(|cx| dialog.read(cx).shown_confirm().is_none()));
    assert!(a.exists() && b.exists() && dir.exists());
    // One candidate: singular title.
    std::fs::remove_file(&b).unwrap();
    dialog.update(cx, |d, cx| d.rescan(cx));
    wait_for(cx, "the rescan", |cx| {
        let d = dialog.read(cx);
        d.state() == &ScanState::Ready && d.candidates().len() == 1
    });
    let shown = open_confirm(cx, handle, &dialog);
    assert_eq!(shown.title, "Delete 1 empty transcript?");
    cx.simulate_keystrokes(handle, "y");
    wait_for(cx, "the deletion", |cx| {
        !dialog.read(cx).result_lines().is_empty() && !dialog.read(cx).is_deleting()
    });
    cx.run_until_parked();
    assert!(cx.read(|cx| dialog.read(cx).shown_confirm().is_none()));
}

/// T-003: the table has the §5.10 headers and every row shows the short
/// session id, project, user and line counts, preview and modified time.
#[gpui_kit::test]
fn the_table_has_headers_and_cells(cx: &mut TestAppContext) {
    use crate::tools::delete_empty::COLUMNS;
    let world = TestWorld::new();
    let (a, _b, _) = two_empty(&world);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    assert_eq!(
        COLUMNS,
        [
            "Session", "Project", "Users", "Lines", "Preview", "Modified"
        ]
    );
    let ix = paths_of(cx, &dialog)
        .iter()
        .position(|path| *path == a)
        .expect("the first transcript is listed");
    let cells = cx
        .read(|cx| dialog.read(cx).row_cells(ix))
        .expect("the row exists");
    assert_eq!(cells[0], crate::model::short_session_id(&synth::uuid(11)));
    assert_eq!(
        cells[1],
        synth::cwd_base().join("proj").display().to_string()
    );
    assert_eq!(cells[2], "1", "one user message");
    assert_eq!(cells[3], "1", "one line");
    assert_eq!(cells[4], "/status");
    assert!(!cells[5].is_empty(), "a modified time");
    assert!(cx.read(|cx| dialog.read(cx).row_cells(99)).is_none());
}

/// T-003: with hundreds of candidates only the visible rows are built, and the
/// keyboard reaches every row (Up/Down/Home/End/PageDown) and shows its path.
#[gpui_kit::test]
fn a_large_table_is_virtualized_and_keyboard_reachable(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let base = synth::cwd_base().join("many");
    for n in 0..300 {
        let path = write_empty_transcript(&world, &base, 1000 + n, Some("hello"));
        age_file(&path, 2);
    }
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, false);
    assert_eq!(paths_of(cx, &dialog).len(), 300);
    let built = cx.read(|cx| dialog.read(cx).rendered_rows());
    assert!(
        !built.is_empty() && built.len() < 40,
        "only the visible rows are built, not all 300: {built:?}"
    );
    assert_eq!(cx.read(|cx| dialog.read(cx).selected()), None);
    let selected = |cx: &TestAppContext| cx.read(|cx| dialog.read(cx).selected());
    cx.simulate_keystrokes(handle, "down");
    assert_eq!(selected(cx), Some(0));
    cx.simulate_keystrokes(handle, "down down");
    assert_eq!(selected(cx), Some(2));
    cx.simulate_keystrokes(handle, "up");
    assert_eq!(selected(cx), Some(1));
    cx.simulate_keystrokes(handle, "pagedown");
    assert_eq!(selected(cx), Some(10));
    cx.simulate_keystrokes(handle, "end");
    assert_eq!(selected(cx), Some(299));
    cx.simulate_keystrokes(handle, "down");
    assert_eq!(selected(cx), Some(299), "stays on the last row");
    let expected = paths_of(cx, &dialog)[299].display().to_string();
    assert_eq!(
        cx.read(|cx| dialog.read(cx).selected_path()),
        Some(expected),
        "the full path of the keyboard-selected row"
    );
    settle(cx, handle);
    let built = cx.read(|cx| dialog.read(cx).rendered_rows());
    assert!(
        built.contains(&299),
        "the selection is scrolled into view: {built:?}"
    );
    assert!(built.len() < 40);
    cx.simulate_keystrokes(handle, "home");
    assert_eq!(selected(cx), Some(0));
}
