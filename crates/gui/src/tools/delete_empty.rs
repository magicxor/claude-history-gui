//! Tools → Delete empty transcripts… (DESIGN §5.10, §11.15; T-001, T-003…T-007).
//! WP5.
//!
//! The dialog scans with `maintenance::find_empty_candidates` (Claude only),
//! lists the candidates and deletes exactly the confirmed rows with
//! `delete_empty_candidates`. Files modified in the last 10 minutes (a brand
//! new session without a reply yet) are excluded unless the box is checked.

use std::path::PathBuf;

use chrono::{DateTime, Local};
use claude_history_core::debug::DebugLevel;
use claude_history_core::display_format::format_timestamp;
use claude_history_core::maintenance::{
    DeleteEmptyScope, EmptyCandidate, SkipReason, delete_empty_candidates,
    delete_empty_summary_line, find_empty_candidates, format_delete_empty_report,
    truncate_delete_empty_preview,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, UniformListScrollHandle,
    Window, div, rems, uniform_list,
};

use super::common::{
    DialogSpec, Scope, ShownConfirm, close_dialog, current, error_text, forget_dialog, open_dialog,
    place_here, scope_control, set_current, spawn_thread, workspace_available,
};
use crate::globals::{CorpusBus, CorpusNotice, app_state, debug_buffer, main_panes};
use crate::model::short_session_id;
use crate::notify;
use crate::qa_ext::QaExt as _;
use crate::services::services;
use crate::widgets::{
    ConfirmOptions, ConfirmOutcome, PathItem, PathList, open_destructive_confirm,
};

/// The QA id of the dialog.
pub const DIALOG_ID: &str = "delete-empty";
/// Files modified more recently than this are "recent" (T-005).
pub const RECENT_MINUTES: i64 = 10;

/// The column headers of the candidate table (§5.10).
pub const COLUMNS: [&str; 6] = [
    "Session", "Project", "Users", "Lines", "Preview", "Modified",
];
/// The height of one candidate row, in rem (every row has this height, so the
/// table can be virtualized).
const ROW_REM: f32 = 1.5;
/// Rows the table shows before it scrolls.
const VISIBLE_ROWS: usize = 9;

/// The cell texts of one candidate row, in [`COLUMNS`] order (the table renders
/// exactly these).
pub fn row_cells(candidate: &EmptyCandidate, recent: bool, now: DateTime<Local>) -> [String; 6] {
    [
        format!(
            "{}{}",
            short_session_id(candidate.session_id()),
            if recent { " ⏱" } else { "" }
        ),
        candidate.project().to_string(),
        candidate.transcript.user_messages.to_string(),
        candidate.transcript.line_count.to_string(),
        candidate
            .transcript
            .preview
            .as_deref()
            .map(truncate_delete_empty_preview)
            .unwrap_or_else(|| "(no user messages)".to_string()),
        candidate
            .modified
            .map(|at| format_timestamp(at, now).0)
            .unwrap_or_default(),
    ]
}

/// What the scan is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanState {
    Scanning,
    Ready,
    Failed(String),
}

/// What a finished deletion reports (plain data, built on the worker thread).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeleteResult {
    pub deleted: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, &'static str)>,
    pub failure: Option<(PathBuf, String)>,
}

/// The reason text of a skipped candidate.
pub fn skip_reason_text(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::Changed => "changed since the scan",
        SkipReason::NoLongerEmpty => "now has an assistant message",
        SkipReason::Missing => "no longer exists",
    }
}

/// Run the confirmed deletion (blocking; worker thread).
pub fn run_delete(candidates: &[EmptyCandidate]) -> DeleteResult {
    let outcome = delete_empty_candidates(candidates);
    DeleteResult {
        deleted: outcome.deleted.clone(),
        skipped: outcome
            .skipped
            .iter()
            .map(|skipped| (skipped.path.clone(), skip_reason_text(skipped.reason)))
            .collect(),
        failure: outcome
            .failure
            .as_ref()
            .map(|failure| (failure.path.clone(), error_text(&failure.error))),
    }
}

/// The dialog content.
pub struct DeleteEmptyDialog {
    focus: FocusHandle,
    scope: Scope,
    include_recent: bool,
    state: ScanState,
    candidates: Vec<EmptyCandidate>,
    scanned_at: DateTime<Local>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    /// The rows the last frame built (the table is virtualized).
    rendered: std::ops::Range<usize>,
    result: Vec<String>,
    deleting: bool,
    shown_confirm: Option<ShownConfirm>,
    closed: bool,
    scan_id: u64,
    _scan_task: Option<Task<()>>,
    _delete_task: Option<Task<()>>,
}

impl Focusable for DeleteEmptyDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The open dialog (tests and QA).
pub fn current_dialog(cx: &App) -> Option<Entity<DeleteEmptyDialog>> {
    current::<DeleteEmptyDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl DeleteEmptyDialog {
    fn new(local: bool, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            scope: Scope::initial(local, cx),
            include_recent: false,
            state: ScanState::Scanning,
            candidates: Vec::new(),
            scanned_at: Local::now(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            rendered: 0..0,
            result: Vec::new(),
            deleting: false,
            shown_confirm: None,
            closed: false,
            scan_id: 0,
            _scan_task: None,
            _delete_task: None,
        }
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn state(&self) -> &ScanState {
        &self.state
    }

    pub fn candidates(&self) -> &[EmptyCandidate] {
        &self.candidates
    }

    pub fn include_recent(&self) -> bool {
        self.include_recent
    }

    pub fn result_lines(&self) -> &[String] {
        &self.result
    }

    /// The open delete confirmation's title and path list.
    pub fn shown_confirm(&self) -> Option<&ShownConfirm> {
        self.shown_confirm.as_ref()
    }

    pub fn is_deleting(&self) -> bool {
        self.deleting
    }

    /// Whether `candidate` is recent (modified in the last 10 minutes at the scan).
    pub fn is_recent(&self, candidate: &EmptyCandidate) -> bool {
        candidate.modified_within(self.scanned_at, chrono::Duration::minutes(RECENT_MINUTES))
    }

    /// The candidates a delete would remove now.
    pub fn eligible(&self) -> Vec<EmptyCandidate> {
        self.candidates
            .iter()
            .filter(|candidate| self.include_recent || !self.is_recent(candidate))
            .cloned()
            .collect()
    }

    /// `Found N empty transcript(s).` / `No empty transcripts found.`
    pub fn found_line(&self) -> String {
        match &self.state {
            ScanState::Scanning => "Scanning…".to_string(),
            ScanState::Failed(error) => error.clone(),
            ScanState::Ready if self.candidates.is_empty() => {
                "No empty transcripts found.".to_string()
            }
            ScanState::Ready => format!("Found {} empty transcript(s).", self.candidates.len()),
        }
    }

    /// The "Copy as text" payload: the CLI's dry-run output for every candidate.
    pub fn report_text(&self) -> String {
        let transcripts: Vec<_> = self
            .candidates
            .iter()
            .map(|candidate| candidate.transcript.clone())
            .collect();
        format_delete_empty_report(&transcripts, None)
    }

    /// Scan again (scope change, Rescan, after a delete).
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.scan_id += 1;
        let id = self.scan_id;
        self.state = ScanState::Scanning;
        self.selected = None;
        let env = app_state(cx).env_with_workspace();
        if self.scope == Scope::Workspace && app_state(cx).workspace.is_none() {
            // The workspace went away: show the wider scope instead of
            // silently scanning it under the "Current workspace" label.
            self.scope = Scope::All;
        }
        let scope = match (self.scope, app_state(cx).workspace.clone()) {
            (Scope::Workspace, Some(workspace)) => DeleteEmptyScope::Local(workspace),
            _ => DeleteEmptyScope::All,
        };
        let rx = spawn_thread("delete-empty-scan", move || {
            find_empty_candidates(&env, &scope).map_err(|error| error_text(&error))
        });
        self._scan_task = Some(cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| this.finish_scan(id, result, cx))
                    .ok();
            }
        }));
        cx.notify();
    }

    fn finish_scan(
        &mut self,
        id: u64,
        result: Result<Vec<EmptyCandidate>, String>,
        cx: &mut Context<Self>,
    ) {
        if id != self.scan_id || self.closed {
            return;
        }
        self.scanned_at = Local::now();
        match result {
            Ok(candidates) => {
                self.candidates = candidates;
                self.state = ScanState::Ready;
            }
            Err(error) => {
                self.candidates.clear();
                self.state = ScanState::Failed(error);
            }
        }
        cx.notify();
    }

    /// Choose the scope (Current workspace needs a workspace).
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if scope == Scope::Workspace && !workspace_available(cx) {
            return;
        }
        if self.scope != scope && !self.deleting {
            self.scope = scope;
            self.result.clear();
            self.rescan(cx);
        }
    }

    /// Check or uncheck "Include files modified in the last 10 min".
    pub fn set_include_recent(&mut self, include: bool, cx: &mut Context<Self>) {
        self.include_recent = include;
        cx.notify();
    }

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.candidates.len() {
            self.selected = Some(ix);
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// The candidate rows the last frame built.
    pub fn rendered_rows(&self) -> std::ops::Range<usize> {
        self.rendered.clone()
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The full path of the selected row (shown under the table).
    pub fn selected_path(&self) -> Option<String> {
        self.selected
            .and_then(|ix| self.candidates.get(ix))
            .map(|candidate| candidate.path().display().to_string())
    }

    /// The cell texts of row `ix` (what the table shows).
    pub fn row_cells(&self, ix: usize) -> Option<[String; 6]> {
        let candidate = self.candidates.get(ix)?;
        Some(row_cells(
            candidate,
            self.is_recent(candidate),
            self.scanned_at,
        ))
    }

    /// Up / Down / Home / End / PageUp / PageDown move the selected row.
    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let Some(last) = self.candidates.len().checked_sub(1) else {
            return;
        };
        let next = match event.keystroke.key.as_str() {
            "down" => self.selected.map_or(0, |ix| (ix + 1).min(last)),
            "up" => self.selected.map_or(0, |ix| ix.saturating_sub(1)),
            "pagedown" => self.selected.map_or(0, |ix| (ix + VISIBLE_ROWS).min(last)),
            "pageup" => self
                .selected
                .map_or(0, |ix| ix.saturating_sub(VISIBLE_ROWS)),
            "home" => 0,
            "end" => last,
            _ => return,
        };
        self.select(next, cx);
        cx.stop_propagation();
    }

    /// The elements of the visible candidate rows (the table is virtualized).
    fn build_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.rendered = range.clone();
        let theme = cx.theme();
        let (muted, active) = (theme.muted_foreground, theme.list_active);
        let mono = theme.mono_font_family.clone();
        let now = self.scanned_at;
        range
            .filter_map(|ix| {
                let candidate = self.candidates.get(ix)?;
                let recent = self.is_recent(candidate);
                let excluded = recent && !self.include_recent;
                let [session, project, users, lines, preview, modified] =
                    row_cells(candidate, recent, now);
                let selected = self.selected == Some(ix);
                // X-024b: the row's accessible name.
                let label = format!(
                    "{session} \u{b7} {project} \u{b7} {preview} \u{b7} {modified}{}",
                    if excluded {
                        " \u{b7} recent, excluded"
                    } else {
                        ""
                    }
                );
                Some(
                    h_flex()
                        .id(("empty-row", ix))
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(label)
                        .w_full()
                        .h(rems(ROW_REM))
                        .items_center()
                        .gap_2()
                        .px_2()
                        .text_xs()
                        .cursor_pointer()
                        .when(selected, |row| row.bg(active))
                        .when(excluded, |row| row.text_color(muted))
                        .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
                        .child(
                            div()
                                .w(rems(9.0))
                                .flex_none()
                                .font_family(mono.clone())
                                .child(session),
                        )
                        .child(div().w(rems(7.0)).flex_none().truncate().child(project))
                        .child(div().w(rems(3.0)).flex_none().child(users))
                        .child(div().w(rems(3.0)).flex_none().child(lines))
                        .child(div().flex_1().min_w_0().truncate().child(preview))
                        .child(
                            div()
                                .w(rems(7.0))
                                .flex_none()
                                .text_color(muted)
                                .child(modified),
                        )
                        .qa_id(format!("empty:row:{ix}"))
                        .into_any_element(),
                )
            })
            .collect()
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = true;
        close_dialog(DIALOG_ID, window, cx);
    }

    /// The Copy as text button.
    pub fn copy_report(&self, window: &mut Window, cx: &mut Context<Self>) {
        services(cx).clipboard_write(self.report_text(), cx);
        notify::info(window, cx, "Copied to clipboard");
    }

    /// Delete N… : confirm, then delete exactly the confirmed rows.
    pub fn request_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let confirmed = self.eligible();
        if confirmed.is_empty() || self.deleting || self.state != ScanState::Ready {
            return;
        }
        let count = confirmed.len();
        let items: Vec<PathItem> = confirmed
            .iter()
            .flat_map(|candidate| {
                std::iter::once(PathItem::file(candidate.path())).chain(
                    candidate
                        .artifact_dirs
                        .iter()
                        .map(|dir| PathItem::folder(dir.clone())),
                )
            })
            .collect();
        let title = format!(
            "Delete {count} empty transcript{}?",
            if count == 1 { "" } else { "s" }
        );
        self.shown_confirm = Some(ShownConfirm::new(&title, &items));
        let gone = cx.entity().downgrade();
        let options = ConfirmOptions::destructive(title, format!("Delete {count}"))
            .on_closed(move |cx| {
                gone.update(cx, |this, _| this.shown_confirm = None).ok();
            })
            .description("These will be removed from disk. This can't be undone.")
            .width_rem(40.0)
            .body(move |_, _| {
                PathList::new("delete-empty-paths")
                    .items(items.clone())
                    .max_rows(8)
                    .into_any_element()
            });
        let this = cx.entity().downgrade();
        let app: &mut App = cx;
        open_destructive_confirm(options, window, app, move |control, _, cx| {
            let control = control.clone();
            let confirmed = confirmed.clone();
            this.update(cx, |this, cx| this.start_delete(confirmed, control, cx))
                .ok();
            ConfirmOutcome::Busy
        });
    }

    fn start_delete(
        &mut self,
        confirmed: Vec<EmptyCandidate>,
        control: crate::widgets::ConfirmControl,
        cx: &mut Context<Self>,
    ) {
        self.deleting = true;
        cx.notify();
        let rx = spawn_thread("delete-empty-run", move || run_delete(&confirmed));
        self._delete_task = Some(cx.spawn(async move |this, cx| {
            let result = rx.recv().await.ok();
            cx.update(|cx| {
                if let Some(result) = &result {
                    announce_deleted(&result.deleted, cx);
                }
                control.close(cx);
                this.update(cx, |this, cx| this.finish_delete(result, cx))
                    .ok();
            });
        }));
    }

    fn finish_delete(&mut self, result: Option<DeleteResult>, cx: &mut Context<Self>) {
        self.deleting = false;
        let Some(result) = result else {
            self.result = vec!["Deleting failed unexpectedly.".to_string()];
            cx.notify();
            return;
        };
        let mut lines = vec![delete_empty_summary_line(true, result.deleted.len())];
        for (path, reason) in &result.skipped {
            lines.push(format!("Skipped ({reason}): {}", path.display()));
        }
        if let Some((path, error)) = &result.failure {
            let line = format!("Failed to delete {}: {error}", path.display());
            debug_buffer(cx).push(DebugLevel::Error, line.clone());
            lines.push(line);
        }
        self.result = lines;
        if !self.closed {
            self.rescan(cx);
        } else {
            cx.notify();
        }
    }
}

/// Broadcast the deleted transcripts (§11.15) and refresh the list.
pub fn announce_deleted(deleted: &[PathBuf], cx: &mut App) {
    if deleted.is_empty() {
        return;
    }
    CorpusBus::broadcast(CorpusNotice::Deleted(deleted.to_vec()), cx);
    if let Some(list) = main_panes(cx).list() {
        list.update(cx, |list, cx| list.refresh(cx));
    }
}

impl Render for DeleteEmptyDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, danger, warning) = (
            theme.muted_foreground,
            theme.border,
            theme.danger,
            theme.warning,
        );
        let mono = theme.mono_font_family.clone();
        let scanning = self.state == ScanState::Scanning;
        let ready = self.state == ScanState::Ready;
        let eligible = if ready { self.eligible().len() } else { 0 };
        let busy = scanning || self.deleting;
        let entity = cx.entity().downgrade();
        let scope_entity = entity.clone();
        let selected_path = self.selected_path();
        let count = self.candidates.len();
        let scroll = self.scroll.clone();
        let table_height = rems(ROW_REM * count.min(VISIBLE_ROWS) as f32);
        v_flex()
            .id("delete-empty-dialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(scope_control(
                        "empty-scope",
                        self.scope,
                        workspace_available(cx),
                        self.deleting,
                        move |scope, _, cx| {
                            scope_entity
                                .update(cx, |this, cx| this.set_scope(scope, cx))
                                .ok();
                        },
                    ))
                    .child(
                        Checkbox::new("empty-include-recent")
                            .label("Include files modified in the last 10 min")
                            .checked(self.include_recent)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_include_recent(*checked, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("empty-found")
                    .text_sm()
                    .when(matches!(self.state, ScanState::Failed(_)), |line| {
                        line.text_color(danger)
                    })
                    .child(self.found_line()),
            )
            .when(count > 0, |this| {
                this.child(
                    v_flex()
                        .id("empty-table")
                        .w_full()
                        .border_1()
                        .border_color(border)
                        .rounded(cx.theme().radius)
                        .child(
                            h_flex()
                                .id("empty-header")
                                .w_full()
                                .h(rems(ROW_REM))
                                .items_center()
                                .gap_2()
                                .px_2()
                                .text_xs()
                                .text_color(muted)
                                .border_b_1()
                                .border_color(border)
                                .child(div().w(rems(9.0)).flex_none().child(COLUMNS[0]))
                                .child(div().w(rems(7.0)).flex_none().child(COLUMNS[1]))
                                .child(div().w(rems(3.0)).flex_none().child(COLUMNS[2]))
                                .child(div().w(rems(3.0)).flex_none().child(COLUMNS[3]))
                                .child(div().flex_1().child(COLUMNS[4]))
                                .child(div().w(rems(7.0)).flex_none().child(COLUMNS[5]))
                                .qa_id("empty:header"),
                        )
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .h(table_height)
                                .child(
                                    uniform_list(
                                        "empty-rows",
                                        count,
                                        cx.processor(|this, range, _, cx| {
                                            this.build_rows(range, cx)
                                        }),
                                    )
                                    .size_full()
                                    .track_scroll(&self.scroll),
                                )
                                .child(Scrollbar::vertical(&scroll)),
                        ),
                )
            })
            .when_some(selected_path, |this, path| {
                this.child(
                    div()
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(muted)
                        .child(format!("Path of the selected row: {path}")),
                )
            })
            .when(count > 0 && self.selected.is_none(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Select a row (click, or Up/Down) to see its full path."),
                )
            })
            .when(
                self.candidates.iter().any(|c| self.is_recent(c)) && !self.include_recent,
                |this| {
                    this.child(div().text_xs().text_color(warning).child(
                        "⏱ = modified in the last 10 minutes; excluded until the box is checked.",
                    ))
                },
            )
            .children(
                self.result
                    .iter()
                    .map(|line| div().text_sm().child(SharedString::from(line.clone()))),
            )
            .child(
                div().text_xs().text_color(muted).child(
                    "Claude Code transcripts only; Pi and OMP conversations are never touched.",
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("empty-copy")
                            .label("Copy as text")
                            .small()
                            .disabled(!ready)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.copy_report(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("empty-close")
                            .label("Close")
                            .small()
                            .disabled(self.deleting)
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        Button::new("empty-rescan")
                            .label("Rescan")
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                    )
                    .child(
                        Button::new("empty-delete")
                            .label(format!("Delete {eligible}…"))
                            .small()
                            .danger()
                            .disabled(busy || eligible == 0)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.request_delete(window, cx)),
                            )
                            .qa_id("empty:delete"),
                    ),
            )
    }
}

/// Tools → Delete empty transcripts… (§5.10); `local` preselects Current workspace.
pub fn open_delete_empty(local: bool, window: &mut Window, cx: &mut App) {
    let existing = current_dialog(cx);
    if let Some(existing) = place_here(
        cx,
        window,
        DIALOG_ID,
        existing,
        |d: &mut DeleteEmptyDialog| d.closed = true,
    ) {
        existing.update(cx, |dialog, cx| dialog.rescan(cx));
        return;
    }
    let dialog = cx.new(|cx| DeleteEmptyDialog::new(local, cx));
    set_current(cx, &dialog, window);
    dialog.update(cx, |dialog, cx| dialog.rescan(cx));
    let focus = dialog.read(cx).focus.clone();
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: DIALOG_ID,
            title: "Delete empty transcripts",
            width_rem: 52.0,
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
