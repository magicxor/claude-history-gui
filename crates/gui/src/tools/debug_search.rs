//! Tools → Debug search… (DESIGN §5.12, §11.24; T-011, C-014). WP5.
//!
//! The corpus is a fresh `load_sorted_conversations` with the launch time
//! filter (never the list's corpus, which may hold UUID-jump rows), cached for
//! the dialog's lifetime. Scope Workspace uses `debug_search_report(…,
//! Some(workspace))`; "Copy as text" is `DebugSearchReport::to_text()`.

use std::collections::BTreeSet;

use chrono::Local;
use claude_history_core::maintenance::{DebugSearchReport, debug_search_report};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, rems,
};

use super::common::{
    Corpus, DialogSpec, Scope, close_dialog, corpus_line, current, forget_dialog, load_corpus,
    open_dialog, place_here, scope_control, set_current, spawn_thread, workspace_available,
};
use crate::globals::app_state;
use crate::model::short_session_id;
use crate::notify;
use crate::qa_ext::QaExt as _;
use crate::services::services;

/// The QA id of the dialog.
pub const DIALOG_ID: &str = "debug-search";

/// The column headers of the results table (§5.12).
pub const COLUMNS: [&str; 6] = ["#", "Score", "Fresh", "Project", "Session", "Age"];

/// The cell texts of one result row, in [`COLUMNS`] order (the table renders
/// exactly these).
pub fn row_cells(row: &claude_history_core::maintenance::DebugSearchRow) -> [String; 6] {
    [
        format!("{:02}", row.rank),
        format!("{:.2}", row.score),
        format!("{:.2}", row.freshness),
        row.project.clone(),
        short_session_id(&row.session),
        row.age_label.clone(),
    ]
}

/// The corpus load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorpusState {
    Loading,
    Ready(usize),
    Failed(String),
}

/// The dialog content.
pub struct DebugSearchDialog {
    query: Entity<InputState>,
    scope: Scope,
    corpus: Option<Corpus>,
    corpus_state: CorpusState,
    report: Option<DebugSearchReport>,
    running: bool,
    /// Run as soon as the corpus is loaded (a prefilled query).
    run_when_loaded: bool,
    expanded: BTreeSet<usize>,
    load_id: u64,
    run_id: u64,
    closed: bool,
    _load_task: Option<Task<()>>,
    _run_task: Option<Task<()>>,
    _subscription: Subscription,
}

impl Focusable for DebugSearchDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

/// The open dialog (tests and QA).
pub fn current_dialog(cx: &App) -> Option<Entity<DebugSearchDialog>> {
    current::<DebugSearchDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl DebugSearchDialog {
    fn new(
        prefill: Option<String>,
        local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let run_when_loaded = prefill
            .as_ref()
            .is_some_and(|query| !query.trim().is_empty());
        let query = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Query, e.g. refresh token");
            if let Some(text) = prefill {
                state.set_value(text, window, cx);
            }
            state
        });
        let subscription = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.run(cx);
            }
        });
        Self {
            query,
            scope: Scope::initial(local, cx),
            corpus: None,
            corpus_state: CorpusState::Loading,
            report: None,
            running: false,
            run_when_loaded,
            expanded: BTreeSet::new(),
            load_id: 0,
            run_id: 0,
            closed: false,
            _load_task: None,
            _run_task: None,
            _subscription: subscription,
        }
    }

    pub fn corpus_state(&self) -> &CorpusState {
        &self.corpus_state
    }

    pub fn report(&self) -> Option<&DebugSearchReport> {
        self.report.as_ref()
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().to_string()
    }

    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
    }

    /// The text "Copy as text" copies.
    pub fn report_text(&self) -> Option<String> {
        self.report.as_ref().map(DebugSearchReport::to_text)
    }

    /// (Re)load the corpus in the background ("Reload").
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_id += 1;
        let id = self.load_id;
        self.corpus_state = CorpusState::Loading;
        let (env, show_last, filter) = {
            let state = app_state(cx);
            (
                (*state.env).clone(),
                state.settings.show_last,
                state.launch.time_filter,
            )
        };
        let level = Some(crate::debug_sink::capture_level(cx));
        let rx = spawn_thread("debug-search-load", move || {
            load_corpus(&env, show_last, level, &filter)
        });
        self._load_task = Some(cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| this.finish_load(id, result, cx))
                    .ok();
            }
        }));
        cx.notify();
    }

    fn finish_load(&mut self, id: u64, result: Result<Corpus, String>, cx: &mut Context<Self>) {
        if id != self.load_id || self.closed {
            return;
        }
        match result {
            Ok(corpus) => {
                self.corpus_state = CorpusState::Ready(corpus.len());
                self.corpus = Some(corpus);
                if std::mem::take(&mut self.run_when_loaded) {
                    self.run(cx);
                }
            }
            Err(error) => {
                self.corpus = None;
                self.corpus_state = CorpusState::Failed(error);
            }
        }
        cx.notify();
    }

    /// Choose the scope. A report already shown is stale under the new scope,
    /// so the query runs again (as Delete empty transcripts rescans).
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        if self.report.is_some() {
            self.run(cx);
        } else {
            cx.notify();
        }
    }

    /// Run the query on the loaded corpus (or as soon as it is loaded).
    pub fn run(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(corpus) = self.corpus.clone() else {
            self.run_when_loaded = true;
            return;
        };
        self.run_id += 1;
        let id = self.run_id;
        self.running = true;
        self.expanded.clear();
        let workspace = match self.scope {
            Scope::Workspace => app_state(cx).workspace.clone(),
            Scope::All => None,
        };
        let rx = spawn_thread("debug-search-run", move || {
            debug_search_report(&corpus, &query, Local::now(), workspace.as_ref())
        });
        self._run_task = Some(cx.spawn(async move |this, cx| {
            if let Ok(report) = rx.recv().await {
                this.update(cx, |this, cx| {
                    if id == this.run_id {
                        this.running = false;
                        this.report = Some(report);
                        cx.notify();
                    }
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn toggle(&mut self, rank: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&rank) {
            self.expanded.insert(rank);
        }
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = true;
        close_dialog(DIALOG_ID, window, cx);
    }

    /// The Copy as text button.
    pub fn copy(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.report_text() {
            services(cx).clipboard_write(text, cx);
            notify::info(window, cx, "Copied to clipboard");
        }
    }
}

impl Render for DebugSearchDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, danger) = (theme.muted_foreground, theme.border, theme.danger);
        let mono = theme.mono_font_family.clone();
        let entity = cx.entity().downgrade();
        let corpus_text = match &self.corpus_state {
            CorpusState::Loading => "Corpus: loading…".to_string(),
            CorpusState::Ready(count) => corpus_line(*count, cx),
            CorpusState::Failed(error) => error.clone(),
        };
        let mut table: Vec<AnyElement> = Vec::new();
        if let Some(report) = &self.report {
            for row in &report.rows {
                let rank = row.rank;
                let [rank_text, score, freshness, project, session, age] = row_cells(row);
                let open = self.expanded.contains(&rank);
                table.push(
                    h_flex()
                        .id(("debug-row", rank))
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(format!(
                            "{rank:02} \u{b7} {} \u{b7} {} \u{b7} score {:.2} \u{b7} {}",
                            row.project,
                            short_session_id(&row.session),
                            row.score,
                            row.age_label
                        ))
                        .w_full()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .text_xs()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle(rank, cx)))
                        .child(div().w(rems(1.0)).flex_none().child(if open {
                            "▾"
                        } else {
                            "▸"
                        }))
                        .child(div().w(rems(2.0)).flex_none().child(rank_text))
                        .child(div().w(rems(3.5)).flex_none().child(score))
                        .child(div().w(rems(3.5)).flex_none().child(freshness))
                        .child(div().w(rems(8.0)).flex_none().truncate().child(project))
                        .child(
                            div()
                                .w(rems(8.0))
                                .flex_none()
                                .font_family(mono.clone())
                                .child(session),
                        )
                        .child(div().flex_1().text_color(muted).child(age))
                        .qa_id(format!("debug-search:row:{rank}"))
                        .into_any_element(),
                );
                if open {
                    for field in row.fields.iter().filter(|field| field.contributes()) {
                        let mut line = format!(
                            "{}: tf={:.2} adj={:.2} (w={:.1})",
                            field.name, field.tf_score, field.adjacency_score, field.weight
                        );
                        for word in field.words.iter().filter(|word| word.tf > 0) {
                            line.push_str(&format!(
                                "   \"{}\" tf={} ln={:.2}",
                                word.word, word.tf, word.ln_score
                            ));
                        }
                        table.push(
                            div()
                                .pl_8()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(muted)
                                .child(SharedString::from(line))
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        let summary = self.report.as_ref().map(|report| {
            let literals = if report.literals.is_empty() {
                "none".to_string()
            } else {
                report
                    .literals
                    .iter()
                    .map(|literal| format!("{:?}", literal.text))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!(
                "intent: {:?}     literals: {literals}     ({} match{}, top {})",
                report.intent,
                report.total_results,
                if report.total_results == 1 { "" } else { "es" },
                report.rows.len()
            )
        });
        v_flex()
            .id("debug-search-dialog")
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.query).small()))
                    .child(scope_control(
                        "debug-search-scope",
                        self.scope,
                        workspace_available(cx),
                        false,
                        move |scope, _, cx| {
                            entity.update(cx, |this, cx| this.set_scope(scope, cx)).ok();
                        },
                    ))
                    .child(
                        Button::new("debug-search-run")
                            .label("Run")
                            .small()
                            .primary()
                            .disabled(self.running)
                            .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                    ),
            )
            .child(
                div()
                    .id("debug-search-corpus")
                    .text_xs()
                    .text_color(if matches!(self.corpus_state, CorpusState::Failed(_)) {
                        danger
                    } else {
                        muted
                    })
                    .child(corpus_text),
            )
            .when_some(summary, |this, summary| {
                this.child(div().text_xs().font_family(mono.clone()).child(summary))
            })
            .child(
                v_flex()
                    .id("debug-search-table")
                    .w_full()
                    .h(rems(16.0))
                    .overflow_y_scroll()
                    .border_1()
                    .border_color(border)
                    .rounded(cx.theme().radius)
                    .when(!table.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .id("debug-search-header")
                                .w_full()
                                .gap_2()
                                .px_2()
                                .py_0p5()
                                .text_xs()
                                .text_color(muted)
                                .border_b_1()
                                .border_color(border)
                                .child(div().w(rems(1.0)).flex_none())
                                .child(div().w(rems(2.0)).flex_none().child(COLUMNS[0]))
                                .child(div().w(rems(3.5)).flex_none().child(COLUMNS[1]))
                                .child(div().w(rems(3.5)).flex_none().child(COLUMNS[2]))
                                .child(div().w(rems(8.0)).flex_none().child(COLUMNS[3]))
                                .child(div().w(rems(8.0)).flex_none().child(COLUMNS[4]))
                                .child(div().flex_1().child(COLUMNS[5]))
                                .qa_id("debug-search:header"),
                        )
                    })
                    .when(table.is_empty(), |this| {
                        this.child(
                            div()
                                .p_2()
                                .text_xs()
                                .text_color(muted)
                                .child(if self.running {
                                    "Running…"
                                } else {
                                    "No results yet."
                                }),
                        )
                    })
                    .children(table),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("debug-search-copy")
                            .label("Copy as text")
                            .small()
                            .disabled(self.report.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.copy(window, cx))),
                    )
                    .child(
                        Button::new("debug-search-reload")
                            .label("Reload")
                            .small()
                            .disabled(matches!(self.corpus_state, CorpusState::Loading))
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("debug-search-close")
                            .label("Close")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
    }
}

/// Tools → Debug search… (§5.12), optionally prefilled and run.
pub fn open_debug_search(prefill: Option<String>, local: bool, window: &mut Window, cx: &mut App) {
    let existing = current_dialog(cx);
    if let Some(existing) = place_here(
        cx,
        window,
        DIALOG_ID,
        existing,
        |d: &mut DebugSearchDialog| d.closed = true,
    ) {
        existing.update(cx, |dialog, cx| {
            if let Some(text) = &prefill {
                dialog.set_query(text, window, cx);
                dialog.run(cx);
            }
        });
        return;
    }
    let dialog = cx.new(|cx| DebugSearchDialog::new(prefill, local, window, cx));
    set_current(cx, &dialog, window);
    dialog.update(cx, |dialog, cx| dialog.reload(cx));
    let focus = dialog.read(cx).query.focus_handle(cx);
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: DIALOG_ID,
            title: "Debug search",
            width_rem: 50.0,
            enter_closes: false,
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
