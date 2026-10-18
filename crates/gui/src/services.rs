//! Every side effect the GPUI test platform cannot perform, that touches the
//! user's desktop, or that must never run in tests/QA goes through
//! [`PlatformServices`] (DESIGN §7.4). The CI safety lint forbids the
//! underlying calls anywhere else in `crates/gui/src`.
//!
//! * The trait signature is frozen (WP0); new methods only via the integrator.
//! * [`RecordingServices`] (WP0, complete): records every call, answers
//!   prompts from queues, keeps an in-memory clipboard, returns scripted
//!   results. Used by every test and by the QA runner
//!   ([`Services::for_launch`]: a `qa` build started with `--qa-scenario`).
//! * [`RealServices`] (WP0 → WP7): the gpui/OS implementation.
//! * [`ClipboardGuard`]: whenever the services record (tests, QA), gpui-kit's
//!   own clipboard actions (Input/Textarea Copy, Cut, Paste; the root's and
//!   text views' Copy) are taken over in the capture phase and run through
//!   the services, so a test or QA run never reads or writes the user's real
//!   clipboard, whatever has focus (dialogs included).
//!
//! gpui-kit components that perform a side effect themselves must not be
//! used without routing it through these services: `Link` with an `href`,
//! `PopupMenu::link`, a `TextView` without a link handler (they call
//! `App::open_url`), and the `Clipboard` copy button (writes the clipboard).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claude_history_core::resume::{LaunchSpec, TerminalCommand};
use claude_history_core::update::UpdateStatus;
use gpui_kit::base::{Root, RootPlugin, TextSelection};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Copy, Cut, Paste};
use gpui_kit::{
    App, ClipboardItem, Context, Div, Global, InteractiveElement as _, IntoElement,
    PathPromptOptions, Render, Stateful, Window, div,
};

use crate::model::{LaunchContext, OpenPathsOptions, StdoutKind};

/// Completion of [`PlatformServices::prompt_save_path`]: `Ok(None)` = cancelled.
pub type SaveDone = Box<dyn FnOnce(Result<Option<PathBuf>, String>, &mut Window, &mut App)>;
/// Completion of [`PlatformServices::prompt_open_paths`]: `Ok(None)` = cancelled.
pub type OpenDone = Box<dyn FnOnce(Result<Option<Vec<PathBuf>>, String>, &mut Window, &mut App)>;

/// The seam for every desktop side effect (frozen).
pub trait PlatformServices: Send + Sync + 'static {
    fn reveal_path(&self, path: &Path, cx: &mut App);
    fn open_url(&self, url: &str, cx: &mut App);
    fn open_with_system(&self, path: &Path, cx: &mut App);
    fn prompt_save_path(
        &self,
        dir: &Path,
        suggested_name: &str,
        window: &mut Window,
        cx: &mut App,
        done: SaveDone,
    );
    fn prompt_open_paths(
        &self,
        options: OpenPathsOptions,
        window: &mut Window,
        cx: &mut App,
        done: OpenDone,
    );
    fn clipboard_write(&self, text: String, cx: &mut App);
    fn clipboard_read(&self, cx: &mut App) -> Option<String>;
    /// Blocking (≤ `wait`); call from a background task. Real: core
    /// `terminal::spawn_terminal_checked` (K17). `Ok(None)` = still running
    /// after `wait`; `Ok(Some(code))` = exited early with `code`; `Err` =
    /// could not spawn.
    fn spawn_terminal(
        &self,
        command: &TerminalCommand,
        wait: Duration,
    ) -> Result<Option<i32>, String>;
    /// Unix picker resume: flush, close windows, chdir + execvp. The real
    /// implementation never returns on success (the process image is
    /// replaced), so it only ever returns `Err`. [`RecordingServices`]
    /// returns `Ok(())` to stand for that success: callers treat `Ok` as
    /// "the launching terminal took over" and quit with exit 0 and no output.
    fn exec_in_current_terminal(&self, spec: &LaunchSpec) -> Result<(), String>;
    /// `Redirected | Terminal | None` (after `AttachConsole` on Windows).
    fn stdout_kind(&self) -> StdoutKind;
    /// Unix picker exec decision (PD5); always `false` on Windows.
    fn stdin_is_tty(&self) -> bool;
    fn write_stdout(&self, text: &str) -> Result<(), String>;
    fn write_stderr(&self, text: &str);
    /// Blocking; call from a background task. Real: core `update::check_latest_release`.
    fn fetch_latest_release(&self, current_version: &str) -> Result<UpdateStatus, String>;
    /// Blocking (≤ 3 s); background. macOS/Linux login + interactive shell
    /// PATH (§14.2); `None` on Windows.
    fn login_shell_path(&self) -> Option<OsString>;
}

/// The services of this process (a global).
#[derive(Clone)]
pub struct Services {
    pub inner: Arc<dyn PlatformServices>,
    /// Set when `inner` is a [`RecordingServices`] (tests, QA).
    pub recording: Option<Arc<RecordingServices>>,
}

impl Global for Services {}

impl Services {
    /// The real OS services.
    pub fn real() -> Self {
        Self {
            inner: Arc::new(RealServices::new()),
            recording: None,
        }
    }

    /// Recording services (tests and the QA runner).
    pub fn recording(recording: Arc<RecordingServices>) -> Self {
        Self {
            inner: recording.clone(),
            recording: Some(recording),
        }
    }

    /// The services of a GUI run (`app::run`): a `qa` build started with
    /// `--qa-scenario` records every side effect (§15.3; with
    /// `--qa-real-console` stdout/stderr go to the real console); every
    /// other run uses the real OS services.
    pub fn for_launch(launch: &LaunchContext) -> Self {
        #[cfg(feature = "qa")]
        if let Some(qa) = &launch.qa {
            let recording = if qa.real_console {
                RecordingServices::with_real_console()
            } else {
                RecordingServices::new()
            };
            return Self::recording(Arc::new(recording));
        }
        let _ = launch;
        Self::real()
    }

    /// Whether every side effect is recorded instead of performed.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }
}

impl std::ops::Deref for Services {
    type Target = dyn PlatformServices;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

/// The installed services (panics before `app::init`).
pub fn services(cx: &App) -> Services {
    cx.global::<Services>().clone()
}

/// Every call recorded so far. Tests and QA only: panics without
/// [`RecordingServices`].
pub fn recorded_calls(cx: &App) -> Vec<ServiceCall> {
    services(cx)
        .recording
        .expect("recorded_calls needs RecordingServices")
        .calls()
}

// ---------------------------------------------------------------------------
// RecordingServices
// ---------------------------------------------------------------------------

/// One recorded side effect.
#[derive(Clone, Debug, PartialEq)]
pub enum ServiceCall {
    Reveal(PathBuf),
    OpenUrl(String),
    OpenWithSystem(PathBuf),
    PromptSave {
        dir: PathBuf,
        name: String,
    },
    PromptOpen(OpenPathsOptions),
    ClipboardWrite(String),
    Terminal(TerminalCommand),
    Exec(LaunchSpec),
    Stdout(String),
    /// A `write_stdout` that failed (no stdout); nothing was written.
    StdoutFailed(String),
    Stderr(String),
    FetchRelease,
    LoginShellPath,
}

struct RecordingState {
    calls: Vec<ServiceCall>,
    save_answers: VecDeque<Result<Option<PathBuf>, String>>,
    open_answers: VecDeque<Result<Option<Vec<PathBuf>>, String>>,
    clipboard: Option<String>,
    stdout_kind: StdoutKind,
    stdin_is_tty: bool,
    release: Result<UpdateStatus, String>,
    spawn_result: Result<Option<i32>, String>,
    exec_result: Result<(), String>,
    login_shell_path: Option<OsString>,
    /// `--qa-real-console`: stdout/stderr (and their kinds) are the real
    /// console's; the calls are still recorded.
    real_console: bool,
}

/// Records every call and answers from scripted values (tests and QA).
///
/// Prompts take the next queued answer; an unanswered prompt answers
/// `Ok(None)` (cancel). Answers are delivered on the window after the
/// current update (`window.defer`), i.e. by the test's next `run_until_parked`.
/// Queue answers *before* triggering the action that opens the prompt.
pub struct RecordingServices {
    state: Mutex<RecordingState>,
}

impl Default for RecordingServices {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingServices {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RecordingState {
                calls: Vec::new(),
                save_answers: VecDeque::new(),
                open_answers: VecDeque::new(),
                clipboard: None,
                stdout_kind: StdoutKind::None,
                stdin_is_tty: false,
                release: Err("no release scripted (RecordingServices::set_release)".to_string()),
                spawn_result: Ok(None),
                exec_result: Ok(()),
                login_shell_path: None,
                real_console: false,
            }),
        }
    }

    /// Recording services whose stdout/stderr are the real console
    /// (`--qa-real-console`, used to verify picker output end to end).
    pub fn with_real_console() -> Self {
        let services = Self::new();
        services.state().real_console = true;
        services
    }

    fn state(&self) -> std::sync::MutexGuard<'_, RecordingState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, call: ServiceCall) {
        self.state().calls.push(call);
    }

    /// Queue the answer of the next save prompt.
    pub fn answer_save(&self, answer: Result<Option<PathBuf>, String>) {
        self.state().save_answers.push_back(answer);
    }

    /// Queue the answer of the next open prompt.
    pub fn answer_open(&self, answer: Result<Option<Vec<PathBuf>>, String>) {
        self.state().open_answers.push_back(answer);
    }

    pub fn set_stdout_kind(&self, kind: StdoutKind) {
        self.state().stdout_kind = kind;
    }

    pub fn set_stdin_is_tty(&self, tty: bool) {
        self.state().stdin_is_tty = tty;
    }

    /// The result of every following `fetch_latest_release`.
    pub fn set_release(&self, result: Result<UpdateStatus, String>) {
        self.state().release = result;
    }

    /// The result of every following `spawn_terminal` (default `Ok(None)`).
    pub fn set_spawn_result(&self, result: Result<Option<i32>, String>) {
        self.state().spawn_result = result;
    }

    /// The result of every following `exec_in_current_terminal` (default
    /// `Ok(())` = the exec succeeded; see the trait method).
    pub fn set_exec_result(&self, result: Result<(), String>) {
        self.state().exec_result = result;
    }

    pub fn set_login_shell_path(&self, path: Option<OsString>) {
        self.state().login_shell_path = path;
    }

    /// Replace the in-memory clipboard (as if the user copied elsewhere).
    pub fn set_clipboard(&self, text: Option<String>) {
        self.state().clipboard = text;
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<ServiceCall> {
        self.state().calls.clone()
    }

    /// Forget the recorded calls.
    pub fn clear_calls(&self) {
        self.state().calls.clear();
    }

    /// The in-memory clipboard.
    pub fn clipboard(&self) -> Option<String> {
        self.state().clipboard.clone()
    }

    /// The recorded terminal launches.
    pub fn terminal_commands(&self) -> Vec<TerminalCommand> {
        self.state()
            .calls
            .iter()
            .filter_map(|call| match call {
                ServiceCall::Terminal(command) => Some(command.clone()),
                _ => None,
            })
            .collect()
    }

    /// Everything written to stdout, concatenated.
    pub fn stdout(&self) -> String {
        self.state()
            .calls
            .iter()
            .filter_map(|call| match call {
                ServiceCall::Stdout(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Everything written to stderr, concatenated.
    pub fn stderr(&self) -> String {
        self.state()
            .calls
            .iter()
            .filter_map(|call| match call {
                ServiceCall::Stderr(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

impl PlatformServices for RecordingServices {
    fn reveal_path(&self, path: &Path, _cx: &mut App) {
        self.record(ServiceCall::Reveal(path.to_path_buf()));
    }

    fn open_url(&self, url: &str, _cx: &mut App) {
        self.record(ServiceCall::OpenUrl(url.to_string()));
    }

    fn open_with_system(&self, path: &Path, _cx: &mut App) {
        self.record(ServiceCall::OpenWithSystem(path.to_path_buf()));
    }

    fn prompt_save_path(
        &self,
        dir: &Path,
        suggested_name: &str,
        window: &mut Window,
        cx: &mut App,
        done: SaveDone,
    ) {
        let answer = {
            let mut state = self.state();
            state.calls.push(ServiceCall::PromptSave {
                dir: dir.to_path_buf(),
                name: suggested_name.to_string(),
            });
            state.save_answers.pop_front().unwrap_or(Ok(None))
        };
        window.defer(cx, move |window, cx| done(answer, window, cx));
    }

    fn prompt_open_paths(
        &self,
        options: OpenPathsOptions,
        window: &mut Window,
        cx: &mut App,
        done: OpenDone,
    ) {
        let answer = {
            let mut state = self.state();
            state.calls.push(ServiceCall::PromptOpen(options));
            state.open_answers.pop_front().unwrap_or(Ok(None))
        };
        window.defer(cx, move |window, cx| done(answer, window, cx));
    }

    fn clipboard_write(&self, text: String, _cx: &mut App) {
        let mut state = self.state();
        state.calls.push(ServiceCall::ClipboardWrite(text.clone()));
        state.clipboard = Some(text);
    }

    fn clipboard_read(&self, _cx: &mut App) -> Option<String> {
        self.state().clipboard.clone()
    }

    fn spawn_terminal(
        &self,
        command: &TerminalCommand,
        _wait: Duration,
    ) -> Result<Option<i32>, String> {
        let mut state = self.state();
        state.calls.push(ServiceCall::Terminal(command.clone()));
        state.spawn_result.clone()
    }

    fn exec_in_current_terminal(&self, spec: &LaunchSpec) -> Result<(), String> {
        let mut state = self.state();
        state.calls.push(ServiceCall::Exec(spec.clone()));
        state.exec_result.clone()
    }

    fn stdout_kind(&self) -> StdoutKind {
        let state = self.state();
        if state.real_console {
            return crate::ops::console::stdout_kind();
        }
        state.stdout_kind
    }

    fn stdin_is_tty(&self) -> bool {
        let state = self.state();
        if state.real_console {
            return crate::ops::console::stdin_is_tty();
        }
        state.stdin_is_tty
    }

    fn write_stdout(&self, text: &str) -> Result<(), String> {
        let mut state = self.state();
        let result = if state.real_console {
            crate::ops::console::write_stdout(text)
        } else if state.stdout_kind == StdoutKind::None {
            Err("no stdout".to_string())
        } else {
            Ok(())
        };
        match &result {
            Ok(()) => state.calls.push(ServiceCall::Stdout(text.to_string())),
            Err(_) => state
                .calls
                .push(ServiceCall::StdoutFailed(text.to_string())),
        }
        result
    }

    fn write_stderr(&self, text: &str) {
        let real_console = {
            let mut state = self.state();
            state.calls.push(ServiceCall::Stderr(text.to_string()));
            state.real_console
        };
        if real_console {
            crate::ops::console::write_stderr(text);
        }
    }

    fn fetch_latest_release(&self, _current_version: &str) -> Result<UpdateStatus, String> {
        let mut state = self.state();
        state.calls.push(ServiceCall::FetchRelease);
        state.release.clone()
    }

    fn login_shell_path(&self) -> Option<OsString> {
        let mut state = self.state();
        state.calls.push(ServiceCall::LoginShellPath);
        state.login_shell_path.clone()
    }
}

// ---------------------------------------------------------------------------
// Clipboard guard (tests and QA)
// ---------------------------------------------------------------------------

/// A root plugin that takes over gpui-kit's clipboard actions in every window
/// while the services record ([`install_clipboard_guard`]). Its capture-phase
/// listeners sit on the window root, above the app view and every dialog.
pub struct ClipboardGuard;

impl Render for ClipboardGuard {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl RootPlugin for ClipboardGuard {
    fn style(&self, surface: &mut Stateful<Div>, _: &mut Window, _: &mut App) {
        let interactivity = surface.interactivity();
        interactivity.capture_action(|_: &Copy, window: &mut Window, cx: &mut App| {
            guarded_copy(window, cx);
            cx.stop_propagation();
        });
        interactivity.capture_action(|_: &Cut, window: &mut Window, cx: &mut App| {
            guarded_cut(window, cx);
            cx.stop_propagation();
        });
        interactivity.capture_action(|_: &Paste, window: &mut Window, cx: &mut App| {
            guarded_paste(window, cx);
            cx.stop_propagation();
        });
    }
}

/// Route gpui-kit's clipboard actions through the installed services in every
/// window opened afterwards (`app::init` / `app::init_for_test` call it when
/// the services record). Idempotent.
pub fn install_clipboard_guard(cx: &mut App) {
    Root::register_plugin::<ClipboardGuard>(cx, |_, _| ClipboardGuard);
}

/// The focused input's selection (empty for masked inputs), else the
/// window's text selection.
fn guarded_selection(window: &mut Window, cx: &mut App) -> String {
    if let Some(input) = window.focused_input(cx) {
        if let Some(state) = input.as_input() {
            // `is_copyable`: a non-empty selection of an input that is not masked.
            let state = state.read(cx);
            return if state.is_copyable() {
                state.selected_text().to_string()
            } else {
                String::new()
            };
        }
        if let Some(state) = input.as_textarea() {
            return state.read(cx).selected_text().to_string();
        }
        return String::new();
    }
    TextSelection::selected_text(window, cx).trim().to_string()
}

fn guarded_copy(window: &mut Window, cx: &mut App) {
    let text = guarded_selection(window, cx);
    if !text.is_empty() {
        services(cx).clipboard_write(text, cx);
    }
}

fn guarded_cut(window: &mut Window, cx: &mut App) {
    let Some(input) = window.focused_input(cx) else {
        return;
    };
    let text = guarded_selection(window, cx);
    if text.is_empty() {
        return;
    }
    if let Some(state) = input.as_input() {
        if !state.read(cx).is_editable() {
            return;
        }
        services(cx).clipboard_write(text, cx);
        state.update(cx, |state, cx| state.replace("", window, cx));
    } else if let Some(state) = input.as_textarea() {
        if !state.read(cx).is_editable() {
            return;
        }
        services(cx).clipboard_write(text, cx);
        state.update(cx, |state, cx| state.replace("", window, cx));
    }
}

fn guarded_paste(window: &mut Window, cx: &mut App) {
    let timer = crate::globals::BusyTimer::start();
    guarded_paste_inner(window, cx);
    crate::globals::record_sub_span(cx, "paste.guard", timer.stop());
}

fn guarded_paste_inner(window: &mut Window, cx: &mut App) {
    let Some(input) = window.focused_input(cx) else {
        return;
    };
    let Some(text) = services(cx)
        .clipboard_read(cx)
        .filter(|text| !text.is_empty())
    else {
        return;
    };
    if let Some(state) = input.as_input() {
        if state.read(cx).is_editable() {
            // Like gpui-kit's own paste into a single-line input.
            let line = text.replace('\n', "");
            let timer = crate::globals::BusyTimer::start();
            state.update(cx, |state, cx| state.replace(line, window, cx));
            crate::globals::record_sub_span(cx, "paste.replace", timer.stop());
        }
    } else if let Some(state) = input.as_textarea()
        && state.read(cx).is_editable()
    {
        state.update(cx, |state, cx| state.replace(text, window, cx));
    }
}

// ---------------------------------------------------------------------------
// RealServices
// ---------------------------------------------------------------------------

/// The real implementation (WP7): gpui/OS side effects, core's checked
/// terminal launchers, the Unix in-place exec and the login-shell PATH probe.
#[derive(Default)]
pub struct RealServices {
    _private: (),
}

impl RealServices {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// Test builds never reach the network or the desktop through `RealServices`.
const REFUSED_IN_TESTS: &str = "RealServices is refused in test builds";

/// The variable that lets a debug build start a real `claude`/`pi`/`omp`.
pub const ALLOW_REAL_LAUNCH_VAR: &str = "CHG_ALLOW_REAL_LAUNCH";

/// Programs a debug build never starts for real without an override (§15.4).
const GUARDED_PROGRAMS: [&str; 3] = ["claude", "pi", "omp"];

/// The debug-build launch guard (§15.4, X-037) on the text of a launch
/// (a terminal command line, a launch script, an exec line): a word whose file
/// stem is `claude`, `pi` or `omp` and that is not an existing directory is a
/// guarded program. It is allowed only when `allow_real` is set
/// (`CHG_ALLOW_REAL_LAUNCH=1`) or the word is a path inside the sandbox home
/// (a `[gui] <prog>_path` override pointing at a harmless recorder).
pub fn launch_guard(
    command_text: &str,
    allow_real: bool,
    sandbox_home: Option<&Path>,
) -> Result<(), String> {
    if allow_real {
        return Ok(());
    }
    for word in command_text.split_whitespace() {
        let word = word.trim_matches(|c| matches!(c, '"' | '\'' | '`' | ';' | '(' | ')'));
        if word.is_empty() {
            continue;
        }
        let path = Path::new(word);
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let stem = stem.to_ascii_lowercase();
        if !GUARDED_PROGRAMS.contains(&stem.as_str()) || path.is_dir() {
            continue;
        }
        let inside_sandbox =
            sandbox_home.is_some_and(|home| path.is_absolute() && path_starts_with(path, home));
        if !inside_sandbox {
            return Err(format!(
                "Refusing to start `{stem}` in a debug build: set [gui] {stem}_path to a harmless \
                 program inside the sandbox, or set {ALLOW_REAL_LAUNCH_VAR}=1"
            ));
        }
    }
    Ok(())
}

/// `path` is `base` or inside it (separator- and, on Windows, case-insensitive).
fn path_starts_with(path: &Path, base: &Path) -> bool {
    let norm = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/').to_string();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    let (path, base) = (norm(path), norm(base));
    path == base || path.starts_with(&format!("{base}/"))
}

/// The sandbox home of this process (debug builds): the home directory when
/// it carries the `.qa-sandbox` marker.
fn sandbox_home() -> Option<PathBuf> {
    home::home_dir().filter(|home| home.join(crate::qa::gate::SANDBOX_MARKER).is_file())
}

/// Check a launch against the debug-build guard (release builds never refuse).
fn guard_launch(command_text: &str) -> Result<(), String> {
    if cfg!(test) {
        return Err(REFUSED_IN_TESTS.to_string());
    }
    // The QA build (`--release --features qa`) is guarded like a debug build.
    if !(cfg!(debug_assertions) || cfg!(feature = "qa")) {
        return Ok(());
    }
    let allow = std::env::var(ALLOW_REAL_LAUNCH_VAR).as_deref() == Ok("1");
    launch_guard(command_text, allow, sandbox_home().as_deref())
}

/// The words of a terminal command a guard looks at: its command line and
/// the launch script it writes.
fn terminal_command_text(command: &TerminalCommand) -> String {
    let mut text = command.display();
    if let Some(script) = &command.script {
        text.push(' ');
        text.push_str(&script.contents);
    }
    text
}

impl PlatformServices for RealServices {
    fn reveal_path(&self, path: &Path, cx: &mut App) {
        if cfg!(test) {
            return;
        }
        cx.reveal_path(path);
    }

    fn open_url(&self, url: &str, cx: &mut App) {
        if cfg!(test) {
            return;
        }
        cx.open_url(url);
    }

    fn open_with_system(&self, path: &Path, cx: &mut App) {
        if cfg!(test) {
            return;
        }
        cx.open_with_system(path);
    }

    fn prompt_save_path(
        &self,
        dir: &Path,
        suggested_name: &str,
        window: &mut Window,
        cx: &mut App,
        done: SaveDone,
    ) {
        let receiver = cx.prompt_for_new_path(dir, Some(suggested_name));
        window
            .spawn(cx, async move |cx| {
                let answer = match receiver.await {
                    Ok(Ok(path)) => Ok(path),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_canceled) => Ok(None),
                };
                cx.update(|window, cx| done(answer, window, cx)).ok();
            })
            .detach();
    }

    fn prompt_open_paths(
        &self,
        options: OpenPathsOptions,
        window: &mut Window,
        cx: &mut App,
        done: OpenDone,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: options.files,
            directories: options.directories,
            multiple: options.multiple,
            prompt: Some(options.prompt.clone()),
        });
        window
            .spawn(cx, async move |cx| {
                let answer = match receiver.await {
                    Ok(Ok(paths)) => Ok(paths),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_canceled) => Ok(None),
                };
                cx.update(|window, cx| done(answer, window, cx)).ok();
            })
            .detach();
    }

    fn clipboard_write(&self, text: String, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn clipboard_read(&self, cx: &mut App) -> Option<String> {
        cx.read_from_clipboard().and_then(|item| item.text())
    }

    fn spawn_terminal(
        &self,
        command: &TerminalCommand,
        wait: Duration,
    ) -> Result<Option<i32>, String> {
        use claude_history_core::resume::terminal::SpawnOutcome;
        guard_launch(&terminal_command_text(command))?;
        match claude_history_core::resume::terminal::spawn_terminal_checked(command, wait) {
            Ok(SpawnOutcome::Running) => Ok(None),
            Ok(SpawnOutcome::Exited(code)) => Ok(Some(code)),
            Err(error) => Err(error.to_string()),
        }
    }

    #[cfg(unix)]
    fn exec_in_current_terminal(&self, spec: &LaunchSpec) -> Result<(), String> {
        use std::os::unix::process::CommandExt as _;
        let line = std::iter::once(spec.program.path.to_string_lossy().into_owned())
            .chain(
                spec.args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .collect::<Vec<_>>()
            .join(" ");
        guard_launch(&line)?;
        let mut command = std::process::Command::new(&spec.program.path);
        command.args(&spec.args);
        if let Some(cwd) = spec.cwd.as_ref().filter(|cwd| cwd.is_dir()) {
            command.current_dir(cwd);
        }
        // `exec` replaces the process image (chdir + execvp); it returns only
        // when that failed.
        Err(command.exec().to_string())
    }

    #[cfg(not(unix))]
    fn exec_in_current_terminal(&self, _spec: &LaunchSpec) -> Result<(), String> {
        Err("Resuming in the launching terminal is only available on macOS and Linux".to_string())
    }

    fn stdout_kind(&self) -> StdoutKind {
        crate::ops::console::stdout_kind()
    }

    fn stdin_is_tty(&self) -> bool {
        crate::ops::console::stdin_is_tty()
    }

    fn write_stdout(&self, text: &str) -> Result<(), String> {
        crate::ops::console::write_stdout(text)
    }

    fn write_stderr(&self, text: &str) {
        crate::ops::console::write_stderr(text);
    }

    fn fetch_latest_release(&self, current_version: &str) -> Result<UpdateStatus, String> {
        if cfg!(test) {
            return Err(REFUSED_IN_TESTS.to_string());
        }
        claude_history_core::update::check_latest_release(current_version)
            .map_err(|error| error.to_string())
    }

    fn login_shell_path(&self) -> Option<OsString> {
        if cfg!(test) {
            return None;
        }
        probe_login_shell_path(LOGIN_SHELL_TIMEOUT)
    }
}

// ---------------------------------------------------------------------------
// Login-shell PATH (§14.2)
// ---------------------------------------------------------------------------

/// How long the probe may take.
pub const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(3);
/// The marker before the PATH in the probe's output.
pub const PATH_START_MARKER: &str = "__CHG_PATH__";
/// The marker after the PATH.
pub const PATH_END_MARKER: &str = "__CHG_END__";

/// The shell script the probe runs.
pub fn login_shell_script() -> String {
    format!("printf \"{PATH_START_MARKER}%s{PATH_END_MARKER}\" \"$PATH\"")
}

/// The text between the markers; shell output outside the markers (rc-file
/// banners, prompts) is ignored. `None` without both markers or when empty.
pub fn parse_login_shell_output(output: &str) -> Option<OsString> {
    let start = output.rfind(PATH_START_MARKER)? + PATH_START_MARKER.len();
    let rest = &output[start..];
    let end = rest.find(PATH_END_MARKER)?;
    let path = rest[..end].trim();
    (!path.is_empty()).then(|| OsString::from(path))
}

/// Run `$SHELL -ilc '<script>'` (interactive + login, so `.zshrc`/`.bashrc`
/// entries such as nvm, volta and bun are included) with `timeout`; the child
/// is killed when it overruns. `None` on any failure.
#[cfg(unix)]
fn probe_login_shell_path(timeout: Duration) -> Option<OsString> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| {
            OsString::from(if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/bash"
            })
        });
    let mut child = Command::new(&shell)
        .arg("-ilc")
        .arg(login_shell_script())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("login-shell-read".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        })
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                // Our own child: stop it, the output so far is unusable.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    // A background process that inherited the pipe keeps it open after the
    // shell exits: bound the wait for EOF by what is left of the deadline.
    let remaining = deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(Duration::from_millis(200));
    let bytes = receiver.recv_timeout(remaining).ok()?;
    parse_login_shell_output(&String::from_utf8_lossy(&bytes))
}

#[cfg(not(unix))]
fn probe_login_shell_path(_timeout: Duration) -> Option<OsString> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// X-037: the debug-build guard refuses a resolved `claude` without an
    /// override and allows it with `CHG_ALLOW_REAL_LAUNCH=1` (injected).
    #[test]
    fn guard_refuses_a_real_claude_unless_allowed() {
        // The program path uses the platform's own separators.
        let program = std::env::temp_dir().join("npm").join("claude.cmd");
        let line = format!(
            "wt.exe -w new -d proj cmd.exe /S /K \"{} --resume abc\"",
            program.display()
        );
        let line = line.as_str();
        let refused = launch_guard(line, false, None).unwrap_err();
        assert!(refused.contains("Refusing to start `claude`"), "{refused}");
        assert!(refused.contains("CHG_ALLOW_REAL_LAUNCH=1"), "{refused}");
        assert_eq!(launch_guard(line, true, None), Ok(()));
    }

    #[test]
    fn guard_covers_pi_and_omp_and_bare_names() {
        for program in ["pi", "omp", "/usr/local/bin/pi", "omp.exe"] {
            let line = format!("xterm -e {program} --session x");
            assert!(launch_guard(&line, false, None).is_err(), "{line}");
        }
        assert!(launch_guard("xterm -e claude", false, None).is_err());
    }

    #[test]
    fn guard_allows_an_override_inside_the_sandbox_home() {
        let home = std::env::temp_dir().join("chg-sbx-guard-test");
        let recorder = home.join("bin").join("claude.cmd");
        let line = format!("wt.exe cmd.exe /K \"{}\" --resume x", recorder.display());
        assert_eq!(launch_guard(&line, false, Some(&home)), Ok(()));
        let outside = std::env::temp_dir().join("elsewhere").join("claude.cmd");
        let line = format!("wt.exe cmd.exe /K \"{}\"", outside.display());
        assert!(launch_guard(&line, false, Some(&home)).is_err());
    }

    #[test]
    fn guard_ignores_other_programs_and_directories() {
        assert_eq!(
            launch_guard("wt.exe -d C:\\proj cmd.exe /K echo hi", false, None),
            Ok(())
        );
        assert_eq!(
            launch_guard("xterm -e bash -c 'echo pigeon'", false, None),
            Ok(())
        );
        // A directory called `pi` is a working directory, not a program.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pi");
        std::fs::create_dir_all(&dir).unwrap();
        let line = format!("wt.exe -d {} cmd.exe", dir.display());
        assert_eq!(launch_guard(&line, false, None), Ok(()));
    }

    /// Test builds refuse every real launch, whatever the guard says.
    #[test]
    fn real_services_refuse_launches_in_test_builds() {
        let command = TerminalCommand {
            launcher: "xterm".to_string(),
            program: OsString::from("xterm"),
            args: claude_history_core::resume::terminal::TerminalArgs::Argv(vec![]),
            cwd: None,
            script: None,
            hide_console: false,
        };
        let result = RealServices::new().spawn_terminal(&command, Duration::from_millis(10));
        assert_eq!(result, Err(REFUSED_IN_TESTS.to_string()));
    }

    #[test]
    fn login_shell_output_is_parsed_between_the_markers() {
        let output = "Last login: Mon\nwelcome to zsh\n__CHG_PATH__/opt/homebrew/bin:/usr/bin__CHG_END__\nbye\n";
        assert_eq!(
            parse_login_shell_output(output),
            Some(OsString::from("/opt/homebrew/bin:/usr/bin"))
        );
        // The last start marker wins (a banner that echoes the script).
        let echoed = "__CHG_PATH__%s__CHG_END__ __CHG_PATH__/a:/b__CHG_END__";
        assert_eq!(
            parse_login_shell_output(echoed),
            Some(OsString::from("/a:/b"))
        );
        assert_eq!(parse_login_shell_output("no markers"), None);
        assert_eq!(parse_login_shell_output("__CHG_PATH__/a"), None);
        assert_eq!(parse_login_shell_output("__CHG_PATH____CHG_END__"), None);
    }

    #[test]
    fn login_shell_script_prints_the_marked_path() {
        let script = login_shell_script();
        assert!(script.contains("__CHG_PATH__%s__CHG_END__"), "{script}");
        assert!(script.contains("\"$PATH\""), "{script}");
    }
}
