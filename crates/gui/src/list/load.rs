//! Loading, refreshing and reloading (DESIGN §11.2, §11.3, §11.5, §11.20,
//! §11.21): the poll loop that drains core's loader and search workers, the
//! load-event handling with its toasts, the time-filter / workspace reloads
//! that revert on failure, the UUID lookup and the semantic-error toast.
//! WP1. Methods of `ListPane` that live here are private to the `list`
//! module except the public API (`set_workspace`, `set_time_filter`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use claude_history_core::browser::{
    HistoryLoader, ListSearchMode, LoadEvent, RefreshPolicy, UuidLookup,
};
use claude_history_core::history::{self, Conversation};
use claude_history_core::time_filter::TimeFilter;
use claude_history_core::{HistoryEnv, WorkspaceScope};
use gpui_kit::{Context, SharedString, Window};

use crate::actions::ShowDebugLog;
use crate::globals::{BusySpan, app_state, update_app_state};
use crate::notify::Toast;

use super::{ListPane, ListPaneEvent};

/// What the list area shows besides rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ViewState {
    /// The initial load is running.
    Loading,
    /// Loaded (rows, or an empty/no-match state derived from the browser).
    Ready,
    /// The initial load failed before producing data: the error text.
    Failed(String),
}

/// The time filter a reload replaces (restored when the reload fails).
#[derive(Clone, Debug)]
pub(super) struct TimeUndo {
    filter: TimeFilter,
    label: Option<SharedString>,
}

/// The workspace a reload replaces (restored when the reload fails).
#[derive(Clone, Debug)]
pub(super) struct WorkspaceUndo {
    workspace: Option<WorkspaceScope>,
    scope: bool,
    env: Arc<HistoryEnv>,
}

/// The settings changes a reload applies. Rows stay as loaded under the old
/// settings until the reload succeeds; on failure every undo is applied.
#[derive(Clone, Debug, Default)]
pub(super) struct Changes {
    time: Option<TimeUndo>,
    workspace: Option<WorkspaceUndo>,
}

impl Changes {
    /// Merge a later change into this one: the first undo of each kind wins
    /// (it describes what the rows currently show).
    fn merge(&mut self, other: Changes) {
        if self.time.is_none() {
            self.time = other.time;
        }
        if self.workspace.is_none() {
            self.workspace = other.workspace;
        }
    }

    fn is_settings_change(&self) -> bool {
        self.time.is_some() || self.workspace.is_some()
    }
}

/// The running reload and the one waiting for the running load to end.
#[derive(Default)]
pub(super) struct ReloadState {
    active: Option<Changes>,
    queued: Option<Changes>,
}

impl ListPane {
    // ----- the poll loop -----------------------------------------------------------

    /// Drain the loader and the search workers (the waker fired): bounded
    /// work only (K1 moved the corpus preparation to a core thread). Returns
    /// the install this poll applied (the poll task times the whole update,
    /// subscribers included, as that span; X-038).
    pub(super) fn poll_all(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<BusySpan> {
        let started = Instant::now();
        self.now = self.wall_now(cx);
        let loader_started = Instant::now();
        let loader_poll = self.loader.poll(&mut self.browser);
        let loader_elapsed = loader_started.elapsed();
        self.browser.poll();
        #[cfg(test)]
        if loader_poll.batches > 0 && self.browser.is_loading() {
            let snapshot = super::LoadingSnapshot {
                rows: self.browser.filtered().len(),
                selected: self.browser.selected(),
                status: self.search_status().text,
                is_loading: self.is_loading(),
            };
            self.loading_snapshots.push(snapshot);
        }
        let installed = loader_poll.event.is_some();
        let span = match &loader_poll.event {
            Some(LoadEvent::Loaded { .. } | LoadEvent::NoHistory(_)) => Some(BusySpan::LoadEnd),
            Some(LoadEvent::Refreshed { .. }) => Some(BusySpan::RefreshEnd),
            _ => None,
        };
        if let Some(event) = loader_poll.event {
            self.on_load_event(event, window, cx);
        }
        self.check_semantic_error(window, cx);
        self.check_uuid_lookup(window, cx);
        self.emit_selection(false, cx);
        self.start_queued_reload(cx);
        let flags = (self.is_loading(), self.is_refreshing());
        if flags != self.load_flags {
            self.load_flags = flags;
            cx.emit(ListPaneEvent::LoadStateChanged);
        }
        self.busy_poll_max = self.busy_poll_max.max(started.elapsed());
        if installed {
            self.busy_loader_install_max = self.busy_loader_install_max.max(loader_elapsed);
            self.busy_install_max = self.busy_install_max.max(started.elapsed());
            // The pane's own share of an install (what the pane controls; the
            // loader's share is core's, see the hand-off CCRs).
            let pane = started.elapsed().saturating_sub(loader_elapsed);
            self.busy_install_pane_max = self.busy_install_pane_max.max(pane);
        }
        cx.notify();
        span
    }

    /// How a load ended (§11.2, §11.3, §11.20, §11.21).
    pub(super) fn on_load_event(
        &mut self,
        event: LoadEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            LoadEvent::Loaded { project_errors, .. } => {
                self.view = ViewState::Ready;
                self.warn_project_errors(project_errors, window, cx);
                self.after_corpus_load(cx);
            }
            LoadEvent::NoHistory(_) => {
                self.view = ViewState::Ready;
                self.after_corpus_load(cx);
            }
            LoadEvent::LoadFailed(error) => {
                self.view = ViewState::Failed(error.to_string());
                cx.emit(ListPaneEvent::LoadStateChanged);
            }
            LoadEvent::Refreshed {
                old,
                new,
                project_errors,
            } => {
                let was_loading = self.is_loading();
                self.view = ViewState::Ready;
                let changes = self.reload.active.take();
                if let Some(changes) = &changes
                    && changes.workspace.is_some()
                {
                    self.remember_workspace(cx);
                }
                let text = crate::notify::gui_text(
                    &LoadEvent::Refreshed {
                        old,
                        new,
                        project_errors,
                    }
                    .status_message()
                    .unwrap_or_default(),
                );
                if !was_loading && changes.as_ref().is_none_or(|c| !c.is_settings_change()) {
                    self.toast(
                        Toast::success(text).replacing::<RefreshToast>("refresh"),
                        Vec::new(),
                        window,
                        cx,
                    );
                }
                self.warn_project_errors(project_errors, window, cx);
                self.after_corpus_load(cx);
            }
            LoadEvent::RefreshIncomplete => {
                self.reload.active = None;
                if self.is_loading() {
                    // A retry of the initial load that ended incomplete: there is
                    // no list to keep, so show the error view again.
                    self.view = ViewState::Failed(
                        "The load was incomplete; some projects could not be read".to_string(),
                    );
                    cx.emit(ListPaneEvent::LoadStateChanged);
                    cx.notify();
                    return;
                }
                let text = crate::notify::gui_text("Refresh incomplete; keeping existing sessions");
                let this = cx.entity().downgrade();
                let toast = Toast::warning(text)
                    .replacing::<RefreshToast>("refresh")
                    .action("Show debug log", move |window, cx| {
                        let _ = &this;
                        window.dispatch_action(Box::new(ShowDebugLog), cx);
                    });
                self.toast(toast, vec!["Show debug log".to_string()], window, cx);
                cx.emit(ListPaneEvent::LoadStateChanged);
            }
            LoadEvent::RefreshFailed(error) => {
                self.refresh_failed(error.to_string(), true, window, cx);
            }
            LoadEvent::RefreshInterrupted => {
                self.refresh_failed(String::new(), false, window, cx);
            }
        }
    }

    /// A refresh or reload failed: revert what the reload was changing and
    /// say so (§11.3, §11.20, §11.21).
    fn refresh_failed(
        &mut self,
        error: String,
        has_text: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changes = self.reload.active.take().unwrap_or_default();
        if self.is_loading() {
            // A failed retry of the initial load: the error view again (it
            // carries the error and its recovery actions; no toast).
            self.view = ViewState::Failed(if has_text {
                error
            } else {
                "The load was interrupted".to_string()
            });
            cx.emit(ListPaneEvent::LoadStateChanged);
            cx.notify();
            return;
        }
        let mut reverted = false;
        let mut changes = changes;
        // A change queued behind the failed reload holds the user's latest
        // choice: keep that value, and make the committed state its undo.
        let mut kept_time = false;
        let mut kept_workspace = false;
        if let Some(queued) = self.reload.queued.as_mut() {
            if queued.time.is_some() && changes.time.is_some() {
                queued.time = changes.time.take();
                kept_time = true;
            }
            if queued.workspace.is_some() && changes.workspace.is_some() {
                queued.workspace = changes.workspace.take();
                kept_workspace = true;
            }
        }
        if kept_time {
            self.toast(
                Toast::error("Couldn't apply the time filter".to_string()),
                Vec::new(),
                window,
                cx,
            );
            reverted = true;
        }
        if kept_workspace {
            self.toast(
                Toast::error("Couldn't switch the workspace".to_string()),
                Vec::new(),
                window,
                cx,
            );
            reverted = true;
        }
        if let Some(undo) = changes.time {
            self.time_filter = undo.filter;
            self.time_label = undo.label;
            self.loader.set_time_filter(undo.filter);
            let reason = if has_text {
                error.clone()
            } else {
                "the load was interrupted".to_string()
            };
            self.toast(
                Toast::error(format!("Couldn't apply the time filter: {reason}")),
                Vec::new(),
                window,
                cx,
            );
            reverted = true;
        }
        if let Some(undo) = changes.workspace {
            self.revert_workspace(undo, cx);
            let reason = if has_text {
                error.clone()
            } else {
                "the load was interrupted".to_string()
            };
            self.toast(
                Toast::error(format!("Couldn't switch the workspace: {reason}")),
                Vec::new(),
                window,
                cx,
            );
            reverted = true;
        }
        if !reverted {
            let text = if has_text {
                format!("Refresh failed: {error}")
            } else {
                crate::notify::gui_text("Refresh failed; keeping existing sessions")
            };
            let this = cx.entity().downgrade();
            let toast = Toast::error(text)
                .replacing::<RefreshToast>("refresh")
                .action("Retry", move |_, cx| {
                    this.update(cx, |list, cx| list.refresh(cx)).ok();
                });
            self.toast(toast, vec!["Retry".to_string()], window, cx);
        }
        cx.emit(ListPaneEvent::LoadStateChanged);
        cx.notify();
    }

    /// `N project(s) could not be loaded. See Tools → Debug log.` (§11.2).
    fn warn_project_errors(&mut self, count: usize, window: &mut Window, cx: &mut Context<Self>) {
        if count == 0 {
            return;
        }
        let toast = Toast::warning(format!(
            "{count} project(s) could not be loaded. See Tools → Debug log."
        ))
        .action("Show debug log", |window, cx| {
            window.dispatch_action(Box::new(ShowDebugLog), cx);
        });
        self.toast(toast, vec!["Show debug log".to_string()], window, cx);
    }

    /// The corpus changed under the list (load, refresh, rename, delete, UUID
    /// load): the viewer header and the menus re-read, the workspace picker's
    /// project list is recomputed.
    fn after_corpus_load(&mut self, cx: &mut Context<Self>) {
        self.refresh_paths(true, cx);
        self.after_corpus_edit(cx);
    }

    /// A rename, delete, UUID load or load finished: selection and header.
    pub(super) fn after_corpus_edit(&mut self, cx: &mut Context<Self>) {
        self.emit_selection(false, cx);
        cx.emit(ListPaneEvent::CorpusChanged);
        cx.notify();
    }

    // ----- reloads -------------------------------------------------------------------

    /// Change the workspace: reload with `ApplyPartial`, revert on failure (§11.20).
    /// `AppState.workspace` is already the new one (the caller updated it).
    pub fn set_workspace(&mut self, workspace: Option<WorkspaceScope>, cx: &mut Context<Self>) {
        if self.browser.workspace() == workspace.as_ref() {
            return;
        }
        let undo = WorkspaceUndo {
            workspace: self.browser.workspace().cloned(),
            scope: self.scope_is_workspace(),
            env: self.loader_env.clone(),
        };
        if workspace.is_none() {
            self.browser.set_workspace_filter(false);
        }
        self.browser.set_workspace(workspace);
        self.after_filter_change(cx);
        self.start_reload(
            Changes {
                time: None,
                workspace: Some(undo),
            },
            cx,
        );
    }

    /// Change the time filter: reload with `ApplyPartial`, revert on failure (§11.21).
    pub fn set_time_filter(
        &mut self,
        filter: TimeFilter,
        label: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let label: Option<SharedString> = label.map(SharedString::from);
        if self.time_filter == filter && self.time_label == label {
            return;
        }
        let undo = TimeUndo {
            filter: self.time_filter,
            label: self.time_label.clone(),
        };
        self.time_filter = filter;
        self.time_label = label;
        self.start_reload(
            Changes {
                time: Some(undo),
                workspace: None,
            },
            cx,
        );
    }

    /// Start a reload now, or queue it behind the running load (§11.20).
    fn start_reload(&mut self, changes: Changes, cx: &mut Context<Self>) {
        if self.loader.is_running() {
            if changes.is_settings_change() {
                match &mut self.reload.queued {
                    Some(queued) => queued.merge(changes),
                    None => self.reload.queued = Some(changes),
                }
            }
            cx.notify();
            return;
        }
        let settings_change = changes.is_settings_change();
        let level = Some(crate::debug_sink::capture_level(cx));
        let level_changed = self.loader.options().debug_level != level;
        if changes.workspace.is_some() || level_changed {
            // The loader's environment carries the workspace (Pi's
            // `<workspace>/.pi/settings.json`): a new loader with the same
            // options; the Debug log level applies from this load on (T-022).
            let env = app_state(cx).env.clone();
            let mut options = self.loader.options().clone();
            options.debug_level = level;
            self.loader = HistoryLoader::new(env.clone(), options);
            self.loader_env = env;
        }
        self.loader.set_time_filter(self.time_filter);
        // A retry after a failed initial load tolerates project errors like
        // the initial load does (it applies the partial result).
        let policy = if settings_change || matches!(self.view, ViewState::Failed(_)) {
            RefreshPolicy::ApplyPartial
        } else {
            RefreshPolicy::KeepOnProjectError
        };
        if self.loader.start_refresh_with(policy) {
            self.reload.active = Some(changes);
            self.load_flags.1 = true;
            cx.emit(ListPaneEvent::LoadStateChanged);
        }
        cx.notify();
    }

    /// `refresh()` entry: a plain refresh.
    pub(super) fn start_plain_refresh(&mut self, cx: &mut Context<Self>) {
        self.start_reload(Changes::default(), cx);
    }

    fn start_queued_reload(&mut self, cx: &mut Context<Self>) {
        if self.loader.is_running() {
            return;
        }
        if let Some(changes) = self.reload.queued.take() {
            self.start_reload(changes, cx);
        }
    }

    fn revert_workspace(&mut self, undo: WorkspaceUndo, cx: &mut Context<Self>) {
        self.browser.set_workspace(undo.workspace.clone());
        self.browser
            .set_workspace_filter(undo.scope && undo.workspace.is_some());
        let workspace = undo.workspace;
        update_app_state(cx, |state, _| state.set_workspace(workspace));
        let options = self.loader.options().clone();
        self.loader = HistoryLoader::new(undo.env.clone(), options);
        self.loader_env = undo.env;
    }

    /// A workspace switch succeeded: remember it (recents, last workspace).
    fn remember_workspace(&mut self, cx: &mut Context<Self>) {
        let workspace = self.browser.workspace().map(|w| w.dir().to_path_buf());
        update_app_state(cx, |state, _| {
            state.ui.workspace = workspace.clone();
            if let Some(dir) = &workspace {
                state.ui.recent_workspaces.retain(|recent| recent != dir);
                state.ui.recent_workspaces.insert(0, dir.clone());
                state
                    .ui
                    .recent_workspaces
                    .truncate(crate::persist::MAX_RECENT_WORKSPACES);
            }
        });
        if let Some(dir) = workspace
            && !self.recent_existing.contains(&dir)
        {
            self.recent_existing.insert(0, dir);
        }
        crate::persist::schedule_save(cx);
    }

    // ----- project paths -------------------------------------------------------------

    /// Recompute the existing recent workspaces (and, with `projects`, the
    /// corpus's distinct project paths) on a background thread: the menu
    /// builders only read the cached results (PD3).
    pub(super) fn refresh_paths(&mut self, projects: bool, cx: &mut Context<Self>) {
        self.paths_generation += 1;
        let generation = self.paths_generation;
        let snapshot = projects.then(|| self.browser.corpus_snapshot());
        let recents = app_state(cx).ui.recent_workspaces.clone();
        self.paths_task = cx.spawn(async move |this, cx| {
            let (project_paths, recent_existing) = cx
                .background_executor()
                .spawn(async move {
                    let projects = snapshot.map(|corpus| {
                        let mut seen = HashSet::new();
                        let mut out: Vec<PathBuf> = Vec::new();
                        for conversation in corpus.iter() {
                            let Some(path) = &conversation.project_path else {
                                continue;
                            };
                            if !seen.insert(path.clone()) {
                                continue;
                            }
                            if super::workspace_picker::path_is_dir(path) {
                                out.push(path.clone());
                                if out.len() >= super::workspace_picker::MAX_PROJECTS {
                                    break;
                                }
                            }
                        }
                        out
                    });
                    let existing: Vec<PathBuf> = recents
                        .into_iter()
                        .filter(|path| super::workspace_picker::path_is_dir(path))
                        .collect();
                    (projects, existing)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.paths_generation != generation {
                    return;
                }
                if let Some(projects) = project_paths {
                    this.project_paths = projects;
                }
                this.recent_existing = recent_existing;
                cx.notify();
            })
            .ok();
        });
    }

    /// Construction: the recent workspaces' existence.
    pub(super) fn refresh_recent_workspaces(
        &mut self,
        _recents: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.refresh_paths(false, cx);
    }

    // ----- UUID jump -----------------------------------------------------------------

    /// After a query change or a poll: start the disk lookup of a pending
    /// UUID in the background, report a missing session (K2, §11.5).
    pub(super) fn check_uuid_lookup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.browser.uuid_lookup().cloned() {
            Some(UuidLookup::Pending { uuid }) => {
                if self.uuid_in_flight.as_deref() != Some(uuid.as_str()) {
                    self.start_uuid_lookup(uuid, window, cx);
                }
            }
            Some(UuidLookup::NotFound { uuid }) => {
                if self.uuid_reported.as_deref() != Some(uuid.as_str()) {
                    self.uuid_reported = Some(uuid.clone());
                    self.toast(
                        Toast::error(format!("Session not found: {uuid}")),
                        Vec::new(),
                        window,
                        cx,
                    );
                }
            }
            Some(UuidLookup::Found { .. }) => {}
            None => {
                self.uuid_reported = None;
                self.uuid_in_flight = None;
            }
        }
    }

    fn start_uuid_lookup(&mut self, uuid: String, window: &mut Window, cx: &mut Context<Self>) {
        self.uuid_in_flight = Some(uuid.clone());
        let env = self.browser.env().clone();
        let show_last = self.browser.show_last();
        self.uuid_task = cx.spawn_in(window, async move |this, cx| {
            let lookup_uuid = uuid.clone();
            let result: Option<Conversation> = cx
                .background_executor()
                .spawn(async move {
                    history::load_session_by_uuid(&env, &lookup_uuid, show_last)
                        .ok()
                        .flatten()
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.finish_uuid_lookup(uuid, result, window, cx)
            })
            .ok();
        });
    }

    fn finish_uuid_lookup(
        &mut self,
        uuid: String,
        loaded: Option<Conversation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.uuid_in_flight.as_deref() == Some(uuid.as_str()) {
            self.uuid_in_flight = None;
        }
        let started = Instant::now();
        self.browser.resolve_uuid_lookup(&uuid, loaded);
        self.check_uuid_lookup(window, cx);
        if matches!(
            self.browser.uuid_lookup(),
            Some(UuidLookup::Found {
                loaded_from_disk: true,
                ..
            })
        ) {
            self.after_corpus_edit(cx);
        }
        self.busy_poll_max = self.busy_poll_max.max(started.elapsed());
        cx.notify();
    }

    // ----- semantic error --------------------------------------------------------------

    /// A semantic failure is reported once per error text with recovery
    /// actions (quirk 8, §11.4).
    pub(super) fn check_semantic_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let error = self.browser.semantic_error().map(str::to_string);
        match error {
            Some(error) if self.semantic_error_toasted.as_deref() != Some(error.as_str()) => {
                self.semantic_error_toasted = Some(error.clone());
                let lexical = cx.entity().downgrade();
                let retry = cx.entity().downgrade();
                let toast = Toast::error(format!("Semantic search failed: {error}"))
                    .replacing::<SemanticErrorToast>("semantic")
                    .button("Switch to lexical", move |_, cx| {
                        lexical
                            .update(cx, |list, cx| list.switch_to_lexical(cx))
                            .ok();
                    })
                    .button("Retry", move |_, cx| {
                        retry.update(cx, |list, cx| list.retry_semantic(cx)).ok();
                    });
                self.toast(
                    toast,
                    vec!["Switch to lexical".to_string(), "Retry".to_string()],
                    window,
                    cx,
                );
            }
            None => self.semantic_error_toasted = None,
            _ => {}
        }
    }

    /// Empty-state / toast action: back to lexical search.
    pub(super) fn switch_to_lexical(&mut self, cx: &mut Context<Self>) {
        self.set_mode(ListSearchMode::Lexical, cx);
    }

    /// Empty-state / toast action: run the semantic search again.
    pub(super) fn retry_semantic(&mut self, cx: &mut Context<Self>) {
        self.semantic_error_toasted = None;
        self.browser.set_list_search_mode(ListSearchMode::Lexical);
        self.browser.set_list_search_mode(ListSearchMode::Semantic);
        self.after_filter_change(cx);
    }
}

/// Replace key of the refresh toasts (one at a time, §12).
struct RefreshToast;
/// Replace key of the semantic error toast.
struct SemanticErrorToast;
