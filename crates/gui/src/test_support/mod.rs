//! The hermetic gpui test harness (DESIGN §15.2). Frozen: written by WP0,
//! afterwards changed only by the integrator. Per-WP fixture builders live in
//! `test_support/fixtures_<wp>.rs` (owned by that WP: `fixtures_list` WP1,
//! `fixtures_viewer` WP2, `fixtures_ops` WP3, `fixtures_agent` WP4,
//! `fixtures_tools` WP5); this file only declares them.
//!
//! Isolation layers:
//! * a `#[ctor]` runs before any test thread exists and points `HOME`,
//!   `USERPROFILE`, `CLAUDE_CONFIG_DIR`, the Pi/OMP and XDG variables into a
//!   per-process temp root, and removes `OMP_PROFILE`, `PI_PROFILE`,
//!   `TERMINAL`, `CLAUDE_HISTORY_CLIPBOARD` — even a stray
//!   process-environment read in GUI code cannot reach the user's data. The
//!   root (`%TEMP%/claude-history-gui-tests/pid-<pid>`) is removed when the
//!   test process exits normally; roots of processes that no longer run are
//!   swept at the next start;
//! * every test world is a `HistoryEnv::sandbox(tempdir)`;
//! * `app::init_for_test` installs `RecordingServices` (no desktop side
//!   effects; gpui-kit's own Copy/Cut/Paste go through it too), disables
//!   state.json writes and installs a refusing embedder;
//! * core's `hermetic` feature (K13, enabled by this crate's
//!   `[dev-dependencies]`) makes core's own guards refuse in GUI test builds
//!   too (`skeleton_tests::hermetic_guards_active`).
//!
//! Test modules import names explicitly (never `use gpui_kit::*`, which
//! shadows `#[test]`). Doc tests are off for this crate (`doctest = false`):
//! they would run outside the redirect.

pub mod fixtures_agent;
pub mod fixtures_list;
pub mod fixtures_ops;
pub mod fixtures_tools;
pub mod fixtures_viewer;

mod self_tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use claude_history_core::HistoryEnv;
use claude_history_core::settings::{LaunchCommand, parse_launch_args};
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, Global, TestAppContext};

use crate::app;
use crate::globals::AppState;
use crate::model::LaunchContext;
use crate::services::{RecordingServices, services};
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Process environment redirect
// ---------------------------------------------------------------------------

static REDIRECT_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Variables the redirect points into the temp root (relative to its `home`).
pub const REDIRECTED_VARS: [(&str, &str); 10] = [
    ("HOME", ""),
    ("USERPROFILE", ""),
    ("CLAUDE_CONFIG_DIR", ".claude"),
    ("PI_CODING_AGENT_DIR", ".pi/agent"),
    ("PI_CODING_AGENT_SESSION_DIR", ".pi/sessions"),
    ("PI_CONFIG_DIR", ".omp"),
    ("XDG_DATA_HOME", ".local/share"),
    ("XDG_CONFIG_HOME", ".config"),
    ("XDG_CACHE_HOME", ".cache"),
    ("XDG_STATE_HOME", ".local/state"),
];

/// Variables the redirect removes.
pub const REMOVED_VARS: [&str; 6] = [
    "OMP_PROFILE",
    "PI_PROFILE",
    "TERMINAL",
    "CLAUDE_HISTORY_CLIPBOARD",
    "CHG_ALLOW_REAL_LAUNCH",
    "CHG_ALLOW_REAL_HOME",
];

/// The parent of every test process's redirect root.
fn redirect_parent() -> PathBuf {
    std::env::temp_dir().join("claude-history-gui-tests")
}

/// Runs before `main` of the test binary, while it is single-threaded, so
/// changing the process environment is sound.
#[ctor::ctor(unsafe)]
fn redirect_process_env() {
    let parent = redirect_parent();
    sweep_stale_roots(&parent);
    let root = parent.join(format!("pid-{}", std::process::id()));
    let home = root.join("home");
    let _ = std::fs::create_dir_all(&home);
    for (name, rel) in REDIRECTED_VARS {
        let value = if rel.is_empty() {
            home.clone()
        } else {
            home.join(rel)
        };
        // SAFETY: ctor functions run before the test harness starts threads.
        unsafe { std::env::set_var(name, value) };
    }
    for name in REMOVED_VARS {
        // SAFETY: as above.
        unsafe { std::env::remove_var(name) };
    }
    let _ = REDIRECT_ROOT.set(root);
    // SAFETY: `remove_redirect_root` is a plain `extern "C" fn()` that only
    // removes this process's own temp root.
    unsafe {
        atexit(remove_redirect_root);
    }
}

unsafe extern "C" {
    /// The C runtime's `atexit` (ucrt on Windows, libc on Unix): runs when
    /// the test binary returns from `main`.
    fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
}

/// Remove this process's redirect root at exit (best effort).
extern "C" fn remove_redirect_root() {
    if let Some(root) = REDIRECT_ROOT.get() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Remove the `pid-<n>` roots of test processes that no longer run (a
/// process that exited abnormally skipped its `atexit` cleanup). Only
/// `pid-<number>` directories under [`redirect_parent`] are touched; a root
/// whose process may still run is kept.
fn sweep_stale_roots(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let own = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix("pid-"))
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own || process_alive(pid) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether a process with this id may still run (errors count as running).
#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    /// `GetExitCodeProcess` of a running process.
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok == 0 || code == STILL_ACTIVE
    }
}

/// Whether a process with this id may still run (errors count as running).
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // An id that does not fit `pid_t` belongs to no process (Linux caps ids at
    // 2^22); `kill` with its wrapped negative value would signal a process group.
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(windows, unix)))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// The per-process temp root every redirected variable points into.
pub fn redirect_root() -> &'static Path {
    REDIRECT_ROOT
        .get()
        .expect("the test environment redirect ran before main")
}

/// The redirected home directory (`<root>/home`).
pub fn redirect_home() -> PathBuf {
    redirect_root().join("home")
}

/// Tests that touch process-wide state (`core::theme`, `core::debug` sink)
/// hold this lock.
pub fn global_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Marker: `gpui_kit::init` ran in this test app.
pub struct KitInitialized;

impl Global for KitInitialized {}

/// What a test app was initialised with (one world and one command line per
/// app; `ensure_app` refuses a different one instead of ignoring it).
#[derive(Clone)]
struct TestLaunch {
    home: PathBuf,
    args: Vec<String>,
}

impl Global for TestLaunch {}

// ---------------------------------------------------------------------------
// Test worlds
// ---------------------------------------------------------------------------

/// A sandbox home with Claude/Pi/OMP data, config and caches in a temp dir.
pub struct TestWorld {
    pub tmp: tempfile::TempDir,
    pub env: HistoryEnv,
}

impl Default for TestWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl TestWorld {
    pub fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("chg-world-")
            .tempdir()
            .expect("create a temp dir");
        let env = HistoryEnv::sandbox(tmp.path());
        std::fs::create_dir_all(tmp.path().join(".claude").join("projects"))
            .expect("create the projects dir");
        Self { tmp, env }
    }

    /// The sandbox home.
    pub fn home(&self) -> &Path {
        self.tmp.path()
    }

    /// `<home>/.claude/projects`
    pub fn projects_root(&self) -> PathBuf {
        self.home().join(".claude").join("projects")
    }

    /// Write a Claude session `<projects>/<project_dir>/<uuid>.jsonl`.
    pub fn add_claude_session(&self, project_dir: &str, uuid: &str, lines: &[&str]) -> PathBuf {
        let dir = self.projects_root().join(project_dir);
        std::fs::create_dir_all(&dir).expect("create the project dir");
        let path = dir.join(format!("{uuid}.jsonl"));
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        std::fs::write(&path, text).expect("write the session");
        path
    }

    /// Write `bytes` to `<home>/<rel>` (e.g. a fixture from
    /// `crates/core/tests/fixtures` or `crates/gui/tests/fixtures` read with
    /// `include_bytes!`).
    pub fn add_fixture(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.home().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the fixture dir");
        }
        std::fs::write(&path, bytes).expect("write the fixture");
        path
    }

    /// Write `config.toml` and/or `gui.toml` in the sandbox config dir.
    pub fn write_config(&self, config_toml: Option<&str>, gui_toml: Option<&str>) {
        if let (Some(text), Some(path)) = (config_toml, self.env.config_file()) {
            write_file(&path, text);
        }
        if let (Some(text), Some(path)) = (gui_toml, self.env.gui_config_file()) {
            write_file(&path, text);
        }
    }

    /// The standard corpus: 6 Claude sessions in 3 projects (one of them a
    /// worktree of another), 1 Pi session, 1 OMP session, 1 empty Claude
    /// transcript and 1 Claude transcript with a malformed line. Returns every
    /// written transcript, Claude first.
    pub fn standard_corpus(&self) -> Vec<PathBuf> {
        let base = synth::cwd_base();
        let projects = [
            ("proj", base.join("proj")),
            ("api", base.join("api")),
            (
                "proj-worktree",
                base.join("proj").join(".worktrees").join("feature"),
            ),
        ];
        let titles = [
            "Fix login bug",
            "Deploy pipeline",
            "Token refresh retry",
            "Add API pagination",
            "Rate limit errors",
            "Worktree cleanup",
        ];
        let mut paths = Vec::new();
        for (ix, title) in titles.iter().enumerate() {
            let (_, cwd) = &projects[ix / 2];
            let project_dir = synth::encoded_project_dir(cwd);
            let uuid = synth::uuid(ix as u32 + 1);
            let lines = synth::claude_conversation(&uuid, cwd, title, ix as u32, 2);
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            paths.push(self.add_claude_session(&project_dir, &uuid, &refs));
        }
        // Empty transcript (no user messages).
        let empty_uuid = synth::uuid(90);
        let proj_dir = synth::encoded_project_dir(&projects[0].1);
        paths.push(self.add_claude_session(&proj_dir, &empty_uuid, &[]));
        // A transcript with one malformed line.
        let broken_uuid = synth::uuid(91);
        let mut lines =
            synth::claude_conversation(&broken_uuid, &projects[1].1, "Malformed line", 9, 1);
        lines.insert(1, "{this is not json".to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let api_dir = synth::encoded_project_dir(&projects[1].1);
        paths.push(self.add_claude_session(&api_dir, &broken_uuid, &refs));
        // Pi and OMP sessions.
        paths.push(self.add_fixture(
            ".pi/agent/sessions/--tmp-pi-project--/2024-03-01T00-00-00-000Z_01912345-6789-7abc-8def-0123456789ab.jsonl",
            synth::PI_SESSION.as_bytes(),
        ));
        paths.push(self.add_fixture(
            ".omp/agent/sessions/--tmp-omp-project--/2026-01-02T03-04-05-000Z_omp_session_custom_id.jsonl",
            synth::OMP_SESSION.as_bytes(),
        ));
        paths
    }
}

fn write_file(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the config dir");
    }
    std::fs::write(path, text).expect("write the config file");
}

// ---------------------------------------------------------------------------
// Opening windows
// ---------------------------------------------------------------------------

/// The launch context for `args` (as typed on the command line), with the
/// world's home as the launch directory.
pub fn launch_for_args(world: &TestWorld, args: &[&str]) -> LaunchContext {
    let command = parse_launch_args(args.iter().copied()).expect("valid test arguments");
    let LaunchCommand::Run(args) = command else {
        panic!("test arguments must start the GUI, got {command:?}");
    };
    let time_filter = args.time_filter().expect("valid test time filter");
    crate::workspace::startup::to_launch_context(
        args,
        Some(world.home().to_path_buf()),
        time_filter,
        None,
    )
}

/// Initialise the test app once with `world` and `args`. A later call with
/// another world, or with non-empty `args` that differ from the first ones,
/// panics: the app state (workspace, flags, settings) cannot change after
/// init, so open the window that needs the arguments first.
fn ensure_app(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) {
    let current = cx.read(|cx| {
        cx.try_global::<AppState>()
            .map(|_| cx.try_global::<TestLaunch>().cloned())
    });
    if let Some(current) = current {
        if let Some(current) = current {
            assert_eq!(
                current.home,
                world.home(),
                "this test app was initialised with another TestWorld; use one world per app"
            );
            let requested: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            assert!(
                requested.is_empty() || requested == current.args,
                "this test app was initialised with arguments {:?}; cannot reopen it with {:?} \
                 (open the window that needs the arguments first)",
                current.args,
                requested
            );
        }
        return;
    }
    let launch = launch_for_args(world, args);
    let recording = Arc::new(RecordingServices::new());
    let test_launch = TestLaunch {
        home: world.home().to_path_buf(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    };
    cx.update(|cx| {
        app::init_for_test(cx, world, launch, recording);
        cx.set_global(test_launch);
    });
    // The list's loader and search workers are real core threads that wake the
    // poll task; the test scheduler refuses foreign-thread wake-ups unless
    // parking is allowed (tests wait in real time).
    cx.executor().allow_parking();
}

/// Open the production main window on `world` with command-line `args`.
/// The window is activated like the real app's window (the GPUI test
/// platform starts windows inactive, and an inactive window reports no
/// focus-in events, so `ActivePane` would never change).
pub fn open_main(
    cx: &mut TestAppContext,
    world: &TestWorld,
    args: &[&str],
) -> (AnyWindowHandle, Entity<Workspace>) {
    ensure_app(cx, world, args);
    let (handle, workspace) = cx.update(crate::workspace::open_main_window_with_view);
    activate(cx, handle);
    (handle, workspace)
}

/// Activate `handle` (as the OS does for the window the user works in) and
/// draw it, so focus events reach the panes.
pub fn activate(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, _| window.activate_window())
        .expect("the window is open");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        gpui_kit::test::TestWindowExt::render_frame(window, cx)
    })
    .expect("the window is open");
    cx.run_until_parked();
}

/// Open a single-file window for `path` on `world`.
pub fn open_single_file(
    cx: &mut TestAppContext,
    world: &TestWorld,
    path: &Path,
) -> AnyWindowHandle {
    open_single_file_with_args(cx, world, path, &[])
}

/// [`open_single_file`] in an app started with command-line `args` (when it
/// is the first window of the test app).
pub fn open_single_file_with_args(
    cx: &mut TestAppContext,
    world: &TestWorld,
    path: &Path,
    args: &[&str],
) -> AnyWindowHandle {
    ensure_app(cx, world, args);
    let path = path.to_path_buf();
    let handle = cx.update(move |cx| crate::single_file::open_window(path, cx));
    activate(cx, handle);
    handle
}

/// Wait (real time, ≤ 30 s) until the list finished loading and its loader
/// stopped (no later poll changes the selection behind the test's back).
pub fn wait_loaded(cx: &mut TestAppContext, ws: &Entity<Workspace>) {
    cx.executor().allow_parking();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        let loading = cx.read(|cx| {
            let list = ws.read(cx).list().read(cx);
            list.is_loading() || list.loader().is_running()
        });
        if !loading {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the conversation list did not finish loading within 30 s"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The test's `RecordingServices` (answer queues, calls, clipboard).
pub fn recording(cx: &TestAppContext) -> Arc<RecordingServices> {
    cx.read(|cx| {
        services(cx)
            .recording
            .expect("tests run with RecordingServices")
    })
}

/// The platform label of a keystroke as the keymap and the hints show it
/// (`ctrl-r` → `Ctrl+R`, on macOS `⌃R`; `secondary-q` → `Ctrl+Q` / `⌘Q`).
pub fn kbd(keystroke: &str) -> String {
    let keystroke = gpui_kit::Keystroke::parse(keystroke).expect("a valid keystroke");
    gpui_kit::component::kbd::Kbd::format(&keystroke)
}

// ---------------------------------------------------------------------------
// Synthetic data
// ---------------------------------------------------------------------------

/// Synthetic transcripts and corpora (deterministic).
pub mod synth {
    use std::path::{Path, PathBuf};

    /// A Pi v3 session (one user question, one answer).
    pub const PI_SESSION: &str = concat!(
        r#"{"type":"session","version":3,"id":"01912345-6789-7abc-8def-0123456789ab","timestamp":"2024-03-01T00:00:00.000Z","cwd":"/tmp/pi-project"}"#,
        "\n",
        r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2024-03-01T00:01:00.000Z","message":{"role":"user","content":"Pi question about caching","timestamp":1709251260000}}"#,
        "\n",
        r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2024-03-01T00:02:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Pi answer about caching"}],"provider":"anthropic","model":"claude-opus","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1709251320000}}"#,
        "\n",
    );

    /// An OMP v3 session with a title.
    pub const OMP_SESSION: &str = concat!(
        r#"{"type":"title","v":1,"title":"OMP fixture title","source":"user","updatedAt":"2026-01-02T03:04:05.000Z","pad":""}"#,
        "\n",
        r#"{"type":"session","version":3,"id":"omp_session_custom_id","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/tmp/omp-project"}"#,
        "\n",
        r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-01-02T03:04:06.000Z","message":{"role":"user","content":[{"type":"text","text":"OMP root question"}],"timestamp":1767323046000}}"#,
        "\n",
        r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-01-02T03:04:07.000Z","message":{"role":"assistant","content":[{"type":"text","text":"OMP root answer"}],"provider":"anthropic","model":"claude-opus-4-5","usage":{"input":10,"output":5,"cacheRead":2,"cacheWrite":1},"timestamp":1767323047000}}"#,
        "\n",
    );

    /// A deterministic session UUID.
    pub fn uuid(n: u32) -> String {
        format!("{n:08x}-1c2d-4e5f-8a9b-0c1d2e3f4a5b")
    }

    /// An absolute base directory for synthetic project cwds.
    pub fn cwd_base() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\synthetic")
        } else {
            PathBuf::from("/synthetic")
        }
    }

    /// Claude Code's project directory name for `cwd`.
    pub fn encoded_project_dir(cwd: &Path) -> String {
        claude_history_core::env::workspace_project_dir_name(cwd)
    }

    fn json_string(text: &str) -> String {
        serde_json::to_string(text).expect("strings serialize")
    }

    /// A Claude conversation: `exchanges` user/assistant pairs plus a
    /// summary line; `day` spreads timestamps (newer for larger values).
    pub fn claude_conversation(
        uuid: &str,
        cwd: &Path,
        title: &str,
        day: u32,
        exchanges: u32,
    ) -> Vec<String> {
        let cwd = json_string(&cwd.display().to_string());
        let mut lines = vec![format!(
            r#"{{"type":"summary","summary":{},"leafUuid":"leaf-{uuid}"}}"#,
            json_string(title)
        )];
        for turn in 0..exchanges {
            let minute = turn * 2;
            let stamp = |m: u32| format!("2026-09-{:02}T10:{:02}:00Z", 1 + day % 28, m);
            lines.push(format!(
                r#"{{"type":"user","sessionId":"{uuid}","cwd":{cwd},"timestamp":"{}","message":{{"role":"user","content":{}}}}}"#,
                stamp(minute),
                json_string(&format!("{title}: question {}", turn + 1))
            ));
            lines.push(format!(
                r#"{{"type":"assistant","sessionId":"{uuid}","cwd":{cwd},"timestamp":"{}","message":{{"role":"assistant","model":"claude-opus-4-5","content":[{{"type":"text","text":{}}}]}}}}"#,
                stamp(minute + 1),
                json_string(&format!("{title}: answer {}", turn + 1))
            ));
        }
        lines
    }

    /// `n` Claude sessions spread over 10 projects under `world`. Their
    /// modification times are distinct, a minute apart, and follow the write
    /// order (the last written is the newest): core's conversation timestamp is
    /// the modification time, files written in one coarse clock tick would tie,
    /// and tied conversations load in an order that differs between two loads
    /// (seen on Linux; WP-L, WP-I).
    pub fn corpus(world: &super::TestWorld, n: usize) {
        let base = cwd_base();
        let now = chrono::Utc::now();
        for ix in 0..n {
            let cwd = base.join(format!("project-{:02}", ix % 10));
            let uuid = uuid(1_000 + ix as u32);
            let lines = claude_conversation(
                &uuid,
                &cwd,
                &format!("Synthetic conversation {ix}"),
                ix as u32,
                1,
            );
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let path = world.add_claude_session(&encoded_project_dir(&cwd), &uuid, &refs);
            let age = chrono::Duration::minutes((n - ix) as i64);
            super::fixtures_list::set_mtime(&path, now - age);
        }
    }

    /// A large Claude transcript at `path`: `messages` messages (user and
    /// assistant alternating) and `big_outputs` Bash tool calls whose results
    /// have `lines_per_output` lines each.
    pub fn transcript(path: &Path, messages: usize, big_outputs: usize, lines_per_output: usize) {
        use std::io::Write as _;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the transcript dir");
        }
        let file = std::fs::File::create(path).expect("create the transcript");
        let mut out = std::io::BufWriter::new(file);
        let uuid = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "synthetic".to_string());
        // An even stride keeps every big output on an assistant (odd) index:
        // indices 1, 1 + every, 1 + 2·every, … (as many as fit).
        let every = messages
            .checked_div(big_outputs)
            .map_or(usize::MAX, |every| every.max(2).next_multiple_of(2));
        let mut outputs = 0;
        for ix in 0..messages {
            let second = ix % 60;
            let minute = (ix / 60) % 60;
            let hour = (ix / 3600) % 24;
            let stamp = format!("2026-09-01T{hour:02}:{minute:02}:{second:02}Z");
            let line = if ix % 2 == 0 {
                format!(
                    r#"{{"type":"user","sessionId":"{uuid}","timestamp":"{stamp}","message":{{"role":"user","content":{}}}}}"#,
                    json_string(&format!("Question {ix}: how does part {ix} work?"))
                )
            } else if outputs < big_outputs && ix % every == 1 {
                outputs += 1;
                let tool_id = format!("toolu_{ix}");
                let call = format!(
                    r#"{{"type":"assistant","sessionId":"{uuid}","timestamp":"{stamp}","message":{{"role":"assistant","content":[{{"type":"text","text":"Running a command."}},{{"type":"tool_use","id":"{tool_id}","name":"Bash","input":{{"command":"seq {lines_per_output}"}}}}]}}}}"#
                );
                let body: String = (1..=lines_per_output)
                    .map(|n| format!("output line {n}\n"))
                    .collect();
                let result = format!(
                    r#"{{"type":"user","sessionId":"{uuid}","timestamp":"{stamp}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{tool_id}","content":{}}}]}}}}"#,
                    json_string(&body)
                );
                format!("{call}\n{result}")
            } else {
                format!(
                    r#"{{"type":"assistant","sessionId":"{uuid}","timestamp":"{stamp}","message":{{"role":"assistant","content":[{{"type":"text","text":{}}}]}}}}"#,
                    json_string(&format!(
                        "Answer {ix}: part {ix} works like this.\n\n```rust\nfn part_{ix}() {{}}\n```"
                    ))
                )
            };
            writeln!(out, "{line}").expect("write the transcript");
        }
        out.flush().expect("flush the transcript");
    }
}

// ---------------------------------------------------------------------------
// Failure fixtures (§15.10)
// ---------------------------------------------------------------------------

/// Failure fixtures per OS. On Unix, tests that rely on permissions skip
/// with a printed reason when running as root ([`fault::running_as_root`]).
pub mod fault {
    use std::path::{Path, PathBuf};

    /// Undoes a fault when dropped.
    pub struct FaultGuard {
        kind: FaultKind,
    }

    enum FaultKind {
        /// Windows: an open handle without sharing keeps the file undeletable.
        #[cfg(windows)]
        OpenHandle(#[allow(dead_code)] std::fs::File),
        /// Restore these permissions on drop.
        Permissions(PathBuf, std::fs::Permissions),
    }

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            match &self.kind {
                #[cfg(windows)]
                FaultKind::OpenHandle(_) => {}
                FaultKind::Permissions(path, permissions) => {
                    let _ = std::fs::set_permissions(path, permissions.clone());
                }
            }
        }
    }

    /// Make `path` impossible to delete while the guard lives.
    /// Windows: the file is opened with `share_mode(0)` (a sharing violation
    /// on delete). Unix: the parent directory becomes `0o555`.
    pub fn undeletable(path: &Path) -> FaultGuard {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
                .expect("open the file without sharing");
            FaultGuard {
                kind: FaultKind::OpenHandle(file),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let parent = path.parent().expect("the file has a parent").to_path_buf();
            let previous = std::fs::metadata(&parent)
                .expect("stat the parent")
                .permissions();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555))
                .expect("make the parent read-only");
            FaultGuard {
                kind: FaultKind::Permissions(parent, previous),
            }
        }
    }

    /// Make `path` impossible to write while the guard lives.
    /// Windows: the read-only attribute. Unix: `0o444`.
    pub fn unwritable(path: &Path) -> FaultGuard {
        let previous = std::fs::metadata(path)
            .expect("stat the file")
            .permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
                .expect("make the file read-only");
        }
        #[cfg(not(unix))]
        {
            let mut readonly = previous.clone();
            readonly.set_readonly(true);
            std::fs::set_permissions(path, readonly).expect("make the file read-only");
        }
        FaultGuard {
            kind: FaultKind::Permissions(path.to_path_buf(), previous),
        }
    }

    /// Unix tests that rely on permissions skip as root.
    pub fn running_as_root() -> bool {
        #[cfg(unix)]
        {
            // SAFETY: geteuid has no preconditions.
            unsafe { libc::geteuid() == 0 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}
