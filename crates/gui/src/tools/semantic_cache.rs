//! Tools → Generate semantic cache… / Clear semantic cache… (DESIGN §5.11,
//! §7.6, §11.16; T-008…T-010, T-024). WP5.
//!
//! Both take the semantic gate: the list worker is suspended (`sem paused`),
//! agent semantic jobs are paused, and a second Generate/Clear is refused.
//! Generate runs over the whole corpus (AD9: never a time filter). Clear
//! empties the semantic directory and keeps the directory and
//! `embeddings.lock` (core K6), after the shared model was dropped.

use claude_history_core::maintenance::load_sorted_conversations;
use claude_history_core::semantic::types::SemanticCancellationToken;
use claude_history_core::semantic_ops::{
    GenerateProgress, GenerateReport, clear_semantic_cache, generate_semantic_cache,
};
use claude_history_core::time_filter::TimeFilter;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Window, div,
};

use super::common::{
    DialogSpec, Scope, close_dialog, current, current_window, error_text, forget_dialog,
    open_dialog, place_here, scope_control, set_current, spawn_progress_thread, spawn_thread,
    workspace_available,
};
use crate::embedder;
use crate::globals::{SemanticGate, app_state};
use crate::model::SemanticOp;
use crate::notify::Toast;
use crate::widgets::{ConfirmOptions, ConfirmOutcome, open_destructive_confirm};

/// The QA id of the Generate dialog.
/// Shown when the Generate worker ended without a result.
pub const WORKER_LOST: &str = "The semantic cache worker stopped unexpectedly.";
pub const GENERATE_ID: &str = "generate-semantic-cache";
/// The QA id of the Clear confirmation.
pub const CLEAR_ID: &str = "clear-semantic-cache";
/// Shown when Generate is chosen in a window while a run is going in another.
pub const RUNNING_ELSEWHERE: &str = "Generate semantic cache is already running in another window.";
/// Shown while the gate is held by another Generate/Clear/report.
pub const GATE_BUSY: &str = "Semantic search is busy with another cache operation.";

/// What a Generate run reports to the dialog.
enum Msg {
    Progress(GenerateProgress),
    Done(Result<GenerateReport, String>),
}

/// How the dialog's run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenerateState {
    Idle,
    Running,
    /// `GenerateReport::text`.
    Done(String),
    /// Error text (cancelled: `Semantic search cancelled`).
    Failed(String),
}

/// The Generate dialog content.
pub struct GenerateDialog {
    focus: FocusHandle,
    scope: Scope,
    state: GenerateState,
    progress: Option<GenerateProgress>,
    cancel: Option<SemanticCancellationToken>,
    closed: bool,
}

impl Focusable for GenerateDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The open Generate dialog (tests and QA).
pub fn current_generate(cx: &App) -> Option<Entity<GenerateDialog>> {
    current::<GenerateDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl GenerateDialog {
    pub fn state(&self) -> &GenerateState {
        &self.state
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The last progress line (`Semantic cache: embedded x/y changed chunk(s)`).
    pub fn progress_text(&self) -> Option<String> {
        self.progress.map(|progress| progress.to_string())
    }

    /// The progress bar's value (0-100): embedded / total changed chunks.
    pub fn percent(&self) -> f32 {
        match self.progress {
            Some(GenerateProgress::Embedded { completed, total }) if total > 0 => {
                completed as f32 * 100.0 / total as f32
            }
            _ => 0.0,
        }
    }

    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.state != GenerateState::Running {
            self.scope = scope;
            cx.notify();
        }
    }

    /// The Generate button.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if self.state == GenerateState::Running
            || !SemanticGate::enter_exclusive(SemanticOp::Generate, cx)
        {
            return;
        }
        self.state = GenerateState::Running;
        self.progress = None;
        let cancel = SemanticCancellationToken::new();
        self.cancel = Some(cancel.clone());
        let (env, show_last, workspace) = {
            let state = app_state(cx);
            (
                (*state.env).clone(),
                state.settings.show_last,
                match self.scope {
                    Scope::Workspace => state.workspace.clone(),
                    Scope::All => None,
                },
            )
        };
        let mut factory = embedder::ops_factory(cx);
        let level = Some(crate::debug_sink::capture_level(cx));
        let rx = spawn_progress_thread("semantic-generate", move |tx| {
            let progress_tx = tx.clone();
            let result = load_sorted_conversations(&env, show_last, level, &TimeFilter::default())
                .and_then(|conversations| {
                    generate_semantic_cache(
                        &env,
                        &conversations,
                        workspace.as_ref(),
                        &TimeFilter::default(),
                        &mut *factory,
                        &mut |progress| {
                            let _ = progress_tx.send_blocking(Msg::Progress(progress));
                        },
                        &cancel,
                    )
                })
                .map_err(|error| error_text(&error));
            let _ = tx.send_blocking(Msg::Done(result));
        });
        // Detached on purpose: the gate must be left when the run ends, even
        // if the dialog was closed meanwhile (closing cancels the run).
        cx.spawn(async move |this, cx| {
            let mut finished = false;
            while let Ok(msg) = rx.recv().await {
                let done = matches!(msg, Msg::Done(_));
                cx.update(|cx| {
                    if done {
                        SemanticGate::leave(cx);
                    }
                    this.update(cx, |this, cx| this.on_msg(msg, cx)).ok();
                });
                if done {
                    finished = true;
                    break;
                }
            }
            if !finished {
                // The worker died (panic, no thread): never keep the gate.
                cx.update(|cx| {
                    SemanticGate::leave(cx);
                    let failed = Msg::Done(Err(WORKER_LOST.to_string()));
                    this.update(cx, |this, cx| this.on_msg(failed, cx)).ok();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn on_msg(&mut self, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Progress(progress) => self.progress = Some(progress),
            Msg::Done(result) => {
                self.cancel = None;
                self.state = match result {
                    Ok(report) => GenerateState::Done(report.text.trim_end().to_string()),
                    Err(error) => GenerateState::Failed(error),
                };
            }
        }
        cx.notify();
    }

    /// Cancel while running, else close.
    pub fn cancel_or_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        } else {
            self.close(window, cx);
        }
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.closed = true;
        close_dialog(GENERATE_ID, window, cx);
    }
}

impl Render for GenerateDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let mono = theme.mono_font_family.clone();
        let running = self.state == GenerateState::Running;
        let gate_busy = !running && !SemanticGate::is_open(cx);
        let cache_dir = app_state(cx)
            .env
            .semantic_cache_dir()
            .map(|dir| dir.display().to_string());
        let entity = cx.entity().downgrade();
        let status = match &self.state {
            GenerateState::Idle => None,
            GenerateState::Running => Some(
                self.progress_text()
                    .unwrap_or_else(|| "Semantic cache: starting…".to_string()),
            ),
            GenerateState::Done(text) | GenerateState::Failed(text) => Some(text.clone()),
        };
        v_flex()
            .id("generate-dialog")
            .track_focus(&self.focus)
            .w_full()
            .gap_3()
            .child(div().text_sm().child(
                "Embeds every conversation so semantic search and Deep search are fast. The model (~128 MB) is downloaded on first use.",
            ))
            .child(scope_control(
                "generate-scope",
                self.scope,
                workspace_available(cx),
                running,
                move |scope, _, cx| {
                    entity.update(cx, |this, cx| this.set_scope(scope, cx)).ok();
                },
            ))
            .when(running, |this| {
                this.child(Progress::new("generate-progress").value(self.percent()))
            })
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .id("generate-status")
                        .text_sm()
                        .when(matches!(self.state, GenerateState::Failed(_)), |d| {
                            d.text_color(danger)
                        })
                        .child(status),
                )
            })
            .when(running, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Semantic search is paused while the cache is generated."),
                )
            })
            .when(gate_busy, |this| {
                this.child(div().text_xs().text_color(danger).child(GATE_BUSY))
            })
            .when_some(cache_dir, |this, dir| {
                this.child(
                    div()
                        .text_xs()
                        .font_family(mono)
                        .text_color(muted)
                        .child(format!("Cache: {dir}")),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("generate-cancel")
                            .label(if running { "Cancel" } else { "Close" })
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel_or_close(window, cx)
                            })),
                    )
                    .child(
                        Button::new("generate-run")
                            .label(if matches!(self.state, GenerateState::Failed(_)) {
                                "Retry"
                            } else {
                                "Generate"
                            })
                            .small()
                            .primary()
                            .disabled(running || gate_busy)
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
    }
}

/// Tools → Generate semantic cache… (§5.11, under the semantic gate). The run
/// is started with the Generate button, never automatically.
pub fn open_generate_semantic_cache(local: bool, window: &mut Window, cx: &mut App) {
    let existing = current_generate(cx);
    // A running generation is never taken over (and so never cancelled) by
    // choosing the item in another window: cancelling is Cancel/close only.
    if let Some(running) = &existing
        && running.read(cx).state == GenerateState::Running
        && current_window::<GenerateDialog>(cx) != Some(window.window_handle())
    {
        Toast::info(RUNNING_ELSEWHERE).show(window, cx);
        return;
    }
    if place_here(
        cx,
        window,
        GENERATE_ID,
        existing,
        |d: &mut GenerateDialog| {
            if let Some(cancel) = d.cancel.take() {
                cancel.cancel();
            }
            d.closed = true;
        },
    )
    .is_some()
    {
        return;
    }
    let scope = Scope::initial(local, cx);
    let dialog = cx.new(|cx| GenerateDialog {
        focus: cx.focus_handle(),
        scope,
        state: GenerateState::Idle,
        progress: None,
        cancel: None,
        closed: false,
    });
    set_current(cx, &dialog, window);
    let focus = dialog.read(cx).focus.clone();
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: GENERATE_ID,
            title: "Generate semantic cache",
            width_rem: 36.0,
            enter_closes: false,
        },
        dialog,
        focus,
        move |cx| {
            dismissed.update(cx, |dialog, _| {
                if let Some(cancel) = dialog.cancel.take() {
                    cancel.cancel();
                }
                dialog.closed = true;
            });
            forget_dialog(GENERATE_ID, cx);
        },
        window,
        cx,
    );
}

/// Tools → Clear semantic cache… (§5.11): a confirmation, then the gate, the
/// shared model dropped, and `clear_semantic_cache`.
pub fn open_clear_semantic_cache(window: &mut Window, cx: &mut App) {
    let dir = app_state(cx)
        .env
        .semantic_cache_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_else(|| "the semantic cache folder".to_string());
    if !SemanticGate::is_open(cx) {
        // §7.6: while the gate is held, Clear is unavailable.
        Toast::info(GATE_BUSY).show(window, cx);
        return;
    }
    crate::workspace::startup::register_dialog(cx, CLEAR_ID);
    let options = ConfirmOptions::destructive("Clear semantic cache?", "Clear")
        .description(format!(
            "Deletes everything in {dir}, including the downloaded embedding model. This can't be undone."
        ))
        .width_rem(36.0)
        .on_closed(|cx| forget_dialog(CLEAR_ID, cx));
    open_destructive_confirm(options, window, cx, move |control, _, cx| {
        if !SemanticGate::enter_exclusive(SemanticOp::Clear, cx) {
            control.set_error(Some(GATE_BUSY.into()), cx);
            return ConfirmOutcome::KeepOpen;
        }
        // Files the model holds open must be closed first.
        embedder::drop_model(cx);
        let env = (*app_state(cx).env).clone();
        let rx = spawn_thread("semantic-clear", move || {
            clear_semantic_cache(&env)
                .map(|report| (report.removed, report.message))
                .map_err(|error| error_text(&error))
        });
        let control = control.clone();
        cx.spawn(async move |cx| {
            let result = rx.recv().await.ok();
            cx.update(|cx| {
                SemanticGate::leave(cx);
                match result {
                    Some(Ok((removed, message))) => {
                        control.close_then(cx, move |window, cx| {
                            let toast = if removed {
                                Toast::success(message)
                            } else {
                                Toast::info(message)
                            };
                            toast.show(window, cx);
                        });
                    }
                    Some(Err(error)) => control.set_error(Some(error.into()), cx),
                    None => control.set_error(Some("Clearing failed unexpectedly.".into()), cx),
                }
            });
        })
        .detach();
        ConfirmOutcome::Busy
    });
}
