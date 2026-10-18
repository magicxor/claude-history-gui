//! Confirmations of destructive and data-writing actions (DESIGN §5.7, AD18).
//! Frozen: written by WP0, afterwards changed only by the integrator.
//!
//! The pattern every destructive confirmation follows (delete, delete empty
//! transcripts, delete by session ID, Clear semantic cache, copy with
//! overwrites, export Replace):
//!
//! * an `AlertDialog` (not overlay-closable) whose title names the target;
//! * a focused content element with key context `Confirm`: `y`/`Y` confirm,
//!   `n`/`N` cancel, **Enter is ignored** (it beats the Dialog's
//!   `enter → Confirm` because the context is deeper); Esc cancels (Dialog);
//! * `Cancel` and a danger button with a specific verb (`Delete`, `Clear`);
//! * while the work runs the buttons are disabled ([`ConfirmOutcome::Busy`]),
//!   and the caller closes the dialog when it is done ([`ConfirmControl::close`]).
//!
//! With `danger = false` (e.g. a copy without overwrites) the button is the
//! primary one and Enter confirms.
//!
//! **Closing contract.** gpui-kit 0.7.3 can only close the *top-most* dialog
//! of a window, and [`ConfirmControl::close`] closes it after the current
//! effect. So close a confirmation only while it is the top-most dialog:
//! * never open another dialog from `on_confirm` and then return
//!   [`ConfirmOutcome::Close`] (the new dialog would be closed instead);
//! * to continue with another dialog (e.g. a Replace prompt after a choice),
//!   use [`ConfirmControl::close_then`], which runs its follow-up after this
//!   confirmation is gone;
//! * an `OpenTargets` close function (a deletion closes the dialog) calls
//!   [`ConfirmControl::close`]; it runs deferred in the dialog's window.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    WeakEntity, Window, div,
};

use crate::actions::{ConfirmEnterIgnored, ConfirmNo, ConfirmYes};

/// The key context of the focused confirmation content.
pub const CONFIRM_CONTEXT: &str = "Confirm";

/// Extra content below the description (e.g. a `PathList`). Re-runs on every render.
pub type ConfirmBody = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;
/// Called once when the dialog closes (any way). Use it to unregister `OpenTargets`.
pub type ConfirmClosed = Rc<dyn Fn(&mut App)>;

/// What a confirmation shows.
#[derive(Clone)]
pub struct ConfirmOptions {
    /// Names the target: `Delete "Fix login bug"?`.
    pub title: SharedString,
    /// New consequence information only (never "Are you sure…").
    pub description: Option<SharedString>,
    /// Specific verb of the commit button: `Delete`, `Clear`, `Replace`.
    pub confirm_label: SharedString,
    pub cancel_label: SharedString,
    /// Danger button, Enter ignored, y/n hint. `false`: primary button, Enter confirms.
    pub danger: bool,
    /// The key hint line, e.g. `Y delete · N or Esc cancel` (derived when `None`
    /// and `danger`).
    pub key_hint: Option<SharedString>,
    /// Dialog width in rem (default 30rem).
    pub width_rem: Option<f32>,
    pub body: Option<ConfirmBody>,
    pub on_closed: Option<ConfirmClosed>,
}

impl ConfirmOptions {
    /// A destructive confirmation: danger `confirm_label`, Enter ignored.
    pub fn destructive(
        title: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            title: title.into(),
            description: None,
            confirm_label: confirm_label.into(),
            cancel_label: "Cancel".into(),
            danger: true,
            key_hint: None,
            width_rem: None,
            body: None,
            on_closed: None,
        }
    }

    /// A data-writing but non-destructive confirmation: primary button, Enter confirms.
    pub fn non_destructive(
        title: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            danger: false,
            ..Self::destructive(title, confirm_label)
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn key_hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.key_hint = Some(hint.into());
        self
    }

    pub fn width_rem(mut self, rem: f32) -> Self {
        self.width_rem = Some(rem);
        self
    }

    pub fn body(mut self, body: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        self.body = Some(Rc::new(body));
        self
    }

    pub fn on_closed(mut self, on_closed: impl Fn(&mut App) + 'static) -> Self {
        self.on_closed = Some(Rc::new(on_closed));
        self
    }

    /// The hint shown under the content.
    pub fn hint_text(&self) -> Option<SharedString> {
        if let Some(hint) = &self.key_hint {
            return Some(hint.clone());
        }
        self.danger
            .then(|| format!("Y {} · N or Esc cancel", self.confirm_label.to_lowercase()).into())
    }
}

/// What the confirm handler wants next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// Done: close the dialog.
    Close,
    /// Stay open (e.g. an inline error was set).
    KeepOpen,
    /// Work started: disable the buttons until [`ConfirmControl::close`] or
    /// [`ConfirmControl::set_busy`]`(false)`.
    Busy,
}

type ConfirmHandler = Rc<dyn Fn(&ConfirmControl, &mut Window, &mut App) -> ConfirmOutcome>;

/// A handle to an open confirmation.
#[derive(Clone)]
pub struct ConfirmControl {
    entity: WeakEntity<ConfirmContent>,
    window: AnyWindowHandle,
}

impl ConfirmControl {
    /// Whether the dialog is still open.
    pub fn is_open(&self, cx: &App) -> bool {
        self.entity
            .upgrade()
            .is_some_and(|entity| !entity.read(cx).closed)
    }

    /// Whether the buttons are disabled because work is running.
    pub fn is_busy(&self, cx: &App) -> bool {
        self.entity
            .upgrade()
            .is_some_and(|entity| entity.read(cx).busy)
    }

    pub fn set_busy(&self, busy: bool, cx: &mut App) {
        if let Some(entity) = self.entity.upgrade() {
            entity.update(cx, |content, cx| {
                content.busy = busy;
                cx.notify();
            });
        }
    }

    /// Show (or clear) an inline error; the dialog stays open and usable.
    pub fn set_error(&self, error: Option<SharedString>, cx: &mut App) {
        if let Some(entity) = self.entity.upgrade() {
            entity.update(cx, |content, cx| {
                content.error = error;
                content.busy = false;
                cx.notify();
            });
        }
    }

    /// Close the dialog (no-op when it is already closed). Safe to call from
    /// any context: the dialog layer is updated after the current effect.
    /// It closes the window's top-most dialog, so call it only while this
    /// confirmation is the top-most one (module docs).
    pub fn close(&self, cx: &mut App) {
        self.close_then(cx, |_, _| {});
    }

    /// [`ConfirmControl::close`], then run `then` in the dialog's window once
    /// the confirmation is gone (open a follow-up dialog from there, never
    /// before closing this one). `then` does not run when the dialog was
    /// already closed or its window is gone.
    pub fn close_then(&self, cx: &mut App, then: impl FnOnce(&mut Window, &mut App) + 'static) {
        let Some(entity) = self.entity.upgrade() else {
            return;
        };
        if entity.read(cx).closed {
            return;
        }
        mark_closed(&entity, cx);
        let window = self.window;
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    window.close_dialog(cx);
                    then(window, cx);
                })
                .ok();
        });
    }

    fn apply(&self, outcome: ConfirmOutcome, cx: &mut App) {
        match outcome {
            ConfirmOutcome::Close => self.close(cx),
            ConfirmOutcome::KeepOpen => self.set_busy(false, cx),
            ConfirmOutcome::Busy => self.set_busy(true, cx),
        }
    }
}

fn mark_closed(entity: &Entity<ConfirmContent>, cx: &mut App) {
    let on_closed = entity.update(cx, |content, _| {
        if content.closed {
            None
        } else {
            content.closed = true;
            content.options.on_closed.clone()
        }
    });
    if let Some(on_closed) = on_closed {
        on_closed(cx);
    }
}

/// The focused content of a confirmation (key context `Confirm`).
pub struct ConfirmContent {
    options: ConfirmOptions,
    focus: FocusHandle,
    busy: bool,
    error: Option<SharedString>,
    closed: bool,
    on_confirm: ConfirmHandler,
    control: Option<ConfirmControl>,
}

impl Focusable for ConfirmContent {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConfirmContent {
    fn request_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.closed {
            return;
        }
        let Some(control) = self.control.clone() else {
            return;
        };
        let on_confirm = self.on_confirm.clone();
        self.error = None;
        cx.notify();
        // Run the handler after this update so it may use the control freely.
        window.defer(cx, move |window, cx| {
            let outcome = on_confirm(&control, window, cx);
            control.apply(outcome, cx);
        });
    }

    fn request_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.closed {
            return;
        }
        if let Some(control) = self.control.clone() {
            window.defer(cx, move |_, cx| control.close(cx));
        }
    }

    fn on_yes(&mut self, _: &ConfirmYes, window: &mut Window, cx: &mut Context<Self>) {
        self.request_confirm(window, cx);
    }

    fn on_no(&mut self, _: &ConfirmNo, window: &mut Window, cx: &mut Context<Self>) {
        self.request_cancel(window, cx);
    }

    fn on_enter(&mut self, _: &ConfirmEnterIgnored, window: &mut Window, cx: &mut Context<Self>) {
        // Destructive confirmations ignore Enter (AD18); others confirm.
        if !self.options.danger {
            self.request_confirm(window, cx);
        }
    }
}

impl Render for ConfirmContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger_color = theme.danger;
        let busy = self.busy;
        let danger = self.options.danger;
        let body = self.options.body.clone().map(|body| body(window, cx));
        v_flex()
            .id("confirm-content")
            .key_context(CONFIRM_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_yes))
            .on_action(cx.listener(Self::on_no))
            .on_action(cx.listener(Self::on_enter))
            .gap_3()
            .w_full()
            .when_some(self.options.description.clone(), |this, description| {
                this.child(div().text_sm().child(description))
            })
            .when_some(body, |this, body| this.child(body))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .gap_1()
                        .text_sm()
                        .text_color(danger_color)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(error),
                )
            })
            .when_some(self.options.hint_text(), |this, hint| {
                this.child(div().text_xs().text_color(muted).child(hint))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("cancel")
                            .label(self.options.cancel_label.clone())
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.request_cancel(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("ok")
                            .label(self.options.confirm_label.clone())
                            .map(|button| {
                                if danger {
                                    button.danger()
                                } else {
                                    button.primary()
                                }
                            })
                            .loading(busy)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.request_confirm(window, cx)),
                            ),
                    ),
            )
    }
}

/// Open a confirmation (§5.7 pattern). `on_confirm` runs after `y`/`Y`, the
/// confirm button or (non-destructive only) Enter; its [`ConfirmOutcome`]
/// decides whether the dialog closes, stays, or shows busy buttons. It must
/// not open another dialog itself when it returns `Close` (closing contract,
/// module docs): return `KeepOpen` and call [`ConfirmControl::close_then`].
pub fn open_destructive_confirm(
    options: ConfirmOptions,
    window: &mut Window,
    cx: &mut App,
    on_confirm: impl Fn(&ConfirmControl, &mut Window, &mut App) -> ConfirmOutcome + 'static,
) -> ConfirmControl {
    let title = options.title.clone();
    let danger = options.danger;
    let width_rem = options.width_rem.unwrap_or(30.0);
    let entity = cx.new(|cx| ConfirmContent {
        options,
        focus: cx.focus_handle(),
        busy: false,
        error: None,
        closed: false,
        on_confirm: Rc::new(on_confirm),
        control: None,
    });
    let control = ConfirmControl {
        entity: entity.downgrade(),
        window: window.window_handle(),
    };
    entity.update(cx, |content, _| content.control = Some(control.clone()));

    let first_render = Rc::new(Cell::new(true));
    let width = window.rem_size() * width_rem;
    window.open_alert_dialog(cx, {
        let entity = entity.clone();
        move |alert, window, cx| {
            if first_render.replace(false) {
                entity.read(cx).focus.clone().focus(window, cx);
            }
            let cancel_entity = entity.clone();
            let ok_entity = entity.clone();
            let close_entity = entity.clone();
            alert
                .title(title.clone())
                .when(danger, |alert| {
                    alert.icon(Icon::new(IconName::TriangleAlert).text_color(cx.theme().danger))
                })
                .width(width)
                .child(entity.clone())
                // The buttons live in the content (so they can show busy state).
                .footer(div())
                .on_ok(move |_, window, cx| {
                    // Enter outside the focused content: confirm only when not destructive.
                    if !danger {
                        ok_entity.update(cx, |content, cx| content.request_confirm(window, cx));
                    }
                    false
                })
                .on_cancel(move |_, _, cx| {
                    let busy = cancel_entity.read(cx).busy;
                    if !busy {
                        mark_closed(&cancel_entity, cx);
                    }
                    !busy
                })
                .on_close(move |_, _, cx| mark_closed(&close_entity, cx))
        }
    });
    control
}
