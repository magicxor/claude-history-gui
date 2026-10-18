//! The agent panel's Within tab (DESIGN §5.4, §11.14). WP4.
//!
//! Query, Mode, Top (default 20, `[agent] within_top`), Budget / No budget.
//! The target is the viewer's conversation at the moment of the run — there
//! is no conversation field — and the result set keeps it (`SetInfo`), so a
//! later viewer change shows the stale banner while the rows still reveal in
//! their own conversation.

use std::rc::Rc;

use claude_history_core::agent::AgentRequest;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Styled as _, Window, div,
    rems,
};

use crate::deep_search::controls::{Segment, segmented};
use crate::deep_search::form::{Defaults, ModeChoice, WithinForm, default_placeholder, help};
use crate::deep_search::outline::FormTab;
use crate::deep_search::panel::{AgentPanel, TabRun};
use crate::deep_search::worker::AgentJobKind;
use crate::globals::app_state;
use crate::model::AgentTab;

/// State of the Within tab.
pub struct WithinTab {
    pub form: WithinForm,
    pub query: Entity<InputState>,
    pub top: Entity<InputState>,
    pub budget: Entity<InputState>,
    pub run: TabRun,
}

impl WithinTab {
    pub(super) fn new(window: &mut Window, cx: &mut Context<AgentPanel>) -> Self {
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        WithinTab {
            form: WithinForm::default(),
            query: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search within this conversation…")
            }),
            top: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_placeholder(defaults.within_top))
            }),
            budget: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_placeholder(defaults.budget))
            }),
            run: TabRun::new("agent-within", cx),
        }
    }
}

impl AgentPanel {
    /// Run the search within the viewer's conversation for the query field.
    pub fn run_within(&mut self, cx: &mut Context<Self>) {
        let query = self.within.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(full_ref) = self.viewer_full_ref().map(str::to_string) else {
            self.pending_run = Some(AgentTab::Within);
            return;
        };
        self.within.form.top = self.within.top.read(cx).value().to_string();
        self.within.form.budget = self.within.budget.read(cx).value().to_string();
        match self.within.form.to_args(&full_ref, &query) {
            Ok(args) => {
                self.within.run.error = None;
                self.submit(
                    AgentTab::Within,
                    AgentJobKind::Within,
                    AgentRequest::Within(args),
                    "Searching…",
                    cx,
                );
            }
            Err(error) => {
                self.within.run.error = Some(error);
                cx.notify();
            }
        }
    }

    pub fn set_within_mode(&mut self, mode: ModeChoice, cx: &mut Context<Self>) {
        self.within.form.mode = mode;
        cx.notify();
    }

    pub fn set_within_no_budget(&mut self, no_budget: bool, cx: &mut Context<Self>) {
        self.within.form.no_budget = no_budget;
        cx.notify();
    }

    /// The Within tab's controls.
    pub(super) fn render_within_controls(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        let muted = cx.theme().muted_foreground;
        let running = self.within.run.results.read(cx).is_running();
        let segments: Vec<Segment> = ModeChoice::ALL
            .iter()
            .map(|mode| {
                Segment::new(mode.label(&defaults)).tooltip(match mode {
                    ModeChoice::Default => "Use [agent] mode, then [search] mode, then lexical",
                    ModeChoice::Hybrid => help::HYBRID,
                    _ => help::MODE,
                })
            })
            .collect();
        let current = ModeChoice::ALL
            .iter()
            .position(|mode| *mode == self.within.form.mode)
            .unwrap_or(0);
        let budget = self.render_budget(
            &self.within.budget.clone(),
            self.within.form.no_budget,
            FormTab::Within,
            cx,
        );
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.within.query)
                                .small()
                                .aria_label("Search within"),
                        ),
                    )
                    .child({
                        let view = view.clone();
                        if running {
                            Button::new("within-cancel")
                                .label("Cancel")
                                .small()
                                .outline()
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.cancel_tab(AgentTab::Within, cx)
                                    });
                                })
                        } else {
                            Button::new("within-run")
                                .label("Search")
                                .small()
                                .primary()
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| this.run_within(cx));
                                })
                        }
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(segmented(
                        "within-mode",
                        segments,
                        current,
                        Rc::new({
                            let view = view.clone();
                            move |ix, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.set_within_mode(ModeChoice::ALL[ix], cx)
                                });
                            }
                        }),
                    ))
                    .child(
                        h_flex()
                            .id("within-top")
                            .gap_1()
                            .items_center()
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(help::WITHIN_TOP)
                                    .build(window, cx)
                            })
                            .child(div().text_xs().text_color(muted).child("Top"))
                            .child(
                                div()
                                    // Wide enough for the `Default (20)` placeholder.
                                    .w(rems(6.0))
                                    .child(Input::new(&self.within.top).small().aria_label("Top")),
                            ),
                    ),
            )
            .child(budget)
    }
}

use gpui_kit::InteractiveElement as _;
use gpui_kit::StatefulInteractiveElement as _;
