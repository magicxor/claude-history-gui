//! Tools → Show projects directory (DESIGN §5.20; T-016). WP5 → WP-I.
//!
//! With a workspace: toast `Projects directory: <path>` with Copy and Reveal
//! (Reveal is left out, and the text says `(does not exist yet)`, when the
//! folder is missing: a toast button cannot be disabled). Without one the
//! command is disabled: the window roots register its listener only while a
//! workspace is set (integrator, Phase C). The only way to run it without a
//! workspace is the `--show-dir` startup tool of a launch without stdout; it
//! shows core's error (`Choose a workspace folder to show its Claude
//! projects directory`) with a Choose workspace… action (PD16).

use std::path::{Path, PathBuf};

use claude_history_core::maintenance::show_dir;
use gpui_kit::{App, Window};

use super::common::error_text;
use crate::globals::{app_state, main_panes};
use crate::notify::{self, Toast};
use crate::services::services;

/// The toast text for `path`.
pub fn toast_text(path: &Path, exists: bool) -> String {
    let mut text = format!("Projects directory: {}", path.display());
    if !exists {
        text.push_str(" (does not exist yet)");
    }
    text
}

/// The Copy button: the path to the clipboard.
pub fn copy_path(path: &Path, window: &mut Window, cx: &mut App) {
    services(cx).clipboard_write(path.display().to_string(), cx);
    notify::info(window, cx, "Copied to clipboard");
}

/// The Reveal button: the folder in the file manager.
pub fn reveal_path(path: &Path, cx: &mut App) {
    services(cx).reveal_path(path, cx);
}

/// The directory the toast names for the current workspace (core's error
/// without one).
pub fn projects_directory(cx: &App) -> Result<PathBuf, String> {
    let state = app_state(cx);
    show_dir(&state.env, state.workspace.as_ref()).map_err(|error| error_text(&error))
}

/// Tools → Show projects directory (§5.20).
pub fn show_projects_directory(window: &mut Window, cx: &mut App) {
    match projects_directory(cx) {
        Ok(path) => {
            let exists = path.is_dir();
            let copy = path.clone();
            let toast = Toast::info(toast_text(&path, exists))
                .button("Copy", move |window, cx| copy_path(&copy, window, cx));
            let toast = if exists {
                toast.button("Reveal", move |_, cx| reveal_path(&path, cx))
            } else {
                toast
            };
            toast.show(window, cx);
        }
        Err(error) => {
            // PD16: the error comes with its recovery action (§5.20).
            if app_state(cx).workspace.is_none() {
                Toast::error(error)
                    .action("Choose workspace…", |window, cx| {
                        if let Some(list) = main_panes(cx).list() {
                            list.update(cx, |list, cx| list.choose_workspace_folder(window, cx));
                        }
                    })
                    .show(window, cx);
            } else {
                notify::error(window, cx, error);
            }
        }
    }
}
