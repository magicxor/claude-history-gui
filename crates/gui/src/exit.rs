//! Quitting and exit codes (PD5, PD6, DESIGN §11.22). Frozen: written by
//! WP0, afterwards changed only by the integrator.
//!
//! The process exit code is decided in the UI and read by `main` after the
//! application loop returns ([`exit_code`]).

use std::sync::atomic::{AtomicI32, Ordering};

use gpui_kit::{App, Global};

use crate::globals::app_state;
use crate::persist;
use crate::services::services;

/// Normal end (also: picker cancelled, nothing printed).
pub const EXIT_OK: i32 = 0;
/// Runtime error (upstream `Error: …` exits, e.g. an invalid time range).
pub const EXIT_ERROR: i32 = 1;
/// Command-line usage error (clap-style).
pub const EXIT_USAGE: i32 = 2;
/// The sandbox gate refused to run (§15.4).
pub const EXIT_REFUSED: i32 = 3;

static EXIT_CODE: AtomicI32 = AtomicI32::new(EXIT_OK);

/// The exit code `main` exits with once the application loop returns.
pub fn exit_code() -> i32 {
    EXIT_CODE.load(Ordering::SeqCst)
}

/// The last quit request of this app (tests and QA observe quits through it:
/// the GPUI test platform's `quit` does nothing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuitState {
    pub requested: Option<i32>,
}

impl Global for QuitState {}

/// The exit code of the last quit request, if any.
pub fn quit_requested(cx: &App) -> Option<i32> {
    cx.try_global::<QuitState>()
        .and_then(|state| state.requested)
}

/// Quit with `code`: persist the UI state, record the code, end the app loop.
pub fn quit_with_code(code: i32, cx: &mut App) {
    persist::flush(cx);
    EXIT_CODE.store(code, Ordering::SeqCst);
    cx.set_global(QuitState {
        requested: Some(code),
    });
    cx.quit();
}

/// File → Quit, `secondary-q`, closing the main window. Picker mode quits
/// without output (exit 0).
pub fn quit(cx: &mut App) {
    let picker = cx
        .try_global::<crate::globals::AppState>()
        .is_some_and(|state| state.is_picker());
    if picker {
        quit_without_output(cx);
    } else {
        quit_with_code(EXIT_OK, cx);
    }
}

/// Picker cancel (Esc on an empty query, Quit, main window closed): exit 0,
/// nothing on stdout.
pub fn quit_without_output(cx: &mut App) {
    quit_with_code(EXIT_OK, cx);
}

/// Print `text` to stdout (one write + flush) and quit with exit 0. When
/// stdout cannot be written, `Error: <e>` goes to stderr and the exit code is 1.
pub fn quit_with_output(text: String, cx: &mut App) {
    let services = services(cx);
    match services.write_stdout(&text) {
        Ok(()) => quit_with_code(EXIT_OK, cx),
        Err(error) => {
            services.write_stderr(&format!("Error: {error}\n"));
            quit_with_code(EXIT_ERROR, cx);
        }
    }
}

/// Whether the app runs in picker mode (Quit prints nothing).
pub fn is_picker(cx: &App) -> bool {
    app_state(cx).is_picker()
}
