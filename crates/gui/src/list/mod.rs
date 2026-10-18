//! The conversation list pane (DESIGN §5.1, §10, §11.2–§11.5). WP1.
//!
//! `ListPane` owns core's `Browser` and `HistoryLoader` exclusively (UI
//! thread, bounded work only: every O(corpus) or I/O step runs on core
//! threads or GUI background tasks, K1–K4) and renders the visible rows
//! with a `uniform_list`. Results arrive through the waker: a bounded
//! `async_channel` wakes `_poll_task`, which drains the loader and the
//! search workers (`poll_all`).
//!
//! Frozen contracts this pane follows:
//! * key contexts: `ConversationList` on the pane root, `ListToolbar` on the
//!   toolbar (an F6 region with its own focus handle). The list's keys are
//!   bound in `ConversationList && !ListToolbar` (`keymap::CTX_LIST_KEYS`),
//!   so a focused toolbar button keeps Enter/Space/Tab/arrows;
//! * session actions resolve `selected_target()`; the main window flushes a
//!   pending preview in the capture phase before they arrive here
//!   (`Workspace`, PD2/L-085). What `routing::run_session_action` leaves to
//!   the pane: `SessionRoute::Open` → emit [`ListPaneEvent::OpenRequested`]
//!   (with the agent-panel tab of Search within / Outline / Read range);
//!   `SessionRoute::Viewer` (Copy message) → `cx.propagate()` so the window
//!   root forwards it to the viewer;
//! * row-menu items run `routing::row_menu_action(item)` on the target
//!   captured under the item's token (§5.2).
//!
//! Layout of the module: `load` (loader/refresh/reload state machine, UUID
//! lookup, semantic error), `rows` (row model and element), `states`
//! (empty/error/loading views), `toolbar`, `footer`, `picker_banner`,
//! `context_menu`, `workspace_picker`, `semantic_details`.

pub mod context_menu;
pub mod footer;
mod load;
pub mod picker_banner;
pub mod rows;
pub mod semantic_details;
mod states;
pub mod toolbar;
pub mod workspace_picker;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use claude_history_core::browser::{
    Browser, BrowserOptions, HistoryLoader, ListSearchMode, LoaderOptions, RenameError,
};
#[cfg(test)]
use claude_history_core::browser::{LoadEvent, UuidLookup};
use claude_history_core::display_format::HighlightQuery;
use claude_history_core::export::ExportOptions;
use claude_history_core::history::Conversation;
use claude_history_core::time_filter::TimeFilter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    ScrollStrategy, SharedString, Styled as _, Subscription, Task, UniformListScrollHandle, Window,
    div, px, uniform_list,
};

use crate::actions::*;
use crate::exit;
use crate::globals::{ActivePane, BusyTimer, UiBusy, app_state, update_app_state};
use crate::keymap::CTX_LIST;
use crate::model::{AgentTab, ConversationTarget, HeaderInfo, PaneFocus};
#[cfg(any(test, feature = "qa"))]
use crate::notify::ToastKind;
use crate::notify::{self, Toast};
use crate::persist::RememberedSearchMode;
use crate::workspace::routing::{self, SessionRoute};

use load::{ReloadState, ViewState};
use rows::{RowContext, RowStyle};

/// Events of the conversation list (§7.2).
#[derive(Clone, Debug, PartialEq)]
pub enum ListPaneEvent {
    /// Selection changed (any cause). `user` = keyboard/mouse navigation by the user.
    SelectionChanged {
        target: Option<ConversationTarget>,
        user: bool,
    },
    /// Enter / double-click / row-menu Open; with `agent_tab` also the agent
    /// panel on that tab (row menu Search within conversation… / Outline,
    /// Session → Search within / Outline / Read range with the list focused,
    /// D-029), which then takes the focus.
    OpenRequested {
        target: ConversationTarget,
        focus_viewer: bool,
        agent_tab: Option<AgentTab>,
    },
    /// Corpus changed: load finished, refresh, rename, delete, UUID load.
    CorpusChanged,
    /// Loading/refreshing state changed (footer, menus, palette enabled state).
    LoadStateChanged,
}

/// The browser's embedder factory, loading the shared model while the
/// semantic worker reports `InitializingModel`: the shared embedder loads its
/// model lazily at the first embedding call, which would hide the model load
/// behind `Embedding 0/n` (the TUI created the embedder eagerly). The warm-up
/// call embeds nothing; its result is ignored (a load failure shows again at
/// the first real call).
fn warmed_factory(
    inner: claude_history_core::browser::SemanticEmbedderFactory,
) -> claude_history_core::browser::SemanticEmbedderFactory {
    Arc::new(move || {
        let mut embedder = inner()?;
        let _ = embedder.embed_passages(&[]);
        Ok(embedder)
    })
}

/// Pasted text for the one-line search: `\r\n`, `\n`, `\r` and tabs become
/// single spaces (ADD C3, L-034).
pub fn sanitize_pasted(text: &str) -> String {
    text.replace("\r\n", "\n").replace(['\n', '\r', '\t'], " ")
}

/// The relative-time ticker interval (§7.5).
pub const TICK_INTERVAL: Duration = Duration::from_secs(30);
/// Frame samples kept for the `list.frame_*` probes.
const FRAME_SAMPLES_KEPT: usize = 600;
/// Row-menu tokens kept (older ones are forgotten).
const ROW_MENU_TOKENS_KEPT: usize = 8;
/// Visible-row fallback before the first layout.
const DEFAULT_VISIBLE_ROWS: usize = 10;
/// The pane width assumed before the first layout (rem).
const DEFAULT_PANE_REM: f32 = 30.0;

/// Measurements the render pass records for the next frame.
#[derive(Clone, Copy, Debug, Default)]
struct Metrics {
    /// The pane's width in px (last layout).
    pane_width: Option<Pixels>,
}

/// The conversation list.
pub struct ListPane {
    /// The pane root (the `ConversationList` context); focus lives in `search`.
    focus: FocusHandle,
    /// The toolbar region (`ListToolbar`, F6 stop between the search input
    /// and the viewer, AD16).
    toolbar_focus: FocusHandle,
    search: Entity<InputState>,
    browser: Browser,
    loader: HistoryLoader,
    /// The environment the loader runs with (carries the workspace).
    loader_env: Arc<claude_history_core::HistoryEnv>,
    /// `(loading, refreshing)` as last announced with `LoadStateChanged`.
    load_flags: (bool, bool),
    view: ViewState,
    scroll: UniformListScrollHandle,
    /// `HighlightQuery::parse` of the query, parsed once per query change.
    highlight: HighlightQuery,
    literal: bool,
    now: DateTime<Local>,
    /// The path whose `SelectionChanged` was emitted last.
    emitted_selection: Option<PathBuf>,
    /// `[tui] exclude_projects` (for the empty-scope state, L-079).
    exclude_projects: Vec<String>,
    time_filter: TimeFilter,
    time_label: Option<SharedString>,
    reload: ReloadState,
    semantic_suspended: bool,
    /// The semantic error already reported with a toast.
    semantic_error_toasted: Option<String>,
    /// UUIDs whose lookup is in flight / whose outcome was reported.
    uuid_in_flight: Option<String>,
    uuid_reported: Option<String>,
    uuid_task: Task<()>,
    project_paths: Vec<PathBuf>,
    /// Recent workspaces that exist (checked off-thread, never in the menu builder).
    recent_existing: Vec<PathBuf>,
    paths_generation: u64,
    paths_task: Task<()>,
    _poll_task: Task<()>,
    _tick_task: Task<()>,
    /// Runs of the 30 s ticker, and those that repainted because a painted
    /// row's time label changed (the idle probe, X-007).
    ticks: u64,
    tick_repaints: u64,
    /// The filtered positions the last frame painted.
    rendered_rows: std::ops::Range<usize>,
    row_menus: VecDeque<(u64, ConversationTarget)>,
    next_menu_token: u64,
    last_menu: Option<context_menu::RowMenuModel>,
    /// Calls of `Browser::set_query` made by this pane (probe `list.query_calls`).
    query_calls: usize,
    metrics: Rc<Cell<Metrics>>,
    busy_key_max: Duration,
    busy_key_last: Duration,
    busy_poll_max: Duration,
    /// The slowest poll that installed a finished load or refresh, and the
    /// share of it spent inside core's `HistoryLoader::poll` (the install).
    busy_install_max: Duration,
    busy_loader_install_max: Duration,
    busy_install_pane_max: Duration,
    /// The last rename apply / delete removal, measured inside the pane (the
    /// event subscribers run after it).
    busy_edit_last: Duration,
    /// UI-thread time of the last frames: `render` + building the visible rows.
    frame_samples: VecDeque<Duration>,
    /// `render`'s share of the frame being built.
    frame_pending: Duration,
    /// Wall-clock base of the virtual test clock.
    #[cfg(test)]
    clock_base: (DateTime<Local>, Instant),
    #[cfg(any(test, feature = "qa"))]
    toast_log: Vec<ToastRecord>,
    #[cfg(test)]
    visible_rows_override: Option<usize>,
    /// Tests drive the poll loop by hand (`step`) to hold the load mid-way.
    #[cfg(test)]
    poll_paused: bool,
    /// What the pane showed right after each poll that appended batches
    /// while the initial load was still running (L-001, L-002).
    #[cfg(test)]
    loading_snapshots: Vec<LoadingSnapshot>,
    _subscriptions: Vec<Subscription>,
}

/// The state right after a poll that appended batches mid-load (tests).
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoadingSnapshot {
    pub rows: usize,
    pub selected: Option<usize>,
    pub status: String,
    pub is_loading: bool,
}

#[cfg(test)]
thread_local! {
    /// The next `ListPane::new` on this thread starts with its poll loop paused.
    pub(crate) static PAUSE_NEW_POLLS: Cell<bool> = const { Cell::new(false) };
}

/// A toast the list pushed (tests and QA read them; `LastToast` only keeps the last text).
#[cfg(any(test, feature = "qa"))]
#[derive(Clone, Debug, PartialEq)]
pub struct ToastRecord {
    pub kind: ToastKind,
    pub text: String,
    pub buttons: Vec<String>,
}

impl EventEmitter<ListPaneEvent> for ListPane {}

impl Focusable for ListPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl ListPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search conversations…"));
        let focus = cx.focus_handle();
        let toolbar_focus = cx.focus_handle();

        let (env, workspace, mode, local, launch_filter, show_last, excluded, recents) = {
            let state = app_state(cx);
            (
                state.env.clone(),
                state.workspace.clone(),
                state.list_mode,
                state.launch.args.local,
                state.launch.time_filter,
                state.settings.show_last,
                state.settings.exclude_projects.clone(),
                state.ui.recent_workspaces.clone(),
            )
        };
        let level = Some(crate::debug_sink::capture_level(cx));

        // The waker: worker threads post to a one-slot channel, the poll task
        // drains the loader and the search workers on the UI thread.
        let (wake_tx, wake_rx) = async_channel::bounded::<()>(1);
        let waker: claude_history_core::browser::Waker = Arc::new(move || {
            let _ = wake_tx.try_send(());
        });

        let options = BrowserOptions::new(env.clone())
            .with_show_last(show_last)
            .with_default_mode(mode)
            .with_exclude_projects(excluded.clone())
            .with_workspace(workspace.clone())
            .with_workspace_filter(local && workspace.is_some())
            .with_waker(waker.clone())
            .with_embedder_factory(warmed_factory(crate::embedder::browser_factory(cx)));
        let browser = Browser::new_loading(options);
        let loader_env = env.clone();
        let mut loader = HistoryLoader::new(
            env,
            LoaderOptions::new(show_last)
                .with_debug_level(level)
                .with_time_filter(launch_filter)
                .with_waker(waker),
        );
        loader.start_initial_load();

        let poll_task = cx.spawn_in(window, async move |this, cx| {
            while wake_rx.recv().await.is_ok() {
                // Timed around the whole update: the install plus every
                // subscriber it notified (X-038, `UiBusy`).
                let timer = BusyTimer::start();
                let polled = this.update_in(cx, |this, window, cx| {
                    #[cfg(test)]
                    if this.poll_paused {
                        return None;
                    }
                    this.poll_all(window, cx)
                });
                match polled {
                    Ok(Some(span)) => {
                        let measured = timer.stop();
                        cx.update(|_, cx| UiBusy::record(cx, span, measured)).ok();
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
        let tick_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK_INTERVAL).await;
                let alive = this
                    .update(cx, |this, cx| {
                        let now = this.wall_now(cx);
                        // Repaint only when a painted row's time label
                        // changes ("5 minutes ago", a new day): an idle
                        // window draws nothing (X-007; integrator, Phase C).
                        let changed = this.row_times_change(now);
                        this.now = now;
                        this.ticks += 1;
                        if changed {
                            this.tick_repaints += 1;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &search,
                window,
                |this, state, event: &InputEvent, window, cx| {
                    // Input propagates Enter, so the `enter → OpenConversation`
                    // binding handles it; PressEnter is ignored here.
                    if let InputEvent::Change = event {
                        let value = state.read(cx).value();
                        this.on_search_changed(value, window, cx);
                    }
                },
            ),
            cx.on_focus_in(&focus, window, |this, window, cx| {
                let pane = if this.toolbar_focus.contains_focused(window, cx) {
                    PaneFocus::ListToolbar
                } else {
                    PaneFocus::List
                };
                ActivePane::set(cx, window.window_handle(), pane);
            }),
            // Focus magnet: the pane root itself never keeps focus; typing
            // always edits the query (AD1).
            cx.on_focus(&focus, window, |this, window, cx| {
                this.focus_search(window, cx);
            }),
        ];

        let now = Local::now();
        let mut this = Self {
            focus,
            toolbar_focus,
            search,
            browser,
            loader,
            loader_env,
            load_flags: (true, false),
            view: ViewState::Loading,
            scroll: UniformListScrollHandle::new(),
            highlight: HighlightQuery::parse(""),
            literal: false,
            now,
            emitted_selection: None,
            exclude_projects: excluded,
            time_filter: launch_filter,
            time_label: crate::widgets::time_range::launch_filter_label(&launch_filter, now),
            reload: ReloadState::default(),
            semantic_suspended: false,
            semantic_error_toasted: None,
            uuid_in_flight: None,
            uuid_reported: None,
            uuid_task: Task::ready(()),
            project_paths: Vec::new(),
            recent_existing: Vec::new(),
            paths_generation: 0,
            paths_task: Task::ready(()),
            _poll_task: poll_task,
            _tick_task: tick_task,
            ticks: 0,
            tick_repaints: 0,
            rendered_rows: 0..0,
            row_menus: VecDeque::new(),
            next_menu_token: 1,
            last_menu: None,
            query_calls: 0,
            metrics: Rc::new(Cell::new(Metrics::default())),
            busy_key_max: Duration::ZERO,
            busy_key_last: Duration::ZERO,
            busy_poll_max: Duration::ZERO,
            busy_install_max: Duration::ZERO,
            busy_loader_install_max: Duration::ZERO,
            busy_install_pane_max: Duration::ZERO,
            busy_edit_last: Duration::ZERO,
            frame_samples: VecDeque::new(),
            frame_pending: Duration::ZERO,
            #[cfg(test)]
            clock_base: (now, cx.background_executor().now()),
            #[cfg(any(test, feature = "qa"))]
            toast_log: Vec::new(),
            #[cfg(test)]
            visible_rows_override: None,
            #[cfg(test)]
            poll_paused: PAUSE_NEW_POLLS.with(Cell::get),
            #[cfg(test)]
            loading_snapshots: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.refresh_recent_workspaces(recents, cx);
        this
    }

    // ----- focus -----------------------------------------------------------------

    /// Focus the search input (the list's primary element).
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.search.focus_handle(cx).focus(window, cx);
    }

    /// Focus the toolbar region (F6, AD16). Tab moves on from there.
    pub fn focus_toolbar(&self, window: &mut Window, cx: &mut App) {
        self.toolbar_focus.focus(window, cx);
    }

    /// Whether focus is anywhere inside the Conversations pane (toolbar included).
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Whether focus is in the toolbar region.
    pub fn toolbar_contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.toolbar_focus.contains_focused(window, cx)
    }

    // ----- state accessors ---------------------------------------------------------

    /// The selected row's target (never the live selection of a later moment:
    /// callers capture it).
    pub fn selected_target(&self) -> Option<ConversationTarget> {
        self.browser
            .selected_conversation()
            .map(ConversationTarget::from_conversation)
    }

    /// The loaded conversation at `path` (live `project_path`).
    pub fn conversation(&self, path: &Path) -> Option<Arc<Conversation>> {
        self.conversation_index(path)
            .and_then(|index| self.browser.conversations().get(index).cloned())
    }

    /// Index (into `Browser::conversations`) of the loaded conversation at
    /// `path`: core's path index (CCR-2; X-038 keeps the UI-thread work of an
    /// edit off the corpus size), then, on Windows, a scan for the same file
    /// spelled with another case or separator.
    fn conversation_index(&self, path: &Path) -> Option<usize> {
        if let Some((index, _)) = self.browser.conversation_by_path(path) {
            return Some(index);
        }
        if cfg!(windows) {
            self.browser
                .conversations()
                .iter()
                .position(|conversation| path_matches(&conversation.path, path))
        } else {
            None
        }
    }

    /// The row (position in `Browser::filtered`) of `path`: the indexed
    /// conversation's row, else a scan (a file listed twice, another
    /// spelling).
    fn row_of_path(&self, path: &Path) -> Option<usize> {
        let filtered = self.browser.filtered();
        if let Some(index) = self.conversation_index(path)
            && let Some(row) = filtered.iter().position(|&row_index| row_index == index)
        {
            return Some(row);
        }
        let conversations = self.browser.conversations();
        filtered
            .iter()
            .position(|&index| path_matches(&conversations[index].path, path))
    }

    /// The distinct working directories recorded by the loaded Pi/OMP
    /// conversations (PD3 rule 3 (c), L-082: the main window passes them to
    /// `startup::apply_recorded_cwd_workspace` after the first load).
    pub fn recorded_agent_cwds(&self) -> Vec<PathBuf> {
        let mut cwds: Vec<PathBuf> = Vec::new();
        for conversation in self.browser.conversations() {
            if conversation.source == claude_history_core::history::Source::Claude {
                continue;
            }
            if let Some(cwd) = &conversation.cwd
                && !cwds.contains(cwd)
            {
                cwds.push(cwd.clone());
            }
        }
        cwds
    }

    /// The viewer header for `path` from the live corpus (`None`: not loaded).
    pub fn header_info(&self, path: &Path) -> Option<HeaderInfo> {
        self.conversation(path)
            .map(|conversation| HeaderInfo::from_conversation(&conversation))
    }

    /// Select the row of `path`; `false` when it is not listed.
    pub fn select_path(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        let Some(position) = self.row_of_path(path) else {
            return false;
        };
        if self.browser.selected() != Some(position) {
            self.browser.select(position);
            self.scroll_to_selection();
            self.emit_selection(false, cx);
        }
        cx.notify();
        true
    }

    /// Whether the initial load is still running.
    pub fn is_loading(&self) -> bool {
        matches!(self.view, ViewState::Loading)
    }

    pub fn mode(&self) -> ListSearchMode {
        self.browser.list_search_mode()
    }

    pub fn scope_is_workspace(&self) -> bool {
        self.browser.workspace_filter()
    }

    pub fn has_workspace(&self) -> bool {
        self.browser.has_project_context()
    }

    /// The prompt in front of the query: `❯`, or `Project ❯` with the
    /// Workspace scope (L-031).
    pub fn prompt_text(&self) -> &'static str {
        if self.scope_is_workspace() {
            "Project ❯"
        } else {
            "❯"
        }
    }

    /// The current query text.
    pub fn query(&self, cx: &App) -> SharedString {
        self.search.read(cx).value()
    }

    /// Distinct existing project paths of the corpus (workspace picker).
    pub fn project_paths(&self) -> &[PathBuf] {
        &self.project_paths
    }

    /// The active time filter and its chip label (`None` = Any time).
    pub fn time_filter(&self) -> (&TimeFilter, Option<&SharedString>) {
        (&self.time_filter, self.time_label.as_ref())
    }

    /// Whether a refresh or reload is running (footer indicator).
    pub fn is_refreshing(&self) -> bool {
        self.loader.is_refreshing()
    }

    // ----- commands ----------------------------------------------------------------

    /// Ctrl+L / F5 / toolbar / menu (no-op while a load runs, as upstream).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loader.is_running() {
            return;
        }
        let was_failed = matches!(self.view, ViewState::Failed(_));
        self.start_plain_refresh(cx);
        if was_failed && self.loader.is_running() {
            // Retry after a failed initial load: the spinner state again.
            self.view = ViewState::Loading;
            cx.emit(ListPaneEvent::LoadStateChanged);
        }
    }

    /// Apply a rename written in the background (K3).
    pub fn apply_renamed(
        &mut self,
        path: &Path,
        renamed: Conversation,
        cx: &mut Context<Self>,
    ) -> Result<(), RenameError> {
        let started = Instant::now();
        let result = self.browser.apply_renamed(path, renamed);
        if result.is_ok() {
            self.after_corpus_edit(cx);
        }
        self.busy_edit_last = started.elapsed();
        self.busy_poll_max = self.busy_poll_max.max(self.busy_edit_last);
        result
    }

    /// Drop deleted transcripts from the list (K4).
    pub fn remove_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let started = Instant::now();
        if self.browser.remove_paths(paths) > 0 {
            self.after_corpus_edit(cx);
        }
        self.busy_edit_last = started.elapsed();
        self.busy_poll_max = self.busy_poll_max.max(self.busy_edit_last);
    }

    /// `[tui] exclude_projects` changed in Settings.
    pub fn set_exclude_projects(&mut self, excluded: Vec<String>, cx: &mut Context<Self>) {
        self.exclude_projects = excluded.clone();
        self.browser.set_excluded_projects(excluded);
        self.after_filter_change(cx);
    }

    /// The excluded project names the list currently applies.
    pub fn excluded_projects(&self) -> &std::collections::HashSet<String> {
        self.browser.excluded_projects()
    }

    /// Whether previews show the last messages.
    pub fn show_last(&self) -> bool {
        self.browser.show_last()
    }

    /// The `[tui] show_last` setting changed (C-023, a config retry): later
    /// loads parse the other preview; reload now.
    pub fn set_show_last(&mut self, show_last: bool, cx: &mut Context<Self>) {
        if self.browser.show_last() == show_last {
            return;
        }
        self.browser.set_show_last(show_last);
        if !self.loader.is_running() {
            let mut options = self.loader.options().clone();
            options.show_last = show_last;
            self.loader = HistoryLoader::new(self.loader_env.clone(), options);
        }
        self.refresh(cx);
    }

    /// Semantic gate entered (§7.6): join the semantic worker, footer `sem paused`.
    pub fn suspend_semantic(&mut self, cx: &mut Context<Self>) {
        self.semantic_suspended = true;
        self.browser.suspend_semantic();
        cx.notify();
    }

    /// Semantic gate left: the worker restarts and reloads the cache.
    pub fn resume_semantic(&mut self, cx: &mut Context<Self>) {
        self.semantic_suspended = false;
        self.browser.resume_semantic();
        cx.notify();
    }

    /// File → Choose workspace… (native folder picker through `PlatformServices`).
    pub fn choose_workspace_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        workspace_picker::choose_folder(window, cx);
    }

    /// Ctrl+S: the semantic result details of the selected row (§5.19).
    pub fn show_semantic_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lines) = self
            .browser
            .semantic_result_metadata_for_selection()
            .map(semantic_details::detail_lines)
        else {
            return;
        };
        let Some(target) = self.selected_target() else {
            return;
        };
        semantic_details::open(lines, target, window, cx);
    }

    /// Handle an action forwarded by `workspace::routing` (no pane on the
    /// dispatch path). Returns `false` when the action is not a list action
    /// or needs the viewer (`CopyFocusedMessage`: the root forwards it there).
    pub fn handle_forwarded(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let any = action.as_any();
        if any.is::<SetModeLexical>() {
            self.set_mode(ListSearchMode::Lexical, cx);
        } else if any.is::<SetModeSemantic>() {
            self.set_mode(ListSearchMode::Semantic, cx);
        } else if any.is::<ToggleSearchMode>() {
            let next = self.mode().toggled();
            self.set_mode(next, cx);
        } else if any.is::<SetScopeAll>() {
            self.set_scope_workspace(false, cx);
        } else if any.is::<SetScopeWorkspace>() {
            self.set_scope_workspace(true, cx);
        } else if any.is::<ToggleScope>() {
            let next = !self.scope_is_workspace();
            self.set_scope_workspace(next, cx);
        } else if any.is::<ShowSemanticDetails>() {
            self.show_semantic_details(window, cx);
        } else if any.is::<Refresh>() {
            self.refresh(cx);
        } else if any.is::<CopyFocusedMessage>() {
            return false;
        } else if routing::is_session_action(action) {
            self.run_session_action(action, window, cx);
        } else {
            return false;
        }
        true
    }

    // ----- mode and scope ----------------------------------------------------------

    fn set_mode(&mut self, mode: ListSearchMode, cx: &mut Context<Self>) {
        if self.mode() == mode
            || (mode == ListSearchMode::Semantic && !self.browser.semantic_available())
        {
            return;
        }
        self.browser.set_list_search_mode(mode);
        self.remember_mode(mode, cx);
        self.semantic_error_toasted = None;
        self.after_filter_change(cx);
    }

    fn remember_mode(&self, mode: ListSearchMode, cx: &mut Context<Self>) {
        let remembered = match mode {
            ListSearchMode::Lexical => RememberedSearchMode::Lexical,
            ListSearchMode::Semantic => RememberedSearchMode::Semantic,
        };
        update_app_state(cx, |state, _| state.ui.search_mode = Some(remembered));
        crate::persist::schedule_save(cx);
    }

    fn set_scope_workspace(&mut self, workspace: bool, cx: &mut Context<Self>) {
        let enabled = workspace && self.has_workspace();
        if self.scope_is_workspace() == enabled {
            return;
        }
        self.browser.set_workspace_filter(enabled);
        self.after_filter_change(cx);
    }

    /// A change that re-runs the filter on the worker: the old rows stay until
    /// the answer arrives; the pane repaints now (prefix, toolbar, footer).
    fn after_filter_change(&mut self, cx: &mut Context<Self>) {
        self.emit_selection(false, cx);
        cx.emit(ListPaneEvent::LoadStateChanged);
        cx.notify();
    }

    // ----- query -------------------------------------------------------------------

    /// `InputEvent::Change`: one `Browser::set_query` per change (a paste is
    /// one change). `\n`/`\t` become spaces (ADD C3).
    fn on_search_changed(
        &mut self,
        value: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let started = Instant::now();
        let mut query = value.to_string();
        if query.contains(['\n', '\r', '\t']) {
            query = query.replace(['\n', '\r', '\t'], " ");
            let sanitized = query.clone();
            self.search
                .update(cx, |search, cx| search.set_value(sanitized, window, cx));
        }
        self.apply_query(query, window, cx);
        self.busy_key_last = started.elapsed();
        self.busy_key_max = self.busy_key_max.max(self.busy_key_last);
        crate::globals::record_sub_span(
            cx,
            "list.search_changed",
            crate::globals::BusyMeasure {
                wall: self.busy_key_last,
                cycles: None,
                window: Some((started, started + self.busy_key_last)),
            },
        );
    }

    /// A paste whose text has line breaks or tabs (ADD C3): they become
    /// spaces and the text is inserted as one edit, instead of the input
    /// dropping the newlines. Returns whether the paste was taken.
    pub(crate) fn handle_paste(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !text.contains(['\n', '\r', '\t']) {
            return false;
        }
        let sanitized = sanitize_pasted(text);
        self.search
            .update(cx, |search, cx| search.replace(sanitized, window, cx));
        true
    }

    fn apply_query(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.query_calls += 1;
        self.highlight = HighlightQuery::parse(&query);
        self.literal = rows::literal_mode_of(&self.highlight, &query);
        self.browser.set_query(query);
        self.check_uuid_lookup(window, cx);
        cx.notify();
    }

    fn on_clear_query(&mut self, _: &ClearQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.query(cx).is_empty() {
            if exit::is_picker(cx) {
                exit::quit_without_output(cx);
            }
            return;
        }
        self.clear_query(window, cx);
    }

    /// Empty the query and browse (`set_value` does not emit `Change`).
    pub(crate) fn clear_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.apply_query(String::new(), window, cx);
    }

    fn on_toggle_scope(&mut self, _: &ToggleScope, _: &mut Window, cx: &mut Context<Self>) {
        if !self.has_workspace() {
            // Without a workspace Tab moves focus on (Root focus cycling, F13).
            cx.propagate();
            return;
        }
        let next = !self.scope_is_workspace();
        self.set_scope_workspace(next, cx);
    }

    // ----- selection and navigation --------------------------------------------------

    /// Rows that fit the viewport (from the last layout).
    pub fn visible_rows(&self) -> usize {
        #[cfg(test)]
        if let Some(rows) = self.visible_rows_override {
            return rows;
        }
        let state = self.scroll.0.borrow();
        let Some(size) = state.last_item_size else {
            return DEFAULT_VISIBLE_ROWS;
        };
        let count = self.browser.filtered().len().max(1) as f32;
        let row_height = size.contents.height.as_f32() / count;
        if row_height <= 0.0 {
            return DEFAULT_VISIBLE_ROWS;
        }
        ((size.item.height.as_f32() / row_height).floor() as usize).max(1)
    }

    fn scroll_to_selection(&self) {
        if let Some(position) = self.browser.selected() {
            self.scroll
                .scroll_to_item(position, ScrollStrategy::Nearest);
        }
    }

    /// Emit `SelectionChanged` when the selected path differs from the last
    /// one emitted.
    fn emit_selection(&mut self, user: bool, cx: &mut Context<Self>) {
        let path = self.browser.selected_path();
        if path == self.emitted_selection {
            return;
        }
        self.emitted_selection = path;
        // Every selection change keeps the row visible (§10.5): a new result
        // set resets the selection to row 0 without going through navigate().
        self.scroll_to_selection();
        let target = self.selected_target();
        cx.emit(ListPaneEvent::SelectionChanged { target, user });
    }

    /// Keyboard navigation: apply, keep the selection visible, notify.
    fn navigate(&mut self, apply: impl FnOnce(&mut Browser, usize), cx: &mut Context<Self>) {
        let visible = self.visible_rows();
        apply(&mut self.browser, visible);
        self.scroll_to_selection();
        self.emit_selection(true, cx);
        cx.notify();
    }

    fn select_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(selected) = self.browser.selected() else {
            return;
        };
        let last = self.browser.filtered().len().saturating_sub(1);
        let next = if delta < 0 {
            selected.saturating_sub(delta.unsigned_abs())
        } else {
            (selected + delta as usize).min(last)
        };
        self.navigate(
            |browser, _| {
                browser.select(next);
            },
            cx,
        );
    }

    /// A row click (PD2): select + preview (the search input keeps focus);
    /// a double click opens in the viewer and moves focus there.
    pub(crate) fn on_row_click(
        &mut self,
        position: usize,
        path: &Path,
        double: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Rows swallow the mouse-down (no focus move): the search input keeps
        // the focus, so the Workspace previews the selection (PD2).
        self.focus_search(window, cx);
        // The row may have moved since it was painted: trust the path.
        let position = self.position_of(path).unwrap_or(position);
        if self.browser.selected() != Some(position) {
            self.browser.select(position);
        } else {
            // L-065: a click always previews the clicked row, also when it is
            // already selected - the viewer may show another conversation
            // (deep search / agent open, delete while unfocused). Forget the
            // dedupe so the event is emitted again.
            self.emitted_selection = None;
        }
        self.emit_selection(true, cx);
        if double && let Some(target) = self.selected_target() {
            cx.emit(ListPaneEvent::OpenRequested {
                target,
                focus_viewer: true,
                agent_tab: None,
            });
        }
        cx.notify();
    }

    fn position_of(&self, path: &Path) -> Option<usize> {
        self.row_of_path(path)
    }

    /// Right click on a row: select it, build the context menu for it (§5.2).
    pub(crate) fn on_row_right_click(
        &mut self,
        position: usize,
        path: &Path,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The chosen item is dispatched along the focused path: the list must
        // hold the focus for `RowMenu` to reach it.
        self.focus_search(window, cx);
        let position = self.position_of(path).unwrap_or(position);
        if self.browser.selected() != Some(position) {
            self.browser.select(position);
            self.emit_selection(true, cx);
        }
        cx.notify();
        let Some(target) = self.selected_target() else {
            return;
        };
        self.show_row_menu(target, at, window, cx);
    }

    /// Shift+F10 / Menu key: the menu of the selected row at its bounds.
    fn on_open_row_menu(
        &mut self,
        _: &OpenRowContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.selected_target() else {
            return;
        };
        let at = self.selected_row_anchor(window);
        self.show_row_menu(target, at, window, cx);
    }

    /// Where the keyboard menu appears: the selected row's left part.
    fn selected_row_anchor(&self, window: &Window) -> Point<Pixels> {
        let rem = window.rem_size().as_f32();
        let state = self.scroll.0.borrow();
        let bounds = state.base_handle.bounds();
        let offset = state.base_handle.offset();
        let row_px = state
            .last_item_size
            .map(|size| size.contents.height.as_f32() / self.browser.filtered().len().max(1) as f32)
            .unwrap_or(rem * 4.25);
        let position = self.browser.selected().unwrap_or(0) as f32;
        let y = bounds.origin.y + offset.y + px(position * row_px + row_px / 2.0);
        let y = y.clamp(bounds.origin.y, bounds.origin.y + bounds.size.height);
        Point::new(bounds.origin.x + window.rem_size() * 3.0, y)
    }

    fn on_row_menu(&mut self, action: &RowMenu, window: &mut Window, cx: &mut Context<Self>) {
        // The target was captured when the menu was built (A1).
        let target = self.row_menu_target(action.token);
        let options = routing::default_export_options(cx);
        let session_action = routing::row_menu_action(action.item);
        self.run_session_action_on(session_action.as_ref(), target, options, window, cx);
    }

    /// The target captured under `token` when a row menu was built.
    pub fn row_menu_target(&self, token: u64) -> Option<ConversationTarget> {
        self.row_menus
            .iter()
            .find(|(candidate, _)| *candidate == token)
            .map(|(_, target)| target.clone())
    }

    /// The last row menu that was built (tests, QA).
    pub fn last_row_menu(&self) -> Option<&context_menu::RowMenuModel> {
        self.last_menu.as_ref()
    }

    fn show_row_menu(
        &mut self,
        target: ConversationTarget,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = self.next_menu_token;
        self.next_menu_token += 1;
        self.row_menus.push_back((token, target.clone()));
        while self.row_menus.len() > ROW_MENU_TOKENS_KEPT {
            self.row_menus.pop_front();
        }
        let model = context_menu::build(token, &target, self.is_loading(), cx);
        context_menu::show(&model, at, window, cx);
        self.last_menu = Some(model);
    }

    // ----- session actions -----------------------------------------------------------

    /// Session actions act on the captured selection (A1).
    fn run_session_action(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.selected_target();
        let options = routing::default_export_options(cx);
        self.run_session_action_on(action, target, options, window, cx);
    }

    /// Run a session action on `target` (the selection, or a row-menu token's
    /// captured target) and finish what `ops` leaves to the pane.
    fn run_session_action_on(
        &mut self,
        action: &dyn Action,
        target: Option<ConversationTarget>,
        options: ExportOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action.as_any().is::<CopyFocusedMessage>() {
            // The viewer's focused message: the window root forwards it there.
            cx.propagate();
            return;
        }
        let Some(target) = target else {
            if !action.as_any().is::<OpenConversation>() {
                notify::no_conversation_selected(window, cx);
            }
            return;
        };
        // While loading, Resume/Fork/Rename/Delete/Select no-op (AD6, quirk 3).
        if self.is_loading()
            && (routing::needs_known_source(action) || action.as_any().is::<SelectAndExit>())
        {
            return;
        }
        match routing::run_session_action(action, target.clone(), options, window, cx) {
            SessionRoute::Open { agent_tab } => cx.emit(ListPaneEvent::OpenRequested {
                target,
                focus_viewer: true,
                agent_tab,
            }),
            SessionRoute::Viewer => cx.propagate(),
            SessionRoute::Done | SessionRoute::NotSession => {}
        }
    }

    // ----- toasts ----------------------------------------------------------------------

    /// Push `toast` and keep a record of it for tests and QA.
    fn toast(
        &mut self,
        toast: Toast,
        buttons: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(any(test, feature = "qa"))]
        self.toast_log.push(ToastRecord {
            kind: toast.kind(),
            text: toast.message().to_string(),
            buttons,
        });
        #[cfg(not(any(test, feature = "qa")))]
        let _ = buttons;
        toast.show(window, cx);
    }

    /// The toasts this pane pushed (oldest first).
    #[cfg(any(test, feature = "qa"))]
    pub fn toasts(&self) -> &[ToastRecord] {
        &self.toast_log
    }

    // ----- time ------------------------------------------------------------------------

    /// The current time: the wall clock (tests: the virtual clock of the
    /// executor, so `advance_clock` moves it).
    fn wall_now(&self, cx: &App) -> DateTime<Local> {
        #[cfg(test)]
        {
            let elapsed = cx.background_executor().now() - self.clock_base.1;
            self.clock_base.0
                + chrono::Duration::from_std(elapsed).unwrap_or_else(|_| chrono::Duration::zero())
        }
        #[cfg(not(test))]
        {
            let _ = cx;
            Local::now()
        }
    }

    // ----- probes ----------------------------------------------------------------------

    /// Test and QA probes (§15.3 keys `list.*`).
    #[cfg(any(test, feature = "qa"))]
    pub fn qa_probe(&self, key: &str, cx: &App) -> Option<String> {
        Some(match key {
            "list.count" => self.browser.filtered().len().to_string(),
            "list.total" => self.browser.conversations().len().to_string(),
            "list.loaded" => (!self.is_loading()).to_string(),
            "list.loading" => self.is_loading().to_string(),
            "list.refreshing" => self.is_refreshing().to_string(),
            "list.query" => self.query(cx).to_string(),
            "list.query_calls" => self.query_calls.to_string(),
            "list.mode" => self.mode().label().to_string(),
            "list.scope" => if self.scope_is_workspace() {
                "workspace"
            } else {
                "all"
            }
            .to_string(),
            "list.time" => self
                .time_label
                .as_ref()
                .map(|label| label.to_string())
                .unwrap_or_else(|| "Any time".to_string()),
            "list.status" => self.search_status().text,
            "list.selected_title" => self
                .selected_target()
                .map(|target| target.title.to_string())
                .unwrap_or_default(),
            "list.selected_path" => self
                .selected_target()
                .map(|target| target.path.display().to_string())
                .unwrap_or_default(),
            // Milliseconds: the slowest typed-key handler, the slowest
            // poll/install/rename/remove, the visible rows' build time.
            "list.ui_busy_max" => format!("{:.3}", self.busy_key_max.as_secs_f64() * 1000.0),
            // p95 / max of the UI-thread time to build a frame (render + the
            // visible rows), milliseconds.
            "list.frame_p95" => {
                let mut sorted: Vec<f64> = self
                    .frame_samples
                    .iter()
                    .map(|d| d.as_secs_f64() * 1000.0)
                    .collect();
                sorted.sort_by(f64::total_cmp);
                let ix = ((sorted.len() as f64 * 0.95).ceil() as usize)
                    .saturating_sub(1)
                    .min(sorted.len().saturating_sub(1));
                format!("{:.3}", sorted.get(ix).copied().unwrap_or(0.0))
            }
            "list.frame_max" => format!(
                "{:.3}",
                self.frame_samples
                    .iter()
                    .max()
                    .map(|d| d.as_secs_f64() * 1000.0)
                    .unwrap_or(0.0)
            ),
            "list.ui_busy_last" => format!("{:.3}", self.busy_key_last.as_secs_f64() * 1000.0),
            "list.edit_busy_last" => {
                format!("{:.3}", self.busy_edit_last.as_secs_f64() * 1000.0)
            }
            "list.loader_install_max" => {
                format!("{:.3}", self.busy_loader_install_max.as_secs_f64() * 1000.0)
            }
            "list.install_pane_max" => {
                format!("{:.3}", self.busy_install_pane_max.as_secs_f64() * 1000.0)
            }
            "list.searching" => self.browser.has_search_work_in_flight().to_string(),
            "list.install_busy_max" => {
                format!("{:.3}", self.busy_install_max.as_secs_f64() * 1000.0)
            }
            "list.poll_busy_max" => format!("{:.3}", self.busy_poll_max.as_secs_f64() * 1000.0),
            "list.frame_last" => format!(
                "{:.3}",
                self.frame_samples
                    .back()
                    .map(|d| d.as_secs_f64() * 1000.0)
                    .unwrap_or(0.0)
            ),
            "list.ticks" => self.ticks.to_string(),
            "list.tick_repaints" => self.tick_repaints.to_string(),
            // Rows of the current result that carry semantic scores, and
            // whether the selected one does (semantic-toggle.qa: semantic
            // result rows end to end; integrator, Phase C).
            "list.semantic_rows" => if self.mode() == ListSearchMode::Semantic {
                self.browser
                    .filtered()
                    .iter()
                    .filter(|ix| self.browser.semantic_result_metadata(**ix).is_some())
                    .count()
            } else {
                0
            }
            .to_string(),
            "list.selected_semantic" => self
                .browser
                .semantic_result_metadata_for_selection()
                .is_some()
                .to_string(),
            // The selected position in the filtered rows (L-098; integrator).
            "list.selected_index" => self
                .browser
                .selected()
                .map(|position| position.to_string())
                .unwrap_or_default(),
            _ => return None,
        })
    }
}

impl Render for ListPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_started = Instant::now();
        let theme = cx.theme();
        let sidebar = theme.sidebar;
        let roles = crate::theme_tokens::roles(cx);
        let status = self.search_status();
        let prompt = if self.scope_is_workspace() {
            div()
                .flex()
                .gap_1()
                .child(div().text_color(roles.muted_foreground).child("Project"))
                .child(div().text_color(roles.accent).child("❯"))
        } else {
            div().text_color(roles.accent).child("❯")
        };
        let search = self.search.clone();
        let toolbar = toolbar::render_toolbar(self, cx);
        let banner = picker_banner::render(cx);
        let body = self.render_body(window, cx);
        let footer = footer::render_footer(self, window, cx);
        let metrics = self.metrics.clone();
        let this = cx.entity().downgrade();

        let root = v_flex().id("conversation-list");
        let root = crate::on_session_actions!(root, cx, Self::run_session_action);
        let element = root
            .key_context(CTX_LIST)
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .bg(sidebar)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, _, window, cx| {
                    if !window.default_prevented() {
                        search.focus_handle(cx).focus(window, cx);
                    }
                }),
            )
            .on_action(cx.listener(Self::on_clear_query))
            .on_action(cx.listener(Self::on_toggle_scope))
            .on_action(cx.listener(Self::on_row_menu))
            .on_action(cx.listener(Self::on_open_row_menu))
            .on_action(cx.listener(|this, a: &SetModeLexical, w, cx| {
                this.handle_forwarded(a, w, cx);
            }))
            .on_action(cx.listener(|this, a: &SetModeSemantic, w, cx| {
                this.handle_forwarded(a, w, cx);
            }))
            .on_action(cx.listener(|this, a: &ToggleSearchMode, w, cx| {
                this.handle_forwarded(a, w, cx);
            }))
            .on_action(cx.listener(|this, a: &SetScopeAll, w, cx| {
                this.handle_forwarded(a, w, cx);
            }))
            .on_action(cx.listener(|this, a: &SetScopeWorkspace, w, cx| {
                this.handle_forwarded(a, w, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowSemanticDetails, w, cx| {
                this.show_semantic_details(w, cx);
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx)))
            .on_action(cx.listener(|this, _: &ChooseWorkspace, w, cx| {
                this.choose_workspace_folder(w, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.select_by(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_by(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| {
                let rows = this.visible_rows() as isize;
                this.select_by(-rows, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| {
                let rows = this.visible_rows() as isize;
                this.select_by(rows, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectHalfPageDown, _, cx| {
                this.navigate(
                    |browser, visible| browser.select_half_page_down(visible),
                    cx,
                );
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.navigate(|browser, _| browser.select_first(), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.navigate(|browser, _| browser.select_last(), cx);
            }))
            .child(
                div()
                    .w_full()
                    .h(px(0.0))
                    .on_children_prepainted(move |bounds, _, cx| {
                        // The pane's width, measured by a full-width probe row.
                        let width = bounds.first().map(|first| first.size.width);
                        let mut current = metrics.get();
                        let changed = width.is_some()
                            && current
                                .pane_width
                                .is_none_or(|old| (old - width.unwrap_or(old)).abs() > px(1.0));
                        if changed {
                            current.pane_width = width;
                            metrics.set(current);
                            this.update(cx, |_, cx| cx.notify()).ok();
                        }
                    })
                    .child(div().w_full().h(px(0.0))),
            )
            .child(
                div().px_2().pt_2().pb_1().child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .on_paste({
                            let this = cx.entity().downgrade();
                            move |item, window, cx| {
                                let Some(text) = item.text() else {
                                    return false;
                                };
                                this.update(cx, |list, cx| list.handle_paste(&text, window, cx))
                                    .unwrap_or(false)
                            }
                        })
                        .prefix(prompt)
                        .suffix(status.element(cx))
                        .aria_label("Search conversations"),
                ),
            )
            .child(toolbar)
            .when_some(banner, |this, banner| this.child(banner))
            .child(body)
            .child(footer);
        self.frame_pending = render_started.elapsed();
        element
    }
}

impl ListPane {
    /// The list area: rows, or the loading / empty / error state.
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        if let Some(state) = self.empty_state(cx) {
            return state.render(self, window, cx);
        }
        let count = self.browser.filtered().len();
        let scroll = self.scroll.clone();
        let list = uniform_list(
            "conversation-rows",
            count,
            cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                this.build_rows(range, window, cx)
            }),
        )
        .size_full()
        .track_scroll(&self.scroll);
        div()
            .id("conversation-rows-area")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(list)
            .child(Scrollbar::vertical(&scroll))
            .into_any_element()
    }

    /// The elements of the visible rows (built each frame; cheap: only the
    /// visible range, highlight ranges computed here and nowhere else).
    fn build_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let started = Instant::now();
        self.rendered_rows = range.clone();
        let roles = crate::theme_tokens::roles(cx);
        let mono = cx.theme().mono_font_family.clone();
        let rem = window.rem_size().as_f32();
        let pane_px = self
            .metrics
            .get()
            .pane_width
            .map(|width| width.as_f32())
            .unwrap_or(DEFAULT_PANE_REM * rem);
        let context = RowContext {
            query: &self.highlight,
            now: self.now,
            literal: self.literal,
            multiple_sources: self.browser.has_multiple_sources(),
            pane_width_px: pane_px,
            rem_px: rem,
        };
        let style = RowStyle {
            roles,
            owner: cx.entity().downgrade(),
            mono_font: mono,
        };
        let built = range
            .filter_map(|position| rows::build_row_model(&self.browser, position, &context))
            .map(|model| rows::render_row(model, &style))
            .collect();
        self.frame_samples
            .push_back(self.frame_pending + started.elapsed());
        while self.frame_samples.len() > FRAME_SAMPLES_KEPT {
            self.frame_samples.pop_front();
        }
        built
    }

    /// Whether a painted row's time label (text or recency colour) differs at
    /// `now` from the one shown (the ticker repaints only then).
    fn row_times_change(&self, now: DateTime<Local>) -> bool {
        self.rendered_rows.clone().any(|position| {
            self.browser.row(position).is_some_and(|conversation| {
                rows::format_row_time(conversation.timestamp, self.now)
                    != rows::format_row_time(conversation.timestamp, now)
            })
        })
    }

    /// Row models for the filtered positions in `range` (tests, QA).
    #[cfg(any(test, feature = "qa"))]
    pub fn row_models(
        &self,
        range: std::ops::Range<usize>,
        pane_width_px: f32,
    ) -> Vec<rows::RowModel> {
        let context = RowContext {
            query: &self.highlight,
            now: self.now,
            literal: self.literal,
            multiple_sources: self.browser.has_multiple_sources(),
            pane_width_px,
            rem_px: 16.0,
        };
        range
            .filter_map(|position| rows::build_row_model(&self.browser, position, &context))
            .collect()
    }

    /// A one-line dump of the pane's state for test failure messages.
    #[cfg(any(test, feature = "qa"))]
    pub fn debug_state(&self) -> String {
        format!(
            "view={:?} mode={:?} query={:?} rows={} total={} in_flight={} progress={:?} activity={:?} error={:?} status={:?} loader_running={} refreshing={}",
            self.view,
            self.mode(),
            self.browser.query(),
            self.browser.filtered().len(),
            self.browser.conversations().len(),
            self.browser.has_search_work_in_flight(),
            self.browser.semantic_progress(),
            self.browser.semantic_activity_status_text(),
            self.browser.semantic_error(),
            self.browser.search_status_text(),
            self.loader.is_running(),
            self.is_refreshing(),
        )
    }

    /// The rows of the filtered list as paths, in display order (tests).
    #[cfg(any(test, feature = "qa"))]
    pub fn filtered_paths(&self) -> Vec<PathBuf> {
        let conversations = self.browser.conversations();
        self.browser
            .filtered()
            .iter()
            .map(|&index| conversations[index].path.clone())
            .collect()
    }

    /// The browser (tests).
    #[cfg(test)]
    pub(crate) fn browser(&self) -> &Browser {
        &self.browser
    }

    /// The loader (tests).
    #[cfg(test)]
    pub(crate) fn loader(&self) -> &HistoryLoader {
        &self.loader
    }

    /// The UUID lookup outcome of the current query (tests).
    #[cfg(test)]
    pub(crate) fn uuid_lookup(&self) -> Option<&UuidLookup> {
        self.browser.uuid_lookup()
    }

    /// The scroll offset of the list (tests).
    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> Point<Pixels> {
        self.scroll.0.borrow().base_handle.offset()
    }

    /// Pretend `rows` rows fit the viewport (tests).
    #[cfg(test)]
    pub(crate) fn set_visible_rows(&mut self, rows: usize) {
        self.visible_rows_override = Some(rows);
    }

    /// One poll by hand (tests with a paused poll loop).
    #[cfg(test)]
    pub(crate) fn step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.poll_all(window, cx);
    }

    /// Pause or resume the automatic poll loop (tests).
    #[cfg(test)]
    pub(crate) fn set_poll_paused(&mut self, paused: bool) {
        self.poll_paused = paused;
    }

    /// The snapshots of mid-load polls (tests).
    #[cfg(test)]
    pub(crate) fn loading_snapshots(&self) -> &[LoadingSnapshot] {
        &self.loading_snapshots
    }

    /// Apply a load event as if the loader had produced it (tests).
    #[cfg(test)]
    pub(crate) fn inject_load_event(
        &mut self,
        event: LoadEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_load_event(event, window, cx);
    }
}

/// Whether two transcript paths name the same file, without allocating
/// (exact, then case/separator-insensitive on Windows). Compared from the end:
/// file names differ first, so a miss over a large corpus stays cheap.
fn path_matches(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let (Some(a), Some(b)) = (a.to_str(), b.to_str()) else {
            return crate::globals::same_path(a, b);
        };
        let norm = |c: char| if c == '/' { '\\' } else { c };
        a.chars().rev().map(norm).flat_map(char::to_lowercase).eq(b
            .chars()
            .rev()
            .map(norm)
            .flat_map(char::to_lowercase))
    } else {
        a.as_os_str() == b.as_os_str()
    }
}
