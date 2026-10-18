//! Tools → Semantic debug search… (DESIGN §5.13, §11.24; T-012, T-013, C-015).
//! WP5.
//!
//! One dialog, two modes: the search report (`--semantic-search`, read-only
//! text) and the debug dump (`--debug-semantic-search`). The corpus is a fresh
//! load with the launch time filter. A run holds the semantic gate (the list
//! worker and agent semantic jobs wait, there is one shared model) and is
//! refused while Generate/Clear holds it. The output is core's `text`
//! verbatim.

use claude_history_core::semantic_ops::{semantic_debug_report, semantic_search_report};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    Task, Window, div, rems,
};

use super::common::{
    Corpus, DialogSpec, Scope, close_dialog, corpus_line, current, error_text, forget_dialog,
    load_corpus, open_dialog, place_here, scope_control, set_current, spawn_thread,
    workspace_available,
};
use super::semantic_cache::GATE_BUSY;
use crate::embedder;
use crate::globals::{SemanticGate, app_state};
use crate::model::{SemanticOp, SemanticReportMode};
use crate::notify;
use crate::services::services;
use crate::widgets::ReadOnlyText;

/// The QA id of the dialog.
pub const DIALOG_ID: &str = "semantic-debug";
/// The default `--semantic-top`.
pub const DEFAULT_TOP: usize = 20;

/// What the dialog shows below the controls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Loading,
    Running,
    Done,
    Failed(String),
}

/// The dialog content.
pub struct SemanticReportDialog {
    query: Entity<InputState>,
    top: Entity<InputState>,
    mode_dump: bool,
    scope: Scope,
    output: ReadOnlyText,
    corpus: Option<Corpus>,
    state: RunState,
    corpus_error: Option<String>,
    run_when_loaded: bool,
    load_id: u64,
    closed: bool,
    _load_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SemanticReportDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

/// The open dialog (tests and QA).
pub fn current_dialog(cx: &App) -> Option<Entity<SemanticReportDialog>> {
    current::<SemanticReportDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl SemanticReportDialog {
    fn new(
        prefill: Option<String>,
        mode: SemanticReportMode,
        local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let top_value = match mode {
            SemanticReportMode::Search { top } => top,
            SemanticReportMode::DebugDump => DEFAULT_TOP,
        };
        let run_when_loaded = prefill
            .as_ref()
            .is_some_and(|query| !query.trim().is_empty());
        let query = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Query");
            if let Some(text) = prefill {
                state.set_value(text, window, cx);
            }
            state
        });
        let top = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("20");
            state.set_value(top_value.to_string(), window, cx);
            state
        });
        let subscriptions = [&query, &top]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.run(window, cx);
                    }
                })
            })
            .collect();
        Self {
            query,
            top,
            mode_dump: mode == SemanticReportMode::DebugDump,
            scope: Scope::initial(local, cx),
            output: ReadOnlyText::new(window, cx),
            corpus: None,
            state: RunState::Loading,
            corpus_error: None,
            run_when_loaded,
            load_id: 0,
            closed: false,
            _load_task: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn state(&self) -> &RunState {
        &self.state
    }

    pub fn is_debug_dump(&self) -> bool {
        self.mode_dump
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().to_string()
    }

    pub fn top_text(&self, cx: &App) -> String {
        self.top.read(cx).value().to_string()
    }

    /// The report text shown.
    pub fn output_text(&self, cx: &App) -> String {
        self.output.text(cx).to_string()
    }

    /// Switch between the search report and the debug dump.
    pub fn set_debug_dump(&mut self, dump: bool, cx: &mut Context<Self>) {
        if self.state != RunState::Running {
            self.mode_dump = dump;
            cx.notify();
        }
    }

    /// Choose the scope. A report already shown is stale under the new scope,
    /// so it is run again; the control is disabled while a run is going.
    pub fn set_scope(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        if self.scope == scope || self.state == RunState::Running {
            return;
        }
        self.scope = scope;
        if matches!(self.state, RunState::Done | RunState::Failed(_)) {
            self.run(window, cx);
        } else {
            cx.notify();
        }
    }

    /// Load the corpus (fresh, launch time filter) in the background.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_id += 1;
        let id = self.load_id;
        self.state = RunState::Loading;
        let (env, show_last, filter) = {
            let state = app_state(cx);
            (
                (*state.env).clone(),
                state.settings.show_last,
                state.launch.time_filter,
            )
        };
        let level = Some(crate::debug_sink::capture_level(cx));
        let rx = spawn_thread("semantic-report-load", move || {
            load_corpus(&env, show_last, level, &filter)
        });
        self._load_task = Some(cx.spawn_in(window, async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update_in(cx, |this, window, cx| {
                    this.finish_load(id, result, window, cx)
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn finish_load(
        &mut self,
        id: u64,
        result: Result<Corpus, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if id != self.load_id || self.closed {
            return;
        }
        match result {
            Ok(corpus) => {
                self.corpus = Some(corpus);
                self.corpus_error = None;
                self.state = RunState::Idle;
                if std::mem::take(&mut self.run_when_loaded) {
                    self.run(window, cx);
                }
            }
            Err(error) => {
                self.corpus_error = Some(error.clone());
                self.state = RunState::Failed(error);
            }
        }
        cx.notify();
    }

    /// Run the report (the Run button / Enter).
    pub fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() || self.state == RunState::Running {
            return;
        }
        let Some(corpus) = self.corpus.clone() else {
            self.run_when_loaded = true;
            return;
        };
        let top = if self.mode_dump {
            DEFAULT_TOP
        } else {
            match self.top.read(cx).value().trim().parse::<usize>() {
                Ok(top) if top > 0 => top,
                _ => {
                    self.state = RunState::Failed("Top must be a positive number".to_string());
                    cx.notify();
                    return;
                }
            }
        };
        if !SemanticGate::enter_exclusive(SemanticOp::Generate, cx) {
            self.state = RunState::Failed(GATE_BUSY.to_string());
            cx.notify();
            return;
        }
        self.state = RunState::Running;
        self.output.set_text("", window, cx);
        let (env, workspace) = {
            let state = app_state(cx);
            (
                (*state.env).clone(),
                match self.scope {
                    Scope::Workspace => state.workspace.clone(),
                    Scope::All => None,
                },
            )
        };
        let dump = self.mode_dump;
        let mut factory = embedder::ops_factory(cx);
        let rx = spawn_thread("semantic-report-run", move || {
            if dump {
                semantic_debug_report(&env, &query, &corpus, workspace.as_ref(), &mut *factory)
                    .map(|report| report.text)
            } else {
                semantic_search_report(
                    &env,
                    &query,
                    &corpus,
                    top,
                    workspace.as_ref(),
                    &mut *factory,
                )
                .map(|report| report.text)
            }
            .map_err(|error| error_text(&error))
        });
        let handle = window.window_handle();
        let this = cx.entity().downgrade();
        // Detached on purpose: the gate must be left when the run ends, even
        // if the dialog was closed meanwhile.
        cx.spawn(async move |_, cx| {
            let result = rx.recv().await.ok();
            cx.update(|cx| {
                SemanticGate::leave(cx);
                deliver(handle, this, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_run(
        &mut self,
        result: Option<Result<String, String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Some(Ok(text)) => {
                self.output
                    .set_text(text.trim_end().to_string(), window, cx);
                self.state = RunState::Done;
            }
            Some(Err(error)) => self.state = RunState::Failed(error),
            None => self.state = RunState::Failed("The report stopped unexpectedly.".to_string()),
        }
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = true;
        close_dialog(DIALOG_ID, window, cx);
    }

    fn copy(&self, window: &mut Window, cx: &mut Context<Self>) {
        services(cx).clipboard_write(self.output.text(cx).to_string(), cx);
        notify::info(window, cx, "Copied to clipboard");
    }
}

fn deliver(
    handle: AnyWindowHandle,
    dialog: gpui_kit::WeakEntity<SemanticReportDialog>,
    result: Option<Result<String, String>>,
    cx: &mut App,
) {
    handle
        .update(cx, |_, window, cx| {
            dialog
                .update(cx, |dialog, cx| dialog.finish_run(result, window, cx))
                .ok();
        })
        .ok();
}

impl Render for SemanticReportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let running = self.state == RunState::Running;
        let entity = cx.entity().downgrade();
        let corpus_text = match (&self.corpus, &self.corpus_error) {
            (Some(corpus), _) => corpus_line(corpus.len(), cx),
            (None, Some(error)) => error.clone(),
            (None, None) => "Corpus: loading…".to_string(),
        };
        let mode = |dump: bool, label: &'static str, cx: &mut Context<Self>| {
            Button::new(if dump {
                "semantic-mode-dump"
            } else {
                "semantic-mode-search"
            })
            .label(label)
            .small()
            .selected(self.mode_dump == dump)
            .disabled(running)
            .on_click(cx.listener(move |this, _, _, cx| this.set_debug_dump(dump, cx)))
        };
        let search_button = mode(false, "Search report", cx);
        let dump_button = mode(true, "Debug dump", cx);
        v_flex()
            .id("semantic-debug-dialog")
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.query).small()))
                    .child(
                        ButtonGroup::new("semantic-mode")
                            .outline()
                            .compact()
                            .child(search_button)
                            .child(dump_button),
                    )
                    .when(!self.mode_dump, |row| {
                        row.child(div().text_xs().text_color(muted).child("Top"))
                            .child(div().w(rems(4.0)).child(Input::new(&self.top).small()))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(scope_control(
                        "semantic-scope",
                        self.scope,
                        workspace_available(cx),
                        running,
                        move |scope, window, cx| {
                            entity
                                .update(cx, |this, cx| this.set_scope(scope, window, cx))
                                .ok();
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("semantic-run")
                            .label("Run")
                            .small()
                            .primary()
                            .disabled(running || self.corpus.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.run(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("semantic-corpus")
                    .text_xs()
                    .text_color(muted)
                    .child(corpus_text),
            )
            .when_some(
                match &self.state {
                    RunState::Failed(error) => Some(error.clone()),
                    RunState::Running => Some("Running…".to_string()),
                    _ => None,
                },
                |this, text| {
                    this.child(
                        div()
                            .id("semantic-status")
                            .text_xs()
                            .when(matches!(self.state, RunState::Failed(_)), |d| {
                                d.text_color(danger)
                            })
                            .child(text),
                    )
                },
            )
            .child(
                div()
                    .h(rems(18.0))
                    // The text area fills its box (a bare Textarea is a few lines tall).
                    .child(self.output.element("Semantic report").size_full()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("semantic-copy")
                            .label("Copy")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.copy(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("semantic-close")
                            .label("Close")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
    }
}

/// Tools → Semantic debug search… (§5.13).
pub fn open_semantic_debug(
    prefill: Option<String>,
    mode: SemanticReportMode,
    local: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let existing = current_dialog(cx);
    if let Some(existing) = place_here(
        cx,
        window,
        DIALOG_ID,
        existing,
        |d: &mut SemanticReportDialog| d.closed = true,
    ) {
        existing.update(cx, |dialog, cx| {
            dialog.set_debug_dump(mode == SemanticReportMode::DebugDump, cx);
            if let Some(text) = &prefill {
                dialog
                    .query
                    .update(cx, |state, cx| state.set_value(text.clone(), window, cx));
                dialog.run(window, cx);
            }
        });
        return;
    }
    let dialog = cx.new(|cx| SemanticReportDialog::new(prefill, mode, local, window, cx));
    set_current(cx, &dialog, window);
    dialog.update(cx, |dialog, cx| dialog.reload(window, cx));
    let focus = dialog.read(cx).query.focus_handle(cx);
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: DIALOG_ID,
            title: "Semantic debug search",
            width_rem: 54.0,
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
