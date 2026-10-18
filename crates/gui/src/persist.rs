//! Persisted UI state, `state.json` (PD17, DESIGN §13.3). WP0 → WP6.
//!
//! Path: `<home>/.local/state/claude-history-gui/state.json` (the directory of
//! `HistoryEnv::debug_log_file`). Every field has a serde default; unknown
//! keys are ignored, so a version-1 file (with `time_filter_text`) reads fine.
//!
//! * [`load`] reads the file at startup: a corrupt file is renamed to
//!   `state.json.bad` (one warning line in the debug log) and defaults are
//!   used; restored values are clamped ([`UiState::sanitized`]: widths to
//!   their ranges, a missing workspace folder is dropped, recents are
//!   deduplicated and capped); with `[gui] restore_session = false` only the
//!   last export folder and the recent workspaces are used.
//! * [`schedule_save`] debounces a save by 500 ms, [`flush`] writes now (quit).
//!   Writes are atomic (temp file + rename) and skipped when nothing changed.
//!   Tests and QA disable writes with [`PersistWrites`].
//! * [`install`] (called with the palette's install, §7.3 step 8) saves on
//!   every `AppState` change that touched `ui`, keeps the workspace and the
//!   recents in sync with `AppState.workspace`, follows the window bounds of
//!   the main and single-file windows and flushes when the app quits.
//! * Window bounds are clamped to the connected displays
//!   ([`clamp_placement`]); [`restored_window_bounds`] is what a window
//!   opens with.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use claude_history_core::HistoryEnv;
use claude_history_core::config::load_gui_config_from;
use claude_history_core::debug::DebugLevel;
use gpui_kit::{App, Bounds, Global, Pixels, Task, Window, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

use crate::globals::{AppState, DebugBufferHandle, same_path};
use crate::model::{AgentTab, LeftTab};

/// Current schema version.
pub const STATE_VERSION: u32 = 2;
/// File name inside the state directory.
pub const STATE_FILE_NAME: &str = "state.json";
/// Where a corrupt file is moved (next to the state file).
pub const BAD_FILE_NAME: &str = "state.json.bad";
/// Recent workspaces kept (most recent first).
pub const MAX_RECENT_WORKSPACES: usize = 10;
/// Saves are debounced by this long (PD17).
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// Agent panel width range (rem, PD1).
pub const AGENT_WIDTH_MIN_REM: f32 = 18.0;
pub const AGENT_WIDTH_MAX_REM: f32 = 28.0;
/// The smallest window a restored placement is shrunk to fit around (logical px).
pub const MIN_WINDOW_SIZE: (f32, f32) = (880.0, 560.0);

/// The contents of `state.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_window: Option<WindowPlacement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_file_window: Option<WindowPlacement>,
    pub split: SplitWidths,
    /// Focus viewer mode (left pane hidden).
    pub list_collapsed: bool,
    pub agent_panel: AgentPanelState,
    pub left_tab: LeftTab,
    pub deep_options_open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    pub recent_workspaces: Vec<PathBuf>,
    /// Last list search mode; used at startup only when no config key is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<RememberedSearchMode>,
    pub view: RememberedView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_export_dir: Option<PathBuf>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            main_window: None,
            single_file_window: None,
            split: SplitWidths::default(),
            list_collapsed: false,
            agent_panel: AgentPanelState::default(),
            left_tab: LeftTab::Conversations,
            deep_options_open: false,
            workspace: None,
            recent_workspaces: Vec::new(),
            search_mode: None,
            view: RememberedView::default(),
            last_export_dir: None,
        }
    }
}

impl UiState {
    /// The state a run restores: widths clamped to their ranges (NaN and
    /// infinities become the minimum), a workspace folder that no longer
    /// exists dropped, the recents deduplicated and capped at
    /// [`MAX_RECENT_WORKSPACES`], the schema version current, window
    /// placements sanity-checked (finite, positive size).
    pub fn sanitized(mut self) -> Self {
        self.version = STATE_VERSION;
        self.split.list_width_rem = clamp_rem(
            self.split.list_width_rem,
            crate::workspace::LIST_WIDTH_MIN_REM,
            crate::workspace::LIST_WIDTH_MAX_REM,
        );
        self.split.deep_width_rem = clamp_rem(
            self.split.deep_width_rem,
            crate::workspace::DEEP_WIDTH_MIN_REM,
            crate::workspace::DEEP_WIDTH_MAX_REM,
        );
        self.split.agent_panel_width_rem = clamp_rem(
            self.split.agent_panel_width_rem,
            AGENT_WIDTH_MIN_REM,
            AGENT_WIDTH_MAX_REM,
        );
        if self.workspace.as_deref().is_some_and(|dir| !dir.is_dir()) {
            self.workspace = None;
        }
        let mut kept: Vec<PathBuf> = Vec::new();
        for dir in std::mem::take(&mut self.recent_workspaces) {
            if !kept.iter().any(|known| same_path(known, &dir)) {
                kept.push(dir);
            }
        }
        kept.truncate(MAX_RECENT_WORKSPACES);
        self.recent_workspaces = kept;
        self.main_window = self.main_window.filter(WindowPlacement::is_usable);
        self.single_file_window = self.single_file_window.filter(WindowPlacement::is_usable);
        self
    }

    /// `restore_session = false`: only the last export folder and the recent
    /// workspaces are used (§13.3); everything else starts at its default.
    pub fn without_session(self) -> Self {
        Self {
            recent_workspaces: self.recent_workspaces,
            last_export_dir: self.last_export_dir,
            ..Self::default()
        }
    }

    /// The recent workspaces that still exist, newest first (the picker offers
    /// only these, L-083).
    pub fn visible_recents(&self) -> Vec<PathBuf> {
        visible_recents(&self.recent_workspaces)
    }

    /// The last export folder when it still exists.
    pub fn export_dir(&self) -> Option<&Path> {
        self.last_export_dir.as_deref().filter(|dir| dir.is_dir())
    }
}

fn clamp_rem(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}

/// `dir` becomes the newest recent workspace: an equal entry moves to the
/// front, the list is capped at [`MAX_RECENT_WORKSPACES`] (L-083).
pub fn add_recent(recents: &mut Vec<PathBuf>, dir: &Path) {
    recents.retain(|known| !same_path(known, dir));
    recents.insert(0, dir.to_path_buf());
    recents.truncate(MAX_RECENT_WORKSPACES);
}

/// The recents whose folder still exists, in order (missing folders are hidden).
pub fn visible_recents(recents: &[PathBuf]) -> Vec<PathBuf> {
    recents.iter().filter(|dir| dir.is_dir()).cloned().collect()
}

/// The folder an export starts in: the remembered one when it exists, else
/// `downloads`, else `home` (C-042).
pub fn export_dir_or_default(
    last: Option<&Path>,
    downloads: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    [last, downloads, home]
        .into_iter()
        .flatten()
        .find(|dir| dir.is_dir())
        .map(Path::to_path_buf)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowPlacementKind {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

/// A window rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlacementBounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PlacementBounds {
    fn is_finite(&self) -> bool {
        [self.x, self.y, self.w, self.h]
            .iter()
            .all(|value| value.is_finite())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowPlacement {
    pub kind: WindowPlacementKind,
    pub bounds: PlacementBounds,
}

impl WindowPlacement {
    fn is_usable(&self) -> bool {
        self.bounds.is_finite() && self.bounds.w > 0.0 && self.bounds.h > 0.0
    }
}

/// Pane widths in rem (clamped on restore, PD1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitWidths {
    pub list_width_rem: f32,
    pub deep_width_rem: f32,
    pub agent_panel_width_rem: f32,
}

impl Default for SplitWidths {
    fn default() -> Self {
        Self {
            list_width_rem: 30.0,
            deep_width_rem: 34.0,
            agent_panel_width_rem: 22.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentPanelState {
    pub visible: bool,
    pub tab: AgentTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberedSearchMode {
    Lexical,
    Semantic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberedToolDisplay {
    Summary,
    Truncated,
    Full,
}

/// Remembered viewer toggles (unset = never remembered).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RememberedView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_display: Option<RememberedToolDisplay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_timing: Option<bool>,
}

/// Whether state.json writes are enabled (tests and QA disable them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersistWrites(pub bool);

impl Global for PersistWrites {}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// `<state dir>/state.json`, next to the debug log.
pub fn state_file(env: &HistoryEnv) -> Option<PathBuf> {
    env.debug_log_file()
        .and_then(|log| log.parent().map(|dir| dir.join(STATE_FILE_NAME)))
}

/// `<state dir>/state.json.bad`.
pub fn bad_file(env: &HistoryEnv) -> Option<PathBuf> {
    state_file(env).map(|path| path.with_file_name(BAD_FILE_NAME))
}

/// Parse state.json text (unknown keys ignored, missing keys defaulted).
pub fn parse(text: &str) -> Result<UiState, String> {
    serde_json::from_str::<UiState>(text).map_err(|error| error.to_string())
}

/// What [`load_with`] found.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadReport {
    pub state: UiState,
    /// A corrupt file was moved here.
    pub moved_to: Option<PathBuf>,
    /// One warning line for the debug log (corrupt file, failed rename).
    pub warning: Option<String>,
}

/// Lines [`load`] could not log yet (it runs before the debug buffer is the
/// owner of anything); [`install`] drains them into the debug log.
static LOAD_WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Read state.json for the startup (§11.1 step 3): defaults when it is missing
/// or unreadable, `.bad` rename when corrupt, `restore_session` from gui.toml.
pub fn load(env: &HistoryEnv) -> UiState {
    let restore = load_gui_config_from(env.gui_config_file().as_deref())
        .map(|gui| gui.gui_settings().restore_session())
        .unwrap_or(true);
    let report = load_with(env, restore);
    if let Some(warning) = report.warning.clone() {
        LOAD_WARNINGS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(warning);
    }
    report.state
}

/// [`load`] with `restore_session` given; reports what happened.
pub fn load_with(env: &HistoryEnv, restore_session: bool) -> LoadReport {
    let mut report = LoadReport {
        state: UiState::default(),
        moved_to: None,
        warning: None,
    };
    let Some(path) = state_file(env) else {
        return report;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
        Err(error) => {
            report.warning = Some(format!(
                "[state] Could not read {}: {error}; using defaults",
                path.display()
            ));
            return report;
        }
    };
    match parse(&text) {
        Ok(state) => {
            let state = state.sanitized();
            report.state = if restore_session {
                state
            } else {
                state.without_session()
            };
        }
        Err(error) => {
            let bad = path.with_file_name(BAD_FILE_NAME);
            let _ = std::fs::remove_file(&bad);
            match std::fs::rename(&path, &bad) {
                Ok(()) => {
                    report.warning = Some(format!(
                        "[state] {} is not valid ({error}); moved to {} and using defaults",
                        path.display(),
                        bad.display()
                    ));
                    report.moved_to = Some(bad);
                }
                Err(rename_error) => {
                    report.warning = Some(format!(
                        "[state] {} is not valid ({error}) and could not be moved aside \
                         ({rename_error}); using defaults",
                        path.display()
                    ));
                }
            }
        }
    }
    report
}

/// Write `state` to `path` atomically (temp file in the same folder + rename).
pub fn write_state(path: &Path, state: &UiState) -> Result<(), String> {
    let text = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text.as_bytes())
        .map_err(|error| format!("Failed to write {}: {error}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        format!("Failed to replace {}: {error}", path.display())
    })
}

// ---------------------------------------------------------------------------
// Window placements
// ---------------------------------------------------------------------------

/// The window kinds that remember their bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WindowKind {
    Main,
    SingleFile,
}

/// A display's usable rectangle (logical pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl DisplayRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn overlap(&self, bounds: &PlacementBounds) -> f32 {
        let width = (self.x + self.w).min(bounds.x + bounds.w) - self.x.max(bounds.x);
        let height = (self.y + self.h).min(bounds.y + bounds.h) - self.y.max(bounds.y);
        if width > 0.0 && height > 0.0 {
            width * height
        } else {
            0.0
        }
    }
}

/// Fit a restored placement onto the connected `displays` (the first one is
/// the primary display, C-037): the display with the largest overlap hosts
/// the window; a window outside every display moves to the primary display
/// (centered); an oversized window shrinks to its display (never below the
/// minimum window size unless the display is smaller); a partly hidden one
/// moves fully inside. Without displays the placement is returned unchanged.
pub fn clamp_placement(placement: WindowPlacement, displays: &[DisplayRect]) -> WindowPlacement {
    let Some(primary) = displays.first() else {
        return placement;
    };
    let bounds = placement.bounds;
    let host = displays
        .iter()
        .map(|display| (display, display.overlap(&bounds)))
        .filter(|(_, overlap)| *overlap > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(display, _)| display);
    let outside = host.is_none();
    let host = host.unwrap_or(primary);
    let width = bounds.w.min(host.w).max(MIN_WINDOW_SIZE.0.min(host.w));
    let height = bounds.h.min(host.h).max(MIN_WINDOW_SIZE.1.min(host.h));
    let (x, y) = if outside {
        (
            host.x + (host.w - width) / 2.0,
            host.y + (host.h - height) / 2.0,
        )
    } else {
        (
            bounds.x.clamp(host.x, host.x + host.w - width),
            bounds.y.clamp(host.y, host.y + host.h - height),
        )
    };
    WindowPlacement {
        kind: placement.kind,
        bounds: PlacementBounds {
            x,
            y,
            w: width,
            h: height,
        },
    }
}

/// The connected displays' rectangles, primary first.
pub fn display_rects(cx: &App) -> Vec<DisplayRect> {
    let rect = |bounds: Bounds<Pixels>| {
        DisplayRect::new(
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
        )
    };
    let primary = cx.primary_display();
    let mut rects = Vec::new();
    if let Some(primary) = &primary {
        rects.push(rect(primary.visible_bounds()));
    }
    for display in cx.displays() {
        if primary
            .as_ref()
            .is_some_and(|primary| primary.id() == display.id())
        {
            continue;
        }
        rects.push(rect(display.visible_bounds()));
    }
    rects
}

/// A window's placement as stored.
pub fn placement_of(bounds: WindowBounds) -> WindowPlacement {
    let (kind, inner) = match bounds {
        WindowBounds::Windowed(bounds) => (WindowPlacementKind::Windowed, bounds),
        WindowBounds::Maximized(bounds) => (WindowPlacementKind::Maximized, bounds),
        WindowBounds::Fullscreen(bounds) => (WindowPlacementKind::Fullscreen, bounds),
    };
    WindowPlacement {
        kind,
        bounds: PlacementBounds {
            x: f32::from(inner.origin.x),
            y: f32::from(inner.origin.y),
            w: f32::from(inner.size.width),
            h: f32::from(inner.size.height),
        },
    }
}

/// A stored placement as window bounds.
pub fn window_bounds_of(placement: &WindowPlacement) -> WindowBounds {
    let bounds = Bounds::new(
        point(px(placement.bounds.x), px(placement.bounds.y)),
        size(px(placement.bounds.w), px(placement.bounds.h)),
    );
    match placement.kind {
        WindowPlacementKind::Windowed => WindowBounds::Windowed(bounds),
        WindowPlacementKind::Maximized => WindowBounds::Maximized(bounds),
        WindowPlacementKind::Fullscreen => WindowBounds::Fullscreen(bounds),
    }
}

/// The bounds a window of `kind` opens with: the remembered placement clamped
/// to the connected displays (`restore_session` on), else `None` (the window
/// uses its default size).
pub fn restored_window_bounds(kind: WindowKind, cx: &App) -> Option<WindowBounds> {
    let state = cx.try_global::<AppState>()?;
    if !state.config.gui.restore_session() {
        return None;
    }
    let placement = match kind {
        WindowKind::Main => state.ui.main_window,
        WindowKind::SingleFile => state.ui.single_file_window,
    }?;
    let placement = clamp_placement(placement, &display_rects(cx));
    Some(window_bounds_of(&placement))
}

/// `options` with the remembered bounds of `kind` ([`restored_window_bounds`])
/// in place of the default ones, when there are any. The window code calls
/// this on the options it builds (`workspace::main_window_options`,
/// `single_file::open_window`), so a saved maximized state is re-applied too.
pub fn with_restored_bounds(
    mut options: gpui_kit::WindowOptions,
    kind: WindowKind,
    cx: &App,
) -> gpui_kit::WindowOptions {
    if let Some(bounds) = restored_window_bounds(kind, cx) {
        options.window_bounds = Some(bounds);
    }
    options
}

// ---------------------------------------------------------------------------
// Runtime: debounce, save, sync
// ---------------------------------------------------------------------------

/// The save pipeline's state.
#[derive(Default)]
struct PersistRuntime {
    /// The pending debounced save (replaced by a later [`schedule_save`]).
    pending: Option<Task<()>>,
    /// What the file holds (or was last written); an identical state is not written.
    last_written: Option<UiState>,
    /// The live placement of each window kind, merged into `ui` when saving
    /// (window moves are not `AppState` changes).
    placements: BTreeMap<WindowKind, WindowPlacement>,
    /// `ui` at the last observed `AppState` change (change detection).
    observed: Option<UiState>,
    installed: bool,
}

impl Global for PersistRuntime {}

fn writes_enabled(cx: &App) -> bool {
    cx.try_global::<PersistWrites>()
        .is_none_or(|writes| writes.0)
}

/// Debounce (500 ms) a save of `AppState.ui`. A later call restarts the timer.
/// No-op while writes are disabled (tests, QA) or before `AppState` exists.
pub fn schedule_save(cx: &mut App) {
    if !writes_enabled(cx) || cx.try_global::<AppState>().is_none() {
        return;
    }
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DEBOUNCE).await;
        cx.update(write_now);
    });
    cx.default_global::<PersistRuntime>().pending = Some(task);
}

/// Write `AppState.ui` now (quit, tests). Cancels a pending debounced save.
pub fn flush(cx: &mut App) {
    if cx.try_global::<PersistRuntime>().is_some() {
        cx.global_mut::<PersistRuntime>().pending = None;
    }
    if writes_enabled(cx) && cx.try_global::<AppState>().is_some() {
        write_now(cx);
    }
}

/// The state to write: `ui` with the live window placements merged in.
fn current_state(cx: &App) -> Option<UiState> {
    let state = cx.try_global::<AppState>()?;
    let mut ui = state.ui.clone();
    if let Some(runtime) = cx.try_global::<PersistRuntime>() {
        if let Some(placement) = runtime.placements.get(&WindowKind::Main) {
            ui.main_window = Some(*placement);
        }
        if let Some(placement) = runtime.placements.get(&WindowKind::SingleFile) {
            ui.single_file_window = Some(*placement);
        }
    }
    ui.version = STATE_VERSION;
    Some(ui)
}

fn debug_line(cx: &App, level: DebugLevel, text: String) {
    if let Some(buffer) = cx.try_global::<DebugBufferHandle>() {
        buffer.0.push(level, text);
    }
}

fn write_now(cx: &mut App) {
    let Some(ui) = current_state(cx) else {
        return;
    };
    let unchanged = cx
        .try_global::<PersistRuntime>()
        .is_some_and(|runtime| runtime.last_written.as_ref() == Some(&ui));
    if unchanged {
        return;
    }
    let Some(path) = cx
        .try_global::<AppState>()
        .and_then(|state| state_file(&state.env))
    else {
        return;
    };
    match write_state(&path, &ui) {
        Ok(()) => cx.default_global::<PersistRuntime>().last_written = Some(ui),
        Err(error) => debug_line(cx, DebugLevel::Warn, format!("[state] {error}")),
    }
}

/// Make `ui` changes persist on their own: save (debounced) whenever an
/// `AppState` change touched `ui`, keep `ui.workspace` and the recents in
/// sync with `AppState.workspace` (C-039), follow the window bounds of main
/// and single-file windows (C-037) and flush when the app quits. Also moves
/// the startup warnings of [`load`] into the debug log. Idempotent.
pub fn install(cx: &mut App) {
    if cx
        .try_global::<PersistRuntime>()
        .is_some_and(|runtime| runtime.installed)
    {
        return;
    }
    // What was loaded counts as written: only a real change is saved.
    let loaded = cx.try_global::<AppState>().map(|state| state.ui.clone());
    let runtime = cx.default_global::<PersistRuntime>();
    runtime.installed = true;
    runtime.observed = loaded.clone();
    runtime.last_written = loaded;
    let warnings = std::mem::take(
        &mut *LOAD_WARNINGS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    for warning in warnings {
        debug_line(cx, DebugLevel::Warn, warning);
    }
    cx.observe_global::<AppState>(on_app_state_changed).detach();
    cx.observe_new(|_: &mut crate::workspace::Workspace, window, cx| {
        if let Some(window) = window {
            track_window(WindowKind::Main, window, cx);
        }
    })
    .detach();
    cx.observe_new(|_: &mut crate::single_file::SingleFileWindow, window, cx| {
        if let Some(window) = window {
            track_window(WindowKind::SingleFile, window, cx);
        }
    })
    .detach();
    cx.on_app_quit(|cx| {
        flush(cx);
        async {}
    })
    .detach();
}

fn track_window<T: 'static>(kind: WindowKind, window: &mut Window, cx: &mut gpui_kit::Context<T>) {
    cx.observe_window_bounds(window, move |_, window, cx| {
        note_window_bounds(kind, window.window_bounds(), cx);
    })
    .detach();
}

/// Remember the live bounds of a window kind (saved with the next save).
pub fn note_window_bounds(kind: WindowKind, bounds: WindowBounds, cx: &mut App) {
    let placement = placement_of(bounds);
    if !placement.is_usable() {
        return;
    }
    let runtime = cx.default_global::<PersistRuntime>();
    if runtime.placements.get(&kind) == Some(&placement) {
        return;
    }
    runtime.placements.insert(kind, placement);
    schedule_save(cx);
}

fn on_app_state_changed(cx: &mut App) {
    let Some(state) = cx.try_global::<AppState>() else {
        return;
    };
    let workspace = state.workspace.as_ref().map(|w| w.dir().to_path_buf());
    let mut ui = state.ui.clone();
    // Workspace + recents follow the live workspace.
    let before = ui.clone();
    if ui.workspace != workspace {
        ui.workspace = workspace.clone();
    }
    if let Some(dir) = &workspace
        && ui.recent_workspaces.first() != Some(dir)
    {
        add_recent(&mut ui.recent_workspaces, dir);
    }
    if ui != before {
        let ui_synced = ui.clone();
        cx.global_mut::<AppState>().ui = ui_synced;
    }
    let changed = {
        let runtime = cx.default_global::<PersistRuntime>();
        let changed = runtime.observed.as_ref() != Some(&ui);
        runtime.observed = Some(ui);
        changed
    };
    if changed {
        schedule_save(cx);
    }
}

// ---------------------------------------------------------------------------
// Remembering (entry points for the panes)
// ---------------------------------------------------------------------------

/// Change `AppState.ui` and schedule a save when it changed.
pub fn update_ui(cx: &mut App, f: impl FnOnce(&mut UiState)) {
    let mut changed = false;
    crate::globals::update_app_state(cx, |state, _| {
        let before = state.ui.clone();
        f(&mut state.ui);
        changed = state.ui != before;
    });
    if changed {
        schedule_save(cx);
    }
}

/// Remember the viewer toggles of the last change (V-032, C-041).
pub fn remember_view(view: RememberedView, cx: &mut App) {
    update_ui(cx, |ui| ui.view = view);
}

/// Remember the list's search mode (C-040).
pub fn remember_search_mode(mode: RememberedSearchMode, cx: &mut App) {
    update_ui(cx, |ui| ui.search_mode = Some(mode));
}

/// Remember the folder of the last export (C-042).
pub fn remember_export_dir(dir: &Path, cx: &mut App) {
    update_ui(cx, |ui| ui.last_export_dir = Some(dir.to_path_buf()));
}

/// Remember the agent panel's visibility, tab and width (C-038).
pub fn remember_agent_panel(visible: bool, tab: AgentTab, width_rem: Option<f32>, cx: &mut App) {
    update_ui(cx, |ui| {
        ui.agent_panel = AgentPanelState { visible, tab };
        if let Some(width) = width_rem {
            ui.split.agent_panel_width_rem =
                clamp_rem(width, AGENT_WIDTH_MIN_REM, AGENT_WIDTH_MAX_REM);
        }
    });
}

/// Remember the Deep search options disclosure (C-038).
pub fn remember_deep_options(open: bool, cx: &mut App) {
    update_ui(cx, |ui| ui.deep_options_open = open);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let state = UiState::default();
        assert_eq!(state.version, STATE_VERSION);
        assert_eq!(state.split.list_width_rem, 30.0);
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(parse(&text).unwrap(), state);
    }

    #[test]
    fn design_example_parses() {
        let text = r#"{
          "version": 2,
          "main_window": { "kind": "maximized", "bounds": {"x":0,"y":0,"w":1280,"h":820} },
          "split": { "list_width_rem": 28.0, "deep_width_rem": 34.0, "agent_panel_width_rem": 22.0 },
          "list_collapsed": false,
          "agent_panel": { "visible": true, "tab": "within" },
          "left_tab": "deep_search",
          "deep_options_open": true,
          "workspace": "C:\\git\\proj",
          "recent_workspaces": ["C:\\git\\proj", "C:\\git\\api"],
          "search_mode": "semantic",
          "view": { "tool_display": "full", "show_thinking": true, "show_timing": false },
          "last_export_dir": "C:\\Users\\me\\Downloads",
          "time_filter_text": "2d"
        }"#;
        let state = parse(text).unwrap();
        assert_eq!(
            state.main_window.unwrap().kind,
            WindowPlacementKind::Maximized
        );
        assert_eq!(state.split.list_width_rem, 28.0);
        assert_eq!(state.agent_panel.tab, AgentTab::Within);
        assert_eq!(state.left_tab, LeftTab::DeepSearch);
        assert_eq!(state.search_mode, Some(RememberedSearchMode::Semantic));
        assert_eq!(state.view.tool_display, Some(RememberedToolDisplay::Full));
        assert_eq!(state.recent_workspaces.len(), 2);
    }

    #[test]
    fn empty_object_is_all_defaults() {
        assert_eq!(parse("{}").unwrap(), UiState::default());
    }
}

#[cfg(test)]
#[path = "persist_tests.rs"]
mod persist_tests;
