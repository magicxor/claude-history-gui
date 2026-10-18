//! The main window root (PD1, PD2, DESIGN §5.1, §7.2, §7.7). WP0 → WP-I.
//!
//! Layout: app-drawn title bar (+ `AppMenuBar` on Windows/Linux), then the
//! left pane (tabs Conversations / Deep search, each with its own clamped
//! width) and the viewer, separated by a draggable divider. Owns the event
//! wiring between the panes (§7.2), the focus rules (§7.7) and the fallback
//! routing of actions dispatched with no pane on the path (`routing`).
//!
//! A pending debounced preview is flushed in the **capture** phase of every
//! action that acts on the list's selection (session actions,
//! `ShowSemanticDetails`, `RowMenu`) while the list has focus, before
//! `ListPane` handles it — so the viewer always shows what the action targets
//! (PD2, §7.8, L-085). The fallback path flushes too (`ActivePane` = list).
//!
//! Phase C (WP-I): the left pane makes room for the agent panel — its width
//! shrinks down to the tab's minimum so the viewer keeps 32rem, and when even
//! that does not fit it is hidden ("auto" focus-viewer mode, not remembered)
//! until the panel closes (PD1, X-040). Show projects directory has a
//! listener (is enabled) only with a workspace (T-016). The root counts its
//! frames and their UI time for the idle probe (X-007).

pub mod routing;
pub mod startup;
pub mod title_bar;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyWindowHandle, App, AppContext as _, Bounds, Context, CursorStyle, DispatchPhase,
    DragMoveEvent, Entity, InteractiveElement as _, IntoElement, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, Render, ScrollWheelEvent, Size, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, WindowBounds, WindowOptions,
    canvas, div, px, rems, size,
};

use crate::actions::*;
use crate::deep_search::panel::{AgentPanelEvent, panel_width_rem};
use crate::deep_search::{DeepSearchEvent, DeepSearchPane};
use crate::globals::{
    ActivePane, CorpusBus, CorpusNotice, MainPanes, app_state, same_path, update_app_state,
};
use crate::list::{ListPane, ListPaneEvent};
use crate::model::{ConversationTarget, LeftTab, OpenRequest, PaneFocus, ViewerMode};
use crate::viewer::{ViewerPane, ViewerPaneEvent};
use crate::{exit, keymap, menus, persist, theme_tokens};

use routing::{Forwarded, RouteTargets};
use title_bar::{AppTitleBar, window_title};

/// Conversations tab width range and default (rem, PD1).
pub const LIST_WIDTH_MIN_REM: f32 = 18.0;
pub const LIST_WIDTH_MAX_REM: f32 = 40.0;
pub const LIST_WIDTH_DEFAULT_REM: f32 = 30.0;
/// Deep search tab width range and default (rem, PD1).
pub const DEEP_WIDTH_MIN_REM: f32 = 24.0;
pub const DEEP_WIDTH_MAX_REM: f32 = 48.0;
pub const DEEP_WIDTH_DEFAULT_REM: f32 = 34.0;
/// The viewer never gets narrower than this (rem).
pub const VIEWER_MIN_REM: f32 = 32.0;
/// Width of the divider between the left pane and the viewer (rem).
pub const DIVIDER_REM: f32 = 0.25;
/// Keyboard selection previews are debounced (PD2).
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);
/// Default and minimum main window size (logical px; window bounds are a
/// documented `px` exception).
pub const MAIN_WINDOW_SIZE: (f32, f32) = (1280.0, 820.0);
pub const MAIN_WINDOW_MIN_SIZE: (f32, f32) = (880.0, 560.0);

/// The width range of a left tab (rem).
pub fn width_range(tab: LeftTab) -> (f32, f32) {
    match tab {
        LeftTab::Conversations => (LIST_WIDTH_MIN_REM, LIST_WIDTH_MAX_REM),
        LeftTab::DeepSearch => (DEEP_WIDTH_MIN_REM, DEEP_WIDTH_MAX_REM),
    }
}

/// Whether the left pane fits next to a viewer of [`VIEWER_MIN_REM`] and an
/// open agent panel `panel_rem` wide in a window `window_rem` wide, at its
/// tab's minimum width (PD1: when not, opening the panel hides the left pane).
pub fn left_fits_with_panel(tab: LeftTab, window_rem: f32, panel_rem: f32) -> bool {
    window_rem - width_range(tab).0 - DIVIDER_REM - panel_rem >= VIEWER_MIN_REM
}

/// Clamp a left-pane width (rem) to its tab's range and, when the window
/// width is known, so that the viewer keeps at least [`VIEWER_MIN_REM`] (PD1).
pub fn clamp_left_width(requested_rem: f32, tab: LeftTab, window_width_rem: Option<f32>) -> f32 {
    let (min, max) = width_range(tab);
    let requested = if requested_rem.is_finite() {
        requested_rem
    } else {
        min
    };
    let width = requested.clamp(min, max);
    match window_width_rem {
        // The tab's minimum wins over the viewer's: a visible pane is never
        // narrower than its minimum (X-040).
        Some(window) => width.min((window - VIEWER_MIN_REM - DIVIDER_REM).max(min)),
        None => width,
    }
}

/// Frames a window rendered and the UI time they took (render start → the
/// end of paint; the idle probe, X-007).
#[derive(Debug, Default)]
pub struct FrameStats {
    frames: Cell<u64>,
    total: Cell<Duration>,
    /// The UI thread's own CPU cycles in those frames (Windows).
    cycles: Cell<u64>,
    last: Cell<Duration>,
    /// When the last frame ended.
    last_at: Cell<Option<Instant>>,
    /// Mouse events the window received (moves, presses, wheel): input from
    /// outside, which repaints hovered elements (the idle probe tells it apart).
    input: Cell<u64>,
    /// The wall-clock time of the last [`FRAME_SAMPLES_KEPT`] frames (X-008).
    samples: RefCell<VecDeque<Duration>>,
}

/// Frames kept for the window's p95 / max probes.
const FRAME_SAMPLES_KEPT: usize = 512;

impl FrameStats {
    /// The nearest-rank 95th percentile of the kept frame times in
    /// milliseconds (0 without frames).
    pub fn p95_ms(&self) -> f64 {
        let mut times: Vec<Duration> = self.samples.borrow().iter().copied().collect();
        if times.is_empty() {
            return 0.0;
        }
        times.sort_unstable();
        let rank = (times.len() * 95).div_ceil(100).max(1);
        times[rank - 1].as_secs_f64() * 1000.0
    }

    /// The slowest kept frame in milliseconds.
    pub fn max_ms(&self) -> f64 {
        self.samples
            .borrow()
            .iter()
            .max()
            .map_or(0.0, |worst| worst.as_secs_f64() * 1000.0)
    }

    pub fn frames(&self) -> u64 {
        self.frames.get()
    }

    /// Wall-clock time of every frame.
    pub fn total(&self) -> Duration {
        self.total.get()
    }

    /// UI-thread CPU cycles of every frame (0 where not measured).
    pub fn cycles(&self) -> u64 {
        self.cycles.get()
    }

    /// The last frame's wall-clock time.
    pub fn last(&self) -> Duration {
        self.last.get()
    }

    fn add(&self, measured: crate::globals::BusyMeasure) {
        self.frames.set(self.frames.get() + 1);
        self.total.set(self.total.get() + measured.wall);
        self.cycles
            .set(self.cycles.get() + measured.cycles.unwrap_or_default());
        self.last.set(measured.wall);
        self.last_at.set(Some(Instant::now()));
        let mut samples = self.samples.borrow_mut();
        samples.push_back(measured.wall);
        while samples.len() > FRAME_SAMPLES_KEPT {
            samples.pop_front();
        }
    }

    /// Mouse events the window received.
    pub fn input_events(&self) -> u64 {
        self.input.get()
    }

    fn count_input(&self, phase: DispatchPhase) {
        if phase == DispatchPhase::Capture {
            self.input.set(self.input.get() + 1);
        }
    }

    /// Count the next frame's mouse events (registered while painting).
    fn listen_for_input(self: &Rc<Self>, window: &mut Window) {
        let stats = self.clone();
        window.on_mouse_event(move |_: &MouseMoveEvent, phase, _, _| stats.count_input(phase));
        let stats = self.clone();
        window.on_mouse_event(move |_: &MouseDownEvent, phase, _, _| stats.count_input(phase));
        let stats = self.clone();
        window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, _| stats.count_input(phase));
    }

    /// Time since the last frame ended.
    pub fn since_last(&self) -> Option<Duration> {
        self.last_at.get().map(|at| at.elapsed())
    }
}

/// Drag payload of the main split divider.
#[derive(Clone, Copy, Debug)]
struct SplitDrag;

impl Render for SplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The main window's root view.
pub struct Workspace {
    window: AnyWindowHandle,
    menu_bar: Option<Entity<AppMenuBar>>,
    list: Entity<ListPane>,
    deep: Entity<DeepSearchPane>,
    viewer: Entity<ViewerPane>,
    left_tab: LeftTab,
    left_collapsed: bool,
    /// The left pane is hidden because the open agent panel needed the room
    /// (PD1, X-040): not remembered, shown again when the panel closes.
    auto_collapsed: bool,
    /// The agent panel was open at the last check, and its width (rem).
    panel_open: bool,
    panel_rem: f32,
    /// The window width (rem) at the last check (resizes while the panel is open).
    last_window_rem: f32,
    list_width_rem: f32,
    deep_width_rem: f32,
    title: String,
    pending_preview: Option<ConversationTarget>,
    _preview_task: Task<()>,
    /// PD3 rule 3 (c) ran (once, after the first completed load).
    recorded_cwd_checked: bool,
    frame_stats: Rc<FrameStats>,
    /// Activation changes of this window (the OS's doing; the idle probe).
    activations: u64,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let menu_bar = (!cfg!(target_os = "macos")).then(|| {
            let bar = AppMenuBar::new(cx);
            menus::register_menu_bar(&bar, cx);
            bar
        });
        let list = cx.new(|cx| ListPane::new(window, cx));
        let deep = cx.new(|cx| DeepSearchPane::new(window, cx));
        let viewer = cx.new(|cx| ViewerPane::new(ViewerMode::Main, window, cx));
        let handle = window.window_handle();
        cx.set_global(MainPanes {
            list: Some(list.downgrade()),
            viewer: Some(viewer.downgrade()),
            deep: Some(deep.downgrade()),
            window: Some(handle),
        });

        let (left_tab, left_collapsed, list_width_rem, deep_width_rem) = {
            let ui = &app_state(cx).ui;
            let restore = app_state(cx).config.gui.restore_session();
            (
                if restore {
                    ui.left_tab
                } else {
                    LeftTab::Conversations
                },
                restore && ui.list_collapsed,
                ui.split.list_width_rem,
                ui.split.deep_width_rem,
            )
        };

        let bus = CorpusBus::entity(cx);
        let agent_panel = viewer.read(cx).agent_panel().clone();
        // The remembered visibility and tab of the agent panel (C-038).
        let remembered = app_state(cx).ui.agent_panel;
        agent_panel.update(cx, |panel, cx| panel.restore(remembered, cx));
        let subscriptions = vec![
            cx.subscribe_in(&list, window, Self::on_list_event),
            cx.subscribe_in(&viewer, window, Self::on_viewer_event),
            cx.subscribe_in(&deep, window, Self::on_deep_event),
            cx.subscribe_in(&agent_panel, window, Self::on_agent_panel_event),
            // The panel opening or closing, and window resizes: make room
            // for it (PD1, X-040).
            cx.observe_in(&agent_panel, window, |this, _, window, cx| {
                this.fit_agent_panel(window, cx)
            }),
            cx.observe_window_bounds(window, |this, window, cx| this.fit_agent_panel(window, cx)),
            cx.subscribe_in(&bus, window, Self::on_corpus_notice),
            cx.observe_global_in::<crate::globals::AppState>(window, |this, window, cx| {
                this.refresh_title(window, cx);
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                theme_tokens::follow_system(window, cx);
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.activations += 1;
                if window.is_window_active() {
                    menus::rebuild(cx);
                }
            }),
            cx.on_focus_lost(window, Self::on_focus_lost),
        ];

        let title = window_title(cx);
        window.set_window_title(&title);
        let this = Self {
            window: handle,
            menu_bar,
            list,
            deep,
            viewer,
            left_tab,
            left_collapsed,
            auto_collapsed: false,
            panel_open: false,
            panel_rem: 0.0,
            last_window_rem: window_width_rem(window),
            list_width_rem: clamp_left_width(list_width_rem, LeftTab::Conversations, None),
            deep_width_rem: clamp_left_width(deep_width_rem, LeftTab::DeepSearch, None),
            title,
            pending_preview: None,
            _preview_task: Task::ready(()),
            recorded_cwd_checked: false,
            frame_stats: Rc::new(FrameStats::default()),
            activations: 0,
            _subscriptions: subscriptions,
        };
        let mut this = this;
        this.fit_agent_panel(window, cx);
        // Initial focus: the visible tab's primary element, else the viewer (§7.7).
        if this.left_collapsed {
            this.viewer
                .update(cx, |viewer, cx| viewer.focus(window, cx));
        } else {
            this.focus_left_primary(window, cx);
        }
        this
    }

    pub fn window_handle(&self) -> AnyWindowHandle {
        self.window
    }

    /// The window title (`Claude History — <workspace | All projects>`).
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn list(&self) -> &Entity<ListPane> {
        &self.list
    }

    pub fn viewer(&self) -> &Entity<ViewerPane> {
        &self.viewer
    }

    pub fn deep(&self) -> &Entity<DeepSearchPane> {
        &self.deep
    }

    pub fn left_tab(&self) -> LeftTab {
        self.left_tab
    }

    /// Focus viewer mode (left pane hidden).
    pub fn left_collapsed(&self) -> bool {
        self.left_collapsed
    }

    /// The left pane is hidden only because the agent panel needed the room
    /// (PD1, X-040).
    pub fn auto_collapsed(&self) -> bool {
        self.auto_collapsed
    }

    /// Frames this window rendered and their UI time (X-007).
    pub fn frame_stats(&self) -> &FrameStats {
        &self.frame_stats
    }

    /// The stored width of the active left tab (rem), clamped to its range.
    pub fn left_width_rem(&self) -> f32 {
        match self.left_tab {
            LeftTab::Conversations => self.list_width_rem,
            LeftTab::DeepSearch => self.deep_width_rem,
        }
    }

    /// The width the active left tab gets in the current window (rem): the
    /// viewer keeps 32rem next to the open agent panel (PD1).
    pub fn effective_left_width_rem(&self, window: &Window) -> f32 {
        let panel = if self.panel_open { self.panel_rem } else { 0.0 };
        clamp_left_width(
            self.left_width_rem(),
            self.left_tab,
            Some(window_width_rem(window) - panel),
        )
    }

    /// Make room for the agent panel (PD1, X-040), on every change of the
    /// panel and every window resize: when it opens (or the window shrinks
    /// while it is open) and the left pane does not fit at its minimum width
    /// next to a 32rem viewer, the left pane is hidden ("auto"); when the
    /// panel closes, an auto-hidden left pane comes back at its remembered
    /// width (clamped to the window).
    fn fit_agent_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.viewer.read(cx).agent_panel().clone();
        let open = panel.read(cx).is_visible();
        let width = panel_width_rem(cx);
        let opened = open && !self.panel_open;
        let closed = !open && self.panel_open;
        let width_changed = open && (self.panel_rem - width).abs() > 0.01;
        let tab = panel.read(cx).tab();
        let remembered = app_state(cx).ui.agent_panel;
        if remembered.visible != open || remembered.tab != tab {
            crate::persist::remember_agent_panel(open, tab, None, cx);
        }
        let resized = open && (window_width_rem(window) - self.last_window_rem).abs() > 0.01;
        self.panel_open = open;
        self.panel_rem = width;
        self.last_window_rem = window_width_rem(window);
        if (opened || resized || width_changed)
            && !self.left_collapsed
            && !left_fits_with_panel(self.left_tab, window_width_rem(window), width)
        {
            if self.left_contains_focus(window, cx) {
                self.viewer
                    .update(cx, |viewer, cx| viewer.focus(window, cx));
            }
            self.left_collapsed = true;
            self.set_auto_collapsed(true, cx);
            cx.notify();
        } else if closed && self.auto_collapsed {
            self.set_auto_collapsed(false, cx);
            self.left_collapsed = false;
            cx.notify();
        } else if opened || closed || width_changed {
            // The left pane's width follows the panel.
            cx.notify();
        }
    }

    /// Set the active tab's width (rem): clamped to its range and so that
    /// the viewer keeps ≥ 32rem in the current window (PD1, L-088).
    pub fn set_left_width_rem(&mut self, rem: f32, window: &mut Window, cx: &mut Context<Self>) {
        let width = clamp_left_width(rem, self.left_tab, Some(window_width_rem(window)));
        match self.left_tab {
            LeftTab::Conversations => self.list_width_rem = width,
            LeftTab::DeepSearch => self.deep_width_rem = width,
        }
        let (list, deep) = (self.list_width_rem, self.deep_width_rem);
        update_app_state(cx, |state, _| {
            state.ui.split.list_width_rem = list;
            state.ui.split.deep_width_rem = deep;
        });
        persist::schedule_save(cx);
        cx.notify();
    }

    /// Run a pending debounced preview now (list-target actions call this
    /// first so the viewer shows what they act on, PD2). The root's capture
    /// hook calls it for every list-target action while the list has focus.
    pub fn flush_pending_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._preview_task = Task::ready(());
        if let Some(target) = self.pending_preview.take() {
            self.open_in_viewer(OpenRequest::new(target, false), window, cx);
        }
    }

    /// Whether a debounced preview is waiting.
    pub fn has_pending_preview(&self) -> bool {
        self.pending_preview.is_some()
    }

    fn open_in_viewer(&mut self, req: OpenRequest, window: &mut Window, cx: &mut Context<Self>) {
        let same = self
            .viewer
            .read(cx)
            .target()
            .is_some_and(|current| same_path(&current.path, &req.target.path));
        let header = (!same).then(|| self.list.read(cx).header_info(&req.target.path));
        self.viewer.update(cx, |viewer, cx| {
            viewer.open(req, window, cx);
            if let Some(header) = header {
                viewer.set_header(header, cx);
            }
        });
    }

    /// Open `req` in the viewer (§7.2): a pending preview is dropped; when
    /// the viewer already shows the conversation it only reveals / focuses /
    /// opens the agent tab, else (with `select_in_list`) the row is selected
    /// when listed and the conversation opens — a reveal never applies to
    /// another conversation.
    pub fn open_request(
        &mut self,
        req: OpenRequest,
        select_in_list: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_preview = None;
        self._preview_task = Task::ready(());
        let shown = self
            .viewer
            .read(cx)
            .target()
            .is_some_and(|current| same_path(&current.path, &req.target.path));
        if !shown && select_in_list {
            let path = req.target.path.clone();
            self.list.update(cx, |list, cx| list.select_path(&path, cx));
        }
        self.open_in_viewer(req, window, cx);
    }

    // ----- events ----------------------------------------------------------------

    fn on_list_event(
        &mut self,
        list: &Entity<ListPane>,
        event: &ListPaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let timer = crate::globals::BusyTimer::start();
        self.handle_list_event(list, event, window, cx);
        crate::globals::record_sub_span(cx, "ws.list_event", timer.stop());
    }

    fn handle_list_event(
        &mut self,
        _: &Entity<ListPane>,
        event: &ListPaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ListPaneEvent::SelectionChanged { target, .. } => {
                let Some(target) = target.clone() else {
                    return;
                };
                if !self.list.read(cx).contains_focus(window, cx) {
                    return;
                }
                let shown = self
                    .viewer
                    .read(cx)
                    .target()
                    .is_some_and(|current| current.path == target.path);
                if shown {
                    self.pending_preview = None;
                    return;
                }
                self.pending_preview = Some(target);
                self._preview_task = cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
                    this.update_in(cx, |this, window, cx| {
                        this.flush_pending_preview(window, cx)
                    })
                    .ok();
                });
            }
            ListPaneEvent::OpenRequested {
                target,
                focus_viewer,
                agent_tab,
            } => {
                let req =
                    OpenRequest::new(target.clone(), *focus_viewer).with_agent_tab(*agent_tab);
                self.open_request(req, false, window, cx);
            }
            ListPaneEvent::CorpusChanged => {
                let path = self.viewer.read(cx).target().map(|t| t.path.clone());
                if let Some(path) = path {
                    let header = self.list.read(cx).header_info(&path);
                    self.viewer
                        .update(cx, |viewer, cx| viewer.set_header(header, cx));
                }
                self.apply_recorded_cwd_once(cx);
                // Only when a check mark changed (X-038: an edit of a 20k
                // corpus must not rebuild and reload every menu).
                menus::refresh_if_changed(cx);
            }
            ListPaneEvent::LoadStateChanged => menus::rebuild(cx),
        }
    }

    /// PD3 rule 3 (c), L-082: once the first load is complete, a launch
    /// directory recorded as the cwd of a loaded Pi/OMP conversation becomes
    /// the workspace when rules 1–5 chose none; the list then re-scopes and
    /// reloads (integrator, Phase A, WP7 request).
    fn apply_recorded_cwd_once(&mut self, cx: &mut Context<Self>) {
        if self.recorded_cwd_checked || self.list.read(cx).is_loading() {
            return;
        }
        self.recorded_cwd_checked = true;
        let state = app_state(cx);
        if state.workspace.is_some() || state.launch.launch_cwd.is_none() {
            return;
        }
        let cwds = self.list.read(cx).recorded_agent_cwds();
        if startup::apply_recorded_cwd_workspace(cx, cwds.iter().map(std::path::PathBuf::as_path)) {
            let workspace = app_state(cx).workspace.clone();
            self.list
                .update(cx, |list, cx| list.set_workspace(workspace, cx));
        }
    }

    fn on_viewer_event(
        &mut self,
        _: &Entity<ViewerPane>,
        event: &ViewerPaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ViewerPaneEvent::LeaveRequested => {
                // Back to the list (L-087): re-shown if collapsed, the
                // Conversations tab, its search input focused.
                self.show_left_tab(LeftTab::Conversations, window, cx);
            }
            ViewerPaneEvent::TargetChanged(target) => {
                let panel = self.viewer.read(cx).agent_panel().clone();
                panel.update(cx, |panel, cx| panel.set_viewer_target(target.clone(), cx));
            }
        }
    }

    fn on_deep_event(
        &mut self,
        _: &Entity<DeepSearchPane>,
        event: &DeepSearchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let DeepSearchEvent::OpenHit {
            target,
            reveal,
            focus_viewer,
            agent_tab,
        } = event;
        let req = OpenRequest::new(target.clone(), *focus_viewer)
            .with_reveal(reveal.clone())
            .with_agent_tab(*agent_tab);
        self.open_request(req, true, window, cx);
    }

    fn on_agent_panel_event(
        &mut self,
        _: &Entity<crate::deep_search::panel::AgentPanel>,
        event: &AgentPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let AgentPanelEvent::Reveal { target, reveal } = event;
        let req = OpenRequest::new(target.clone(), true).with_reveal(reveal.clone());
        self.open_request(req, true, window, cx);
    }

    fn on_corpus_notice(
        &mut self,
        _: &Entity<CorpusBus>,
        notice: &CorpusNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let timer = crate::globals::BusyTimer::start();
        self.handle_corpus_notice(notice, window, cx);
        crate::globals::record_sub_span(cx, "ws.corpus_notice", timer.stop());
    }

    fn handle_corpus_notice(
        &mut self,
        notice: &CorpusNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CorpusNotice::Deleted(paths) = notice;
        let list = self.list.clone();
        crate::globals::sub_span(cx, "list.remove_paths", |cx| {
            list.update(cx, |list, cx| list.remove_paths(paths, cx))
        });
        if self
            .pending_preview
            .as_ref()
            .is_some_and(|pending| paths.iter().any(|path| same_path(path, &pending.path)))
        {
            self.pending_preview = None;
        }
        let viewer_shows_deleted = self
            .viewer
            .read(cx)
            .target()
            .is_some_and(|target| paths.iter().any(|path| same_path(path, &target.path)));
        if viewer_shows_deleted {
            let viewer_had_focus = self.viewer.read(cx).contains_focus(window, cx);
            let viewer = self.viewer.clone();
            crate::globals::sub_span(cx, "viewer.clear", |cx| {
                viewer.update(cx, |viewer, cx| viewer.clear(cx))
            });
            if viewer_had_focus {
                self.set_left_collapsed(false, cx);
                self.focus_left_primary(window, cx);
            }
        }
        let panel = self.viewer.read(cx).agent_panel().clone();
        crate::globals::sub_span(cx, "panel.mark_deleted", |cx| {
            panel.update(cx, |panel, cx| panel.mark_deleted(paths, cx))
        });
        // Dialogs registered in `OpenTargets` are closed by the app's own
        // `CorpusBus` handler (globals.rs), with or without this window.
    }

    // ----- focus -----------------------------------------------------------------

    fn focus_left_primary(&self, window: &mut Window, cx: &mut App) {
        match self.left_tab {
            LeftTab::Conversations => self
                .list
                .update(cx, |list, cx| list.focus_search(window, cx)),
            LeftTab::DeepSearch => self
                .deep
                .update(cx, |deep, cx| deep.focus_query(window, cx)),
        }
    }

    fn left_contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.list.read(cx).contains_focus(window, cx)
            || self.deep.read(cx).contains_focus(window, cx)
    }

    /// Focus lost (the focused element disappeared): restore the platform's
    /// target if it is still rendered, else the visible left tab's primary
    /// element, else the viewer (§7.7, X-043).
    fn on_focus_lost(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        if let Some(target) = window.focus_lost_restore_target(cx) {
            target.focus(window, cx);
            return;
        }
        if !self.left_collapsed {
            self.focus_left_primary(window, cx);
        } else {
            self.viewer
                .update(cx, |viewer, cx| viewer.focus(window, cx));
        }
    }

    /// Mark the left pane as hidden automatically (or not) and mirror it for
    /// the View > Focus viewer check, which reads checked while it is hidden.
    fn set_auto_collapsed(&mut self, auto: bool, cx: &mut Context<Self>) {
        if self.auto_collapsed == auto {
            return;
        }
        self.auto_collapsed = auto;
        update_app_state(cx, |state, _| state.list_auto_collapsed = auto);
        menus::rebuild(cx);
    }

    /// An explicit show/hide (focus-viewer mode, a tab, leaving the viewer):
    /// remembered, and it ends an automatic collapse (X-040).
    fn set_left_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.set_auto_collapsed(false, cx);
        if self.left_collapsed == collapsed {
            return;
        }
        self.left_collapsed = collapsed;
        update_app_state(cx, |state, _| state.ui.list_collapsed = collapsed);
        persist::schedule_save(cx);
        cx.notify();
    }

    fn show_left_tab(&mut self, tab: LeftTab, window: &mut Window, cx: &mut Context<Self>) {
        self.set_left_collapsed(false, cx);
        if self.left_tab != tab {
            self.left_tab = tab;
            update_app_state(cx, |state, _| state.ui.left_tab = tab);
            persist::schedule_save(cx);
        }
        self.focus_left_primary(window, cx);
        cx.notify();
    }

    fn on_focus_conversations(
        &mut self,
        _: &FocusConversations,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_left_tab(LeftTab::Conversations, window, cx);
    }

    fn on_focus_deep_search(
        &mut self,
        _: &FocusDeepSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_left_tab(LeftTab::DeepSearch, window, cx);
    }

    fn on_focus_viewer(&mut self, _: &FocusViewer, window: &mut Window, cx: &mut Context<Self>) {
        self.viewer
            .update(cx, |viewer, cx| viewer.focus(window, cx));
    }

    fn on_toggle_list_pane(
        &mut self,
        _: &ToggleListPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.left_collapsed {
            self.set_left_collapsed(false, cx);
            self.focus_left_primary(window, cx);
        } else {
            // Move focus off the pane before hiding it (§7.7).
            if self.left_contains_focus(window, cx) {
                self.viewer
                    .update(cx, |viewer, cx| viewer.focus(window, cx));
            }
            self.set_left_collapsed(true, cx);
        }
        menus::rebuild(cx);
    }

    /// The F6 regions in cycle order (AD16): list search → list toolbar →
    /// viewer → agent panel (when open); with the Deep search tab active,
    /// viewer → agent panel → deep search. Collapsed left panes are skipped.
    pub fn regions(&self, cx: &App) -> Vec<PaneFocus> {
        let panel_open = self.viewer.read(cx).agent_panel().read(cx).is_visible();
        let mut regions = Vec::with_capacity(4);
        if !self.left_collapsed && self.left_tab == LeftTab::Conversations {
            regions.push(PaneFocus::List);
            regions.push(PaneFocus::ListToolbar);
        }
        regions.push(PaneFocus::Viewer);
        if panel_open {
            regions.push(PaneFocus::AgentPanel);
        }
        if !self.left_collapsed && self.left_tab == LeftTab::DeepSearch {
            regions.push(PaneFocus::DeepSearch);
        }
        regions
    }

    /// The region that holds the focus now.
    pub fn focused_region(&self, window: &Window, cx: &App) -> PaneFocus {
        let list = self.list.read(cx);
        if list.toolbar_contains_focus(window, cx) {
            PaneFocus::ListToolbar
        } else if list.contains_focus(window, cx) {
            PaneFocus::List
        } else if self.deep.read(cx).contains_focus(window, cx) {
            PaneFocus::DeepSearch
        } else if self
            .viewer
            .read(cx)
            .agent_panel()
            .read(cx)
            .contains_focus(window, cx)
        {
            PaneFocus::AgentPanel
        } else {
            PaneFocus::Viewer
        }
    }

    /// F6 / Shift+F6 (AD16): move to the next / previous region.
    fn cycle_region(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let regions = self.regions(cx);
        let current = self.focused_region(window, cx);
        let next = match regions.iter().position(|r| *r == current) {
            Some(ix) if forward => regions[(ix + 1) % regions.len()],
            Some(ix) => regions[(ix + regions.len() - 1) % regions.len()],
            None => regions[0],
        };
        match next {
            PaneFocus::List => self
                .list
                .update(cx, |list, cx| list.focus_search(window, cx)),
            PaneFocus::ListToolbar => self
                .list
                .update(cx, |list, cx| list.focus_toolbar(window, cx)),
            PaneFocus::DeepSearch => self
                .deep
                .update(cx, |deep, cx| deep.focus_query(window, cx)),
            PaneFocus::AgentPanel => {
                let panel = self.viewer.read(cx).agent_panel().clone();
                let focus = gpui_kit::Focusable::focus_handle(panel.read(cx), cx);
                focus.focus(window, cx);
            }
            PaneFocus::Viewer | PaneFocus::Other => self
                .viewer
                .update(cx, |viewer, cx| viewer.focus(window, cx)),
        }
    }

    // ----- routing ---------------------------------------------------------------

    fn route_targets(&self, cx: &App) -> RouteTargets {
        RouteTargets {
            list: Some(self.list.clone()),
            viewer: Some(self.viewer.clone()),
            deep: Some(self.deep.clone()),
            active: ActivePane::of_window(cx, self.window),
        }
    }

    /// Capture phase of every list-target action (`capture_session_actions!`):
    /// with the list focused, the pending preview is shown first, then the
    /// action continues to the list (PD2, L-085).
    fn capture_list_target_action(
        &mut self,
        _: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_preview.is_some() && self.list.read(cx).contains_focus(window, cx) {
            self.flush_pending_preview(window, cx);
        }
    }

    /// Fallback for actions no pane on the path handled (§8.1).
    fn route_fallback(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        let a = action.as_any();
        if a.is::<ToggleScope>() && !self.list.read(cx).has_workspace() {
            // Without a workspace Tab moves focus on (Root focus cycling).
            cx.propagate();
            return;
        }
        let targets = self.route_targets(cx);
        if routing::is_session_action(action)
            && matches!(targets.active, PaneFocus::List | PaneFocus::ListToolbar)
        {
            self.flush_pending_preview(window, cx);
        }
        match routing::forward(action, &targets, window, cx) {
            Forwarded::Unhandled => cx.propagate(),
            Forwarded::Handled => {}
            Forwarded::Open(req) => self.open_request(*req, true, window, cx),
        }
    }

    fn app_action(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        let active = ActivePane::of_window(cx, self.window);
        if !routing::run_app_action(action, Some(&self.viewer), active, window, cx) {
            cx.propagate();
        }
    }

    fn on_choose_workspace(
        &mut self,
        _: &ChooseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list
            .update(cx, |list, cx| list.choose_workspace_folder(window, cx));
    }

    fn on_clear_workspace(&mut self, _: &ClearWorkspace, _: &mut Window, cx: &mut Context<Self>) {
        update_app_state(cx, |state, _| state.set_workspace(None));
        self.list
            .update(cx, |list, cx| list.set_workspace(None, cx));
    }

    fn on_split_drag(
        &mut self,
        event: &DragMoveEvent<SplitDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rem = f32::from(window.rem_size());
        if rem <= 0.0 {
            return;
        }
        let width = f32::from(event.event.position.x) / rem;
        self.set_left_width_rem(width, window, cx);
    }

    fn refresh_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = window_title(cx);
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
            cx.notify();
        }
    }

    /// Test and QA probes (§15.3): `focus`, `toast.last`, `theme`, and the
    /// panes' `list.*`, `viewer.*`, `deep.*`, `agent.*` keys.
    #[cfg(any(test, feature = "qa"))]
    pub fn qa_probe(&self, key: &str, window: &mut Window, cx: &mut App) -> Option<String> {
        match key {
            "focus" => Some(
                if window.has_active_dialog(cx) {
                    "dialog"
                } else if self.list.read(cx).contains_focus(window, cx) {
                    "list"
                } else if self.deep.read(cx).contains_focus(window, cx) {
                    "deep"
                } else if self
                    .viewer
                    .read(cx)
                    .agent_panel()
                    .read(cx)
                    .contains_focus(window, cx)
                {
                    "agent"
                } else if self.viewer.read(cx).contains_focus(window, cx) {
                    "viewer"
                } else {
                    "none"
                }
                .to_string(),
            ),
            "region" => Some(format!("{:?}", self.focused_region(window, cx)).to_lowercase()),
            // Frames of this window and their UI time (X-007).
            "window.frames" => Some(self.frame_stats.frames().to_string()),
            "window.activations" => Some(self.activations.to_string()),
            "window.input_events" => Some(self.frame_stats.input_events().to_string()),
            "window.frame_ms_total" => Some(format!(
                "{:.3}",
                self.frame_stats.total().as_secs_f64() * 1000.0
            )),
            "window.frame_cpu_ms_total" => {
                crate::globals::cycles_to_ms(self.frame_stats.cycles()).map(|ms| format!("{ms:.3}"))
            }
            "window.frame_last_ms" => Some(format!(
                "{:.3}",
                self.frame_stats.last().as_secs_f64() * 1000.0
            )),
            // The window frame (the whole main window, viewer included) over
            // the last 512 frames (X-008).
            "window.frame_p95" => Some(format!("{:.3}", self.frame_stats.p95_ms())),
            "window.frame_max" => Some(format!("{:.3}", self.frame_stats.max_ms())),
            "window.frame_age_ms" => self
                .frame_stats
                .since_last()
                .map(|age| format!("{:.0}", age.as_secs_f64() * 1000.0)),
            // The split (PD1, X-040).
            "layout.left_collapsed" => Some(self.left_collapsed.to_string()),
            "layout.auto_collapsed" => Some(self.auto_collapsed.to_string()),
            "layout.left_width_rem" => Some(format!(
                "{:.2}",
                if self.left_collapsed {
                    0.0
                } else {
                    self.effective_left_width_rem(window)
                }
            )),
            "layout.left_tab" => Some(
                match self.left_tab {
                    LeftTab::Conversations => "conversations",
                    LeftTab::DeepSearch => "deep",
                }
                .to_string(),
            ),
            "toast.last" => Some(crate::globals::last_toast(cx).to_string()),
            "theme" => Some(theme_tokens::preference(cx).as_str().to_string()),
            // The viewer's agent panel (§15.3 `agent.*`; WP4 also forwards
            // them as `deep.agent.*`).
            key if key.starts_with("agent.") => {
                let panel = self.viewer.read(cx).agent_panel().clone();
                panel.read(cx).qa_probe(key, cx)
            }
            key if key.starts_with("list.") => self.list.read(cx).qa_probe(key, cx),
            key if key.starts_with("viewer.") => self.viewer.read(cx).qa_probe(key, cx),
            key if key.starts_with("deep.") => self.deep.read(cx).qa_probe(key, cx),
            _ => None,
        }
    }
}

/// The window width in rem.
fn window_width_rem(window: &Window) -> f32 {
    let rem = f32::from(window.rem_size());
    if rem <= 0.0 {
        return f32::MAX;
    }
    f32::from(window.viewport_size().width) / rem
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_timer = crate::globals::BusyTimer::start();
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;
        let sidebar = theme.sidebar;
        let border = theme.border;
        let accent = theme.ring;
        let left_width = self.effective_left_width_rem(window);
        let tab_ix = match self.left_tab {
            LeftTab::Conversations => 0,
            LeftTab::DeepSearch => 1,
        };
        let picker = crate::globals::app_state(cx).is_picker();
        let has_workspace = crate::globals::app_state(cx).workspace.is_some();
        // The last child: painted last, it closes this frame's measurement.
        let frame_probe = {
            let stats = self.frame_stats.clone();
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    stats.listen_for_input(window);
                    stats.add(frame_timer.stop());
                },
            )
            .absolute()
            .size_0()
        };

        let root = v_flex()
            .id("workspace")
            .key_context(keymap::CTX_WORKSPACE)
            .size_full()
            .bg(background)
            .text_color(foreground)
            .on_action(cx.listener(Self::on_focus_conversations))
            .on_action(cx.listener(Self::on_focus_deep_search))
            .on_action(cx.listener(Self::on_focus_viewer))
            .on_action(cx.listener(Self::on_toggle_list_pane))
            .on_action(cx.listener(|this, _: &FocusNextRegion, window, cx| {
                this.cycle_region(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPrevRegion, window, cx| {
                this.cycle_region(false, window, cx)
            }))
            .on_action(cx.listener(Self::on_choose_workspace))
            .on_action(cx.listener(Self::on_clear_workspace))
            // Element-level Quit (menu enabled state); the global handler
            // covers dispatch with nothing focused.
            .on_action(cx.listener(|_, _: &Quit, _, cx| exit::quit(cx)))
            // No extra windows in picker mode (PD5): no listener, item disabled.
            .when(!picker, |root| {
                root.on_action(cx.listener(|this, action: &OpenJsonlFile, window, cx| {
                    this.app_action(action, window, cx)
                }))
            })
            // Nothing to show without a workspace: no listener, the item is
            // disabled (T-016).
            .when(has_workspace, |root| {
                root.on_action(
                    cx.listener(|this, action: &ShowProjectsDirectory, window, cx| {
                        this.app_action(action, window, cx)
                    }),
                )
            })
            .on_drag_move(cx.listener(Self::on_split_drag));
        let root = crate::on_app_actions!(root, cx, Self::app_action);
        let root = crate::on_fallback_actions!(root, cx, Self::route_fallback);
        let root = crate::capture_session_actions!(root, cx, Self::capture_list_target_action);

        root.child(AppTitleBar::new(self.menu_bar.clone(), self.title.clone()))
            .child(
                h_flex()
                    .id("main-split")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_stretch()
                    .when(!self.left_collapsed, |this| {
                        this.child(
                            v_flex()
                                .id("left-pane")
                                .test_support()
                                .w(rems(left_width))
                                .flex_none()
                                .h_full()
                                .min_h_0()
                                .bg(sidebar)
                                .child(
                                    div().px_2().pt_1().child(
                                        TabBar::new("left-tabs")
                                            .underline()
                                            .small()
                                            .selected_index(tab_ix)
                                            .on_click(cx.listener(
                                                |this, ix: &usize, window, cx| {
                                                    let tab = if *ix == 0 {
                                                        LeftTab::Conversations
                                                    } else {
                                                        LeftTab::DeepSearch
                                                    };
                                                    this.show_left_tab(tab, window, cx);
                                                },
                                            ))
                                            .child("Conversations")
                                            .child("Deep search"),
                                    ),
                                )
                                .child(div().flex_1().min_h_0().map(|this| match self.left_tab {
                                    LeftTab::Conversations => this.child(self.list.clone()),
                                    LeftTab::DeepSearch => this.child(self.deep.clone()),
                                })),
                        )
                        .child(
                            div()
                                .id("main-split-divider")
                                .w(rems(DIVIDER_REM))
                                .flex_none()
                                .h_full()
                                .cursor(CursorStyle::ResizeLeftRight)
                                .border_l_1()
                                .border_color(border)
                                .hover(move |style| style.bg(accent.opacity(0.4)))
                                .on_drag(SplitDrag, |drag, _, _, cx| cx.new(|_| *drag)),
                        )
                    })
                    .child(
                        div()
                            .id("viewer-area")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.viewer.clone()),
                    ),
            )
            .child(frame_probe)
    }
}

/// Open the main window (§11.1 step 3) and register its should-close
/// handler (closing it quits, PD6).
pub fn open_main_window(cx: &mut App) -> AnyWindowHandle {
    open_main_window_with_view(cx).0
}

/// [`open_main_window`], also returning the root view.
pub fn open_main_window_with_view(cx: &mut App) -> (AnyWindowHandle, Entity<Workspace>) {
    let options = main_window_options(cx);
    gpui_kit::open_window(options, cx, |window, cx| {
        window.on_window_should_close(cx, |_, cx| {
            exit::quit(cx);
            true
        });
        cx.new(|cx| Workspace::new(window, cx))
    })
    .expect("failed to open the main window")
}

/// The main window's options: the remembered placement when
/// `restore_session` is on and one was saved (C-037, clamped to the
/// displays by `persist`), else centered 1280×820 clamped to 85% of the
/// primary display; minimum 880×560 either way (PD1).
pub fn main_window_options(cx: &App) -> WindowOptions {
    persist::with_restored_bounds(
        default_main_window_options(cx),
        persist::WindowKind::Main,
        cx,
    )
}

fn default_main_window_options(cx: &App) -> WindowOptions {
    let (mut width, mut height) = MAIN_WINDOW_SIZE;
    if let Some(display) = cx.primary_display() {
        let bounds = display.bounds();
        width = width
            .min(f32::from(bounds.size.width) * 0.85)
            .max(MAIN_WINDOW_MIN_SIZE.0);
        height = height
            .min(f32::from(bounds.size.height) * 0.85)
            .max(MAIN_WINDOW_MIN_SIZE.1);
    }
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        window_min_size: Some(size(px(MAIN_WINDOW_MIN_SIZE.0), px(MAIN_WINDOW_MIN_SIZE.1))),
        app_id: Some("claude-history-gui".into()),
        #[cfg(target_os = "linux")]
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    }
}

/// File → Show conversations: focus the main window, or open it.
pub fn show_main_window(cx: &mut App) {
    if let Some(handle) = crate::globals::main_panes(cx).window
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    open_main_window(cx);
}

/// A test/QA helper: the main window size as `Size<Pixels>`.
pub fn main_window_size() -> Size<gpui_kit::Pixels> {
    size(px(MAIN_WINDOW_SIZE.0), px(MAIN_WINDOW_SIZE.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_clamp_to_the_tab_range_and_the_window() {
        // A narrow window never pushes a visible pane below its minimum.
        assert_eq!(
            clamp_left_width(28.0, LeftTab::Conversations, Some(33.0)),
            18.0
        );
        assert_eq!(
            clamp_left_width(28.0, LeftTab::DeepSearch, Some(10.0)),
            24.0
        );
        assert_eq!(clamp_left_width(50.0, LeftTab::Conversations, None), 40.0);
        assert_eq!(clamp_left_width(10.0, LeftTab::Conversations, None), 18.0);
        assert_eq!(clamp_left_width(50.0, LeftTab::DeepSearch, None), 48.0);
        // An 880 px window at 16 px/rem is 55rem: the viewer keeps 32rem.
        let width = clamp_left_width(30.0, LeftTab::Conversations, Some(55.0));
        assert!(55.0 - width - DIVIDER_REM >= VIEWER_MIN_REM);
        assert_eq!(
            clamp_left_width(f32::NAN, LeftTab::Conversations, None),
            18.0
        );
    }
}
