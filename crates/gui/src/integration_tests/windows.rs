//! Window semantics: C-065, X-033.

use claude_history_core::theme::{ThemeKind, ThemePreference};
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};

use super::{render, settle};
use crate::actions::*;
use crate::exit::{EXIT_OK, quit_requested};
use crate::globals::main_panes;
use crate::services::ServiceCall;
use crate::test_support::fixtures_ops::claude_session;
use crate::test_support::fixtures_viewer::{single_file_viewer, wait_viewer_ready};
use crate::test_support::{TestWorld, global_lock, open_main, open_single_file, recording};

/// Close `handle` the way the OS does (its should-close handler). Returns
/// whether the handler let it close.
fn os_close(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle) -> bool {
    let mut window = VisualTestContext::from_window(handle, cx);
    window.simulate_close()
}

fn is_open(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle) -> bool {
    cx.update_window(handle, |_, _, _| ()).is_ok()
}

/// C-065, X-033: closing a single-file window keeps the app; closing the main
/// window quits, with single-file windows open.
#[gpui_kit::test]
fn c065_closing_the_main_window_quits(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Two windows");
    let other = claude_session(&world, "proj", 2, "Second window");
    let (main, _ws) = open_main(cx, &world, &[]);
    let first = open_single_file(cx, &world, &path);
    let second = open_single_file(cx, &world, &other);
    // File → Close window in a single-file window (Ctrl+W there).
    cx.dispatch_action(first, CloseWindow);
    settle(cx);
    assert!(!is_open(cx, first), "the single-file window closed");
    assert!(is_open(cx, main));
    assert_eq!(cx.read(quit_requested), None, "the app keeps running");
    // Closing the main window quits, the other single-file window notwithstanding.
    assert!(is_open(cx, second));
    os_close(cx, main);
    assert_eq!(cx.read(quit_requested), Some(EXIT_OK));
}

/// C-065, PD5: in picker mode closing the main window quits without output.
#[gpui_kit::test]
fn c065_picker_mode_closes_without_output(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    claude_session(&world, "proj", 1, "Picker");
    let (main, _ws) = open_main(cx, &world, &["--show-path"]);
    settle(cx);
    os_close(cx, main);
    assert_eq!(cx.read(quit_requested), Some(EXIT_OK));
    let printed = recording(cx)
        .calls()
        .into_iter()
        .filter(|call| matches!(call, ServiceCall::Stdout(_) | ServiceCall::StdoutFailed(_)))
        .count();
    assert_eq!(printed, 0, "nothing printed");
}

/// C-065, PD14: a FILE launch has no main window; File → Show conversations
/// opens it (once), and from then on it is the main window.
#[gpui_kit::test]
fn c065_show_conversations_opens_the_main_window(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "File launch");
    let single = open_single_file(cx, &world, &path);
    assert!(
        cx.read(|cx| main_panes(cx).window.is_none()),
        "no main window yet"
    );
    let before = cx.read(|cx| cx.windows().len());
    cx.dispatch_action(single, ShowConversations);
    settle(cx);
    let main = cx
        .read(|cx| main_panes(cx).window)
        .expect("Show conversations opened the main window");
    assert_eq!(cx.read(|cx| cx.windows().len()), before + 1);
    assert!(is_open(cx, single), "the single-file window stays");
    // Again: the main window is focused, not opened twice.
    cx.dispatch_action(single, ShowConversations);
    settle(cx);
    assert_eq!(cx.read(|cx| cx.windows().len()), before + 1);
    assert_eq!(cx.read(|cx| main_panes(cx).window), Some(main));
    // It is the main window: closing it quits.
    os_close(cx, main);
    assert_eq!(cx.read(quit_requested), Some(EXIT_OK));
}

/// X-033: the main window and a single-file window share the app state — a
/// theme change from either reaches both (kit theme, core palette, each
/// viewer redrawn in the new kind).
#[gpui_kit::test]
fn x033_windows_share_the_theme(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Theme in two windows");
    let (main, ws) = open_main(cx, &world, &[]);
    let single = open_single_file(cx, &world, &path);
    let single_viewer = single_file_viewer(cx, single);
    wait_viewer_ready(cx, single, &single_viewer);
    let main_viewer = cx.read(|cx| ws.read(cx).viewer().clone());
    cx.update_window(main, |_, window, cx| {
        main_viewer.update(cx, |viewer, cx| {
            viewer.open(
                crate::model::OpenRequest::new(
                    crate::model::ConversationTarget::from_parsed_file(&path, None),
                    true,
                ),
                window,
                cx,
            )
        })
    })
    .unwrap();
    wait_viewer_ready(cx, main, &main_viewer);

    let switches: [(
        gpui_kit::AnyWindowHandle,
        Box<dyn gpui_kit::Action>,
        ThemeKind,
        bool,
    ); 2] = [
        (main, Box::new(SetThemeDark), ThemeKind::Dark, true),
        (single, Box::new(SetThemeLight), ThemeKind::Light, false),
    ];
    for (from, action, kind, dark) in switches {
        cx.update_window(from, |_, window, cx| window.dispatch_action(action, cx))
            .unwrap();
        settle(cx);
        render(cx, main);
        render(cx, single);
        assert_eq!(cx.read(|cx| cx.theme_is_dark()), dark);
        assert_eq!(claude_history_core::theme::kind(), kind);
        for viewer in [&main_viewer, &single_viewer] {
            assert_eq!(cx.read(|cx| viewer.read(cx).drawn_theme()), Some(kind));
        }
        let wanted = if dark {
            ThemePreference::Dark
        } else {
            ThemePreference::Light
        };
        assert_eq!(cx.read(crate::theme_tokens::preference), wanted);
    }
}

trait ThemeIsDark {
    fn theme_is_dark(&self) -> bool;
}

impl ThemeIsDark for gpui_kit::App {
    fn theme_is_dark(&self) -> bool {
        use gpui_kit::component::ActiveTheme as _;
        self.theme().is_dark()
    }
}
