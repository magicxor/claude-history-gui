//! Session action tests (G, DESIGN §17.7). WP3.
//!
//! One module per action family; the helpers every module shares live here.

mod copy_tests;
mod delete_tests;
mod export_tests;
mod launcher_tests;
mod rename_tests;
mod resume_tests;
mod select_tests;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

use crate::globals::last_toast;

/// Render a frame of `handle` (focus and key contexts settle on a frame).
pub(crate) fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("the window is open");
}

/// Whether `handle` shows a dialog.
pub(crate) fn has_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .expect("the window is open")
}

/// The text of the last toast.
pub(crate) fn toast(cx: &TestAppContext) -> String {
    cx.read(|cx| last_toast(cx).to_string())
}

/// The last QA dialog id registered by an owner.
pub(crate) fn dialog_id(cx: &TestAppContext) -> String {
    cx.read(|cx| crate::workspace::startup::last_dialog(cx).to_string())
}
