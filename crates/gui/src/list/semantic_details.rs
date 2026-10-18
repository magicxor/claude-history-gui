//! The semantic result details dialog (Ctrl+S, DESIGN §5.19): a snapshot of
//! the selected row's `SemanticResultMetadata` taken when it opens (ADD C5:
//! a result set arriving while the dialog is open does not change it),
//! exactly the TUI lines. Any key closes it; the dialog holds its
//! conversation in `OpenTargets` so a deletion closes it. WP1.

use std::cell::Cell;
use std::rc::Rc;

use claude_history_core::browser::SemanticResultMetadata;
use claude_history_core::display_format::{sanitize_preview, semantic_rationale_label};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window, div,
};

use crate::globals::OpenTargets;
use crate::model::ConversationTarget;

/// The dialog content's key context.
pub const CTX_SEMANTIC_DETAILS: &str = "SemanticDetails";

/// The lines of the dialog, as the TUI shows them (labels included).
pub fn detail_lines(metadata: &SemanticResultMetadata) -> Vec<String> {
    let score = &metadata.score_breakdown;
    let explanation = &metadata.explanation;
    vec![
        format!(
            "hybrid {:.2}  semantic {:.2}  lexical {:.2}",
            score.hybrid, score.semantic, score.lexical
        ),
        format!(
            "rationale {}  quality {}",
            semantic_rationale_label(metadata),
            explanation.quality_label
        ),
        format!(
            "chunk {} #{}",
            explanation.chunk.session, explanation.chunk.chunk_index
        ),
        format!(
            "terms {}",
            if explanation.matched_terms.is_empty() {
                "(none)".to_string()
            } else {
                explanation.matched_terms.join(", ")
            }
        ),
        format!(
            "preview {}",
            sanitize_preview(&explanation.evidence_preview)
        ),
        "Esc close".to_string(),
    ]
}

/// The dialog's content (key context `SemanticDetails`): lines fixed at open.
pub struct DetailsContent {
    lines: Vec<SharedString>,
    focus: FocusHandle,
    registration: Rc<Cell<Option<u64>>>,
}

impl DetailsContent {
    /// The snapshot lines (tests).
    pub fn lines(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.to_string()).collect()
    }
}

impl Focusable for DetailsContent {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DetailsContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let registration = self.registration.clone();
        let last = self.lines.len().saturating_sub(1);
        v_flex()
            .id("semantic-details")
            .key_context(CTX_SEMANTIC_DETAILS)
            .track_focus(&self.focus)
            .gap_1()
            .text_sm()
            // Any key closes (TUI parity).
            .on_key_down(move |_, window, cx| {
                if let Some(id) = registration.take() {
                    OpenTargets::unregister(cx, id);
                }
                window.close_dialog(cx);
            })
            .children(self.lines.iter().enumerate().map(|(ix, line)| {
                div()
                    .when_else(ix == last, |d| d.text_color(muted), |d| d)
                    .child(line.clone())
            }))
    }
}

use gpui_kit::prelude::FluentBuilder as _;

/// The most recently opened details dialog (tests).
#[cfg(test)]
pub struct LastDetails(pub gpui_kit::WeakEntity<DetailsContent>);

#[cfg(test)]
impl gpui_kit::Global for LastDetails {}

/// Open the dialog for `lines` (a snapshot) of `target`.
pub fn open(lines: Vec<String>, target: ConversationTarget, window: &mut Window, cx: &mut App) {
    let registration = Rc::new(Cell::new(None));
    let content = cx.new(|cx| DetailsContent {
        lines: lines.into_iter().map(SharedString::from).collect(),
        focus: cx.focus_handle(),
        registration: registration.clone(),
    });
    let id = OpenTargets::register(
        cx,
        window.window_handle(),
        target.path.clone(),
        target.title.clone(),
        Box::new(|window, cx| window.close_dialog(cx)),
    );
    registration.set(Some(id));
    // The `dialog` probe names it (integrator, Phase C: semantic-toggle.qa).
    crate::workspace::startup::register_dialog(cx, DIALOG_ID);
    #[cfg(test)]
    cx.set_global(LastDetails(content.downgrade()));
    let first_render = Rc::new(Cell::new(true));
    let width = window.rem_size() * 36.0;
    let on_close = registration.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        if first_render.replace(false) {
            content.read(cx).focus.clone().focus(window, cx);
        }
        let on_close = on_close.clone();
        dialog
            .title("Semantic result")
            .width(width)
            .child(content.clone())
            .on_close(move |_, _, cx| {
                if let Some(id) = on_close.take() {
                    OpenTargets::unregister(cx, id);
                }
                crate::ops::copy::clear_dialog(cx, DIALOG_ID);
            })
    });
}

/// The QA / probe id of the dialog.
pub const DIALOG_ID: &str = "semantic-details";
