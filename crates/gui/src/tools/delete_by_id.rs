//! Tools → Delete conversation by session ID… (DESIGN §5.15, §11.15;
//! T-017, T-018, C-013). WP5.
//!
//! Find = `maintenance::find_session_copies` (errors read `Session not
//! found: <id>`); Delete… plans with `plan_session_delete` (K4), confirms with
//! the §5.7 pattern and executes with `execute_session_delete`. The deleted
//! transcripts are broadcast (§11.15).

use claude_history_core::history::Source;
use claude_history_core::maintenance::{
    DeletePlan, SessionCopyFiles, delete_by_uuid_message, execute_session_delete,
    find_session_copies, plan_session_delete,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Task, Window, div,
};

use super::common::{
    DialogSpec, ShownConfirm, close_dialog, current, error_text, forget_dialog, open_dialog,
    place_here, set_current, spawn_thread,
};
use super::delete_empty::announce_deleted;
use crate::globals::app_state;
use crate::model::short_session_id;
use crate::notify::Toast;
use crate::qa_ext::QaExt as _;
use crate::widgets::{
    ConfirmControl, ConfirmOptions, ConfirmOutcome, PathItem, PathList, open_destructive_confirm,
};

/// The QA id of the dialog.
pub const DIALOG_ID: &str = "delete-by-id";
/// The note under the copies (T-018).
pub const NOT_REMOVED_NOTE: &str =
    "Not removed: file-history, todos, session-env and other global data.";

/// The dialog content.
pub struct DeleteByIdDialog {
    input: Entity<InputState>,
    finding: bool,
    /// The id the copies were found for.
    found_id: Option<String>,
    copies: Vec<SessionCopyFiles>,
    error: Option<String>,
    result: Option<String>,
    busy: bool,
    find_id: u64,
    shown_confirm: Option<ShownConfirm>,
    closed: bool,
    _task: Option<Task<()>>,
    _subscription: Subscription,
}

impl Focusable for DeleteByIdDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

/// The open dialog (tests and QA).
pub fn current_dialog(cx: &App) -> Option<Entity<DeleteByIdDialog>> {
    current::<DeleteByIdDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl DeleteByIdDialog {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Session ID"));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.find(cx),
                    InputEvent::Change => this.on_edited(cx),
                    _ => {}
                },
            );
        Self {
            input,
            finding: false,
            found_id: None,
            copies: Vec::new(),
            error: None,
            result: None,
            busy: false,
            find_id: 0,
            shown_confirm: None,
            closed: false,
            _task: None,
            _subscription: subscription,
        }
    }

    pub fn copies(&self) -> &[SessionCopyFiles] {
        &self.copies
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn result(&self) -> Option<&str> {
        self.result.as_deref()
    }

    /// The open delete confirmation's title and path list.
    pub fn shown_confirm(&self) -> Option<&ShownConfirm> {
        self.shown_confirm.as_ref()
    }

    pub fn is_busy(&self) -> bool {
        self.busy || self.finding
    }

    pub fn id_text(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    /// Whether Delete… is enabled (a Find succeeded for the text now typed).
    pub fn can_delete(&self, cx: &App) -> bool {
        !self.is_busy()
            && !self.copies.is_empty()
            && self.found_id.as_deref() == Some(self.id_text(cx).as_str())
    }

    pub fn set_id(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
    }

    fn on_edited(&mut self, cx: &mut Context<Self>) {
        let text = self.id_text(cx);
        if self.found_id.as_deref() != Some(text.as_str()) && !self.copies.is_empty() {
            self.copies.clear();
            self.found_id = None;
        }
        cx.notify();
    }

    /// The Find button.
    pub fn find(&mut self, cx: &mut Context<Self>) {
        let id = self.id_text(cx);
        if id.is_empty() || self.busy {
            return;
        }
        self.find_id += 1;
        let token = self.find_id;
        self.finding = true;
        self.error = None;
        self.result = None;
        self.copies.clear();
        self.found_id = None;
        let env = (*app_state(cx).env).clone();
        let looked_up = id.clone();
        let rx = spawn_thread("delete-by-id-find", move || {
            find_session_copies(&env, &looked_up).map_err(|error| error_text(&error))
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    if token != this.find_id {
                        return;
                    }
                    this.finding = false;
                    match result {
                        Ok(copies) => {
                            this.copies = copies;
                            this.found_id = Some(id);
                        }
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    /// The Delete… button: plan, then confirm.
    pub fn request_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_delete(cx) {
            return;
        }
        let id = self.found_id.clone().unwrap_or_default();
        let Some(first) = self.copies.first().map(|copy| copy.jsonl.clone()) else {
            return;
        };
        self.busy = true;
        let env = (*app_state(cx).env).clone();
        let rx = spawn_thread("delete-by-id-plan", move || {
            plan_session_delete(&env, &first, Source::Claude).map_err(|error| error_text(&error))
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update_in(cx, |this, window, cx| {
                    this.busy = false;
                    match result {
                        Ok(plan) => this.confirm(id, plan, window, cx),
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn confirm(
        &mut self,
        id: String,
        plan: DeletePlan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.found_id.as_deref() != Some(id.as_str()) {
            // The ID was edited while the plan ran: that plan is stale.
            return;
        }
        let mut items: Vec<PathItem> = plan.files.iter().map(PathItem::file).collect();
        items.extend(plan.dirs.iter().map(|dir| PathItem::folder(dir.clone())));
        let copies = plan.files.len();
        let title = format!("Delete session {}?", short_session_id(&id));
        self.shown_confirm = Some(ShownConfirm::new(&title, &items));
        let gone = cx.entity().downgrade();
        let options = ConfirmOptions::destructive(title, "Delete")
            .on_closed(move |cx| {
                gone.update(cx, |this, _| this.shown_confirm = None).ok();
            })
            .description(format!(
                "These will be removed from disk ({copies} cop{}). This can't be undone.",
                if copies == 1 { "y" } else { "ies" }
            ))
            .width_rem(40.0)
            .body(move |_, _| {
                PathList::new("delete-by-id-paths")
                    .items(items.clone())
                    .max_rows(8)
                    .into_any_element()
            });
        let this = cx.entity().downgrade();
        let app: &mut App = cx;
        open_destructive_confirm(options, window, app, move |control, _, cx| {
            let (control, plan, id) = (control.clone(), plan.clone(), id.clone());
            this.update(cx, |this, cx| this.start_delete(id, plan, control, cx))
                .ok();
            ConfirmOutcome::Busy
        });
    }

    fn start_delete(
        &mut self,
        id: String,
        plan: DeletePlan,
        control: ConfirmControl,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        let env = (*app_state(cx).env).clone();
        let planned = plan.clone();
        let rx = spawn_thread("delete-by-id-run", move || {
            let (result, removed) = match execute_session_delete(&env, &planned) {
                Ok(_) => (Ok(()), planned.files.clone()),
                Err(failure) => {
                    // Copies removed (or already gone) before a failure are
                    // gone all the same.
                    let mut gone = failure.partial.removed_files;
                    gone.extend(
                        failure
                            .partial
                            .missing
                            .into_iter()
                            .filter(|path| planned.files.contains(path)),
                    );
                    (Err(error_text(&failure.error)), gone)
                }
            };
            (removed, result)
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = rx.recv().await.ok();
            cx.update(|cx| match result {
                Some((_, Ok(()))) => {
                    announce_deleted(&plan.files, cx);
                    let message = delete_by_uuid_message(&id, plan.files.len());
                    this.update(cx, |this, cx| {
                        this.busy = false;
                        this.copies.clear();
                        this.found_id = None;
                        this.result = Some(message.clone());
                        cx.notify();
                    })
                    .ok();
                    control.close_then(cx, move |window, cx| {
                        Toast::success(message).show(window, cx)
                    });
                }
                other => {
                    let error = match other {
                        Some((removed, Err(error))) => {
                            if !removed.is_empty() {
                                announce_deleted(&removed, cx);
                            }
                            error
                        }
                        _ => "Deleting failed unexpectedly.".to_string(),
                    };
                    this.update(cx, |this, cx| {
                        this.busy = false;
                        cx.notify();
                    })
                    .ok();
                    control.set_error(
                        Some(format!("Failed to delete session {id}: {error}").into()),
                        cx,
                    );
                }
            });
        }));
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = true;
        close_dialog(DIALOG_ID, window, cx);
    }
}

impl Render for DeleteByIdDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let can_delete = self.can_delete(cx);
        let busy = self.is_busy();
        let mut items: Vec<PathItem> = Vec::new();
        for copy in &self.copies {
            items.push(PathItem::file(copy.jsonl.clone()));
            if let Some(dir) = &copy.session_dir {
                items.push(PathItem::folder(dir.clone()));
            }
        }
        let found = (!self.copies.is_empty()).then(|| {
            format!(
                "Found {} cop{}:",
                self.copies.len(),
                if self.copies.len() == 1 { "y" } else { "ies" }
            )
        });
        v_flex()
            .id("delete-by-id-dialog")
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.input).small()))
                    .child(
                        Button::new("delete-by-id-find")
                            .label("Find")
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.find(cx)))
                            .qa_id("delete-by-id:find"),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Claude Code conversations only."),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("delete-by-id-error")
                        .text_sm()
                        .text_color(danger)
                        .child(error),
                )
            })
            .when_some(found, |this, found| {
                this.child(div().id("delete-by-id-found").text_sm().child(found))
                    .child(
                        PathList::new("delete-by-id-copies")
                            .items(items)
                            .max_rows(8),
                    )
            })
            .when_some(self.result.clone(), |this, result| {
                this.child(div().id("delete-by-id-result").text_sm().child(result))
            })
            .child(div().text_xs().text_color(muted).child(NOT_REMOVED_NOTE))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("delete-by-id-close")
                            .label("Close")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        Button::new("delete-by-id-delete")
                            .label("Delete…")
                            .small()
                            .danger()
                            .disabled(!can_delete)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.request_delete(window, cx)),
                            )
                            .qa_id("delete-by-id:delete"),
                    ),
            )
    }
}

/// Tools → Delete conversation by session ID… (§5.15), optionally prefilled + Find.
pub fn open_delete_by_id(prefill: Option<String>, window: &mut Window, cx: &mut App) {
    let existing = current_dialog(cx);
    if let Some(existing) = place_here(
        cx,
        window,
        DIALOG_ID,
        existing,
        |d: &mut DeleteByIdDialog| d.closed = true,
    ) {
        existing.update(cx, |dialog, cx| {
            if let Some(text) = &prefill {
                dialog.set_id(text, window, cx);
                dialog.find(cx);
            }
        });
        return;
    }
    let dialog = cx.new(|cx| DeleteByIdDialog::new(window, cx));
    set_current(cx, &dialog, window);
    if let Some(text) = &prefill {
        dialog.update(cx, |dialog, cx| {
            dialog.set_id(text, window, cx);
            dialog.find(cx);
        });
    }
    let focus = dialog.read(cx).input.focus_handle(cx);
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: DIALOG_ID,
            title: "Delete conversation by session ID",
            width_rem: 44.0,
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
