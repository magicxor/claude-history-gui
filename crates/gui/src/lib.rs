//! claude-history-gui: a desktop browser for Claude Code, Pi and OMP
//! conversation history on gpui-kit 0.7.3. All history logic lives in
//! `claude-history-core`; this crate is the GUI (DESIGN.md in `docs/`).
//!
//! Frozen module tree (WP0; changed only by the integrator). The crate is a
//! library plus a thin binary (`main.rs`), so public items consumed by work
//! packages that are not merged yet are not dead code.

pub mod actions;
pub mod app;
pub mod debug_sink;
pub mod deep_search;
pub mod embedder;
pub mod exit;
pub mod globals;
pub mod help;
pub mod keymap;
pub mod list;
pub mod menus;
pub mod model;
pub mod notify;
pub mod ops;
pub mod palette;
pub mod persist;
pub mod qa;
pub mod qa_ext;
pub mod services;
pub mod settings;
pub mod single_file;
pub mod theme_tokens;
pub mod tools;
pub mod viewer;
pub mod widgets;
pub mod workspace;

#[cfg(test)]
pub mod test_support;

#[cfg(test)]
mod skeleton_tests;

/// Cross-pane gpui tests (WP-I, DESIGN §17.10).
#[cfg(test)]
mod integration_tests;

use claude_history_core::HistoryEnv;

use crate::model::LaunchContext;
use crate::services::Services;

/// Run the GUI with the launch context and environment `main` prepared.
/// Returns when the app quits; `exit::exit_code()` is the exit code. The
/// services follow the launch (`Services::for_launch`: recording for a QA
/// run, else the real ones).
pub fn run(launch: LaunchContext, env: HistoryEnv) {
    app::run(launch, env);
}

/// [`run`] with explicit services.
pub fn run_with_services(launch: LaunchContext, env: HistoryEnv, services: Services) {
    app::run_with_services(launch, env, services);
}
