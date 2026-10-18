//! Copy path / session ID / message, show path, reveal (DESIGN §11.11, PD8).
//! WP3.
//!
//! Every copy goes through `PlatformServices::clipboard_write` (PD10): one
//! call per action and no other side effect (A-044). The texts are core's
//! constants (`export::*_COPIED_TO_CLIPBOARD`), so what the user reads is what
//! the TUI said.

use claude_history_core::export::{
    MESSAGE_COPIED_TO_CLIPBOARD, NO_TEXT_CONTENT, PATH_COPIED_TO_CLIPBOARD,
    SESSION_ID_COPIED_TO_CLIPBOARD,
};
use gpui_kit::{App, Entity, Window};

use crate::globals::main_panes;
use crate::list::ListPane;
use crate::model::ConversationTarget;
use crate::notify::Toast;
use crate::services::services;
use crate::workspace::startup::{LastDialog, last_dialog};

// ----- helpers shared by the `ops` modules ------------------------------------------

/// The main window's list (`None`: a FILE launch has no main window yet).
pub(crate) fn main_list(cx: &App) -> Option<Entity<ListPane>> {
    main_panes(cx).list()
}

/// Resume, Fork, Rename and Delete do nothing for a target of unknown source
/// ("Unknown conversation type"). The "list still loading" no-op is the
/// pane's (the list handles those actions before it calls `ops`, and `ops`
/// entry points run inside the pane's update, where reading it would panic).
pub(crate) fn can_act(target: &ConversationTarget) -> bool {
    target.can_modify()
}

/// `target` with the title, custom title and project of the live corpus entry
/// (the captured target may be older than the last rename, or built from an
/// agent hit that has no custom title). Must not run inside the list's own
/// update.
pub(crate) fn live_target(mut target: ConversationTarget, cx: &App) -> ConversationTarget {
    let Some(list) = main_list(cx) else {
        return target;
    };
    if let Some(conversation) = list.read(cx).conversation(&target.path) {
        let fresh = ConversationTarget::from_conversation(&conversation);
        target.title = fresh.title;
        target.custom_title = fresh.custom_title;
        if fresh.project_name.is_some() {
            target.project_name = fresh.project_name;
        }
        if fresh.project_path.is_some() {
            target.project_path = fresh.project_path;
        }
    }
    target
}

/// Forget the QA dialog id when `id` is still the registered one.
pub(crate) fn clear_dialog(cx: &mut App, id: &str) {
    if last_dialog(cx).as_ref() == id {
        cx.set_global(LastDialog::default());
    }
}

/// Marker type: copy confirmations replace one another instead of stacking.
struct CopyToast;

/// Marker type: the Show file path toast.
struct PathToast;

/// Write `text` to the clipboard and confirm with `message` (success).
fn copy_text(text: String, message: &'static str, window: &mut Window, cx: &mut App) {
    services(cx).clipboard_write(text, cx);
    Toast::success(message)
        .replacing::<CopyToast>("clipboard")
        .show(window, cx);
}

/// `Y`: the transcript path.
pub fn copy_path(target: &ConversationTarget, window: &mut Window, cx: &mut App) {
    copy_text(
        target.path.display().to_string(),
        PATH_COPIED_TO_CLIPBOARD,
        window,
        cx,
    );
}

/// `I`: the "session ID" of the TUI, the file stem (quirk 6 kept).
pub fn copy_session_id(target: &ConversationTarget, window: &mut Window, cx: &mut App) {
    copy_text(
        target.file_stem.clone(),
        SESSION_ID_COPIED_TO_CLIPBOARD,
        window,
        cx,
    );
}

/// Pi/OMP: the session header's id (shown only when it differs from the stem).
pub fn copy_header_session_id(target: &ConversationTarget, window: &mut Window, cx: &mut App) {
    copy_text(
        target.session_id.clone(),
        SESSION_ID_COPIED_TO_CLIPBOARD,
        window,
        cx,
    );
}

/// `p`: an info toast with the full path and Copy / Reveal buttons.
pub fn show_path(target: &ConversationTarget, window: &mut Window, cx: &mut App) {
    let for_copy = target.clone();
    let for_reveal = target.clone();
    Toast::info(target.path.display().to_string())
        .replacing::<PathToast>("path")
        .button("Copy", move |window, cx| copy_path(&for_copy, window, cx))
        .button("Reveal", move |window, cx| reveal(&for_reveal, window, cx))
        .show(window, cx);
}

/// Session → Reveal in file manager (the captured target's transcript).
pub fn reveal(target: &ConversationTarget, _window: &mut Window, cx: &mut App) {
    services(cx).reveal_path(&target.path, cx);
}

/// Copy the focused message's text (`Err` = message not found). An empty text
/// (a summary group, quirk 5) copies nothing.
pub fn copy_message(
    target: &ConversationTarget,
    text: Result<String, String>,
    window: &mut Window,
    cx: &mut App,
) {
    let _ = target;
    match text {
        Ok(text) if !text.is_empty() => {
            copy_text(text, MESSAGE_COPIED_TO_CLIPBOARD, window, cx);
        }
        Ok(_) => {
            Toast::info(NO_TEXT_CONTENT)
                .replacing::<CopyToast>("clipboard")
                .show(window, cx);
        }
        Err(error) => {
            Toast::warning(error)
                .replacing::<CopyToast>("clipboard")
                .show(window, cx);
        }
    }
}
