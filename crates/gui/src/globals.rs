//! App-wide state (DESIGN §7.3). Frozen: written by WP0, afterwards changed
//! only by the integrator.
//!
//! * [`AppState`]: environment, configuration, launch context, settings,
//!   workspace, view defaults, persisted UI state. Read with [`app_state`],
//!   change with [`update_app_state`] (bumps `revision`; views observe the
//!   global and re-read).
//! * [`DebugBuffer`]: the ring of debug-sink lines shown by Tools → Debug log.
//! * [`MainPanes`], [`ActivePane`]: the main window's panes and the region
//!   that last had focus per window (menu/palette fallback, §7.8).
//! * [`OpenTargets`]: dialogs holding a conversation path; a deletion closes them.
//! * [`SemanticGate`]: exclusive access for Generate/Clear semantic cache (§7.6).
//! * [`LastToast`]: the last notification text (QA probes).
//! * [`CorpusBus`]: broadcasts [`CorpusNotice`]s from any deleting route. The
//!   app itself handles every notice once (closing the `OpenTargets` dialogs,
//!   whichever windows exist); each window root clears its own panes.
//! * [`UiBusy`]: UI-thread time of whole operations (handler plus the effects
//!   it triggers), measured where they start (§15.8, X-038; integrator,
//!   Phase C).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use claude_history_core::HistoryEnv;
use claude_history_core::WorkspaceScope;
use claude_history_core::browser::ListSearchMode;
use claude_history_core::config::LayeredConfig;
use claude_history_core::debug::DebugLevel;
use claude_history_core::settings::EffectiveSettings;
use claude_history_core::view::ViewPrefs;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Entity, EventEmitter, Global,
    SharedString, WeakEntity, Window,
};

use crate::deep_search::DeepSearchPane;
use crate::deep_search::worker::AgentWorkerHandle;
use crate::list::ListPane;
use crate::model::{ConfigErrors, LaunchContext, PaneFocus, SemanticOp};
use crate::persist::UiState;
use crate::viewer::ViewerPane;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Everything the app was started with and the settings in effect.
pub struct AppState {
    /// The environment; `workspace_dir` is kept in sync with `workspace`.
    pub env: Arc<HistoryEnv>,
    /// Effective configuration = gui.toml over config.toml (a failing file
    /// contributes defaults).
    pub config: LayeredConfig,
    /// Per-file configuration errors.
    pub config_errors: ConfigErrors,
    pub launch: LaunchContext,
    /// `core::settings::effective_settings(config.effective, flags, mode)`.
    pub settings: EffectiveSettings,
    pub workspace: Option<WorkspaceScope>,
    /// View preferences for newly opened conversations (PD17, then t/T/i).
    pub view_defaults: ViewPrefs,
    /// The list's initial search mode (PD17).
    pub list_mode: ListSearchMode,
    /// Persisted UI state (state.json).
    pub ui: UiState,
    /// The main window's left pane is hidden automatically by the open agent
    /// panel (X-040); runtime only, not persisted.
    pub list_auto_collapsed: bool,
    /// Bumped on every [`update_app_state`]; observers re-read.
    pub revision: u64,
}

impl Global for AppState {}

impl AppState {
    /// The environment with the current workspace applied.
    pub fn env_with_workspace(&self) -> HistoryEnv {
        let mut env = (*self.env).clone();
        env.workspace_dir = self.workspace.as_ref().map(|w| w.dir().to_path_buf());
        env
    }

    /// Replace the workspace and keep `env.workspace_dir` in sync.
    pub fn set_workspace(&mut self, workspace: Option<WorkspaceScope>) {
        let mut env = (*self.env).clone();
        env.workspace_dir = workspace.as_ref().map(|w| w.dir().to_path_buf());
        self.env = Arc::new(env);
        self.workspace = workspace;
    }

    /// Picker mode (`--show-path`, `--show-id`, `--resume`).
    pub fn is_picker(&self) -> bool {
        self.launch.is_picker()
    }
}

/// The app state. Panics when called before `app::init`.
pub fn app_state(cx: &App) -> &AppState {
    cx.global::<AppState>()
}

/// Mutate the app state; bumps `revision` (observers of the global re-render).
pub fn update_app_state(cx: &mut App, f: impl FnOnce(&mut AppState, &mut App)) {
    cx.update_global::<AppState, _>(|state, cx| {
        f(state, cx);
        state.revision += 1;
    });
}

// ---------------------------------------------------------------------------
// Debug buffer
// ---------------------------------------------------------------------------

/// Number of lines the debug buffer keeps.
pub const DEBUG_BUFFER_LINES: usize = 5_000;

/// One captured debug-sink line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugLine {
    pub at: DateTime<Local>,
    pub level: DebugLevel,
    pub text: String,
}

/// Ring buffer of debug-sink lines (`Send + Sync`: the core sink runs on any thread).
pub struct DebugBuffer {
    lines: Mutex<VecDeque<DebugLine>>,
    capacity: usize,
    /// Capture level of the Debug log dialog (default Info), as `DebugLevel as u8`.
    min_level: AtomicU8,
    /// Lines ever pushed (a ring that is full keeps its length).
    pushed: std::sync::atomic::AtomicU64,
}

impl Default for DebugBuffer {
    fn default() -> Self {
        Self::new(DEBUG_BUFFER_LINES)
    }
}

impl DebugBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            capacity: capacity.max(1),
            min_level: AtomicU8::new(level_to_u8(DebugLevel::Info)),
            pushed: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Append a line (drops the oldest beyond the capacity).
    pub fn push(&self, level: DebugLevel, text: impl Into<String>) {
        let line = DebugLine {
            at: Local::now(),
            level,
            text: text.into(),
        };
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
        self.pushed.fetch_add(1, Ordering::Relaxed);
    }

    /// How many lines were ever pushed; changes whenever a line arrives.
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    /// A copy of every line, oldest first.
    pub fn lines(&self) -> Vec<DebugLine> {
        self.lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Lines at or above `level`, oldest first.
    pub fn lines_at_or_above(&self, level: DebugLevel) -> Vec<DebugLine> {
        self.lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|line| line.level >= level)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// The Debug log dialog's capture level.
    pub fn min_level(&self) -> DebugLevel {
        level_from_u8(self.min_level.load(Ordering::Relaxed))
    }

    pub fn set_min_level(&self, level: DebugLevel) {
        self.min_level.store(level_to_u8(level), Ordering::Relaxed);
    }
}

fn level_to_u8(level: DebugLevel) -> u8 {
    match level {
        DebugLevel::Debug => 0,
        DebugLevel::Info => 1,
        DebugLevel::Warn => 2,
        DebugLevel::Error => 3,
    }
}

fn level_from_u8(value: u8) -> DebugLevel {
    match value {
        0 => DebugLevel::Debug,
        1 => DebugLevel::Info,
        2 => DebugLevel::Warn,
        _ => DebugLevel::Error,
    }
}

/// The process's debug buffer.
#[derive(Clone)]
pub struct DebugBufferHandle(pub Arc<DebugBuffer>);

impl Global for DebugBufferHandle {}

/// The debug buffer (panics before `app::init`).
pub fn debug_buffer(cx: &App) -> Arc<DebugBuffer> {
    cx.global::<DebugBufferHandle>().0.clone()
}

// ---------------------------------------------------------------------------
// Panes and focus
// ---------------------------------------------------------------------------

/// The main window's panes (set by `Workspace::new`).
#[derive(Clone, Default)]
pub struct MainPanes {
    pub list: Option<WeakEntity<ListPane>>,
    pub viewer: Option<WeakEntity<ViewerPane>>,
    pub deep: Option<WeakEntity<DeepSearchPane>>,
    pub window: Option<AnyWindowHandle>,
}

impl Global for MainPanes {}

impl MainPanes {
    pub fn list(&self) -> Option<Entity<ListPane>> {
        self.list.as_ref().and_then(WeakEntity::upgrade)
    }

    pub fn viewer(&self) -> Option<Entity<ViewerPane>> {
        self.viewer.as_ref().and_then(WeakEntity::upgrade)
    }

    pub fn deep(&self) -> Option<Entity<DeepSearchPane>> {
        self.deep.as_ref().and_then(WeakEntity::upgrade)
    }
}

/// The main window's panes (empty before the main window opens).
pub fn main_panes(cx: &App) -> MainPanes {
    cx.try_global::<MainPanes>().cloned().unwrap_or_default()
}

/// The region that last received focus, per window (updated by the panes'
/// `on_focus_in` handlers). The palette/menu fallback for session actions.
#[derive(Clone, Default)]
pub struct ActivePane {
    pub window: Option<AnyWindowHandle>,
    pub pane: PaneFocus,
    /// The last focused region of every window, by window id.
    per_window: BTreeMap<u64, PaneFocus>,
}

impl Global for ActivePane {}

impl ActivePane {
    /// Record that `pane` of `window` received focus.
    pub fn set(cx: &mut App, window: AnyWindowHandle, pane: PaneFocus) {
        let state = cx.default_global::<ActivePane>();
        state.window = Some(window);
        state.pane = pane;
        state.per_window.insert(window.window_id().as_u64(), pane);
    }

    /// The region that last had focus in `window` (`List` when unknown).
    pub fn of_window(cx: &App, window: AnyWindowHandle) -> PaneFocus {
        cx.try_global::<ActivePane>()
            .and_then(|state| state.per_window.get(&window.window_id().as_u64()).copied())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Open targets
// ---------------------------------------------------------------------------

/// Closes one registered dialog when its path is deleted. It runs in the
/// dialog's window, outside any other entity or window update (deferred), so
/// `window.close_dialog(cx)`, `ConfirmControl::close` or entity updates work
/// as written.
pub type CloseFn = Box<dyn FnOnce(&mut Window, &mut App)>;

struct OpenTarget {
    window: AnyWindowHandle,
    path: PathBuf,
    title: SharedString,
    close: CloseFn,
}

/// A dialog [`OpenTargets::close_for`] closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedTarget {
    /// The window the dialog was in (the toast goes there).
    pub window: AnyWindowHandle,
    pub title: SharedString,
}

/// Dialogs and result sets that hold a conversation path (§7.2, PD15). A
/// deletion of that path closes them with the toast
/// `Closed: "<title>" was deleted` (the app's `CorpusBus` handler does both,
/// whichever windows exist).
#[derive(Default)]
pub struct OpenTargets {
    next_id: u64,
    entries: BTreeMap<u64, OpenTarget>,
}

impl Global for OpenTargets {}

impl OpenTargets {
    /// Register a dialog of `window` holding `path`; returns its id for
    /// [`OpenTargets::unregister`] (call it when the dialog closes on its own).
    pub fn register(
        cx: &mut App,
        window: AnyWindowHandle,
        path: PathBuf,
        title: SharedString,
        close: CloseFn,
    ) -> u64 {
        let targets = cx.default_global::<OpenTargets>();
        targets.next_id += 1;
        let id = targets.next_id;
        targets.entries.insert(
            id,
            OpenTarget {
                window,
                path,
                title,
                close,
            },
        );
        id
    }

    /// Forget a dialog (it closed on its own).
    pub fn unregister(cx: &mut App, id: u64) {
        if let Some(targets) = cx.try_global::<OpenTargets>()
            && targets.entries.contains_key(&id)
        {
            cx.global_mut::<OpenTargets>().entries.remove(&id);
        }
    }

    /// Close every registered dialog holding one of `paths`. The entries are
    /// removed now; each close function runs deferred, in its own window and
    /// outside the caller's update (a caller is usually inside some window's
    /// update, where updating that window again fails). A dialog whose window
    /// is gone is skipped (it is gone too). Returns the closed dialogs (for
    /// the `Closed: "<title>" was deleted` toasts).
    pub fn close_for(cx: &mut App, paths: &[PathBuf]) -> Vec<ClosedTarget> {
        let Some(targets) = cx.try_global::<OpenTargets>() else {
            return Vec::new();
        };
        let ids: Vec<u64> = targets
            .entries
            .iter()
            .filter(|(_, entry)| paths.iter().any(|p| same_path(p, &entry.path)))
            .map(|(id, _)| *id)
            .collect();
        let mut closed = Vec::new();
        for id in ids {
            let entry = cx.global_mut::<OpenTargets>().entries.remove(&id);
            if let Some(entry) = entry {
                let OpenTarget {
                    window,
                    title,
                    close,
                    ..
                } = entry;
                cx.defer(move |cx| {
                    window.update(cx, |_, window, cx| close(window, cx)).ok();
                });
                closed.push(ClosedTarget { window, title });
            }
        }
        closed
    }

    /// Whether any registered dialog holds `path`.
    pub fn holds(cx: &App, path: &Path) -> bool {
        cx.try_global::<OpenTargets>()
            .is_some_and(|t| t.entries.values().any(|e| same_path(&e.path, path)))
    }

    pub fn len(cx: &App) -> usize {
        cx.try_global::<OpenTargets>()
            .map_or(0, |t| t.entries.len())
    }

    /// The registered paths and titles (tests: which conversation a dialog
    /// captured).
    #[cfg(test)]
    pub fn entries(cx: &App) -> Vec<(PathBuf, SharedString)> {
        cx.try_global::<OpenTargets>()
            .map(|t| {
                t.entries
                    .values()
                    .map(|entry| (entry.path.clone(), entry.title.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Path equality used for deletions (exact, then case-insensitive on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if cfg!(windows) {
        let a = a.to_string_lossy().replace('/', "\\").to_lowercase();
        let b = b.to_string_lossy().replace('/', "\\").to_lowercase();
        return a == b;
    }
    false
}

// ---------------------------------------------------------------------------
// Semantic gate
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SemanticGateState {
    #[default]
    Open,
    Exclusive(SemanticOp),
}

/// Exclusive access to the semantic cache and model for Generate/Clear
/// (§7.6): entering suspends the list's semantic worker and pauses the agent
/// worker's semantic jobs; leaving resumes them. The agent worker is paused
/// directly (`AgentWorkerHandle`), so the gate holds in single-file sessions
/// and before the main window exists too; the panes only update their UI.
#[derive(Clone, Copy, Debug, Default)]
pub struct SemanticGate {
    pub state: SemanticGateState,
}

impl Global for SemanticGate {}

impl SemanticGate {
    pub fn current(cx: &App) -> SemanticGateState {
        cx.try_global::<SemanticGate>()
            .map(|gate| gate.state)
            .unwrap_or_default()
    }

    pub fn is_open(cx: &App) -> bool {
        Self::current(cx) == SemanticGateState::Open
    }

    /// Take the gate for `op`. Returns `false` (and changes nothing) when it
    /// is already held: a second Generate/Clear is not allowed.
    pub fn enter_exclusive(op: SemanticOp, cx: &mut App) -> bool {
        if !Self::is_open(cx) {
            return false;
        }
        cx.default_global::<SemanticGate>().state = SemanticGateState::Exclusive(op);
        AgentWorkerHandle::global(cx).pause_semantic(true);
        let panes = main_panes(cx);
        if let Some(list) = panes.list() {
            list.update(cx, |list, cx| list.suspend_semantic(cx));
        }
        if let Some(deep) = panes.deep() {
            deep.update(cx, |deep, cx| deep.pause_semantic(true, cx));
        }
        true
    }

    /// Release the gate: the list worker resumes (reloading the cache) and
    /// agent semantic jobs are accepted again.
    pub fn leave(cx: &mut App) {
        if Self::is_open(cx) {
            return;
        }
        cx.default_global::<SemanticGate>().state = SemanticGateState::Open;
        AgentWorkerHandle::global(cx).pause_semantic(false);
        let panes = main_panes(cx);
        if let Some(list) = panes.list() {
            list.update(cx, |list, cx| list.resume_semantic(cx));
        }
        if let Some(deep) = panes.deep() {
            deep.update(cx, |deep, cx| deep.pause_semantic(false, cx));
        }
    }
}

// ---------------------------------------------------------------------------
// Last toast
// ---------------------------------------------------------------------------

/// The text of the last notification pushed through `notify` (QA probes).
#[derive(Clone, Debug, Default)]
pub struct LastToast(pub SharedString);

impl Global for LastToast {}

/// The last notification text (empty when none).
pub fn last_toast(cx: &App) -> SharedString {
    cx.try_global::<LastToast>()
        .map(|toast| toast.0.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// UI-thread busy time of whole operations
// ---------------------------------------------------------------------------

/// The operations [`UiBusy`] measures (§15.8, X-038).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BusySpan {
    /// The list installs the finished initial load (the poll that applied
    /// `Loaded`/`NoHistory`, with every subscriber it notified).
    LoadEnd,
    /// The list installs a finished refresh or reload.
    RefreshEnd,
    /// A background rename is applied (list row, viewer header, dialog).
    RenameApply,
    /// A finished delete is broadcast and every pane drops the conversation.
    DeleteRemoval,
    /// A paste into the focused input (QA `paste` step).
    Paste,
}

impl BusySpan {
    /// Every span, in probe order.
    pub const ALL: [BusySpan; 5] = [
        BusySpan::LoadEnd,
        BusySpan::RefreshEnd,
        BusySpan::RenameApply,
        BusySpan::DeleteRemoval,
        BusySpan::Paste,
    ];

    /// The probe name (`busy.<name>`, `busy.<name>_max`).
    pub fn name(self) -> &'static str {
        match self {
            BusySpan::LoadEnd => "load_end",
            BusySpan::RefreshEnd => "refresh_end",
            BusySpan::RenameApply => "rename_apply",
            BusySpan::DeleteRemoval => "delete_removal",
            BusySpan::Paste => "paste",
        }
    }
}

/// One span's measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BusyRecord {
    /// Wall-clock time of the last run (includes the time the OS ran other
    /// threads instead of the UI thread).
    pub last: Duration,
    pub max: Duration,
    /// CPU cycles the UI thread itself spent in the last run (Windows).
    pub last_cycles: Option<u64>,
    pub count: u32,
    /// When the last run started and ended (its parts: [`SubSpans::within`]).
    pub last_window: Option<(std::time::Instant, std::time::Instant)>,
}

/// UI-thread time of whole operations. Each one is timed around the outermost
/// update that runs it (an async task's `update`, a QA step's dispatch), so
/// the measurement includes the effects the update flushed: emitted events,
/// subscribers, observers — everything but the next frame's render.
#[derive(Clone, Debug, Default)]
pub struct UiBusy {
    records: BTreeMap<BusySpan, BusyRecord>,
    /// The last [`UiBusy::RECENT`] wall-clock times per span (medians).
    recent: BTreeMap<BusySpan, VecDeque<Duration>>,
}

impl Global for UiBusy {}

impl UiBusy {
    /// How many recent runs a span keeps for its median.
    pub const RECENT: usize = 9;

    /// Record one run of `span`.
    pub fn record(cx: &mut App, span: BusySpan, measured: BusyMeasure) {
        let busy = cx.default_global::<UiBusy>();
        let recent = busy.recent.entry(span).or_default();
        if recent.len() == Self::RECENT {
            recent.pop_front();
        }
        recent.push_back(measured.wall);
        let record = busy.records.entry(span).or_default();
        record.last = measured.wall;
        record.max = record.max.max(measured.wall);
        record.last_cycles = measured.cycles;
        record.last_window = measured.window;
        record.count += 1;
    }

    /// The measurements of `span` (`None` before its first run).
    pub fn of(cx: &App, span: BusySpan) -> Option<BusyRecord> {
        cx.try_global::<UiBusy>()
            .and_then(|busy| busy.records.get(&span).copied())
    }

    /// The median wall-clock time of the span's recent runs (the mean of the
    /// middle two for an even count): one hiccup outside the operation (the
    /// OS, work an earlier frame deferred) does not decide a measurement.
    pub fn median(cx: &App, span: BusySpan) -> Option<Duration> {
        let mut recent: Vec<Duration> = cx
            .try_global::<UiBusy>()?
            .recent
            .get(&span)?
            .iter()
            .copied()
            .collect();
        if recent.is_empty() {
            return None;
        }
        recent.sort();
        let mid = recent.len() / 2;
        Some(if recent.len() % 2 == 1 {
            recent[mid]
        } else {
            (recent[mid - 1] + recent[mid]) / 2
        })
    }

    /// How many recent runs [`UiBusy::median`] is taken over.
    pub fn recent_count(cx: &App, span: BusySpan) -> usize {
        cx.try_global::<UiBusy>()
            .and_then(|busy| busy.recent.get(&span))
            .map_or(0, VecDeque::len)
    }
}

/// Parts of the measured spans (tests and QA diagnostics: which handler of
/// an operation took the time; `busy.<span>_parts`). Each part keeps its last
/// measurement and how often it ran.
#[derive(Clone, Debug, Default)]
pub struct SubSpans(pub BTreeMap<&'static str, (BusyMeasure, u64)>);

impl Global for SubSpans {}

impl SubSpans {
    /// The parts whose last run fell inside `window` (`name ms`).
    pub fn within(&self, window: (std::time::Instant, std::time::Instant)) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, (part, _))| {
                part.window
                    .is_some_and(|(start, end)| start >= window.0 && end <= window.1)
            })
            .map(|(name, (part, _))| format!("{name} {:.3}", part.wall.as_secs_f64() * 1000.0))
            .collect()
    }
}

/// Record one run of the part `name` of a span (tests and `qa` builds only).
pub fn record_sub_span(cx: &mut App, name: &'static str, measured: BusyMeasure) {
    #[cfg(any(test, feature = "qa"))]
    {
        let entry = cx.default_global::<SubSpans>().0.entry(name).or_default();
        entry.0 = measured;
        entry.1 += 1;
    }
    #[cfg(not(any(test, feature = "qa")))]
    let _ = (cx, name, measured);
}

/// Time `f` as the part `name` of whatever span runs around it.
pub fn sub_span<R>(cx: &mut App, name: &'static str, f: impl FnOnce(&mut App) -> R) -> R {
    let timer = BusyTimer::start();
    let result = f(cx);
    record_sub_span(cx, name, timer.stop());
    result
}

/// What a [`BusyTimer`] measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BusyMeasure {
    pub wall: Duration,
    pub cycles: Option<u64>,
    /// When the measurement started and ended.
    pub window: Option<(std::time::Instant, std::time::Instant)>,
}

/// Times a span on the UI thread: wall clock and the thread's own CPU
/// cycles, so a measurement can tell work on the UI thread from the OS
/// running other threads in between (worker threads saturating the cores).
#[derive(Clone, Copy, Debug)]
pub struct BusyTimer {
    started: std::time::Instant,
    cycles: Option<u64>,
}

impl BusyTimer {
    pub fn start() -> Self {
        Self {
            started: std::time::Instant::now(),
            cycles: thread_cycles(),
        }
    }

    pub fn stop(self) -> BusyMeasure {
        let ended = std::time::Instant::now();
        let wall = ended - self.started;
        let cycles = match (self.cycles, thread_cycles()) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        };
        BusyMeasure {
            wall,
            cycles,
            window: Some((self.started, ended)),
        }
    }
}

/// CPU cycles the calling thread has used (Windows `QueryThreadCycleTime`;
/// `None` elsewhere).
#[cfg(windows)]
pub fn thread_cycles() -> Option<u64> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::GetCurrentThread;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryThreadCycleTime(thread: HANDLE, cycles: *mut u64) -> i32;
    }
    let mut cycles = 0u64;
    // SAFETY: the pseudo handle of the calling thread and a valid out pointer.
    let ok = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
    (ok != 0).then_some(cycles)
}

/// CPU cycles the calling thread has used (`None`: not measured here).
#[cfg(not(windows))]
pub fn thread_cycles() -> Option<u64> {
    None
}

/// Thread cycles per millisecond, measured by the QA runner at start
/// ([`calibrate_cycles`]); 0 = unknown.
static CYCLES_PER_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Measure how many thread cycles a millisecond of busy work takes (spins
/// for about `spin`, keeps the best of three runs: a run the OS interrupted
/// counts fewer cycles per millisecond).
pub fn calibrate_cycles(spin: Duration) -> Option<u64> {
    let mut best = 0u64;
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let before = thread_cycles()?;
        while started.elapsed() < spin {
            std::hint::spin_loop();
        }
        let cycles = thread_cycles()?.saturating_sub(before);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        if ms > 0.0 {
            best = best.max((cycles as f64 / ms) as u64);
        }
    }
    (best > 0).then(|| {
        CYCLES_PER_MS.store(best, Ordering::Relaxed);
        best
    })
}

/// `cycles` of UI-thread CPU time as milliseconds (`None` before
/// [`calibrate_cycles`]).
pub fn cycles_to_ms(cycles: u64) -> Option<f64> {
    let per_ms = CYCLES_PER_MS.load(Ordering::Relaxed);
    (per_ms > 0).then(|| cycles as f64 / per_ms as f64)
}

// ---------------------------------------------------------------------------
// Corpus bus
// ---------------------------------------------------------------------------

/// Broadcast by every deleting route (session delete, delete by session ID,
/// delete empty transcripts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorpusNotice {
    /// Transcripts removed from disk.
    Deleted(Vec<PathBuf>),
}

/// A small entity that emits [`CorpusNotice`]s; windows subscribe to it.
pub struct CorpusBus;

impl EventEmitter<CorpusNotice> for CorpusBus {}

/// The global [`CorpusBus`] entity.
#[derive(Clone)]
pub struct CorpusBusHandle(pub Entity<CorpusBus>);

impl Global for CorpusBusHandle {}

impl CorpusBus {
    /// Create the bus and the app's own handler (idempotent). The app handler
    /// closes every [`OpenTargets`] dialog holding a deleted path and shows
    /// `Closed: "<title>" was deleted` in that dialog's window — with or
    /// without a main window. Window roots subscribe for their panes (list
    /// rows, viewer, agent result sets).
    pub fn install(cx: &mut App) {
        if cx.try_global::<CorpusBusHandle>().is_none() {
            let bus = cx.new(|_| CorpusBus);
            cx.subscribe(&bus, |_, notice: &CorpusNotice, cx| {
                let timer = BusyTimer::start();
                let CorpusNotice::Deleted(paths) = notice;
                for closed in OpenTargets::close_for(cx, paths) {
                    let title = closed.title;
                    closed
                        .window
                        .update(cx, |_, window, cx| {
                            crate::notify::closed_because_deleted(&title, window, cx)
                        })
                        .ok();
                }
                record_sub_span(cx, "app.close_targets", timer.stop());
            })
            .detach();
            cx.set_global(CorpusBusHandle(bus));
        }
    }

    /// The bus entity (installs it when missing).
    pub fn entity(cx: &mut App) -> Entity<CorpusBus> {
        Self::install(cx);
        cx.global::<CorpusBusHandle>().0.clone()
    }

    /// Emit `notice` to every subscriber.
    pub fn broadcast(notice: CorpusNotice, cx: &mut App) {
        let bus = Self::entity(cx);
        bus.update(cx, |_, cx| cx.emit(notice));
    }
}
