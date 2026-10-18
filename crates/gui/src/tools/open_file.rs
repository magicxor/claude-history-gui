//! File → Open JSONL file… (DESIGN §5.20; T-019). WP5.
//!
//! The native picker (multi-select, through `PlatformServices`) opens one
//! single-file window per chosen file; unavailable in picker mode.

use gpui_kit::{App, Window};

use crate::globals::app_state;
use crate::model::OpenPathsOptions;
use crate::notify;
use crate::services::services;
use crate::single_file;

/// The picker options of §5.20.
pub fn picker_options() -> OpenPathsOptions {
    OpenPathsOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: "Open".into(),
    }
}

/// File/Tools → Open JSONL file… (one single-file window per chosen file).
pub fn open_jsonl_file_dialog(window: &mut Window, cx: &mut App) {
    if app_state(cx).is_picker() {
        return;
    }
    services(cx).prompt_open_paths(
        picker_options(),
        window,
        cx,
        Box::new(|result, window, cx| match result {
            Ok(Some(paths)) => {
                for path in paths {
                    single_file::open_window(path, cx);
                }
            }
            Ok(None) => {}
            Err(error) => notify::error(
                window,
                cx,
                format!("Couldn't open the file picker: {error}"),
            ),
        }),
    );
}
