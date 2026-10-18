//! G tests of copy path / session ID / message, show path and reveal
//! (V-083, A-039…A-045).

use claude_history_core::export::{
    MESSAGE_COPIED_TO_CLIPBOARD, MESSAGE_NOT_FOUND, NO_TEXT_CONTENT, PATH_COPIED_TO_CLIPBOARD,
    SESSION_ID_COPIED_TO_CLIPBOARD,
};
use gpui_kit::{AppContext as _, TestAppContext};

use super::toast;
use crate::actions::{CopyHeaderSessionId, RevealInFileManager};
use crate::services::ServiceCall;
use crate::test_support::TestWorld;
use crate::test_support::fixtures_list::SessionSpec;
use crate::test_support::fixtures_ops::{
    OpsFixture, claude_session, pi_session, project_dir, tool_session,
};

fn labels(cx: &TestAppContext, target: &crate::model::ConversationTarget) -> Vec<String> {
    cx.read(|cx| crate::list::context_menu::build(0, target, false, cx).labels())
}

/// Whether the app menu's Session menu lists Copy header session ID now.
fn session_menu_lists_header_id(cx: &TestAppContext) -> bool {
    cx.read(|cx| {
        crate::menus::menu_tree(cx)
            .iter()
            .filter(|menu| menu.name.as_ref() == "Session")
            .flat_map(|menu| menu.items.iter())
            .any(|item| {
                matches!(item, gpui_kit::MenuItem::Action { name, .. }
                    if name.as_ref() == "Copy header session ID")
            })
    })
}

/// V-083, A-041, A-042, A-044: `Y`, `I` and the header ID each write the
/// clipboard once, with the documented toast and no other side effect.
#[gpui_kit::test]
fn copy_path_session_id_and_header_id(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let claude = claude_session(&world, "proj", 1, "Fix login bug");
    let pi = pi_session(&world, &project_dir(&world, "native"));
    let f = OpsFixture::open(cx, &world, &[]);

    let claude_target = f.target(cx, &claude);
    assert!(
        !claude_target.header_id_differs(),
        "the item is hidden for Claude"
    );
    // The row menu lists the item for Pi only.
    assert!(!labels(cx, &claude_target).contains(&"Copy header session ID".to_string()));
    f.open_in_viewer(cx, &claude);
    // The app menu's Session item follows the active conversation (A-042).
    assert!(!session_menu_lists_header_id(cx));
    f.recording.clear_calls();
    f.press(cx, "shift-y");
    assert_eq!(
        f.clipboard_writes(),
        vec![claude_target.path.display().to_string()]
    );
    assert_eq!(
        f.recording.calls().len(),
        1,
        "A-044: exactly one clipboard write"
    );
    assert_eq!(toast(cx), PATH_COPIED_TO_CLIPBOARD);

    f.recording.clear_calls();
    f.press(cx, "shift-i");
    assert_eq!(f.clipboard_writes(), vec![claude_target.file_stem.clone()]);
    assert_eq!(f.recording.calls().len(), 1);
    assert_eq!(toast(cx), SESSION_ID_COPIED_TO_CLIPBOARD);

    // Pi: `I` copies the file stem (quirk 6), the menu item the header id.
    let pi_target = f.target(cx, &pi);
    assert!(pi_target.header_id_differs());
    assert!(labels(cx, &pi_target).contains(&"Copy header session ID".to_string()));
    f.open_in_viewer(cx, &pi);
    assert!(session_menu_lists_header_id(cx));
    f.recording.clear_calls();
    f.press(cx, "shift-i");
    assert_eq!(f.clipboard_writes(), vec![pi_target.file_stem.clone()]);
    f.recording.clear_calls();
    cx.dispatch_action(f.handle(), CopyHeaderSessionId);
    f.settle(cx);
    assert_eq!(
        f.clipboard_writes(),
        vec!["01912345-6789-7abc-8def-0123456789ab".to_string()]
    );
    assert_eq!(f.recording.calls().len(), 1);
    assert_eq!(toast(cx), SESSION_ID_COPIED_TO_CLIPBOARD);
}

/// A-039: with navigation active `y` copies the focused message's raw text.
#[gpui_kit::test]
fn copy_the_focused_message(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = SessionSpec::new("proj", 1)
        .cwd(project_dir(&world, "proj"))
        .messages(&[
            ("user", "first **question**"),
            ("assistant", "the *answer*"),
        ])
        .write(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &path);
    f.press(cx, "shift-j");
    f.recording.clear_calls();
    f.press(cx, "y");
    f.settle(cx);
    let writes = f.clipboard_writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(
        writes[0] == "first **question**" || writes[0] == "the *answer*",
        "the raw markdown of the focused message: {writes:?}"
    );
    assert_eq!(toast(cx), MESSAGE_COPIED_TO_CLIPBOARD);
    f.press(cx, "shift-k");
    f.press(cx, "y");
    f.settle(cx);
    let writes = f.clipboard_writes();
    assert_eq!(writes.len(), 2);
    assert_ne!(writes[0], writes[1], "the neighbouring message");
}

/// A-040: a summary group (tool calls in Summary mode) has no text: the toast
/// says so and the clipboard keeps its content.
#[gpui_kit::test]
fn a_summary_group_copies_nothing(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = tool_session(&world, "proj", 1);
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &path);
    cx.dispatch_action(f.handle(), crate::actions::SetToolsSummary);
    cx.run_until_parked();
    let mut found = false;
    for _ in 0..8 {
        f.press(cx, "shift-j");
        let before = f.recording.clipboard();
        f.press(cx, "y");
        f.settle(cx);
        if toast(cx) == NO_TEXT_CONTENT {
            assert_eq!(
                f.recording.clipboard(),
                before,
                "the clipboard is unchanged"
            );
            found = true;
            break;
        }
    }
    assert!(found, "a summary group was focused at some step");
}

/// The copy-message entry point's three outcomes.
#[gpui_kit::test]
fn copy_message_outcomes(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    let target = f.target(cx, &path);
    cx.update_window(f.handle(), |_, window, cx| {
        crate::ops::copy::copy_message(&target, Ok("text".to_string()), window, cx)
    })
    .unwrap();
    assert_eq!(toast(cx), MESSAGE_COPIED_TO_CLIPBOARD);
    assert_eq!(f.recording.clipboard().as_deref(), Some("text"));
    cx.update_window(f.handle(), |_, window, cx| {
        crate::ops::copy::copy_message(&target, Ok(String::new()), window, cx)
    })
    .unwrap();
    assert_eq!(toast(cx), NO_TEXT_CONTENT);
    assert_eq!(
        f.recording.clipboard().as_deref(),
        Some("text"),
        "unchanged"
    );
    cx.update_window(f.handle(), |_, window, cx| {
        crate::ops::copy::copy_message(&target, Err(MESSAGE_NOT_FOUND.to_string()), window, cx)
    })
    .unwrap();
    assert_eq!(toast(cx), MESSAGE_NOT_FOUND);
    assert_eq!(
        f.clipboard_writes().len(),
        1,
        "only the first wrote the clipboard"
    );
}

/// A-043, A-045: `p` shows the full path; Reveal records the viewer's
/// conversation even though the list selection is elsewhere.
#[gpui_kit::test]
fn show_path_and_reveal(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let shown = claude_session(&world, "proj", 1, "Shown in the viewer");
    let other = claude_session(&world, "proj", 2, "Selected in the list");
    let f = OpsFixture::open(cx, &world, &[]);
    let target = f.target(cx, &shown);
    f.open_in_viewer(cx, &shown);
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&other, cx))
    );
    f.recording.clear_calls();
    f.press(cx, "p");
    assert_eq!(toast(cx), target.path.display().to_string());
    assert!(
        f.recording.calls().is_empty(),
        "showing the path has no side effect"
    );

    cx.dispatch_action(f.handle(), RevealInFileManager);
    f.settle(cx);
    assert_eq!(
        f.recording.calls(),
        vec![ServiceCall::Reveal(target.path.clone())]
    );

    // The toast's buttons: Copy writes the path, Reveal reveals this target.
    assert_eq!(
        cx.read(crate::notify::last_toast_actions),
        vec![
            gpui_kit::SharedString::from("Copy"),
            gpui_kit::SharedString::from("Reveal")
        ]
    );
    f.recording.clear_calls();
    let copied = cx
        .update_window(f.handle(), |_, window, cx| {
            crate::notify::trigger_last_toast_action("Copy", window, cx)
        })
        .unwrap();
    assert!(copied);
    assert_eq!(
        f.clipboard_writes(),
        vec![target.path.display().to_string()]
    );
    // Copying replaced the toast: show the path again for the Reveal button.
    f.press(cx, "p");
    f.recording.clear_calls();
    let revealed = cx
        .update_window(f.handle(), |_, window, cx| {
            crate::notify::trigger_last_toast_action("Reveal", window, cx)
        })
        .unwrap();
    assert!(revealed);
    f.settle(cx);
    assert_eq!(
        f.recording.calls(),
        vec![ServiceCall::Reveal(target.path.clone())]
    );
}
