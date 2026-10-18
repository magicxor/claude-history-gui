//! Help/Tools → Check for updates… (DESIGN §5.18, §11.17; T-020). WP5.
//!
//! One background `services.fetch_latest_release(version)`; the result is
//! mapped to the §5.18 texts by [`describe`]. No automatic checks, no binary
//! replacement: Open release page → `services.open_url`.

use claude_history_core::update::{RELEASES_PAGE_URL, UpdateStatus};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Task,
    Window, div, rems,
};

use super::common::{
    DialogSpec, close_dialog, current, forget_dialog, open_dialog, place_here, set_current,
    spawn_thread,
};
use crate::services::services;

/// The QA id of the dialog.
pub const DIALOG_ID: &str = "updates";

/// What the dialog shows for a finished check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateView {
    /// The headline (§5.18).
    pub message: String,
    /// Release notes (Available only).
    pub notes: Option<String>,
    /// The page Open release page opens (Available only).
    pub release_url: Option<String>,
}

/// The §5.18 text of `status` for the platform `os`/`arch`.
pub fn describe(status: &UpdateStatus, os: &str, arch: &str) -> UpdateView {
    match status {
        UpdateStatus::UpToDate { current } => UpdateView {
            message: format!("Claude History GUI is up to date ({current})."),
            notes: None,
            release_url: None,
        },
        UpdateStatus::Available { current, release } => {
            let mut message = format!(
                "Claude History GUI {} is available (you have {current})",
                release.version
            );
            if release.asset_for(os, arch).is_none() {
                message.push_str(", but there is no build for this platform");
            }
            message.push('.');
            let notes = release.notes.trim();
            UpdateView {
                message,
                notes: (!notes.is_empty()).then(|| notes.to_string()),
                release_url: Some(if release.url.is_empty() {
                    RELEASES_PAGE_URL.to_string()
                } else {
                    release.url.clone()
                }),
            }
        }
        UpdateStatus::Newer { current, latest } => UpdateView {
            message: format!(
                "This build ({current}) is newer than the latest release ({}).",
                latest.version
            ),
            notes: None,
            release_url: None,
        },
        UpdateStatus::NoRelease { .. } => UpdateView {
            message: "No release has been published yet.".to_string(),
            notes: None,
            release_url: None,
        },
    }
}

/// `Update error: <e>` (core's text already carries the prefix).
pub fn error_message(error: &str) -> String {
    if error.starts_with("Update error: ") {
        error.to_string()
    } else {
        format!("Update error: {error}")
    }
}

/// The state of the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckState {
    Checking,
    Done(UpdateView),
    Failed(String),
}

/// The dialog content.
pub struct UpdatesDialog {
    focus: FocusHandle,
    state: CheckState,
    check_id: u64,
    closed: bool,
    _task: Option<Task<()>>,
}

impl Focusable for UpdatesDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The open dialog (tests and QA).
pub fn current_dialog(cx: &App) -> Option<Entity<UpdatesDialog>> {
    current::<UpdatesDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl UpdatesDialog {
    pub fn state(&self) -> &CheckState {
        &self.state
    }

    /// Check (again): Retry.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        self.check_id += 1;
        let id = self.check_id;
        self.state = CheckState::Checking;
        let services = services(cx);
        let version = env!("CARGO_PKG_VERSION");
        let rx = spawn_thread("update-check", move || {
            services.fetch_latest_release(version)
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    if id != this.check_id {
                        return;
                    }
                    this.state = match result {
                        Ok(status) => CheckState::Done(describe(
                            &status,
                            std::env::consts::OS,
                            std::env::consts::ARCH,
                        )),
                        Err(error) => CheckState::Failed(error_message(&error)),
                    };
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    /// The Open release page button.
    pub fn open_release_page(&self, cx: &mut Context<Self>) {
        if let CheckState::Done(view) = &self.state
            && let Some(url) = &view.release_url
        {
            services(cx).open_url(url, cx);
        }
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = true;
        close_dialog(DIALOG_ID, window, cx);
    }
}

impl Render for UpdatesDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let (message, notes, url, failed) = match &self.state {
            CheckState::Checking => (None, None, None, false),
            CheckState::Done(view) => (
                Some(view.message.clone()),
                view.notes.clone(),
                view.release_url.clone(),
                false,
            ),
            CheckState::Failed(error) => (Some(error.clone()), None, None, true),
        };
        v_flex()
            .id("updates-dialog")
            .track_focus(&self.focus)
            .w_full()
            .gap_3()
            .when(message.is_none(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child("Checking for updates…"),
                )
            })
            .when_some(message, |this, message| {
                this.child(
                    div()
                        .id("updates-message")
                        .text_sm()
                        .when(failed, |d| d.text_color(danger))
                        .child(message),
                )
            })
            .when_some(notes, |this, notes| {
                this.child(
                    div()
                        .id("updates-notes")
                        .max_h(rems(14.0))
                        .overflow_y_scroll()
                        .text_xs()
                        .text_color(muted)
                        .child(notes),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(failed, |row| {
                        row.child(
                            Button::new("updates-retry")
                                .label("Retry")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                        )
                    })
                    .when(url.is_some(), |row| {
                        row.child(
                            Button::new("updates-open")
                                .label("Open release page")
                                .small()
                                .primary()
                                .on_click(cx.listener(|this, _, _, cx| this.open_release_page(cx))),
                        )
                    })
                    .child(
                        Button::new("updates-close")
                            .label("Close")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
    }
}

/// Help/Tools → Check for updates… (§5.18, §11.17).
pub fn check_for_updates(window: &mut Window, cx: &mut App) {
    let existing = current_dialog(cx);
    if let Some(existing) = place_here(cx, window, DIALOG_ID, existing, |d: &mut UpdatesDialog| {
        d.closed = true
    }) {
        existing.update(cx, |dialog, cx| dialog.check(cx));
        return;
    }
    let dialog = cx.new(|cx| UpdatesDialog {
        focus: cx.focus_handle(),
        state: CheckState::Checking,
        check_id: 0,
        closed: false,
        _task: None,
    });
    set_current(cx, &dialog, window);
    dialog.update(cx, |dialog, cx| dialog.check(cx));
    let focus = dialog.read(cx).focus.clone();
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: DIALOG_ID,
            title: "Check for updates",
            width_rem: 36.0,
            enter_closes: true,
        },
        dialog,
        focus,
        move |cx| {
            dismissed.update(cx, |dialog, _| dialog.closed = true);
            forget_dialog(DIALOG_ID, cx);
        },
        window,
        cx,
    );
}
