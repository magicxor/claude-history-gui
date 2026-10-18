//! G tests of delete (L-037, A-025…A-033, X-003, X-014, X-030). Deletions only
//! ever touch files inside the test world's own sandbox.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use claude_history_core::history::Source;
use claude_history_core::maintenance::{execute_session_delete, plan_session_delete};
use gpui_kit::{AppContext as _, TestAppContext};

use super::{dialog_id, has_dialog, render, toast};
use crate::ops::delete::{self, DIALOG_ID};
use crate::test_support::fault;
use crate::test_support::fixtures_list::{Gate, ListFixture, SessionSpec, install_fake_embedder};
use crate::test_support::fixtures_ops::{
    OpsFixture, claude_session, omp_session, pi_session, project_dir, two_copies,
};
use crate::test_support::{TestWorld, synth};

/// Every file below the world's data folders (Claude, Pi, OMP).
fn snapshot(world: &TestWorld) -> BTreeSet<PathBuf> {
    fn walk(dir: &Path, out: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path);
            }
        }
    }
    let mut out = BTreeSet::new();
    for sub in [".claude", ".pi", ".omp"] {
        walk(&world.home().join(sub), &mut out);
    }
    out
}

fn open_delete(cx: &mut TestAppContext, f: &OpsFixture, path: &Path) {
    f.select(cx, path);
    f.press(cx, "ctrl-x");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "Ctrl+X opens the confirmation");
    assert_eq!(dialog_id(cx), DIALOG_ID);
    render(cx, f.handle());
}

/// L-037, A-025: Ctrl+X opens `Delete "<title>"?`; Enter leaves the file and
/// the dialog; Shift+Y deletes; `n`, Shift+N and Esc cancel.
#[gpui_kit::test]
fn delete_confirmation_keys(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let doomed = claude_session(&world, "proj", 1, "Fix login bug");
    let kept = claude_session(&world, "proj", 2, "Keep me");
    let f = OpsFixture::open(cx, &world, &[]);

    // Cancel with each cancelling key.
    for keys in ["n", "shift-n", "escape"] {
        open_delete(cx, &f, &kept);
        f.press(cx, keys);
        f.settle(cx);
        assert!(!has_dialog(cx, f.handle()), "{keys} cancels");
        assert!(kept.is_file(), "{keys} deletes nothing");
    }

    open_delete(cx, &f, &doomed);
    let target = f.target(cx, &doomed);
    assert_eq!(delete::dialog_title(&target), "Delete \"Fix login bug\"?");
    f.press(cx, "enter");
    f.settle(cx);
    assert!(doomed.is_file(), "Enter never deletes");
    assert!(has_dialog(cx, f.handle()), "Enter leaves the dialog open");
    f.press(cx, "shift-y");
    f.settle(cx);
    assert!(!doomed.exists(), "Y deletes");
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(toast(cx), "Deleted \"Fix login bug\"");
    assert!(!f.list.paths(cx).contains(&doomed), "the row left the list");
    assert!(kept.is_file());
}

/// A-027, X-003: the plan names every transcript and folder and the number of
/// copies; the title names the target.
#[gpui_kit::test]
fn delete_plan_lists_every_copy_and_folder(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let copies = two_copies(&world, 1);
    let pi = pi_session(&world, &project_dir(&world, "native"));
    let omp = omp_session(&world, &project_dir(&world, "native"));
    let omp_dir = omp.with_extension("");
    std::fs::create_dir_all(&omp_dir).unwrap();
    std::fs::write(omp_dir.join("artifact.txt"), "a").unwrap();
    let f = OpsFixture::open(cx, &world, &[]);

    let env = cx.read(|cx| crate::globals::app_state(cx).env_with_workspace());
    let plan = plan_session_delete(&env, &copies.first, Source::Claude).unwrap();
    let items = delete::plan_items(&plan);
    let paths: Vec<PathBuf> = items.iter().map(|item| item.path.clone()).collect();
    for expected in [
        &copies.first,
        &copies.second,
        &copies.first_dir,
        &copies.second_dir,
    ] {
        assert!(paths.contains(expected), "{expected:?} in {paths:?}");
    }
    assert_eq!(
        delete::consequence_text(&plan),
        "2 copies across projects. This can't be undone."
    );
    let target = f.target(cx, &copies.first);
    assert!(delete::dialog_title(&target).contains("Copy in a"));

    // The dialog opens for the two-copy conversation.
    open_delete(cx, &f, &copies.first);
    f.press(cx, "n");
    f.settle(cx);

    let pi_plan = plan_session_delete(&env, &f.target(cx, &pi).path, Source::Pi).unwrap();
    assert_eq!(delete::plan_items(&pi_plan).len(), 1);
    assert_eq!(delete::consequence_text(&pi_plan), "This can't be undone.");
    let omp_plan = plan_session_delete(&env, &f.target(cx, &omp).path, Source::Omp).unwrap();
    assert_eq!(
        delete::plan_items(&omp_plan).len(),
        2,
        "the file and its folder"
    );
}

/// A-026, A-032: Claude copies + session folders, a Pi file and an OMP file +
/// folder: exactly the plan's items are removed, nothing else changes; every
/// copy leaves the list.
#[gpui_kit::test]
fn delete_removes_exactly_the_planned_items(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let copies = two_copies(&world, 1);
    let pi = pi_session(&world, &project_dir(&world, "native"));
    let omp = omp_session(&world, &project_dir(&world, "native"));
    let omp_dir = omp.with_extension("");
    std::fs::create_dir_all(&omp_dir).unwrap();
    std::fs::write(omp_dir.join("artifact.txt"), "a").unwrap();
    let bystander = claude_session(&world, "bystander", 9, "Bystander");
    let f = OpsFixture::open(cx, &world, &[]);
    assert!(f.list.paths(cx).contains(&copies.first) && f.list.paths(cx).contains(&copies.second));

    let before = snapshot(&world);
    open_delete(cx, &f, &copies.first);
    f.press(cx, "y");
    f.settle(cx);
    let after = snapshot(&world);
    let removed: BTreeSet<PathBuf> = before.difference(&after).cloned().collect();
    let expected: BTreeSet<PathBuf> = [
        copies.first.clone(),
        copies.second.clone(),
        copies.first_dir.join("tool-results").join("result.txt"),
        copies.second_dir.join("tool-results").join("result.txt"),
    ]
    .into_iter()
    .collect();
    assert_eq!(removed, expected);
    assert!(
        after.difference(&before).next().is_none(),
        "nothing was added"
    );
    assert!(!copies.first_dir.exists() && !copies.second_dir.exists());
    let listed = f.list.paths(cx);
    assert!(
        !listed.contains(&copies.first) && !listed.contains(&copies.second),
        "A-032: both copies left the list: {listed:?}"
    );
    assert_eq!(
        toast(cx),
        format!("Deleted session {} (2 copies across projects)", copies.uuid)
    );

    // Pi: the one file.
    let before = snapshot(&world);
    open_delete(cx, &f, &pi);
    f.press(cx, "y");
    f.settle(cx);
    let after = snapshot(&world);
    let removed: Vec<PathBuf> = before.difference(&after).cloned().collect();
    assert_eq!(removed.len(), 1, "{removed:?}");

    // OMP: the file and its artifacts folder.
    let before = snapshot(&world);
    open_delete(cx, &f, &omp);
    f.press(cx, "y");
    f.settle(cx);
    let after = snapshot(&world);
    assert_eq!(before.difference(&after).count(), 2);
    assert!(!omp_dir.exists());
    assert!(bystander.is_file(), "other conversations are untouched");
}

/// A-029, A-028: the source comes from the captured target: a Pi target whose
/// list selection moved to a Claude row deletes only the Pi file.
#[gpui_kit::test]
fn delete_uses_the_captured_target_and_source(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let pi = pi_session(&world, &project_dir(&world, "native"));
    let claude = claude_session(&world, "proj", 1, "A Claude row");
    let f = OpsFixture::open(cx, &world, &[]);
    open_delete(cx, &f, &pi);
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&claude, cx))
    );
    f.list_pane().update(cx, |list, cx| list.refresh(cx));
    f.press(cx, "y");
    f.settle(cx);
    assert!(claude.is_file(), "the Claude row is untouched");
    assert!(!f.target(cx, &claude).path.as_os_str().is_empty());
    assert!(
        !f.list
            .paths(cx)
            .iter()
            .any(|p| p.to_string_lossy().contains("--ops--"))
    );
}

/// A-030: deleting from the viewer clears it, moves the focus to the list and
/// keeps the selected position (the next row).
#[gpui_kit::test]
fn delete_from_the_viewer_clears_it_and_keeps_the_position(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let now = chrono::Utc::now();
    let mut paths = Vec::new();
    for n in 1..=5u32 {
        paths.push(
            SessionSpec::new("proj", n)
                .cwd(project_dir(&world, "proj"))
                .summary(&format!("Row {n}"))
                .start(now - chrono::Duration::hours(i64::from(n)))
                .write(&world),
        );
    }
    let f = OpsFixture::open(cx, &world, &[]);
    let order = f.list.paths(cx);
    let third = order[2].clone();
    f.select(cx, &third);
    let viewer = f.open_in_viewer(cx, &third);
    f.press(cx, "ctrl-x");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "Ctrl+X in the viewer opens it");
    render(cx, f.handle());
    f.press(cx, "y");
    f.settle(cx);
    assert!(!third.exists());
    assert!(
        cx.read(|cx| viewer.read(cx).target().is_none()),
        "the viewer is cleared"
    );
    let in_list = cx
        .update_window(f.handle(), |_, window, cx| {
            f.list_pane().read(cx).contains_focus(window, cx)
        })
        .unwrap();
    assert!(in_list, "focus moved to the list");
    assert_eq!(
        f.list.selected_position(cx),
        Some(2),
        "the next row took the position"
    );
    assert_eq!(f.list.paths(cx).len(), 4);
    let _ = paths;
}

/// A-031, X-030: a failing delete toasts the error and appends exactly one
/// `Failed to delete session <path>: …` line to debug.log; the row stays.
#[gpui_kit::test]
fn failed_delete_toasts_and_logs_once(cx: &mut TestAppContext) {
    if fault::running_as_root() {
        eprintln!("skipped: permissions do not apply to root");
        return;
    }
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let log = world.env.debug_log_file().expect("a debug log path");
    let f = OpsFixture::open(cx, &world, &[]);
    open_delete(cx, &f, &path);
    let guard = fault::undeletable(&path);
    f.press(cx, "y");
    f.settle(cx);
    let text = toast(cx);
    assert!(
        text.starts_with(&format!("Failed to delete session {}: ", path.display())),
        "{text}"
    );
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    let count = logged
        .lines()
        .filter(|line| line.contains("Failed to delete session "))
        .count();
    assert_eq!(count, 1, "{logged}");
    assert!(
        f.list.paths(cx).contains(&path),
        "nothing was removed from the list"
    );
    assert!(!has_dialog(cx, f.handle()));
    drop(guard);
}

/// A-033: deleting while a lexical search is pending neither panics nor leaves
/// the deleted path in the results.
#[gpui_kit::test]
fn delete_during_a_lexical_search(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 12);
    let f = OpsFixture::open(cx, &world, &[]);
    f.list.type_text(cx, "synthetic");
    let victim = f.list.selected_path(cx).expect("a selected row");
    f.press(cx, "ctrl-x");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    render(cx, f.handle());
    f.press(cx, "y");
    f.list.wait_idle(cx);
    f.settle(cx);
    assert!(!victim.exists());
    assert!(!f.list.paths(cx).contains(&victim));
}

/// X-014: deleting while a semantic search is pending: no panic, and the
/// completed results contain no deleted path.
#[gpui_kit::test]
fn delete_during_a_semantic_search(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 12);
    ListFixture::init_app(cx, &world, &[]);
    let control = install_fake_embedder(cx);
    let gate = Gate::closed();
    *control.batch_gate.lock().unwrap() = Some(gate.clone());
    let f = OpsFixture::open(cx, &world, &[]);
    f.press(cx, "ctrl-t");
    f.list.type_text(cx, "synthetic");
    let victim = f.list.selected_path(cx).expect("a selected row");
    f.press(cx, "ctrl-x");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    render(cx, f.handle());
    f.press(cx, "y");
    f.settle(cx);
    gate.open();
    f.list.wait_idle(cx);
    f.settle(cx);
    assert!(!victim.exists());
    assert!(!f.list.paths(cx).contains(&victim));
}

/// PD15: another route deleting the path closes the confirmation (the app's
/// `CorpusBus` handler) with the documented toast.
#[gpui_kit::test]
fn a_deletion_elsewhere_closes_the_confirmation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    open_delete(cx, &f, &path);
    cx.update_window(f.handle(), |_, _, cx| {
        crate::globals::CorpusBus::broadcast(
            crate::globals::CorpusNotice::Deleted(vec![path.clone()]),
            cx,
        )
    })
    .unwrap();
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(toast(cx), "Closed: \"Fix login bug\" was deleted");
    assert!(path.is_file(), "the broadcast alone deletes nothing");
}

/// Core refuses a tampered plan before removing anything (K4): the GUI only
/// ever executes plans `plan_session_delete` produced.
#[gpui_kit::test]
fn tampered_plans_are_refused(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let victim = world.add_fixture("outside/precious.jsonl", b"keep\n");
    let f = OpsFixture::open(cx, &world, &[]);
    let _ = f;
    let env = world.env.clone();
    let mut plan = plan_session_delete(&env, &path, Source::Claude).unwrap();
    plan.files.push(victim.clone());
    let failure = execute_session_delete(&env, &plan).unwrap_err();
    assert!(failure.partial.removed_files.is_empty());
    assert!(victim.is_file() && path.is_file());
}

/// A-032: when a later item fails, the transcripts core already removed leave
/// the list (core returns the partial report with the error).
#[gpui_kit::test]
fn a_partial_failure_still_removes_the_deleted_copies_from_the_list(cx: &mut TestAppContext) {
    if fault::running_as_root() {
        eprintln!("skipped: permissions do not apply to root");
        return;
    }
    let world = TestWorld::new();
    let copies = two_copies(&world, 1);
    let f = OpsFixture::open(cx, &world, &[]);
    open_delete(cx, &f, &copies.first);
    let guard = fault::undeletable(&copies.second_dir.join("tool-results").join("result.txt"));
    f.press(cx, "y");
    f.settle(cx);
    assert!(
        toast(cx).starts_with("Failed to delete session "),
        "{}",
        toast(cx)
    );
    let listed = f.list.paths(cx);
    for path in [&copies.first, &copies.second] {
        if !path.exists() {
            assert!(!listed.contains(path), "{path:?} is gone but still listed");
        }
    }
    assert!(
        !copies.first.exists() || !copies.second.exists(),
        "the failure came after at least one removal"
    );
    drop(guard);
}

/// A-032: a copy that vanished after the plan was made counts as gone, so a
/// later failure still removes both rows from the list (core reports it in
/// `missing`, not in `removed_files`).
#[gpui_kit::test]
fn a_vanished_copy_leaves_the_list_with_a_later_failure(cx: &mut TestAppContext) {
    if fault::running_as_root() {
        eprintln!("skipped: permissions do not apply to root");
        return;
    }
    let world = TestWorld::new();
    let copies = two_copies(&world, 1);
    let f = OpsFixture::open(cx, &world, &[]);
    open_delete(cx, &f, &copies.first);
    std::fs::remove_file(&copies.second).unwrap();
    // The failure comes in the directories loop, after both files.
    let guard = fault::undeletable(&copies.second_dir.join("tool-results").join("result.txt"));
    f.press(cx, "y");
    f.settle(cx);
    assert!(
        toast(cx).starts_with("Failed to delete session "),
        "{}",
        toast(cx)
    );
    assert!(!copies.first.exists() && copies.second_dir.exists());
    let listed = f.list.paths(cx);
    assert!(!listed.contains(&copies.first), "removed by the run");
    assert!(!listed.contains(&copies.second), "vanished meanwhile");
    drop(guard);
}

/// X-003: the confirmation names the conversation as it is now (the viewer's
/// captured target predates a rename).
#[gpui_kit::test]
fn delete_from_the_viewer_titles_the_renamed_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = SessionSpec::new("proj", 1)
        .cwd(project_dir(&world, "proj"))
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
    f.press(cx, "ctrl-x");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "y");
    f.settle(cx);
    assert_eq!(toast(cx), "Deleted \"New title\"");
}

/// A-028: delete opened from the viewer, then the list selection moves and a
/// refresh runs before the confirmation: only the captured file is deleted.
#[gpui_kit::test]
fn delete_from_the_viewer_ignores_a_moved_selection(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let shown = claude_session(&world, "proj", 1, "Shown");
    let other = claude_session(&world, "proj", 2, "Other");
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &shown);
    f.press(cx, "ctrl-x");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    render(cx, f.handle());
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&other, cx))
    );
    f.list_pane().update(cx, |list, cx| list.refresh(cx));
    f.press(cx, "y");
    f.settle(cx);
    assert!(!shown.exists());
    assert!(other.is_file(), "the selected row is untouched");
}

/// A-033: a deletion that lands while a lexical search is in flight neither
/// panics nor leaves the deleted path in the results. The keystroke is
/// dispatched without running the executor, so the search is pending (asserted)
/// when the deletion is broadcast.
#[gpui_kit::test]
fn a_deletion_during_a_pending_lexical_search(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 12);
    let f = OpsFixture::open(cx, &world, &[]);
    let victim = f.list.selected_path(cx).expect("a selected row");
    f.focus_list(cx);
    cx.update_window(f.handle(), |_, window, cx| {
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("s").unwrap(), cx)
    })
    .unwrap();
    let pending = f
        .list
        .read(cx, |list, _| list.browser().has_search_work_in_flight());
    assert!(pending, "the search is in flight when the deletion lands");
    std::fs::remove_file(&victim).unwrap();
    cx.update_window(f.handle(), |_, _, cx| {
        crate::globals::CorpusBus::broadcast(
            crate::globals::CorpusNotice::Deleted(vec![victim.clone()]),
            cx,
        )
    })
    .unwrap();
    f.list.wait_idle(cx);
    f.settle(cx);
    assert!(!f.list.paths(cx).contains(&victim));
}
