//! Cross-pane gpui tests with the real panes (DESIGN §17.10). WP0 created
//! the module; WP-I owns it: one file per area.
//!
//! * `focus` — preview debounce and flush (L-085), list ↔ viewer round trip
//!   (L-098), region cycling (C-058), focus-lost fallback (X-043), focus
//!   restore after overlays (X-024), pane widths and the agent panel (X-040);
//! * `dispatch` — menu dispatch equals key dispatch (A-055), A1/A2 through the
//!   menu bar, the palette and Deep search hits (X-001, X-002), SmartCopy
//!   routing (V-103);
//! * `corpus` — header live update (V-007), tool deletions across panes
//!   (T-023), Deep search open-at-hit (D-011), the error table (X-023);
//! * `windows` — main-window close and Show conversations (C-065), multiple
//!   windows (X-033);
//! * `measure` — the UI-thread spans of X-038, the frame counter of X-007,
//!   the live text view of the Debug log.

mod corpus;
mod dispatch;
mod focus;
mod measure;
mod windows;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, TestAppContext};

use crate::viewer::ViewerPane;
use crate::workspace::Workspace;

/// Draw a frame of `handle` and let deferred work run.
pub(super) fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("the window is open");
    cx.run_until_parked();
}

/// Wait (real time, at most 30 s) until `done` holds.
pub(super) fn wait_for(cx: &mut TestAppContext, what: &str, done: impl Fn(&App) -> bool) {
    cx.executor().allow_parking();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if cx.read(&done) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Let background work and its deferred callbacks finish.
pub(super) fn settle(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    for _ in 0..25 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(2));
    }
    cx.run_until_parked();
}

/// The main window's viewer.
pub(super) fn viewer_of(cx: &TestAppContext, ws: &Entity<Workspace>) -> Entity<ViewerPane> {
    cx.read(|cx| ws.read(cx).viewer().clone())
}

/// The path the main window's viewer shows.
pub(super) fn shown_path(cx: &TestAppContext, ws: &Entity<Workspace>) -> Option<PathBuf> {
    cx.read(|cx| {
        ws.read(cx)
            .viewer()
            .read(cx)
            .target()
            .map(|target| target.path.clone())
    })
}

/// A `Workspace` probe (`focus`, `region`, `layout.*`, the panes' keys).
pub(super) fn probe(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    ws: &Entity<Workspace>,
    key: &str,
) -> String {
    let ws = ws.clone();
    cx.update_window(handle, |_, window, cx| {
        ws.update(cx, |ws, cx| ws.qa_probe(key, window, cx))
    })
    .expect("the window is open")
    .unwrap_or_else(|| panic!("probe {key} is answered"))
}

/// The element id of the list row of `path`.
pub(super) fn row_id(path: &Path) -> String {
    format!("row:{}", path.display())
}

/// Click the list row of `path` (a real click on the painted row).
pub(super) fn click_row(cx: &mut TestAppContext, handle: AnyWindowHandle, path: &Path) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(row_id(path), cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
}

/// Whether `handle` shows a modal dialog.
pub(super) fn has_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    use gpui_kit::component::WindowExt as _;
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap_or(false)
}
