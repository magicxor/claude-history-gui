//! Terminal launcher smoke test for CI (K17). NOT run by `cargo test`; it
//! opens a real terminal window.
//!
//! ```text
//! cargo run -p claude-history-core --example terminal_smoke -- <marker> [launcher] [timeout-secs]
//! ```
//!
//! For each launcher candidate ([`launcher_candidates`]: the `launcher`
//! argument, a `gui.toml`-style name or template, else every detected
//! launcher) it opens a terminal that creates the file `<marker>` (Unix:
//! `touch <marker>`, Windows: `cmd /C copy NUL <marker>`) and waits for the
//! file to appear. Exits 0 with the first launcher that worked, 1 when none
//! did, 2 on bad arguments. The marker must not exist yet, and its
//! directory must.

use claude_history_core::HistoryEnv;
use claude_history_core::program::{ProgramKind, ResolvedProgram, windows_cmd_exe};
use claude_history_core::resume::terminal::{
    BuildOptions, LaunchOptions, LaunchSpec, SpawnOutcome, build_terminal_command,
    launcher_candidates, spawn_terminal_checked,
};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(marker) = args.next().map(PathBuf::from) else {
        eprintln!("usage: terminal_smoke <marker> [launcher] [timeout-secs]");
        return ExitCode::from(2);
    };
    let launcher = args
        .next()
        .map(|value| value.to_string_lossy().into_owned());
    let timeout = args
        .next()
        .and_then(|value| value.to_string_lossy().parse::<u64>().ok())
        .unwrap_or(30);
    if marker.exists() {
        eprintln!("marker {} already exists", marker.display());
        return ExitCode::from(2);
    }
    let marker = match std::path::absolute(&marker) {
        Ok(marker) => marker,
        Err(error) => {
            eprintln!("bad marker path {}: {error}", marker.display());
            return ExitCode::from(2);
        }
    };

    let env = HistoryEnv::from_process_env();
    let spec = match touch_spec(&env, &marker) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let options = LaunchOptions {
        terminal: launcher,
        terminal_var: std::env::var_os("TERMINAL"),
        keep_open: false,
        ..LaunchOptions::default()
    };

    let candidates = launcher_candidates(&env, &options);
    if candidates.is_empty() {
        eprintln!("no terminal launcher found");
        return ExitCode::from(1);
    }
    for (attempt, candidate) in candidates.into_iter().enumerate() {
        let build = BuildOptions {
            platform: options.platform,
            keep_open: options.keep_open,
            // Terminal.app's launch script (it removes itself).
            script_path: Some(std::env::temp_dir().join(format!(
                "terminal-smoke-{}-{attempt}.command",
                std::process::id()
            ))),
        };
        let command = match build_terminal_command(&candidate, &spec, &build) {
            Ok(command) => command,
            Err(error) => {
                eprintln!("{}: cannot build the command: {error}", candidate.name());
                continue;
            }
        };
        println!("{}: {}", command.launcher, command.display());
        match spawn_terminal_checked(&command, Duration::from_secs(3)) {
            Ok(SpawnOutcome::Exited(code)) if code != 0 => {
                eprintln!("{}: exited with code {code}", command.launcher);
                continue;
            }
            Ok(outcome) => println!("{}: {outcome:?}", command.launcher),
            Err(error) => {
                eprintln!("{}: spawn failed: {error}", command.launcher);
                continue;
            }
        }
        let deadline = Instant::now() + Duration::from_secs(timeout);
        while Instant::now() < deadline {
            if marker.exists() {
                println!("{}: marker created", command.launcher);
                return ExitCode::SUCCESS;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("{}: no marker after {timeout} s", command.launcher);
    }
    ExitCode::from(1)
}

/// What the terminal runs: create the empty file `marker`.
fn touch_spec(env: &HistoryEnv, marker: &std::path::Path) -> Result<LaunchSpec, String> {
    let cwd = marker.parent().map(PathBuf::from);
    if cfg!(windows) {
        // `copy NUL <file>` creates an empty file without a redirection
        // (which the launcher would escape).
        Ok(LaunchSpec {
            program: ResolvedProgram {
                path: windows_cmd_exe(),
                kind: ProgramKind::Executable,
            },
            args: ["/C", "copy", "NUL"]
                .into_iter()
                .map(OsString::from)
                .chain([marker.as_os_str().to_os_string()])
                .collect(),
            cwd,
        })
    } else {
        let program = env
            .resolve_program("touch", None)
            .map_err(|error| error.to_string())?;
        Ok(LaunchSpec {
            program,
            args: vec![marker.as_os_str().to_os_string()],
            cwd,
        })
    }
}
