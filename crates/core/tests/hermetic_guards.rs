//! K13: the `hermetic` feature turns on the guard rails of the unit tests
//! for code built WITHOUT `cfg(test)`, as the GUI's tests use the core
//! library. Integration tests link the library like the GUI does, so these
//! only pass when the feature is on:
//!
//! ```text
//! cargo test -p claude-history-core --features hermetic
//! ```

#![cfg(feature = "hermetic")]

use claude_history_core::HistoryEnv;
use claude_history_core::resume::terminal::{
    LaunchSpec, Platform, TerminalLauncher, build_terminal_command, spawn_terminal,
    spawn_terminal_checked,
};
use std::time::Duration;

#[test]
#[should_panic(expected = "tests must use HistoryEnv::sandbox")]
fn the_process_environment_is_never_read() {
    let _ = HistoryEnv::from_process_env();
}

#[test]
#[should_panic(expected = "tests must not use the network")]
fn the_update_check_never_reaches_the_network() {
    let _ = claude_history_core::update::check_latest_release("0.0.0");
}

#[test]
fn the_default_embedders_refuse_to_load_the_model() {
    let root = tempfile::tempdir().unwrap();
    let env = HistoryEnv::sandbox(root.path());
    let mut factory = claude_history_core::semantic_ops::fastembed_factory(&env);
    let error = match factory() {
        Ok(_) => panic!("the real model must not load in a hermetic build"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("tests must inject"), "{error}");
    assert!(!env.semantic_model_dir().unwrap().exists());
}

#[test]
fn the_gui_embedder_loader_refuses_to_load_the_model() {
    // DESIGN §7.6: the GUI's shared embedder calls `new_in` directly, not
    // through a default factory; a GUI test that reaches it (for example a
    // missed fake) must not download either.
    use claude_history_core::semantic::fastembed::{FastembedEmbedder, HERMETIC_REFUSAL};
    let root = tempfile::tempdir().unwrap();
    let env = HistoryEnv::sandbox(root.path());
    let model_dir = env.semantic_model_dir().unwrap();
    let error = match FastembedEmbedder::new_in(model_dir.clone(), false) {
        Ok(_) => panic!("the real model must not load in a hermetic build"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(HERMETIC_REFUSAL), "{error}");
    assert!(!model_dir.exists(), "nothing created");
    assert!(!env.semantic_cache_dir().unwrap().exists());
}

#[test]
fn terminals_are_never_spawned() {
    let root = tempfile::tempdir().unwrap();
    let spec = LaunchSpec {
        program: claude_history_core::program::ResolvedProgram {
            path: root.path().join("claude"),
            kind: claude_history_core::program::ProgramKind::Executable,
        },
        args: Vec::new(),
        cwd: Some(root.path().to_path_buf()),
    };
    let launcher = if cfg!(windows) {
        TerminalLauncher::WindowsConsole
    } else {
        TerminalLauncher::Template("true {args}".to_string())
    };
    let options = claude_history_core::resume::terminal::BuildOptions {
        platform: Platform::current(),
        keep_open: false,
        script_path: None,
    };
    let command = build_terminal_command(&launcher, &spec, &options).unwrap();
    assert_eq!(
        spawn_terminal(&command).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(
        spawn_terminal_checked(&command, Duration::from_millis(10))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
}
