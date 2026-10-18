//! Read-only text views for large plain text: agent text output, Tools
//! reports, the debug log (DESIGN §5.4, §5.13, §5.14). Frozen: written by WP0.
//!
//! Backed by a rope-based, virtualized `Textarea` (selectable, copyable,
//! searchable with Ctrl+F), never a single `StyledText`.
//!
//! Live text (the Debug log's Messages tab refreshes every second) uses
//! [`ReadOnlyText::append_text`] / [`ReadOnlyText::update_text`], which keep
//! the reader's scroll position and selection; [`ReadOnlyText::set_text`]
//! starts over at the top (integrator, Phase C; WP5 request).

use std::ops::Range;

use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, Pixels, Point, SharedString, Window, px,
};

/// A read-only, selectable, searchable text area.
#[derive(Clone)]
pub struct ReadOnlyText {
    state: Entity<TextareaState>,
}

impl ReadOnlyText {
    /// Create the state (call once, e.g. in a dialog's constructor; never in a
    /// render or dialog builder).
    pub fn new<T: 'static>(window: &mut Window, cx: &mut Context<T>) -> Self {
        Self::new_in(window, cx)
    }

    /// [`ReadOnlyText::new`] from an `App` context.
    pub fn new_in(window: &mut Window, cx: &mut App) -> Self {
        let state = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).searchable(true);
            state.set_readonly(true, cx);
            state
        });
        Self { state }
    }

    /// Replace the text (no change events; the view scrolls to the start).
    pub fn set_text(&self, text: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        let text: SharedString = text.into();
        self.state.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
    }

    /// Add `text` at the end. The view does not move: the scroll position and
    /// the selection stay where the reader left them.
    pub fn append_text(&self, text: &str, window: &mut Window, cx: &mut App) {
        if text.is_empty() {
            return;
        }
        self.state.update(cx, |state, cx| {
            let (selection, scroll) = (state.selected_range(), state.scroll_offset());
            let end = state.text().len();
            state.set_selected_range(end..end, cx);
            state.insert(text.to_string(), window, cx);
            restore_view(state, selection, scroll, cx);
        });
    }

    /// Show `text` without moving the view: a text that continues the current
    /// one is appended ([`Self::append_text`]); any other text replaces it
    /// with the scroll position kept and the selection clamped to the new
    /// length (the view is clamped to the content at the next layout).
    pub fn update_text(&self, text: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        let text: SharedString = text.into();
        let current = self.text(cx);
        if let Some(suffix) = text.strip_prefix(current.as_ref()) {
            self.append_text(suffix, window, cx);
            return;
        }
        // A ring buffer dropped whole lines from the front and appended new
        // ones: the old text minus those lines is a prefix of the new text.
        let dropped = dropped_front_bytes(current.as_ref(), text.as_ref());
        self.state.update(cx, |state, cx| {
            let (selection, mut scroll) = (state.selected_range(), state.scroll_offset());
            let line_height = state.line_height();
            state.set_value(text.to_string(), window, cx);
            let len = state.text().len();
            let selection = match dropped {
                Some(dropped) => {
                    // The same text stays selected; a dropped one collapses to the top.
                    let shift = |at: usize| at.saturating_sub(dropped).min(len);
                    shift(selection.start)..shift(selection.end)
                }
                None => selection.start.min(len)..selection.end.min(len),
            };
            if let (Some(dropped), Some(line_height)) = (dropped, line_height) {
                let lines = current[..dropped].matches('\n').count() as f32;
                scroll.y = (scroll.y + line_height * lines).min(px(0.));
            }
            restore_view(state, selection, scroll, cx);
        });
    }

    /// The current text.
    pub fn text(&self, cx: &App) -> SharedString {
        self.state.read(cx).value()
    }

    /// The selected byte range (empty: the caret).
    pub fn selected_range(&self, cx: &App) -> Range<usize> {
        self.state.read(cx).selected_range()
    }

    /// The scroll offset of the view.
    pub fn scroll_offset(&self, cx: &App) -> Point<Pixels> {
        self.state.read(cx).scroll_offset()
    }

    pub fn state(&self) -> &Entity<TextareaState> {
        &self.state
    }

    /// Focus the text (keyboard selection, Ctrl+A, Ctrl+C, Ctrl+F).
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.focus_handle(cx).focus(window, cx);
    }

    /// The element; size it with `.h(..)`/`.flex_1()` at the call site.
    pub fn element(&self, label: impl Into<SharedString>) -> Textarea {
        Textarea::new(&self.state).readonly(true).aria_label(label)
    }
}

/// The byte length of the leading whole lines of `old` that are gone from
/// `new`, when what is left of `old` starts `new` (a full ring buffer).
fn dropped_front_bytes(old: &str, new: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(newline) = old[start..].find('\n') {
        start += newline + 1;
        if start < old.len() && new.starts_with(&old[start..]) {
            return Some(start);
        }
    }
    None
}

/// Put the selection back and pin the scroll position for the next layout
/// (it wins over the caret-follow scroll a selection change asks for).
fn restore_view(
    state: &mut TextareaState,
    selection: Range<usize>,
    scroll: Point<Pixels>,
    cx: &mut Context<TextareaState>,
) {
    state.set_selected_range(selection, cx);
    state.set_scroll_offset(scroll, cx);
}

#[cfg(test)]
mod ring_tests {
    use super::dropped_front_bytes;

    #[test]
    fn dropped_lines_are_found_at_line_boundaries() {
        let old = "a\nb\nc\n";
        assert_eq!(dropped_front_bytes(old, "b\nc\nd\n"), Some(2));
        assert_eq!(dropped_front_bytes(old, "c\nd\ne\n"), Some(4));
        assert_eq!(dropped_front_bytes(old, "x\ny\n"), None);
        assert_eq!(dropped_front_bytes("", "x\n"), None);
    }
}
