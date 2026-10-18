//! Startup decisions (DESIGN §11.1, §11.23, PD3, PD17, AD14). WP0 → WP7.
//!
//! * the command line → [`dispatch`] (what `main` does with a parsed
//!   command: print and exit, run a headless path, or start the GUI with a
//!   [`LaunchContext`]); [`to_launch_context`], [`startup_tool_for`],
//!   [`extract_qa_args`];
//! * configuration: [`load_config`] (both files independently),
//!   [`show_config_errors`] (one dialog per failing file + the other first
//!   frame notices), [`reload_config`];
//! * the workspace (PD3): [`resolve_workspace`] and the pieces it is built
//!   from ([`is_meaningful_cwd`], [`workspace_from_recorded_cwd`]);
//! * the initial view (PD17, C-066): [`resolve_initial_view`];
//! * the headless paths (§11.23): [`run_agent`], [`run_show_dir`],
//!   [`run_render`];
//! * startup tools (AD14): [`run_startup_tool`];
//! * the login-shell PATH probe (§14.2): [`start_login_shell_probe`],
//!   [`login_shell_waiter`], [`augment_path`].
//!
//! `main` is the only reader of the process state (arguments, current
//! directory, environment); everything here takes its inputs as values so it
//! is tested without touching the process.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use claude_history_core::agent::{AgentCli, AgentRequest};
use claude_history_core::browser::ListSearchMode;
use claude_history_core::config::{
    GuiSettings, LayeredConfig, load_config_from, load_gui_config_from,
};
use claude_history_core::debug::{self, DebugLevel};
use claude_history_core::export::{ExportFormat, ExportOptions, generate_export};
use claude_history_core::settings::{
    EffectiveSettings, GuiLaunchArgs, LaunchArgsError, LaunchCommand, help_text, parse_launch_args,
};
use claude_history_core::time_filter::TimeFilter;
use claude_history_core::view::{ToolDisplayMode, ViewPrefs};
use claude_history_core::{AppError, HistoryEnv, WorkspaceScope};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, IconName, WindowExt as _, h_flex};
use gpui_kit::{App, Global, SharedString, Task, Window};
use gpui_kit::{ParentElement as _, Styled as _};

use crate::exit::{EXIT_ERROR, EXIT_OK, EXIT_REFUSED, EXIT_USAGE};
use crate::globals::{app_state, same_path, update_app_state};
use crate::model::{ConfigErrors, LaunchContext, QaArgs, StartupTool, StdoutKind};
use crate::notify::Toast;
use crate::persist::{RememberedSearchMode, RememberedToolDisplay, UiState};
use crate::services::{PlatformServices, services};

/// The program name in help, usage and version texts.
pub const PROGRAM: &str = "claude-history-gui";

/// `claude-history-gui <version>\n` (X-032).
pub fn version_text() -> String {
    format!("{PROGRAM} {}\n", env!("CARGO_PKG_VERSION"))
}

// ---------------------------------------------------------------------------
// Command line → launch context
// ---------------------------------------------------------------------------

/// Map the parsed command line to the launch context. `[FILE]` and
/// `--render FILE` (when it is not printed headless) open the single-file
/// window; a relative file is resolved against the launch directory.
pub fn to_launch_context(
    args: GuiLaunchArgs,
    launch_cwd: Option<PathBuf>,
    time_filter: TimeFilter,
    qa: Option<QaArgs>,
) -> LaunchContext {
    let file = args
        .file
        .clone()
        .or_else(|| args.render.clone())
        .map(|file| match &launch_cwd {
            Some(cwd) if file.is_relative() => cwd.join(file),
            _ => file,
        });
    LaunchContext {
        picker: args.picker(),
        startup_tool: startup_tool_for(&args),
        file,
        args,
        launch_cwd,
        time_filter,
        qa,
        terminal_var: None,
    }
}

/// The Tools dialog a one-shot flag opens after startup (AD14, K14 flags).
/// `--local` preselects the Workspace scope of the tools that have one.
pub fn startup_tool_for(args: &GuiLaunchArgs) -> Option<StartupTool> {
    let local = args.local;
    if args.show_dir {
        Some(StartupTool::ShowDir)
    } else if let Some(id) = &args.delete {
        Some(StartupTool::DeleteSession(id.clone()))
    } else if let Some(query) = &args.debug_search {
        Some(StartupTool::DebugSearch {
            query: query.clone(),
            local,
        })
    } else if let Some(query) = &args.semantic_search {
        Some(StartupTool::SemanticSearch {
            query: query.clone(),
            top: args.semantic_top_or_default(),
            local,
        })
    } else if let Some(query) = &args.debug_semantic_search {
        Some(StartupTool::SemanticDebug {
            query: query.clone(),
            local,
        })
    } else if args.generate_semantic_cache {
        Some(StartupTool::GenerateSemanticCache { local })
    } else if args.clear_semantic_cache {
        Some(StartupTool::ClearSemanticCache)
    } else {
        None
    }
}

/// Pull the QA runner's flags (`--qa-scenario`, `--qa-out`, `--qa-model-dir`,
/// `--qa-real-console`, §15.3) out of the argument list; core's parser never
/// sees them. Returns the remaining arguments and the QA arguments (`None`
/// without `--qa-scenario`). Used by `main` in `--features qa` builds only.
pub fn extract_qa_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<(Vec<OsString>, Option<QaArgs>), String> {
    let mut rest = Vec::new();
    let mut scenario = None;
    let mut out_dir = None;
    let mut model_dir = None;
    let mut real_console = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy().into_owned();
        let (name, inline) = match text.split_once('=') {
            Some((name, value)) if name.starts_with("--qa-") => {
                (name.to_string(), Some(OsString::from(value)))
            }
            _ => (text, None),
        };
        let mut value_of = |flag: &str| -> Result<PathBuf, String> {
            inline
                .clone()
                .or_else(|| iter.next())
                .map(PathBuf::from)
                .ok_or_else(|| {
                    format!("a value is required for '{flag} <PATH>' but none was supplied")
                })
        };
        match name.as_str() {
            "--qa-scenario" => scenario = Some(value_of("--qa-scenario")?),
            "--qa-out" => out_dir = Some(value_of("--qa-out")?),
            "--qa-model-dir" => model_dir = Some(value_of("--qa-model-dir")?),
            "--qa-real-console" => real_console = true,
            _ => rest.push(arg),
        }
    }
    let qa = match scenario {
        Some(scenario) => {
            let out_dir = out_dir
                .ok_or_else(|| "the following required arguments were not provided: --qa-out <PATH> (required by --qa-scenario)".to_string())?;
            Some(QaArgs {
                scenario,
                out_dir,
                model_dir,
                real_console,
            })
        }
        None => {
            if out_dir.is_some() || model_dir.is_some() || real_console {
                return Err(
                    "the following required arguments were not provided: --qa-scenario <PATH>"
                        .to_string(),
                );
            }
            None
        }
    };
    Ok((rest, qa))
}

/// The usage line of a usage error: the subcommand's own line for
/// `agent …`, `update`, `delete-empty` and `help`, else the top-level one.
pub fn usage_line(args: &[OsString]) -> String {
    let top = format!("Usage: {PROGRAM} [OPTIONS] [FILE]");
    let first = match args.first().and_then(|arg| arg.to_str()) {
        Some(first @ ("agent" | "update" | "delete-empty" | "help")) => first,
        _ => return top,
    };
    let mut probe = vec![OsString::from(first)];
    if first == "agent"
        && let Some(sub) = args.get(1).and_then(|arg| arg.to_str())
        && matches!(sub, "search" | "within" | "read" | "outline")
    {
        probe.push(OsString::from(sub));
    }
    probe.push(OsString::from("--help"));
    let text = match parse_launch_args(probe) {
        Ok(LaunchCommand::SubcommandHelp(text) | LaunchCommand::Agent(AgentCli::Help(text))) => {
            text
        }
        _ => return top,
    };
    text.lines()
        .find(|line| line.starts_with("Usage:"))
        .map(str::to_string)
        .unwrap_or(top)
}

/// The text a usage error prints to stderr (clap style).
pub fn usage_error_text(error: &LaunchArgsError, args: &[OsString]) -> String {
    format!(
        "{error}\n\n{}\n\nFor more information, try '--help'.\n",
        usage_line(args)
    )
}

/// What the process learned about itself (read once, in `main`).
#[derive(Clone, Debug, Default)]
pub struct ProcessInfo {
    /// The launch working directory.
    pub launch_cwd: Option<PathBuf>,
    /// QA runner arguments (`--features qa` builds).
    pub qa: Option<QaArgs>,
}

/// What `main` does next.
#[derive(Debug)]
pub enum Flow {
    /// Done without a window: exit with this code.
    Exit(i32),
    /// Start the GUI.
    Gui(Box<LaunchContext>, Box<HistoryEnv>),
}

/// The environment of this run, after the sandbox gate; `Err` is the refusal
/// text (exit code [`EXIT_REFUSED`]).
pub type PrepareEnv<'a> = &'a mut dyn FnMut() -> Result<HistoryEnv, String>;

/// Act on a parsed command line (§11.1, §11.23): print help/version/usage
/// errors, answer `agent …`, `--show-dir` and `--render` headless when
/// stdout allows, else produce the launch context of the GUI. `raw_args` are
/// the arguments after the program name (for the usage line of an error).
pub fn dispatch(
    parsed: Result<LaunchCommand, LaunchArgsError>,
    raw_args: &[OsString],
    process: &ProcessInfo,
    out: &dyn PlatformServices,
    prepare_env: PrepareEnv<'_>,
) -> Flow {
    let launch_cwd = process.launch_cwd.clone();
    let command = match parsed {
        Ok(command) => command,
        Err(error) => {
            out.write_stderr(&usage_error_text(&error, raw_args));
            return Flow::Exit(EXIT_USAGE);
        }
    };
    let (args, tool) = match command {
        LaunchCommand::Run(args) => (args, None),
        // The subcommands open their Tools dialog in the GUI (§13.1, AD14);
        // `delete-empty --yes` is ignored (the dialog confirms) and `--local`
        // means the launch directory, as `-L` does (PD3 rule 2).
        LaunchCommand::DeleteEmpty { local, .. } => (
            GuiLaunchArgs {
                local,
                ..GuiLaunchArgs::default()
            },
            Some(StartupTool::DeleteEmpty { local }),
        ),
        LaunchCommand::Update => (GuiLaunchArgs::default(), Some(StartupTool::CheckForUpdates)),
        LaunchCommand::Help => return print_text(out, &help_text(PROGRAM)),
        LaunchCommand::Version => return print_text(out, &version_text()),
        LaunchCommand::SubcommandHelp(text) | LaunchCommand::Agent(AgentCli::Help(text)) => {
            return print_text(out, &text);
        }
        LaunchCommand::Agent(AgentCli::Request(request)) => {
            let env = match prepare_env() {
                Ok(env) => env,
                Err(message) => return refused(out, &message),
            };
            return Flow::Exit(run_agent(env, launch_cwd, request, out));
        }
    };
    let time_filter = match args.time_filter() {
        Ok(filter) => filter,
        Err(error) => {
            out.write_stderr(&format!("Error: {}\n", AppError::TimeFilter(error)));
            return Flow::Exit(EXIT_ERROR);
        }
    };
    // Only the headless paths need the stream kind: querying it attaches to the
    // parent console on Windows, which a plain GUI launch must not do (§14.3).
    let stdout = if args.show_dir || args.render.is_some() {
        out.stdout_kind()
    } else {
        StdoutKind::None
    };
    if args.show_dir && stdout != StdoutKind::None {
        let env = match prepare_env() {
            Ok(env) => env,
            Err(message) => return refused(out, &message),
        };
        return Flow::Exit(run_show_dir(&env, &args, launch_cwd.as_deref(), out));
    }
    if let (Some(file), StdoutKind::Redirected) = (&args.render, stdout) {
        let env = match prepare_env() {
            Ok(env) => env,
            Err(message) => return refused(out, &message),
        };
        return Flow::Exit(run_render(
            env,
            &args,
            file.clone(),
            launch_cwd.as_deref(),
            out,
        ));
    }
    let env = match prepare_env() {
        Ok(env) => env,
        Err(message) => return refused(out, &message),
    };
    let mut launch = to_launch_context(args, launch_cwd, time_filter, process.qa.clone());
    if tool.is_some() {
        launch.startup_tool = tool;
    }
    Flow::Gui(Box::new(launch), Box::new(env))
}

fn print_text(out: &dyn PlatformServices, text: &str) -> Flow {
    // Without any stdout (a desktop launch) there is nowhere to print.
    let _ = out.write_stdout(text);
    Flow::Exit(EXIT_OK)
}

fn refused(out: &dyn PlatformServices, message: &str) -> Flow {
    out.write_stderr(&format!("{message}\n"));
    Flow::Exit(EXIT_REFUSED)
}

// ---------------------------------------------------------------------------
// Headless paths (§11.23)
// ---------------------------------------------------------------------------

/// `agent search|within|read|outline …` (K15): the protocol output and exit
/// code of `core::agent::execute`, workspace = the launch directory (the
/// upstream CLI used its cwd). Returns the exit code.
pub fn run_agent(
    env: HistoryEnv,
    launch_cwd: Option<PathBuf>,
    request: AgentRequest,
    out: &dyn PlatformServices,
) -> i32 {
    let env = match launch_cwd {
        Some(cwd) => env.with_workspace(cwd),
        None => env,
    };
    let (config, errors) = load_config(&env);
    if let Some(error) = errors.cli.or(errors.gui) {
        out.write_stderr(&format!("Error: {error}\n"));
        return EXIT_ERROR;
    }
    let response = claude_history_core::agent::execute(&env, &config.effective, request);
    if !response.stdout.is_empty() {
        let _ = out.write_stdout(&response.stdout);
    }
    if !response.stderr.is_empty() {
        out.write_stderr(&response.stderr);
    }
    response.exit_code
}

/// `--show-dir` with stdout available: workspace by PD3 rules 1–3 (no GUI
/// state) → `maintenance::show_dir` → the path and a newline.
pub fn run_show_dir(
    env: &HistoryEnv,
    args: &GuiLaunchArgs,
    launch_cwd: Option<&Path>,
    out: &dyn PlatformServices,
) -> i32 {
    let workspace = cli_workspace(args, launch_cwd, env, exe_dir().as_deref());
    match claude_history_core::maintenance::show_dir(env, workspace.as_ref()) {
        Ok(path) => match out.write_stdout(&format!("{}\n", path.display())) {
            Ok(()) => EXIT_OK,
            Err(error) => {
                out.write_stderr(&format!("Error: {error}\n"));
                EXIT_ERROR
            }
        },
        Err(error) => {
            out.write_stderr(&format!("Error: {error}\n"));
            EXIT_ERROR
        }
    }
}

/// `--render FILE` with stdout redirected: the Ledger export (no ANSI) of
/// FILE with the startup display options, to stdout.
pub fn run_render(
    env: HistoryEnv,
    args: &GuiLaunchArgs,
    file: PathBuf,
    launch_cwd: Option<&Path>,
    out: &dyn PlatformServices,
) -> i32 {
    let (config, errors) = load_config(&env);
    if let Some(error) = errors.cli.or(errors.gui) {
        out.write_stderr(&format!("Error: {error}\n"));
        return EXIT_ERROR;
    }
    let file = match launch_cwd {
        Some(cwd) if file.is_relative() => cwd.join(&file),
        _ => file,
    };
    if !file.exists() {
        out.write_stderr(&format!("Error: File not found: {}\n", file.display()));
        return EXIT_ERROR;
    }
    if !file.is_file() {
        out.write_stderr(&format!("Error: Not a file: {}\n", file.display()));
        return EXIT_ERROR;
    }
    let settings = args.settings(&config.effective);
    let options = ExportOptions {
        show_tools: settings.show_tools,
        show_thinking: settings.show_thinking,
    };
    match generate_export(&file, ExportFormat::Ledger, options) {
        Ok(text) => match out.write_stdout(&text) {
            Ok(()) => EXIT_OK,
            Err(error) => {
                out.write_stderr(&format!("Error: {error}\n"));
                EXIT_ERROR
            }
        },
        Err(error) => {
            out.write_stderr(&format!("Error: {error}\n"));
            EXIT_ERROR
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Load `config.toml` and `gui.toml` independently; a failing file
/// contributes defaults and its error (§11.1).
pub fn load_config(env: &HistoryEnv) -> (LayeredConfig, ConfigErrors) {
    let mut errors = ConfigErrors::default();
    let cli = load_config_from(env.config_file().as_deref()).unwrap_or_else(|error| {
        errors.cli = Some(error.to_string());
        Default::default()
    });
    let gui_file = load_gui_config_from(env.gui_config_file().as_deref()).unwrap_or_else(|error| {
        errors.gui = Some(error.to_string());
        Default::default()
    });
    let effective = cli.clone().overlaid_with(&gui_file);
    let gui = gui_file.gui_settings();
    (
        LayeredConfig {
            cli,
            gui_file,
            effective,
            gui,
        },
        errors,
    )
}

/// Which of the two configuration files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigFileKind {
    /// `config.toml` (the CLI's file, read-only).
    Cli,
    /// `gui.toml`.
    Gui,
}

impl ConfigFileKind {
    /// The file name shown in dialogs.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Cli => "config.toml",
            Self::Gui => "gui.toml",
        }
    }

    fn path(self, env: &HistoryEnv) -> Option<PathBuf> {
        match self {
            Self::Cli => env.config_file(),
            Self::Gui => env.gui_config_file(),
        }
    }

    fn error(self, errors: &ConfigErrors) -> Option<&String> {
        match self {
            Self::Cli => errors.cli.as_ref(),
            Self::Gui => errors.gui.as_ref(),
        }
    }
}

/// Dialog title of a configuration error (§5.20).
pub fn config_error_title(kind: ConfigFileKind) -> String {
    format!("Couldn't read {}", kind.file_name())
}

/// The persistent warning after "Continue with defaults" (§5.20).
pub fn config_warning_text(kind: ConfigFileKind) -> String {
    format!(
        "{} has errors; using defaults for its settings",
        kind.file_name()
    )
}

/// The title of the last dialog this module opened (tests and QA probes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LastDialog(pub SharedString);

impl Global for LastDialog {}

/// The last dialog title this module recorded (empty when none).
pub fn last_dialog(cx: &App) -> SharedString {
    cx.try_global::<LastDialog>()
        .map(|dialog| dialog.0.clone())
        .unwrap_or_default()
}

/// Record the id (title) of the dialog just opened, for the QA `dialog` probe;
/// other owners call this when they open a dialog.
pub fn register_dialog(cx: &mut App, id: impl Into<SharedString>) {
    cx.set_global(LastDialog(id.into()));
}

/// How a configuration-error dialog ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigDialogEnd {
    Quit,
    Retry,
    Continue,
}

/// One dialog per failing file, plus the other first-frame notices
/// (`Workspace not found: <DIR>`, the `CLAUDE_HISTORY_CLIPBOARD` info line);
/// called once after the first frame (§11.1 step 4).
pub fn show_config_errors(window: &mut Window, cx: &mut App) {
    if let Some(notice) = take_clipboard_notice() {
        debug::info(Some(DebugLevel::Info), &notice);
    }
    if let Some(dir) = workspace_not_found(&app_state(cx).launch) {
        Toast::warning(format!("Workspace not found: {}", dir.display())).show(window, cx);
    }
    let errors = app_state(cx).config_errors.clone();
    // gui.toml last, so its dialog is on top (the file that blocks saves).
    for kind in [ConfigFileKind::Cli, ConfigFileKind::Gui] {
        if kind.error(&errors).is_some() {
            open_config_error_dialog(kind, window, cx);
        }
    }
}

/// The `--workspace` value when it names no existing directory (the toast
/// `Workspace not found: <DIR>` and the next rule, PD3 rule 1).
pub fn workspace_not_found(launch: &LaunchContext) -> Option<PathBuf> {
    let dir = launch.args.workspace.clone()?;
    let resolved = match &launch.launch_cwd {
        Some(cwd) if dir.is_relative() => cwd.join(&dir),
        _ => dir.clone(),
    };
    (!resolved.is_dir()).then_some(dir)
}

fn open_config_error_dialog(kind: ConfigFileKind, window: &mut Window, cx: &mut App) {
    let state = app_state(cx);
    let message = kind
        .error(&state.config_errors)
        .cloned()
        .unwrap_or_default();
    let path = kind.path(&state.env);
    let title = SharedString::from(config_error_title(kind));
    register_dialog(cx, title.clone());
    let body = SharedString::from(match &path {
        Some(path) => format!("{message}\n\n{}", path.display()),
        None => message,
    });
    // Closes the dialog, then acts (Copy: it captures only `kind`).
    let finish = move |end: ConfigDialogEnd, window: &mut Window, cx: &mut App| {
        window.close_dialog(cx);
        finish_config_dialog(kind, end, window, cx);
    };
    window.open_alert_dialog(cx, move |alert, window, _| {
        let open_path = path.clone();
        alert
            .title(title.clone())
            .icon(Icon::new(IconName::TriangleAlert))
            .description(body.clone())
            .width(window.rem_size() * 36.0)
            .close_button(false)
            .footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .w_full()
                    .child(
                        Button::new("config-quit")
                            .label("Quit")
                            .on_click(move |_, window, cx| {
                                finish(ConfigDialogEnd::Quit, window, cx)
                            }),
                    )
                    .child(Button::new("config-open").label("Open file").on_click(
                        move |_, _, cx| {
                            if let Some(path) = &open_path {
                                services(cx).open_with_system(path, cx);
                            }
                        },
                    ))
                    .child(
                        Button::new("config-retry").label("Retry").on_click(
                            move |_, window, cx| finish(ConfigDialogEnd::Retry, window, cx),
                        ),
                    )
                    .child(
                        Button::new("config-continue")
                            .label("Continue with defaults")
                            .primary()
                            .on_click(move |_, window, cx| {
                                finish(ConfigDialogEnd::Continue, window, cx)
                            }),
                    ),
            )
            // Enter commits the primary button; the dialog closes itself.
            .on_ok(move |_, window, cx| {
                finish_config_dialog(kind, ConfigDialogEnd::Continue, window, cx);
                true
            })
            // Esc counts as "Continue with defaults"; the dialog closes itself.
            .on_cancel(move |_, window, cx| {
                finish_config_dialog(kind, ConfigDialogEnd::Continue, window, cx);
                true
            })
    });
}
fn finish_config_dialog(
    kind: ConfigFileKind,
    end: ConfigDialogEnd,
    window: &mut Window,
    cx: &mut App,
) {
    // The dialog is gone: a later dialog must not report this one's title.
    cx.set_global(LastDialog::default());
    match end {
        ConfigDialogEnd::Quit => crate::exit::quit(cx),
        ConfigDialogEnd::Retry => {
            let before = app_state(cx).settings.clone();
            reload_config(cx);
            crate::settings::apply_reloaded(&before, window, cx);
            let still_failing = kind.error(&app_state(cx).config_errors.clone()).is_some();
            if still_failing {
                // Open the next dialog after this one is gone.
                window.defer(cx, move |window, cx| {
                    open_config_error_dialog(kind, window, cx);
                });
            } else {
                Toast::success(format!("{} loaded", kind.file_name())).show(window, cx);
            }
        }
        ConfigDialogEnd::Continue => {
            Toast::warning(config_warning_text(kind))
                .sticky()
                .show(window, cx);
        }
    }
}

/// Re-read both configuration files (Retry): the layered config, the errors,
/// the settings derived from them and the view defaults for conversations
/// opened later are replaced ([`crate::settings::reload_config`]); the live
/// effects follow in [`crate::settings::apply_reloaded`].
pub fn reload_config(cx: &mut App) {
    crate::settings::reload_config(cx);
}

static CLIPBOARD_NOTICE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// `CLAUDE_HISTORY_CLIPBOARD` is ignored by the GUI (PD10, C-047): the Info
/// line for a set variable (`main` reads it; the line is logged after the
/// first frame, when the debug sink exists).
pub fn clipboard_env_notice(value: Option<&str>) -> Option<String> {
    value.map(|_| "CLAUDE_HISTORY_CLIPBOARD is ignored by the GUI".to_string())
}

/// Remember the notice for [`show_config_errors`].
pub fn note_clipboard_env(value: Option<&str>) {
    let notice = clipboard_env_notice(value);
    *CLIPBOARD_NOTICE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = notice;
}

fn take_clipboard_notice() -> Option<String> {
    CLIPBOARD_NOTICE
        .get()?
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

// ---------------------------------------------------------------------------
// Workspace (PD3)
// ---------------------------------------------------------------------------

/// The directory of the running executable (excluded as a workspace).
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Directories that are never a meaningful launch directory: the executable's
/// directory and, inside a macOS bundle, the `.app` directory.
fn excluded_dirs(exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let Some(dir) = exe_dir else {
        return Vec::new();
    };
    let mut excluded = vec![dir.to_path_buf()];
    for ancestor in dir.ancestors() {
        if ancestor.extension().is_some_and(|ext| ext == "app") {
            excluded.push(ancestor.to_path_buf());
        }
    }
    excluded
}

fn same_dir(a: &Path, b: &Path) -> bool {
    if same_path(a, b) {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => same_path(&a, &b),
        _ => false,
    }
}

/// PD3 rule 3 (a) and (b): the launch directory is *meaningful* when it is
/// not a filesystem root, not the home directory, not the executable's
/// directory (or `.app` bundle), and it maps to an existing Claude projects
/// directory or has a `.git` entry.
pub fn is_meaningful_cwd(cwd: &Path, env: &HistoryEnv, exe_dir: Option<&Path>) -> bool {
    if cwd.parent().is_none() || !cwd.is_dir() {
        return false;
    }
    if let Some(home) = &env.home_dir
        && same_dir(cwd, home)
    {
        return false;
    }
    if excluded_dirs(exe_dir)
        .iter()
        .any(|excluded| same_dir(cwd, excluded))
    {
        return false;
    }
    let has_projects_dir = env
        .claude_projects_dir_for(cwd)
        .is_ok_and(|projects| projects.is_dir());
    has_projects_dir || cwd.join(".git").exists()
}

fn scope_of(dir: &Path) -> Option<WorkspaceScope> {
    WorkspaceScope::new(dir)
        .ok()
        .filter(|scope| scope.dir().is_dir())
}

/// PD3 rules 1–3, the ones that need no GUI state (`--show-dir`, headless).
fn cli_workspace(
    args: &GuiLaunchArgs,
    launch_cwd: Option<&Path>,
    env: &HistoryEnv,
    exe_dir: Option<&Path>,
) -> Option<WorkspaceScope> {
    // 1. --workspace DIR (an existing directory).
    if let Ok(Some(scope)) = args.workspace_scope(launch_cwd)
        && scope.dir().is_dir()
    {
        return Some(scope);
    }
    // 2. -L / --local: the launch directory, whatever it is.
    if args.local
        && let Some(cwd) = launch_cwd
        && let Some(scope) = scope_of(cwd)
    {
        return Some(scope);
    }
    // 3. A meaningful launch directory.
    launch_cwd
        .filter(|cwd| is_meaningful_cwd(cwd, env, exe_dir))
        .and_then(scope_of)
}

/// The startup workspace (PD3), first match wins:
/// 1. `--workspace DIR` (existing; else a toast and the next rule);
/// 2. `--local` → the launch directory;
/// 3. a meaningful launch directory ([`is_meaningful_cwd`]);
/// 4. `[gui] default_workspace`;
/// 5. the last used workspace of state.json when `[gui] restore_session`;
/// 6. none.
///
/// Rule 3 (c), a directory recorded by Pi/OMP sessions, needs the loaded
/// corpus: [`workspace_from_recorded_cwd`] applies it after the first batch.
pub fn resolve_workspace(
    launch: &LaunchContext,
    env: &HistoryEnv,
    gui: &GuiSettings,
    ui: &UiState,
) -> Option<WorkspaceScope> {
    resolve_workspace_with(launch, env, gui, ui, exe_dir().as_deref())
}

/// [`resolve_workspace`] with an explicit executable directory.
pub fn resolve_workspace_with(
    launch: &LaunchContext,
    env: &HistoryEnv,
    gui: &GuiSettings,
    ui: &UiState,
    exe_dir: Option<&Path>,
) -> Option<WorkspaceScope> {
    if let Some(scope) = cli_workspace(&launch.args, launch.launch_cwd.as_deref(), env, exe_dir) {
        return Some(scope);
    }
    if let Ok(Some(scope)) = gui.default_workspace_scope()
        && scope.dir().is_dir()
    {
        return Some(scope);
    }
    if gui.restore_session()
        && let Some(dir) = &ui.workspace
        && let Some(scope) = scope_of(dir)
    {
        return Some(scope);
    }
    None
}

/// PD3 rule 3 (c): after the first load batch, when no workspace was chosen
/// by rules 1–5 and the launch directory is not excluded (root, home, the
/// executable's directory) and equals the cwd recorded by at least one Pi/OMP
/// session, that directory becomes the workspace.
pub fn workspace_from_recorded_cwd<'a>(
    launch_cwd: Option<&Path>,
    current: Option<&WorkspaceScope>,
    env: &HistoryEnv,
    recorded: impl IntoIterator<Item = &'a Path>,
) -> Option<WorkspaceScope> {
    workspace_from_recorded_cwd_with(launch_cwd, current, env, recorded, exe_dir().as_deref())
}

/// [`workspace_from_recorded_cwd`] with an explicit executable directory.
pub fn workspace_from_recorded_cwd_with<'a>(
    launch_cwd: Option<&Path>,
    current: Option<&WorkspaceScope>,
    env: &HistoryEnv,
    recorded: impl IntoIterator<Item = &'a Path>,
    exe_dir: Option<&Path>,
) -> Option<WorkspaceScope> {
    if current.is_some() {
        return None;
    }
    let cwd = launch_cwd?;
    if cwd.parent().is_none() || !cwd.is_dir() {
        return None;
    }
    if env
        .home_dir
        .as_deref()
        .is_some_and(|home| same_dir(cwd, home))
    {
        return None;
    }
    if excluded_dirs(exe_dir).iter().any(|dir| same_dir(cwd, dir)) {
        return None;
    }
    recorded
        .into_iter()
        .any(|path| same_dir(cwd, path))
        .then(|| scope_of(cwd))
        .flatten()
}

/// The app-level entry for PD3 rule 3 (c): the list calls this once after its
/// first load batch with the cwds the batch's Pi/OMP sessions recorded. When
/// the rule picks a workspace, `AppState` takes it and `true` is returned (the
/// caller then re-scopes and reloads its list); otherwise nothing changes.
pub fn apply_recorded_cwd_workspace<'a>(
    cx: &mut App,
    recorded: impl IntoIterator<Item = &'a Path>,
) -> bool {
    let state = app_state(cx);
    let Some(scope) = workspace_from_recorded_cwd(
        state.launch.launch_cwd.as_deref(),
        state.workspace.as_ref(),
        &state.env,
        recorded,
    ) else {
        return false;
    };
    update_app_state(cx, |state, _| state.set_workspace(Some(scope)));
    true
}

// ---------------------------------------------------------------------------
// Initial view (PD17, C-066)
// ---------------------------------------------------------------------------

/// The initial view preferences and list mode, per field (§11.1 table):
///
/// | Field | 1st | 2nd | 3rd (only with `restore_session`) | default |
/// |---|---|---|---|---|
/// | tool display | `--show-tools`/`--no-tools` | `[display] no_tools` | state `view.tool_display` | Summary |
/// | thinking | `--show-thinking`/`--hide-thinking` | `[display] show_thinking` | state `view.show_thinking` | off |
/// | timing | – | – | state `view.show_timing` | off |
/// | list mode | `--semantic`/`--lexical` | `[search] mode` / `[tui] semantic_search` | state `search_mode` | Lexical |
///
/// `settings` already folds the first two layers (core `effective_settings`);
/// this function adds the remembered state where they are silent.
pub fn resolve_initial_view(
    launch: &LaunchContext,
    config: &LayeredConfig,
    settings: &EffectiveSettings,
    ui: &UiState,
) -> (ViewPrefs, ListSearchMode) {
    let args = &launch.args;
    let restore = config.gui.restore_session();
    let display = config.effective.display.clone().unwrap_or_default();
    let remembered = |pick: &dyn Fn(&UiState) -> bool| restore && pick(ui);

    let tool_display = if args.show_tools || args.no_tools || display.no_tools.is_some() {
        settings.tool_display.into()
    } else if remembered(&|ui| ui.view.tool_display.is_some()) {
        match ui.view.tool_display {
            Some(RememberedToolDisplay::Full) => ToolDisplayMode::Full,
            Some(RememberedToolDisplay::Truncated) => ToolDisplayMode::Truncated,
            _ => ToolDisplayMode::Hidden,
        }
    } else {
        settings.tool_display.into()
    };

    let show_thinking =
        if args.show_thinking || args.hide_thinking || display.show_thinking.is_some() {
            settings.show_thinking
        } else if remembered(&|ui| ui.view.show_thinking.is_some()) {
            ui.view.show_thinking.unwrap_or(false)
        } else {
            settings.show_thinking
        };

    let show_timing = restore && ui.view.show_timing.unwrap_or(false);

    let search = config.effective.search.clone().unwrap_or_default();
    let tui = config.effective.tui.clone().unwrap_or_default();
    let mode_from_flags_or_config =
        args.semantic || args.lexical || search.mode.is_some() || tui.semantic_search.is_some();
    let list_mode = if mode_from_flags_or_config {
        settings.list_search_mode.into()
    } else {
        match ui.search_mode.filter(|_| restore) {
            Some(RememberedSearchMode::Semantic) => ListSearchMode::Semantic,
            Some(RememberedSearchMode::Lexical) => ListSearchMode::Lexical,
            None => settings.list_search_mode.into(),
        }
    };

    (
        ViewPrefs {
            tool_display,
            show_thinking,
            show_timing,
        },
        list_mode,
    )
}

// ---------------------------------------------------------------------------
// Startup tools (AD14)
// ---------------------------------------------------------------------------

/// The `tools::*` entry point a startup tool calls (tests assert the mapping).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolEntry {
    ShowProjectsDirectory,
    DeleteById {
        prefill: String,
    },
    DebugSearch {
        query: String,
        local: bool,
    },
    SemanticSearch {
        query: String,
        top: usize,
        local: bool,
    },
    SemanticDebug {
        query: String,
        local: bool,
    },
    GenerateSemanticCache {
        local: bool,
    },
    ClearSemanticCache,
    DeleteEmpty {
        local: bool,
    },
    CheckForUpdates,
}

/// Which entry point opens `tool`.
pub fn tool_entry(tool: &StartupTool) -> ToolEntry {
    match tool.clone() {
        StartupTool::ShowDir => ToolEntry::ShowProjectsDirectory,
        StartupTool::DeleteSession(id) => ToolEntry::DeleteById { prefill: id },
        StartupTool::DebugSearch { query, local } => ToolEntry::DebugSearch { query, local },
        StartupTool::SemanticSearch { query, top, local } => {
            ToolEntry::SemanticSearch { query, top, local }
        }
        StartupTool::SemanticDebug { query, local } => ToolEntry::SemanticDebug { query, local },
        StartupTool::GenerateSemanticCache { local } => ToolEntry::GenerateSemanticCache { local },
        StartupTool::ClearSemanticCache => ToolEntry::ClearSemanticCache,
        StartupTool::DeleteEmpty { local } => ToolEntry::DeleteEmpty { local },
        StartupTool::CheckForUpdates => ToolEntry::CheckForUpdates,
    }
}

/// Open the Tools dialog of a one-shot flag (AD14), after the first frame.
pub fn run_startup_tool(tool: StartupTool, window: &mut Window, cx: &mut App) {
    use crate::model::SemanticReportMode;
    use crate::tools;
    match tool_entry(&tool) {
        ToolEntry::ShowProjectsDirectory => tools::show_projects_directory(window, cx),
        ToolEntry::DeleteById { prefill } => tools::open_delete_by_id(Some(prefill), window, cx),
        ToolEntry::DebugSearch { query, local } => {
            tools::open_debug_search(Some(query), local, window, cx)
        }
        ToolEntry::SemanticSearch { query, top, local } => tools::open_semantic_debug(
            Some(query),
            SemanticReportMode::Search { top },
            local,
            window,
            cx,
        ),
        ToolEntry::SemanticDebug { query, local } => tools::open_semantic_debug(
            Some(query),
            SemanticReportMode::DebugDump,
            local,
            window,
            cx,
        ),
        ToolEntry::GenerateSemanticCache { local } => {
            tools::open_generate_semantic_cache(local, window, cx)
        }
        ToolEntry::ClearSemanticCache => tools::open_clear_semantic_cache(window, cx),
        ToolEntry::DeleteEmpty { local } => tools::open_delete_empty(local, window, cx),
        ToolEntry::CheckForUpdates => tools::check_for_updates(window, cx),
    }
}

// ---------------------------------------------------------------------------
// Login-shell PATH (§14.2)
// ---------------------------------------------------------------------------

/// Directories added after the login shell's PATH when they exist.
const EXTRA_PATH_DIRS: [&str; 5] = [
    ".local/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    ".npm-global/bin",
    ".bun/bin",
];

/// Prepend the login shell's PATH entries (deduplicated) and the usual tool
/// directories that exist to `existing`; later duplicates are dropped.
pub fn augment_path(
    existing: Option<&OsStr>,
    login: &OsStr,
    home: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> OsString {
    let mut entries: Vec<PathBuf> = std::env::split_paths(login).collect();
    for extra in EXTRA_PATH_DIRS {
        let dir = if extra.starts_with('/') {
            PathBuf::from(extra)
        } else if let Some(home) = home {
            home.join(extra)
        } else {
            continue;
        };
        if exists(&dir) {
            entries.push(dir);
        }
    }
    if let Some(existing) = existing {
        entries.extend(std::env::split_paths(existing));
    }
    let mut seen = Vec::<PathBuf>::new();
    entries.retain(|entry| {
        if entry.as_os_str().is_empty() || seen.contains(entry) {
            false
        } else {
            seen.push(entry.clone());
            true
        }
    });
    std::env::join_paths(entries)
        .unwrap_or_else(|_| existing.map(OsStr::to_os_string).unwrap_or_default())
}

/// Whether the probe runs: macOS/Linux, and not when started from a terminal
/// (stdin is a TTY: the shell's PATH is already ours).
pub fn should_probe_login_shell(is_unix: bool, stdin_is_tty: bool) -> bool {
    is_unix && !stdin_is_tty
}

#[derive(Default)]
struct ProbeState {
    finished: bool,
    path: Option<OsString>,
}

/// Lets a background task wait for the probe (resume waits ≤ 3 s, §14.2).
#[derive(Clone)]
pub struct LoginShellWaiter {
    shared: Arc<(Mutex<ProbeState>, Condvar)>,
}

impl LoginShellWaiter {
    /// Block up to `timeout` for the probe; the merged PATH when it found
    /// one (`None` when it failed or is still running).
    pub fn wait(&self, timeout: Duration) -> Option<OsString> {
        let (lock, cvar) = &*self.shared;
        let deadline = Instant::now() + timeout;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !state.finished {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = cvar
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        state.path.clone()
    }

    /// Whether the probe has finished (successfully or not).
    pub fn is_finished(&self) -> bool {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished
    }
}

/// The running probe (a global); the task is stored here, not detached.
pub struct LoginShellProbe {
    waiter: LoginShellWaiter,
    _task: Task<()>,
}

impl Global for LoginShellProbe {}

/// The waiter of the running probe, `None` when no probe was started (resume
/// then has nothing to wait for).
pub fn login_shell_waiter(cx: &App) -> Option<LoginShellWaiter> {
    cx.try_global::<LoginShellProbe>()
        .map(|probe| probe.waiter.clone())
}

/// macOS/Linux: start the login-shell PATH probe when not started from a
/// terminal (§14.2), after the first frame.
pub fn start_login_shell_probe(cx: &mut App) {
    let tty = services(cx).stdin_is_tty();
    if should_probe_login_shell(cfg!(unix), tty) {
        run_login_shell_probe(cx);
    }
}

/// Run the probe on a `login-shell-path` thread; the merged PATH replaces
/// `AppState.env.path_var` and releases the waiters. A failure leaves PATH
/// unchanged (a debug line).
pub fn run_login_shell_probe(cx: &mut App) {
    if cx.try_global::<LoginShellProbe>().is_some() {
        return;
    }
    let services = services(cx);
    let state = app_state(cx);
    let existing = state.env.path_var.clone();
    let home = state.env.home_dir.clone();
    let shared = Arc::new((Mutex::new(ProbeState::default()), Condvar::new()));
    let waiter = LoginShellWaiter {
        shared: shared.clone(),
    };
    let (sender, receiver) = async_channel::bounded::<Option<OsString>>(1);
    let thread_shared = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("login-shell-path".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let merged = services.login_shell_path().map(|login| {
                augment_path(existing.as_deref(), &login, home.as_deref(), |dir| {
                    dir.is_dir()
                })
            });
            {
                let (lock, cvar) = &*thread_shared;
                let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
                state.finished = true;
                state.path = merged.clone();
                cvar.notify_all();
            }
            let _ = sender.send_blocking(merged);
        });
    if spawned.is_err() {
        let (lock, cvar) = &*shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).finished = true;
        cvar.notify_all();
        return;
    }
    let task = cx.spawn(async move |cx| match receiver.recv().await {
        Ok(Some(path)) => {
            cx.update(|cx| {
                update_app_state(cx, |state, _| {
                    Arc::make_mut(&mut state.env).path_var = Some(path.clone());
                });
            });
            debug::info(
                Some(DebugLevel::Info),
                "login-shell PATH merged into the program search path",
            );
        }
        _ => debug::warn(
            Some(DebugLevel::Info),
            "login-shell PATH probe failed; PATH is unchanged",
        ),
    });
    cx.set_global(LoginShellProbe {
        waiter,
        _task: task,
    });
}

#[cfg(test)]
mod tests;
