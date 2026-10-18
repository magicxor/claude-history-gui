//! "Show agent text output": `AgentResponse::text()` verbatim in a read-only
//! text view (`widgets::ReadOnlyText`) with Copy, plus the notices of
//! `stderr` (the exact upstream lines) in a second small view. WP4.
//!
//! The first view always holds exactly `response.text()` — the CLI's stdout
//! on success, the error envelope on failure — so "what the agent sees" and
//! what Copy puts on the clipboard are the same bytes.

use claude_history_core::agent::AgentResponse;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, rems,
};

use crate::services::services;
use crate::widgets::ReadOnlyText;

/// The notices part of `stderr`: every notice line on success, the lines
/// before the error envelope on failure (the envelope itself is the text).
pub fn stderr_part(response: &AgentResponse) -> String {
    if response.is_success() {
        response.stderr.clone()
    } else {
        let envelope = response.error_text();
        response.stderr[..response.stderr.len() - envelope.len()].to_string()
    }
}

/// The toggle's state and the two text views of one result set.
pub struct AgentTextView {
    shown: bool,
    text: Option<ReadOnlyText>,
    stderr: Option<ReadOnlyText>,
    /// What the views hold (or will hold once created).
    content: String,
    notices: String,
}

impl Default for AgentTextView {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentTextView {
    pub fn new() -> Self {
        Self {
            shown: false,
            text: None,
            stderr: None,
            content: String::new(),
            notices: String::new(),
        }
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The text of the current response (what Copy puts on the clipboard).
    pub fn text(&self) -> &str {
        &self.content
    }

    /// The notice lines of the current response.
    pub fn notices(&self) -> &str {
        &self.notices
    }

    /// What the text view displays right now (tests, probes).
    pub fn displayed(&self, cx: &App) -> Option<SharedString> {
        self.text.as_ref().map(|view| view.text(cx))
    }

    /// What the stderr view displays right now (tests, probes).
    pub fn displayed_notices(&self, cx: &App) -> Option<SharedString> {
        self.stderr.as_ref().map(|view| view.text(cx))
    }

    /// Show or hide the text view (created on first use).
    pub fn set_shown(&mut self, shown: bool, window: &mut Window, cx: &mut App) {
        self.shown = shown;
        if shown {
            self.ensure_views(window, cx);
        }
    }

    /// A new response (`None` = nothing to show).
    pub fn set_response(
        &mut self,
        response: Option<&AgentResponse>,
        window: &mut Window,
        cx: &mut App,
    ) {
        match response {
            Some(response) => {
                self.content = response.text().to_string();
                self.notices = stderr_part(response);
            }
            None => {
                self.content.clear();
                self.notices.clear();
            }
        }
        if self.shown {
            self.ensure_views(window, cx);
        }
        if let Some(view) = &self.text {
            view.set_text(self.content.clone(), window, cx);
        }
        if let Some(view) = &self.stderr {
            view.set_text(self.notices.clone(), window, cx);
        }
    }

    fn ensure_views(&mut self, window: &mut Window, cx: &mut App) {
        if self.text.is_none() {
            let view = ReadOnlyText::new_in(window, cx);
            view.set_text(self.content.clone(), window, cx);
            self.text = Some(view);
        }
        if self.stderr.is_none() {
            let view = ReadOnlyText::new_in(window, cx);
            view.set_text(self.notices.clone(), window, cx);
            self.stderr = Some(view);
        }
    }

    /// Copy the exact agent text to the clipboard (through the services).
    pub fn copy(&self, cx: &mut App) {
        services(cx).clipboard_write(self.content.clone(), cx);
    }

    /// The text views (when shown): the agent text, and the stderr notices
    /// below it when there are any.
    pub fn body(&self, id: &'static str, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let mut column = v_flex().id(id).size_full().min_h_0().gap_1();
        if let Some(view) = &self.text {
            column = column.child(
                div()
                    .flex_1()
                    .min_h(rems(6.0))
                    .child(view.element("Agent text output").size_full()),
            );
        }
        if !self.notices.is_empty()
            && let Some(view) = &self.stderr
        {
            column = column.child(
                v_flex()
                    .flex_none()
                    .gap_0p5()
                    .border_t_1()
                    .border_color(border)
                    .pt_1()
                    .child(div().text_xs().text_color(muted).child("stderr (notices)"))
                    .child(
                        div()
                            .h(rems(4.0))
                            .child(view.element("Agent stderr notices").size_full()),
                    ),
            );
        }
        column
    }
}

/// The footer of a result view: `[Show agent text output] [Copy]`.
pub fn toggle_bar<T: 'static>(
    id: &'static str,
    shown: bool,
    enabled: bool,
    on_toggle: impl Fn(&mut T, bool, &mut Window, &mut gpui_kit::Context<T>) + 'static + Clone,
    on_copy: impl Fn(&mut T, &mut Window, &mut gpui_kit::Context<T>) + 'static,
    cx: &mut gpui_kit::Context<T>,
) -> impl IntoElement {
    use gpui_kit::component::Disableable as _;
    h_flex()
        .id(id)
        .gap_2()
        .items_center()
        .child(
            Checkbox::new(SharedString::from(format!("{id}-toggle")))
                .label("Show agent text output")
                .checked(shown)
                .disabled(!enabled)
                .small()
                .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                    on_toggle(this, *checked, window, cx)
                })),
        )
        .child(
            Button::new(SharedString::from(format!("{id}-copy")))
                .label("Copy")
                .xsmall()
                .outline()
                .disabled(!enabled)
                .tooltip("Copy the agent text output")
                .on_click(cx.listener(move |this, _, window, cx| on_copy(this, window, cx))),
        )
}
