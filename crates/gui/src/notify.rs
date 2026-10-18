//! In-app notifications (PD16, DESIGN §12). Frozen: written by WP0,
//! afterwards changed only by the integrator.
//!
//! * Four kinds ([`ToastKind`]): info, success, warning, error.
//! * Repeated status toasts replace the previous one with a key
//!   (`Notification::id1::<K>(key)`, [`Toast::replacing`]).
//! * Toasts with an action button set `autohide(true)` explicitly (gpui-kit
//!   turns autohide off when an action is added).
//! * Two-button toasts use custom content ([`Toast::buttons`]).
//! * Every toast records its text in [`LastToast`] (QA probes, tests) and
//!   its button labels and handlers in [`LastToastActions`]; tests and QA
//!   press a button with [`trigger_last_toast_action`] (integrator, Phase B,
//!   WP3 request: A-010, A-061).
//! * [`gui_text`] maps core status constants to the in-app copy rules.

use std::any::TypeId;
use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, App, ElementId, Global, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Window, div,
};

use crate::globals::LastToast;

/// The four notification kinds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToastKind {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// A button handler of a toast.
pub type ToastHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A notification under construction. Push it with [`Toast::show`].
#[derive(Clone)]
pub struct Toast {
    kind: ToastKind,
    message: SharedString,
    detail: Option<SharedString>,
    replace_key: Option<(TypeId, ElementId)>,
    action: Option<(SharedString, ToastHandler)>,
    buttons: Vec<(SharedString, ToastHandler)>,
    autohide: bool,
}

impl Toast {
    pub fn new(kind: ToastKind, message: impl Into<SharedString>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            replace_key: None,
            action: None,
            buttons: Vec::new(),
            autohide: true,
        }
    }

    pub fn info(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Info, message)
    }

    pub fn success(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Success, message)
    }

    pub fn warning(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Warning, message)
    }

    pub fn error(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Error, message)
    }

    /// A muted second line (e.g. the command a resume ran).
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Replace an earlier toast pushed with the same `K` and `key`.
    pub fn replacing<K: 'static>(mut self, key: impl Into<ElementId>) -> Self {
        self.replace_key = Some((TypeId::of::<K>(), key.into()));
        self
    }

    /// One action button (e.g. "Retry"). The toast still hides on its own.
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.action = Some((label.into(), Rc::new(handler)));
        self
    }

    /// Additional buttons rendered in the toast body (e.g. "Copy" and
    /// "Reveal" of Show file path). Use at most two.
    pub fn button(
        mut self,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.buttons.push((label.into(), Rc::new(handler)));
        self
    }

    /// Keep the toast until the user closes it (errors that need attention).
    pub fn sticky(mut self) -> Self {
        self.autohide = false;
        self
    }

    pub fn kind(&self) -> ToastKind {
        self.kind
    }

    pub fn message(&self) -> &SharedString {
        &self.message
    }

    /// The labels of the toast's buttons: the action button first, then the
    /// body buttons.
    pub fn action_labels(&self) -> Vec<SharedString> {
        self.actions().into_iter().map(|(label, _)| label).collect()
    }

    fn actions(&self) -> Vec<(SharedString, ToastHandler)> {
        self.action
            .iter()
            .chain(self.buttons.iter())
            .cloned()
            .collect()
    }

    /// Push the toast to `window` and record it in [`LastToast`] and
    /// [`LastToastActions`].
    pub fn show(self, window: &mut Window, cx: &mut App) {
        let timer = crate::globals::BusyTimer::start();
        self.show_now(window, cx);
        crate::globals::record_sub_span(cx, "toast.show", timer.stop());
    }

    fn show_now(self, window: &mut Window, cx: &mut App) {
        cx.set_global(LastToast(self.message.clone()));
        cx.set_global(LastToastActions(self.actions()));
        #[cfg(test)]
        cx.default_global::<ToastLog>()
            .0
            .push((window.window_handle(), self.message.clone()));
        let note = self.build();
        window.push_notification(note, cx);
    }

    /// Push the toast to the window behind `handle` (async code).
    pub fn show_in(self, handle: AnyWindowHandle, cx: &mut App) {
        handle
            .update(cx, |_, window, cx| self.show(window, cx))
            .ok();
    }

    fn build(self) -> Notification {
        let mut note = match self.kind {
            ToastKind::Info => Notification::info(self.message.clone()),
            ToastKind::Success => Notification::success(self.message.clone()),
            ToastKind::Warning => Notification::warning(self.message.clone()),
            ToastKind::Error => Notification::error(self.message.clone()),
        };
        if let Some((type_id, key)) = self.replace_key {
            note = note.id1_dyn(type_id, key);
        }
        if self.detail.is_some() || !self.buttons.is_empty() {
            let message = self.message.clone();
            let detail = self.detail.clone();
            let buttons = self.buttons.clone();
            note =
                note.content(move |_, _, cx| {
                    let muted = cx.theme().muted_foreground;
                    v_flex()
                        .gap_1()
                        .child(message.clone())
                        .when_some(detail.clone(), |this, detail| {
                            this.child(div().text_xs().text_color(muted).child(detail))
                        })
                        .when(!buttons.is_empty(), |this| {
                            this.child(h_flex().gap_2().pt_1().children(
                                buttons.iter().cloned().enumerate().map(
                                    |(ix, (label, handler))| {
                                        Button::new(("toast-button", ix))
                                            .label(label)
                                            .xsmall()
                                            .outline()
                                            .on_click(move |_, window, cx| handler(window, cx))
                                    },
                                ),
                            ))
                        })
                        .into_any_element()
                });
        }
        if let Some((label, handler)) = self.action {
            note = note
                .action(move |_, _, _| {
                    let handler = handler.clone();
                    Button::new("toast-action")
                        .label(label.clone())
                        .small()
                        .on_click(move |_, window, cx| handler(window, cx))
                })
                // `action` turns autohide off; restore the requested value.
                .autohide(self.autohide);
        } else {
            note = note.autohide(self.autohide);
        }
        note
    }
}

/// Every toast of this test app with the window it went to, oldest first
/// (tests: which window said what).
#[cfg(test)]
#[derive(Clone, Default)]
pub struct ToastLog(pub Vec<(AnyWindowHandle, SharedString)>);

#[cfg(test)]
impl Global for ToastLog {}

/// The toasts shown so far (tests).
#[cfg(test)]
pub fn toast_log(cx: &App) -> Vec<(AnyWindowHandle, SharedString)> {
    cx.try_global::<ToastLog>()
        .map(|log| log.0.clone())
        .unwrap_or_default()
}

/// The buttons of the last toast shown (labels and handlers, the action
/// button first). Replaced by every toast, like [`LastToast`].
#[derive(Clone, Default)]
pub struct LastToastActions(pub Vec<(SharedString, ToastHandler)>);

impl Global for LastToastActions {}

/// The button labels of the last toast shown (empty when it had none).
pub fn last_toast_actions(cx: &App) -> Vec<SharedString> {
    cx.try_global::<LastToastActions>()
        .map(|actions| actions.0.iter().map(|(label, _)| label.clone()).collect())
        .unwrap_or_default()
}

/// Press the last toast's button labelled `label` (tests and QA: what a
/// click on it runs). `false` when the last toast has no such button.
#[cfg(any(test, feature = "qa"))]
pub fn trigger_last_toast_action(label: &str, window: &mut Window, cx: &mut App) -> bool {
    let handler = cx.try_global::<LastToastActions>().and_then(|actions| {
        actions
            .0
            .iter()
            .find(|(name, _)| name.as_ref() == label)
            .map(|(_, handler)| handler.clone())
    });
    match handler {
        Some(handler) => {
            handler(window, cx);
            true
        }
        None => false,
    }
}

/// `Notification::id1` for a runtime type id.
trait NotificationIdExt {
    fn id1_dyn(self, type_id: TypeId, key: ElementId) -> Self;
}

impl NotificationIdExt for Notification {
    fn id1_dyn(self, type_id: TypeId, key: ElementId) -> Self {
        // `Notification::id1::<T>` only takes a static type; route the stored
        // TypeId through a keyed element id instead, which keeps the
        // replacement semantics (same kind + key → same id).
        let key: ElementId = ElementId::from((key, SharedString::from(format!("{type_id:?}"))));
        self.id1::<ReplaceKeyed>(key)
    }
}

/// Type marker for keyed replacement ids.
struct ReplaceKeyed;

/// Push an info toast.
pub fn info(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    Toast::info(message).show(window, cx);
}

/// Push a success toast.
pub fn success(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    Toast::success(message).show(window, cx);
}

/// Push a warning toast.
pub fn warning(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    Toast::warning(message).show(window, cx);
}

/// Push an error toast.
pub fn error(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    Toast::error(message).show(window, cx);
}

/// The placeholder of a command whose work package has not landed yet:
/// info toast `<feature> is not available yet`.
pub fn not_available(feature: &str, window: &mut Window, cx: &mut App) {
    Toast::info(format!("{feature} is not available yet"))
        .replacing::<NotAvailable>(SharedString::from(feature.to_string()))
        .show(window, cx);
}

struct NotAvailable;

/// `No conversation selected` (info): a session action found no target.
pub fn no_conversation_selected(window: &mut Window, cx: &mut App) {
    Toast::info("No conversation selected")
        .replacing::<NoTarget>("none")
        .show(window, cx);
}

/// `No conversation open` (info): a viewer action with an empty viewer.
pub fn no_conversation_open(window: &mut Window, cx: &mut App) {
    Toast::info("No conversation open")
        .replacing::<NoTarget>("viewer")
        .show(window, cx);
}

struct NoTarget;

/// `Closed: "<title>" was deleted` (info), after a deletion closed a dialog.
pub fn closed_because_deleted(title: &str, window: &mut Window, cx: &mut App) {
    Toast::info(format!("Closed: \"{title}\" was deleted")).show(window, cx);
}

/// Map a core status text to the in-app copy rules (§12): `...` → `…`,
/// `->` → `→`, and "sessions" (the list objects) → "conversations".
/// Text that leaves the app (stdout, clipboard, debug.log) never goes
/// through this.
pub fn gui_text(core_text: &str) -> String {
    let mut text = core_text
        .replace("keeping existing sessions", "keeping the current list")
        .replace("...", "…")
        .replace("->", "→");
    text = replace_word(&text, "sessions", "conversations");
    text = replace_word(&text, "Sessions", "Conversations");
    text
}

/// Replace whole-word occurrences of `word`.
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(word) {
        let before = rest[..pos].chars().next_back();
        let after = rest[pos + word.len()..].chars().next();
        let boundary_before = before.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let boundary_after = after.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        out.push_str(&rest[..pos]);
        if boundary_before && boundary_after {
            out.push_str(with);
        } else {
            out.push_str(word);
        }
        rest = &rest[pos + word.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::gui_text;

    #[test]
    fn gui_text_follows_the_copy_rules() {
        assert_eq!(gui_text("Loading... 0"), "Loading… 0");
        assert_eq!(
            gui_text("Refreshing sessions..."),
            "Refreshing conversations…"
        );
        assert_eq!(
            gui_text("Refreshed sessions: 3 -> 4"),
            "Refreshed conversations: 3 → 4"
        );
        assert_eq!(
            gui_text("Refresh incomplete; keeping existing sessions"),
            "Refresh incomplete; keeping the current list"
        );
        assert_eq!(
            gui_text("Refresh failed; keeping existing sessions"),
            "Refresh failed; keeping the current list"
        );
        assert_eq!(gui_text("(12 more lines...)"), "(12 more lines…)");
        assert_eq!(gui_text("Session not found: x"), "Session not found: x");
        assert_eq!(gui_text("subsessions stay"), "subsessions stay");
    }
}
