//! Opening a terminal window that runs a resume/fork command (W10).
//!
//! Upstream `exec`ed `claude`/`pi`/`omp` in its own terminal. A GUI has no
//! terminal, so it opens one. Everything here except [`spawn_terminal`] is
//! pure: [`build_terminal_command`] turns a [`LaunchSpec`] (resolved program,
//! arguments, working directory) and a [`TerminalLauncher`] into the exact
//! [`TerminalCommand`] to spawn, for any [`Platform`], so the quoting can be
//! tested everywhere.
//!
//! Built-in launchers (chosen by [`detect_launcher`] or named in `gui.toml`,
//! see [`TerminalLauncher::from_setting`]):
//!
//! | Platform | Launcher | Command |
//! |---|---|---|
//! | Windows | Windows Terminal | `wt.exe -w new -d <cwd> <sys>\cmd.exe /S /K "<command>"` |
//! | Windows | console (fallback) | `<sys>\cmd.exe /C start "" /D "<cwd>" "<sys>\cmd.exe" /S /K "<command>"` |
//! | macOS | Terminal.app | a temporary `.command` script, `open -a Terminal <script>` |
//! | macOS | AppleScript | `osascript -e 'tell application "Terminal" ... do script "<line>"'` |
//! | Linux | `$TERMINAL` | `$TERMINAL -e sh -c <line>` |
//! | Linux | x-terminal-emulator, gnome-terminal, ptyxis, kgx, konsole, xfce4-terminal, tilix, alacritty, kitty, wezterm, xterm | the emulator's own "execute" syntax with `sh -c <line>` |
//!
//! [`launcher_candidates`] lists the launchers to try in order (the
//! configured one, else every detected one), and [`spawn_terminal_checked`]
//! reports a launcher that exits at once with an error code.
//!
//! The spawn functions refuse under `cfg(test)` and the `hermetic` feature:
//! tests only build commands.
//!
//! On Unix `<line>` is `cd -- '<cwd>' && '<program>' '<arg>'...`, followed by
//! `; exec "${SHELL:-/bin/sh}"` when [`LaunchOptions::keep_open`] is set, so
//! the window drops into a shell when the agent exits (like returning to the
//! shell after upstream's `exec`). On Windows `<command>` is a `cmd.exe`
//! command line and `/K` keeps the window open (`/C` without `keep_open`).
//!
//! # Windows program lookup
//!
//! `cmd.exe`, and its `start`, look a bare program name up in the current
//! directory before `PATH`. The session or workspace directory may be an
//! untrusted repository with a planted `cmd.exe` (or `cmd.bat`), so:
//!
//! * `cmd.exe` is always named by its absolute path in the system directory
//!   (`<sys>`, [`crate::program::windows_cmd_exe`]): in the built-in
//!   launchers, in `{args}` for batch scripts, and for every bare `cmd` /
//!   `cmd.exe` word of a template;
//! * the built-in launchers place the window with `-d` / `start /D` and
//!   run in the system directory themselves, never in the session
//!   directory ([`TerminalCommand::cwd`]); so does a template run by `cmd`
//!   that places the directory itself (`{cwd}`, or `{cmd}` with its
//!   `cd /d`).
//!
//! Upstream ran `Command::new("claude")`, whose lookup never searches the
//! working directory, so it had no such exposure.
//!
//! # UNC working directories
//!
//! `cmd.exe` cannot use a UNC directory (`\\server\share\...`, any path
//! starting with two slashes) as its current directory: it falls back to
//! the Windows directory, and `cd /d` to one fails. Both built-in Windows
//! launchers run the program through `cmd.exe`, so they refuse a UNC
//! working directory with an error instead of resuming in the wrong
//! project. So does a template that runs `cmd` (a `cmd` word, `{cmd}`, or
//! `{args}` inside a word) and a batch script (`claude.cmd`, run through
//! `cmd /C`). A template that starts a native executable directly, such as
//! `wt -d {cwd} {args}`, works (upstream spawned the program directly).
//!
//! # Windows quoting
//!
//! `<command>` is built so that `cmd.exe` hands the program exactly the
//! original arguments:
//!
//! * the program path is wrapped in double quotes (Windows paths cannot
//!   contain `"`);
//! * each argument is quoted with the MSVC (`CommandLineToArgvW`) rules,
//!   and also when it contains a `cmd.exe` special character, so that batch
//!   shims (`claude.cmd`) that re-expand `%*` see it quoted;
//! * every `cmd.exe` metacharacter in the arguments, including the quotes, is
//!   escaped with `^`, so `cmd.exe` never interprets `& | < > ( ) ^ " % !`;
//! * the console fallback passes through two `cmd.exe` layers and therefore
//!   escapes twice; for Windows Terminal, `;` (its command separator) is
//!   written as `\;`, which it unescapes.
//!
//! This was checked against the real `cmd.exe` (one and two layers, and a
//! `.cmd` shim forwarding `%*`) with spaces, quotes, trailing backslashes,
//! `& | < > ^ ( ) % ! ; , =`, defined `%VARIABLES%`, empty arguments and
//! non-ASCII text. Arguments containing line breaks or NUL cannot be passed
//! through `cmd.exe` and are rejected. A `%` inside the program path or the
//! working directory is not escaped (it cannot be, inside quotes). A batch
//! shim re-parses `%*` itself, so an argument that contains both a `"` and a
//! `cmd.exe` metacharacter may still be split by the shim (an inherent
//! limitation of batch files; such `default_args` are unusual).
//!
//! # Templates
//!
//! `gui.toml` `[gui].terminal` may hold a command template instead of a
//! launcher name. It is split into words at whitespace (`"..."` and `'...'`
//! group words, there are no backslash escapes), then:
//!
//! * `{cwd}` is replaced by the working directory;
//! * a word that is exactly `{args}` becomes the program and its arguments
//!   as separate words (a Windows batch script as `<sys>\cmd.exe /C <script>
//!   ...`, which [`ResolvedProgram::argv`] refuses for arguments `cmd /C` would
//!   mangle: spaces, quotes, `& | < > ^ ( ) % !`);
//! * `{cmd}` is replaced by the command as one shell string: on Unix the
//!   `<line>` above (for `sh -c {cmd}`), on Windows `"cd /d "<cwd>" &&
//!   <command>"` for a `cmd /S /K {cmd}` run by a template whose program is
//!   `cmd`;
//! * without `{args}` or `{cmd}` the program and arguments are appended.
//!
//! On Windows the expanded words form a raw command line. A word holding
//! `{cwd}` is always quoted, so `cmd.exe` keeps `& | < > ^ ( )` in a
//! directory name literal. When the template's program is `cmd` (or
//! `cmd.exe`), which reads its command line with `cmd.exe` rules rather than
//! the MSVC rules, the program and arguments are also `^`-escaped like the
//! built-in launchers, and a `{cmd}` that follows a nested `cmd` (`cmd /C
//! start ... cmd /S /K {cmd}`) is escaped once more for the outer `cmd.exe`.
//! A `%` in the directory is not escaped (see above). Every bare `cmd` or
//! `cmd.exe` word becomes `<sys>\cmd.exe`, and a `cmd` template that places
//! the directory itself runs in the system directory (see "Windows program
//! lookup"); other templates run in the session directory, as some
//! terminals start there.
//!
//! Examples: `kitty --directory {cwd} {args}`, `alacritty
//! --working-directory {cwd} -e sh -c {cmd}`, `wezterm start --cwd {cwd} --
//! {args}`, `cmd /S /K {cmd}`, `cmd /C start "" /D {cwd} cmd /S /K {cmd}`.

use crate::config::GuiSettings;
use crate::env::HistoryEnv;
use crate::error::{AppError, Result};
use crate::program::{ResolvedProgram, find_program, windows_cmd_exe, windows_system_dir};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Target platform of a terminal command (the build functions take it as a
/// parameter so every platform can be tested on any host).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    MacOs,
    /// Linux and other Unix-like systems.
    Linux,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }

    pub fn is_windows(self) -> bool {
        self == Platform::Windows
    }
}

/// What has to run inside the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: ResolvedProgram,
    pub args: Vec<OsString>,
    /// Working directory (`None`: the terminal's default).
    pub cwd: Option<PathBuf>,
}

/// Linux terminal emulators, in detection order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinuxTerminal {
    XTerminalEmulator,
    GnomeTerminal,
    /// GNOME's Ptyxis (the default terminal of recent Fedora and Ubuntu).
    Ptyxis,
    /// GNOME Console (`kgx`).
    Kgx,
    Konsole,
    Xfce4Terminal,
    Tilix,
    Alacritty,
    Kitty,
    Wezterm,
    Xterm,
}

impl LinuxTerminal {
    /// Detection order (after `$TERMINAL`).
    pub const ALL: [LinuxTerminal; 11] = [
        LinuxTerminal::XTerminalEmulator,
        LinuxTerminal::GnomeTerminal,
        LinuxTerminal::Ptyxis,
        LinuxTerminal::Kgx,
        LinuxTerminal::Konsole,
        LinuxTerminal::Xfce4Terminal,
        LinuxTerminal::Tilix,
        LinuxTerminal::Alacritty,
        LinuxTerminal::Kitty,
        LinuxTerminal::Wezterm,
        LinuxTerminal::Xterm,
    ];

    /// The executable name.
    pub fn program(self) -> &'static str {
        match self {
            LinuxTerminal::XTerminalEmulator => "x-terminal-emulator",
            LinuxTerminal::GnomeTerminal => "gnome-terminal",
            LinuxTerminal::Ptyxis => "ptyxis",
            LinuxTerminal::Kgx => "kgx",
            LinuxTerminal::Konsole => "konsole",
            LinuxTerminal::Xfce4Terminal => "xfce4-terminal",
            LinuxTerminal::Tilix => "tilix",
            LinuxTerminal::Alacritty => "alacritty",
            LinuxTerminal::Kitty => "kitty",
            LinuxTerminal::Wezterm => "wezterm",
            LinuxTerminal::Xterm => "xterm",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|terminal| terminal.program().eq_ignore_ascii_case(name))
    }

    /// Arguments before the command, including the working directory when
    /// the emulator has an option for it.
    fn prefix(self, cwd: Option<&str>) -> Vec<String> {
        let mut args = Vec::new();
        match self {
            LinuxTerminal::XTerminalEmulator | LinuxTerminal::Xterm => args.push("-e".into()),
            LinuxTerminal::GnomeTerminal => {
                if let Some(cwd) = cwd {
                    args.push(format!("--working-directory={cwd}"));
                }
                args.push("--".into());
            }
            LinuxTerminal::Ptyxis => {
                // A window of its own, like the other launchers.
                args.push("--new-window".into());
                if let Some(cwd) = cwd {
                    args.push(format!("--working-directory={cwd}"));
                }
                args.push("--".into());
            }
            LinuxTerminal::Kgx => {
                if let Some(cwd) = cwd {
                    args.push(format!("--working-directory={cwd}"));
                }
                args.push("--".into());
            }
            LinuxTerminal::Tilix => {
                if let Some(cwd) = cwd {
                    args.push(format!("--working-directory={cwd}"));
                }
                // -x: execute the remainder of the command line.
                args.push("-x".into());
            }
            LinuxTerminal::Konsole => {
                if let Some(cwd) = cwd {
                    args.extend(["--workdir".into(), cwd.to_string()]);
                }
                args.push("-e".into());
            }
            LinuxTerminal::Xfce4Terminal => {
                if let Some(cwd) = cwd {
                    args.push(format!("--working-directory={cwd}"));
                }
                // -x: execute the remainder of the command line.
                args.push("-x".into());
            }
            LinuxTerminal::Alacritty => {
                if let Some(cwd) = cwd {
                    args.extend(["--working-directory".into(), cwd.to_string()]);
                }
                args.push("-e".into());
            }
            LinuxTerminal::Kitty => {
                if let Some(cwd) = cwd {
                    args.extend(["--directory".into(), cwd.to_string()]);
                }
            }
            LinuxTerminal::Wezterm => {
                args.push("start".into());
                if let Some(cwd) = cwd {
                    args.extend(["--cwd".into(), cwd.to_string()]);
                }
                args.push("--".into());
            }
        }
        args
    }
}

/// How to open a terminal window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalLauncher {
    /// Windows Terminal (`wt.exe`, normally an app execution alias).
    WindowsTerminal { program: PathBuf },
    /// A classic console window via `cmd.exe /C start`.
    WindowsConsole,
    /// macOS Terminal.app running a temporary `.command` script.
    MacTerminal,
    /// macOS Terminal.app driven through `osascript`.
    MacOsascript,
    /// `$TERMINAL` (program plus optional leading arguments) with `-e`.
    EnvTerminal { program: PathBuf, args: Vec<String> },
    /// A known terminal emulator.
    Linux {
        terminal: LinuxTerminal,
        program: PathBuf,
    },
    /// A user template (see the module docs).
    Template(String),
}

impl TerminalLauncher {
    /// Interpret `gui.toml` `[gui].terminal`: `"auto"` (or empty) means
    /// detect ([`None`]); a launcher name selects it (`wt`,
    /// `windows-terminal`, `cmd`, `terminal`, `osascript`,
    /// `x-terminal-emulator`, `gnome-terminal`, `ptyxis`, `kgx`, `konsole`,
    /// `xfce4-terminal`, `tilix`, `alacritty`, `kitty`, `wezterm`, `xterm`;
    /// case-insensitive); anything else is a template.
    pub fn from_setting(value: &str) -> Option<TerminalLauncher> {
        let value = value.trim();
        let lower = value.to_ascii_lowercase();
        Some(match lower.as_str() {
            "" | "auto" => return None,
            "wt" | "wt.exe" | "windows-terminal" | "windowsterminal" => {
                TerminalLauncher::WindowsTerminal {
                    program: PathBuf::from("wt.exe"),
                }
            }
            "cmd" | "cmd.exe" | "console" | "conhost" => TerminalLauncher::WindowsConsole,
            "terminal" | "terminal.app" => TerminalLauncher::MacTerminal,
            "osascript" => TerminalLauncher::MacOsascript,
            name => match LinuxTerminal::from_name(name) {
                Some(terminal) => TerminalLauncher::Linux {
                    terminal,
                    program: PathBuf::from(terminal.program()),
                },
                None => TerminalLauncher::Template(value.to_string()),
            },
        })
    }

    /// A short name for messages ("Windows Terminal", "kitty", ...).
    pub fn name(&self) -> String {
        match self {
            TerminalLauncher::WindowsTerminal { .. } => "Windows Terminal".to_string(),
            TerminalLauncher::WindowsConsole => "Command Prompt".to_string(),
            TerminalLauncher::MacTerminal | TerminalLauncher::MacOsascript => {
                "Terminal".to_string()
            }
            TerminalLauncher::EnvTerminal { program, .. } => program
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.display().to_string()),
            TerminalLauncher::Linux { terminal, .. } => terminal.program().to_string(),
            TerminalLauncher::Template(_) => "custom terminal".to_string(),
        }
    }
}

/// Pick the default launcher for `platform`: the first of
/// [`detected_launchers`].
///
/// * Windows: Windows Terminal when `wt` is on `PATH`, else the console.
/// * macOS: Terminal.app.
/// * Linux: `$TERMINAL` (`terminal_var`) when its program is found, then the
///   first [`LinuxTerminal`] found on `PATH`; `None` when there is none.
pub fn detect_launcher(
    env: &HistoryEnv,
    platform: Platform,
    terminal_var: Option<&OsStr>,
) -> Option<TerminalLauncher> {
    detected_launchers(env, platform, terminal_var)
        .into_iter()
        .next()
}

/// Every usable launcher for `platform`, in preference order:
///
/// * Windows: Windows Terminal when `wt` is on `PATH`, then the console;
/// * macOS: Terminal.app with a launch script, then through `osascript`;
/// * Linux: `$TERMINAL` (`terminal_var`) when its program is found, then
///   every [`LinuxTerminal`] found on `PATH`, in [`LinuxTerminal::ALL`]
///   order.
pub fn detected_launchers(
    env: &HistoryEnv,
    platform: Platform,
    terminal_var: Option<&OsStr>,
) -> Vec<TerminalLauncher> {
    let find = |name: &str| {
        find_program(
            name,
            env.path_var.as_deref(),
            env.pathext.as_deref(),
            platform.is_windows(),
        )
        .map(|program| program.path)
    };
    match platform {
        Platform::Windows => find("wt")
            .map(|program| TerminalLauncher::WindowsTerminal { program })
            .into_iter()
            .chain([TerminalLauncher::WindowsConsole])
            .collect(),
        Platform::MacOs => vec![
            TerminalLauncher::MacTerminal,
            TerminalLauncher::MacOsascript,
        ],
        Platform::Linux => {
            let mut launchers = Vec::new();
            if let Some(value) = terminal_var.and_then(OsStr::to_str) {
                let mut words = value.split_whitespace();
                if let Some(program) = words.next().and_then(find) {
                    launchers.push(TerminalLauncher::EnvTerminal {
                        program,
                        args: words.map(str::to_string).collect(),
                    });
                }
            }
            launchers.extend(LinuxTerminal::ALL.into_iter().filter_map(|terminal| {
                find(terminal.program())
                    .map(|program| TerminalLauncher::Linux { terminal, program })
            }));
            launchers
        }
    }
}

/// The launchers to try, in order, for a resume or fork (K17): the one
/// configured in `options.terminal` (a name or a template) alone, else
/// every detected launcher ([`detected_launchers`]). A GUI builds each
/// command with [`build_terminal_command`] and moves on to the next
/// launcher when spawning fails. Empty when nothing is configured and no
/// terminal is found (Linux).
pub fn launcher_candidates(env: &HistoryEnv, options: &LaunchOptions) -> Vec<TerminalLauncher> {
    match options
        .terminal
        .as_deref()
        .and_then(TerminalLauncher::from_setting)
    {
        Some(launcher) => vec![launcher],
        None => detected_launchers(env, options.platform, options.terminal_var.as_deref()),
    }
}

/// Arguments of a [`TerminalCommand`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalArgs {
    /// Separate arguments (quoted by the OS / standard library as usual).
    Argv(Vec<OsString>),
    /// A complete Windows command-line tail, passed verbatim
    /// (`CommandExt::raw_arg`), because `cmd.exe` and `wt.exe` do not parse
    /// their command line with the MSVC rules.
    WindowsRaw(String),
}

/// A script file to write (executable) before spawning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchScript {
    pub path: PathBuf,
    pub contents: String,
}

/// A fully constructed terminal launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCommand {
    /// Launcher name for messages.
    pub launcher: String,
    pub program: OsString,
    pub args: TerminalArgs,
    /// Working directory of the spawned process. Usually the session's
    /// directory; the system directory for the Windows `cmd.exe` and
    /// Windows Terminal launchers, which get the session directory as an
    /// argument instead (see "Windows program lookup" in the module docs).
    pub cwd: Option<PathBuf>,
    /// Script to write first (macOS Terminal.app).
    pub script: Option<LaunchScript>,
    /// Windows: start the helper process without a console window
    /// (`CREATE_NO_WINDOW`).
    pub hide_console: bool,
}

impl TerminalCommand {
    /// The command for logs and error messages.
    pub fn display(&self) -> String {
        let program = self.program.to_string_lossy();
        match &self.args {
            TerminalArgs::WindowsRaw(raw) => format!("{program} {raw}"),
            TerminalArgs::Argv(args) => std::iter::once(quote_posix(&program))
                .chain(args.iter().map(|arg| quote_posix(&arg.to_string_lossy())))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// Options of [`build_terminal_command`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildOptions {
    pub platform: Platform,
    /// Keep the window open after the program exits.
    pub keep_open: bool,
    /// Where [`TerminalLauncher::MacTerminal`] writes its script.
    pub script_path: Option<PathBuf>,
}

/// Options of [`prepare_terminal_command`] / [`super::launch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    pub platform: Platform,
    /// `gui.toml` `[gui].terminal`: a launcher name or a template
    /// ([`TerminalLauncher::from_setting`]); `None` detects.
    pub terminal: Option<String>,
    /// The value of `$TERMINAL` (Linux), read by the GUI from its own
    /// environment (core never reads process environment variables).
    pub terminal_var: Option<OsString>,
    /// Keep the window open after the program exits (default `true`).
    pub keep_open: bool,
    /// Directory for temporary launch scripts (macOS). Default:
    /// `<cache root>/launch`.
    pub script_dir: Option<PathBuf>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            platform: Platform::current(),
            terminal: None,
            terminal_var: None,
            keep_open: true,
            script_dir: None,
        }
    }
}

impl LaunchOptions {
    /// Options with the `[gui].terminal` setting applied.
    pub fn from_gui_settings(settings: &GuiSettings) -> Self {
        Self {
            terminal: settings.terminal.clone(),
            ..Self::default()
        }
    }
}

/// Choose the launcher (setting or detection) and build the command.
///
/// A missing working directory defaults to the home directory.
pub fn prepare_terminal_command(
    env: &HistoryEnv,
    spec: &LaunchSpec,
    options: &LaunchOptions,
) -> Result<TerminalCommand> {
    let launcher = match options
        .terminal
        .as_deref()
        .and_then(TerminalLauncher::from_setting)
    {
        Some(launcher) => launcher,
        None => detect_launcher(env, options.platform, options.terminal_var.as_deref())
            .ok_or_else(|| {
                AppError::TerminalLaunch(
                    "no terminal emulator found; set [gui].terminal in gui.toml".to_string(),
                )
            })?,
    };
    prepare_launcher_command(env, &launcher, spec, options)
}

/// Build the command for one already-chosen `launcher` (a candidate of
/// [`launcher_candidates`]): a missing working directory defaults to the home
/// directory and the macOS Terminal launcher gets a fresh script path under
/// `options.script_dir` (default `<cache root>/launch`).
pub fn prepare_launcher_command(
    env: &HistoryEnv,
    launcher: &TerminalLauncher,
    spec: &LaunchSpec,
    options: &LaunchOptions,
) -> Result<TerminalCommand> {
    let mut spec = spec.clone();
    if spec.cwd.is_none() {
        spec.cwd = env.home_dir.clone();
    }
    let script_path = (*launcher == TerminalLauncher::MacTerminal).then(|| {
        let dir = options
            .script_dir
            .clone()
            .or_else(|| env.cache_root().map(|root| root.join("launch")))
            .unwrap_or_else(std::env::temp_dir);
        dir.join(format!("resume-{}.command", unique_suffix()))
    });
    build_terminal_command(
        launcher,
        &spec,
        &BuildOptions {
            platform: options.platform,
            keep_open: options.keep_open,
            script_path,
        },
    )
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{nanos:x}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn launch_error(message: impl Into<String>) -> AppError {
    AppError::TerminalLaunch(message.into())
}

fn utf8<'a>(value: &'a OsStr, what: &str) -> Result<&'a str> {
    value.to_str().ok_or_else(|| {
        launch_error(format!(
            "{what} is not valid Unicode: {}",
            value.to_string_lossy()
        ))
    })
}

/// The program and arguments as UTF-8 strings.
fn spec_strings(spec: &LaunchSpec) -> Result<(String, Vec<String>, Option<String>)> {
    let program = utf8(spec.program.path.as_os_str(), "program path")?.to_string();
    let args = spec
        .args
        .iter()
        .map(|arg| utf8(arg, "argument").map(str::to_string))
        .collect::<Result<Vec<_>>>()?;
    let cwd = spec
        .cwd
        .as_ref()
        .map(|cwd| utf8(cwd.as_os_str(), "working directory").map(str::to_string))
        .transpose()?;
    Ok((program, args, cwd))
}

/// Build the command that opens a terminal running `spec`.
pub fn build_terminal_command(
    launcher: &TerminalLauncher,
    spec: &LaunchSpec,
    options: &BuildOptions,
) -> Result<TerminalCommand> {
    let (program, args, cwd) = spec_strings(spec)?;
    let name = launcher.name();
    let command = |program: OsString, args: TerminalArgs| TerminalCommand {
        launcher: name.clone(),
        program,
        args,
        cwd: spec.cwd.clone(),
        script: None,
        hide_console: false,
    };
    match launcher {
        TerminalLauncher::WindowsTerminal { program: wt } => {
            let inner = windows_cmd_command(&program, &args)?;
            let shell = system_cmd_text()?;
            // `-w new`: always a new window, never a tab of a window that
            // happens to be open.
            let mut raw = vec!["-w".to_string(), "new".to_string()];
            if let Some(cwd) = &cwd {
                reject_cmd_unsafe(cwd, "working directory")?;
                reject_cmd_unusable_dir(cwd)?;
                raw.push("-d".to_string());
                raw.push(quote_windows_arg(&escape_wt_delimiters(cwd), false));
            }
            raw.push(quote_windows_arg(&escape_wt_delimiters(&shell), false));
            raw.extend(["/S", keep_open_switch(options.keep_open)].map(str::to_string));
            raw.push(quote_windows_arg(&escape_wt_delimiters(&inner), false));
            let mut command = command(
                wt.clone().into_os_string(),
                TerminalArgs::WindowsRaw(raw.join(" ")),
            );
            // `-d` places the tab; the launcher itself never runs in the
            // (possibly untrusted) session directory.
            command.cwd = cwd.is_some().then(windows_system_dir);
            Ok(command)
        }
        TerminalLauncher::WindowsConsole => {
            let inner = windows_cmd_command(&program, &args)?;
            let shell = system_cmd_text()?;
            let mut raw = String::from("/C start \"\"");
            if let Some(cwd) = &cwd {
                reject_cmd_unsafe(cwd, "working directory")?;
                reject_cmd_unusable_dir(cwd)?;
                raw.push_str(&format!(" /D \"{cwd}\""));
            }
            raw.push_str(&format!(
                " \"{shell}\" /S {} {}",
                keep_open_switch(options.keep_open),
                escape_cmd_metachars(&format!("\"{inner}\""))
            ));
            let mut command = command(OsString::from(&shell), TerminalArgs::WindowsRaw(raw));
            // `start /D` places the window. The outer cmd.exe runs in the
            // system directory: `start` looks a program up in cmd's current
            // directory before PATH, so a session directory holding a
            // planted `cmd.exe` must never be that directory.
            command.cwd = cwd.is_some().then(windows_system_dir);
            command.hide_console = true;
            Ok(command)
        }
        TerminalLauncher::MacTerminal => {
            let script_path = options
                .script_path
                .clone()
                .ok_or_else(|| launch_error("no location for the launch script"))?;
            let (line, tail) = if options.keep_open {
                (
                    posix_line(&program, &args, cwd.as_deref(), LineEnd::Plain),
                    "\nexec \"${SHELL:-/bin/sh}\" -l\n",
                )
            } else {
                (
                    posix_line(&program, &args, cwd.as_deref(), LineEnd::Exec),
                    "\n",
                )
            };
            let contents = format!(
                "#!/bin/sh\n# Generated by claude-history-gui; removes itself.\nrm -f -- \"$0\"\n{line}{tail}"
            );
            let mut command = command(
                OsString::from("open"),
                TerminalArgs::Argv(vec![
                    OsString::from("-a"),
                    OsString::from("Terminal"),
                    script_path.clone().into_os_string(),
                ]),
            );
            command.script = Some(LaunchScript {
                path: script_path,
                contents,
            });
            Ok(command)
        }
        TerminalLauncher::MacOsascript => {
            let mut line = posix_line(&program, &args, cwd.as_deref(), LineEnd::Plain);
            if !options.keep_open {
                line.push_str("; exit");
            }
            let script = format!("do script \"{}\"", escape_applescript(&line));
            Ok(command(
                OsString::from("osascript"),
                TerminalArgs::Argv(
                    [
                        "-e",
                        "tell application \"Terminal\"",
                        "-e",
                        "activate",
                        "-e",
                        &script,
                        "-e",
                        "end tell",
                    ]
                    .into_iter()
                    .map(OsString::from)
                    .collect(),
                ),
            ))
        }
        TerminalLauncher::EnvTerminal {
            program: terminal,
            args: leading,
        } => {
            let mut argv: Vec<String> = leading.clone();
            argv.push("-e".into());
            argv.extend(inner_argv(spec, &program, &args, cwd.as_deref(), options)?);
            Ok(command(terminal.clone().into_os_string(), to_argv(argv)))
        }
        TerminalLauncher::Linux {
            terminal,
            program: terminal_program,
        } => {
            let mut argv = terminal.prefix(cwd.as_deref());
            argv.extend(inner_argv(spec, &program, &args, cwd.as_deref(), options)?);
            Ok(command(
                terminal_program.clone().into_os_string(),
                to_argv(argv),
            ))
        }
        TerminalLauncher::Template(template) => build_template(
            template,
            spec,
            &program,
            &args,
            cwd.as_deref(),
            options,
            name.clone(),
        ),
    }
}

fn keep_open_switch(keep_open: bool) -> &'static str {
    if keep_open { "/K" } else { "/C" }
}

fn to_argv(args: Vec<String>) -> TerminalArgs {
    TerminalArgs::Argv(args.into_iter().map(OsString::from).collect())
}

/// What a terminal emulator runs: `sh -c <line>` on Unix, the program
/// directly on Windows (where `sh` is not available).
fn inner_argv(
    spec: &LaunchSpec,
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    options: &BuildOptions,
) -> Result<Vec<String>> {
    if options.platform.is_windows() {
        if let Some(cwd) = cwd {
            reject_batch_script_in(spec, cwd)?;
        }
        return direct_argv(spec);
    }
    Ok(vec![
        "sh".to_string(),
        "-c".to_string(),
        posix_line(
            program,
            args,
            cwd,
            LineEnd::for_keep_open(options.keep_open),
        ),
    ])
}

/// The program and its arguments as separate words; a Windows batch script
/// runs through `cmd /C` (W9), which refuses arguments `cmd` would mangle
/// ([`ResolvedProgram::argv`]).
fn direct_argv(spec: &LaunchSpec) -> Result<Vec<String>> {
    spec.program
        .argv(&spec.args)?
        .iter()
        .map(|arg| utf8(arg, "argument").map(str::to_string))
        .collect()
}

/// How a POSIX command line ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineEnd {
    /// Just run the program (the caller keeps the shell).
    Plain,
    /// `exec` the program (the window closes with it).
    Exec,
    /// Run the program, then replace the shell with the user's shell.
    Shell,
}

impl LineEnd {
    fn for_keep_open(keep_open: bool) -> Self {
        if keep_open {
            LineEnd::Shell
        } else {
            LineEnd::Exec
        }
    }
}

/// `cd -- '<cwd>' && '<program>' '<arg>'...`, with `exec` or a trailing
/// `; exec "${SHELL:-/bin/sh}"` depending on `end`.
fn posix_line(program: &str, args: &[String], cwd: Option<&str>, end: LineEnd) -> String {
    let command = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote_posix)
        .collect::<Vec<_>>()
        .join(" ");
    let run = if end == LineEnd::Exec {
        format!("exec {command}")
    } else {
        command
    };
    let mut line = match cwd {
        Some(cwd) => format!("cd -- {} && {run}", quote_posix(cwd)),
        None => run,
    };
    if end == LineEnd::Shell {
        line.push_str("; exec \"${SHELL:-/bin/sh}\"");
    }
    line
}

#[allow(clippy::too_many_arguments)]
fn build_template(
    template: &str,
    spec: &LaunchSpec,
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    options: &BuildOptions,
    name: String,
) -> Result<TerminalCommand> {
    let words = split_template(template)?;
    if words.is_empty() {
        return Err(launch_error("the terminal template is empty"));
    }
    if words[0].contains("{args}") || words[0].contains("{cmd}") {
        return Err(launch_error(
            "the terminal template must start with a program, not {args} or {cmd}",
        ));
    }
    let uses_command = words
        .iter()
        .any(|word| word.contains("{args}") || word.contains("{cmd}"));
    let windows = options.platform.is_windows();
    if windows {
        for arg in args {
            reject_cmd_unsafe(arg, "argument")?;
        }
    }
    if words.iter().any(|word| word.contains("{cwd}")) && cwd.is_none() {
        return Err(launch_error(
            "the terminal template uses {cwd} but the session has no working directory",
        ));
    }
    let cwd_text = cwd.unwrap_or_default();
    if windows && cwd.is_some() {
        reject_cmd_unsafe(cwd_text, "working directory")?;
        // Anything cmd.exe runs in (or `cd /d` / `start /D` sends to) a UNC
        // directory lands in the Windows directory instead: a `cmd` word,
        // the `{cmd}` string (and `{args}` inside a word), or a batch script
        // placed by `{args}` (it runs through `cmd /C`).
        let shell_string = words
            .iter()
            .any(|word| word.contains("{cmd}") || (word.contains("{args}") && word != "{args}"));
        if shell_string || words.iter().any(|word| is_cmd_program(word)) {
            reject_cmd_unusable_dir(cwd_text)?;
        }
        reject_batch_script_in(spec, cwd_text)?;
    }
    // The program and arguments as separate words, when the template places
    // them so (a batch script then runs through `cmd /C`, see
    // `ResolvedProgram::argv`).
    let direct = if !uses_command || words.iter().any(|word| word == "{args}") {
        direct_argv(spec)?
    } else {
        Vec::new()
    };
    // The single shell string of `{cmd}` (and of `{args}` inside a word).
    let shell_command = if windows {
        let mut command = windows_cmd_command(program, args)?;
        if let Some(cwd) = cwd {
            command = format!("cd /d \"{cwd}\" && {command}");
        }
        format!("\"{command}\"")
    } else {
        posix_line(
            program,
            args,
            cwd,
            LineEnd::for_keep_open(options.keep_open),
        )
    };
    // Inside a verbatim word the directory carries its own quotes.
    let verbatim_cwd = if windows {
        quote_windows_arg(cwd_text, true)
    } else {
        cwd_text.to_string()
    };
    // On Windows a template run by `cmd.exe` is read with cmd.exe rules.
    let cmd_template = windows && is_cmd_program(&words[0]);

    let mut out: Vec<TemplateWord> = Vec::new();
    let mut nested_cmd = false;
    for (index, word) in words.iter().enumerate() {
        if word == "{args}" {
            out.extend(direct.iter().cloned().map(TemplateWord::Command));
        } else if word.contains("{cmd}") || word.contains("{args}") {
            let text = word
                .replace("{cwd}", &verbatim_cwd)
                .replace("{cmd}", &shell_command)
                .replace("{args}", &shell_command);
            out.push(TemplateWord::Verbatim {
                text,
                // `cmd /C start ... cmd /S /K {cmd}`: the outer cmd.exe
                // reads the command meant for the inner one first.
                outer_cmd_layer: cmd_template && nested_cmd,
            });
        } else if word.contains("{cwd}") {
            out.push(TemplateWord::Cwd(word.replace("{cwd}", cwd_text)));
        } else if windows && is_bare_cmd(word) {
            // Never let a cmd.exe (`start`) or a terminal look `cmd` up in
            // the session directory.
            out.push(TemplateWord::Literal(system_cmd_text()?));
        } else {
            out.push(TemplateWord::Literal(word.clone()));
        }
        nested_cmd |= index > 0 && is_cmd_program(word);
    }
    if !uses_command {
        out.extend(direct.into_iter().map(TemplateWord::Command));
    }
    let program_word = out.remove(0).into_text();
    let args = if windows {
        TerminalArgs::WindowsRaw(
            out.into_iter()
                .map(|word| word.windows_text(cmd_template))
                .collect::<Vec<_>>()
                .join(" "),
        )
    } else {
        to_argv(out.into_iter().map(TemplateWord::into_text).collect())
    };
    // A `cmd` template looks bare program names (`start "" wt ...`) up in
    // its current directory before PATH. When the template places the
    // directory itself (`{cwd}`, or `{cmd}` with its `cd /d`), it runs in
    // the system directory instead of the session directory.
    let places_cwd = cwd.is_some()
        && words.iter().any(|word| {
            word.contains("{cwd}")
                || word.contains("{cmd}")
                || (word.contains("{args}") && word != "{args}")
        });
    let process_cwd = if cmd_template && places_cwd {
        Some(windows_system_dir())
    } else {
        spec.cwd.clone()
    };
    Ok(TerminalCommand {
        launcher: name,
        program: OsString::from(program_word),
        args,
        cwd: process_cwd,
        script: None,
        hide_console: false,
    })
}

/// One word of an expanded template.
enum TemplateWord {
    /// Template text without placeholders (the user's own quoting applies).
    Literal(String),
    /// A word with `{cwd}` substituted.
    Cwd(String),
    /// The program or one of its arguments (`{args}`, or appended).
    Command(String),
    /// A word with `{cmd}` (or `{args}` inside a longer word), already
    /// quoted for the `cmd.exe` that runs the command.
    Verbatim { text: String, outer_cmd_layer: bool },
}

impl TemplateWord {
    fn into_text(self) -> String {
        match self {
            TemplateWord::Literal(text)
            | TemplateWord::Cwd(text)
            | TemplateWord::Command(text)
            | TemplateWord::Verbatim { text, .. } => text,
        }
    }

    /// The word on a raw Windows command line. Substituted values must
    /// survive both the MSVC rules (most terminals) and, when the template
    /// runs `cmd.exe`, the `cmd.exe` rules: the directory is always quoted
    /// (`& | < > ^ ( )` are literal inside quotes), and for a `cmd.exe`
    /// template the program and arguments are `^`-escaped like
    /// [`windows_cmd_command`].
    fn windows_text(self, cmd_template: bool) -> String {
        match self {
            TemplateWord::Literal(text) => quote_windows_arg(&text, false),
            TemplateWord::Cwd(text) => quote_windows_arg(&text, true),
            TemplateWord::Command(text) if cmd_template => cmd_escaped_arg(&text),
            TemplateWord::Command(text) => quote_windows_arg(&text, false),
            TemplateWord::Verbatim {
                text,
                outer_cmd_layer: true,
            } => escape_cmd_metachars(&text),
            TemplateWord::Verbatim { text, .. } => text,
        }
    }
}

/// Split a template into words: whitespace separates, `"..."` and `'...'`
/// group (the quotes are removed), there are no backslash escapes.
pub fn split_template(template: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for ch in template.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
        }
    }
    if let Some(q) = quote {
        return Err(launch_error(format!(
            "unterminated {q} quote in the terminal template"
        )));
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// Quote a word for a POSIX shell (`'...'`, with `'` written as `'\''`).
/// Words made only of safe characters are left as they are.
pub fn quote_posix(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-_./=:@%+,".contains(ch));
    if safe {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Characters `cmd.exe` treats specially somewhere on a command line or in
/// a batch file's argument splitting.
const CMD_SPECIAL: &[char] = &[
    ' ', '\t', '"', '&', '|', '<', '>', '^', '(', ')', '%', '!', ',', ';', '=',
];

/// `cmd.exe` metacharacters escaped with `^` by [`escape_cmd_metachars`].
const CMD_METACHARS: &[char] = &['(', ')', '%', '!', '^', '"', '<', '>', '&', '|'];

/// Quote one argument with the MSVC rules (`CommandLineToArgvW`): wrap it
/// in `"` when needed (or when `force`), double backslashes before a quote,
/// escape quotes as `\"`.
pub fn quote_windows_arg(arg: &str, force: bool) -> String {
    let needs_quotes = force
        || arg.is_empty()
        || arg
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\u{b}' | '"'));
    if !needs_quotes {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(ch);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Prefix every `cmd.exe` metacharacter with `^`.
pub fn escape_cmd_metachars(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for ch in text.chars() {
        if CMD_METACHARS.contains(&ch) {
            out.push('^');
        }
        out.push(ch);
    }
    out
}

/// Windows Terminal splits its command line at `;`; `\;` is a literal `;`.
fn escape_wt_delimiters(text: &str) -> String {
    text.replace(';', "\\;")
}

/// AppleScript string-literal escaping.
fn escape_applescript(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn reject_cmd_unsafe(text: &str, what: &str) -> Result<()> {
    if text.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
        return Err(launch_error(format!(
            "{what} contains a line break, which cannot be passed through cmd.exe: {text:?}"
        )));
    }
    Ok(())
}

/// Whether `cmd.exe` cannot use `dir` as its current directory: any path
/// starting with two slashes (UNC `\\server\share`, `\\?\UNC\...`, and even
/// `\\?\C:\...`). `cmd.exe` then prints "UNC paths are not supported",
/// falls back to the Windows directory, and a `cd /d` to it fails, so the
/// session would run (and Claude would look it up) in the wrong project.
/// Upstream spawned the program directly, which works.
fn cmd_unusable_dir(dir: &str) -> bool {
    let mut chars = dir.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('\\' | '/'), Some('\\' | '/'))
    )
}

/// Refuse to run `cmd.exe` in a directory it cannot use
/// ([`cmd_unusable_dir`]).
fn reject_cmd_unusable_dir(cwd: &str) -> Result<()> {
    if cmd_unusable_dir(cwd) {
        return Err(launch_error(format!(
            "cmd.exe cannot run in the UNC directory {cwd}: it would fall back to the Windows \
             directory and the session would run in the wrong project. Set [gui].terminal to a \
             template that starts the program without cmd.exe, such as `wt -d {{cwd}} {{args}}` \
             (this needs a native executable, not a .cmd/.bat shim)"
        )));
    }
    Ok(())
}

/// A batch script runs through `cmd /C` ([`ResolvedProgram::argv`]), so it
/// cannot start in a directory `cmd.exe` cannot use either.
fn reject_batch_script_in(spec: &LaunchSpec, cwd: &str) -> Result<()> {
    if spec.program.kind == crate::program::ProgramKind::BatchScript {
        reject_cmd_unusable_dir(cwd)?;
    }
    Ok(())
}

/// The `cmd.exe` command line running `program` with `args` (see the module
/// docs for the quoting rules).
pub fn windows_cmd_command(program: &str, args: &[String]) -> Result<String> {
    reject_cmd_unsafe(program, "program path")?;
    let mut command = format!("\"{program}\"");
    for arg in args {
        reject_cmd_unsafe(arg, "argument")?;
        command.push(' ');
        command.push_str(&cmd_escaped_arg(arg));
    }
    Ok(command)
}

/// One argument of a `cmd.exe` command line: MSVC-quoted (always when it
/// contains a `cmd.exe` special character), then `^`-escaped.
fn cmd_escaped_arg(arg: &str) -> String {
    let force = arg.chars().any(|ch| CMD_SPECIAL.contains(&ch));
    escape_cmd_metachars(&quote_windows_arg(arg, force))
}

/// Whether a template's program is `cmd.exe`, which reads the rest of its
/// command line with `cmd.exe` rules instead of the MSVC rules.
fn is_cmd_program(word: &str) -> bool {
    let name = word.rsplit(['\\', '/']).next().unwrap_or(word);
    is_bare_cmd(name)
}

/// `cmd` or `cmd.exe` without a directory: a name `cmd.exe` would look up
/// in its current directory first.
fn is_bare_cmd(word: &str) -> bool {
    word.eq_ignore_ascii_case("cmd") || word.eq_ignore_ascii_case("cmd.exe")
}

/// `cmd.exe` by absolute path ([`windows_cmd_exe`]) as text for a command
/// line.
fn system_cmd_text() -> Result<String> {
    let shell = windows_cmd_exe();
    let text = utf8(shell.as_os_str(), "system cmd.exe path")?.to_string();
    reject_cmd_unsafe(&text, "system cmd.exe path")?;
    Ok(text)
}

/// Write the launch script (if any) and spawn the terminal without waiting
/// for it. The child is reaped on a background thread.
///
/// Under `cfg(test)` and the `hermetic` feature it refuses with
/// [`std::io::ErrorKind::Unsupported`]: no test opens a terminal.
pub fn spawn_terminal(command: &TerminalCommand) -> std::io::Result<()> {
    let mut child = spawn_child(command)?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// What a terminal launcher did right after it was spawned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnOutcome {
    /// Still running when the wait ended (the usual case for an emulator
    /// that owns its window).
    Running,
    /// Exited within the wait, with this code (`-1` without one, e.g.
    /// killed by a signal). `0` is a launcher that handed the window to
    /// another process (`wt.exe`, `cmd /C start`, `open`); anything else
    /// means it failed.
    Exited(i32),
}

/// [`spawn_terminal`], then wait up to `wait` for an early exit so that a
/// launcher that fails at once (a bad template, a broken emulator) is
/// reported instead of silently doing nothing. A child still running after
/// `wait` is reaped on a background thread. Blocks for up to `wait`: call
/// it off the UI thread.
///
/// Under `cfg(test)` and the `hermetic` feature it refuses with
/// [`std::io::ErrorKind::Unsupported`].
pub fn spawn_terminal_checked(
    command: &TerminalCommand,
    wait: std::time::Duration,
) -> std::io::Result<SpawnOutcome> {
    let mut child = spawn_child(command)?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(SpawnOutcome::Exited(status.code().unwrap_or(-1)));
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(25)));
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(SpawnOutcome::Running)
}

#[cfg(any(test, feature = "hermetic"))]
fn spawn_child(command: &TerminalCommand) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "hermetic build: refusing to open a terminal ({})",
            command.launcher
        ),
    ))
}

/// Write the launch script (if any) and spawn the terminal process.
#[cfg(not(any(test, feature = "hermetic")))]
fn spawn_child(command: &TerminalCommand) -> std::io::Result<std::process::Child> {
    if let Some(script) = &command.script {
        write_script(script)?;
    }
    let mut process = std::process::Command::new(&command.program);
    match &command.args {
        TerminalArgs::Argv(args) => {
            process.args(args);
        }
        TerminalArgs::WindowsRaw(raw) => {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                process.raw_arg(raw);
            }
            #[cfg(not(windows))]
            {
                let _ = raw;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Windows command lines can only be launched on Windows",
                ));
            }
        }
    }
    if let Some(cwd) = command.cwd.as_ref().filter(|cwd| cwd.is_dir()) {
        process.current_dir(cwd);
    }
    process
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    if command.hide_console {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    process.spawn()
}

#[cfg_attr(any(test, feature = "hermetic"), allow(dead_code))]
fn write_script(script: &LaunchScript) -> std::io::Result<()> {
    if let Some(parent) = script.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(&script.path)?;
    std::io::Write::write_all(&mut file, script.contents.as_bytes())?;
    Ok(())
}

/// Whether `path` names a launch script (`*.command`) as written by
/// [`spawn_terminal`].
pub fn is_launch_script(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str) == Some("command")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::ProgramKind;

    // ----- Reference parsers used to check the quoting end to end. -----

    /// `CommandLineToArgvW` (post-2008 rules) for a full command line.
    fn parse_windows_command_line(line: &str) -> Vec<String> {
        let chars: Vec<char> = line.chars().collect();
        let mut args = Vec::new();
        let mut i = 0;
        // The program name: quotes group, no escapes.
        let mut program = String::new();
        let mut in_quotes = false;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '"' {
                in_quotes = !in_quotes;
            } else if !in_quotes && (ch == ' ' || ch == '\t') {
                break;
            } else {
                program.push(ch);
            }
            i += 1;
        }
        args.push(program);
        loop {
            while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            let mut arg = String::new();
            let mut in_quotes = false;
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' {
                    let mut count = 0;
                    while i < chars.len() && chars[i] == '\\' {
                        count += 1;
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '"' {
                        arg.extend(std::iter::repeat_n('\\', count / 2));
                        if count % 2 == 1 {
                            arg.push('"');
                            i += 1;
                        }
                    } else {
                        arg.extend(std::iter::repeat_n('\\', count));
                    }
                    continue;
                }
                if ch == '"' {
                    if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                        arg.push('"');
                        i += 2;
                        continue;
                    }
                    in_quotes = !in_quotes;
                    i += 1;
                    continue;
                }
                if !in_quotes && (ch == ' ' || ch == '\t') {
                    break;
                }
                arg.push(ch);
                i += 1;
            }
            args.push(arg);
        }
        args
    }

    /// `cmd.exe` phase 2 (caret and quote handling) for one command line,
    /// ignoring `%` expansion. Fails on an unescaped special character
    /// outside quotes.
    fn cmd_phase2(line: &str) -> String {
        let mut out = String::new();
        let mut in_quotes = false;
        let mut chars = line.chars();
        while let Some(ch) = chars.next() {
            if in_quotes {
                out.push(ch);
                if ch == '"' {
                    in_quotes = false;
                }
                continue;
            }
            match ch {
                '^' => out.push(chars.next().expect("dangling caret")),
                '"' => {
                    in_quotes = true;
                    out.push(ch);
                }
                '&' | '|' | '<' | '>' => panic!("unescaped {ch} in {line}"),
                _ => out.push(ch),
            }
        }
        out
    }

    /// `cmd /S /K "<x>"`: strip the first and the last quote.
    fn cmd_s_strip(text: &str) -> String {
        let first = text.find('"').expect("first quote");
        let last = text.rfind('"').expect("last quote");
        assert!(last > first);
        format!(
            "{}{}{}",
            &text[..first],
            &text[first + 1..last],
            &text[last + 1..]
        )
    }

    /// Windows Terminal: split its argv at unescaped `;`, unescape `\;`, and
    /// rebuild the child command line, quoting words that contain a space.
    fn wt_child_command_line(raw: &str) -> (Option<String>, String) {
        let argv = parse_windows_command_line(&format!("wt.exe {raw}"));
        let mut words = argv[1..].to_vec();
        for word in &words {
            assert!(
                !word.replace("\\;", "").contains(';'),
                "unescaped ; in {word}"
            );
        }
        words = words.into_iter().map(|w| w.replace("\\;", ";")).collect();
        // K17: every Windows Terminal launch opens a new window.
        assert_eq!(&words[..2], ["-w", "new"], "{raw}");
        words.drain(..2);
        let mut dir = None;
        if words.first().map(String::as_str) == Some("-d") {
            dir = Some(words[1].clone());
            words.drain(..2);
        }
        let line = words
            .iter()
            .map(|word| {
                if word.contains(' ') {
                    format!("\"{word}\"")
                } else {
                    word.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        (dir, line)
    }

    /// POSIX `sh` words for the subset produced by [`quote_posix`].
    fn parse_posix_words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();
        let mut in_word = false;
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\'' => {
                    in_word = true;
                    for inner in chars.by_ref() {
                        if inner == '\'' {
                            break;
                        }
                        current.push(inner);
                    }
                }
                '\\' => {
                    in_word = true;
                    current.push(chars.next().unwrap());
                }
                ' ' => {
                    if in_word {
                        words.push(std::mem::take(&mut current));
                        in_word = false;
                    }
                }
                _ => {
                    in_word = true;
                    current.push(ch);
                }
            }
        }
        if in_word {
            words.push(current);
        }
        words
    }

    // ----- Fixtures. -----

    const TRICKY_ARGS: &[&str] = &[
        "--resume",
        "12345678-1234-4234-9234-123456789abc",
        "two words",
        "say \"hi\"",
        "trailing\\",
        "with space and trailing\\",
        "a&b|c<d>e^f(g)h",
        "100% !done!",
        "semi;colon,comma=equals",
        "",
        "юникод 日本語 ✓",
        "'single'",
    ];

    fn spec(program: &str, kind: ProgramKind, args: &[&str], cwd: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            program: ResolvedProgram {
                path: PathBuf::from(program),
                kind,
            },
            args: args.iter().map(OsString::from).collect(),
            cwd: cwd.map(PathBuf::from),
        }
    }

    fn options(platform: Platform) -> BuildOptions {
        BuildOptions {
            platform,
            keep_open: true,
            script_path: Some(PathBuf::from("/tmp/launch/resume-1.command")),
        }
    }

    fn raw(command: &TerminalCommand) -> &str {
        match &command.args {
            TerminalArgs::WindowsRaw(raw) => raw,
            other => panic!("expected a raw Windows command line, got {other:?}"),
        }
    }

    fn argv(command: &TerminalCommand) -> Vec<String> {
        match &command.args {
            TerminalArgs::Argv(args) => args
                .iter()
                .map(|arg| arg.to_str().unwrap().to_string())
                .collect(),
            other => panic!("expected argv, got {other:?}"),
        }
    }

    fn expected_argv(program: &str) -> Vec<String> {
        std::iter::once(program)
            .chain(TRICKY_ARGS.iter().copied())
            .map(str::to_string)
            .collect()
    }

    const WIN_PROGRAM: &str = r"C:\Users\John Doe\AppData\Roaming\npm\claude.cmd";
    const WIN_EXE: &str = r"C:\Users\John Doe\.local\bin\claude.exe";
    const WIN_CWD: &str = r"C:\code\my project (1)\юникод";
    /// A directory whose name `cmd.exe` would split at `&` if unquoted.
    const WIN_AMP_CWD: &str = r"C:\code\R&D";

    /// `cmd.exe` by absolute path, as the Windows launchers write it.
    fn sys_cmd() -> String {
        windows_cmd_exe().to_str().unwrap().to_string()
    }

    fn spec_with_args(spec: &LaunchSpec, args: &[&str]) -> LaunchSpec {
        LaunchSpec {
            args: args.iter().map(OsString::from).collect(),
            ..spec.clone()
        }
    }

    // ----- Windows. -----

    #[test]
    fn windows_arg_quoting_round_trips_through_command_line_to_argv() {
        for arg in
            TRICKY_ARGS
                .iter()
                .copied()
                .chain(["\\\\server\\share\\", "\"", "\\\"", "a\\\\\"b"])
        {
            let line = format!("prog {}", quote_windows_arg(arg, false));
            assert_eq!(
                parse_windows_command_line(&line),
                vec!["prog".to_string(), arg.to_string()],
                "{arg:?} -> {line}"
            );
            let forced = format!("prog {}", quote_windows_arg(arg, true));
            assert_eq!(parse_windows_command_line(&forced)[1], arg, "{forced}");
        }
        assert_eq!(quote_windows_arg("plain", false), "plain");
        assert_eq!(quote_windows_arg("C:\\x y\\", false), "\"C:\\x y\\\\\"");
        assert_eq!(quote_windows_arg("", false), "\"\"");
    }

    #[test]
    fn windows_cmd_command_survives_cmd_and_reaches_the_program_intact() {
        let args: Vec<String> = TRICKY_ARGS.iter().map(|arg| arg.to_string()).collect();
        let command = windows_cmd_command(WIN_PROGRAM, &args).unwrap();
        assert!(command.starts_with(&format!("\"{WIN_PROGRAM}\" --resume ")));
        // cmd.exe removes the carets; the program sees MSVC-quoted arguments.
        let line = cmd_phase2(&command);
        assert_eq!(
            parse_windows_command_line(&line),
            expected_argv(WIN_PROGRAM)
        );
        // Every argument with a cmd special character is quoted, so batch
        // shims that re-expand %* keep them protected.
        assert!(line.contains("\"a&b|c<d>e^f(g)h\""), "{line}");
        assert!(line.contains("\"semi;colon,comma=equals\""), "{line}");
    }

    #[test]
    fn windows_terminal_command_line() {
        let launcher = TerminalLauncher::WindowsTerminal {
            program: PathBuf::from(r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\wt.exe"),
        };
        let spec = spec(
            WIN_PROGRAM,
            ProgramKind::BatchScript,
            TRICKY_ARGS,
            Some(WIN_CWD),
        );
        let command =
            build_terminal_command(&launcher, &spec, &options(Platform::Windows)).unwrap();
        assert_eq!(command.launcher, "Windows Terminal");
        assert_eq!(
            command.program,
            OsString::from(r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\wt.exe")
        );
        // `-d` places the tab; wt.exe itself runs in the system directory.
        assert_eq!(command.cwd, Some(windows_system_dir()));
        assert!(!command.hide_console);
        let raw = raw(&command);
        let sys = sys_cmd();
        // `-w new` (K17): a window of its own, never a tab of an open one.
        assert!(
            raw.starts_with(&format!("-w new -d \"{WIN_CWD}\" {sys} /S /K \"")),
            "{raw}"
        );

        // wt.exe rebuilds `<sys>\cmd.exe /S /K "<command>"`; cmd strips the
        // outer quotes (/S), processes carets, and the program gets the
        // arguments.
        let (dir, child) = wt_child_command_line(raw);
        assert_eq!(dir.as_deref(), Some(WIN_CWD));
        let prefix = format!("{sys} /S /K ");
        assert!(child.starts_with(&prefix), "{child}");
        let inner = cmd_s_strip(&child[prefix.len()..]);
        let line = cmd_phase2(&inner);
        assert_eq!(
            parse_windows_command_line(&line),
            expected_argv(WIN_PROGRAM)
        );
    }

    #[test]
    fn windows_terminal_escapes_semicolons_in_the_directory() {
        let launcher = TerminalLauncher::WindowsTerminal {
            program: PathBuf::from("wt.exe"),
        };
        let spec = spec(
            r"C:\bin\claude.exe",
            ProgramKind::Executable,
            &["--resume", "x"],
            Some(r"C:\a;b"),
        );
        let command =
            build_terminal_command(&launcher, &spec, &options(Platform::Windows)).unwrap();
        let (dir, child) = wt_child_command_line(raw(&command));
        assert_eq!(dir.as_deref(), Some(r"C:\a;b"));
        assert_eq!(
            parse_windows_command_line(&cmd_phase2(&cmd_s_strip(
                &child[format!("{} /S /K ", sys_cmd()).len()..]
            ))),
            vec![r"C:\bin\claude.exe", "--resume", "x"]
        );
    }

    #[test]
    fn unc_working_directories_are_refused_wherever_cmd_exe_would_run_in_them() {
        // cmd.exe started in (or `cd /d` / `start /D` to) a UNC directory
        // prints "UNC paths are not supported" and runs in C:\Windows, so
        // claude --resume would look in the wrong projects folder.
        let windows = options(Platform::Windows);
        let wt = TerminalLauncher::WindowsTerminal {
            program: PathBuf::from("wt.exe"),
        };
        let unc_dirs = [
            r"\\server\share\proj",
            r"\\?\UNC\server\share\proj",
            r"\\?\C:\proj",
            "//server/share/proj",
        ];
        for cwd in unc_dirs {
            for kind in [ProgramKind::Executable, ProgramKind::BatchScript] {
                let program = match kind {
                    ProgramKind::Executable => WIN_EXE,
                    ProgramKind::BatchScript => WIN_PROGRAM,
                };
                let spec = spec(program, kind, &["--resume", "x"], Some(cwd));
                for launcher in [&wt, &TerminalLauncher::WindowsConsole] {
                    let error = build_terminal_command(launcher, &spec, &windows).unwrap_err();
                    assert!(matches!(error, AppError::TerminalLaunch(_)), "{error}");
                    assert!(
                        error.to_string().contains("UNC directory"),
                        "{cwd}: {error}"
                    );
                }
                for template in [
                    "cmd /S /K {cmd}",
                    "cmd /C start \"\" /D {cwd} cmd /S /K {cmd}",
                    "cmd /S /K {args}",
                    "wt -d {cwd} cmd /S /K {args}",
                    "wezterm start -- \"{cmd}\"",
                ] {
                    let launcher = TerminalLauncher::Template(template.to_string());
                    assert!(
                        build_terminal_command(&launcher, &spec, &windows).is_err(),
                        "{template} in {cwd}"
                    );
                }
            }
            // A batch script runs through `cmd /C` even from a template
            // that starts the program directly...
            let batch = spec(
                WIN_PROGRAM,
                ProgramKind::BatchScript,
                &["--resume", "x"],
                Some(cwd),
            );
            let direct = TerminalLauncher::Template("wt -d {cwd} {args}".to_string());
            assert!(build_terminal_command(&direct, &batch, &windows).is_err());
            // ...while a native executable started directly works, as
            // upstream's direct spawn did.
            let native = spec(
                WIN_EXE,
                ProgramKind::Executable,
                &["--resume", "x"],
                Some(cwd),
            );
            let command = build_terminal_command(&direct, &native, &windows).unwrap();
            assert_eq!(command.program, OsString::from("wt"));
            let words = parse_windows_command_line(&format!("wt {}", raw(&command)));
            assert_eq!(words, ["wt", "-d", cwd, WIN_EXE, "--resume", "x"]);
        }

        // Ordinary drive paths and other platforms are unaffected.
        let local = spec(
            WIN_EXE,
            ProgramKind::Executable,
            &["--resume", "x"],
            Some(WIN_CWD),
        );
        assert!(build_terminal_command(&wt, &local, &windows).is_ok());
        assert!(
            build_terminal_command(&TerminalLauncher::WindowsConsole, &local, &windows).is_ok()
        );
        assert!(!cmd_unusable_dir(r"C:\code"));
        assert!(!cmd_unusable_dir(r"\code"));
        let posix = spec(
            "/usr/bin/claude",
            ProgramKind::Executable,
            &[],
            Some("//srv"),
        );
        let template = TerminalLauncher::Template("kitty --directory {cwd} {args}".to_string());
        assert!(build_terminal_command(&template, &posix, &options(Platform::Linux)).is_ok());
    }

    #[test]
    fn windows_console_fallback_survives_two_cmd_layers() {
        let spec = spec(
            WIN_PROGRAM,
            ProgramKind::BatchScript,
            TRICKY_ARGS,
            Some(WIN_CWD),
        );
        let command = build_terminal_command(
            &TerminalLauncher::WindowsConsole,
            &spec,
            &options(Platform::Windows),
        )
        .unwrap();
        // Both cmd.exe layers are named by absolute path, and the outer one
        // runs in the system directory (`start /D` places the window), so a
        // `cmd.exe` planted in the session directory is never looked up.
        let sys = sys_cmd();
        assert_eq!(command.program, OsString::from(&sys));
        assert_eq!(command.cwd, Some(windows_system_dir()));
        assert!(command.hide_console);
        let raw = raw(&command);
        let start = format!("/C start \"\" /D \"{WIN_CWD}\" \"{sys}\" /S /K ");
        assert!(raw.starts_with(&start), "{raw}");

        // Outer cmd: processes the whole line, `start` passes the rest on.
        let outer = cmd_phase2(raw);
        let inner_arg = &outer[start.len()..];
        // Inner cmd /S /K: strip, then process the command.
        let line = cmd_phase2(&cmd_s_strip(inner_arg));
        assert_eq!(
            parse_windows_command_line(&line),
            expected_argv(WIN_PROGRAM)
        );
    }

    #[test]
    fn windows_without_keep_open_uses_slash_c_and_rejects_line_breaks() {
        let mut opts = options(Platform::Windows);
        opts.keep_open = false;
        let launcher = TerminalLauncher::WindowsTerminal {
            program: PathBuf::from("wt.exe"),
        };
        let command = build_terminal_command(
            &launcher,
            &spec(
                r"C:\bin\pi.exe",
                ProgramKind::Executable,
                &["--session", "x"],
                None,
            ),
            &opts,
        )
        .unwrap();
        assert_eq!(
            raw(&command),
            format!(
                r#"-w new {} /S /C "\"C:\bin\pi.exe\" --session x""#,
                sys_cmd()
            )
        );
        // Without a directory there is no `-d`: the launcher keeps the
        // inherited one.
        assert_eq!(command.cwd, None);

        let error = build_terminal_command(
            &launcher,
            &spec(
                r"C:\bin\pi.exe",
                ProgramKind::Executable,
                &["two\nlines"],
                None,
            ),
            &opts,
        )
        .unwrap_err();
        assert!(error.to_string().contains("line break"), "{error}");
    }

    // ----- macOS. -----

    #[test]
    fn mac_terminal_writes_a_self_deleting_command_script() {
        let spec = spec(
            "/Users/me/.local/bin/claude",
            ProgramKind::Executable,
            TRICKY_ARGS,
            Some("/Users/me/my project's dir"),
        );
        let command = build_terminal_command(
            &TerminalLauncher::MacTerminal,
            &spec,
            &options(Platform::MacOs),
        )
        .unwrap();
        assert_eq!(command.program, OsString::from("open"));
        assert_eq!(
            argv(&command),
            vec!["-a", "Terminal", "/tmp/launch/resume-1.command"]
        );
        let script = command.script.unwrap();
        assert_eq!(script.path, PathBuf::from("/tmp/launch/resume-1.command"));
        let lines: Vec<&str> = script.contents.lines().collect();
        assert_eq!(lines[0], "#!/bin/sh");
        assert_eq!(lines[2], "rm -f -- \"$0\"");
        let line = lines[3];
        assert!(
            line.starts_with(
                "cd -- '/Users/me/my project'\\''s dir' && /Users/me/.local/bin/claude --resume "
            ),
            "{line}"
        );
        let command_part = &line[line.find("&& ").unwrap() + 3..];
        assert_eq!(
            parse_posix_words(command_part),
            expected_argv("/Users/me/.local/bin/claude")
        );
        assert_eq!(lines[4], "exec \"${SHELL:-/bin/sh}\" -l");
    }

    #[test]
    fn mac_osascript_escapes_for_applescript() {
        let spec = spec(
            "/usr/local/bin/omp",
            ProgramKind::Executable,
            &["--resume", "/tmp/a \"b\".jsonl"],
            Some("/tmp"),
        );
        let command = build_terminal_command(
            &TerminalLauncher::MacOsascript,
            &spec,
            &options(Platform::MacOs),
        )
        .unwrap();
        assert_eq!(command.program, OsString::from("osascript"));
        assert_eq!(
            argv(&command),
            vec![
                "-e",
                "tell application \"Terminal\"",
                "-e",
                "activate",
                "-e",
                "do script \"cd -- /tmp && /usr/local/bin/omp --resume '/tmp/a \\\"b\\\".jsonl'\"",
                "-e",
                "end tell",
            ]
        );
    }

    // ----- Linux. -----

    #[test]
    fn linux_terminals_run_sh_with_a_quoted_line() {
        let spec = spec(
            "/home/me/bin/claude",
            ProgramKind::Executable,
            TRICKY_ARGS,
            Some("/home/me/a b"),
        );
        let opts = options(Platform::Linux);
        let expected_prefix: &[(LinuxTerminal, &[&str])] = &[
            (LinuxTerminal::XTerminalEmulator, &["-e"]),
            (
                LinuxTerminal::GnomeTerminal,
                &["--working-directory=/home/me/a b", "--"],
            ),
            (
                LinuxTerminal::Ptyxis,
                &["--new-window", "--working-directory=/home/me/a b", "--"],
            ),
            (
                LinuxTerminal::Kgx,
                &["--working-directory=/home/me/a b", "--"],
            ),
            (LinuxTerminal::Konsole, &["--workdir", "/home/me/a b", "-e"]),
            (
                LinuxTerminal::Xfce4Terminal,
                &["--working-directory=/home/me/a b", "-x"],
            ),
            (
                LinuxTerminal::Tilix,
                &["--working-directory=/home/me/a b", "-x"],
            ),
            (
                LinuxTerminal::Alacritty,
                &["--working-directory", "/home/me/a b", "-e"],
            ),
            (LinuxTerminal::Kitty, &["--directory", "/home/me/a b"]),
            (
                LinuxTerminal::Wezterm,
                &["start", "--cwd", "/home/me/a b", "--"],
            ),
            (LinuxTerminal::Xterm, &["-e"]),
        ];
        assert_eq!(expected_prefix.len(), LinuxTerminal::ALL.len());
        for (terminal, prefix) in expected_prefix {
            let launcher = TerminalLauncher::Linux {
                terminal: *terminal,
                program: PathBuf::from(format!("/usr/bin/{}", terminal.program())),
            };
            let command = build_terminal_command(&launcher, &spec, &opts).unwrap();
            assert_eq!(
                command.program,
                OsString::from(format!("/usr/bin/{}", terminal.program()))
            );
            assert_eq!(command.cwd, Some(PathBuf::from("/home/me/a b")));
            let words = argv(&command);
            assert_eq!(&words[..prefix.len()], *prefix, "{terminal:?}");
            assert_eq!(&words[prefix.len()..prefix.len() + 2], ["sh", "-c"]);
            let line = &words[prefix.len() + 2];
            assert_eq!(words.len(), prefix.len() + 3);
            let (cd, rest) = line.split_once(" && ").unwrap();
            assert_eq!(cd, "cd -- '/home/me/a b'");
            let (run, shell) = rest.rsplit_once("; ").unwrap();
            assert_eq!(shell, "exec \"${SHELL:-/bin/sh}\"");
            assert_eq!(parse_posix_words(run), expected_argv("/home/me/bin/claude"));
        }
    }

    #[test]
    fn env_terminal_and_no_keep_open() {
        let launcher = TerminalLauncher::EnvTerminal {
            program: PathBuf::from("/usr/bin/foot"),
            args: vec!["--app-id=x".to_string()],
        };
        let mut opts = options(Platform::Linux);
        opts.keep_open = false;
        let command = build_terminal_command(
            &launcher,
            &spec(
                "/bin/pi",
                ProgramKind::Executable,
                &["--session", "/s/a b.jsonl"],
                None,
            ),
            &opts,
        )
        .unwrap();
        assert_eq!(command.launcher, "foot");
        assert_eq!(
            argv(&command),
            vec![
                "--app-id=x",
                "-e",
                "sh",
                "-c",
                "exec /bin/pi --session '/s/a b.jsonl'"
            ]
        );
    }

    #[test]
    fn linux_terminal_on_windows_runs_the_program_directly() {
        let launcher = TerminalLauncher::Linux {
            terminal: LinuxTerminal::Wezterm,
            program: PathBuf::from("wezterm.exe"),
        };
        let command = build_terminal_command(
            &launcher,
            &spec(
                WIN_PROGRAM,
                ProgramKind::BatchScript,
                &["--resume", "x"],
                Some(WIN_CWD),
            ),
            &options(Platform::Windows),
        )
        .unwrap();
        assert_eq!(
            argv(&command),
            vec![
                "start",
                "--cwd",
                WIN_CWD,
                "--",
                sys_cmd().as_str(),
                "/C",
                WIN_PROGRAM,
                "--resume",
                "x"
            ]
        );
    }

    // ----- Templates. -----

    #[test]
    fn template_splitting() {
        assert_eq!(
            split_template(r#"  "C:\Program Files\x\term.exe" -d {cwd} 'a b'c {args} "#).unwrap(),
            vec![
                r"C:\Program Files\x\term.exe",
                "-d",
                "{cwd}",
                "a bc",
                "{args}"
            ]
        );
        assert!(split_template("x \"open").is_err());
        assert!(split_template("   ").unwrap().is_empty());
    }

    #[test]
    fn unix_templates_substitute_cwd_args_and_cmd() {
        let spec = spec(
            "/bin/claude",
            ProgramKind::Executable,
            &["--resume", "it's"],
            Some("/w d"),
        );
        let opts = options(Platform::Linux);

        let args = build_terminal_command(
            &TerminalLauncher::Template("kitty --directory {cwd} {args}".into()),
            &spec,
            &opts,
        )
        .unwrap();
        assert_eq!(args.program, OsString::from("kitty"));
        assert_eq!(
            argv(&args),
            vec!["--directory", "/w d", "/bin/claude", "--resume", "it's"]
        );

        let cmd = build_terminal_command(
            &TerminalLauncher::Template("alacritty -e sh -c {cmd}".into()),
            &spec,
            &opts,
        )
        .unwrap();
        assert_eq!(
            argv(&cmd),
            vec![
                "-e",
                "sh",
                "-c",
                "cd -- '/w d' && /bin/claude --resume 'it'\\''s'; exec \"${SHELL:-/bin/sh}\""
            ]
        );

        let appended =
            build_terminal_command(&TerminalLauncher::Template("xterm -e".into()), &spec, &opts)
                .unwrap();
        assert_eq!(
            argv(&appended),
            vec!["-e", "/bin/claude", "--resume", "it's"]
        );

        for bad in ["", "{args} x", "{cmd}"] {
            assert!(
                build_terminal_command(&TerminalLauncher::Template(bad.into()), &spec, &opts)
                    .is_err(),
                "{bad:?}"
            );
        }
        let no_cwd = LaunchSpec {
            cwd: None,
            ..spec.clone()
        };
        assert!(
            build_terminal_command(
                &TerminalLauncher::Template("kitty --directory {cwd} {args}".into()),
                &no_cwd,
                &opts
            )
            .is_err()
        );
    }

    #[test]
    fn windows_templates_build_raw_command_lines() {
        let spec = spec(
            WIN_PROGRAM,
            ProgramKind::BatchScript,
            TRICKY_ARGS,
            Some(WIN_CWD),
        );
        let opts = options(Platform::Windows);
        let wezterm = TerminalLauncher::Template(
            r#""C:\Program Files\WezTerm\wezterm.exe" start --cwd {cwd} -- {args}"#.into(),
        );

        // {args}: separate MSVC-quoted words. An executable takes any
        // argument.
        let exe = LaunchSpec {
            program: ResolvedProgram {
                path: PathBuf::from(WIN_EXE),
                kind: ProgramKind::Executable,
            },
            ..spec.clone()
        };
        let args = build_terminal_command(&wezterm, &exe, &opts).unwrap();
        assert_eq!(
            args.program,
            OsString::from(r"C:\Program Files\WezTerm\wezterm.exe")
        );
        let parsed = parse_windows_command_line(&format!("x {}", raw(&args)));
        let mut expected = vec!["x", "start", "--cwd", WIN_CWD, "--"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        expected.extend(expected_argv(WIN_EXE));
        assert_eq!(parsed, expected);

        // A batch script runs via `cmd /C`, which only passes plain
        // arguments intact: anything else is refused.
        assert!(build_terminal_command(&wezterm, &spec, &opts).is_err());
        let plain = spec_with_args(&spec, &["--resume", "12345678-1234-4234-9234-123456789abc"]);
        let args = build_terminal_command(&wezterm, &plain, &opts).unwrap();
        assert_eq!(
            parse_windows_command_line(&format!("x {}", raw(&args))),
            [
                "x",
                "start",
                "--cwd",
                WIN_CWD,
                "--",
                sys_cmd().as_str(),
                "/C",
                WIN_PROGRAM,
                "--resume",
                "12345678-1234-4234-9234-123456789abc"
            ]
        );
        // Not a `cmd` template: the terminal starts in the session directory.
        assert_eq!(args.cwd, Some(PathBuf::from(WIN_CWD)));

        // {cmd}: a cmd.exe-ready quoted command for `cmd /S /K {cmd}`.
        let cmd = build_terminal_command(
            &TerminalLauncher::Template("cmd.exe /S /K {cmd}".into()),
            &spec,
            &opts,
        )
        .unwrap();
        assert_eq!(cmd.program, OsString::from(sys_cmd()));
        // `{cmd}` changes into the directory itself.
        assert_eq!(cmd.cwd, Some(windows_system_dir()));
        let raw = raw(&cmd);
        assert!(raw.starts_with("/S /K \"cd /d \""), "{raw}");
        let inner = cmd_s_strip(&raw["/S /K ".len()..]);
        // `&&` is the intended command separator; the rest is the command.
        let (cd, command) = inner.split_once(" && ").unwrap();
        assert_eq!(cd, format!("cd /d \"{WIN_CWD}\""));
        let line = cmd_phase2(command);
        assert_eq!(
            parse_windows_command_line(&line),
            expected_argv(WIN_PROGRAM)
        );
    }

    #[test]
    fn windows_templates_quote_the_directory() {
        let opts = options(Platform::Windows);
        let exe = spec(
            WIN_EXE,
            ProgramKind::Executable,
            &["--resume", "x"],
            Some(WIN_AMP_CWD),
        );
        // MSVC programs get the exact directory; cmd.exe would see it quoted.
        for template in [
            "wezterm start --cwd {cwd} -- {args}",
            "wezterm start --cwd={cwd} -- {args}",
        ] {
            let command =
                build_terminal_command(&TerminalLauncher::Template(template.into()), &exe, &opts)
                    .unwrap();
            let raw = raw(&command);
            assert!(raw.contains(r#"R&D""#), "{raw}");
            // A cmd.exe parse finds no unquoted `&` (it would panic).
            cmd_phase2(raw);
            let parsed = parse_windows_command_line(&format!("x {raw}"));
            assert!(
                parsed.contains(&WIN_AMP_CWD.to_string())
                    || parsed.contains(&format!("--cwd={WIN_AMP_CWD}")),
                "{parsed:?}"
            );
        }
    }

    /// The documented `cmd /C start "" /D {cwd} cmd /S /K {cmd}`: the outer
    /// cmd.exe, then `start`, then the inner `cmd /S /K` each see what they
    /// should, and `&` in the directory never splits a command.
    #[test]
    fn windows_cmd_start_template_survives_both_cmd_layers() {
        let opts = options(Platform::Windows);
        for (kind, program) in [
            (ProgramKind::BatchScript, WIN_PROGRAM),
            (ProgramKind::Executable, WIN_EXE),
        ] {
            for cwd in [WIN_AMP_CWD, WIN_CWD] {
                let spec = spec(program, kind, TRICKY_ARGS, Some(cwd));
                let command = build_terminal_command(
                    &TerminalLauncher::Template(
                        r#"cmd /C start "" /D {cwd} cmd /S /K {cmd}"#.into(),
                    ),
                    &spec,
                    &opts,
                )
                .unwrap();
                // Both bare `cmd` words name the system cmd.exe, and the
                // outer one runs in the system directory.
                assert_eq!(command.program, OsString::from(sys_cmd()));
                assert_eq!(command.cwd, Some(windows_system_dir()));
                let raw = raw(&command);
                let outer = cmd_phase2(raw.strip_prefix("/C ").expect(raw));
                let start_args = format!("start \"\" /D \"{cwd}\" ");
                let inner = outer.strip_prefix(&start_args).expect(&outer);
                let inner = inner
                    .strip_prefix(&format!("{} /S /K ", sys_cmd()))
                    .expect(inner);
                let inner = cmd_s_strip(inner);
                let (cd, command) = inner.split_once(" && ").unwrap();
                assert_eq!(cd, format!("cd /d \"{cwd}\""));
                assert_eq!(
                    parse_windows_command_line(&cmd_phase2(command)),
                    expected_argv(program),
                    "{raw}"
                );
            }
        }
    }

    /// No Windows launch looks `cmd` up by name, and every cmd.exe that
    /// would look a program up runs outside the session directory (a
    /// repository with a planted `cmd.exe` must not run it on resume).
    #[test]
    fn windows_launches_never_look_cmd_up_in_the_session_directory() {
        let opts = options(Platform::Windows);
        let session = PathBuf::from(WIN_AMP_CWD);
        let sys = sys_cmd();
        let spec = spec(
            WIN_PROGRAM,
            ProgramKind::BatchScript,
            &["--resume", "x"],
            Some(WIN_AMP_CWD),
        );
        let wt = TerminalLauncher::WindowsTerminal {
            program: PathBuf::from("wt.exe"),
        };
        let template = |text: &str| TerminalLauncher::Template(text.into());
        // (launcher, the process cwd it must use)
        let cases = [
            (TerminalLauncher::WindowsConsole, windows_system_dir()),
            (wt, windows_system_dir()),
            (
                template(r#"cmd /C start "" /D {cwd} cmd /S /K {cmd}"#),
                windows_system_dir(),
            ),
            (
                template(r#"CMD.EXE /C start "" /D {cwd} {args}"#),
                windows_system_dir(),
            ),
            (template("cmd /S /K {cmd}"), windows_system_dir()),
            // These do not place the directory themselves: they keep it,
            // but still name cmd.exe by absolute path.
            (template("cmd /K {args}"), session.clone()),
            (template("wt -d {cwd} cmd /K {cmd}"), session.clone()),
            (template("wezterm start -- {args}"), session.clone()),
        ];
        for (launcher, cwd) in cases {
            let command = build_terminal_command(&launcher, &spec, &opts).unwrap();
            assert_eq!(command.cwd.as_ref(), Some(&cwd), "{launcher:?}");
            let line = format!("{} {}", command.program.to_string_lossy(), raw(&command));
            // Every word of the command line that names cmd names it by path
            // (whatever layer reads it: strip quotes and carets).
            let words: Vec<&str> = line
                .split_whitespace()
                .map(|word| word.trim_matches(|ch| ch == '"' || ch == '^'))
                .collect();
            assert!(
                words.iter().all(|word| !is_bare_cmd(word)),
                "{launcher:?}: {words:?}"
            );
            assert!(
                words.iter().any(|word| *word == sys),
                "{launcher:?}: {words:?}"
            );
        }
    }

    /// The reported attack against the real `cmd.exe`: a `cmd.exe` planted in
    /// the session directory. The launch command lines (made to wait and
    /// print instead of opening a window) must run the real shell, in the
    /// session directory.
    #[cfg(windows)]
    #[test]
    fn real_cmd_ignores_a_cmd_planted_in_the_session_directory() {
        use std::os::windows::process::CommandExt;

        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("untrusted repo");
        std::fs::create_dir_all(&session).unwrap();
        // The plant: any other program named cmd.exe (it prints nothing
        // useful), plus a cmd.bat for the bare `cmd` of templates.
        std::fs::copy(
            windows_system_dir().join("hostname.exe"),
            session.join("cmd.exe"),
        )
        .unwrap();
        std::fs::write(session.join("cmd.bat"), "@echo PLANTED\r\n").unwrap();
        let agent = root.path().join("bin dir").join("agent.cmd");
        std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
        std::fs::write(&agent, "@echo off\r\necho REAL [%*] [%CD%]\r\n").unwrap();
        let spec = LaunchSpec {
            program: ResolvedProgram {
                path: agent,
                kind: ProgramKind::BatchScript,
            },
            args: vec![OsString::from("--resume"), OsString::from("x")],
            cwd: Some(session.clone()),
        };
        let mut opts = options(Platform::Windows);
        opts.keep_open = false;
        // The command as spawn_terminal runs it, except that the child does
        // not inherit NoDefaultCurrentDirectoryInExePath (which would hide
        // the planting) and the output is captured.
        let run = |program: &OsStr, raw: &str, cwd: Option<&Path>| -> String {
            let mut process = std::process::Command::new(program);
            process
                .raw_arg(raw)
                .env_remove("NoDefaultCurrentDirectoryInExePath");
            if let Some(cwd) = cwd {
                process.current_dir(cwd);
            }
            let output = process.output().unwrap();
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let expected = format!("REAL [--resume x] [{}]", session.display());
        let wait = |raw: &str| raw.replacen("start \"\"", "start \"\" /B /WAIT", 1);

        // Control: the pre-fix shape (bare inner `cmd.exe`, outer cmd in the
        // session directory) runs the plant, so this test can see it.
        let console =
            build_terminal_command(&TerminalLauncher::WindowsConsole, &spec, &opts).unwrap();
        let vulnerable = wait(raw(&console)).replacen(&format!("\"{}\"", sys_cmd()), "cmd.exe", 1);
        let output = run(&console.program, &vulnerable, Some(&session));
        assert!(!output.contains("REAL"), "plant not picked up: {output}");

        // The console launcher as built.
        let output = run(
            &console.program,
            &wait(raw(&console)),
            console.cwd.as_deref(),
        );
        assert_eq!(output.trim_end(), expected);

        // The documented `start` template, waiting.
        let template = TerminalLauncher::Template(
            r#"cmd /C start "" /B /WAIT /D {cwd} cmd /S /C {cmd}"#.into(),
        );
        let command = build_terminal_command(&template, &spec, &opts).unwrap();
        let output = run(&command.program, raw(&command), command.cwd.as_deref());
        assert_eq!(output.trim_end(), expected);
    }

    /// `cmd /C start "" /D {cwd} {args}`: the program and arguments are
    /// escaped for the outer cmd.exe like the built-in launchers.
    #[test]
    fn windows_cmd_template_escapes_args() {
        let opts = options(Platform::Windows);
        let template = TerminalLauncher::Template(r#"cmd /C start "" /D {cwd} {args}"#.into());
        let pi_session = r"C:\Users\R&D\.pi\s.jsonl";
        let mut args: Vec<&str> = TRICKY_ARGS.to_vec();
        args.push(pi_session);
        let exe = spec(WIN_EXE, ProgramKind::Executable, &args, Some(WIN_AMP_CWD));
        let command = build_terminal_command(&template, &exe, &opts).unwrap();
        assert_eq!(command.program, OsString::from(sys_cmd()));
        assert_eq!(command.cwd, Some(windows_system_dir()));
        let raw = raw(&command);
        let outer = cmd_phase2(raw.strip_prefix("/C ").expect(raw));
        let start_args = format!("start \"\" /D \"{WIN_AMP_CWD}\" ");
        let started = outer.strip_prefix(&start_args).expect(&outer);
        let mut expected = expected_argv(WIN_EXE);
        expected.push(pi_session.to_string());
        assert_eq!(parse_windows_command_line(started), expected);

        // A batch script through `{args}` still needs plain arguments.
        let batch = spec(
            WIN_PROGRAM,
            ProgramKind::BatchScript,
            &["--session", pi_session],
            Some(WIN_AMP_CWD),
        );
        assert!(build_terminal_command(&template, &batch, &opts).is_err());
    }

    #[test]
    fn batch_command_arguments_survive_cmd_s_c() {
        let program = ResolvedProgram {
            path: PathBuf::from(WIN_PROGRAM),
            kind: ProgramKind::BatchScript,
        };
        let args: Vec<OsString> = TRICKY_ARGS.iter().map(OsString::from).collect();
        let line = program.batch_command_arguments(&args).unwrap();
        let inner = cmd_s_strip(line.strip_prefix("/D /S /C ").unwrap());
        assert_eq!(
            parse_windows_command_line(&cmd_phase2(&inner)),
            expected_argv(WIN_PROGRAM)
        );
    }

    #[test]
    fn linux_terminal_on_windows_refuses_args_cmd_would_mangle() {
        let launcher = TerminalLauncher::Linux {
            terminal: LinuxTerminal::Wezterm,
            program: PathBuf::from("wezterm.exe"),
        };
        for bad in [r"C:\Users\John Doe\.pi\s.jsonl", "a&echo INJECTED"] {
            let spec = spec(
                WIN_PROGRAM,
                ProgramKind::BatchScript,
                &["--session", bad],
                Some(WIN_CWD),
            );
            assert!(
                build_terminal_command(&launcher, &spec, &options(Platform::Windows)).is_err(),
                "{bad}"
            );
        }
    }

    // ----- Settings and detection. -----

    #[test]
    fn launcher_setting_names_and_templates() {
        assert_eq!(TerminalLauncher::from_setting(""), None);
        assert_eq!(TerminalLauncher::from_setting(" Auto "), None);
        assert_eq!(
            TerminalLauncher::from_setting("WT"),
            Some(TerminalLauncher::WindowsTerminal {
                program: PathBuf::from("wt.exe")
            })
        );
        assert_eq!(
            TerminalLauncher::from_setting("cmd"),
            Some(TerminalLauncher::WindowsConsole)
        );
        assert_eq!(
            TerminalLauncher::from_setting("Terminal"),
            Some(TerminalLauncher::MacTerminal)
        );
        assert_eq!(
            TerminalLauncher::from_setting("osascript"),
            Some(TerminalLauncher::MacOsascript)
        );
        assert_eq!(
            TerminalLauncher::from_setting("konsole"),
            Some(TerminalLauncher::Linux {
                terminal: LinuxTerminal::Konsole,
                program: PathBuf::from("konsole")
            })
        );
        assert_eq!(
            TerminalLauncher::from_setting("kitty --directory {cwd} {args}"),
            Some(TerminalLauncher::Template(
                "kitty --directory {cwd} {args}".to_string()
            ))
        );
        for (name, terminal) in [
            ("ptyxis", LinuxTerminal::Ptyxis),
            ("KGX", LinuxTerminal::Kgx),
            ("tilix", LinuxTerminal::Tilix),
        ] {
            assert_eq!(
                TerminalLauncher::from_setting(name),
                Some(TerminalLauncher::Linux {
                    terminal,
                    program: PathBuf::from(terminal.program())
                }),
                "{name}"
            );
        }
    }

    #[test]
    fn launcher_candidates_are_the_setting_or_every_detected_launcher() {
        // K17: the GUI tries these in order until one spawns.
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mut env = HistoryEnv::sandbox(root.path());
        env.path_var = Some(std::env::join_paths([&bin]).unwrap());
        let options =
            |platform: Platform, terminal: Option<&str>, var: Option<&str>| LaunchOptions {
                platform,
                terminal: terminal.map(str::to_string),
                terminal_var: var.map(OsString::from),
                ..LaunchOptions::default()
            };

        assert_eq!(
            launcher_candidates(&env, &options(Platform::Windows, None, None)),
            vec![TerminalLauncher::WindowsConsole]
        );
        touch_executable(&bin.join("wt.exe"));
        assert_eq!(
            launcher_candidates(&env, &options(Platform::Windows, None, None)),
            vec![
                TerminalLauncher::WindowsTerminal {
                    program: bin.join("wt.exe")
                },
                TerminalLauncher::WindowsConsole
            ]
        );
        assert_eq!(
            launcher_candidates(&env, &options(Platform::MacOs, None, None)),
            vec![
                TerminalLauncher::MacTerminal,
                TerminalLauncher::MacOsascript
            ]
        );

        assert!(launcher_candidates(&env, &options(Platform::Linux, None, None)).is_empty());
        for name in ["xterm", "kitty", "ptyxis", "gnome-terminal", "foot"] {
            touch_executable(&bin.join(name));
        }
        let linux = |terminal: LinuxTerminal| TerminalLauncher::Linux {
            terminal,
            program: bin.join(terminal.program()),
        };
        assert_eq!(
            launcher_candidates(&env, &options(Platform::Linux, None, Some("foot -a x"))),
            vec![
                TerminalLauncher::EnvTerminal {
                    program: bin.join("foot"),
                    args: vec!["-a".to_string(), "x".to_string()],
                },
                linux(LinuxTerminal::GnomeTerminal),
                linux(LinuxTerminal::Ptyxis),
                linux(LinuxTerminal::Kitty),
                linux(LinuxTerminal::Xterm),
            ]
        );
        assert_eq!(
            detect_launcher(&env, Platform::Linux, None),
            Some(linux(LinuxTerminal::GnomeTerminal))
        );

        // A configured launcher or template is the only candidate, found or
        // not; "auto" detects.
        assert_eq!(
            launcher_candidates(&env, &options(Platform::Linux, Some("tilix"), None)),
            vec![TerminalLauncher::Linux {
                terminal: LinuxTerminal::Tilix,
                program: PathBuf::from("tilix"),
            }]
        );
        assert_eq!(
            launcher_candidates(
                &env,
                &options(Platform::Windows, Some("cmd /K {cmd}"), None)
            ),
            vec![TerminalLauncher::Template("cmd /K {cmd}".to_string())]
        );
        assert_eq!(
            launcher_candidates(&env, &options(Platform::MacOs, Some("auto"), None)).len(),
            2
        );
    }

    #[test]
    fn every_launcher_builds_its_command_and_spawning_is_refused_in_tests() {
        let unix = spec(
            "/home/me/bin/claude",
            ProgramKind::Executable,
            &["--resume", "x"],
            Some("/home/me/p"),
        );
        let windows = spec(
            WIN_EXE,
            ProgramKind::Executable,
            &["--resume", "x"],
            Some(WIN_CWD),
        );
        let mut launchers = vec![
            (
                TerminalLauncher::WindowsTerminal {
                    program: PathBuf::from("wt.exe"),
                },
                Platform::Windows,
                "wt.exe".to_string(),
            ),
            (
                TerminalLauncher::WindowsConsole,
                Platform::Windows,
                sys_cmd(),
            ),
            (
                TerminalLauncher::MacTerminal,
                Platform::MacOs,
                "open".to_string(),
            ),
            (
                TerminalLauncher::MacOsascript,
                Platform::MacOs,
                "osascript".to_string(),
            ),
            (
                TerminalLauncher::EnvTerminal {
                    program: PathBuf::from("/usr/bin/foot"),
                    args: Vec::new(),
                },
                Platform::Linux,
                "/usr/bin/foot".to_string(),
            ),
            (
                TerminalLauncher::Template("kitty --directory {cwd} {args}".to_string()),
                Platform::Linux,
                "kitty".to_string(),
            ),
        ];
        launchers.extend(LinuxTerminal::ALL.into_iter().map(|terminal| {
            (
                TerminalLauncher::Linux {
                    terminal,
                    program: PathBuf::from(terminal.program()),
                },
                Platform::Linux,
                terminal.program().to_string(),
            )
        }));
        for (launcher, platform, program) in launchers {
            let spec = if platform.is_windows() {
                &windows
            } else {
                &unix
            };
            let command = build_terminal_command(&launcher, spec, &options(platform)).unwrap();
            assert_eq!(command.program, OsString::from(&program), "{launcher:?}");
            assert!(!command.display().is_empty());
            // The guard rail: tests (and `hermetic` builds) never open a
            // terminal.
            assert_eq!(
                spawn_terminal(&command).unwrap_err().kind(),
                std::io::ErrorKind::Unsupported
            );
            assert_eq!(
                spawn_terminal_checked(&command, std::time::Duration::from_secs(3))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::Unsupported
            );
        }
        // Nothing was written for the macOS script either.
        assert!(!Path::new("/tmp/launch/resume-1.command").exists());
        assert_ne!(SpawnOutcome::Running, SpawnOutcome::Exited(0));
    }

    fn touch_executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn detection_follows_path() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mut env = HistoryEnv::sandbox(root.path());
        env.path_var = Some(std::env::join_paths([&bin]).unwrap());

        assert_eq!(
            detect_launcher(&env, Platform::Windows, None),
            Some(TerminalLauncher::WindowsConsole)
        );
        touch_executable(&bin.join("wt.exe"));
        assert_eq!(
            detect_launcher(&env, Platform::Windows, None),
            Some(TerminalLauncher::WindowsTerminal {
                program: bin.join("wt.exe")
            })
        );
        assert_eq!(
            detect_launcher(&env, Platform::MacOs, None),
            Some(TerminalLauncher::MacTerminal)
        );

        assert_eq!(detect_launcher(&env, Platform::Linux, None), None);
        touch_executable(&bin.join("xterm"));
        touch_executable(&bin.join("konsole"));
        assert_eq!(
            detect_launcher(&env, Platform::Linux, None),
            Some(TerminalLauncher::Linux {
                terminal: LinuxTerminal::Konsole,
                program: bin.join("konsole")
            })
        );
        touch_executable(&bin.join("foot"));
        assert_eq!(
            detect_launcher(&env, Platform::Linux, Some(OsStr::new("foot --server"))),
            Some(TerminalLauncher::EnvTerminal {
                program: bin.join("foot"),
                args: vec!["--server".to_string()]
            })
        );
        // An unknown $TERMINAL falls back to the known emulators.
        assert_eq!(
            detect_launcher(&env, Platform::Linux, Some(OsStr::new("nope"))),
            Some(TerminalLauncher::Linux {
                terminal: LinuxTerminal::Konsole,
                program: bin.join("konsole")
            })
        );
    }

    #[test]
    fn prepare_launcher_command_defaults_cwd_and_makes_fresh_mac_scripts() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let spec = spec("/bin/claude", ProgramKind::Executable, &["x"], None);
        let options = LaunchOptions {
            platform: Platform::MacOs,
            ..LaunchOptions::default()
        };
        let first = prepare_launcher_command(&env, &TerminalLauncher::MacTerminal, &spec, &options)
            .unwrap();
        let second =
            prepare_launcher_command(&env, &TerminalLauncher::MacTerminal, &spec, &options)
                .unwrap();
        let (a, b) = (first.script.unwrap(), second.script.unwrap());
        assert_ne!(a.path, b.path);
        assert!(a.path.starts_with(env.cache_root().unwrap().join("launch")));
        assert!(
            a.contents
                .contains(&quote_posix(root.path().to_str().unwrap()))
        );

        let linux = prepare_launcher_command(
            &env,
            &TerminalLauncher::Template("xterm -e".to_string()),
            &spec,
            &LaunchOptions {
                platform: Platform::Linux,
                ..LaunchOptions::default()
            },
        );
        let command = linux.expect("a template launcher is prepared");
        assert!(command.script.is_none());
        assert_eq!(command.cwd.as_deref(), Some(root.path()));
    }

    #[test]
    fn prepare_uses_the_setting_home_and_cache_script_dir() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let spec = spec(
            "/bin/claude",
            ProgramKind::Executable,
            &["--resume", "x"],
            None,
        );

        let mac = prepare_terminal_command(
            &env,
            &spec,
            &LaunchOptions {
                platform: Platform::MacOs,
                ..LaunchOptions::default()
            },
        )
        .unwrap();
        let script = mac.script.unwrap();
        assert!(
            script
                .path
                .starts_with(env.cache_root().unwrap().join("launch"))
        );
        assert!(is_launch_script(&script.path));
        let home = root.path().to_str().unwrap();
        assert!(
            script.contents.contains(&quote_posix(home)),
            "{}",
            script.contents
        );

        let linux = prepare_terminal_command(
            &env,
            &spec,
            &LaunchOptions {
                platform: Platform::Linux,
                terminal: Some("xterm".to_string()),
                keep_open: false,
                ..LaunchOptions::default()
            },
        )
        .unwrap();
        assert_eq!(linux.program, OsString::from("xterm"));
        assert_eq!(linux.cwd.as_deref(), Some(root.path()));

        // Nothing on PATH and no setting: a clear error on Linux.
        let error = prepare_terminal_command(
            &env,
            &spec,
            &LaunchOptions {
                platform: Platform::Linux,
                ..LaunchOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("[gui].terminal"), "{error}");
    }

    #[test]
    fn posix_quoting() {
        assert_eq!(
            quote_posix("plain-word_1.2/x=y:z@h%+,"),
            "plain-word_1.2/x=y:z@h%+,"
        );
        assert_eq!(quote_posix(""), "''");
        assert_eq!(quote_posix("a b"), "'a b'");
        assert_eq!(quote_posix("it's"), "'it'\\''s'");
        assert_eq!(quote_posix("$HOME `x`"), "'$HOME `x`'");
        assert_eq!(quote_posix("日本"), "'日本'");
        for word in TRICKY_ARGS {
            assert_eq!(
                parse_posix_words(&quote_posix(word)),
                vec![word.to_string()]
            );
        }
    }

    #[test]
    fn display_is_readable() {
        let command = TerminalCommand {
            launcher: "xterm".to_string(),
            program: OsString::from("xterm"),
            args: to_argv(vec!["-e".into(), "a b".into()]),
            cwd: None,
            script: None,
            hide_console: false,
        };
        assert_eq!(command.display(), "xterm -e 'a b'");
    }

    #[cfg(unix)]
    #[test]
    fn launch_scripts_are_written_executable_and_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let script = LaunchScript {
            path: root.path().join("launch").join("x.command"),
            contents: "#!/bin/sh\n".to_string(),
        };
        write_script(&script).unwrap();
        let mode = std::fs::metadata(&script.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(write_script(&script).is_err());
    }
}
