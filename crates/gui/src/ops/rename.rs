//! Rename (DESIGN §5.6, §11.8, K3). WP3.
//!
//! The dialog captures the target when it opens (PD15, A1): every later step
//! (save, the list update, the error text) uses that target and never the
//! live selection. The write is core's `history::rename_session`, run off the
//! UI thread; the result goes to `ListPane::apply_renamed`, which keeps the
//! row, the header and the search indices consistent.

use std::cell::Cell;
use std::rc::Rc;

use claude_history_core::browser::RenameError;
use claude_history_core::history::rename_session;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, WeakEntity,
    Window, div,
};

use crate::actions::RenameClear;
use crate::globals::{BusySpan, BusyTimer, OpenTargets, UiBusy, app_state};
use crate::model::ConversationTarget;
use crate::ops::copy::{can_act, clear_dialog, live_target, main_list};
use crate::qa_ext::QaExt as _;
use crate::workspace::startup::register_dialog;

/// The QA / probe id of the dialog.
pub const DIALOG_ID: &str = "rename";
/// The inline error when the conversation left the list meanwhile.
pub const NOT_IN_LIST: &str = "This conversation is no longer in the list";

/// The Rename conversation dialog for the captured `target` (no-op for an
/// unknown source and while the list loads).
///
/// The caller may be inside the list's own update, which must not be read
/// back, so the dialog opens on the next turn and takes the title (the
/// pre-fill, the target line) from the live corpus entry: the captured
/// target may predate an earlier rename or come from an agent hit that never
/// knew the custom title.
pub fn open_rename_dialog(target: ConversationTarget, window: &mut Window, cx: &mut App) {
    if !can_act(&target) {
        return;
    }
    window.defer(cx, move |window, cx| {
        let target = live_target(target, cx);
        open_now(target, window, cx);
    });
}

fn open_now(target: ConversationTarget, window: &mut Window, cx: &mut App) {
    let registration = Rc::new(Cell::new(None::<u64>));
    let dialog = cx.new(|cx| RenameDialog::new(target.clone(), registration.clone(), window, cx));
    let id = {
        let dialog = dialog.downgrade();
        OpenTargets::register(
            cx,
            window.window_handle(),
            target.path.clone(),
            target.title.clone(),
            Box::new(move |window, cx| {
                // `mark_closed` forgets the QA dialog id; the programmatic
                // close below does not run `on_close`.
                if let Some(dialog) = dialog.upgrade() {
                    dialog.update(cx, |dialog, cx| dialog.mark_closed(cx));
                }
                window.close_dialog(cx);
            }),
        )
    };
    registration.set(Some(id));
    register_dialog(cx, DIALOG_ID);
    #[cfg(test)]
    cx.set_global(LastRename(dialog.downgrade()));

    let first_render = Rc::new(Cell::new(true));
    let width = window.rem_size() * 32.0;
    window.open_dialog(cx, move |builder, window, cx| {
        if first_render.replace(false) {
            let input = dialog.read(cx).input.clone();
            input.read(cx).focus_handle(cx).focus(window, cx);
        }
        let cancel = dialog.clone();
        let close = dialog.clone();
        builder
            .title("Rename conversation")
            .w(width)
            .child(dialog.clone())
            // The buttons live in the content (they show the saving state).
            .footer(div())
            .on_ok(|_, _, _| false)
            .on_cancel(move |_, _, cx| {
                let busy = cancel.read(cx).saving;
                if !busy {
                    cancel.update(cx, |dialog, cx| dialog.mark_closed(cx));
                }
                !busy
            })
            .on_close(move |_, _, cx| close.update(cx, |dialog, cx| dialog.mark_closed(cx)))
    });
}

/// The most recently opened rename dialog (tests).
#[cfg(test)]
pub struct LastRename(pub WeakEntity<RenameDialog>);

#[cfg(test)]
impl gpui_kit::Global for LastRename {}

/// The dialog content (key context `RenameDialog`).
pub struct RenameDialog {
    target: ConversationTarget,
    input: Entity<InputState>,
    focus: FocusHandle,
    error: Option<SharedString>,
    saving: bool,
    closed: bool,
    registration: Rc<Cell<Option<u64>>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for RenameDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        let _ = cx;
        self.focus.clone()
    }
}

impl RenameDialog {
    fn new(
        target: ConversationTarget,
        registration: Rc<Cell<Option<u64>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefill = target.custom_title.clone().unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Conversation title"));
        // `set_value` puts the caret at the end (single-line input).
        input.update(cx, |state, cx| state.set_value(prefill, window, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.save(window, cx),
                InputEvent::Change if this.error.take().is_some() => cx.notify(),
                _ => {}
            },
        );
        Self {
            target,
            input,
            focus: cx.focus_handle(),
            error: None,
            saving: false,
            closed: false,
            registration,
            _subscriptions: vec![subscription],
        }
    }

    /// The captured target.
    pub fn target(&self) -> &ConversationTarget {
        &self.target
    }

    /// The input entity (tests).
    pub fn input(&self) -> Entity<InputState> {
        self.input.clone()
    }

    /// The inline error, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether a write is running.
    pub fn is_saving(&self) -> bool {
        self.saving
    }

    /// Whether the dialog was closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn mark_closed(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(id) = self.registration.take() {
            OpenTargets::unregister(cx, id);
        }
        clear_dialog(cx, DIALOG_ID);
    }

    /// The Cancel button: `close_dialog` alone skips `on_close`, so the
    /// bookkeeping (open targets, QA dialog id) is released first.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.mark_closed(cx);
        window.close_dialog(cx);
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.error = None;
        cx.notify();
    }

    /// Save: trim, empty clears the title; the write runs in the background.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.closed {
            return;
        }
        let Some(source) = self.target.source else {
            return;
        };
        let list = main_list(cx);
        // The conversation must still be listed: the list update is the other
        // half of a rename, and a write nobody can show would only confuse.
        let listed = list
            .as_ref()
            .is_some_and(|list| list.read(cx).conversation(&self.target.path).is_some());
        if !listed {
            self.error = Some(NOT_IN_LIST.into());
            cx.notify();
            return;
        }
        let title = self.input.read(cx).value().to_string();
        let (env, show_last) = {
            let state = app_state(cx);
            (state.env_with_workspace(), state.settings.show_last)
        };
        let path = self.target.path.clone();
        self.saving = true;
        self.error = None;
        cx.notify();
        let task = {
            let path = path.clone();
            cx.background_executor()
                .spawn(async move { rename_session(&env, &path, source, &title, show_last) })
        };
        let dialog = cx.entity().downgrade();
        window
            .spawn(&*cx, async move |cx| {
                let result = task.await;
                // The apply is timed around the whole update: the list, the
                // viewer header and the menus it notifies (X-038, `UiBusy`).
                let timer = BusyTimer::start();
                let applied = cx
                    .update(|window, cx| finish(result, path, list, dialog, window, cx))
                    .unwrap_or(false);
                if applied {
                    let measured = timer.stop();
                    cx.update(|_, cx| UiBusy::record(cx, BusySpan::RenameApply, measured))
                        .ok();
                }
            })
            .detach();
    }
}

/// The write finished: update the list (whatever happened to the dialog),
/// then close the dialog or show the inline error. Returns whether the list
/// applied the rename.
fn finish(
    result: Result<claude_history_core::history::Conversation, RenameError>,
    path: std::path::PathBuf,
    list: Option<Entity<crate::list::ListPane>>,
    dialog: WeakEntity<RenameDialog>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let outcome = result.and_then(|conversation| match &list {
        Some(list) => crate::globals::sub_span(cx, "list.apply_renamed", |cx| {
            list.update(cx, |list, cx| list.apply_renamed(&path, conversation, cx))
        }),
        None => Err(RenameError::NoTarget),
    });
    let applied = outcome.is_ok();
    let Some(dialog) = dialog.upgrade() else {
        return applied;
    };
    match outcome {
        Ok(()) => {
            dialog.update(cx, |dialog, cx| dialog.mark_closed(cx));
            window.close_dialog(cx);
        }
        Err(error) => {
            let text: SharedString = match error {
                RenameError::NoTarget => NOT_IN_LIST.into(),
                other => other.to_string().into(),
            };
            dialog.update(cx, |dialog, cx| {
                dialog.saving = false;
                dialog.error = Some(text);
                cx.notify();
            });
        }
    }
    applied
}

impl Render for RenameDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let saving = self.saving;
        v_flex()
            .id("rename-dialog")
            .key_context("RenameDialog")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &RenameClear, window, cx| this.clear(window, cx)))
            .gap_2()
            .w_full()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(self.target.describe()),
            )
            .child(
                div()
                    .w_full()
                    .child(
                        Input::new(&self.input)
                            .small()
                            .disabled(saving)
                            .aria_label("Conversation title"),
                    )
                    .qa_id("rename:input"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Enter save · Esc cancel · Ctrl+U clear"),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .id("rename-error")
                        .gap_1()
                        .text_sm()
                        .text_color(danger)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(error),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("rename-cancel")
                            .label("Cancel")
                            .disabled(saving)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx)))
                            .qa_id("rename:cancel"),
                    )
                    .child(
                        Button::new("rename-save")
                            .label("Save")
                            .primary()
                            .loading(saving)
                            .disabled(saving)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)))
                            .qa_id("rename:save"),
                    ),
            )
    }
}
