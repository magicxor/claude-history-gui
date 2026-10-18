//! G tests of Delete conversation by session ID (T-017, T-018, C-013).

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::main_window;
use crate::globals::{CorpusBus, CorpusNotice};
use crate::model::short_session_id;
use crate::test_support::fixtures_tools::{settle, two_copy_session, wait_for};
use crate::test_support::{TestWorld, synth};
use crate::tools::delete_by_id::{
    DeleteByIdDialog, NOT_REMOVED_NOTE, current_dialog, open_delete_by_id,
};

fn open(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    prefill: Option<&str>,
) -> Entity<DeleteByIdDialog> {
    let prefill = prefill.map(str::to_string);
    cx.update_window(handle, |_, window, cx| {
        open_delete_by_id(prefill, window, cx)
    })
    .unwrap();
    settle(cx, handle);
    cx.read(current_dialog).expect("the dialog is open")
}

/// T-017: an invalid id reads `Session not found: <id>`.
#[gpui_kit::test]
fn an_invalid_id_is_not_found(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, Some("bad/id"));
    wait_for(cx, "Find", |cx| dialog.read(cx).error().is_some());
    assert_eq!(
        cx.read(|cx| dialog.read(cx).error().map(str::to_string)),
        Some("Session not found: bad/id".to_string())
    );
    assert!(
        !cx.read(|cx| dialog.read(cx).can_delete(cx)),
        "Delete… stays disabled"
    );
}

#[gpui_kit::test]
fn an_unknown_id_is_not_found(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let id = synth::uuid(999);
    let dialog = open(cx, handle, Some(&id));
    wait_for(cx, "Find", |cx| dialog.read(cx).error().is_some());
    assert_eq!(
        cx.read(|cx| dialog.read(cx).error().map(str::to_string)),
        Some(format!("Session not found: {id}"))
    );
}

/// T-017 / C-013: a two-copy id lists both copies, nothing is deleted before
/// the confirmation, and `y` deletes both copies and the session folder.
#[gpui_kit::test]
fn deleting_a_two_copy_session(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (first, second, session_dir) = two_copy_session(&world, 41);
    let id = synth::uuid(41);
    let (_f, handle) = main_window(cx, &world, &[]);
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let sink = seen.clone();
        cx.update(|cx| {
            let bus = CorpusBus::entity(cx);
            cx.subscribe(&bus, move |_, notice: &CorpusNotice, _| {
                sink.borrow_mut().push(notice.clone())
            })
            .detach();
        });
    }
    let dialog = open(cx, handle, Some(&id));
    wait_for(cx, "Find", |cx| !dialog.read(cx).copies().is_empty());
    assert_eq!(cx.read(|cx| dialog.read(cx).copies().len()), 2);
    assert!(cx.read(|cx| dialog.read(cx).can_delete(cx)));
    assert!(
        first.exists() && second.exists(),
        "nothing is deleted before the confirmation"
    );

    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.request_delete(window, cx))
    })
    .unwrap();
    // The plan runs on a worker: `y` must not go to the ID field before the
    // confirmation is open.
    wait_for(cx, "the delete plan", |cx| !dialog.read(cx).is_busy());
    cx.run_until_parked();
    settle(cx, handle);
    assert!(
        first.exists() && second.exists(),
        "still nothing before `y`"
    );
    // The confirmation names the shortened id and lists both transcripts and
    // the session folder.
    let shown = cx
        .read(|cx| dialog.read(cx).shown_confirm().cloned())
        .expect("the confirmation is open");
    assert_eq!(
        shown.title,
        format!("Delete session {}?", short_session_id(&id))
    );
    assert_eq!(shown.paths.len(), 3, "{:?}", shown.paths);
    for path in [&first, &second, &session_dir] {
        let text = path.display().to_string();
        assert!(
            shown.paths.iter().any(|p| p.contains(&text)),
            "{text} in {:?}",
            shown.paths
        );
    }
    cx.simulate_keystrokes(handle, "y");
    wait_for(cx, "the deletion", |cx| dialog.read(cx).result().is_some());
    cx.run_until_parked();
    let expected = format!("Deleted session {id} (2 copies across projects)");
    assert_eq!(
        cx.read(|cx| dialog.read(cx).result().map(str::to_string)),
        Some(expected.clone())
    );
    assert!(!first.exists() && !second.exists() && !session_dir.exists());
    assert!(
        cx.read(|cx| dialog.read(cx).shown_confirm().is_none()),
        "the confirmation is gone"
    );
    let notices = seen.borrow().clone();
    let CorpusNotice::Deleted(mut paths) = notices.last().cloned().expect("a notice");
    paths.sort();
    let mut want = vec![first, second];
    want.sort();
    assert_eq!(paths, want);
}

/// A-032 / T-017: a failure after the transcripts were handled (one removed,
/// one vanished meanwhile) still announces both as deleted.
#[gpui_kit::test]
fn a_partial_failure_announces_removed_and_vanished_copies(cx: &mut TestAppContext) {
    if crate::test_support::fault::running_as_root() {
        eprintln!("skipped: permissions do not apply to root");
        return;
    }
    let world = TestWorld::new();
    let (first, second, session_dir) = two_copy_session(&world, 43);
    let id = synth::uuid(43);
    let (_f, handle) = main_window(cx, &world, &[]);
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let sink = seen.clone();
        cx.update(|cx| {
            let bus = CorpusBus::entity(cx);
            cx.subscribe(&bus, move |_, notice: &CorpusNotice, _| {
                sink.borrow_mut().push(notice.clone())
            })
            .detach();
        });
    }
    let dialog = open(cx, handle, Some(&id));
    wait_for(cx, "Find", |cx| !dialog.read(cx).copies().is_empty());
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.request_delete(window, cx))
    })
    .unwrap();
    wait_for(cx, "the delete plan", |cx| !dialog.read(cx).is_busy());
    cx.run_until_parked();
    settle(cx, handle);
    std::fs::remove_file(&second).unwrap();
    // The session folder cannot be removed: the failure follows both files.
    let guard =
        crate::test_support::fault::undeletable(&session_dir.join("tool-results").join("r.txt"));
    cx.simulate_keystrokes(handle, "y");
    wait_for(cx, "the notice", |_| !seen.borrow().is_empty());
    cx.run_until_parked();
    let CorpusNotice::Deleted(mut paths) = seen.borrow().last().cloned().expect("a notice");
    paths.sort();
    let mut want = vec![first.clone(), second];
    want.sort();
    assert_eq!(paths, want);
    assert!(!first.exists() && session_dir.exists());
    assert!(cx.read(|cx| dialog.read(cx).result().is_none()));
    drop(guard);
}

/// A single-copy session reads `Deleted session <id>`.
#[gpui_kit::test]
fn deleting_a_single_copy_session(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = synth::cwd_base().join("solo");
    let id = synth::uuid(42);
    let lines = synth::claude_conversation(&id, &cwd, "Solo", 2, 1);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let path = world.add_claude_session(&synth::encoded_project_dir(&cwd), &id, &refs);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, Some(&id));
    wait_for(cx, "Find", |cx| !dialog.read(cx).copies().is_empty());
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.request_delete(window, cx))
    })
    .unwrap();
    // The plan runs on a worker: `y` must not go to the ID field before the
    // confirmation is open.
    wait_for(cx, "the delete plan", |cx| !dialog.read(cx).is_busy());
    cx.run_until_parked();
    settle(cx, handle);
    cx.simulate_keystrokes(handle, "y");
    wait_for(cx, "the deletion", |cx| dialog.read(cx).result().is_some());
    assert_eq!(
        cx.read(|cx| dialog.read(cx).result().map(str::to_string)),
        Some(format!("Deleted session {id}"))
    );
    assert!(!path.exists());
}

/// Editing the id after Find disables Delete… again.
#[gpui_kit::test]
fn editing_the_id_after_find_disables_delete(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_a, _b, _d) = two_copy_session(&world, 43);
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, Some(&synth::uuid(43)));
    wait_for(cx, "Find", |cx| !dialog.read(cx).copies().is_empty());
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.set_id("other-id", window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!cx.read(|cx| dialog.read(cx).can_delete(cx)));
}

/// T-018: the dialog's note about what is not removed.
#[test]
fn the_dialog_names_what_is_not_removed() {
    assert_eq!(
        NOT_REMOVED_NOTE,
        "Not removed: file-history, todos, session-env and other global data."
    );
}
