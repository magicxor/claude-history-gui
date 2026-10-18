//! P tests of the terminal launcher fallback (A-015, PD4): a spawn error tries
//! the next candidate, an early non-zero exit is final, every error is kept.
//! The services are a script of spawn results; nothing is spawned.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claude_history_core::HistoryEnv;
use claude_history_core::resume::terminal::{LaunchOptions, LaunchSpec, Platform, TerminalCommand};
use claude_history_core::resume::{ResumeKind, ResumePlan, ResumeStrategy};
use claude_history_core::update::UpdateStatus;
use gpui_kit::{App, Window};

use crate::model::{OpenPathsOptions, StdoutKind};
use crate::ops::resume::{LaunchOutcome, spawn_through_candidates};
use crate::services::{OpenDone, PlatformServices, SaveDone, Services};

/// Answers `spawn_terminal` from a queue (the last answer repeats) and records
/// the launcher of every command; everything else is unreachable here.
struct ScriptedServices {
    answers: Mutex<VecDeque<Result<Option<i32>, String>>>,
    launchers: Mutex<Vec<String>>,
    commands: Mutex<Vec<TerminalCommand>>,
}

impl PlatformServices for ScriptedServices {
    fn reveal_path(&self, _: &Path, _: &mut App) {
        unreachable!()
    }
    fn open_url(&self, _: &str, _: &mut App) {
        unreachable!()
    }
    fn open_with_system(&self, _: &Path, _: &mut App) {
        unreachable!()
    }
    fn prompt_save_path(&self, _: &Path, _: &str, _: &mut Window, _: &mut App, _: SaveDone) {
        unreachable!()
    }
    fn prompt_open_paths(&self, _: OpenPathsOptions, _: &mut Window, _: &mut App, _: OpenDone) {
        unreachable!()
    }
    fn clipboard_write(&self, _: String, _: &mut App) {
        unreachable!()
    }
    fn clipboard_read(&self, _: &mut App) -> Option<String> {
        unreachable!()
    }
    fn spawn_terminal(
        &self,
        command: &TerminalCommand,
        _: Duration,
    ) -> Result<Option<i32>, String> {
        self.commands.lock().unwrap().push(command.clone());
        self.launchers
            .lock()
            .unwrap()
            .push(command.launcher.clone());
        let mut answers = self.answers.lock().unwrap();
        if answers.len() > 1 {
            answers.pop_front().unwrap()
        } else {
            answers.front().cloned().unwrap_or(Ok(None))
        }
    }
    fn exec_in_current_terminal(&self, _: &LaunchSpec) -> Result<(), String> {
        unreachable!()
    }
    fn stdout_kind(&self) -> StdoutKind {
        unreachable!()
    }
    fn stdin_is_tty(&self) -> bool {
        unreachable!()
    }
    fn write_stdout(&self, _: &str) -> Result<(), String> {
        unreachable!()
    }
    fn write_stderr(&self, _: &str) {
        unreachable!()
    }
    fn fetch_latest_release(&self, _: &str) -> Result<UpdateStatus, String> {
        unreachable!()
    }
    fn login_shell_path(&self) -> Option<OsString> {
        unreachable!()
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    env: HistoryEnv,
    spec: LaunchSpec,
}

/// An environment where at least two launchers are detected on every OS, and a
/// launch spec for a fake program.
fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let make = |name: &str| {
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    };
    // Windows: wt.exe + the console; macOS: Terminal + osascript; Linux: two emulators.
    make("wt.exe");
    make("kitty");
    make("xterm");
    let program = make(if cfg!(windows) { "fake.exe" } else { "fake" });
    let mut env = HistoryEnv::sandbox(dir.path());
    env.path_var = Some(bin.clone().into_os_string());
    let plan = ResumePlan {
        source: claude_history_core::history::Source::Claude,
        kind: ResumeKind::Resume,
        strategy: ResumeStrategy::InProject,
        session_path: PathBuf::from("session.jsonl"),
        program: "claude".to_string(),
        program_override: Some(program),
        args: vec![OsString::from("--resume"), OsString::from("abc")],
        cwd: Some(dir.path().to_path_buf()),
        copy: None,
        warnings: Vec::new(),
    };
    let spec = plan.launch_spec(&env).unwrap();
    Setup {
        _dir: dir,
        env,
        spec,
    }
}

fn run(answers: Vec<Result<Option<i32>, String>>) -> (LaunchOutcome, Vec<String>) {
    let setup = setup();
    let scripted = Arc::new(ScriptedServices {
        answers: Mutex::new(answers.into()),
        launchers: Mutex::new(Vec::new()),
        commands: Mutex::new(Vec::new()),
    });
    let services = Services {
        inner: scripted.clone(),
        recording: None,
    };
    let options = LaunchOptions {
        platform: Platform::current(),
        ..LaunchOptions::default()
    };
    let outcome = spawn_through_candidates(&setup.env, setup.spec.clone(), &options, &services);
    let launchers = scripted.launchers.lock().unwrap().clone();
    (outcome, launchers)
}

#[test]
fn a_spawn_error_tries_the_next_candidate() {
    let (outcome, launchers) = run(vec![Err("first is broken".to_string()), Ok(None)]);
    assert!(launchers.len() >= 2, "{launchers:?}");
    match outcome {
        LaunchOutcome::Started { launcher } => assert_eq!(launcher, launchers[1]),
        other => panic!("{other:?}"),
    }
    assert_ne!(
        launchers[0], launchers[1],
        "two different launchers were tried"
    );
}

#[test]
fn an_early_nonzero_exit_is_final() {
    let (outcome, launchers) = run(vec![Ok(Some(3)), Ok(None)]);
    assert_eq!(launchers.len(), 1, "no retry after a launcher that ran");
    match outcome {
        LaunchOutcome::Exited { launcher, code } => {
            assert_eq!(launcher, launchers[0]);
            assert_eq!(code, 3);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_early_zero_exit_counts_as_started() {
    let (outcome, launchers) = run(vec![Ok(Some(0))]);
    assert_eq!(launchers.len(), 1);
    assert!(
        matches!(outcome, LaunchOutcome::Started { .. }),
        "{outcome:?}"
    );
}

#[test]
fn every_candidate_failing_reports_the_last_error() {
    let (outcome, launchers) = run(vec![Err("a".to_string()), Err("last".to_string())]);
    assert!(launchers.len() >= 2);
    match outcome {
        LaunchOutcome::Failed(message) => {
            assert_eq!(message, "Failed to launch terminal: last");
        }
        other => panic!("{other:?}"),
    }
}

/// Run one launch with `options` and `spec`, returning the recorded commands.
fn commands_of(setup: &Setup, spec: LaunchSpec, options: &LaunchOptions) -> Vec<TerminalCommand> {
    let scripted = Arc::new(ScriptedServices {
        answers: Mutex::new(vec![Ok(None)].into()),
        launchers: Mutex::new(Vec::new()),
        commands: Mutex::new(Vec::new()),
    });
    let services = Services {
        inner: scripted.clone(),
        recording: None,
    };
    spawn_through_candidates(&setup.env, spec, options, &services);
    scripted.commands.lock().unwrap().clone()
}

/// K17 / A-015: a spec without a directory starts in the home directory (the
/// default comes from core's per-launcher preparation, not the call site).
#[test]
fn a_spec_without_a_cwd_starts_in_the_home_directory() {
    let setup = setup();
    let mut spec = setup.spec.clone();
    spec.cwd = None;
    let options = LaunchOptions {
        platform: Platform::Linux,
        terminal: Some("xterm -e".to_string()),
        ..LaunchOptions::default()
    };
    let commands = commands_of(&setup, spec, &options);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].cwd.as_deref(), Some(setup._dir.path()));
    assert!(commands[0].script.is_none());
}

/// K17 / A-015: a macOS Terminal candidate arrives with a fresh launch script
/// under the cache's `launch` directory, a new one for every launch.
#[test]
fn a_mac_terminal_candidate_gets_a_fresh_script() {
    let setup = setup();
    let options = LaunchOptions {
        platform: Platform::MacOs,
        ..LaunchOptions::default()
    };
    let first = commands_of(&setup, setup.spec.clone(), &options);
    let second = commands_of(&setup, setup.spec.clone(), &options);
    let script = |commands: &[TerminalCommand]| {
        commands[0]
            .script
            .clone()
            .expect("the Terminal.app launcher needs a script")
    };
    let (a, b) = (script(&first), script(&second));
    assert_ne!(a.path, b.path);
    assert!(
        a.path
            .starts_with(setup.env.cache_root().unwrap().join("launch"))
    );
}
