//! The `claude-history-gui` binary (DESIGN §11.1, §11.23, §14.3). WP0 → WP7.
//!
//! Thin: read the process state once (arguments, launch cwd, environment),
//! let `startup::dispatch` decide (help/version/usage errors, the headless
//! `agent`/`--show-dir`/`--render` paths, or a GUI launch), run the sandbox
//! gate (debug and QA builds) before any data is read, then hand over to
//! `claude_history_gui::run`. This is the only place that reads the process
//! environment (`HistoryEnv::from_process_env`, `current_dir`).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::Path;

use claude_history_core::HistoryEnv;
use claude_history_core::settings::parse_launch_args;
use claude_history_gui::exit::exit_code;
use claude_history_gui::qa::gate;
use claude_history_gui::services::RealServices;
use claude_history_gui::workspace::startup::{self, Flow, ProcessInfo};

fn main() {
    let launch_cwd = std::env::current_dir().ok();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // `--qa-*` flags exist only in `--features qa` builds; core never sees them.
    #[cfg(feature = "qa")]
    let (args, qa) = match startup::extract_qa_args(args) {
        Ok(split) => split,
        Err(message) => {
            use claude_history_gui::services::PlatformServices as _;
            RealServices::new().write_stderr(&format!(
                "error: {message}\n\nUsage: {} [OPTIONS] [FILE]\n\nFor more information, try '--help'.\n",
                startup::PROGRAM
            ));
            std::process::exit(claude_history_gui::exit::EXIT_USAGE);
        }
    };
    #[cfg(not(feature = "qa"))]
    let qa = None;

    // Release verify steps: load the bundled ONNX Runtime and exit, without a
    // window or any data access (S-032). Never set by users.
    if std::env::var_os("CLAUDE_HISTORY_GUI_ORT_SELFTEST").is_some() {
        use claude_history_gui::services::PlatformServices as _;
        let output = RealServices::new();
        match claude_history_core::semantic::fastembed::verify_onnx_runtime() {
            Ok(()) => {
                let _ = output.write_stdout("onnxruntime ok\n");
                std::process::exit(0);
            }
            Err(error) => {
                output.write_stderr(&format!("error: {error}\n"));
                std::process::exit(1);
            }
        }
    }

    // `CLAUDE_HISTORY_CLIPBOARD` is ignored (PD10); an Info line says so.
    startup::note_clipboard_env(std::env::var("CLAUDE_HISTORY_CLIPBOARD").ok().as_deref());

    let parsed = parse_launch_args(args.clone());
    let process = ProcessInfo {
        launch_cwd: launch_cwd.clone(),
        qa,
    };
    let output = RealServices::new();
    let mut prepare = || {
        // The sandbox gate (§15.4; debug and QA builds): refuse before any
        // data is read.
        gate::run()?;
        Ok(process_env(launch_cwd.as_deref()))
    };
    match startup::dispatch(parsed, &args, &process, &output, &mut prepare) {
        Flow::Exit(code) => std::process::exit(code),
        Flow::Gui(mut launch, env) => {
            // `$TERMINAL` (Linux launcher detection, §11.10 step 6), read once here.
            launch.terminal_var = std::env::var_os("TERMINAL");
            claude_history_gui::run(*launch, *env);
            std::process::exit(exit_code());
        }
    }
}

/// The process environment, with the launch directory as the base of
/// relative Pi/OMP environment values (K16).
fn process_env(launch_cwd: Option<&Path>) -> HistoryEnv {
    let mut env = HistoryEnv::from_process_env();
    env.env_path_base = launch_cwd.map(Path::to_path_buf);
    env
}
