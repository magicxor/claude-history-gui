//! The agent panel's Outline tab (DESIGN §5.4, §11.14). WP4.
//!
//! `AgentOutlineArgs { conversation, output }` for the viewer's conversation;
//! re-runs automatically when the viewer target changes while the tab is
//! visible (`AgentPanel::maybe_run_outline`). Up to 20 visible messages are
//! one row each; longer conversations show segments of 10 messages
//! (expandable) — core decides, the rows render what it returns.

use claude_history_core::agent::AgentRequest;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::InputState;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Styled as _, Window,
};

use crate::deep_search::form::{Defaults, OutputForm, default_placeholder};
use crate::deep_search::panel::{AgentPanel, TabRun};
use crate::deep_search::worker::AgentJobKind;
use crate::globals::app_state;

/// State of the Outline tab.
pub struct OutlineTab {
    pub form: OutputForm,
    pub budget: Entity<InputState>,
    pub run: TabRun,
}

impl OutlineTab {
    pub(super) fn new(window: &mut Window, cx: &mut Context<AgentPanel>) -> Self {
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        OutlineTab {
            form: OutputForm::default(),
            budget: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_placeholder(defaults.budget))
            }),
            run: TabRun::new("agent-outline", cx),
        }
    }
}

impl AgentPanel {
    /// Run the outline of the viewer's conversation.
    pub fn run_outline(&mut self, cx: &mut Context<Self>) {
        let Some(full_ref) = self.viewer_full_ref().map(str::to_string) else {
            self.pending_run = Some(crate::model::AgentTab::Outline);
            return;
        };
        self.outline.form.budget = self.outline.budget.read(cx).value().to_string();
        match self.outline.form.to_outline_args(&full_ref) {
            Ok(args) => {
                self.outline.run.error = None;
                self.submit(
                    crate::model::AgentTab::Outline,
                    AgentJobKind::Outline,
                    AgentRequest::Outline(args),
                    "Outlining…",
                    cx,
                );
            }
            Err(error) => {
                self.outline.run.error = Some(error);
                cx.notify();
            }
        }
    }

    /// Re-run the outline when the visible tab shows another conversation's.
    pub(super) fn maybe_run_outline(&mut self, cx: &mut Context<Self>) {
        if !(self.visible && self.tab == crate::model::AgentTab::Outline) {
            return;
        }
        let Some(viewer) = self
            .viewer_target
            .as_ref()
            .map(|target| target.path.clone())
        else {
            return;
        };
        if self.viewer_full_ref().is_none() {
            return;
        }
        let fresh = self
            .outline
            .run
            .set
            .as_ref()
            .is_some_and(|set| crate::globals::same_path(&set.target.path, &viewer));
        let running = self
            .outline
            .run
            .running
            .as_ref()
            .is_some_and(|set| crate::globals::same_path(&set.target.path, &viewer));
        if !fresh && !running {
            self.run_outline(cx);
        }
    }

    /// The Outline tab's controls (visibility toggles, budget, Run).
    pub(super) fn render_outline_controls(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let outline_form = self.outline.form.clone();
        let flags = self.render_flags(FormTab::Outline, cx);
        let budget = self.render_budget(
            &self.outline.budget.clone(),
            outline_form.no_budget,
            FormTab::Outline,
            cx,
        );
        let running = self.outline.run.results.read(cx).is_running();
        let button = if running {
            let view = view.clone();
            Button::new("outline-cancel")
                .label("Cancel")
                .xsmall()
                .outline()
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.cancel_tab(crate::model::AgentTab::Outline, cx)
                    });
                })
        } else {
            Button::new("outline-run")
                .label("Run")
                .xsmall()
                .outline()
                .tooltip("Outline the conversation again")
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.run_outline(cx));
                })
        };
        v_flex()
            .gap_1()
            .child(flags)
            .child(h_flex().gap_2().items_center().child(budget).child(button))
    }
}

/// Which tab's output form a toggle edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormTab {
    Outline,
    Within,
    Read,
}
