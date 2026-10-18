//! Core debug sink → [`DebugBuffer`] and stderr (DESIGN §7.3 step 1). WP0 → WP7.
//!
//! Core filters debug messages by the minimum level its callers pass, so the
//! GUI passes the *capture level* = the lower of the `--debug` level and the
//! Debug log dialog's level (default Info, [`capture_level`]). Lines at or
//! above the `--debug` level are also written to stderr through `Services`
//! in upstream's format (`[LEVEL] message`).

use std::sync::Arc;

use claude_history_core::debug::{self, DebugLevel};
use gpui_kit::{App, Global};

use crate::globals::{DebugBuffer, DebugBufferHandle};
use crate::services::Services;

/// The `--debug` level of this run (`None` = no stderr output).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StderrLevel(pub Option<DebugLevel>);

impl Global for StderrLevel {}

/// Install the process-wide core sink. Needs the `DebugBufferHandle` and
/// `Services` globals.
pub fn install(cx: &mut App, stderr_level: Option<DebugLevel>) {
    cx.set_global(StderrLevel(stderr_level));
    let buffer: Arc<DebugBuffer> = cx.global::<DebugBufferHandle>().0.clone();
    let services = cx.try_global::<Services>().cloned();
    if let Some(level) = stderr_level
        && level < buffer.min_level()
    {
        buffer.set_min_level(level);
    }
    debug::set_sink(move |level, message| {
        buffer.push(level, message);
        if let (Some(min), Some(services)) = (stderr_level, services.as_ref())
            && debug::should_log(min, level)
        {
            services.write_stderr(&format!("[{}] {}\n", level.tag(), message));
        }
    });
}

/// The minimum level the GUI passes to core calls that take one: the lower
/// of the `--debug` level and the Debug log dialog's level.
pub fn capture_level(cx: &App) -> DebugLevel {
    let dialog = cx
        .try_global::<DebugBufferHandle>()
        .map(|handle| handle.0.min_level())
        .unwrap_or(DebugLevel::Info);
    match cx.try_global::<StderrLevel>().and_then(|level| level.0) {
        Some(stderr) => stderr.min(dialog),
        None => dialog,
    }
}
