//! WP7 startup tests: launch mapping (P), the PD3 workspace table (P), the
//! PD17 initial-view table (P), the headless paths with recorded stdout (G),
//! config-error dialogs (G), the debug sink (G), the login-shell probe (G) and
//! the startup tools (G). FEATURES rows: L-082, S-034, V-049, C-001…C-012,
//! C-017…C-020, C-023, C-047, C-051, C-061, C-062, C-066, X-032.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use claude_history_core::WorkspaceScope;
use claude_history_core::agent::AgentCli;
use claude_history_core::browser::ListSearchMode;
use claude_history_core::config::GuiSettings;
use claude_history_core::debug::{self, DebugLevel};
use claude_history_core::export::{ExportFormat, ExportOptions, generate_export};
use claude_history_core::settings::{
    GuiLaunchArgs, LaunchCommand, PickerAction, help_text, parse_launch_args,
};
use claude_history_core::theme::ThemePreference;
use claude_history_core::time_filter::TimeFilter;
use claude_history_core::view::ToolDisplayMode;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{AppContext as _, TestAppContext};

use super::*;
use crate::exit;
use crate::globals::{DebugBufferHandle, app_state, last_toast};
use crate::model::{SemanticReportMode, StdoutKind};
use crate::persist::{RememberedSearchMode, RememberedToolDisplay, RememberedView, UiState};
use crate::services::{RecordingServices, ServiceCall};
use crate::test_support::{TestWorld, global_lock, open_main, recording};

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn parse_run(args: &[&str]) -> GuiLaunchArgs {
    match parse_launch_args(os(args)).expect("valid arguments") {
        LaunchCommand::Run(args) => args,
        other => panic!("not a run command: {other:?}"),
    }
}

fn launch_for(args: &[&str], cwd: Option<&Path>) -> LaunchContext {
    to_launch_context(
        parse_run(args),
        cwd.map(Path::to_path_buf),
        TimeFilter::default(),
        None,
    )
}

/// What `dispatch` did, with the recorded output.
struct Run {
    flow: Flow,
    rec: Arc<RecordingServices>,
}

impl Run {
    fn launch(&self) -> &LaunchContext {
        match &self.flow {
            Flow::Gui(launch, _) => launch,
            other => panic!("expected a GUI launch, got {other:?}"),
        }
    }

    fn code(&self) -> i32 {
        match &self.flow {
            Flow::Exit(code) => *code,
            other => panic!("expected an exit, got {other:?}"),
        }
    }

    fn is_gui(&self) -> bool {
        matches!(self.flow, Flow::Gui(..))
    }
}

fn run(world: &TestWorld, args: &[&str], kind: StdoutKind, cwd: Option<&Path>) -> Run {
    let raw = os(args);
    let rec = Arc::new(RecordingServices::new());
    rec.set_stdout_kind(kind);
    let process = ProcessInfo {
        launch_cwd: cwd.map(Path::to_path_buf),
        qa: None,
    };
    let env = world.env.clone();
    let flow = dispatch(
        parse_launch_args(raw.clone()),
        &raw,
        &process,
        &*rec,
        &mut || Ok(env.clone()),
    );
    Run { flow, rec }
}

fn gui(args: &[&str]) -> Run {
    let world = TestWorld::new();
    let result = run(&world, args, StdoutKind::None, Some(world.home()));
    // Keep the world alive until the flow was produced; the launch holds no paths into it.
    drop(world);
    result
}

fn error_text(args: &[&str]) -> (i32, String) {
    let world = TestWorld::new();
    let result = run(&world, args, StdoutKind::None, Some(world.home()));
    (result.code(), result.rec.stderr())
}

// ---------------------------------------------------------------------------
// Launch mapping (C-001 … C-020, C-062)
// ---------------------------------------------------------------------------

/// C-001: a positional FILE is a single-file launch; it conflicts with `--local`.
#[test]
fn file_argument_maps_to_a_single_file_launch() {
    let world = TestWorld::new();
    let result = run(&world, &["a.jsonl"], StdoutKind::None, Some(world.home()));
    assert_eq!(
        result.launch().file.as_deref(),
        Some(world.home().join("a.jsonl").as_path()),
        "a relative file is resolved against the launch directory"
    );
    let (code, stderr) = error_text(&["a.jsonl", "--local"]);
    assert_eq!(code, 2);
    assert!(
        stderr.starts_with("error: the argument '[FILE]' cannot be used with '--local'"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Usage: claude-history-gui [OPTIONS] [FILE]"),
        "{stderr}"
    );
}

/// C-002 / C-006 / C-007: display flags and their conflicts.
#[test]
fn display_flags_map_and_conflict() {
    let launch = gui(&["--first"]);
    let config = claude_history_core::config::ConfigFile::default();
    assert!(!launch.launch().args.settings(&config).show_last);
    assert!(gui(&[]).launch().args.settings(&config).show_last);
    for pair in [
        ["--first", "--last"],
        ["--show-tools", "--no-tools"],
        ["--show-thinking", "--hide-thinking"],
        ["--semantic", "--lexical"],
    ] {
        let (code, stderr) = error_text(&pair);
        assert_eq!(code, 2, "{pair:?}");
        assert!(stderr.contains("cannot be used with"), "{pair:?}: {stderr}");
    }
    let tools = gui(&["-t"]).launch().args.settings(&config);
    assert_eq!(
        ToolDisplayMode::from(tools.tool_display),
        ToolDisplayMode::Full
    );
    assert!(
        gui(&["--show-thinking"])
            .launch()
            .args
            .settings(&config)
            .show_thinking
    );
}

/// C-004 / C-010: accepted flags that change nothing.
#[test]
fn deprecated_and_no_effect_flags_change_nothing() {
    let baseline = gui(&[]).launch().clone();
    for flag in ["-g", "--global", "--pager", "--no-pager", "--no-color"] {
        let launch = gui(&[flag]).launch().clone();
        assert_eq!(launch.file, baseline.file, "{flag}");
        assert_eq!(launch.picker, baseline.picker, "{flag}");
        assert_eq!(launch.startup_tool, baseline.startup_tool, "{flag}");
        assert_eq!(
            launch.time_filter.after, baseline.time_filter.after,
            "{flag}"
        );
        assert_eq!(launch.args.workspace, baseline.args.workspace, "{flag}");
    }
    let (code, stderr) = error_text(&["--global", "--delete", "x"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
}

/// C-005: time-filter parse errors exit 2, an inverted range exits 1.
#[test]
fn time_filter_errors_and_exit_codes() {
    let (code, stderr) = error_text(&["--since", "2x"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("invalid value '2x' for '--since <WHEN>': '2x': unknown unit 'x' ("),
        "{stderr}"
    );
    let (code, stderr) = error_text(&["--since", "1d", "--after", "2d"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    let (code, stderr) = error_text(&["--since", "1d", "--before", "3d"]);
    assert_eq!(code, 1, "an inverted range is a runtime error");
    assert!(
        stderr.starts_with("Error: Invalid time range: lower bound "),
        "{stderr}"
    );
    assert!(stderr.contains(" is later than upper bound "), "{stderr}");
    assert!(!gui(&["--since", "1d"]).launch().time_filter.after.is_none());
}

/// C-008: picker flags.
#[test]
fn picker_flags_map_to_picker_actions() {
    assert_eq!(gui(&["-i"]).launch().picker, Some(PickerAction::ShowId));
    assert_eq!(gui(&["-p"]).launch().picker, Some(PickerAction::ShowPath));
    assert_eq!(
        gui(&["--resume", "--fork-session"]).launch().picker,
        Some(PickerAction::Resume { fork: true })
    );
    assert!(gui(&[]).launch().picker.is_none());
    let (code, stderr) = error_text(&["--fork-session"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains(
            "the following required arguments were not provided: --resume (required by --fork-session)"
        ),
        "{stderr}"
    );
    let (code, stderr) = error_text(&["--plain", "--delete", "x"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
}

/// C-009: `--debug [LEVEL]`, and a following file is not swallowed.
#[test]
fn debug_flag_levels() {
    assert_eq!(
        gui(&["--debug", "WARNING"]).launch().args.debug,
        Some(DebugLevel::Warn)
    );
    assert_eq!(
        gui(&["--debug"]).launch().args.debug,
        Some(DebugLevel::Debug)
    );
    // A bad level is an error with core's text (the `=` form names it explicitly).
    let (code, stderr) = error_text(&["--debug=loud"]);
    assert_eq!(code, 2);
    assert!(
        stderr.starts_with(
            "error: invalid value 'loud' for '--debug [<LEVEL>]': invalid log level 'loud', expected: debug, info, warn, error"
        ),
        "{stderr}"
    );
    // A following word that is no level is the FILE, not an error.
    let word = gui(&["--debug", "x"]);
    assert_eq!(word.launch().args.debug, Some(DebugLevel::Debug));
    assert!(
        word.launch()
            .file
            .as_ref()
            .is_some_and(|file| file.ends_with("x"))
    );
    let launch = gui(&["--debug", "a.jsonl"]);
    assert_eq!(launch.launch().args.debug, Some(DebugLevel::Debug));
    assert!(
        launch
            .launch()
            .file
            .as_ref()
            .is_some_and(|file| file.ends_with("a.jsonl"))
    );
}

/// C-062: `--semantic` / `--lexical`.
#[test]
fn search_mode_flags_conflict() {
    assert!(gui(&["--semantic"]).launch().args.semantic);
    assert!(gui(&["--lexical"]).launch().args.lexical);
    assert_eq!(error_text(&["--semantic", "--lexical"]).0, 2);
}

/// C-017 / X-032: help and version go through the console, exit 0.
#[test]
fn help_and_version_print_and_exit_zero() {
    let world = TestWorld::new();
    let version = run(&world, &["--version"], StdoutKind::Redirected, None);
    assert_eq!(version.code(), 0);
    assert_eq!(version.rec.stdout(), "claude-history-gui 0.1.0\n");
    assert_eq!(version_text(), "claude-history-gui 0.1.0\n");
    let help = run(&world, &["--help"], StdoutKind::Redirected, None);
    assert_eq!(help.code(), 0);
    assert_eq!(help.rec.stdout(), help_text("claude-history-gui"));
    assert!(
        help.rec
            .stdout()
            .contains("Usage: claude-history-gui [OPTIONS] [FILE]")
    );
    let sub = run(&world, &["update", "--help"], StdoutKind::Redirected, None);
    assert_eq!(sub.code(), 0);
    assert!(
        sub.rec
            .stdout()
            .contains("Usage: claude-history-gui update")
    );
    let help_sub = run(&world, &["help", "update"], StdoutKind::Redirected, None);
    assert_eq!(help_sub.rec.stdout(), sub.rec.stdout());
    // Without any stdout (a desktop launch) nothing is printed, still exit 0.
    let silent = run(&world, &["--version"], StdoutKind::None, None);
    assert_eq!(silent.code(), 0);
    assert!(silent.rec.stdout().is_empty());
    assert!(silent.rec.calls().contains(&ServiceCall::StdoutFailed(
        "claude-history-gui 0.1.0\n".to_string()
    )));
}

/// C-018: subcommands.
#[test]
fn subcommands_map_to_startup_tools() {
    assert_eq!(
        gui(&["delete-empty", "--local", "--yes"])
            .launch()
            .startup_tool,
        Some(StartupTool::DeleteEmpty { local: true })
    );
    let delete_empty = gui(&["delete-empty", "--local"]);
    assert!(
        delete_empty.launch().args.local,
        "--local makes the launch directory the workspace (PD3 rule 2)"
    );
    assert_eq!(
        gui(&["delete-empty"]).launch().startup_tool,
        Some(StartupTool::DeleteEmpty { local: false })
    );
    assert_eq!(
        gui(&["update"]).launch().startup_tool,
        Some(StartupTool::CheckForUpdates)
    );
    let (code, stderr) = error_text(&["delete-empty", "--local", "--all"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    assert!(
        stderr.contains("Usage: claude-history-gui delete-empty"),
        "a subcommand error shows the subcommand's usage: {stderr}"
    );
}

/// C-013…C-015: the K14 one-shot flags open their Tools dialogs, `--local` preselects Workspace.
#[test]
fn one_shot_flags_map_to_startup_tools() {
    let tool = |args: &[&str]| gui(args).launch().startup_tool.clone();
    assert_eq!(
        tool(&["--delete", "abc"]),
        Some(StartupTool::DeleteSession("abc".into()))
    );
    assert_eq!(
        tool(&["--debug-search", "needle", "-L"]),
        Some(StartupTool::DebugSearch {
            query: "needle".into(),
            local: true
        })
    );
    assert_eq!(
        tool(&["--semantic-search", "needle", "--semantic-top", "5"]),
        Some(StartupTool::SemanticSearch {
            query: "needle".into(),
            top: 5,
            local: false
        })
    );
    assert_eq!(
        tool(&["--semantic-search", "needle"]),
        Some(StartupTool::SemanticSearch {
            query: "needle".into(),
            top: claude_history_core::settings::DEFAULT_SEMANTIC_TOP,
            local: false
        })
    );
    assert_eq!(
        tool(&["--debug-semantic-search", "needle"]),
        Some(StartupTool::SemanticDebug {
            query: "needle".into(),
            local: false
        })
    );
    assert_eq!(
        tool(&["--generate-semantic-cache", "-L"]),
        Some(StartupTool::GenerateSemanticCache { local: true })
    );
    assert_eq!(
        tool(&["--clear-semantic-cache"]),
        Some(StartupTool::ClearSemanticCache)
    );
    assert_eq!(tool(&[]), None);
}

/// The tool entry points are mapped one to one (G tests below run them).
#[test]
fn startup_tools_map_to_their_entry_points() {
    assert_eq!(
        tool_entry(&StartupTool::ShowDir),
        ToolEntry::ShowProjectsDirectory
    );
    assert_eq!(
        tool_entry(&StartupTool::DeleteSession("x".into())),
        ToolEntry::DeleteById {
            prefill: "x".into()
        }
    );
    assert_eq!(
        tool_entry(&StartupTool::DeleteEmpty { local: true }),
        ToolEntry::DeleteEmpty { local: true }
    );
    assert_eq!(
        tool_entry(&StartupTool::CheckForUpdates),
        ToolEntry::CheckForUpdates
    );
    assert_eq!(
        tool_entry(&StartupTool::ClearSemanticCache),
        ToolEntry::ClearSemanticCache
    );
}

/// C-019: `-w DIR` maps to the workspace argument; a missing directory is reported.
#[test]
fn workspace_argument_maps_and_missing_is_reported() {
    let world = TestWorld::new();
    let existing = world.home().join("work");
    std::fs::create_dir_all(&existing).unwrap();
    let existing_text = existing.display().to_string();
    let launch = launch_for(&["-w", &existing_text], Some(world.home()));
    assert_eq!(launch.args.workspace.as_deref(), Some(existing.as_path()));
    assert_eq!(workspace_not_found(&launch), None);
    let missing = world.home().join("nope");
    let missing_text = missing.display().to_string();
    let launch = launch_for(&["-w", &missing_text], Some(world.home()));
    assert_eq!(workspace_not_found(&launch), Some(missing));
    // A relative value is checked against the launch directory.
    let launch = launch_for(&["-w", "work"], Some(world.home()));
    assert_eq!(workspace_not_found(&launch), None);
}

/// C-020: the exit-code table.
#[test]
fn exit_code_table() {
    let world = TestWorld::new();
    let corpus = world.standard_corpus();
    // usage error → 2
    assert_eq!(error_text(&["--nope"]).0, 2);
    // inverted time range → 1
    assert_eq!(error_text(&["--since", "1d", "--before", "3d"]).0, 1);
    // --show-dir without a workspace → 1
    let code = run(
        &world,
        &["--show-dir"],
        StdoutKind::Redirected,
        Some(world.home()),
    )
    .code();
    assert_eq!(code, 1);
    // a successful headless print → 0
    let code = run(
        &world,
        &["--render", &corpus[0].display().to_string()],
        StdoutKind::Redirected,
        Some(world.home()),
    )
    .code();
    assert_eq!(code, 0);
    // the sandbox gate refusing → 3
    let raw = os(&["--semantic-search", "x"]);
    let rec = Arc::new(RecordingServices::new());
    let flow = dispatch(
        parse_launch_args(raw.clone()),
        &raw,
        &ProcessInfo::default(),
        &*rec,
        &mut || Err("Refusing to run: test".to_string()),
    );
    assert!(matches!(flow, Flow::Exit(3)));
    assert_eq!(rec.stderr(), "Refusing to run: test\n");
}

/// C-020: a cancelled picker exits 0 and prints nothing.
#[gpui_kit::test]
fn a_cancelled_picker_exits_zero_with_empty_stdout(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &["--show-path"]);
    cx.update_window(handle, |_, _, cx| {
        assert!(app_state(cx).is_picker());
        exit::quit(cx);
    })
    .unwrap();
    assert_eq!(cx.read(exit::quit_requested), Some(exit::EXIT_OK));
    assert_eq!(recording(cx).stdout(), "");
}

/// C-011: `--render FILE` prints the Ledger headless when stdout is redirected.
#[test]
fn render_prints_the_ledger_to_a_redirected_stdout() {
    let world = TestWorld::new();
    let file = world.standard_corpus()[0].clone();
    let file_text = file.display().to_string();
    let redirected = run(
        &world,
        &["--render", &file_text],
        StdoutKind::Redirected,
        Some(world.home()),
    );
    assert_eq!(redirected.code(), 0);
    let expected = generate_export(&file, ExportFormat::Ledger, ExportOptions::default()).unwrap();
    assert!(!expected.is_empty());
    assert_eq!(redirected.rec.stdout(), expected);
    assert!(
        !redirected.rec.stdout().contains('\u{1b}'),
        "the headless ledger has no ANSI sequences"
    );
    // With a terminal or no stdout the single-file window opens instead.
    for kind in [StdoutKind::Terminal, StdoutKind::None] {
        let result = run(&world, &["--render", &file_text], kind, Some(world.home()));
        assert!(result.is_gui(), "{kind:?}");
        assert_eq!(result.launch().file.as_deref(), Some(file.as_path()));
        assert!(result.rec.stdout().is_empty());
    }
    // The startup display options apply (--show-tools prints tool blocks).
    let with_tools = run(
        &world,
        &["--render", &file_text, "--show-tools"],
        StdoutKind::Redirected,
        Some(world.home()),
    );
    let expected = generate_export(
        &file,
        ExportFormat::Ledger,
        ExportOptions {
            show_tools: true,
            show_thinking: false,
        },
    )
    .unwrap();
    assert_eq!(with_tools.rec.stdout(), expected);
    // Errors go to stderr with exit 1.
    let missing = run(
        &world,
        &[
            "--render",
            &world.home().join("none.jsonl").display().to_string(),
        ],
        StdoutKind::Redirected,
        Some(world.home()),
    );
    assert_eq!(missing.code(), 1);
    assert!(missing.rec.stderr().starts_with("Error: File not found: "));
}

/// C-012: `--show-dir` prints the projects directory headless.
#[test]
fn show_dir_prints_the_projects_directory() {
    let world = TestWorld::new();
    let workspace = world.home().join("work").join("proj");
    std::fs::create_dir_all(&workspace).unwrap();
    let scope = WorkspaceScope::new(&workspace).unwrap();
    let expected = format!(
        "{}\n",
        world
            .env
            .claude_projects_root()
            .unwrap()
            .join(scope.project_dir_name())
            .display()
    );
    let result = run(
        &world,
        &["--show-dir", "-w", &workspace.display().to_string()],
        StdoutKind::Redirected,
        Some(world.home()),
    );
    assert_eq!(result.code(), 0);
    assert_eq!(result.rec.stdout(), expected);
    // `-L` uses the launch directory, whatever it is.
    let local = run(
        &world,
        &["-d", "-L"],
        StdoutKind::Terminal,
        Some(&workspace),
    );
    assert_eq!(local.code(), 0);
    assert_eq!(local.rec.stdout(), expected);
    // No workspace: stderr + exit 1.
    let none = run(
        &world,
        &["--show-dir"],
        StdoutKind::Redirected,
        Some(world.home()),
    );
    assert_eq!(none.code(), 1);
    assert_eq!(
        none.rec.stderr(),
        "Error: Choose a workspace folder to show its Claude projects directory\n"
    );
    assert!(none.rec.stdout().is_empty());
    // Without stdout the GUI opens and shows the toast (AD14).
    let gui_run = run(
        &world,
        &["--show-dir"],
        StdoutKind::None,
        Some(world.home()),
    );
    assert_eq!(gui_run.launch().startup_tool, Some(StartupTool::ShowDir));
}

/// C-061: headless `agent …`.
#[test]
fn agent_runs_headless_with_the_core_protocol_output() {
    let world = TestWorld::new();
    let cwd = world.home().join("agentproj");
    std::fs::create_dir_all(&cwd).unwrap();
    let scope = WorkspaceScope::new(&cwd).unwrap();
    let uuid = "3f2a19c0-1c2d-4e5f-8a9b-0c1d2e3f4a5b";
    let lines =
        crate::test_support::synth::claude_conversation(uuid, &cwd, "Alpha investigation", 1, 2);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.add_claude_session(scope.project_dir_name(), uuid, &refs);
    let result = run(
        &world,
        &["agent", "search", "alpha"],
        StdoutKind::Redirected,
        Some(&cwd),
    );
    assert_eq!(result.code(), 0);
    let command = parse_launch_args(os(&["agent", "search", "alpha"])).unwrap();
    let LaunchCommand::Agent(AgentCli::Request(request)) = command else {
        panic!("an agent request");
    };
    let (config, errors) = load_config(&world.env);
    assert!(errors.is_empty());
    let expected = claude_history_core::agent::execute(
        &world.env.clone().with_workspace(&cwd),
        &config.effective,
        request,
    );
    assert!(!expected.stdout.is_empty());
    assert_eq!(result.rec.stdout(), expected.stdout);
    assert_eq!(result.code(), expected.exit_code);

    // A usage error: stderr, exit 2, the subcommand's own usage line.
    let (code, stderr) = error_text(&["agent", "read"]);
    assert_eq!(code, 2);
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(
        stderr.contains("Usage: claude-history-gui agent read [OPTIONS] <REFS>..."),
        "{stderr}"
    );
    // Help: stdout, exit 0.
    let help = run(
        &world,
        &["agent", "--help"],
        StdoutKind::Redirected,
        Some(&cwd),
    );
    assert_eq!(help.code(), 0);
    assert!(
        help.rec
            .stdout()
            .contains("Usage: claude-history-gui agent <COMMAND>")
    );
    // A broken config file: `Error: <e>`, exit 1.
    world.write_config(Some("this is = not toml ["), None);
    let broken = run(
        &world,
        &["agent", "search", "alpha"],
        StdoutKind::Redirected,
        Some(&cwd),
    );
    assert_eq!(broken.code(), 1);
    assert!(
        broken.rec.stderr().starts_with("Error: "),
        "{}",
        broken.rec.stderr()
    );
}

#[test]
fn usage_lines_name_the_subcommand() {
    assert_eq!(
        usage_line(&os(&["--bogus"])),
        "Usage: claude-history-gui [OPTIONS] [FILE]"
    );
    assert_eq!(
        usage_line(&os(&["update", "--x"])),
        "Usage: claude-history-gui update"
    );
    assert_eq!(
        usage_line(&os(&["agent", "search"])),
        "Usage: claude-history-gui agent search [OPTIONS] <QUERY>"
    );
    assert_eq!(
        usage_line(&os(&["agent", "nonsense"])),
        "Usage: claude-history-gui agent <COMMAND>"
    );
}

#[test]
fn qa_arguments_are_split_off() {
    let (rest, qa) = extract_qa_args(os(&[
        "--lexical",
        "--qa-scenario",
        "smoke.qa",
        "--qa-out=out",
        "--qa-model-dir",
        "models",
        "--qa-real-console",
        "x.jsonl",
    ]))
    .unwrap();
    assert_eq!(rest, os(&["--lexical", "x.jsonl"]));
    assert_eq!(
        qa,
        Some(QaArgs {
            scenario: "smoke.qa".into(),
            out_dir: "out".into(),
            model_dir: Some("models".into()),
            real_console: true,
        })
    );
    let (rest, qa) = extract_qa_args(os(&["--lexical"])).unwrap();
    assert_eq!((rest, qa), (os(&["--lexical"]), None));
    assert!(
        extract_qa_args(os(&["--qa-scenario", "a.qa"]))
            .unwrap_err()
            .contains("--qa-out")
    );
    assert!(
        extract_qa_args(os(&["--qa-out", "o"]))
            .unwrap_err()
            .contains("--qa-scenario")
    );
    assert!(extract_qa_args(os(&["--qa-scenario"])).is_err());
}

// ---------------------------------------------------------------------------
// Workspace resolution (PD3, L-082)
// ---------------------------------------------------------------------------

struct Table {
    world: TestWorld,
}

impl Table {
    fn new() -> Self {
        Self {
            world: TestWorld::new(),
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.world.home().join("t").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn resolve(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
        gui: &GuiSettings,
        ui: &UiState,
        exe_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        let launch = launch_for(args, cwd);
        resolve_workspace_with(&launch, &self.world.env, gui, ui, exe_dir)
            .map(|scope| scope.dir().to_path_buf())
    }
}

fn canon(path: &Path) -> PathBuf {
    WorkspaceScope::new(path).unwrap().dir().to_path_buf()
}

/// L-082: `--workspace`, `-L`, the meaningful-cwd exclusions and rules.
#[test]
fn workspace_rules_one_to_three() {
    let t = Table::new();
    let (gui, ui) = (GuiSettings::default(), UiState::default());
    let plain = t.dir("plain");
    let git = t.dir("gitproj");
    std::fs::create_dir_all(git.join(".git")).unwrap();
    let with_projects = t.dir("claudeproj");
    let projects_dir = t.world.env.claude_projects_dir_for(&with_projects).unwrap();
    std::fs::create_dir_all(projects_dir).unwrap();
    let explicit = t.dir("explicit");

    // 1. --workspace beats everything, even a meaningful cwd and -L.
    let explicit_text = explicit.display().to_string();
    assert_eq!(
        t.resolve(&["-w", &explicit_text, "-L"], Some(&git), &gui, &ui, None),
        Some(canon(&explicit))
    );
    // A missing --workspace falls through to the next rule.
    let missing = t.world.home().join("missing").display().to_string();
    assert_eq!(
        t.resolve(&["-w", &missing], Some(&git), &gui, &ui, None),
        Some(canon(&git))
    );
    assert_eq!(
        t.resolve(&["-w", &missing], Some(&plain), &gui, &ui, None),
        None
    );
    // 2. -L is the launch cwd whatever it is (here: the home directory).
    assert_eq!(
        t.resolve(&["-L"], Some(t.world.home()), &gui, &ui, None),
        Some(canon(t.world.home()))
    );
    assert_eq!(
        t.resolve(&["-L"], Some(&plain), &gui, &ui, None),
        Some(canon(&plain))
    );
    // 3. A meaningful cwd: a .git entry (dir or file) or an existing Claude projects dir.
    assert_eq!(
        t.resolve(&[], Some(&git), &gui, &ui, None),
        Some(canon(&git))
    );
    let git_file = t.dir("gitfile");
    std::fs::write(git_file.join(".git"), "gitdir: elsewhere").unwrap();
    assert_eq!(
        t.resolve(&[], Some(&git_file), &gui, &ui, None),
        Some(canon(&git_file))
    );
    assert_eq!(
        t.resolve(&[], Some(&with_projects), &gui, &ui, None),
        Some(canon(&with_projects))
    );
    // ... but not a plain directory.
    assert_eq!(t.resolve(&[], Some(&plain), &gui, &ui, None), None);
    // Not meaningful: a filesystem root, the home directory, the exe directory.
    let root = t.world.home().ancestors().last().unwrap().to_path_buf();
    assert_eq!(t.resolve(&[], Some(&root), &gui, &ui, None), None);
    std::fs::create_dir_all(t.world.home().join(".git")).unwrap();
    assert_eq!(
        t.resolve(&[], Some(t.world.home()), &gui, &ui, None),
        None,
        "the home directory is excluded even with a .git entry"
    );
    assert_eq!(
        t.resolve(&[], Some(&git), &gui, &ui, Some(&git)),
        None,
        "the executable's directory is excluded"
    );
    // A macOS .app bundle: its directory (above Contents/MacOS) is excluded too.
    let bundle = t.dir("Tool.app");
    std::fs::create_dir_all(bundle.join(".git")).unwrap();
    let macos = bundle.join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    assert_eq!(t.resolve(&[], Some(&bundle), &gui, &ui, Some(&macos)), None);
    // No cwd at all.
    assert_eq!(t.resolve(&[], None, &gui, &ui, None), None);
}

/// L-082: `default_workspace` beats state.json; restore_session gates state.json.
#[test]
fn workspace_rules_four_and_five() {
    let t = Table::new();
    let plain = t.dir("plain");
    let default_dir = t.dir("default");
    let remembered = t.dir("remembered");
    let ui = UiState {
        workspace: Some(remembered.clone()),
        ..UiState::default()
    };
    let with_default = GuiSettings {
        default_workspace: Some(default_dir.clone()),
        ..GuiSettings::default()
    };
    // 4. default_workspace beats the remembered workspace.
    assert_eq!(
        t.resolve(&[], Some(&plain), &with_default, &ui, None),
        Some(canon(&default_dir))
    );
    // A meaningful cwd (rule 3) beats default_workspace.
    let git = t.dir("git");
    std::fs::create_dir_all(git.join(".git")).unwrap();
    assert_eq!(
        t.resolve(&[], Some(&git), &with_default, &ui, None),
        Some(canon(&git))
    );
    // 5. The remembered workspace needs restore_session and an existing directory.
    assert_eq!(
        t.resolve(&[], Some(&plain), &GuiSettings::default(), &ui, None),
        Some(canon(&remembered))
    );
    let no_restore = GuiSettings {
        restore_session: Some(false),
        ..GuiSettings::default()
    };
    assert_eq!(t.resolve(&[], Some(&plain), &no_restore, &ui, None), None);
    let gone = UiState {
        workspace: Some(t.world.home().join("gone")),
        ..UiState::default()
    };
    assert_eq!(
        t.resolve(&[], Some(&plain), &GuiSettings::default(), &gone, None),
        None
    );
    // A default_workspace that does not exist falls through to the remembered one.
    let stale_default = GuiSettings {
        default_workspace: Some(t.world.home().join("gone")),
        ..GuiSettings::default()
    };
    assert_eq!(
        t.resolve(&[], Some(&plain), &stale_default, &ui, None),
        Some(canon(&remembered))
    );
}

/// L-082: the app-level entry sets the workspace in `AppState` (once).
#[gpui_kit::test]
fn recorded_cwd_entry_updates_the_app_workspace(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let project = world.home().join("pi-only-project");
    std::fs::create_dir_all(&project).unwrap();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    // Pretend the app was launched from `project` with no workspace chosen.
    cx.update(|cx| {
        update_app_state(cx, |state, _| {
            state.launch.launch_cwd = Some(project.clone());
            state.set_workspace(None);
        });
        assert!(!apply_recorded_cwd_workspace(cx, [world.home()]));
        assert!(app_state(cx).workspace.is_none());
        assert!(apply_recorded_cwd_workspace(cx, [project.as_path()]));
        assert_eq!(
            app_state(cx)
                .workspace
                .as_ref()
                .map(|w| w.dir().to_path_buf()),
            Some(canon(&project))
        );
        assert!(!apply_recorded_cwd_workspace(cx, [project.as_path()]));
    });
}

/// L-082: the cwd recorded by a Pi/OMP session is meaningful once the first batch is loaded.
#[test]
fn pi_omp_recorded_cwd_selects_the_workspace() {
    let t = Table::new();
    let cwd = t.dir("pi-project");
    let other = t.dir("other");
    let env = &t.world.env;
    let found = workspace_from_recorded_cwd_with(
        Some(&cwd),
        None,
        env,
        [other.as_path(), cwd.as_path()],
        None,
    );
    assert_eq!(
        found.map(|scope| scope.dir().to_path_buf()),
        Some(canon(&cwd))
    );
    // No match, an already chosen workspace, the home directory and the exe directory do not.
    assert!(
        workspace_from_recorded_cwd_with(Some(&cwd), None, env, [other.as_path()], None).is_none()
    );
    let chosen = WorkspaceScope::new(&other).unwrap();
    assert!(
        workspace_from_recorded_cwd_with(Some(&cwd), Some(&chosen), env, [cwd.as_path()], None)
            .is_none()
    );
    assert!(
        workspace_from_recorded_cwd_with(Some(t.world.home()), None, env, [t.world.home()], None)
            .is_none()
    );
    assert!(
        workspace_from_recorded_cwd_with(Some(&cwd), None, env, [cwd.as_path()], Some(&cwd))
            .is_none()
    );
    assert!(workspace_from_recorded_cwd_with(None, None, env, [cwd.as_path()], None).is_none());
}

// ---------------------------------------------------------------------------
// Initial view (PD17, C-066, S-034, V-049)
// ---------------------------------------------------------------------------

fn view_of(
    args: &[&str],
    config_toml: Option<&str>,
    gui_toml: Option<&str>,
    ui: &UiState,
) -> (claude_history_core::view::ViewPrefs, ListSearchMode) {
    let world = TestWorld::new();
    world.write_config(config_toml, gui_toml);
    let (config, errors) = load_config(&world.env);
    assert!(errors.is_empty(), "{errors:?}");
    let launch = launch_for(args, Some(world.home()));
    let settings = launch.args.settings(&config.effective);
    resolve_initial_view(&launch, &config, &settings, ui)
}

fn remembered(
    tool: Option<RememberedToolDisplay>,
    thinking: Option<bool>,
    timing: Option<bool>,
    mode: Option<RememberedSearchMode>,
) -> UiState {
    UiState {
        view: RememberedView {
            tool_display: tool,
            show_thinking: thinking,
            show_timing: timing,
        },
        search_mode: mode,
        ..UiState::default()
    }
}

/// C-066 / V-049: tool display precedence.
#[test]
fn initial_tool_display_precedence() {
    use RememberedToolDisplay::{Full, Summary, Truncated};
    let full = remembered(Some(Full), None, None, None);
    let summary = remembered(Some(Summary), None, None, None);
    let none = UiState::default();
    let hidden = ToolDisplayMode::Hidden;
    let tool = |args: &[&str], config: Option<&str>, gui: Option<&str>, ui: &UiState| {
        view_of(args, config, gui, ui).0.tool_display
    };
    // Defaults.
    assert_eq!(tool(&[], None, None, &none), hidden);
    // `no_tools = false` beats remembered Summary; `true` beats remembered Full.
    assert_eq!(
        tool(&[], Some("[display]\nno_tools = false"), None, &summary),
        ToolDisplayMode::Full
    );
    assert_eq!(
        tool(&[], Some("[display]\nno_tools = true"), None, &full),
        hidden
    );
    // gui.toml beats config.toml.
    assert_eq!(
        tool(
            &[],
            Some("[display]\nno_tools = true"),
            Some("[display]\nno_tools = false"),
            &none
        ),
        ToolDisplayMode::Full
    );
    // Remembered applies only without a config key.
    assert_eq!(tool(&[], None, None, &full), ToolDisplayMode::Full);
    assert_eq!(
        tool(
            &[],
            None,
            None,
            &remembered(Some(Truncated), None, None, None)
        ),
        ToolDisplayMode::Truncated
    );
    assert_eq!(tool(&[], None, None, &summary), hidden);
    // Flags beat both.
    assert_eq!(
        tool(
            &["--no-tools"],
            Some("[display]\nno_tools = false"),
            None,
            &full
        ),
        hidden
    );
    assert_eq!(
        tool(&["-t"], Some("[display]\nno_tools = true"), None, &summary),
        ToolDisplayMode::Full
    );
    assert_eq!(tool(&["--no-tools"], None, None, &full), hidden);
    // restore_session = false ignores the state.
    assert_eq!(
        tool(&[], None, Some("[gui]\nrestore_session = false"), &full),
        hidden
    );
}

/// C-066: thinking and timing.
#[test]
fn initial_thinking_and_timing_precedence() {
    let on = remembered(None, Some(true), Some(true), None);
    let off = remembered(None, Some(false), None, None);
    let view = |args: &[&str], config: Option<&str>, gui: Option<&str>, ui: &UiState| {
        view_of(args, config, gui, ui).0
    };
    assert!(!view(&[], None, None, &UiState::default()).show_thinking);
    assert!(
        view(&[], None, None, &on).show_thinking,
        "remembered applies"
    );
    assert!(!view(&["--hide-thinking"], None, None, &on).show_thinking);
    assert!(view(&["--show-thinking"], None, None, &off).show_thinking);
    // A config key beats the remembered value.
    assert!(!view(&[], Some("[display]\nshow_thinking = false"), None, &on).show_thinking);
    assert!(view(&[], Some("[display]\nshow_thinking = true"), None, &off).show_thinking);
    // Timing comes only from the state.
    assert!(view(&[], None, None, &on).show_timing);
    assert!(!view(&[], None, None, &off).show_timing);
    let no_restore = Some("[gui]\nrestore_session = false");
    assert!(!view(&[], None, no_restore, &on).show_timing);
    assert!(!view(&[], None, no_restore, &on).show_thinking);
}

/// S-034 / C-062: list mode precedence.
#[test]
fn initial_list_mode_precedence() {
    use RememberedSearchMode::{Lexical, Semantic};
    let mode = |args: &[&str], config: Option<&str>, gui: Option<&str>, ui: &UiState| {
        view_of(args, config, gui, ui).1
    };
    let state = |mode| remembered(None, None, None, Some(mode));
    let none = UiState::default();
    assert_eq!(mode(&[], None, None, &none), ListSearchMode::Lexical);
    // `[search] mode = "semantic"` with state lexical → Semantic.
    assert_eq!(
        mode(
            &[],
            Some("[search]\nmode = \"semantic\""),
            None,
            &state(Lexical)
        ),
        ListSearchMode::Semantic
    );
    // No config key with state semantic → Semantic.
    assert_eq!(
        mode(&[], None, None, &state(Semantic)),
        ListSearchMode::Semantic
    );
    // The deprecated [tui] key counts as a config key.
    assert_eq!(
        mode(
            &[],
            Some("[tui]\nsemantic_search = false"),
            None,
            &state(Semantic)
        ),
        ListSearchMode::Lexical
    );
    // exact / hybrid start Lexical in the list.
    assert_eq!(
        mode(
            &[],
            Some("[search]\nmode = \"hybrid\""),
            None,
            &state(Semantic)
        ),
        ListSearchMode::Lexical
    );
    // --lexical / --semantic beat both.
    assert_eq!(
        mode(
            &["--lexical"],
            Some("[search]\nmode = \"semantic\""),
            None,
            &state(Semantic)
        ),
        ListSearchMode::Lexical
    );
    assert_eq!(
        mode(
            &["--semantic"],
            Some("[search]\nmode = \"exact\""),
            None,
            &state(Lexical)
        ),
        ListSearchMode::Semantic
    );
    // restore_session = false ignores the state.
    assert_eq!(
        mode(
            &[],
            None,
            Some("[gui]\nrestore_session = false"),
            &state(Semantic)
        ),
        ListSearchMode::Lexical
    );
}

/// The time filter is never restored: the launch context carries only the flags.
#[test]
fn time_filter_comes_only_from_flags() {
    assert!(gui(&[]).launch().time_filter.after.is_none());
    assert!(
        gui(&["--before", "1d"])
            .launch()
            .time_filter
            .before
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// PATH augmentation and the login-shell waiter (§14.2)
// ---------------------------------------------------------------------------

#[test]
fn login_shell_path_is_prepended_deduplicated_with_existing_extras() {
    let home = PathBuf::from(if cfg!(windows) {
        "C:\\home\\me"
    } else {
        "/home/me"
    });
    let local_bin = home.join(".local/bin");
    let brew = PathBuf::from("/opt/homebrew/bin");
    let usr_local = PathBuf::from("/usr/local/bin");
    let join = |paths: &[&Path]| std::env::join_paths(paths).unwrap();
    let nvm = PathBuf::from("/nvm/bin");
    let usr = PathBuf::from("/usr/bin");
    let login = join(&[nvm.as_path(), usr.as_path()]);
    let existing = join(&[usr.as_path(), usr_local.as_path()]);
    let exists = |dir: &Path| dir == local_bin || dir == brew || dir == usr_local;
    let merged = augment_path(Some(&existing), &login, Some(&home), exists);
    let entries: Vec<PathBuf> = std::env::split_paths(&merged).collect();
    assert_eq!(
        entries,
        vec![nvm, usr, local_bin, brew, usr_local],
        "login entries first, then the extras that exist, then the old PATH; no duplicates"
    );
    // Without an existing PATH and without a home.
    let merged = augment_path(None, &login, None, |_| false);
    assert_eq!(std::env::split_paths(&merged).count(), 2);
}

#[test]
fn the_probe_runs_on_unix_outside_terminals_only() {
    assert!(should_probe_login_shell(true, false));
    assert!(!should_probe_login_shell(true, true));
    assert!(!should_probe_login_shell(false, false));
}

#[test]
fn the_waiter_returns_the_path_or_times_out() {
    let shared = Arc::new((Mutex::new(ProbeState::default()), Condvar::new()));
    let waiter = LoginShellWaiter {
        shared: shared.clone(),
    };
    assert!(!waiter.is_finished());
    let started = Instant::now();
    assert_eq!(
        waiter.wait(Duration::from_millis(50)),
        None,
        "still running"
    );
    assert!(started.elapsed() >= Duration::from_millis(40));
    let thread_shared = shared.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        let (lock, cvar) = &*thread_shared;
        let mut state = lock.lock().unwrap();
        state.finished = true;
        state.path = Some(OsString::from("/x"));
        cvar.notify_all();
    });
    assert_eq!(
        waiter.wait(Duration::from_secs(5)),
        Some(OsString::from("/x"))
    );
    worker.join().unwrap();
    assert!(waiter.is_finished());
}

#[test]
fn clipboard_variable_notice() {
    assert_eq!(
        clipboard_env_notice(Some("osc52")),
        Some("CLAUDE_HISTORY_CLIPBOARD is ignored by the GUI".to_string())
    );
    assert_eq!(clipboard_env_notice(None), None);
}

// ---------------------------------------------------------------------------
// G tests
// ---------------------------------------------------------------------------

/// C-023: an invalid config.toml → a dialog naming it; a valid gui.toml theme still applies.
#[gpui_kit::test]
fn invalid_config_toml_opens_its_dialog_and_gui_toml_still_applies(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    world.write_config(
        Some("this is = not toml ["),
        Some("[gui]\ntheme = \"dark\"\n"),
    );
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.read(|cx| {
        let state = app_state(cx);
        assert!(state.config_errors.cli.is_some());
        assert!(state.config_errors.gui.is_none());
        assert_eq!(state.config.gui.theme(), ThemePreference::Dark);
        // ... and it is applied, not just parsed.
        assert_eq!(crate::theme_tokens::preference(cx), ThemePreference::Dark);
        assert!(cx.theme().is_dark(), "the gpui-kit theme is dark");
    });
    cx.update_window(handle, |_, window, cx| {
        assert!(!window.has_active_dialog(cx));
        show_config_errors(window, cx);
        assert!(window.has_active_dialog(cx), "the dialog is open");
        assert_eq!(last_dialog(cx), "Couldn't read config.toml");
        let error = app_state(cx).config_errors.cli.clone().unwrap();
        assert!(
            error.starts_with("Configuration error: Failed to parse config file at '"),
            "core's text: {error}"
        );
    })
    .unwrap();
}

fn open_config_dialog(cx: &mut TestAppContext) -> gpui_kit::AnyWindowHandle {
    let world = TestWorld::new();
    world.write_config(Some("this is = not toml ["), None);
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| show_config_errors(window, cx))
        .unwrap();
    cx.run_until_parked();
    handle
}

/// §5.20: Enter is "Continue with defaults" (the primary button): the dialog
/// closes and the persistent warning stays up.
#[gpui_kit::test]
fn enter_in_the_config_error_dialog_continues_with_defaults(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let handle = open_config_dialog(cx);
    cx.simulate_keystrokes(handle, "enter");
    cx.update_window(handle, |_, window, cx| {
        assert!(!window.has_active_dialog(cx), "the dialog closed");
    })
    .unwrap();
    assert_eq!(
        cx.read(last_toast),
        "config.toml has errors; using defaults for its settings"
    );
    assert_eq!(cx.read(exit::quit_requested), None);
}

/// Esc counts as "Continue with defaults" too, and the closed dialog's id
/// does not leak into the QA probe's answer for a later dialog.
#[gpui_kit::test]
fn escape_in_the_config_error_dialog_continues_and_clears_the_dialog_id(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let handle = open_config_dialog(cx);
    cx.simulate_keystrokes(handle, "escape");
    cx.update_window(handle, |_, window, cx| {
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();
    assert_eq!(
        cx.read(last_toast),
        "config.toml has errors; using defaults for its settings"
    );
    assert_eq!(cx.read(last_dialog), "", "the closed dialog is forgotten");
}

/// C-023: an invalid gui.toml → its own dialog; both invalid → one each, gui.toml on top.
#[gpui_kit::test]
fn invalid_gui_toml_opens_its_dialog(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    world.write_config(None, Some("[gui\ntheme = "));
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| {
        show_config_errors(window, cx);
        assert!(window.has_active_dialog(cx));
        assert_eq!(last_dialog(cx), "Couldn't read gui.toml");
        assert!(app_state(cx).config_errors.gui.is_some());
        assert!(app_state(cx).config_errors.cli.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn both_files_invalid_open_two_dialogs_gui_toml_on_top(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    world.write_config(Some("a = "), Some("b = "));
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| {
        show_config_errors(window, cx);
        assert_eq!(
            last_dialog(cx),
            "Couldn't read gui.toml",
            "the last one opened"
        );
    })
    .unwrap();
}

/// §5.20: Continue with defaults → persistent warning; Retry re-reads; Quit quits.
#[gpui_kit::test]
fn config_error_dialog_outcomes(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    world.write_config(Some("a = "), None);
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| {
        // Continue with defaults: a persistent warning toast.
        finish_config_dialog(ConfigFileKind::Cli, ConfigDialogEnd::Continue, window, cx);
        assert_eq!(
            last_toast(cx),
            "config.toml has errors; using defaults for its settings"
        );
        // Retry while the file is still broken keeps the error and re-opens the dialog.
        finish_config_dialog(ConfigFileKind::Cli, ConfigDialogEnd::Retry, window, cx);
        assert!(app_state(cx).config_errors.cli.is_some());
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert!(window.has_active_dialog(cx), "the dialog came back");
    })
    .unwrap();
    // The user fixes the file; Retry loads it.
    world.write_config(Some("[display]\nlast = false\n"), None);
    cx.update_window(handle, |_, window, cx| {
        finish_config_dialog(ConfigFileKind::Cli, ConfigDialogEnd::Retry, window, cx);
        let state = app_state(cx);
        assert!(state.config_errors.is_empty());
        assert!(!state.settings.show_last, "the new settings are in use");
        assert_eq!(last_toast(cx), "config.toml loaded");
        // Quit exits 0.
        finish_config_dialog(ConfigFileKind::Cli, ConfigDialogEnd::Quit, window, cx);
        assert_eq!(exit::quit_requested(cx), Some(0));
    })
    .unwrap();
}

/// C-019: a missing `--workspace` shows `Workspace not found: <DIR>` at startup.
#[gpui_kit::test]
fn missing_workspace_argument_shows_a_toast(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let missing = world.home().join("nope");
    let missing_text = missing.display().to_string();
    let (handle, _ws) = open_main(cx, &world, &["-w", &missing_text]);
    cx.update_window(handle, |_, window, cx| {
        assert!(app_state(cx).workspace.is_none());
        show_config_errors(window, cx);
        assert_eq!(
            last_toast(cx).to_string(),
            format!("Workspace not found: {missing_text}")
        );
    })
    .unwrap();
}

/// C-047: a set CLAUDE_HISTORY_CLIPBOARD logs one Info line after the first frame.
#[gpui_kit::test]
fn clipboard_variable_logs_an_info_line(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        crate::debug_sink::install(cx, None);
    });
    note_clipboard_env(Some("osc52"));
    cx.update_window(handle, |_, window, cx| {
        show_config_errors(window, cx);
        let lines = cx.global::<DebugBufferHandle>().0.lines();
        assert!(
            lines
                .iter()
                .any(|line| line.text == "CLAUDE_HISTORY_CLIPBOARD is ignored by the GUI"),
            "{lines:?}"
        );
    })
    .unwrap();
    // Consumed: a second first-frame pass adds nothing.
    cx.update_window(handle, |_, window, cx| {
        let before = cx.global::<DebugBufferHandle>().0.len();
        show_config_errors(window, cx);
        assert_eq!(cx.global::<DebugBufferHandle>().0.len(), before);
    })
    .unwrap();
    debug::reset_sink();
}

/// C-051: the sink writes lines at/above the `--debug` level to stderr, everything to the buffer.
#[gpui_kit::test]
fn debug_sink_filters_stderr_by_the_debug_level(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let rec = recording(cx);
    rec.clear_calls();
    cx.update(|cx| crate::debug_sink::install(cx, Some(DebugLevel::Warn)));
    debug::info(Some(DebugLevel::Info), "a");
    debug::warn(Some(DebugLevel::Info), "b");
    let stderr: Vec<ServiceCall> = rec
        .calls()
        .into_iter()
        .filter(|call| matches!(call, ServiceCall::Stderr(_)))
        .collect();
    assert_eq!(stderr, vec![ServiceCall::Stderr("[WARN] b\n".to_string())]);
    let lines = cx.read(|cx| cx.global::<DebugBufferHandle>().0.lines());
    let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(texts, vec!["a", "b"], "the buffer holds both lines");
    // Without --debug nothing reaches stderr.
    rec.clear_calls();
    cx.update(|cx| crate::debug_sink::install(cx, None));
    debug::warn(Some(DebugLevel::Info), "c");
    assert!(rec.stderr().is_empty());
    debug::reset_sink();
}

/// §14.2: the probe merges the login shell's PATH and releases the waiters.
#[gpui_kit::test]
fn login_shell_probe_updates_the_environment_path(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let rec = recording(cx);
    let tool_dir = world.home().join("nvm").join("bin");
    std::fs::create_dir_all(&tool_dir).unwrap();
    rec.set_login_shell_path(Some(std::env::join_paths([&tool_dir]).unwrap()));
    cx.update(run_login_shell_probe);
    let waiter = cx.read(login_shell_waiter).expect("a probe is running");
    let merged = waiter
        .wait(Duration::from_secs(10))
        .expect("the probe finished");
    assert_eq!(
        std::env::split_paths(&merged).next(),
        Some(tool_dir.clone())
    );
    cx.executor().allow_parking();
    for _ in 0..200 {
        cx.run_until_parked();
        let applied = cx.read(|cx| app_state(cx).env.path_var.clone());
        if applied.as_ref() == Some(&merged) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        cx.read(|cx| app_state(cx).env.path_var.clone()),
        Some(merged)
    );
    assert!(rec.calls().contains(&ServiceCall::LoginShellPath));
    // A second start is a no-op.
    cx.update(run_login_shell_probe);
    assert_eq!(
        rec.calls()
            .iter()
            .filter(|call| **call == ServiceCall::LoginShellPath)
            .count(),
        1
    );
}

/// A failing probe leaves PATH unchanged.
#[gpui_kit::test]
fn failed_login_shell_probe_leaves_path_unchanged(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let before = cx.read(|cx| app_state(cx).env.path_var.clone());
    recording(cx).set_login_shell_path(None);
    cx.update(run_login_shell_probe);
    let waiter = cx.read(login_shell_waiter).unwrap();
    assert_eq!(waiter.wait(Duration::from_secs(10)), None);
    assert!(waiter.is_finished());
    cx.executor().allow_parking();
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| app_state(cx).env.path_var.clone()), before);
}

/// AD14: the startup tools run their entry points (a dialog or a toast appears).
#[gpui_kit::test]
fn startup_tools_run_their_entry_points(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    for tool in [
        StartupTool::ShowDir,
        StartupTool::DeleteSession("abc".into()),
        StartupTool::DebugSearch {
            query: "q".into(),
            local: true,
        },
        StartupTool::SemanticSearch {
            query: "q".into(),
            top: 3,
            local: false,
        },
        StartupTool::SemanticDebug {
            query: "q".into(),
            local: false,
        },
        StartupTool::GenerateSemanticCache { local: false },
        StartupTool::ClearSemanticCache,
        StartupTool::DeleteEmpty { local: true },
        StartupTool::CheckForUpdates,
    ] {
        cx.update_window(handle, |_, window, cx| {
            cx.set_global(crate::globals::LastToast(Default::default()));
            run_startup_tool(tool.clone(), window, cx);
            assert!(
                window.has_active_dialog(cx) || !last_toast(cx).is_empty(),
                "{tool:?} shows a dialog or a toast"
            );
            // Close whatever opened so the next tool starts clean.
            while window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
        })
        .unwrap();
        cx.run_until_parked();
    }
    // Recording services never touched the desktop.
    assert!(
        recording(cx)
            .calls()
            .iter()
            .all(|call| !matches!(call, ServiceCall::Terminal(_) | ServiceCall::OpenUrl(_)))
    );
    let _ = SemanticReportMode::DebugDump;
}

/// X-017-style guard: the dispatch never writes anything but the result to stdout.
#[test]
fn usage_errors_write_nothing_to_stdout() {
    let world = TestWorld::new();
    let result = run(&world, &["--nope"], StdoutKind::Terminal, None);
    assert_eq!(result.code(), 2);
    assert!(result.rec.stdout().is_empty());
    assert!(
        result
            .rec
            .stderr()
            .starts_with("error: unexpected argument '--nope' found")
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The committed QA corpus (`tests/fixtures/qa-home`, owned by WP7) loads in core with every
/// source, an empty transcript and a malformed line, and never carries the sandbox marker.
#[test]
fn the_qa_home_fixture_loads_in_core() {
    use claude_history_core::history::{Source, load_all_conversations};

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("qa-home");
    assert!(fixture.is_dir(), "{}", fixture.display());
    assert!(
        !fixture.join(crate::qa::gate::SANDBOX_MARKER).exists(),
        "the marker is added by new-sandbox.ps1, never committed"
    );
    let world = TestWorld::new();
    copy_dir(&fixture, world.home());
    let conversations = load_all_conversations(&world.env, true, None).expect("the corpus loads");
    let titles = |source: Source| {
        let mut titles: Vec<String> = conversations
            .iter()
            .filter(|conversation| conversation.source == source)
            .map(|conversation| conversation.summary.clone().unwrap_or_default())
            .collect();
        titles.sort();
        titles
    };
    assert_eq!(
        titles(Source::Claude),
        vec![
            "Add API pagination",
            "Deploy pipeline",
            "Fix login bug",
            "Malformed line",
            "Rate limit errors",
            "Token refresh retry",
            "Worktree cleanup",
        ]
    );
    assert_eq!(titles(Source::Pi).len(), 1);
    assert_eq!(titles(Source::Omp).len(), 1);
}

/// C-023: after the startup dialog's Retry, the fixed settings take effect
/// without a restart: the `[keys]` bindings, the excluded projects, the
/// preview mode and an explicit gui.toml theme.
#[gpui_kit::test]
fn config_retry_applies_keys_exclusions_preview_and_theme_live(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    world.write_config(Some("a = "), Some("[gui\n"));
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    assert!(cx.read(|cx| list.read(cx).show_last()));
    world.write_config(
        Some("[keys]\nresume = \"ctrl+t\"\n[display]\nlast = false\n[tui]\nexclude_projects = [\"skipme\"]\n"),
        Some("[gui]\ntheme = \"dark\"\n"),
    );
    cx.update_window(handle, |_, window, cx| {
        finish_config_dialog(ConfigFileKind::Cli, ConfigDialogEnd::Retry, window, cx);
        assert!(app_state(cx).config_errors.is_empty());
        assert_eq!(crate::theme_tokens::preference(cx), ThemePreference::Dark);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        assert!(
            keymap
                .bindings_for_action(&crate::actions::Resume)
                .any(|binding| binding
                    .keystrokes()
                    .first()
                    .is_some_and(|key| key.inner().key == "t" && key.inner().modifiers.control)),
            "[keys] resume is bound"
        );
        let list = list.read(cx);
        assert!(list.excluded_projects().contains("skipme"));
        assert!(!list.show_last(), "the preview mode follows [display] last");
    });
}
