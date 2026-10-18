//! The conversation viewer (DESIGN §9, §11.6–§11.7). WP2.
//!
//! Pipeline (§9.1, spike-viewer "B-first hybrid"): `open` posts the path to
//! the pane's `viewer-worker` (parse → `ConversationDocument` → items →
//! segments, single-flight with a generation); `install` swaps in the new
//! items (`ListState::reset_with_uniform_height` on open, a splice diff
//! plus an anchor on rebuilds); each frame renders only the visible items
//! (`render.rs`: lanes + one `SelText` per segment with style, syntax and
//! match runs).
//!
//! Handler placement (frozen contract, §7.8, §8.10):
//! * viewer and session actions are registered on the transcript column
//!   (`Viewer` context), never on the pane root: with the agent panel
//!   focused they reach the panel's handlers or the window root's fallback
//!   (`ActivePane::AgentPanel` → the panel's result-set target), not the
//!   viewer's conversation;
//! * Resume, Fork, Rename, Delete and SelectAndExit are registered only in
//!   `ViewerMode::Main`: a listener makes a menu item enabled, and
//!   single-file windows have none of them (PD14);
//! * `AgentPanelEscape` that the panel did not consume (nothing to clear or
//!   cancel; it propagates) focuses the transcript (§8.3).
//!
//! State machines are core's: `view::MessageNav` (J/K, focus mark),
//! `view::ViewSearch<SegMatch>` (find bar, n/N), `view::escape` (Esc order),
//! `capture_anchor_entry` / `find_entry_or_prev` (anchored rebuilds).

pub mod code;
pub mod header;
pub mod items;
pub mod lru;
pub mod nav;
pub mod render;
pub mod search;
pub mod segments;
pub mod seltext;
pub mod status;
pub mod toolbar;
pub mod worker;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use claude_history_core::export::ExportOptions;
use claude_history_core::theme::ThemeKind;
use claude_history_core::view::{
    ConversationDocument, MessageNav, ToolDisplayMode, ToolOutputId, ViewEscape, ViewPrefs,
    ViewSearch, ViewSearchMode, ViewerModel, capture_anchor_entry, escape as view_escape,
    find_entry_or_prev,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState,
    MouseButton, ParentElement as _, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    canvas, div, font, list, px,
};

use crate::actions::*;
use crate::deep_search::panel::AgentPanel;
use crate::globals::{ActivePane, app_state, same_path, update_app_state};
use crate::keymap::CTX_VIEWER;
use crate::model::{
    AgentTab, ConversationTarget, DebugLogTab, HeaderInfo, OpenRequest, PaneFocus, RevealKind,
    RevealRequest, ViewerMode,
};
use crate::notify::{self, Toast};
use crate::qa_ext::QaExt as _;
use crate::services::services;
use crate::theme_tokens;
use crate::workspace::routing;

use self::items::{BuildInfo, ItemId, ViewerItem};
use self::lru::{HandleCache, TokenCache};
use self::search::{MatchesByItem, SearchIndex, SegMatch};
use self::seltext::LinkHandler;
use self::worker::{BuildJob, BuildResult, ViewerWorker};
use gpui_kit::AnyWindowHandle;

/// Uniform item height hint (rem) for unmeasured rows (scrollbar size).
const ITEM_HEIGHT_HINT_REM: f32 = 3.0;
/// Overdraw of the transcript list (px, a measured runtime geometry value).
const LIST_OVERDRAW_PX: f32 = 600.0;
/// One `j`/`k` line (rem).
const LINE_REM: f32 = 1.5;
/// Width changes rebuild after this quiet time.
const COLS_DEBOUNCE: Duration = Duration::from_millis(100);
/// Content width (mono cells) before the first frame measured it.
/// The most recent handled actions kept for the V-096 probes.
const ACTION_LOG_CAP: usize = 256;

const DEFAULT_COLS: usize = 100;
/// Frames kept by the frame-time probe.
const FRAME_SAMPLES: usize = 600;

/// The toast of a partial copy (§9.5).
pub const PARTIAL_COPY_TOAST: &str =
    "Copied the visible part of the selection. Use Y, y or Export to copy more.";
/// The toast when the whole selection scrolled away (§9.5).
pub const NOTHING_COPIED_TOAST: &str = "Nothing copied: the selection scrolled out of view.";

/// Events of the viewer (§7.2).
#[derive(Clone, Debug, PartialEq)]
pub enum ViewerPaneEvent {
    /// Esc after unwinding / `q`: main window → focus the list; single-file →
    /// close the window.
    LeaveRequested,
    /// The viewer opened or cleared a conversation.
    TargetChanged(Option<ConversationTarget>),
}

/// What the transcript area shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LoadStatus {
    Empty,
    Loading,
    Ready,
    Failed(String),
}

/// An installed build.
struct Loaded {
    model: ViewerModel,
    doc: Arc<ConversationDocument>,
    items: Arc<Vec<ViewerItem>>,
    info: Arc<BuildInfo>,
    cols: usize,
}

/// Where a rebuild keeps the viewport (§9.6).
#[derive(Clone, Debug)]
struct Anchor {
    id: Option<ItemId>,
    offset: Pixels,
    /// Nav active: the focused message's first item sat this far below the
    /// viewport top.
    relative: Option<Pixels>,
    entry: Option<usize>,
}

/// What to do once a rebuild is installed.
#[derive(Clone, Debug)]
enum AfterRebuild {
    Nothing,
    /// A hidden match was expanded: recompute and reveal the current match.
    RevealCurrentMatch,
    /// Reveal an entry (agent reveal with a visibility hint).
    RevealEntry {
        entry: Option<usize>,
        not_found: String,
    },
}

enum PendingKind {
    Open { reveal: Option<RevealRequest> },
    Rebuild { anchor: Anchor, then: AfterRebuild },
}

struct PendingBuild {
    generation: u64,
    kind: PendingKind,
    submitted: Instant,
}

/// An exact reveal in progress: `(item, segment, byte)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RevealTarget {
    item: usize,
    segment: usize,
    byte: usize,
}

/// Performance probes (§15.8).
#[derive(Default)]
struct PerfProbe {
    frames: VecDeque<f64>,
    /// The UI thread's own CPU time of the same frames (QA builds on Windows,
    /// where the runner calibrated the thread cycle counter; integrator,
    /// Phase C): unlike the wall time it does not grow when the OS runs other
    /// threads in between.
    frames_cpu: VecDeque<f64>,
    /// `render()` of the last frame (element tree, items excluded).
    render_ms: f64,
    /// Items rendered by the list closure last frame, and their time.
    items_rendered: Rc<Cell<usize>>,
    items_ms: Rc<Cell<f64>>,
    install_ms: f64,
    build_ms: f64,
    open_to_content_ms: f64,
    toggle_ui_ms: f64,
    last_search_ms: f64,
}

impl PerfProbe {
    /// The last `n` frame times (all kept frames for `n >= FRAME_SAMPLES`).
    fn recent(&self, n: usize) -> Vec<f64> {
        let skip = self.frames.len().saturating_sub(n);
        self.frames.iter().skip(skip).copied().collect()
    }

    /// The same frames' UI-thread CPU times (empty where not measured).
    #[cfg(any(test, feature = "qa"))]
    fn recent_cpu(&self, n: usize) -> Vec<f64> {
        let skip = self.frames_cpu.len().saturating_sub(n);
        self.frames_cpu.iter().skip(skip).copied().collect()
    }

    fn p95_of(mut frames: Vec<f64>) -> f64 {
        if frames.is_empty() {
            return 0.0;
        }
        frames.sort_by(|a, b| a.total_cmp(b));
        frames[((frames.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)]
    }

    fn avg_of(frames: &[f64]) -> f64 {
        if frames.is_empty() {
            return 0.0;
        }
        frames.iter().sum::<f64>() / frames.len() as f64
    }

    /// The median (the mean of the middle two for an even count).
    #[cfg(any(test, feature = "qa"))]
    fn median_of(mut frames: Vec<f64>) -> f64 {
        if frames.is_empty() {
            return 0.0;
        }
        frames.sort_by(|a, b| a.total_cmp(b));
        let mid = frames.len() / 2;
        if frames.len() % 2 == 1 {
            frames[mid]
        } else {
            (frames[mid - 1] + frames[mid]) / 2.0
        }
    }

    fn p95(&self) -> f64 {
        Self::p95_of(self.recent(FRAME_SAMPLES))
    }

    fn avg(&self) -> f64 {
        Self::avg_of(&self.recent(FRAME_SAMPLES))
    }
}

/// The viewer of one conversation (main window right pane, or a single-file window).
pub struct ViewerPane {
    mode: ViewerMode,
    /// Focus of the transcript column (`Viewer` context).
    viewer_focus: FocusHandle,
    target: Option<ConversationTarget>,
    header: Option<HeaderInfo>,
    prefs: ViewPrefs,
    agent_panel: Entity<AgentPanel>,
    worker: ViewerWorker,
    status: LoadStatus,
    loaded: Option<Loaded>,
    pending: Option<PendingBuild>,
    list: ListState,
    nav: MessageNav,
    search: ViewSearch<SegMatch>,
    search_index: Option<SearchIndex>,
    matches_by_item: Rc<MatchesByItem>,
    find_input: Entity<InputState>,
    find_open: bool,
    /// The next find-input change came from code (not typing).
    find_programmatic: bool,
    cols: usize,
    /// Content width in mono cells as last measured, and the whole pane's.
    measured_cols: usize,
    pane_cells: usize,
    _cols_task: Task<()>,
    handles: Rc<RefCell<HandleCache>>,
    tokens: Rc<RefCell<TokenCache>>,
    _highlight_tasks: Vec<Task<()>>,
    hovered: Option<ToolOutputId>,
    hovered_link: Option<SharedString>,
    reveal: Option<RevealTarget>,
    reveal_y: Rc<Cell<Option<Point<Pixels>>>>,
    probe: Option<RevealTarget>,
    probe_point: Rc<Cell<Option<Point<Pixels>>>>,
    painted: Rc<RefCell<Vec<usize>>>,
    painted_last: Vec<usize>,
    /// The items holding the two ends of the last transcript selection seen
    /// (a selection whose end scrolls out of view disappears, §9.5).
    last_selection: Option<(Option<usize>, Option<usize>)>,
    /// The text of that selection while both ends were painted: gpui-kit
    /// drops a selection whose end scrolled out, this is its visible part
    /// (V-088).
    last_selected_text: Option<String>,
    /// Select All is active (V-088): the whole transcript is painted as
    /// selected and Copy takes all of its text. gpui-kit's window selection
    /// has no select-all, so the viewer keeps this itself; any new
    /// selection, click, Esc or other conversation ends it.
    select_all: bool,
    /// A single-file window's file error (`File not found: …`, D1).
    file_error: Option<String>,
    theme_kind: Option<ThemeKind>,
    perf: PerfProbe,
    /// Actions the viewer handled (V-096 action log).
    action_log: Vec<&'static str>,
    window: AnyWindowHandle,
    /// The latest build request (dropping it cancels an unstarted build).
    _build_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ViewerPaneEvent> for ViewerPane {}

impl Focusable for ViewerPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.viewer_focus.clone()
    }
}

impl ViewerPane {
    pub fn new(mode: ViewerMode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        code::warm_up();
        let viewer_focus = cx.focus_handle().tab_stop(true);
        let agent_panel = cx.new(|cx| AgentPanel::new(window, cx));
        let prefs = app_state(cx).view_defaults;
        let find_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find in conversation…"));
        let worker = ViewerWorker::new();
        let rem = window.rem_size();
        let list = ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW_PX))
            .with_uniform_item_height(rem * ITEM_HEIGHT_HINT_REM);
        let weak = cx.entity().downgrade();
        // gpui calls this inside the list state's `borrow_mut` (a wheel scroll), so
        // `after_scroll` (which reads the list) runs once that borrow is released.
        list.set_scroll_handler(move |_, _, cx| {
            let weak = weak.clone();
            cx.defer(move |cx| {
                weak.update(cx, |this, cx| this.after_scroll(cx)).ok();
            });
        });
        let subscriptions = vec![
            cx.on_focus_in(&viewer_focus, window, |_, window, cx| {
                ActivePane::set(cx, window.window_handle(), PaneFocus::Viewer);
            }),
            cx.subscribe_in(&find_input, window, Self::on_find_event),
        ];
        Self {
            mode,
            viewer_focus,
            target: None,
            header: None,
            prefs,
            agent_panel,
            worker,
            status: LoadStatus::Empty,
            loaded: None,
            pending: None,
            list,
            nav: MessageNav::new(0),
            search: ViewSearch::new(),
            search_index: None,
            matches_by_item: Rc::new(MatchesByItem::new()),
            find_input,
            find_open: false,
            find_programmatic: false,
            cols: DEFAULT_COLS,
            measured_cols: 0,
            pane_cells: 0,
            _cols_task: Task::ready(()),
            handles: Rc::new(RefCell::new(HandleCache::default())),
            tokens: Rc::new(RefCell::new(TokenCache::default())),
            _highlight_tasks: Vec::new(),
            hovered: None,
            hovered_link: None,
            reveal: None,
            reveal_y: Rc::new(Cell::new(None)),
            probe: None,
            probe_point: Rc::new(Cell::new(None)),
            painted: Rc::new(RefCell::new(Vec::new())),
            painted_last: Vec::new(),
            last_selection: None,
            last_selected_text: None,
            select_all: false,
            file_error: None,
            theme_kind: None,
            perf: PerfProbe::default(),
            action_log: Vec::new(),
            window: window.window_handle(),
            _build_task: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub fn mode(&self) -> ViewerMode {
        self.mode
    }

    // ----- public API (§17.4) ----------------------------------------------------

    /// Show `req.target` (keeps the previous content until the new one is
    /// installed); a request for the shown path only focuses/reveals. With
    /// `req.agent_tab` the agent panel opens on that tab and takes the focus.
    pub fn open(&mut self, req: OpenRequest, window: &mut Window, cx: &mut Context<Self>) {
        let same = self
            .target
            .as_ref()
            .is_some_and(|target| same_path(&target.path, &req.target.path));
        if !same {
            self.target = Some(req.target.clone());
            self.header = None;
            self.file_error = None;
            self.prefs = app_state(cx).view_defaults;
            self.start_open(req.reveal.clone(), cx);
            self.agent_panel.update(cx, |panel, cx| {
                panel.set_viewer_target(Some(req.target.clone()), cx)
            });
            cx.emit(ViewerPaneEvent::TargetChanged(Some(req.target)));
        } else if let Some(reveal) = req.reveal {
            self.reveal(reveal, window, cx);
        }
        match req.agent_tab {
            Some(tab) => self.open_agent_panel(tab, window, cx),
            None if req.focus => self.focus(window, cx),
            None => {}
        }
        cx.notify();
    }

    /// Show nothing (the open conversation was deleted).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.worker.invalidate();
        self._build_task = Task::ready(());
        self.pending = None;
        self.reset_view_state();
        self.loaded = None;
        self.list.reset(0);
        self.status = LoadStatus::Empty;
        self.file_error = None;
        if self.target.take().is_some() {
            self.header = None;
            self.agent_panel
                .update(cx, |panel, cx| panel.set_viewer_target(None, cx));
            cx.emit(ViewerPaneEvent::TargetChanged(None));
        }
        cx.notify();
    }

    /// The open conversation.
    pub fn target(&self) -> Option<&ConversationTarget> {
        self.target.as_ref()
    }

    /// The open conversation's view toggles (menu check marks).
    pub fn prefs(&self) -> Option<ViewPrefs> {
        self.target.as_ref().map(|_| self.prefs)
    }

    /// Focus the transcript.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.viewer_focus.focus(window, cx);
    }

    /// Whether focus is in the transcript or the agent panel.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.viewer_focus.contains_focused(window, cx)
            || self.agent_panel.read(cx).contains_focus(window, cx)
    }

    /// The header from the live corpus (`None`: the file stem only, V-006).
    pub fn set_header(&mut self, info: Option<HeaderInfo>, cx: &mut Context<Self>) {
        self.header = info;
        cx.notify();
    }

    /// Export options of the open conversation's current view.
    pub fn export_options(&self) -> Option<ExportOptions> {
        self.target.as_ref().map(|_| self.prefs.export_options())
    }

    /// The theme changed: drop theme-keyed caches, re-render (heights do not
    /// change with colors, so the viewport stays where it is).
    pub fn apply_theme(&mut self, cx: &mut Context<Self>) {
        self.tokens.borrow_mut().clear();
        self.theme_kind = None;
        cx.notify();
    }

    pub fn agent_panel(&self) -> &Entity<AgentPanel> {
        &self.agent_panel
    }

    /// Show the agent panel on `tab` and focus it.
    pub fn open_agent_panel(&mut self, tab: AgentTab, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_panel
            .update(cx, |panel, cx| panel.show_tab(tab, window, cx));
        cx.notify();
    }

    /// Reveal a message of the open conversation; `false` when `reveal` is
    /// for another file (the caller opens that one first).
    pub fn reveal(
        &mut self,
        reveal: RevealRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let same = self
            .target
            .as_ref()
            .is_some_and(|target| same_path(&target.path, &reveal.path));
        if !same {
            return false;
        }
        if let Some(PendingBuild {
            kind: PendingKind::Open { reveal: pending },
            ..
        }) = self.pending.as_mut()
        {
            *pending = Some(reveal);
            return true;
        }
        self.apply_reveal(reveal, window, cx);
        true
    }

    /// Handle an action forwarded by `workspace::routing`. Returns `false`
    /// when it is not a viewer or session action this viewer handles.
    pub fn handle_forwarded(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if action.as_any().is::<YankOrCopyMessage>() && self.nav.is_active() {
            return self.handle_viewer_action(action, window, cx);
        }
        if self.handle_viewer_action(action, window, cx) {
            return true;
        }
        if routing::is_session_action(action) {
            return self.run_session_action(action, window, cx);
        }
        false
    }

    /// Test and QA probes (§15.3 keys `viewer.*`).
    #[cfg(any(test, feature = "qa"))]
    pub fn qa_probe(&self, key: &str, cx: &App) -> Option<String> {
        let items = self.loaded.as_ref().map(|l| l.items.len()).unwrap_or(0);
        // The median of the last N frames (`viewer.frame_median_9`): an idle
        // frame judged over several, one scheduler hiccup does not decide it
        // (integrator, Phase C; perf-viewer.qa).
        if let Some(count) = key
            .strip_prefix("viewer.frame_median_")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n > 0)
        {
            return Some(format!(
                "{:.2}",
                PerfProbe::median_of(self.perf.recent(count))
            ));
        }
        // The same over the frames' UI-thread CPU time (`None` before the QA
        // runner calibrated the cycle counter).
        if let Some(count) = key
            .strip_prefix("viewer.frame_cpu_median_")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n > 0)
        {
            if self.perf.frames_cpu.is_empty() {
                return None;
            }
            return Some(format!(
                "{:.2}",
                PerfProbe::median_of(self.perf.recent_cpu(count))
            ));
        }
        // The page-through's frames by UI-thread CPU time (reported next to
        // the wall-clock targets: a busy desktop shows as wall >> CPU).
        if matches!(key, "viewer.frame_cpu_p95_100" | "viewer.frame_cpu_avg_100") {
            if self.perf.frames_cpu.is_empty() {
                return None;
            }
            let recent = self.perf.recent_cpu(100);
            return Some(format!(
                "{:.2}",
                if key.ends_with("p95_100") {
                    PerfProbe::p95_of(recent)
                } else {
                    PerfProbe::avg_of(&recent)
                }
            ));
        }
        Some(match key {
            // The header line as shown (title, project, counts; V-007).
            "viewer.header" => self
                .header_layout()
                .map(|layout| layout.text())
                .unwrap_or_default(),
            "viewer.path" => self
                .target
                .as_ref()
                .map(|t| t.path.display().to_string())
                .unwrap_or_default(),
            "viewer.items" => items.to_string(),
            "viewer.messages" => self
                .loaded
                .as_ref()
                .map(|l| l.info.nav_count)
                .unwrap_or(0)
                .to_string(),
            "viewer.nav_focus" => self
                .nav
                .highlighted()
                .map(|p| (p + 1).to_string())
                .unwrap_or_default(),
            "viewer.indicator" => self.indicator(),
            "viewer.search.count" => self.search.count().to_string(),
            "viewer.search.current" => {
                if self.search.count() == 0 {
                    "0".to_string()
                } else {
                    (self.search.current_index() + 1).to_string()
                }
            }
            "viewer.tool_display" => self.prefs.tool_display.status_label().to_string(),
            "viewer.thinking" => self.prefs.show_thinking.to_string(),
            "viewer.timing" => self.prefs.show_timing.to_string(),
            "viewer.frame_p95" => format!("{:.2}", self.perf.p95()),
            "viewer.frame_avg" => format!("{:.2}", self.perf.avg()),
            // The last 100 frames: a page-through of ≥ 100 steps measures
            // only its own frames (perf-viewer.qa).
            "viewer.frame_p95_100" => format!("{:.2}", PerfProbe::p95_of(self.perf.recent(100))),
            "viewer.frame_avg_100" => {
                format!("{:.2}", PerfProbe::avg_of(&self.perf.recent(100)))
            }
            "viewer.frames" => self.perf.frames.len().to_string(),
            "viewer.render_last" => format!("{:.2}", self.perf.render_ms),
            "viewer.items_last" => format!(
                "{} in {:.2}",
                self.perf.items_rendered.get(),
                self.perf.items_ms.get()
            ),
            "viewer.frame_last" => format!(
                "{:.2}",
                self.perf.frames.back().copied().unwrap_or_default()
            ),
            "viewer.install_ms" => format!("{:.2}", self.perf.install_ms),
            "viewer.build_ms" => format!("{:.2}", self.perf.build_ms),
            "viewer.first_content_ms" => format!("{:.2}", self.perf.open_to_content_ms),
            "viewer.toggle_total_ms" => {
                format!("{:.2}", self.perf.toggle_ui_ms + self.perf.install_ms)
            }
            "viewer.toggle_ms" => format!("{:.2}", self.perf.toggle_ui_ms),
            "viewer.search_ms" => format!("{:.2}", self.perf.last_search_ms),
            "viewer.loading" => self.is_loading().to_string(),
            "viewer.expanded" => self
                .loaded
                .as_ref()
                .map(|l| l.model.expanded.len())
                .unwrap_or(0)
                .to_string(),
            "viewer.nav_active" => self.nav.is_active().to_string(),
            "viewer.search.mode" => format!("{:?}", self.search.mode()).to_lowercase(),
            "viewer.agent_panel" => self.agent_panel.read(cx).is_visible().to_string(),
            "viewer.find" => self.find_bar_text(),
            "viewer.status" => match &self.status {
                LoadStatus::Empty => "empty".to_string(),
                LoadStatus::Loading => "loading".to_string(),
                LoadStatus::Ready => "ready".to_string(),
                LoadStatus::Failed(error) => format!("failed: {error}"),
            },
            _ => return None,
        })
    }

    // ----- crate API (tests, single-file window, renderer) -----------------------

    /// An open or rebuild is in flight.
    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    /// The installed items.
    pub fn items(&self) -> Option<Arc<Vec<ViewerItem>>> {
        self.loaded.as_ref().map(|loaded| loaded.items.clone())
    }

    /// The navigation tables of the installed build.
    pub fn build_info(&self) -> Option<Arc<BuildInfo>> {
        self.loaded.as_ref().map(|loaded| loaded.info.clone())
    }

    /// The installed model.
    pub fn model(&self) -> Option<&ViewerModel> {
        self.loaded.as_ref().map(|loaded| &loaded.model)
    }

    /// The transcript list state.
    pub fn list_state(&self) -> &ListState {
        &self.list
    }

    pub fn nav(&self) -> &MessageNav {
        &self.nav
    }

    pub fn view_search(&self) -> &ViewSearch<SegMatch> {
        &self.search
    }

    /// Whether the lazy search texts exist (V-102).
    pub fn has_search_index(&self) -> bool {
        self.search_index.is_some()
    }

    /// Builds the worker completed (V-101).
    pub fn completed_builds(&self) -> usize {
        self.worker.completed_builds()
    }

    /// The content width in mono cells the items were built for.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Record a handled action (bounded: the newest `ACTION_LOG_CAP`).
    fn log_action(&mut self, name: &'static str) {
        if self.action_log.len() >= ACTION_LOG_CAP {
            self.action_log.remove(0);
        }
        self.action_log.push(name);
    }

    /// The viewer's actions handled so far (V-096).
    pub fn action_log(&self) -> &[&'static str] {
        &self.action_log
    }

    /// The hovered tool output (V-017).
    pub fn hovered(&self) -> Option<&ToolOutputId> {
        self.hovered.as_ref()
    }

    /// The find bar text: `/` + query + input status (V-012).
    pub fn find_bar_text(&self) -> String {
        format!("/{}{}", self.search.query(), self.search.input_status())
    }

    /// Whether the find input is shown.
    pub fn find_open(&self) -> bool {
        self.find_open
    }

    /// The `[i/N]` indicator (V-009).
    pub fn indicator(&self) -> String {
        match &self.loaded {
            Some(loaded) => nav::indicator(
                &loaded.items,
                &loaded.info,
                self.list.logical_scroll_top().item_ix,
            ),
            None => "[1/1]".to_string(),
        }
    }

    /// The header laid out for the current width (V-004, V-006).
    pub fn header_layout(&self) -> Option<header::HeaderLayout> {
        let target = self.target.as_ref()?;
        Some(header::layout(
            self.header.as_ref(),
            &target.file_stem,
            self.pane_cells.max(60),
        ))
    }

    /// The status hints shown with focus in the transcript (V-011).
    pub fn status_hint_texts(&self, window: &Window) -> Vec<String> {
        if let Some(counter) = self.search.match_counter() {
            return vec![status::search_strip(&counter, window)];
        }
        status::trim(status::hints(self.mode, window), self.hint_cells())
            .iter()
            .map(status::Hint::text)
            .collect()
    }

    /// The error state of a single-file window (`File not found: …`, D1).
    pub fn set_file_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.file_error = error;
        cx.notify();
    }

    /// The file error shown instead of the transcript.
    pub fn file_error(&self) -> Option<&str> {
        self.file_error.as_deref()
    }

    /// The open-failure text (`Failed to open: …`).
    pub fn failure(&self) -> Option<String> {
        match &self.status {
            LoadStatus::Failed(error) => Some(failed_to_open(error)),
            _ => None,
        }
    }

    /// A single-file window learned the conversation's source (parsed in
    /// the background): update the captured target in place.
    pub fn set_target(&mut self, target: ConversationTarget, cx: &mut Context<Self>) {
        if self
            .target
            .as_ref()
            .is_some_and(|current| same_path(&current.path, &target.path))
        {
            self.target = Some(target.clone());
            self.agent_panel
                .update(cx, |panel, cx| panel.set_viewer_target(Some(target), cx));
            cx.notify();
        }
    }

    /// The render snapshot of the next frame (tests: hover, focus mark).
    #[cfg(test)]
    pub fn render_ctx_snapshot(&self) -> (Option<ToolOutputId>, Option<usize>) {
        (self.hovered.clone(), self.nav.highlighted())
    }

    /// Ask the next frame for the window position of `byte` in segment
    /// `segment` of item `item` (tests: clicks on links and text).
    pub fn request_text_probe(
        &mut self,
        item: usize,
        segment: usize,
        byte: usize,
        cx: &mut Context<Self>,
    ) {
        self.probe_point.set(None);
        self.probe = Some(RevealTarget {
            item,
            segment,
            byte,
        });
        cx.notify();
    }

    /// The probed position, once painted.
    pub fn text_probe(&self) -> Option<Point<Pixels>> {
        self.probe_point.get()
    }

    /// Items painted in the last frame (partial copy, tests).
    pub fn painted_items(&self) -> Vec<usize> {
        let current = self.painted.borrow();
        if current.is_empty() {
            self.painted_last.clone()
        } else {
            current.clone()
        }
    }

    /// Frame-time probe: average and 95th percentile of the last frames (ms,
    /// render to paint end of the viewer).
    pub fn frame_times(&self) -> (f64, f64) {
        (self.perf.avg(), self.perf.p95())
    }

    /// The syntect token cache size (V-092).
    pub fn token_cache_len(&self) -> usize {
        self.tokens.borrow().lru.len()
    }

    /// The cached tokens with their keys (item, segment, theme kind; V-092).
    #[cfg(test)]
    pub fn cached_tokens(&self) -> Vec<(lru::TokenKey, code::Tokens)> {
        self.tokens.borrow().lru.entries()
    }

    /// The theme kind the last frame was drawn in (X-033, V-092).
    #[cfg(test)]
    pub fn drawn_theme(&self) -> Option<claude_history_core::theme::ThemeKind> {
        self.theme_kind
    }

    /// Completed syntax highlighting batches.
    pub fn highlight_batches(&self) -> usize {
        self.tokens.borrow().batches_done
    }

    /// Live selection handles.
    pub fn selection_handles(&self) -> usize {
        self.handles.borrow().len()
    }

    // ----- open / install ----------------------------------------------------------

    fn reset_view_state(&mut self) {
        self.nav = MessageNav::new(0);
        self.search = ViewSearch::new();
        self.search_index = None;
        self.matches_by_item = Rc::new(MatchesByItem::new());
        self.find_open = false;
        self.hovered = None;
        self.hovered_link = None;
        self.reveal = None;
        self.reveal_y.set(None);
        self.last_selection = None;
        self.last_selected_text = None;
        self.select_all = false;
        self.handles.borrow_mut().clear();
        self.tokens.borrow_mut().clear();
    }

    /// Post a build to the background executor (latest request wins).
    fn submit(&mut self, job: BuildJob, cx: &mut Context<Self>) -> u64 {
        let generation = self.worker.begin();
        let work = self.worker.job(generation, job, self.cols);
        let handle = self.window;
        self._build_task = cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { work() }).await;
            let Some(result) = result else {
                return;
            };
            cx.update_window(handle, |_, window, cx| {
                this.update(cx, |this, cx| this.install(result, window, cx))
                    .ok();
            })
            .ok();
        });
        generation
    }

    fn start_open(&mut self, reveal: Option<RevealRequest>, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        self.reset_view_state();
        self.status = LoadStatus::Loading;
        let generation = self.submit(
            BuildJob::Open {
                path: target.path.clone(),
                prefs: self.prefs,
            },
            cx,
        );
        self.pending = Some(PendingBuild {
            generation,
            kind: PendingKind::Open { reveal },
            submitted: Instant::now(),
        });
        cx.notify();
    }

    fn install(&mut self, result: BuildResult, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation == result.generation);
        if !current {
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        let started = Instant::now();
        match result.outcome {
            Err(error) => {
                self.loaded = None;
                self.list.reset(0);
                self.status = LoadStatus::Failed(error.clone());
                // Single-file windows show their own error view (§11.12).
                if result.opened && self.mode == ViewerMode::Main {
                    Toast::error(failed_to_open(&error))
                        .replacing::<OpenFailed>("open")
                        .show(window, cx);
                }
            }
            Ok(built) => {
                // Built at a stale width (the pane was measured while it was
                // building): rebuild at the measured one before showing it.
                if matches!(pending.kind, PendingKind::Open { .. })
                    && built.info.cols_dependent
                    && self.measured_cols > 0
                    && built.cols != self.measured_cols
                {
                    self.cols = self.measured_cols;
                    let generation = self.submit(BuildJob::Rebuild { model: built.model }, cx);
                    self.pending = Some(PendingBuild {
                        generation,
                        kind: pending.kind,
                        submitted: pending.submitted,
                    });
                    return;
                }
                self.perf.build_ms = built.build_ms;
                let rem = window.rem_size();
                match pending.kind {
                    PendingKind::Open { reveal } => {
                        self.reset_view_state();
                        self.list.reset_with_uniform_height(
                            built.items.len(),
                            rem * ITEM_HEIGHT_HINT_REM,
                        );
                        self.list.scroll_to(ListOffset::default());
                        self.nav = MessageNav::new(built.info.nav_count);
                        self.loaded = Some(Loaded {
                            model: built.model,
                            doc: built.doc,
                            items: built.items,
                            info: built.info,
                            cols: built.cols,
                        });
                        self.status = LoadStatus::Ready;
                        self.perf.open_to_content_ms =
                            pending.submitted.elapsed().as_secs_f64() * 1000.0;
                        if let Some(reveal) = reveal {
                            self.apply_reveal(reveal, window, cx);
                        }
                    }
                    PendingKind::Rebuild { anchor, then } => {
                        self.install_rebuild(built, anchor, rem, cx);
                        self.after_rebuild(then, window, cx);
                    }
                }
            }
        }
        self.perf.install_ms = started.elapsed().as_secs_f64() * 1000.0;
        cx.notify();
    }

    fn install_rebuild(
        &mut self,
        built: worker::Built,
        anchor: Anchor,
        rem: Pixels,
        _cx: &mut Context<Self>,
    ) {
        let old: Vec<(ItemId, u64)> = self
            .loaded
            .as_ref()
            .map(|loaded| {
                loaded
                    .items
                    .iter()
                    .map(|item| (item.id.clone(), item.version))
                    .collect()
            })
            .unwrap_or_default();
        let new: Vec<(ItemId, u64)> = built
            .items
            .iter()
            .map(|item| (item.id.clone(), item.version))
            .collect();
        if self.list.item_count() != old.len() {
            self.list.reset(new.len());
        } else {
            for splice in items::splice_diff(&old, &new) {
                self.list
                    .splice(splice.start..splice.start + splice.removed, splice.inserted);
            }
        }
        // Give the inserted items a height hint (measured ones keep theirs).
        let _ = self
            .list
            .clone()
            .with_uniform_item_height(rem * ITEM_HEIGHT_HINT_REM);

        let index: std::collections::HashMap<&ItemId, usize> = new
            .iter()
            .enumerate()
            .map(|(ix, (id, _))| (id, ix))
            .collect();
        self.handles
            .borrow_mut()
            .retain(|id| index.get(id).copied());

        // Resolve the anchor (§9.6).
        let resolved_ix = anchor.id.as_ref().and_then(|id| index.get(id).copied());
        let (item_ix, offset, exact) = match resolved_ix {
            Some(ix) => (ix, anchor.offset, true),
            None => {
                let ix = anchor
                    .entry
                    .and_then(|entry| find_entry_or_prev(&built.info.nav_entries, entry))
                    .and_then(|position| nav::first_item_of(&built.info, position))
                    .unwrap_or(0);
                (ix, px(0.), false)
            }
        };
        if built.items.is_empty() {
            self.list.scroll_to(ListOffset::default());
        } else {
            self.list.scroll_to(ListOffset {
                item_ix: item_ix.min(built.items.len() - 1),
                offset_in_item: offset,
            });
            if exact && let Some(relative) = anchor.relative {
                if relative > px(0.) {
                    self.list.scroll_by(-relative);
                } else if relative < px(0.) {
                    // The message starts above the viewport: keep that row.
                    self.list.scroll_to(ListOffset {
                        item_ix: item_ix.min(built.items.len() - 1),
                        offset_in_item: anchor.offset - relative,
                    });
                }
            }
        }
        let resolved_position = nav::position_at(&built.items, &built.info, item_ix);
        let count = built.info.nav_count;
        self.loaded = Some(Loaded {
            model: built.model,
            doc: built.doc,
            items: built.items,
            info: built.info,
            cols: built.cols,
        });
        self.nav.refocus(resolved_position.filter(|p| *p < count));
        // Search texts belong to the old items.
        self.search_index = None;
        self.hovered = None;
        self.reveal = None;
        self.status = LoadStatus::Ready;
    }

    fn after_rebuild(&mut self, then: AfterRebuild, window: &mut Window, cx: &mut Context<Self>) {
        match then {
            AfterRebuild::Nothing => {
                if self.search.mode() == ViewSearchMode::Active {
                    self.refresh_search(cx);
                } else if self.search.mode() == ViewSearchMode::Typing {
                    let query = self.search.query().to_string();
                    self.ensure_search_index();
                    self.run_find(&query, false, window, cx);
                }
            }
            AfterRebuild::RevealCurrentMatch => {
                match self.search.mode() {
                    ViewSearchMode::Active => self.refresh_search(cx),
                    ViewSearchMode::Typing => {
                        let query = self.search.query().to_string();
                        self.ensure_search_index();
                        self.run_find(&query, false, window, cx);
                    }
                    ViewSearchMode::Off => {}
                }
                if let Some(current) = self.search.current().cloned()
                    && current.hidden.is_none()
                {
                    self.reveal_match(&current, true, window, cx);
                }
            }
            AfterRebuild::RevealEntry { entry, not_found } => {
                self.refresh_search(cx);
                self.reveal_entry(entry, &not_found, window, cx);
            }
        }
    }

    /// Rebuild the items from the cached entries, keeping the viewport.
    fn rebuild(&mut self, then: AfterRebuild, cx: &mut Context<Self>) {
        // An open is pending: `loaded` (if any) is the previous conversation,
        // which must never be rebuilt under the new target. Restart the open
        // with the new preferences instead.
        if matches!(
            self.pending,
            Some(PendingBuild {
                kind: PendingKind::Open { .. },
                ..
            })
        ) {
            let reveal = match self.pending.take() {
                Some(PendingBuild {
                    kind: PendingKind::Open { reveal },
                    ..
                }) => reveal,
                _ => None,
            };
            self.start_open(reveal, cx);
            return;
        }
        if self.loaded.is_none() {
            return;
        }
        let anchor = self.capture_anchor();
        let prefs = self.prefs;
        let Some(loaded) = self.loaded.as_mut() else {
            return;
        };
        loaded.model.prefs = prefs;
        let model = loaded.model.clone();
        let generation = self.submit(BuildJob::Rebuild { model }, cx);
        self.pending = Some(PendingBuild {
            generation,
            kind: PendingKind::Rebuild { anchor, then },
            submitted: Instant::now(),
        });
        cx.notify();
    }

    fn capture_anchor(&self) -> Anchor {
        let Some(loaded) = &self.loaded else {
            return Anchor {
                id: None,
                offset: px(0.),
                relative: None,
                entry: None,
            };
        };
        let top = self.list.logical_scroll_top();
        if self.nav.is_active()
            && let Some(position) = self.nav.focused()
            && let Some(first) = nav::first_item_of(&loaded.info, position)
        {
            let viewport = self.list.viewport_bounds();
            // The focused message's first row relative to the viewport top;
            // negative when it starts above it (the original keeps that,
            // F9). Not laid out: anchor on the top row instead.
            if let Some(relative) = self
                .list
                .bounds_for_item(first)
                .map(|bounds| bounds.top() - viewport.top())
            {
                return Anchor {
                    id: loaded.items.get(first).map(|item| item.id.clone()),
                    offset: px(0.),
                    relative: Some(relative),
                    entry: capture_anchor_entry(
                        &loaded.info.nav_entries,
                        Some(position),
                        true,
                        None,
                    ),
                };
            }
        }
        let first_at_or_below =
            nav::first_at_or_below_top(&loaded.info, top.item_ix, top.offset_in_item <= px(0.));
        Anchor {
            id: loaded.items.get(top.item_ix).map(|item| item.id.clone()),
            offset: top.offset_in_item,
            relative: None,
            entry: capture_anchor_entry(
                &loaded.info.nav_entries,
                self.nav.focused(),
                false,
                first_at_or_below,
            ),
        }
    }

    // ----- toggles -----------------------------------------------------------------

    fn set_prefs(&mut self, prefs: ViewPrefs, remember: bool, cx: &mut Context<Self>) {
        if prefs == self.prefs {
            return;
        }
        let started = Instant::now();
        self.prefs = prefs;
        if remember {
            update_app_state(cx, |state, _| {
                state.view_defaults = prefs;
                state.ui.view.tool_display = Some(match prefs.tool_display {
                    ToolDisplayMode::Hidden => crate::persist::RememberedToolDisplay::Summary,
                    ToolDisplayMode::Truncated => crate::persist::RememberedToolDisplay::Truncated,
                    ToolDisplayMode::Full => crate::persist::RememberedToolDisplay::Full,
                });
                state.ui.view.show_thinking = Some(prefs.show_thinking);
                state.ui.view.show_timing = Some(prefs.show_timing);
            });
            crate::persist::schedule_save(cx);
        }
        self.rebuild(AfterRebuild::Nothing, cx);
        self.perf.toggle_ui_ms = started.elapsed().as_secs_f64() * 1000.0;
    }

    /// Expand or collapse a tool output (click on a chevron or indicator):
    /// the message gets the focus (V-059).
    pub fn toggle_output(
        &mut self,
        id: &ToolOutputId,
        item_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A modal dialog (e.g. the export menu) owns the mouse (V-089).
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(loaded) = self.loaded.as_mut() else {
            return;
        };
        loaded.model.toggle_expanded(id);
        if let Some(position) = loaded.items.get(item_ix).and_then(|item| item.nav_position) {
            self.nav.focus(position);
        }
        self.rebuild(AfterRebuild::Nothing, cx);
    }

    /// `x`: expand every collapsed output of the focused (or first visible)
    /// message, else collapse them all (AD12, V-095).
    fn toggle_outputs(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let top = self.list.logical_scroll_top().item_ix;
        let position = self
            .nav
            .highlighted()
            .or_else(|| nav::position_at(&loaded.items, &loaded.info, top));
        let Some(position) = position else {
            return;
        };
        let Some((first, last)) = nav::item_range_of(&loaded.items, &loaded.info, position) else {
            return;
        };
        let mut ids: Vec<(ToolOutputId, bool)> = Vec::new();
        for item in &loaded.items[first..=last] {
            if let Some(id) = &item.toggle
                && !ids.iter().any(|(known, _)| known == id)
            {
                ids.push((id.clone(), item.expanded));
            }
        }
        if ids.is_empty() {
            return;
        }
        let expand = ids.iter().any(|(_, expanded)| !expanded);
        let Some(loaded) = self.loaded.as_mut() else {
            return;
        };
        for (id, _) in ids {
            if expand {
                loaded.model.expanded.insert(id);
            } else {
                loaded.model.expanded.remove(&id);
            }
        }
        self.rebuild(AfterRebuild::Nothing, cx);
    }

    // ----- reveal ------------------------------------------------------------------

    fn apply_reveal(&mut self, reveal: RevealRequest, window: &mut Window, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let (entry, not_found, hint) = match reveal.kind {
            RevealKind::AgentOrdinal { ordinal, hint } => (
                loaded.model.entry_for_agent_ordinal(ordinal),
                format!("Message m{ordinal} is not shown in the viewer"),
                Some(hint),
            ),
            RevealKind::EntryIndex(entry) => (
                Some(entry),
                claude_history_core::export::MESSAGE_NOT_FOUND.to_string(),
                None,
            ),
        };
        if let Some(hint) = hint {
            let mut prefs = self.prefs;
            if (hint.tools || hint.tool_results) && prefs.tool_display == ToolDisplayMode::Hidden {
                prefs.tool_display = ToolDisplayMode::Truncated;
            }
            if hint.thinking || hint.subagents {
                prefs.show_thinking = true;
            }
            if prefs != self.prefs {
                // This view only; not remembered as a default.
                self.prefs = prefs;
                self.rebuild(AfterRebuild::RevealEntry { entry, not_found }, cx);
                return;
            }
        }
        self.reveal_entry(entry, &not_found, window, cx);
    }

    fn reveal_entry(
        &mut self,
        entry: Option<usize>,
        not_found: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let position = entry.and_then(|entry| nav::position_of_entry_or_next(&loaded.info, entry));
        let Some(position) = position else {
            Toast::info(not_found.to_string()).show(window, cx);
            return;
        };
        self.nav.focus(position);
        if let Some(first) = nav::first_item_of(&loaded.info, position) {
            self.list.scroll_to(ListOffset {
                item_ix: first,
                offset_in_item: px(0.),
            });
        }
        cx.notify();
    }

    // ----- search ------------------------------------------------------------------

    fn ensure_search_index(&mut self) {
        if self.search_index.is_none()
            && let Some(loaded) = &self.loaded
        {
            self.search_index = Some(SearchIndex::build(&loaded.items));
        }
    }

    fn find_matches(&mut self, query: &str) -> Vec<SegMatch> {
        self.ensure_search_index();
        let (Some(loaded), Some(index)) = (&self.loaded, self.search_index.as_mut()) else {
            return Vec::new();
        };
        index.find(&loaded.items, &loaded.doc, query)
    }

    fn update_match_map(&mut self) {
        let current = (self.search.count() > 0).then(|| self.search.current_index());
        self.matches_by_item = Rc::new(search::by_item(self.search.matches(), current));
    }

    /// Recompute an active search after a rebuild (`ViewSearch::refresh`, V-076).
    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        if self.search.mode() != ViewSearchMode::Active {
            return;
        }
        let query = self.search.query().to_string();
        let matches = self.find_matches(&query);
        self.search.refresh(|_| matches);
        self.update_match_map();
        cx.notify();
    }

    /// A query change while typing: matches, then the first one revealed
    /// and its message focused (V-073).
    fn run_find(&mut self, query: &str, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        let started = Instant::now();
        let matches = self.find_matches(query);
        let first = self.search.set_query(query, |_| matches);
        self.update_match_map();
        self.perf.last_search_ms = started.elapsed().as_secs_f64() * 1000.0;
        if reveal && let Some(first) = first {
            self.reveal_match(&first, false, window, cx);
        }
        cx.notify();
    }

    /// Reveal a match: expand its output when hidden (rebuild, then again),
    /// focus its message, scroll it into view exactly (two frames).
    fn reveal_match(
        &mut self,
        m: &SegMatch,
        always_scroll: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = &m.hidden {
            if let Some(loaded) = self.loaded.as_mut() {
                loaded.model.expanded.insert(id.clone());
            }
            self.rebuild(AfterRebuild::RevealCurrentMatch, cx);
            return;
        }
        let Some(loaded) = &self.loaded else {
            return;
        };
        if let Some(position) = nav::position_at(&loaded.items, &loaded.info, m.item_ix) {
            self.nav.focus(position);
        }
        let visible = self.list.item_is_above_viewport(m.item_ix) == Some(false)
            && self.list.item_is_below_viewport(m.item_ix) == Some(false);
        if always_scroll || !visible {
            self.list.scroll_to(ListOffset {
                item_ix: m.item_ix,
                offset_in_item: px(0.),
            });
        }
        // A position reported for an earlier target must not move this one.
        self.reveal_y.set(None);
        self.reveal = Some(RevealTarget {
            item: m.item_ix,
            segment: m.segment_ix,
            byte: m.range.start,
        });
        cx.notify();
    }

    fn on_find_event(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::Change = event {
            if std::mem::take(&mut self.find_programmatic) {
                return;
            }
            if self.search.mode() != ViewSearchMode::Typing {
                return;
            }
            let query = state.read(cx).value().to_string();
            self.run_find(&query, true, window, cx);
        }
    }

    fn set_find_value(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.find_input.read(cx).value().to_string();
        if current != value {
            self.find_programmatic = true;
            self.find_input.update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
            // `set_value` does not emit Change in gpui-kit 0.7.3.
            self.find_programmatic = false;
        }
    }

    fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loaded.is_none() {
            return;
        }
        self.search.start();
        self.update_match_map();
        self.find_open = true;
        self.set_find_value("", window, cx);
        let focus = self.find_input.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        cx.notify();
    }

    fn commit_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.commit();
        self.find_open = false;
        self.update_match_map();
        self.focus(window, cx);
        cx.notify();
    }

    fn clear_find_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_find_value("", window, cx);
        self.search.clear_query(|_| Vec::new());
        self.update_match_map();
        cx.notify();
    }

    fn step_match(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let next = if forward {
            self.search.next_match()
        } else {
            self.search.prev_match()
        };
        self.update_match_map();
        if let Some(m) = next {
            self.reveal_match(&m, false, window, cx);
        }
        cx.notify();
    }

    /// Esc in the viewer (PD2): typing search → navigation → committed
    /// search → leave (V-077).
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.select_all {
            self.select_all = false;
            cx.notify();
            return;
        }
        match view_escape(&mut self.nav, &mut self.search) {
            ViewEscape::ClearedSearch => {
                self.find_open = false;
                self.update_match_map();
                self.focus(window, cx);
            }
            ViewEscape::ExitedNav => {}
            ViewEscape::Leave => cx.emit(ViewerPaneEvent::LeaveRequested),
        }
        cx.notify();
    }

    // ----- scrolling and navigation ------------------------------------------------

    fn line_height(window: &Window) -> Pixels {
        window.rem_size() * LINE_REM
    }

    fn first_visible_position(&self) -> Option<usize> {
        let loaded = self.loaded.as_ref()?;
        nav::position_at(
            &loaded.items,
            &loaded.info,
            self.list.logical_scroll_top().item_ix,
        )
    }

    /// After any scroll: navigation follows the viewport (V-068).
    fn after_scroll(&mut self, cx: &mut Context<Self>) {
        if self.nav.is_active() {
            let first = self.first_visible_position();
            self.nav.sync_after_scroll(first);
        }
        cx.notify();
    }

    fn scroll(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        if self.loaded.is_none() {
            return;
        }
        let a = action.as_any();
        let viewport = self.list.viewport_bounds().size.height;
        let line = Self::line_height(window);
        if a.is::<ScrollLineDown>() {
            self.list.scroll_by(line);
        } else if a.is::<ScrollLineUp>() {
            self.list.scroll_by(-line);
        } else if a.is::<ScrollHalfPageDown>() {
            self.list.scroll_by(viewport / 2.);
        } else if a.is::<ScrollHalfPageUp>() {
            self.list.scroll_by(-(viewport / 2.));
        } else if a.is::<ScrollPageDown>() {
            self.list.scroll_by(viewport);
        } else if a.is::<ScrollPageUp>() {
            self.list.scroll_by(-viewport);
        } else if a.is::<ScrollTop>() {
            self.list.scroll_to(ListOffset::default());
        } else if a.is::<ScrollBottom>() {
            self.list.scroll_to_end();
        }
        self.after_scroll(cx);
    }

    fn message_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let count = loaded.info.nav_count;
        let first_visible = self.first_visible_position();
        let position = if forward {
            self.nav.focus_next(count, first_visible)
        } else {
            self.nav.focus_prev(count, first_visible)
        };
        if let Some(position) = position
            && let Some(first) = nav::first_item_of(&loaded.info, position)
        {
            let above = self.list.item_is_above_viewport(first);
            let below = self.list.item_is_below_viewport(first);
            if above != Some(false) || below != Some(false) {
                self.list.scroll_to(ListOffset {
                    item_ix: first,
                    offset_in_item: px(0.),
                });
            }
        }
        cx.notify();
    }

    /// A mouse press inside an item focuses its message (V-059).
    pub fn on_item_mouse_down(&mut self, item_ix: usize, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        if let Some(position) = loaded.items.get(item_ix).and_then(|item| item.nav_position) {
            self.nav.focus(position);
            cx.notify();
        }
    }

    /// Hover over an expandable tool output (V-017).
    pub fn set_hovered(&mut self, id: ToolOutputId, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.hovered.as_ref() != Some(&id) {
                self.hovered = Some(id);
                cx.notify();
            }
        } else if self.hovered.as_ref() == Some(&id) {
            self.hovered = None;
            cx.notify();
        }
    }

    fn set_hovered_link(&mut self, url: Option<SharedString>, cx: &mut Context<Self>) {
        if self.hovered_link != url {
            self.hovered_link = url;
            cx.notify();
        }
    }

    // ----- copy ------------------------------------------------------------------

    /// `y` with navigation active / Copy message: the focused message's text.
    fn copy_focused_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first_visible = self.first_visible_position();
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let Some(target) = self.target.clone() else {
            return;
        };
        let position = self.nav.copy_target(first_visible);
        let text = match position.and_then(|p| loaded.info.nav_entries.get(p).copied()) {
            Some(entry) => loaded.model.message_text(entry),
            None => Err(claude_history_core::export::MESSAGE_NOT_FOUND.to_string()),
        };
        crate::ops::copy::copy_message(&target, text, window, cx);
        cx.notify();
    }

    /// Ctrl+C / Edit → Copy (§9.5): the focused input's selection when it
    /// has one, else the transcript selection, with partial-copy toasts.
    fn smart_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = window.focused_input(cx) {
            let selected = if let Some(state) = input.as_input() {
                let state = state.read(cx);
                if state.is_copyable() {
                    state.selected_text().to_string()
                } else {
                    String::new()
                }
            } else if let Some(state) = input.as_textarea() {
                state.read(cx).selected_text().to_string()
            } else {
                String::new()
            };
            if !selected.is_empty() {
                services(cx).clipboard_write(selected, cx);
                return;
            }
        }
        self.copy_transcript_selection(window, cx);
    }

    /// The items holding the ends of the current transcript selection.
    fn selection_ends(&self, cx: &App) -> Option<(Option<usize>, Option<usize>)> {
        let handles = self.handles.borrow();
        for (handle, _) in handles.handles() {
            if let Some(snapshot) = handle.snapshot(cx) {
                let item_of = |endpoint: gpui_kit::base::TextSelectionEndpoint| {
                    endpoint.entity_id().and_then(|id| handles.item_of(id))
                };
                return Some((item_of(snapshot.anchor()), item_of(snapshot.cursor())));
            }
        }
        None
    }

    /// Remember where the transcript selection is; forget it when the user
    /// cleared it (its ends are still painted, but it is gone).
    fn track_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::base::TextSelection;
        if TextSelection::has_selection(window, cx) {
            // A drag selection replaces Select All.
            self.select_all = false;
            if let Some(ends) = self.selection_ends(cx) {
                self.last_selection = Some(ends);
                let text = TextSelection::selected_text(window, cx);
                if !text.is_empty() {
                    self.last_selected_text = Some(text);
                }
            }
        } else if let Some((anchor, cursor)) = self.last_selection {
            let painted = self.painted_items();
            let shown = |end: Option<usize>| end.is_some_and(|ix| painted.contains(&ix));
            if shown(anchor) && shown(cursor) {
                self.last_selection = None;
                self.last_selected_text = None;
            }
        }
    }

    /// Ctrl+A / Edit -> Select All (V-088): select the whole transcript.
    fn select_all_transcript(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::base::TextSelection;
        if self.loaded.is_none() {
            return;
        }
        TextSelection::clear(window, cx);
        self.last_selection = None;
        self.last_selected_text = None;
        self.select_all = true;
        cx.notify();
    }

    /// The text of the whole transcript as painted: every segment of every
    /// item, in reading order, one per line (what a drag over everything
    /// would copy).
    pub(crate) fn all_text(&self) -> String {
        let Some(loaded) = self.loaded.as_ref() else {
            return String::new();
        };
        loaded
            .items
            .iter()
            .flat_map(|item| item.segments.iter())
            .map(|segment| segment.text.as_str())
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn copy_transcript_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::base::TextSelection;
        if self.select_all && self.loaded.is_some() {
            let text = self.all_text();
            if !text.is_empty() {
                services(cx).clipboard_write(text, cx);
            }
            return;
        }
        let has_selection = TextSelection::has_selection(window, cx);
        let text = TextSelection::selected_text(window, cx);
        // Which items hold the selection's two ends, and were they painted?
        let painted = self.painted_items();
        let ends = self.selection_ends(cx);
        let painted_end = |end: Option<usize>| end.is_some_and(|ix| painted.contains(&ix));
        if text.is_empty() {
            // gpui-kit drops a selection once an end left the window: say
            // so instead of copying nothing silently (§9.5).
            let scrolled_away = self
                .last_selection
                .is_some_and(|(anchor, cursor)| !painted_end(anchor) || !painted_end(cursor));
            // One end still painted: copy the part that was visible.
            if scrolled_away
                && !has_selection
                && let Some((anchor, cursor)) = self.last_selection
                && (painted_end(anchor) || painted_end(cursor))
                && let Some(visible) = self.last_selected_text.clone()
            {
                services(cx).clipboard_write(visible, cx);
                Toast::info(PARTIAL_COPY_TOAST)
                    .replacing::<PartialCopy>("copy")
                    .show(window, cx);
                return;
            }
            if self.loaded.is_some() && (has_selection || scrolled_away) {
                Toast::info(NOTHING_COPIED_TOAST)
                    .replacing::<PartialCopy>("copy")
                    .show(window, cx);
            }
            return;
        }
        services(cx).clipboard_write(text, cx);
        if let Some((anchor, cursor)) = ends
            && (!painted_end(anchor) || !painted_end(cursor))
        {
            Toast::info(PARTIAL_COPY_TOAST)
                .replacing::<PartialCopy>("copy")
                .show(window, cx);
        }
    }

    // ----- syntax highlighting ---------------------------------------------------

    /// Start highlighting the code segments the last frame asked for.
    fn start_highlights(&mut self, cx: &mut Context<Self>) {
        let wanted = std::mem::take(&mut self.tokens.borrow_mut().wanted);
        if wanted.is_empty() {
            return;
        }
        let tokens = self.tokens.clone();
        let task = cx.spawn(async move |this, cx| {
            let results = cx
                .background_spawn(async move {
                    wanted
                        .into_iter()
                        .map(|(key, text, lang)| {
                            let runs = code::highlight(&text, &lang, key.2);
                            (key, runs)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |_, cx| {
                let mut cache = tokens.borrow_mut();
                for (key, runs) in results {
                    // A theme switch or open cleared the request: drop it.
                    if cache.requested.remove(&key) {
                        cache.lru.insert(key, runs);
                    }
                }
                cache.batches_done += 1;
                cx.notify();
            })
            .ok();
        });
        self._highlight_tasks.retain(|task| !task.is_ready());
        self._highlight_tasks.push(task);
    }

    // ----- width -------------------------------------------------------------------

    /// The transcript measured its content width (mono cells): rebuild
    /// (debounced) when collapsed tool output depends on it (V-057, V-080).
    fn set_measured_cols(&mut self, cols: usize, pane_cells: usize, cx: &mut Context<Self>) {
        self.pane_cells = pane_cells;
        if cols == self.measured_cols {
            return;
        }
        self.measured_cols = cols;
        let needs_rebuild = self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.info.cols_dependent && loaded.cols != cols);
        if !needs_rebuild {
            self.cols = cols;
            return;
        }
        self._cols_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COLS_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                let cols = this.measured_cols;
                if this.cols != cols {
                    this.cols = cols;
                    this.rebuild(AfterRebuild::Nothing, cx);
                }
            })
            .ok();
        });
    }

    /// Cells left for the status hints.
    fn hint_cells(&self) -> usize {
        self.pane_cells.saturating_sub(48).max(12)
    }

    // ----- actions -----------------------------------------------------------------

    /// The viewer's own (non-session) actions. Returns `false` for others.
    fn handle_viewer_action(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let a = action.as_any();
        let mut prefs = self.prefs;
        if a.is::<CycleToolDisplay>() {
            prefs.tool_display = prefs.tool_display.next();
            self.set_prefs(prefs, true, cx);
        } else if a.is::<SetToolsSummary>() {
            prefs.tool_display = ToolDisplayMode::Hidden;
            self.set_prefs(prefs, true, cx);
        } else if a.is::<SetToolsTruncated>() {
            prefs.tool_display = ToolDisplayMode::Truncated;
            self.set_prefs(prefs, true, cx);
        } else if a.is::<SetToolsFull>() {
            prefs.tool_display = ToolDisplayMode::Full;
            self.set_prefs(prefs, true, cx);
        } else if a.is::<ToggleThinking>() {
            prefs.show_thinking = !prefs.show_thinking;
            self.set_prefs(prefs, true, cx);
        } else if a.is::<ToggleTiming>() {
            prefs.show_timing = !prefs.show_timing;
            self.set_prefs(prefs, true, cx);
        } else if a.is::<ToggleOutputs>() {
            self.toggle_outputs(cx);
        } else if a.is::<ToggleOutline>() {
            let visible = self.agent_panel.read(cx).is_visible();
            if visible {
                self.agent_panel.update(cx, |panel, cx| panel.hide(cx));
                self.focus(window, cx);
            } else {
                self.open_agent_panel(AgentTab::Outline, window, cx);
            }
        } else if a.is::<SearchWithinConversation>() {
            self.open_agent_panel(AgentTab::Within, window, cx);
        } else if a.is::<ReadRange>() {
            self.open_agent_panel(AgentTab::Read, window, cx);
        } else if a.is::<StartSearch>() {
            self.start_search(window, cx);
        } else if a.is::<NextMatch>() {
            self.step_match(true, window, cx);
        } else if a.is::<PrevMatch>() {
            self.step_match(false, window, cx);
        } else if a.is::<CommitSearch>() {
            self.commit_search(window, cx);
        } else if a.is::<CancelSearch>() {
            self.escape(window, cx);
        } else if a.is::<FindClearQuery>() {
            self.clear_find_query(window, cx);
        } else if a.is::<ScrollLineDown>()
            || a.is::<ScrollLineUp>()
            || a.is::<ScrollHalfPageDown>()
            || a.is::<ScrollHalfPageUp>()
            || a.is::<ScrollPageDown>()
            || a.is::<ScrollPageUp>()
            || a.is::<ScrollTop>()
            || a.is::<ScrollBottom>()
        {
            self.scroll(action, window, cx);
        } else if a.is::<NextMessage>() {
            self.message_step(true, cx);
        } else if a.is::<PrevMessage>() {
            self.message_step(false, cx);
        } else if a.is::<SmartCopy>() || a.is::<EditCopy>() {
            self.smart_copy(window, cx);
        } else if a.is::<SelectAllTranscript>() {
            self.select_all_transcript(window, cx);
        } else if a.is::<CopyFocusedMessage>() {
            if self.target.is_none() {
                notify::no_conversation_open(window, cx);
            } else {
                self.copy_focused_message(window, cx);
            }
        } else if a.is::<YankOrCopyMessage>() && self.nav.is_active() {
            self.copy_focused_message(window, cx);
        } else {
            return false;
        }
        self.log_action(action.name());
        cx.notify();
        true
    }

    /// Session actions on the open conversation (single-file windows have no
    /// Resume/Fork/Rename/Delete/Print and exit).
    fn run_session_action(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.mode == ViewerMode::SingleFile && routing::is_main_window_only(action) {
            return false;
        }
        let Some(target) = self.target.clone() else {
            notify::no_conversation_open(window, cx);
            return true;
        };
        let options = self
            .export_options()
            .unwrap_or_else(|| routing::default_export_options(cx));
        self.log_action(action.name());
        match routing::run_session_action(action, target, options, window, cx) {
            routing::SessionRoute::Open { agent_tab: None } => self.focus(window, cx),
            routing::SessionRoute::Open { agent_tab: Some(_) } | routing::SessionRoute::Viewer => {
                // Outline / Within / Read range / Copy message: the viewer's own.
                self.handle_viewer_action(action, window, cx);
            }
            routing::SessionRoute::Done => {}
            routing::SessionRoute::NotSession => return false,
        }
        true
    }

    fn on_viewer_action(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.target.is_none() && !action.as_any().is::<SmartCopy>() {
            return;
        }
        self.handle_viewer_action(action, window, cx);
    }

    fn on_session_action(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `y` with navigation active copies the focused message (TUI).
        if action.as_any().is::<YankOrCopyMessage>() && self.nav.is_active() {
            self.handle_viewer_action(action, window, cx);
            return;
        }
        if !self.run_session_action(action, window, cx) {
            cx.propagate();
        }
    }

    fn on_escape(&mut self, _: &ViewerEscape, window: &mut Window, cx: &mut Context<Self>) {
        self.escape(window, cx);
    }

    fn on_back(&mut self, _: &ViewerBack, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ViewerPaneEvent::LeaveRequested);
    }

    // ----- rendering ---------------------------------------------------------------

    fn open_link_dialog(url: SharedString, window: &mut Window, cx: &mut App) {
        let mono = cx.theme().mono_font_family.clone();
        let width = window.rem_size() * 30.;
        window.open_dialog(cx, move |dialog, _, _| {
            let copy_url = url.clone();
            let open_url = url.clone();
            dialog
                .title("Open link?")
                .width(width)
                .child(div().font_family(mono.clone()).text_sm().child(url.clone()))
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("link-cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("link-copy").label("Copy link").on_click(
                            move |_, window, cx| {
                                services(cx).clipboard_write(copy_url.to_string(), cx);
                                window.close_dialog(cx);
                            },
                        ))
                        .child(Button::new("link-open").label("Open").primary().on_click(
                            move |_, window, cx| {
                                services(cx).open_url(&open_url, cx);
                                window.close_dialog(cx);
                            },
                        )),
                )
        });
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let layout = self.header_layout()?;
        let roles = theme_tokens::roles(cx);
        let separator = || div().text_color(roles.dot_separator).child("·");
        let mut row = h_flex().flex_wrap().gap_x_1p5().items_center();
        for (ix, part) in layout.parts.iter().enumerate() {
            if ix > 0 {
                row = row.child(separator());
            }
            let color = match part.role {
                header::HeaderRole::Project => roles.accent,
                header::HeaderRole::CustomTitle => roles.custom_title,
                header::HeaderRole::Model => roles.model,
                header::HeaderRole::Secondary => roles.text_secondary,
                header::HeaderRole::Duration => roles.duration,
            };
            row = row.child(
                div()
                    .text_color(color)
                    .when(part.role == header::HeaderRole::Project, |d| {
                        d.font_weight(gpui_kit::FontWeight::BOLD)
                    })
                    .child(SharedString::from(part.text.clone())),
            );
        }
        if let Some(summary) = layout.summary.clone().filter(|_| layout.summary_inline) {
            row = row.child(separator()).child(
                div()
                    .text_color(roles.header_summary)
                    .child(SharedString::from(summary)),
            );
        }
        if layout.parse_errors > 0 {
            let count = layout.parse_errors;
            row = row.child(
                div()
                    .id("viewer-parse-errors")
                    .px_1()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .text_color(roles.warning)
                    .child(SharedString::from(header::parse_error_badge(count)))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(header::parse_error_tooltip(
                            count,
                        ))
                        .build(window, cx)
                    })
                    .on_click(|_, window, cx| {
                        crate::tools::open_debug_log(DebugLogTab::ParseErrors, window, cx)
                    })
                    .test_support(),
            );
        }
        let second_line = layout
            .summary
            .clone()
            .filter(|_| !layout.summary_inline)
            .map(|summary| {
                div()
                    .text_color(roles.header_summary)
                    .child(SharedString::from(summary))
            });
        Some(
            v_flex()
                .id("viewer-header")
                .qa_id("viewer.header")
                .px_3()
                .py_2()
                .gap_0p5()
                .border_b_1()
                .border_color(roles.border)
                .text_sm()
                .child(row)
                .children(second_line)
                .test_support()
                .into_any_element(),
        )
    }

    fn render_status(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let roles = theme_tokens::roles(cx);
        let mono = cx.theme().mono_font_family.clone();
        let [tools, think, info] = status::toggle_texts(&self.prefs);
        let indicator = self.indicator();
        let toggle =
            |id: &'static str, text: String, action: Box<dyn Action>, focus: FocusHandle| {
                let (key, rest) = text.split_at(1);
                div()
                    .id(id)
                    .cursor_pointer()
                    .flex()
                    .child(
                        div()
                            .text_color(roles.accent)
                            .child(SharedString::from(key.to_string())),
                    )
                    .child(
                        div()
                            .text_color(roles.muted_foreground)
                            .child(SharedString::from(rest.to_string())),
                    )
                    .on_click(move |_, window, cx| {
                        focus.focus(window, cx);
                        window.dispatch_action(action.boxed_clone(), cx);
                    })
                    .test_support()
            };
        let focused = self.viewer_focus.contains_focused(window, cx);
        let right: Option<AnyElement> = if let Some(url) = &self.hovered_link {
            Some(
                div()
                    .text_color(roles.link)
                    .child(url.clone())
                    .into_any_element(),
            )
        } else if let Some(counter) = self.search.match_counter() {
            Some(
                div()
                    .font_family(mono.clone())
                    .text_color(roles.muted_foreground)
                    .child(SharedString::from(status::search_strip(&counter, window)))
                    .into_any_element(),
            )
        } else if focused && self.target.is_some() {
            let hints = status::trim(status::hints(self.mode, window), self.hint_cells());
            Some(
                h_flex()
                    .gap_3()
                    .children(hints.into_iter().map(|hint| {
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_color(roles.foreground)
                                    .child(SharedString::from(hint.key)),
                            )
                            .child(div().text_color(roles.muted_foreground).child(hint.label))
                    }))
                    .into_any_element(),
            )
        } else {
            status::hints(self.mode, window)
                .into_iter()
                .find(|hint| hint.pinned)
                .map(|hint| {
                    h_flex()
                        .gap_1()
                        .child(
                            div()
                                .font_family(mono.clone())
                                .child(SharedString::from(hint.key)),
                        )
                        .child(div().text_color(roles.muted_foreground).child(hint.label))
                        .into_any_element()
                })
        };
        let loading = self.pending.is_some() && self.loaded.is_some();
        StatusBar::new()
            .left(
                h_flex()
                    .id("viewer-status")
                    .qa_id("viewer.status")
                    .gap_3()
                    .text_xs()
                    .child(
                        div()
                            .font_family(mono.clone())
                            .child(SharedString::from(indicator)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .font_family(mono)
                            .child(toggle(
                                "status-tools",
                                tools,
                                Box::new(CycleToolDisplay),
                                self.viewer_focus.clone(),
                            ))
                            .child(toggle(
                                "status-think",
                                think,
                                Box::new(ToggleThinking),
                                self.viewer_focus.clone(),
                            ))
                            .child(toggle(
                                "status-info",
                                info,
                                Box::new(ToggleTiming),
                                self.viewer_focus.clone(),
                            )),
                    )
                    .when(loading, |d| {
                        d.child(
                            div()
                                .text_color(roles.muted_foreground)
                                .child("Loading conversation…"),
                        )
                    })
                    .test_support(),
            )
            .when_some(right, |bar, right| bar.right(div().text_xs().child(right)))
    }

    fn render_empty(
        &self,
        icon: IconName,
        title: SharedString,
        description: Option<SharedString>,
        actions: Option<AnyElement>,
    ) -> AnyElement {
        let mut header = EmptyHeader::new()
            .media(EmptyMedia::new().child(icon))
            .title(EmptyTitle::new().child(title));
        if let Some(description) = description {
            header = header.description(EmptyDescription::new().child(description));
        }
        let mut empty = Empty::new().header(header);
        if let Some(actions) = actions {
            empty = empty.child(actions);
        }
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_3()
            .child(empty)
            .into_any_element()
    }

    /// What the transcript area shows instead of rows, if anything (§12,
    /// V-098). `render_transcript` draws exactly this state.
    pub fn transcript_state(&self) -> Option<TranscriptState> {
        if self.target.is_none() {
            return Some(TranscriptState::NothingSelected);
        }
        if let Some(error) = &self.file_error {
            return Some(TranscriptState::FileError(error.clone()));
        }
        if let LoadStatus::Failed(error) = &self.status {
            return Some(TranscriptState::OpenFailed(error.clone()));
        }
        match &self.loaded {
            None => Some(TranscriptState::Loading),
            Some(loaded) if loaded.items.is_empty() => Some(TranscriptState::NoVisibleMessages),
            Some(_) => None,
        }
    }

    fn render_transcript(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.track_selection(window, cx);
        let state = self.transcript_state();
        let title = state
            .as_ref()
            .map(|state| SharedString::from(state.title()));
        let description = state
            .as_ref()
            .and_then(TranscriptState::description)
            .map(SharedString::from);
        if let (Some(TranscriptState::NothingSelected), Some(title)) = (&state, &title) {
            return self.render_empty(IconName::FileText, title.clone(), description, None);
        }
        if let (Some(TranscriptState::FileError(_)), Some(title)) = (&state, &title) {
            return self.render_empty(IconName::TriangleAlert, title.clone(), None, None);
        }
        if let (Some(TranscriptState::OpenFailed(_)), Some(title)) = (&state, &title) {
            let weak = cx.entity().downgrade();
            let actions = h_flex()
                .gap_2()
                .child(Button::new("viewer-retry").label("Retry").small().on_click(
                    move |_, _, cx| {
                        weak.update(cx, |this, cx| this.start_open(None, cx)).ok();
                    },
                ))
                .child(
                    Button::new("viewer-debug-log")
                        .label("Show debug log")
                        .small()
                        .on_click(|_, window, cx| {
                            crate::tools::open_debug_log(DebugLogTab::Messages, window, cx)
                        }),
                )
                .into_any_element();
            return self.render_empty(IconName::TriangleAlert, title.clone(), None, Some(actions));
        }
        let Some(loaded) = &self.loaded else {
            // Measure the width while loading, so the open builds at it.
            let weak = cx.entity().downgrade();
            let mono = cx.theme().mono_font_family.clone();
            let title = title.unwrap_or_else(|| TranscriptState::Loading.title().into());
            let empty = self.render_empty(IconName::Loader, title, None, None);
            return div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(empty)
                .child(
                    canvas(
                        measure_prepaint(mono, self.prefs.show_timing, self.measured_cols, weak),
                        |_, _, _, _| (),
                    )
                    .absolute()
                    .size_full(),
                )
                .into_any_element();
        };
        if loaded.items.is_empty() {
            let focus = self.viewer_focus.clone();
            let focus2 = self.viewer_focus.clone();
            let actions = h_flex()
                .gap_2()
                .child(
                    Button::new("viewer-show-thinking")
                        .label("Show thinking")
                        .small()
                        .on_click(move |_, window, cx| {
                            focus.focus(window, cx);
                            window.dispatch_action(Box::new(ToggleThinking), cx);
                        }),
                )
                .child(
                    Button::new("viewer-show-tools")
                        .label("Show tools in full")
                        .small()
                        .on_click(move |_, window, cx| {
                            focus2.focus(window, cx);
                            window.dispatch_action(Box::new(SetToolsFull), cx);
                        }),
                )
                .into_any_element();
            let title = title.unwrap_or_else(|| TranscriptState::NoVisibleMessages.title().into());
            return self.render_empty(IconName::Inbox, title, None, Some(actions));
        }

        // Theme switches drop theme-keyed caches (V-092).
        let roles = theme_tokens::roles(cx);
        if self.theme_kind != Some(roles.kind) {
            if self.theme_kind.is_some() {
                self.tokens.borrow_mut().clear();
            }
            self.theme_kind = Some(roles.kind);
        }
        // An exact reveal reported its y last frame: put the match a few
        // lines from the top when it is outside the viewport (§9.8).
        if self.reveal.is_none() {
            self.reveal_y.set(None);
        }
        if let Some(position) = self.reveal_y.take() {
            let y = position.y;
            let viewport = self.list.viewport_bounds();
            let line = Self::line_height(window);
            if y < viewport.top() || y > viewport.bottom() - line {
                self.list.scroll_by(y - viewport.top() - line * 3.);
            }
            self.reveal = None;
        }
        // Evict selection handles far from the viewport.
        let top = self.list.logical_scroll_top().item_ix;
        self.handles.borrow_mut().evict_far(top);
        self.painted_last = std::mem::take(&mut *self.painted.borrow_mut());

        let weak = cx.entity().downgrade();
        let link_weak = weak.clone();
        let on_link_click: LinkHandler = Rc::new(move |url, window, cx| {
            if let Some(url) = url {
                Self::open_link_dialog(url, window, cx);
            }
            link_weak
                .update(cx, |this, cx| this.set_hovered_link(None, cx))
                .ok();
        });
        let hover_weak = weak.clone();
        let on_link_hover: LinkHandler = Rc::new(move |url, _, cx| {
            hover_weak
                .update(cx, |this, cx| this.set_hovered_link(url, cx))
                .ok();
        });
        let ctx = Rc::new(render::RenderCtx {
            items: loaded.items.clone(),
            roles,
            mono: cx.theme().mono_font_family.clone(),
            kind: roles.kind,
            show_timing: self.prefs.show_timing,
            focused_message: self.nav.highlighted(),
            hovered: self.hovered.clone(),
            matches: self.matches_by_item.clone(),
            handles: self.handles.clone(),
            tokens: self.tokens.clone(),
            reveal: self.reveal.map(|r| (r.item, r.segment, r.byte)),
            reveal_y: self.reveal_y.clone(),
            probe: self.probe.map(|r| (r.item, r.segment, r.byte)),
            probe_point: self.probe_point.clone(),
            painted: self.painted.clone(),
            weak: weak.clone(),
            on_link_click,
            on_link_hover,
            cell_px: {
                let font_size = window.rem_size() * 0.75;
                let font_id = window
                    .text_system()
                    .resolve_font(&font(cx.theme().mono_font_family.clone()));
                window
                    .text_system()
                    .ch_advance(font_id, font_size)
                    .unwrap_or(font_size * 0.6)
            },
            select_all: self.select_all,
        });
        let item_ctx = ctx.clone();
        let items_rendered = self.perf.items_rendered.clone();
        let items_ms = self.perf.items_ms.clone();
        items_rendered.set(0);
        items_ms.set(0.0);
        let measured = self.measured_cols;
        let mono = cx.theme().mono_font_family.clone();
        let show_timing = self.prefs.show_timing;
        let measure_weak = weak.clone();
        let after_weak = weak.clone();
        let tokens = self.tokens.clone();
        let reveal_y = self.reveal_y.clone();
        let reveal_pending = self.reveal.is_some();
        div()
            .id("viewer-list")
            .qa_id("viewer.list")
            .relative()
            .flex_1()
            .min_h_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if this.select_all {
                        this.select_all = false;
                        cx.notify();
                    }
                    if !this.viewer_focus.is_focused(window) {
                        this.viewer_focus.focus(window, cx);
                    }
                }),
            )
            .child(
                list(self.list.clone(), move |ix, window, cx| {
                    let started = Instant::now();
                    let element = render::render_item(&item_ctx, ix, window, cx);
                    items_rendered.set(items_rendered.get() + 1);
                    items_ms.set(items_ms.get() + started.elapsed().as_secs_f64() * 1000.0);
                    element
                })
                .size_full(),
            )
            .child(Scrollbar::vertical(&self.list))
            .child(
                canvas(
                    measure_prepaint(mono, show_timing, measured, measure_weak),
                    move |_, _, window, cx| {
                        // After the items rendered: start pending highlights,
                        // and re-render for an exact reveal.
                        let wanted = !tokens.borrow().wanted.is_empty();
                        if wanted {
                            let weak = after_weak.clone();
                            window.defer(cx, move |_, cx| {
                                weak.update(cx, |this, cx| this.start_highlights(cx)).ok();
                            });
                        }
                        if reveal_pending || reveal_y.get().is_some() {
                            window.refresh();
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .test_support()
            .into_any_element()
    }
}

/// The canvas prepaint that measures the transcript's content width in mono
/// cells (§9.1): the list minus the lanes, at the mono `text_xs` advance.
fn measure_prepaint(
    mono: SharedString,
    show_timing: bool,
    measured: usize,
    weak: gpui_kit::WeakEntity<ViewerPane>,
) -> impl FnOnce(gpui_kit::Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds, window, cx| {
        let rem = window.rem_size();
        let font_size = rem * 0.75;
        let font_id = window.text_system().resolve_font(&font(mono.clone()));
        let advance = window
            .text_system()
            .ch_advance(font_id, font_size)
            .unwrap_or(font_size * 0.6);
        let lanes = px(render::FOCUS_MARK_PX)
            + rem * (render::LABEL_LANE_REM + render::CONTENT_PADDING_REM)
            + if show_timing {
                rem * render::TIMESTAMP_LANE_REM
            } else {
                px(0.)
            };
        let advance = f32::from(advance).max(1.0);
        let content = (f32::from(bounds.size.width) - f32::from(lanes)).max(0.0);
        let cols = (content / advance).floor() as usize;
        let pane = (f32::from(bounds.size.width) / advance).floor() as usize;
        if cols > 0 && cols != measured {
            window.defer(cx, move |_, cx| {
                weak.update(cx, |this, cx| this.set_measured_cols(cols, pane, cx))
                    .ok();
            });
        }
    }
}

/// `Failed to open: <e>` (TUI, §12).
pub fn failed_to_open(error: &str) -> String {
    format!("Failed to open: {error}")
}

/// What the transcript area shows instead of rows (§12, V-098).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptState {
    /// No conversation open.
    NothingSelected,
    /// A single-file window's file cannot be shown (the reason).
    FileError(String),
    /// The open failed (the error), with Retry / Show debug log.
    OpenFailed(String),
    Loading,
    /// Every message is hidden by the toggles (Show thinking / Show tools).
    NoVisibleMessages,
}

impl TranscriptState {
    /// The title the empty state shows.
    pub fn title(&self) -> String {
        match self {
            TranscriptState::NothingSelected => "No conversation selected".to_string(),
            TranscriptState::FileError(error) => error.clone(),
            TranscriptState::OpenFailed(error) => failed_to_open(error),
            TranscriptState::Loading => "Loading conversation…".to_string(),
            TranscriptState::NoVisibleMessages => {
                "This conversation has no visible messages".to_string()
            }
        }
    }

    /// The muted line under the title.
    pub fn description(&self) -> Option<&'static str> {
        matches!(self, TranscriptState::NothingSelected)
            .then_some("Select a conversation on the left, or press Enter.")
    }
}

struct OpenFailed;
struct PartialCopy;

impl Render for ViewerPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_started = Instant::now();
        let frame_cycles = crate::globals::thread_cycles();
        let theme = cx.theme();
        let background = theme.background;
        let panel_visible = self.agent_panel.read(cx).is_visible();
        let header = self.render_header(cx);
        let find = (self.find_open && self.search.mode() == ViewSearchMode::Typing).then(|| {
            (
                self.find_input.clone(),
                SharedString::from(self.search.input_status()),
            )
        });
        let toolbar = toolbar::render_toolbar(
            toolbar::ToolbarModel {
                prefs: self.prefs,
                mode: self.mode,
                has_target: self.target.is_some(),
                can_modify: self.target.as_ref().is_some_and(|t| t.can_modify()),
                header_id_differs: self.target.as_ref().is_some_and(|t| t.header_id_differs()),
                find,
                outline_open: panel_visible,
            },
            &self.viewer_focus,
            window,
            cx,
        );
        let transcript = self.render_transcript(window, cx);
        let status = self.render_status(window, cx);

        let main_mode = self.mode == ViewerMode::Main;
        let column = v_flex()
            .id("viewer")
            .key_context(CTX_VIEWER)
            .track_focus(&self.viewer_focus)
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(background)
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_back));
        // Session actions live on the transcript column (not the pane root,
        // the agent panel's ancestor); main-window-only ones only in Main mode.
        let column = crate::on_actions!(
            column,
            cx,
            Self::on_session_action,
            [
                OpenConversation,
                ExportConversation,
                YankOrCopyMessage,
                CopyFocusedMessage,
                CopyPath,
                CopySessionId,
                CopyHeaderSessionId,
                ShowPath,
                RevealInFileManager,
                SearchWithinConversation,
                ToggleOutline,
                ReadRange,
                CopyAsLedger,
                CopyAsPlain,
                CopyAsMarkdown,
                CopyAsJsonl,
            ]
        );
        let column = if main_mode {
            crate::on_actions!(
                column,
                cx,
                Self::on_session_action,
                [Resume, Fork, Rename, Delete, SelectAndExit]
            )
        } else {
            column
        };
        let column = crate::on_actions!(
            column,
            cx,
            Self::on_viewer_action,
            [
                CycleToolDisplay,
                SetToolsSummary,
                SetToolsTruncated,
                SetToolsFull,
                ToggleThinking,
                ToggleTiming,
                ToggleOutputs,
                StartSearch,
                NextMatch,
                PrevMatch,
                CommitSearch,
                CancelSearch,
                FindClearQuery,
                ScrollLineDown,
                ScrollLineUp,
                NextMessage,
                PrevMessage,
                ScrollHalfPageDown,
                ScrollHalfPageUp,
                ScrollPageDown,
                ScrollPageUp,
                ScrollTop,
                ScrollBottom,
                SmartCopy,
                EditCopy,
                SelectAllTranscript,
            ]
        );
        let frame_probe = {
            let started = frame_started;
            let this = cx.entity().downgrade();
            canvas(
                |_, _, _| (),
                move |_, _, window, cx| {
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    let cpu = match (frame_cycles, crate::globals::thread_cycles()) {
                        (Some(before), Some(after)) => {
                            crate::globals::cycles_to_ms(after.saturating_sub(before))
                        }
                        _ => None,
                    };
                    let this = this.clone();
                    window.defer(cx, move |_, cx| {
                        this.update(cx, |this, _| {
                            if this.perf.frames.len() == FRAME_SAMPLES {
                                this.perf.frames.pop_front();
                            }
                            this.perf.frames.push_back(elapsed);
                            if let Some(cpu) = cpu {
                                if this.perf.frames_cpu.len() == FRAME_SAMPLES {
                                    this.perf.frames_cpu.pop_front();
                                }
                                this.perf.frames_cpu.push_back(cpu);
                            }
                        })
                        .ok();
                    });
                },
            )
            .absolute()
            .size_0()
        };
        self.perf.render_ms = frame_started.elapsed().as_secs_f64() * 1000.0;
        let column = column
            .children(header)
            .child(toolbar)
            .child(transcript)
            .child(status)
            .child(frame_probe);

        h_flex()
            .id("viewer-pane")
            .size_full()
            .items_stretch()
            .min_w_0()
            // Esc in the panel with nothing to clear or cancel (the panel
            // propagates): back to the transcript; the panel stays open.
            .on_action(cx.listener(|this, _: &AgentPanelEscape, window, cx| {
                this.focus(window, cx);
            }))
            // `o` in the agent panel (`AgentPanel && !Input`): the panel's
            // Outline tab toggles closed (focus back to the transcript),
            // any other tab switches to Outline (V-097, §8.3).
            .on_action(cx.listener(|this, _: &ToggleOutline, window, cx| {
                let panel = this.agent_panel.read(cx);
                if panel.is_visible() && panel.tab() == AgentTab::Outline {
                    this.agent_panel.update(cx, |panel, cx| panel.hide(cx));
                    this.focus(window, cx);
                } else {
                    this.open_agent_panel(AgentTab::Outline, window, cx);
                }
                this.log_action("history::ToggleOutline");
                cx.notify();
            }))
            .child(column)
            .when(panel_visible, |this| this.child(self.agent_panel.clone()))
    }
}
