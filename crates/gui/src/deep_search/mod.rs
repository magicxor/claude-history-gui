//! Deep search: the agent protocol's `search` in the left pane's second tab
//! (PD11, DESIGN §5.4, §11.14) and the viewer's agent panel. WP4.
//!
//! * [`DeepSearchPane`] — query, Mode, the Options disclosure
//!   ([`form::SearchForm`]), results as compact cards ([`results`]) and the
//!   agent text toggle ([`agent_text`]); every run goes through the shared
//!   `agent-worker` ([`worker`]) with Cancel and single-flight.
//! * [`panel::AgentPanel`] — Outline / Within / Read of the viewer's
//!   conversation ([`outline`], [`within`], [`read`]).
//!
//! Key contexts (frozen keymap contract): `DeepSearch` on the tab root
//! (Esc), `DeepSearch > Input` for the form inputs (Enter runs), and
//! `DeepResults` on the focusable results list (`↑`/`↓`, Enter /
//! `secondary-enter` open the hit) — so the form's buttons keep Enter/Space.
//! Session actions with the tab focused act on the selected hit's
//! conversation (§7.8): the window root's fallback asks
//! [`DeepSearchPane::selected_hit_target`]; the hit menu's Open at message /
//! Search within conversation… / Outline / Read range… emit
//! [`DeepSearchEvent::OpenHit`] with the matching `agent_tab`.

pub mod agent_text;
pub mod controls;
pub mod form;
pub mod outline;
pub mod panel;
pub mod read;
pub mod results;
pub mod within;
pub mod worker;

#[cfg(test)]
mod tests;

use std::rc::Rc;

use claude_history_core::agent::AgentRequest;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, rems,
};

use crate::actions::*;
use crate::deep_search::controls::{Segment, checkbox, segmented};
use crate::deep_search::form::{
    Defaults, ModeChoice, ScopeChoice, SearchForm, TimeTexts, default_placeholder, help,
};
use crate::deep_search::results::{ResultsEvent, ResultsView, progress_text};
use crate::deep_search::worker::{
    AgentClient, AgentJob, AgentJobKind, AgentReply, AgentWorkerHandle,
};
use crate::globals::{ActivePane, AppState, app_state};
use crate::keymap::CTX_DEEP;
use crate::model::{AgentTab, ConversationTarget, PaneFocus, RevealRequest};
use crate::persist;
use crate::qa_ext::QaExt as _;
use crate::widgets::time_range::{TimeRangeChoice, time_range_button};

/// Events of the Deep search pane (§7.2).
#[derive(Clone, Debug, PartialEq)]
pub enum DeepSearchEvent {
    /// Open a hit: the target comes from the hit's `AgentConversationInfo`
    /// (source, session id, path). With `agent_tab` the viewer's agent panel
    /// also opens on that tab (hit menu Search within conversation… /
    /// Outline / Read range…, D-029) and takes the focus.
    OpenHit {
        target: ConversationTarget,
        reveal: RevealRequest,
        focus_viewer: bool,
        agent_tab: Option<AgentTab>,
    },
}

/// The Deep search tab.
pub struct DeepSearchPane {
    focus: FocusHandle,
    query: Entity<InputState>,
    top: Entity<InputState>,
    hits_per_conv: Entity<InputState>,
    budget: Entity<InputState>,
    form: SearchForm,
    options_open: bool,
    form_error: Option<String>,
    results: Entity<ResultsView>,
    client: AgentClient,
    /// Generation of the latest submitted run (older replies are dropped).
    generation: u64,
    semantic_paused: bool,
    /// The defaults the placeholders show.
    applied: Defaults,
    /// The query of the latest run (what the results belong to).
    last_query: String,
    _reply_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DeepSearchEvent> for DeepSearchPane {}

impl Focusable for DeepSearchPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl DeepSearchPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search all conversations…"));
        let number = |placeholder: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let top = number(default_placeholder(defaults.top), window, cx);
        let hits_per_conv = number(default_placeholder(defaults.hits_per_conv), window, cx);
        let budget = number(default_placeholder(defaults.budget), window, cx);
        let focus = cx.focus_handle();
        let results = cx.new(|cx| ResultsView::new("deep-results", true, cx));
        let client = AgentWorkerHandle::global(cx).client();
        let replies = client.replies();
        let reply_task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(reply) = replies.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_reply(reply, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let subscriptions = vec![
            cx.on_focus_in(&focus, window, |_, window, cx| {
                ActivePane::set(cx, window.window_handle(), PaneFocus::DeepSearch);
            }),
            // The tab root itself never keeps focus: hand it to the query.
            cx.on_focus(&focus, window, |this, window, cx| {
                this.focus_query(window, cx);
            }),
            cx.subscribe_in(&results, window, |_, _, event: &ResultsEvent, _, cx| {
                if let ResultsEvent::Open {
                    target,
                    reveal,
                    focus_viewer,
                    tab,
                } = event
                {
                    cx.emit(DeepSearchEvent::OpenHit {
                        target: target.clone(),
                        reveal: reveal.clone(),
                        focus_viewer: *focus_viewer,
                        agent_tab: *tab,
                    });
                }
            }),
            // A deletion by any route marks the hits of that conversation
            // (clicks disabled, PD15, §7.2).
            {
                let bus = crate::globals::CorpusBus::entity(cx);
                cx.subscribe(
                    &bus,
                    |this, _, notice: &crate::globals::CorpusNotice, cx| {
                        let crate::globals::CorpusNotice::Deleted(paths) = notice;
                        let timer = crate::globals::BusyTimer::start();
                        this.mark_deleted(paths, cx);
                        crate::globals::record_sub_span(cx, "deep.mark_deleted", timer.stop());
                    },
                )
            },
            // Settings changes (live apply, K12) change the resolved defaults.
            cx.observe_global_in::<AppState>(window, |this, window, cx| {
                this.refresh_defaults(window, cx);
            }),
        ];
        Self {
            focus,
            query,
            top,
            hits_per_conv,
            budget,
            form: SearchForm::default(),
            options_open: app_state(cx).ui.deep_options_open,
            form_error: None,
            results,
            client,
            generation: 0,
            semantic_paused: false,
            applied: defaults,
            last_query: String::new(),
            _reply_task: reply_task,
            _subscriptions: subscriptions,
        }
    }

    // ----- public contract -------------------------------------------------------------

    /// Focus the query input (the tab's primary element).
    pub fn focus_query(&self, window: &mut Window, cx: &mut App) {
        self.query.focus_handle(cx).focus(window, cx);
    }

    /// Whether focus is anywhere inside the Deep search tab.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// The selected hit's conversation (session actions from this pane).
    pub fn selected_hit_target(&self, cx: &App) -> Option<ConversationTarget> {
        let results = self.results.read(cx);
        results
            .selected_reveal()
            .filter(|reveal| !results.is_path_deleted(&reveal.info.path))
            .map(|reveal| reveal.request().0)
    }

    /// Hits of deleted conversations are marked and no longer open or act.
    pub fn mark_deleted(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        self.results
            .update(cx, |results, cx| results.mark_deleted(paths, cx));
        cx.notify();
    }

    /// The semantic gate (§7.6): semantic/hybrid jobs fail fast while paused.
    pub fn pause_semantic(&mut self, paused: bool, cx: &mut Context<Self>) {
        self.semantic_paused = paused;
        AgentWorkerHandle::global(cx).pause_semantic(paused);
        cx.notify();
    }

    // ----- form ------------------------------------------------------------------------

    pub fn form(&self) -> &SearchForm {
        &self.form
    }

    pub fn results(&self) -> &Entity<ResultsView> {
        &self.results
    }

    pub fn set_mode(&mut self, mode: ModeChoice, cx: &mut Context<Self>) {
        self.form.mode = mode;
        cx.notify();
    }

    pub fn set_scope(&mut self, scope: ScopeChoice, cx: &mut Context<Self>) {
        // The Workspace scope needs a workspace (D-015).
        if scope == ScopeChoice::Workspace && app_state(cx).workspace.is_none() {
            return;
        }
        self.form.scope = scope;
        cx.notify();
    }

    pub fn set_flat(&mut self, flat: bool, cx: &mut Context<Self>) {
        self.form.flat = flat;
        cx.notify();
    }

    pub fn set_all_hits(&mut self, all_hits: bool, cx: &mut Context<Self>) {
        self.form.all_hits = all_hits;
        cx.notify();
    }

    pub fn set_no_budget(&mut self, no_budget: bool, cx: &mut Context<Self>) {
        self.form.no_budget = no_budget;
        cx.notify();
    }

    pub fn set_time(&mut self, time: TimeTexts, cx: &mut Context<Self>) {
        self.form.time = time;
        cx.notify();
    }

    pub fn set_options_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.options_open = open;
        persist::remember_deep_options(open, cx);
        cx.notify();
    }

    pub fn options_open(&self) -> bool {
        self.options_open
    }

    /// Replace the query text (no run).
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |query, cx| query.set_value(text, window, cx));
    }

    /// Put text into one of the number fields (tests).
    pub fn set_field(&mut self, field: Field, text: &str, window: &mut Window, cx: &mut App) {
        let input = match field {
            Field::Top => &self.top,
            Field::HitsPerConv => &self.hits_per_conv,
            Field::Budget => &self.budget,
        };
        input.update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Read the number fields into the form.
    fn sync_form(&mut self, cx: &App) {
        self.form.top = self.top.read(cx).value().to_string();
        self.form.hits_per_conv = self.hits_per_conv.read(cx).value().to_string();
        self.form.budget = self.budget.read(cx).value().to_string();
    }

    fn refresh_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        if defaults != self.applied {
            for (input, value) in [
                (&self.top, defaults.top),
                (&self.hits_per_conv, defaults.hits_per_conv),
                (&self.budget, defaults.budget),
            ] {
                let placeholder = default_placeholder(value);
                input.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx)
                });
            }
            self.applied = defaults;
        }
        // The Workspace scope is gone when the workspace is.
        if self.form.scope == ScopeChoice::Workspace && app_state(cx).workspace.is_none() {
            self.form.scope = ScopeChoice::Default;
        }
        cx.notify();
    }

    // ----- running -----------------------------------------------------------------------

    /// Run the search for the query field (Enter, the Search button).
    pub fn run(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            return;
        }
        self.sync_form(cx);
        let args = match self.form.to_args(&query) {
            Ok(args) => args,
            Err(error) => {
                self.form_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.form_error = None;
        self.last_query = query;
        let state = app_state(cx);
        let job = AgentJob {
            kind: AgentJobKind::Search,
            request: AgentRequest::Search(args),
            env: state.env_with_workspace(),
            config: state.config.effective.clone(),
        };
        self.generation = self.client.submit(job);
        self.results
            .update(cx, |results, cx| results.set_running("Searching…", cx));
        cx.notify();
    }

    /// Cancel the running search (Cancel button, Esc).
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.results.read(cx).is_running() {
            return;
        }
        self.client.cancel(AgentJobKind::Search);
        // Anything the worker still answers for the cancelled run is stale.
        self.generation = self.generation.wrapping_add(u64::MAX / 2);
        self.results
            .update(cx, |results, cx| results.set_cancelled(cx));
        cx.notify();
    }

    pub fn is_running(&self, cx: &App) -> bool {
        self.results.read(cx).is_running()
    }

    fn on_reply(&mut self, reply: AgentReply, window: &mut Window, cx: &mut Context<Self>) {
        match reply {
            AgentReply::Progress {
                kind: AgentJobKind::Search,
                generation,
                notice,
            } if generation == self.generation => {
                if let Some(text) = progress_text(&notice) {
                    self.results
                        .update(cx, |results, cx| results.set_progress(text, cx));
                }
            }
            AgentReply::Done {
                kind: AgentJobKind::Search,
                generation,
                response,
            } if generation == self.generation => {
                self.results.update(cx, |results, cx| {
                    results.set_response(response, &Default::default(), window, cx)
                });
                cx.notify();
            }
            _ => {}
        }
    }

    // ----- actions -----------------------------------------------------------------------

    fn on_run(&mut self, _: &RunDeepSearch, _: &mut Window, cx: &mut Context<Self>) {
        self.run(cx);
    }

    fn on_cancel(&mut self, _: &CancelDeepSearch, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    fn on_toggle_text(&mut self, _: &ToggleAgentText, window: &mut Window, cx: &mut Context<Self>) {
        self.results
            .update(cx, |results, cx| results.toggle_text(window, cx));
    }

    /// Down in the query moves into the results (the list keys then apply).
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "down"
            || event.keystroke.modifiers.modified()
            || !self.query.focus_handle(cx).is_focused(window)
        {
            return;
        }
        let results = self.results.clone();
        if results.read(cx).selectable_count() > 0 {
            results.update(cx, |results, cx| {
                if results.selected().is_none() {
                    results.select(0, cx);
                }
                results.focus_rows(window, cx);
            });
            cx.stop_propagation();
        }
    }

    fn on_escape(&mut self, _: &DeepSearchEscape, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running(cx) {
            self.cancel(cx);
        } else if !self.query.read(cx).value().is_empty() {
            self.query
                .update(cx, |query, cx| query.set_value("", window, cx));
        } else {
            window.dispatch_action(Box::new(FocusConversations), cx);
        }
    }

    // ----- probes ------------------------------------------------------------------------

    /// Test and QA probes (§15.3 keys `deep.*`, and `deep.agent.*` for the
    /// main window's agent panel).
    #[cfg(any(test, feature = "qa"))]
    pub fn qa_probe(&self, key: &str, cx: &App) -> Option<String> {
        if let Some(rest) = key.strip_prefix("deep.agent.") {
            let viewer = crate::globals::main_panes(cx).viewer()?;
            let panel = viewer.read(cx).agent_panel().clone();
            return panel.read(cx).qa_probe(&format!("agent.{rest}"), cx);
        }
        let results = self.results.read(cx);
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        Some(match key {
            "deep.hits" => results.counts().1.to_string(),
            "deep.groups" => results.counts().0.to_string(),
            "deep.mode" => self.form.mode.name().to_string(),
            "deep.scope" => self.form.scope.name().to_string(),
            "deep.mode_label" => self.form.mode.label(&defaults),
            "deep.scope_label" => self.form.scope.label(&defaults),
            "deep.query" => self.query.read(cx).value().to_string(),
            "deep.running" => results.is_running().to_string(),
            "deep.status" => results.status_text(),
            "deep.error" => results
                .error()
                .map(|error| error.text.clone())
                .or_else(|| self.form_error.clone())
                .unwrap_or_default(),
            "deep.warnings" => results.warnings_banner().unwrap_or_default().to_string(),
            "deep.uncached" => results.uncached_text().unwrap_or_default().to_string(),
            "deep.selected" => results
                .selected()
                .map(|ix| ix.to_string())
                .unwrap_or_else(|| "none".to_string()),
            "deep.options" => self.options_open.to_string(),
            "deep.text_shown" => results.text_view().is_shown().to_string(),
            "deep.text" => results.text_view().text().to_string(),
            "deep.stats" => match results.body() {
                results::Body::Search(model) => model.stats_line(),
                _ => String::new(),
            },
            "deep.first_title" => match results.body() {
                results::Body::Search(model) => model
                    .rows
                    .iter()
                    .map(|row| match row {
                        results::SearchRow::Group(group) => group.title.clone(),
                        results::SearchRow::Hit(hit) => hit.title.clone(),
                    })
                    .next()
                    .unwrap_or_default(),
                _ => String::new(),
            },
            // The text the results view renders in its mode badge.
            "deep.mode_badge" => results.mode_badge_text().unwrap_or_default(),
            "deep.exact_forced" => match results.body() {
                results::Body::Search(model) => model.exact_forced.to_string(),
                _ => "false".to_string(),
            },
            "deep.top_placeholder" => default_placeholder(self.applied.top),
            "deep.top_label" => top_label(self.form.flat).to_string(),
            "deep.paused" => self.semantic_paused.to_string(),
            _ => return None,
        })
    }
}

/// A number field of the Options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Top,
    HitsPerConv,
    Budget,
}

impl Render for DeepSearchPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let border = theme.border;
        let state = app_state(cx);
        let defaults = Defaults::resolve(&state.config.effective);
        let has_workspace = state.workspace.is_some();
        let running = self.results.read(cx).is_running();
        let idle = {
            let results = self.results.read(cx);
            matches!(results.body(), results::Body::None)
                && matches!(results.state(), results::RunState::Idle)
        };

        // Query row.
        let query_row = h_flex()
            .px_2()
            .pt_2()
            .pb_1()
            .gap_2()
            .items_center()
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.query)
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small())
                        .aria_label("Search all conversations"),
                ),
            )
            .child(div().qa_id("deep:search").child({
                let view = view.clone();
                if running {
                    Button::new("deep-cancel")
                        .label("Cancel")
                        .small()
                        .outline()
                        .tooltip("Cancel the running search")
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.cancel(cx));
                        })
                } else {
                    Button::new("deep-search")
                        .label("Search")
                        .small()
                        .primary()
                        .tooltip("Run the search")
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.run(cx));
                        })
                }
            }));

        // Mode row with the Options disclosure.
        let mode_segments: Vec<Segment> = ModeChoice::ALL
            .iter()
            .map(|mode| {
                Segment::new(mode.label(&defaults)).tooltip(match mode {
                    ModeChoice::Default => "Use [agent] mode, then [search] mode, then lexical",
                    ModeChoice::Hybrid => help::HYBRID,
                    _ => help::MODE,
                })
            })
            .collect();
        let current_mode = ModeChoice::ALL
            .iter()
            .position(|mode| *mode == self.form.mode)
            .unwrap_or(0);
        let mode_row = h_flex()
            .px_2()
            .pb_1()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(segmented(
                "deep-mode",
                mode_segments,
                current_mode,
                Rc::new({
                    let view = view.clone();
                    move |ix, _, cx| {
                        view.update(cx, |this, cx| this.set_mode(ModeChoice::ALL[ix], cx));
                    }
                }),
            ))
            .child(div().flex_1())
            .child(div().qa_id("deep:options").child({
                let view = view.clone();
                let open = self.options_open;
                Button::new("deep-options")
                    .label("Options")
                    .icon(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall()
                    .ghost()
                    .tooltip("Scope, time, limits and budget")
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            let open = !this.options_open;
                            this.set_options_open(open, cx)
                        });
                    })
            }));

        // Options.
        let options =
            self.options_open.then(|| {
                let scope_segments: Vec<Segment> = ScopeChoice::ALL
                    .iter()
                    .map(|scope| {
                        Segment::new(scope.label(&defaults))
                            .tooltip(if *scope == ScopeChoice::Workspace && !has_workspace {
                                "Choose a workspace to filter by project"
                            } else {
                                scope.tooltip()
                            })
                            .disabled(*scope == ScopeChoice::Workspace && !has_workspace)
                    })
                    .collect();
                let current_scope = ScopeChoice::ALL
                    .iter()
                    .position(|scope| *scope == self.form.scope)
                    .unwrap_or(0);
                let time_choice = TimeRangeChoice {
                    filter: Default::default(),
                    label: self.form.time.label().map(SharedString::from),
                };
                let flat = self.form.flat;
                let top_label = top_label(flat);
                v_flex()
                    .id("deep-options-body")
                    .mx_2()
                    .mb_1()
                    .p_2()
                    .gap_2()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .flex_wrap()
                            .child(div().text_xs().text_color(muted).child("Scope"))
                            .child(segmented(
                                "deep-scope",
                                scope_segments,
                                current_scope,
                                Rc::new({
                                    let view = view.clone();
                                    move |ix, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.set_scope(ScopeChoice::ALL[ix], cx)
                                        });
                                    }
                                }),
                            ))
                            .child(div().text_xs().text_color(muted).child("Time"))
                            .child(time_range_button("deep-time", time_choice, {
                                let view = view.clone();
                                move |choice, _, cx| {
                                    let texts = TimeTexts::from_label(choice.label.as_deref());
                                    view.update(cx, |this, cx| this.set_time(texts, cx));
                                }
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .flex_wrap()
                            .child(
                                h_flex()
                                    .id("deep-top")
                                    .gap_1()
                                    .items_center()
                                    .tooltip(tooltip_of(help::TOP))
                                    .child(div().text_xs().text_color(muted).child(top_label))
                                    .child(
                                        div()
                                            .w(rems(6.0))
                                            .child(Input::new(&self.top).small().aria_label("Top")),
                                    ),
                            )
                            .child(div().text_xs().text_color(muted).child("Layout"))
                            .child(segmented(
                                "deep-layout",
                                vec![
                                    Segment::new("Grouped").tooltip("Group hits by conversation"),
                                    Segment::new("Flat").tooltip(help::FLAT),
                                ],
                                usize::from(flat),
                                Rc::new({
                                    let view = view.clone();
                                    move |ix, _, cx| {
                                        view.update(cx, |this, cx| this.set_flat(ix == 1, cx));
                                    }
                                }),
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .flex_wrap()
                            .child(
                                h_flex()
                                    .id("deep-hits-per-conv")
                                    .gap_1()
                                    .items_center()
                                    .tooltip(tooltip_of(help::HITS_PER_CONV))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("Hits per conversation"),
                                    )
                                    .child(
                                        div().w(rems(6.0)).child(
                                            Input::new(&self.hits_per_conv)
                                                .small()
                                                .aria_label("Hits per conversation"),
                                        ),
                                    ),
                            )
                            .child(checkbox(
                                "deep-all-hits",
                                "Show duplicate hits",
                                self.form.all_hits,
                                false,
                                Some(help::ALL_HITS.into()),
                                Rc::new({
                                    let view = view.clone();
                                    move |checked, _, cx| {
                                        view.update(cx, |this, cx| this.set_all_hits(checked, cx));
                                    }
                                }),
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .flex_wrap()
                            .child(
                                h_flex()
                                    .id("deep-budget")
                                    .gap_1()
                                    .items_center()
                                    .tooltip(tooltip_of(help::BUDGET))
                                    .child(div().text_xs().text_color(muted).child("Budget"))
                                    .child(div().w(rems(8.0)).child(
                                        Input::new(&self.budget).small().aria_label("Budget"),
                                    ))
                                    .child(div().text_xs().text_color(muted).child("chars")),
                            )
                            .child(checkbox(
                                "deep-no-budget",
                                "No budget",
                                self.form.no_budget,
                                false,
                                Some(help::NO_BUDGET.into()),
                                Rc::new({
                                    let view = view.clone();
                                    move |checked, _, cx| {
                                        view.update(cx, |this, cx| this.set_no_budget(checked, cx));
                                    }
                                }),
                            )),
                    )
            });

        v_flex()
            .id("deep-search")
            .key_context(CTX_DEEP)
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .on_action(cx.listener(Self::on_run))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_cancel))
            .on_action(cx.listener(Self::on_toggle_text))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(query_row)
            .child(mode_row)
            .when_some(options, |this, options| this.child(options))
            .when_some(self.form_error.clone(), |this, error| {
                this.child(
                    div()
                        .id("deep-form-error")
                        .px_2()
                        .pb_1()
                        .text_xs()
                        .text_color(danger)
                        .child(format!("⚠ {error}")),
                )
            })
            .child(div().flex_1().min_h_0().border_t_1().border_color(border).child(
                if idle {
                    Empty::new()
                        .header(
                            EmptyHeader::new()
                                .media(EmptyMedia::new().child(IconName::Search))
                                .title(EmptyTitle::new().child("Search all conversations"))
                                .description(EmptyDescription::new().child(
                                    "Lexical, semantic, exact or hybrid search across transcripts, with evidence.",
                                )),
                        )
                        .into_any_element()
                } else {
                    self.results.clone().into_any_element()
                },
            ))
    }
}

/// The label of the Top field (D-008): flat output counts message hits.
fn top_label(flat: bool) -> &'static str {
    if flat { "Top · message hits" } else { "Top" }
}

/// A tooltip builder for plain text.
fn tooltip_of(text: &'static str) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static {
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text).build(window, cx)
}
