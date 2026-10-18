//! Effective settings (upstream `main.rs:58-71, 160-219`) and the GUI's own
//! command-line options.
//!
//! * [`resolve_bool_setting`] and [`effective_settings`] merge launch flags
//!   with the (layered) configuration exactly like upstream merged its clap
//!   flags with `config.toml`: enable flag > disable flag > config value >
//!   default.
//! * [`parse_launch_args`] is a small hand-written parser (no clap) for the
//!   GUI binary's command line: every upstream top-level flag (the one-shot
//!   actions and compatibility flags too, K14), the `delete-empty` and
//!   `update` subcommands, and `agent ...` ([`crate::agent::cli`], K15), with
//!   upstream's conflict groups and clap-style errors (see [`help_text`]).

use crate::agent::args::TimeRangeArgs;
use crate::config::{ConfigFile, KeyBindings};
use crate::debug::DebugLevel;
use crate::env::WorkspaceScope;
use crate::error::{AppError, Result};
use crate::history::Conversation;
use crate::search::mode::{
    SearchMode, SearchModeResolution, TuiSearchMode, resolve_search_mode, resolve_tui_search_mode,
};
use crate::time_filter::{TimeFilter, TimeFilterError, TimePoint};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// Merge a pair of boolean flags with a config value (`main.rs:55-71`).
///
/// Priority: enable_flag > disable_flag > config_value > default_value
pub fn resolve_bool_setting(
    enable_flag: bool,
    disable_flag: bool,
    config_value: Option<bool>,
    default_value: bool,
) -> bool {
    if enable_flag {
        true
    } else if disable_flag {
        false
    } else {
        config_value.unwrap_or(default_value)
    }
}

/// The viewer's initial tool display. Upstream's `ToolDisplayMode` also has
/// `Truncated`, which is only reachable by cycling inside the viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InitialToolDisplay {
    /// Summary mode ("Searched for 2 patterns, read 1 file").
    #[default]
    Hidden,
    /// Every tool call and result in full.
    Full,
}

impl From<InitialToolDisplay> for crate::view::ToolDisplayMode {
    fn from(display: InitialToolDisplay) -> Self {
        match display {
            InitialToolDisplay::Hidden => Self::Hidden,
            InitialToolDisplay::Full => Self::Full,
        }
    }
}

/// Display flags given on the command line (all `false` = not given).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayFlags {
    pub show_tools: bool,
    pub no_tools: bool,
    pub last: bool,
    pub first: bool,
    pub show_thinking: bool,
    pub hide_thinking: bool,
}

/// Settings after merging flags, configuration and defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveSettings {
    /// List previews show the last messages (`true`, default) or the first.
    pub show_last: bool,
    /// Tool visibility of plain output and exports (upstream `show_tools`).
    pub show_tools: bool,
    /// The viewer's initial tool display.
    pub tool_display: InitialToolDisplay,
    /// Show thinking blocks and subagent internals.
    pub show_thinking: bool,
    /// `[search].mode` (or the deprecated `[tui].semantic_search`), possibly
    /// overridden on the command line.
    pub search_mode: SearchMode,
    /// The list's initial mode: semantic stays semantic, lexical, exact and
    /// hybrid all start lexical.
    pub list_search_mode: TuiSearchMode,
    /// `[tui].exclude_projects`: display names hidden from the list.
    pub exclude_projects: Vec<String>,
    /// `[keys]` with defaults.
    pub keys: KeyBindings,
    /// Whether the lexical/semantic toggle (Ctrl+T) is available: upstream
    /// disables it when an action is bound to Ctrl+T.
    pub semantic_toggle_enabled: bool,
    /// `[resume].default_args` (Claude only).
    pub resume_default_args: Vec<String>,
}

impl EffectiveSettings {
    /// The viewer preferences a session starts with: the initial tool
    /// display and thinking visibility from these settings, timing off (as
    /// upstream, where timing always starts off).
    pub fn initial_view_prefs(&self) -> crate::view::ViewPrefs {
        crate::view::ViewPrefs {
            tool_display: self.tool_display.into(),
            show_thinking: self.show_thinking,
            show_timing: false,
        }
    }
}

/// The viewer's initial tool display (`main.rs:193-205`): `--show-tools`
/// gives Full, `--no-tools` gives Hidden; otherwise `[display].no_tools`
/// `true` gives Hidden, `false` gives **Full**, unset gives Hidden.
pub fn initial_tool_display(
    flags: &DisplayFlags,
    no_tools_config: Option<bool>,
) -> InitialToolDisplay {
    if flags.show_tools {
        InitialToolDisplay::Full
    } else if flags.no_tools {
        InitialToolDisplay::Hidden
    } else {
        match no_tools_config {
            Some(true) => InitialToolDisplay::Hidden,
            Some(false) => InitialToolDisplay::Full,
            None => InitialToolDisplay::Hidden,
        }
    }
}

/// Resolve every setting the GUI starts with from `config` (normally the
/// layered `gui.toml` > `config.toml` configuration), the command-line
/// display `flags` and an optional search-mode override (`--semantic` /
/// `--lexical`).
pub fn effective_settings(
    config: &ConfigFile,
    flags: &DisplayFlags,
    mode_override: Option<SearchMode>,
) -> EffectiveSettings {
    let display = config.display.clone().unwrap_or_default();
    let tui = config.tui.clone().unwrap_or_default();
    let search = config.search.clone().unwrap_or_default();
    let resolution = SearchModeResolution {
        cli_mode: mode_override,
        config_mode: search.mode,
        tui_semantic_search: tui.semantic_search,
    };
    let keys = KeyBindings::from_config(config.keys.clone());
    EffectiveSettings {
        show_last: resolve_bool_setting(flags.last, flags.first, display.last, true),
        show_tools: resolve_bool_setting(
            flags.show_tools,
            flags.no_tools,
            display.no_tools.map(|no_tools| !no_tools),
            false,
        ),
        tool_display: initial_tool_display(flags, display.no_tools),
        show_thinking: resolve_bool_setting(
            flags.show_thinking,
            flags.hide_thinking,
            display.show_thinking,
            false,
        ),
        search_mode: resolve_search_mode(resolution),
        list_search_mode: resolve_tui_search_mode(resolution),
        exclude_projects: tui.exclude_projects,
        semantic_toggle_enabled: !keys.conflicts_with_ctrl_t(),
        keys,
        resume_default_args: config
            .resume
            .as_ref()
            .and_then(|resume| resume.default_args.clone())
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// GUI command line
// ---------------------------------------------------------------------------

/// What to do after the user picks a conversation in picker mode (upstream
/// Ctrl+O followed by `--show-path`, `--show-id` or `--resume`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PickerAction {
    /// Print the transcript path and exit.
    ShowPath,
    /// Print the session id and exit.
    ShowId,
    /// Resume (or fork) the session.
    Resume { fork: bool },
}

/// The output of [`PickerAction::ShowPath`] / [`PickerAction::ShowId`]
/// (`main.rs:469-487`): the path, or the session id (the file stem when the
/// id is empty). `None` for [`PickerAction::Resume`].
pub fn picker_output(action: PickerAction, conversation: &Conversation) -> Result<Option<String>> {
    match action {
        PickerAction::ShowPath => Ok(Some(conversation.path.display().to_string())),
        PickerAction::ShowId => {
            if !conversation.session_id.is_empty() {
                return Ok(Some(conversation.session_id.clone()));
            }
            session_id_from_path(&conversation.path).map(Some)
        }
        PickerAction::Resume { .. } => Ok(None),
    }
}

/// Upstream's `--show-id` fallback: the file stem.
pub fn session_id_from_path(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::ClaudeExecutionError("Conversation filename is not valid Unicode".to_string())
        })
}

/// The GUI binary's command-line options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuiLaunchArgs {
    /// `[FILE]`: open one JSONL transcript in the viewer.
    pub file: Option<PathBuf>,
    /// `-L, --local`: start with the workspace filter on.
    pub local: bool,
    /// `-w, --workspace <DIR>`: the workspace folder, as typed (it may be
    /// relative or end with a separator). Turn it into a scope with
    /// [`GuiLaunchArgs::workspace_scope`].
    pub workspace: Option<PathBuf>,
    /// `--since`, `--after`, `--before`.
    pub time: TimeRangeArgs,
    /// `--first`
    pub first: bool,
    /// `-l, --last`
    pub last: bool,
    /// `-t, --show-tools`
    pub show_tools: bool,
    /// `--no-tools`
    pub no_tools: bool,
    /// `--show-thinking`
    pub show_thinking: bool,
    /// `--hide-thinking`
    pub hide_thinking: bool,
    /// `-p, --show-path` (picker mode)
    pub show_path: bool,
    /// `-i, --show-id` (picker mode)
    pub show_id: bool,
    /// `-c, --resume` (picker mode)
    pub resume: bool,
    /// `--fork-session` (requires `--resume`)
    pub fork_session: bool,
    /// `--debug [LEVEL]`
    pub debug: Option<DebugLevel>,
    /// `--semantic`
    pub semantic: bool,
    /// `--lexical`
    pub lexical: bool,
    /// `--plain`: picker output in the CLI plain format.
    pub plain: bool,
    /// `--render <FILE>`
    pub render: Option<PathBuf>,
    /// `-d, --show-dir`
    pub show_dir: bool,
    /// `--delete <SESSION_ID>`
    pub delete: Option<String>,
    /// `--debug-search <QUERY>`
    pub debug_search: Option<String>,
    /// `--semantic-search <QUERY>` (hidden; trimmed, non-empty)
    pub semantic_search: Option<String>,
    /// `--semantic-top <SEMANTIC_TOP>` (hidden; requires `--semantic-search`;
    /// [`DEFAULT_SEMANTIC_TOP`] when not given)
    pub semantic_top: Option<usize>,
    /// `--debug-semantic-search <QUERY>` (hidden; trimmed, non-empty)
    pub debug_semantic_search: Option<String>,
    /// `--generate-semantic-cache` (hidden)
    pub generate_semantic_cache: bool,
    /// `--clear-semantic-cache` (hidden)
    pub clear_semantic_cache: bool,
    /// `-g, --global` (hidden, deprecated: global is the default; no effect)
    pub global: bool,
    /// `--pager` (accepted; the GUI has no pager)
    pub pager: bool,
    /// `--no-pager` (accepted; no effect)
    pub no_pager: bool,
    /// `--no-color` (accepted; no effect)
    pub no_color: bool,
}

/// `--semantic-top` when not given (upstream `default_value_t = 20`).
pub const DEFAULT_SEMANTIC_TOP: usize = 20;

/// Result of [`parse_launch_args`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchCommand {
    /// Start the GUI.
    Run(GuiLaunchArgs),
    /// `-h, --help`: print [`help_text`] and exit.
    Help,
    /// `-V, --version`: print the version and exit.
    Version,
    /// `delete-empty [--local|--all] [--yes]` (`--local` and `--all`
    /// conflict).
    DeleteEmpty { local: bool, all: bool, yes: bool },
    /// `update`
    Update,
    /// `agent search|within|read|outline ...` (K15).
    Agent(crate::agent::cli::AgentCli),
    /// The help of a subcommand (`update --help`, `delete-empty -h`,
    /// `help update`, `help help`): print this text to stdout and exit 0.
    /// `help` alone is [`LaunchCommand::Help`], and `help agent [...]` is
    /// [`LaunchCommand::Agent`] with [`crate::agent::cli::AgentCli::Help`].
    SubcommandHelp(String),
}

/// A command-line error (clap-style message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchArgsError(pub String);

impl fmt::Display for LaunchArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {}", self.0)
    }
}

impl std::error::Error for LaunchArgsError {}

fn error(message: impl Into<String>) -> LaunchArgsError {
    LaunchArgsError(message.into())
}

fn conflict(first: &str, second: &str) -> LaunchArgsError {
    error(format!(
        "the argument '{first}' cannot be used with '{second}'"
    ))
}

impl GuiLaunchArgs {
    /// `--workspace` as a validated, normalized [`WorkspaceScope`].
    ///
    /// A relative value is resolved against `launch_dir`, the directory the
    /// GUI was started from (only the GUI's `main` knows it; core never reads
    /// the process cwd). Without a `launch_dir` a relative value is an
    /// [`AppError::InvalidWorkspace`] error, as is a value that is still not
    /// absolute (for example `C:proj` on Windows).
    pub fn workspace_scope(&self, launch_dir: Option<&Path>) -> Result<Option<WorkspaceScope>> {
        let Some(dir) = &self.workspace else {
            return Ok(None);
        };
        let dir = match launch_dir {
            Some(base) if dir.is_relative() => base.join(dir),
            _ => dir.clone(),
        };
        WorkspaceScope::new(dir).map(Some)
    }

    /// The display flags for [`effective_settings`].
    pub fn display_flags(&self) -> DisplayFlags {
        DisplayFlags {
            show_tools: self.show_tools,
            no_tools: self.no_tools,
            last: self.last,
            first: self.first,
            show_thinking: self.show_thinking,
            hide_thinking: self.hide_thinking,
        }
    }

    /// `--semantic` / `--lexical`.
    pub fn search_mode_override(&self) -> Option<SearchMode> {
        if self.semantic {
            Some(SearchMode::Semantic)
        } else if self.lexical {
            Some(SearchMode::Lexical)
        } else {
            None
        }
    }

    /// The picker action, in upstream's precedence: `--show-path`, then
    /// `--show-id`, then `--resume`.
    pub fn picker(&self) -> Option<PickerAction> {
        if self.show_path {
            Some(PickerAction::ShowPath)
        } else if self.show_id {
            Some(PickerAction::ShowId)
        } else if self.resume {
            Some(PickerAction::Resume {
                fork: self.fork_session,
            })
        } else {
            None
        }
    }

    /// Resolve `--since/--after/--before` against the current time.
    pub fn time_filter(&self) -> std::result::Result<TimeFilter, TimeFilterError> {
        self.time.resolve()
    }

    /// [`effective_settings`] with these flags.
    pub fn settings(&self, config: &ConfigFile) -> EffectiveSettings {
        effective_settings(config, &self.display_flags(), self.search_mode_override())
    }

    /// `--semantic-top`, or [`DEFAULT_SEMANTIC_TOP`].
    pub fn semantic_top_or_default(&self) -> usize {
        self.semantic_top.unwrap_or(DEFAULT_SEMANTIC_TOP)
    }

    /// The one-shot actions given (group `one_shot_action`), in upstream's
    /// group order, with their clap display names.
    fn one_shot_actions(&self) -> Vec<&'static str> {
        [
            (self.show_dir, "--show-dir"),
            (self.delete.is_some(), "--delete <SESSION_ID>"),
            (self.debug_search.is_some(), "--debug-search <QUERY>"),
            (
                self.debug_semantic_search.is_some(),
                "--debug-semantic-search <QUERY>",
            ),
            (self.semantic_search.is_some(), "--semantic-search <QUERY>"),
            (self.generate_semantic_cache, "--generate-semantic-cache"),
            (self.clear_semantic_cache, "--clear-semantic-cache"),
            (self.render.is_some(), "--render <FILE>"),
            (self.file.is_some(), "[FILE]"),
        ]
        .into_iter()
        .filter_map(|(set, name)| set.then_some(name))
        .collect()
    }

    /// The conflicts and requirements upstream's clap definition enforced
    /// (K14): the flag pairs, the `one_shot_action` group (at most one
    /// action), the `selection_conflicting_action` group (the actions except
    /// `--show-dir` and `--render` conflict with `--resume`, `--show-path`,
    /// `--show-id` and `--plain`), `--delete` × `--global`, `[FILE]` ×
    /// `--local`/`--global`, and the requirements of `--fork-session` and
    /// `--semantic-top`. The picker flags combine as upstream
    /// ([`GuiLaunchArgs::picker`] applies its precedence).
    pub fn validate(&self) -> std::result::Result<(), LaunchArgsError> {
        let pairs = [
            (self.first, "--first", self.last, "--last"),
            (self.show_tools, "--show-tools", self.no_tools, "--no-tools"),
            (
                self.show_thinking,
                "--show-thinking",
                self.hide_thinking,
                "--hide-thinking",
            ),
            (self.semantic, "--semantic", self.lexical, "--lexical"),
            (self.pager, "--pager", self.no_pager, "--no-pager"),
            (
                self.delete.is_some(),
                "--delete <SESSION_ID>",
                self.global,
                "--global",
            ),
            (self.file.is_some(), "[FILE]", self.local, "--local"),
            (self.file.is_some(), "[FILE]", self.global, "--global"),
        ];
        for (left, left_name, right, right_name) in pairs {
            if left && right {
                return Err(conflict(left_name, right_name));
            }
        }
        if let [first, second, ..] = self.one_shot_actions().as_slice() {
            return Err(conflict(first, second));
        }
        let selection_conflicting = [
            (self.delete.is_some(), "--delete <SESSION_ID>"),
            (self.debug_search.is_some(), "--debug-search <QUERY>"),
            (
                self.debug_semantic_search.is_some(),
                "--debug-semantic-search <QUERY>",
            ),
            (self.semantic_search.is_some(), "--semantic-search <QUERY>"),
            (self.generate_semantic_cache, "--generate-semantic-cache"),
            (self.clear_semantic_cache, "--clear-semantic-cache"),
            (self.file.is_some(), "[FILE]"),
        ];
        let selection = [
            (self.resume, "--resume"),
            (self.show_path, "--show-path"),
            (self.show_id, "--show-id"),
            (self.plain, "--plain"),
        ];
        for (action, action_name) in selection_conflicting {
            for (set, name) in selection {
                if action && set {
                    return Err(conflict(action_name, name));
                }
            }
        }
        self.time.validate().map_err(LaunchArgsError)?;
        if self.fork_session && !self.resume {
            return Err(error(
                "the following required arguments were not provided: --resume (required by --fork-session)",
            ));
        }
        if self.semantic_top.is_some() && self.semantic_search.is_none() {
            return Err(error(
                "the following required arguments were not provided: --semantic-search <QUERY> (required by --semantic-top)",
            ));
        }
        Ok(())
    }
}

/// The usage text for `--help`. The hidden upstream flags (`--global`,
/// `--semantic-search`, `--semantic-top`, `--debug-semantic-search`,
/// `--generate-semantic-cache`, `--clear-semantic-cache`) are accepted but
/// not listed, as upstream.
pub fn help_text(program: &str) -> String {
    format!(
        "Browse Claude Code, Pi and OMP conversation history

Usage: {program} [OPTIONS] [FILE]
       {program} delete-empty [--local|--all] [--yes]
       {program} update
       {program} agent <search|within|read|outline> ...

Commands:
  agent         Run agent-oriented search and transcript commands
  delete-empty  Delete transcript files with no Claude messages
  update        Check for a newer release
  help          Print this message or the help of the given subcommand(s)

Arguments:
  [FILE]  Open a JSONL transcript in the viewer

Options:
  -L, --local                   Start with the workspace filter on
  -w, --workspace <DIR>         Workspace folder (used for the workspace filter, resume and fork)
      --since <WHEN>            Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)
      --after <WHEN>            Alias for --since
      --before <WHEN>           Only conversations older than this duration or date
      --first                   Preview the first messages of each conversation
  -l, --last                    Preview the last messages of each conversation (default)
  -t, --show-tools              Start the viewer with tool calls shown in full
      --no-tools                Start the viewer with tool calls summarized (default)
      --show-thinking           Show thinking blocks and subagent internals
      --hide-thinking           Hide thinking blocks and subagent internals (default)
  -p, --show-path               Picker mode: print the selected conversation's path and exit
  -i, --show-id                 Picker mode: print the selected conversation's session id and exit
  -c, --resume                  Picker mode: resume the selected conversation
      --fork-session            With --resume: fork instead of resuming
      --plain                   Picker mode: print plain text without ledger formatting
  -d, --show-dir                Print the conversation directory path and exit
      --render <FILE>           Render a JSONL file in ledger format and exit
      --delete <SESSION_ID>     Delete a session by its UUID (asks for confirmation)
      --debug-search <QUERY>    Debug search result scoring for a query
      --debug [<LEVEL>]         Enable debug output [levels: debug, info, warn, error; default: debug]
      --semantic                Start the list in semantic search mode
      --lexical                 Start the list in lexical search mode
      --pager                   Accepted for compatibility (no effect)
      --no-pager                Accepted for compatibility (no effect)
      --no-color                Accepted for compatibility (no effect)
  -h, --help                    Print help
  -V, --version                 Print version
"
    )
}

/// Parse the current process's arguments (without the program name).
pub fn parse_process_args() -> std::result::Result<LaunchCommand, LaunchArgsError> {
    parse_launch_args(std::env::args_os().skip(1))
}

/// Option metadata: canonical long name and whether it takes a value.
fn long_option(name: &str) -> Option<(&'static str, bool)> {
    const OPTIONS: &[(&str, bool)] = &[
        ("local", false),
        ("workspace", true),
        ("since", true),
        ("after", true),
        ("before", true),
        ("first", false),
        ("last", false),
        ("show-tools", false),
        ("no-tools", false),
        ("show-thinking", false),
        ("hide-thinking", false),
        ("show-path", false),
        ("show-id", false),
        ("resume", false),
        ("fork-session", false),
        ("debug", false),
        ("semantic", false),
        ("lexical", false),
        ("help", false),
        ("version", false),
        ("plain", false),
        ("render", true),
        ("show-dir", false),
        ("delete", true),
        ("debug-search", true),
        ("semantic-search", true),
        ("semantic-top", true),
        ("debug-semantic-search", true),
        ("generate-semantic-cache", false),
        ("clear-semantic-cache", false),
        ("global", false),
        ("pager", false),
        ("no-pager", false),
        ("no-color", false),
    ];
    OPTIONS
        .iter()
        .find(|(option, _)| *option == name)
        .map(|(option, takes_value)| (*option, *takes_value))
}

fn short_option(flag: char) -> Option<(&'static str, bool)> {
    Some(match flag {
        'L' => ("local", false),
        'w' => ("workspace", true),
        'l' => ("last", false),
        't' => ("show-tools", false),
        'p' => ("show-path", false),
        'i' => ("show-id", false),
        'c' => ("resume", false),
        'd' => ("show-dir", false),
        'g' => ("global", false),
        'h' => ("help", false),
        'V' => ("version", false),
        _ => return None,
    })
}

fn value_name(option: &str) -> &'static str {
    match option {
        "workspace" => "<DIR>",
        "render" => "<FILE>",
        "delete" => "<SESSION_ID>",
        "debug-search" | "semantic-search" | "debug-semantic-search" => "<QUERY>",
        "semantic-top" => "<SEMANTIC_TOP>",
        _ => "<WHEN>",
    }
}

/// clap `value_parser = non_empty_string` (`cli.rs:486-493`): trimmed, not
/// empty.
fn non_empty_value(option: &str, value: String) -> std::result::Result<String, LaunchArgsError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(error(format!(
            "invalid value '{value}' for '--{option} {}': value must not be empty",
            value_name(option)
        )));
    }
    Ok(trimmed.to_string())
}

/// clap `value_parser = non_zero_usize` (`cli.rs:495-504`).
fn non_zero_value(option: &str, value: &str) -> std::result::Result<usize, LaunchArgsError> {
    let invalid = |reason: String| {
        error(format!(
            "invalid value '{value}' for '--{option} {}': {reason}",
            value_name(option)
        ))
    };
    let parsed = value
        .parse::<usize>()
        .map_err(|_| invalid(format!("invalid positive integer: {value}")))?;
    if parsed == 0 {
        return Err(invalid("value must be greater than zero".to_string()));
    }
    Ok(parsed)
}

/// The subcommands (upstream `Commands`, plus the `help` clap adds).
const SUBCOMMANDS: [&str; 4] = ["agent", "delete-empty", "help", "update"];

/// The program name in subcommand help texts (as in `agent` help).
const PROGRAM: &str = "claude-history-gui";

/// What clap prints for `<name> --help` and `help <name>` (`name` is
/// `delete-empty`, `update` or `help`).
fn subcommand_help(name: &str) -> String {
    match name {
        "delete-empty" => format!(
            "Delete transcript files with no Claude messages

Usage: {PROGRAM} delete-empty [OPTIONS]

Options:
      --yes    Delete matching transcripts instead of printing a dry run
      --local  Only inspect the current workspace
      --all    Inspect all workspaces
  -h, --help   Print help
"
        ),
        "update" => format!(
            "Check for a newer release

Usage: {PROGRAM} update

Options:
  -h, --help  Print help
"
        ),
        _ => format!(
            "Print this message or the help of the given subcommand(s)

Usage: {PROGRAM} help [COMMAND]...

Arguments:
  [COMMAND]...  Print help for the subcommand(s)
"
        ),
    }
}

/// `help [COMMAND]...` (the subcommand clap adds): `help` is the top-level
/// help, `help <name>` that subcommand's help, `help agent <name>` the help
/// of `agent <name>`.
fn parse_help_subcommand(args: &[OsString]) -> std::result::Result<LaunchCommand, LaunchArgsError> {
    let names = args
        .iter()
        .map(|arg| {
            arg.to_str()
                .ok_or_else(|| error("invalid UTF-8 was detected in one or more arguments"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let unrecognized = |name: &str| {
        if name.starts_with('-') {
            error(format!("unexpected argument '{name}' found"))
        } else {
            error(format!("unrecognized subcommand '{name}'"))
        }
    };
    let Some((&name, rest)) = names.split_first() else {
        return Ok(LaunchCommand::Help);
    };
    match name {
        "agent" => {
            crate::agent::cli::parse_agent_args(std::iter::once("help").chain(rest.iter().copied()))
                .map(LaunchCommand::Agent)
        }
        "delete-empty" | "update" | "help" => match rest.first() {
            None => Ok(LaunchCommand::SubcommandHelp(subcommand_help(name))),
            Some(extra) => Err(unrecognized(extra)),
        },
        other => Err(unrecognized(other)),
    }
}

/// `delete-empty [--yes] [--local|--all]` and `update` (`cli.rs:46-66`).
fn parse_simple_subcommand(
    name: &str,
    args: &[OsString],
) -> std::result::Result<LaunchCommand, LaunchArgsError> {
    let (mut local, mut all, mut yes) = (false, false, false);
    let mut seen = HashSet::new();
    for arg in args {
        let text = arg
            .to_str()
            .ok_or_else(|| error("invalid UTF-8 was detected in one or more arguments"))?;
        if text == "-h" || text == "--help" {
            return Ok(LaunchCommand::SubcommandHelp(subcommand_help(name)));
        }
        let flag = match (name, text) {
            ("delete-empty", "--local") => &mut local,
            ("delete-empty", "--all") => &mut all,
            ("delete-empty", "--yes") => &mut yes,
            _ => return Err(error(format!("unexpected argument '{text}' found"))),
        };
        if !seen.insert(text) {
            return Err(error(format!(
                "the argument '{text}' cannot be used multiple times"
            )));
        }
        *flag = true;
    }
    if name == "update" {
        return Ok(LaunchCommand::Update);
    }
    if local && all {
        return Err(conflict("--local", "--all"));
    }
    Ok(LaunchCommand::DeleteEmpty { local, all, yes })
}

fn utf8_value(value: OsString) -> std::result::Result<String, LaunchArgsError> {
    value
        .into_string()
        .map_err(|_| error("invalid UTF-8 was detected in one or more arguments"))
}

fn parse_time_point(option: &str, value: &str) -> std::result::Result<TimePoint, LaunchArgsError> {
    value.parse::<TimePoint>().map_err(|err| {
        error(format!(
            "invalid value '{value}' for '--{option} {}': {err}",
            value_name(option)
        ))
    })
}

/// Parse the GUI's options (`args` excludes the program name).
///
/// Accepts `--long`, `--long=value`, `--long value`, short flags (`-t`,
/// clusters such as `-tL`, `-w DIR`, `-wDIR`) and `--` to end options.
/// `--debug` takes an optional level: `--debug=LEVEL`, or `--debug LEVEL`
/// when the next argument is a level name (unlike upstream, a following file
/// name is not swallowed as a level). Validation runs on the result.
///
/// A first argument `agent`, `delete-empty`, `update` or `help` selects
/// that subcommand ([`LaunchCommand::Agent`], [`LaunchCommand::DeleteEmpty`],
/// [`LaunchCommand::Update`], and for `help` and a subcommand's `--help`
/// [`LaunchCommand::Help`] or [`LaunchCommand::SubcommandHelp`]); as
/// upstream, subcommands cannot be combined with the top-level options.
/// After `--` these names are file names.
pub fn parse_launch_args<I, T>(args: I) -> std::result::Result<LaunchCommand, LaunchArgsError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let all = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    if let Some(name) = all.first().and_then(|first| first.to_str())
        && SUBCOMMANDS.contains(&name)
    {
        return match name {
            "agent" => crate::agent::cli::parse_agent_args(all[1..].iter().cloned())
                .map(LaunchCommand::Agent),
            "help" => parse_help_subcommand(&all[1..]),
            name => parse_simple_subcommand(name, &all[1..]),
        };
    }
    let mut args = all.into_iter().peekable();
    let mut parsed = GuiLaunchArgs::default();
    let mut seen = HashSet::new();
    let mut only_positionals = false;
    // The first option given (for the subcommand conflict message).
    let mut first_option: Option<String> = None;

    while let Some(arg) = args.next() {
        let text = arg.to_str().map(str::to_string);
        let is_option = !only_positionals
            && text
                .as_deref()
                .is_some_and(|text| text.starts_with('-') && text != "-");
        if !is_option {
            if !only_positionals
                && let (Some(name), Some(option)) = (text.as_deref(), first_option.as_deref())
                && SUBCOMMANDS.contains(&name)
            {
                return Err(error(format!(
                    "the subcommand '{name}' cannot be used with '{option}'"
                )));
            }
            if parsed.file.is_some() {
                return Err(error(format!(
                    "unexpected argument '{}' found",
                    arg.to_string_lossy()
                )));
            }
            parsed.file = Some(PathBuf::from(arg));
            continue;
        }
        let text = text.expect("options are UTF-8");
        if text == "--" {
            only_positionals = true;
            continue;
        }
        if first_option.is_none() {
            first_option = Some(text.split('=').next().unwrap_or(&text).to_string());
        }

        // (canonical long name, inline value, display spelling)
        let mut pending: Vec<(&'static str, Option<String>, String)> = Vec::new();
        if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            let (option, _) = long_option(name)
                .ok_or_else(|| error(format!("unexpected argument '{text}' found")))?;
            pending.push((option, inline, format!("--{name}")));
        } else {
            let flags: Vec<char> = text[1..].chars().collect();
            let mut index = 0;
            while index < flags.len() {
                let flag = flags[index];
                let (option, takes_value) = short_option(flag)
                    .ok_or_else(|| error(format!("unexpected argument '-{flag}' found")))?;
                if takes_value {
                    let rest: String = flags[index + 1..].iter().collect();
                    let inline =
                        (!rest.is_empty()).then(|| rest.trim_start_matches('=').to_string());
                    pending.push((option, inline, format!("-{flag}")));
                    break;
                }
                pending.push((option, None, format!("-{flag}")));
                index += 1;
            }
        }

        for (option, inline, spelling) in pending {
            if !seen.insert(option) {
                return Err(error(format!(
                    "the argument '--{option}' cannot be used multiple times"
                )));
            }
            let (_, takes_value) = long_option(option).expect("known option");
            if option == "debug" {
                let level = match inline {
                    Some(value) => Some(value),
                    None => match args.peek().and_then(|next| next.to_str()) {
                        Some(next) if next.parse::<DebugLevel>().is_ok() => {
                            Some(args.next().unwrap().into_string().unwrap())
                        }
                        _ => None,
                    },
                };
                parsed.debug = Some(match level {
                    None => DebugLevel::Debug,
                    Some(level) => level.parse::<DebugLevel>().map_err(|err| {
                        error(format!(
                            "invalid value '{level}' for '--debug [<LEVEL>]': {err}"
                        ))
                    })?,
                });
                continue;
            }
            if !takes_value {
                if inline.is_some() {
                    return Err(error(format!(
                        "unexpected value for '{spelling}': it takes no value"
                    )));
                }
                match option {
                    "local" => parsed.local = true,
                    "first" => parsed.first = true,
                    "last" => parsed.last = true,
                    "show-tools" => parsed.show_tools = true,
                    "no-tools" => parsed.no_tools = true,
                    "show-thinking" => parsed.show_thinking = true,
                    "hide-thinking" => parsed.hide_thinking = true,
                    "show-path" => parsed.show_path = true,
                    "show-id" => parsed.show_id = true,
                    "resume" => parsed.resume = true,
                    "fork-session" => parsed.fork_session = true,
                    "semantic" => parsed.semantic = true,
                    "lexical" => parsed.lexical = true,
                    "plain" => parsed.plain = true,
                    "show-dir" => parsed.show_dir = true,
                    "generate-semantic-cache" => parsed.generate_semantic_cache = true,
                    "clear-semantic-cache" => parsed.clear_semantic_cache = true,
                    "global" => parsed.global = true,
                    "pager" => parsed.pager = true,
                    "no-pager" => parsed.no_pager = true,
                    "no-color" => parsed.no_color = true,
                    "help" => return Ok(LaunchCommand::Help),
                    "version" => return Ok(LaunchCommand::Version),
                    other => unreachable!("flag {other}"),
                }
                continue;
            }
            let value = match inline {
                Some(value) => OsString::from(value),
                None => args.next().ok_or_else(|| {
                    error(format!(
                        "a value is required for '--{option} {}' but none was supplied",
                        value_name(option)
                    ))
                })?,
            };
            match option {
                "workspace" => {
                    if value.is_empty() {
                        return Err(error(
                            "a value is required for '--workspace <DIR>' but none was supplied",
                        ));
                    }
                    parsed.workspace = Some(PathBuf::from(value));
                }
                "since" | "after" | "before" => {
                    let value = utf8_value(value)?;
                    let point = parse_time_point(option, &value)?;
                    match option {
                        "since" => parsed.time.since = Some(point),
                        "after" => parsed.time.after = Some(point),
                        _ => parsed.time.before = Some(point),
                    }
                }
                "render" => parsed.render = Some(PathBuf::from(value)),
                "delete" => parsed.delete = Some(utf8_value(value)?),
                "debug-search" => parsed.debug_search = Some(utf8_value(value)?),
                "semantic-search" => {
                    parsed.semantic_search = Some(non_empty_value(option, utf8_value(value)?)?);
                }
                "debug-semantic-search" => {
                    parsed.debug_semantic_search =
                        Some(non_empty_value(option, utf8_value(value)?)?);
                }
                "semantic-top" => {
                    parsed.semantic_top = Some(non_zero_value(option, &utf8_value(value)?)?);
                }
                other => unreachable!("option {other}"),
            }
        }
    }

    parsed.validate()?;
    Ok(LaunchCommand::Run(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        DisplayConfig, Key, KeyChord, KeysConfig, ResumeConfig, SearchConfig, TuiConfig,
    };

    fn run(args: &[&str]) -> GuiLaunchArgs {
        match parse_launch_args(args.iter().copied()) {
            Ok(LaunchCommand::Run(parsed)) => parsed,
            other => panic!("{args:?} -> {other:?}"),
        }
    }

    fn fail(args: &[&str]) -> String {
        match parse_launch_args(args.iter().copied()) {
            Err(error) => error.0,
            other => panic!("{args:?} should fail, got {other:?}"),
        }
    }

    // --- resolve_bool_setting (upstream semantics) ---

    #[test]
    fn bool_setting_priority() {
        assert!(resolve_bool_setting(true, true, Some(false), false));
        assert!(!resolve_bool_setting(false, true, Some(true), true));
        assert!(resolve_bool_setting(false, false, Some(true), false));
        assert!(!resolve_bool_setting(false, false, Some(false), true));
        assert!(resolve_bool_setting(false, false, None, true));
        assert!(!resolve_bool_setting(false, false, None, false));
    }

    #[test]
    fn defaults_without_config_or_flags() {
        let settings = effective_settings(&ConfigFile::default(), &DisplayFlags::default(), None);
        assert_eq!(
            settings,
            EffectiveSettings {
                show_last: true,
                show_tools: false,
                tool_display: InitialToolDisplay::Hidden,
                show_thinking: false,
                search_mode: SearchMode::Lexical,
                list_search_mode: TuiSearchMode::Lexical,
                exclude_projects: Vec::new(),
                keys: KeyBindings::default(),
                semantic_toggle_enabled: true,
                resume_default_args: Vec::new(),
            }
        );
    }

    #[test]
    fn tool_display_mapping_matches_upstream() {
        let none = DisplayFlags::default();
        assert_eq!(
            initial_tool_display(&none, None),
            InitialToolDisplay::Hidden
        );
        assert_eq!(
            initial_tool_display(&none, Some(true)),
            InitialToolDisplay::Hidden
        );
        // Upstream quirk kept: no_tools = false starts in Full, not summary.
        assert_eq!(
            initial_tool_display(&none, Some(false)),
            InitialToolDisplay::Full
        );
        let show = DisplayFlags {
            show_tools: true,
            ..none
        };
        assert_eq!(
            initial_tool_display(&show, Some(true)),
            InitialToolDisplay::Full
        );
        let hide = DisplayFlags {
            no_tools: true,
            ..none
        };
        assert_eq!(
            initial_tool_display(&hide, Some(false)),
            InitialToolDisplay::Hidden
        );

        let config = ConfigFile {
            display: Some(DisplayConfig {
                no_tools: Some(false),
                ..DisplayConfig::default()
            }),
            ..ConfigFile::default()
        };
        let settings = effective_settings(&config, &none, None);
        assert!(settings.show_tools);
        assert_eq!(settings.tool_display, InitialToolDisplay::Full);
        let settings = effective_settings(&config, &hide, None);
        assert!(!settings.show_tools);
    }

    #[test]
    fn initial_view_prefs_map_settings_to_the_viewer() {
        use crate::view::{ToolDisplayMode, ViewPrefs};

        assert_eq!(
            ToolDisplayMode::from(InitialToolDisplay::Hidden),
            ToolDisplayMode::Hidden
        );
        assert_eq!(
            ToolDisplayMode::from(InitialToolDisplay::Full),
            ToolDisplayMode::Full
        );

        let config = ConfigFile {
            display: Some(DisplayConfig {
                no_tools: Some(false),
                ..DisplayConfig::default()
            }),
            ..ConfigFile::default()
        };
        let show_thinking = DisplayFlags {
            show_thinking: true,
            ..DisplayFlags::default()
        };
        let settings = effective_settings(&config, &show_thinking, None);
        assert_eq!(
            settings.initial_view_prefs(),
            ViewPrefs {
                tool_display: ToolDisplayMode::Full,
                show_thinking: true,
                show_timing: false,
            }
        );

        let settings = effective_settings(&ConfigFile::default(), &DisplayFlags::default(), None);
        assert_eq!(
            settings.initial_view_prefs(),
            ViewPrefs {
                tool_display: ToolDisplayMode::Hidden,
                show_thinking: settings.show_thinking,
                show_timing: false,
            }
        );
    }

    #[test]
    fn config_values_and_flags_merge() {
        let config = ConfigFile {
            display: Some(DisplayConfig {
                last: Some(false),
                show_thinking: Some(true),
                ..DisplayConfig::default()
            }),
            resume: Some(ResumeConfig {
                default_args: Some(vec!["--model".into(), "opus".into()]),
            }),
            tui: Some(TuiConfig {
                exclude_projects: vec!["secret".into()],
                semantic_search: None,
            }),
            keys: Some(KeysConfig {
                rename: Some(KeyChord::ctrl('t')),
                ..KeysConfig::default()
            }),
            ..ConfigFile::default()
        };
        let settings = effective_settings(&config, &DisplayFlags::default(), None);
        assert!(!settings.show_last);
        assert!(settings.show_thinking);
        assert_eq!(settings.resume_default_args, vec!["--model", "opus"]);
        assert_eq!(settings.exclude_projects, vec!["secret"]);
        assert_eq!(settings.keys.rename, KeyChord::ctrl('t'));
        assert!(!settings.semantic_toggle_enabled);

        let flags = DisplayFlags {
            last: true,
            hide_thinking: true,
            ..DisplayFlags::default()
        };
        let settings = effective_settings(&config, &flags, None);
        assert!(settings.show_last);
        assert!(!settings.show_thinking);
        assert_eq!(settings.keys.fork, KeyChord::ctrl('f'));
        assert_eq!(settings.keys.rename.key, Key::Char('t'));
    }

    #[test]
    fn search_mode_resolution_includes_the_deprecated_tui_key() {
        let legacy = |semantic: Option<bool>, mode: Option<SearchMode>| ConfigFile {
            tui: Some(TuiConfig {
                exclude_projects: Vec::new(),
                semantic_search: semantic,
            }),
            search: Some(SearchConfig { mode }),
            ..ConfigFile::default()
        };
        let flags = DisplayFlags::default();
        let settings = effective_settings(&legacy(Some(true), None), &flags, None);
        assert_eq!(settings.search_mode, SearchMode::Semantic);
        assert_eq!(settings.list_search_mode, TuiSearchMode::Semantic);
        // [search].mode wins over the legacy key.
        let settings =
            effective_settings(&legacy(Some(true), Some(SearchMode::Lexical)), &flags, None);
        assert_eq!(settings.list_search_mode, TuiSearchMode::Lexical);
        // Hybrid and exact start the list in lexical mode.
        for mode in [SearchMode::Hybrid, SearchMode::Exact] {
            let settings = effective_settings(&legacy(None, Some(mode)), &flags, None);
            assert_eq!(settings.search_mode, mode);
            assert_eq!(settings.list_search_mode, TuiSearchMode::Lexical);
        }
        // The command line wins over both.
        let settings = effective_settings(
            &legacy(Some(false), Some(SearchMode::Lexical)),
            &flags,
            Some(SearchMode::Semantic),
        );
        assert_eq!(settings.list_search_mode, TuiSearchMode::Semantic);
    }

    // --- launch arguments ---

    #[test]
    fn empty_command_line_runs_with_defaults() {
        assert_eq!(run(&[]), GuiLaunchArgs::default());
    }

    #[test]
    fn every_option_parses() {
        let parsed = run(&[
            "--local",
            "--workspace",
            "C:\\code\\my project",
            "--since",
            "2d",
            "--before=2099-12-31",
            "--first",
            "--show-tools",
            "--hide-thinking",
            "--resume",
            "--fork-session",
            "--debug",
            "warn",
            "--semantic",
        ]);
        assert!(parsed.local);
        assert_eq!(
            parsed.workspace,
            Some(PathBuf::from("C:\\code\\my project"))
        );
        assert_eq!(parsed.time.since, Some("2d".parse().unwrap()));
        assert_eq!(parsed.time.before, Some("2099-12-31".parse().unwrap()));
        assert!(parsed.first && parsed.show_tools && parsed.hide_thinking);
        assert_eq!(parsed.picker(), Some(PickerAction::Resume { fork: true }));
        assert_eq!(parsed.debug, Some(DebugLevel::Warn));
        assert_eq!(parsed.search_mode_override(), Some(SearchMode::Semantic));
        assert!(parsed.time_filter().unwrap().is_active());

        let short = run(&["-tL", "-wC:\\w", "-l", "-p", "-i", "-c"]);
        assert!(short.show_tools && short.local && short.last);
        assert_eq!(short.workspace, Some(PathBuf::from("C:\\w")));
        // Upstream precedence: show-path, then show-id, then resume.
        assert_eq!(short.picker(), Some(PickerAction::ShowPath));
        assert_eq!(run(&["-w", "dir"]).workspace, Some(PathBuf::from("dir")));
        assert_eq!(
            run(&["--workspace=a b"]).workspace,
            Some(PathBuf::from("a b"))
        );
        assert_eq!(
            run(&["--after", "6mo"]).time.after,
            Some("6mo".parse().unwrap())
        );
        assert_eq!(
            run(&["--lexical"]).search_mode_override(),
            Some(SearchMode::Lexical)
        );
        assert_eq!(run(&["-i", "-c"]).picker(), Some(PickerAction::ShowId));
        assert_eq!(
            run(&["-c"]).picker(),
            Some(PickerAction::Resume { fork: false })
        );
    }

    #[test]
    fn workspace_argument_becomes_a_normalized_absolute_scope() {
        let (launch, absolute, encoded) = if cfg!(windows) {
            (r"C:\launch", r"C:\code\proj", "C--code-proj")
        } else {
            ("/launch", "/code/proj", "-code-proj")
        };
        let launch = Path::new(launch);
        assert_eq!(run(&[]).workspace_scope(Some(launch)).unwrap(), None);

        // Absolute values ignore the launch directory; a trailing separator
        // is dropped before encoding.
        let sep = std::path::MAIN_SEPARATOR_STR;
        let trailing = format!("{absolute}{sep}");
        for value in [absolute, trailing.as_str()] {
            for base in [None, Some(launch)] {
                let scope = run(&["-w", value]).workspace_scope(base).unwrap().unwrap();
                assert_eq!(scope.dir(), Path::new(absolute), "{value}");
                assert_eq!(scope.project_dir_name(), encoded, "{value}");
            }
        }

        // Relative values resolve against the launch directory only.
        let scope = run(&["-w", "."])
            .workspace_scope(Some(launch))
            .unwrap()
            .unwrap();
        assert_eq!(scope.dir(), launch);
        let dotted = format!("..{sep}proj{sep}");
        let scope = run(&["-w", &dotted])
            .workspace_scope(Some(&launch.join("sub")))
            .unwrap()
            .unwrap();
        assert_eq!(scope.dir(), launch.join("proj"));
        for value in [".", "proj", dotted.as_str()] {
            assert!(
                matches!(
                    run(&["-w", value]).workspace_scope(None),
                    Err(AppError::InvalidWorkspace(_))
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn file_argument_and_double_dash() {
        assert_eq!(
            run(&["session.jsonl"]).file,
            Some(PathBuf::from("session.jsonl"))
        );
        assert_eq!(
            run(&["--", "-odd.jsonl"]).file,
            Some(PathBuf::from("-odd.jsonl"))
        );
        assert_eq!(run(&["-"]).file, Some(PathBuf::from("-")));
        let parsed = run(&["--show-tools", "x.jsonl", "--show-thinking"]);
        assert_eq!(parsed.file, Some(PathBuf::from("x.jsonl")));
        assert!(parsed.show_thinking);
        assert_eq!(
            fail(&["a.jsonl", "b.jsonl"]),
            "unexpected argument 'b.jsonl' found"
        );
    }

    #[test]
    fn debug_level_is_optional_and_does_not_swallow_files() {
        assert_eq!(run(&["--debug"]).debug, Some(DebugLevel::Debug));
        assert_eq!(run(&["--debug=error"]).debug, Some(DebugLevel::Error));
        assert_eq!(run(&["--debug", "INFO"]).debug, Some(DebugLevel::Info));
        let parsed = run(&["--debug", "some.jsonl"]);
        assert_eq!(parsed.debug, Some(DebugLevel::Debug));
        assert_eq!(parsed.file, Some(PathBuf::from("some.jsonl")));
        assert_eq!(
            fail(&["--debug=loud"]),
            "invalid value 'loud' for '--debug [<LEVEL>]': invalid log level 'loud', expected: debug, info, warn, error"
        );
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse_launch_args(["--help"]), Ok(LaunchCommand::Help));
        assert_eq!(
            parse_launch_args(["-h", "--bogus"]),
            Ok(LaunchCommand::Help)
        );
        assert_eq!(parse_launch_args(["-V"]), Ok(LaunchCommand::Version));
        let help = help_text("claude-history-gui");
        assert!(help.contains("Usage: claude-history-gui [OPTIONS] [FILE]"));
        for option in [
            "--local",
            "--workspace",
            "--since",
            "--after",
            "--before",
            "--first",
            "--last",
            "--show-tools",
            "--no-tools",
            "--show-thinking",
            "--hide-thinking",
            "--show-path",
            "--show-id",
            "--resume",
            "--fork-session",
            "--debug",
            "--semantic",
            "--lexical",
        ] {
            assert!(help.contains(option), "{option}");
            assert!(long_option(&option[2..]).is_some(), "{option}");
        }
    }

    #[test]
    fn errors_are_clap_style() {
        assert_eq!(fail(&["--bogus"]), "unexpected argument '--bogus' found");
        assert_eq!(fail(&["-x"]), "unexpected argument '-x' found");
        assert_eq!(
            fail(&["--since"]),
            "a value is required for '--since <WHEN>' but none was supplied"
        );
        assert_eq!(
            fail(&["-w"]),
            "a value is required for '--workspace <DIR>' but none was supplied"
        );
        assert!(
            fail(&["--since", "yesterday"])
                .starts_with("invalid value 'yesterday' for '--since <WHEN>': "),
        );
        assert_eq!(
            fail(&["--local", "--local"]),
            "the argument '--local' cannot be used multiple times"
        );
        assert_eq!(
            fail(&["--local=yes"]),
            "unexpected value for '--local': it takes no value"
        );
        assert_eq!(
            fail(&["--first", "--last"]),
            "the argument '--first' cannot be used with '--last'"
        );
        assert_eq!(
            fail(&["-t", "--no-tools"]),
            "the argument '--show-tools' cannot be used with '--no-tools'"
        );
        assert_eq!(
            fail(&["--show-thinking", "--hide-thinking"]),
            "the argument '--show-thinking' cannot be used with '--hide-thinking'"
        );
        assert_eq!(
            fail(&["--semantic", "--lexical"]),
            "the argument '--semantic' cannot be used with '--lexical'"
        );
        assert_eq!(
            fail(&["--since", "1d", "--after", "2d"]),
            "the argument '--since' cannot be used with '--after'"
        );
        assert!(fail(&["--fork-session"]).contains("--resume"));
        for flag in ["--local", "--show-path", "--show-id", "--resume"] {
            assert_eq!(
                fail(&["x.jsonl", flag]),
                format!("the argument '[FILE]' cannot be used with '{flag}'")
            );
        }
        assert_eq!(LaunchArgsError("x".into()).to_string(), "error: x");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_accepted_but_not_values() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'f', 0xff]);
        let Ok(LaunchCommand::Run(parsed)) = parse_launch_args([bad.clone()]) else {
            panic!()
        };
        assert_eq!(parsed.file, Some(PathBuf::from(bad.clone())));
        assert!(parse_launch_args([OsString::from("--since"), bad]).is_err());
    }

    #[test]
    fn launch_args_feed_settings() {
        let parsed = run(&["--no-tools", "--first", "--show-thinking", "--semantic"]);
        let settings = parsed.settings(&ConfigFile::default());
        assert_eq!(settings.tool_display, InitialToolDisplay::Hidden);
        assert!(!settings.show_last);
        assert!(settings.show_thinking);
        assert_eq!(settings.list_search_mode, TuiSearchMode::Semantic);
    }

    #[test]
    fn picker_output_prints_path_or_session_id() {
        let mut conversation = crate::semantic::test_fixtures::SemanticConversationFixture::new(
            "/p/-proj/abc-123.jsonl",
            Vec::<String>::new(),
        )
        .build();
        assert_eq!(
            picker_output(PickerAction::ShowPath, &conversation).unwrap(),
            Some(
                PathBuf::from("/p/-proj/abc-123.jsonl")
                    .display()
                    .to_string()
            )
        );
        assert_eq!(
            picker_output(PickerAction::ShowId, &conversation).unwrap(),
            Some("abc-123".to_string())
        );
        conversation.session_id = "pi-header-id".to_string();
        assert_eq!(
            picker_output(PickerAction::ShowId, &conversation).unwrap(),
            Some("pi-header-id".to_string())
        );
        conversation.session_id.clear();
        assert_eq!(
            picker_output(PickerAction::ShowId, &conversation).unwrap(),
            Some("abc-123".to_string())
        );
        assert_eq!(
            picker_output(PickerAction::Resume { fork: false }, &conversation).unwrap(),
            None
        );
    }

    // --- K14: the rest of upstream's top-level surface (C-016) ---

    #[test]
    fn upstream_one_shot_and_compatibility_flags_parse() {
        let parsed = run(&[
            "--pager",
            "--no-color",
            "-g",
            "--debug-search",
            "cache warming",
        ]);
        assert!(parsed.pager && parsed.no_color && parsed.global);
        assert_eq!(parsed.debug_search.as_deref(), Some("cache warming"));
        assert!(run(&["--plain", "-p"]).plain);
        assert!(run(&["--no-pager"]).no_pager);
        assert!(run(&["-d"]).show_dir);
        assert!(run(&["--show-dir", "--plain"]).show_dir);
        assert_eq!(
            run(&["--render", "session.jsonl", "--resume"]).render,
            Some(PathBuf::from("session.jsonl"))
        );
        assert_eq!(run(&["--delete=abc"]).delete.as_deref(), Some("abc"));
        let semantic = run(&["--semantic-search", "  cache  ", "--semantic-top", "5"]);
        assert_eq!(semantic.semantic_search.as_deref(), Some("cache"));
        assert_eq!(semantic.semantic_top, Some(5));
        assert_eq!(semantic.semantic_top_or_default(), 5);
        assert_eq!(
            run(&["--semantic-search", "x"]).semantic_top_or_default(),
            DEFAULT_SEMANTIC_TOP
        );
        assert_eq!(
            run(&["--debug-semantic-search", "cache"])
                .debug_semantic_search
                .as_deref(),
            Some("cache")
        );
        assert!(run(&["--generate-semantic-cache"]).generate_semantic_cache);
        assert!(run(&["--clear-semantic-cache"]).clear_semantic_cache);
        // The compatibility flags change nothing else.
        let mut plain = run(&["--pager", "--no-color", "--global"]);
        plain.pager = false;
        plain.no_color = false;
        plain.global = false;
        assert_eq!(plain, GuiLaunchArgs::default());
    }

    #[test]
    fn every_conflict_group_and_validator_is_enforced() {
        let actions: &[&[&str]] = &[
            &["--show-dir"],
            &["--delete", "id"],
            &["--debug-search", "q"],
            &["--debug-semantic-search", "q"],
            &["--semantic-search", "q"],
            &["--generate-semantic-cache"],
            &["--clear-semantic-cache"],
            &["--render", "r.jsonl"],
            &["f.jsonl"],
        ];
        // one_shot_action: every pair conflicts.
        for (index, first) in actions.iter().enumerate() {
            for second in &actions[index + 1..] {
                let args = first
                    .iter()
                    .chain(second.iter())
                    .copied()
                    .collect::<Vec<_>>();
                assert!(fail(&args).contains("cannot be used with"), "{args:?}");
            }
        }
        // selection_conflicting_action: the actions except --show-dir and
        // --render conflict with the picker flags and --plain.
        for action in &actions[1..7] {
            for selection in ["--resume", "--show-path", "--show-id", "--plain"] {
                let mut args = action.to_vec();
                args.push(selection);
                assert!(fail(&args).contains("cannot be used with"), "{args:?}");
            }
        }
        for selection in ["--resume", "--show-path", "--show-id", "--plain"] {
            assert!(fail(&["f.jsonl", selection]).contains("cannot be used with"));
            assert!(run(&["--show-dir", selection]).show_dir);
            assert!(run(&["--render", "r.jsonl", selection]).render.is_some());
        }
        for (args, expected) in [
            (
                &["--delete", "x", "--global"][..],
                "the argument '--delete <SESSION_ID>' cannot be used with '--global'",
            ),
            (
                &["f.jsonl", "-g"],
                "the argument '[FILE]' cannot be used with '--global'",
            ),
            (
                &["f.jsonl", "--local"],
                "the argument '[FILE]' cannot be used with '--local'",
            ),
            (
                &["--pager", "--no-pager"],
                "the argument '--pager' cannot be used with '--no-pager'",
            ),
            (
                &["--semantic-top", "5"],
                "the following required arguments were not provided: --semantic-search <QUERY> (required by --semantic-top)",
            ),
            (
                &["--semantic-search", "q", "--semantic-top", "0"],
                "invalid value '0' for '--semantic-top <SEMANTIC_TOP>': value must be greater than zero",
            ),
            (
                &["--semantic-search", "q", "--semantic-top", "x"],
                "invalid value 'x' for '--semantic-top <SEMANTIC_TOP>': invalid positive integer: x",
            ),
            (
                &["--semantic-search", "   "],
                "invalid value '   ' for '--semantic-search <QUERY>': value must not be empty",
            ),
            (
                &["--debug-semantic-search", ""],
                "invalid value '' for '--debug-semantic-search <QUERY>': value must not be empty",
            ),
            (
                &["--plain", "--delete", "x"],
                "the argument '--delete <SESSION_ID>' cannot be used with '--plain'",
            ),
            (
                &["--render"],
                "a value is required for '--render <FILE>' but none was supplied",
            ),
        ] {
            assert_eq!(fail(args), expected, "{args:?}");
        }
    }

    #[test]
    fn subcommands_parse_and_never_combine_with_options() {
        assert_eq!(
            parse_launch_args(["delete-empty"]),
            Ok(LaunchCommand::DeleteEmpty {
                local: false,
                all: false,
                yes: false
            })
        );
        assert_eq!(
            parse_launch_args(["delete-empty", "--yes", "--local"]),
            Ok(LaunchCommand::DeleteEmpty {
                local: true,
                all: false,
                yes: true
            })
        );
        assert_eq!(
            fail(&["delete-empty", "--local", "--all"]),
            "the argument '--local' cannot be used with '--all'"
        );
        assert_eq!(
            fail(&["delete-empty", "--force"]),
            "unexpected argument '--force' found"
        );
        assert_eq!(parse_launch_args(["update"]), Ok(LaunchCommand::Update));
        assert_eq!(
            fail(&["update", "--yes"]),
            "unexpected argument '--yes' found"
        );
        assert!(matches!(
            parse_launch_args(["delete-empty", "--help"]),
            Ok(LaunchCommand::SubcommandHelp(_))
        ));
        assert_eq!(
            fail(&["--local", "delete-empty"]),
            "the subcommand 'delete-empty' cannot be used with '--local'"
        );
        // After `--` it is a file name.
        assert_eq!(run(&["--", "update"]).file, Some(PathBuf::from("update")));

        // K15: `agent ...` goes to the agent parser.
        match parse_launch_args(["agent", "search", "cache", "--top", "3"]) {
            Ok(LaunchCommand::Agent(crate::agent::cli::AgentCli::Request(
                crate::agent::AgentRequest::Search(search),
            ))) => {
                assert_eq!(search.query, "cache");
                assert_eq!(search.top, Some(3));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_launch_args(["agent", "--help"]),
            Ok(LaunchCommand::Agent(crate::agent::cli::AgentCli::Help(_)))
        ));
        assert_eq!(
            fail(&["agent", "read"]),
            "the following required arguments were not provided: <REFS>..."
        );

        let help = help_text("claude-history-gui");
        for line in [
            "claude-history-gui delete-empty [--local|--all] [--yes]",
            "claude-history-gui update",
            "claude-history-gui agent <search|within|read|outline> ...",
            "--show-dir",
            "--render <FILE>",
            "--delete <SESSION_ID>",
            "--debug-search <QUERY>",
            "--plain",
            "--pager",
            "--no-pager",
            "--no-color",
        ] {
            assert!(help.contains(line), "{line}");
        }
        // Hidden upstream flags stay hidden.
        for hidden in [
            "--global",
            "--semantic-search",
            "--semantic-top",
            "--debug-semantic-search",
            "--generate-semantic-cache",
            "--clear-semantic-cache",
        ] {
            assert!(!help.contains(hidden), "{hidden}");
        }
    }

    fn subcommand_help_text(args: &[&str]) -> String {
        match parse_launch_args(args.iter().copied()) {
            Ok(LaunchCommand::SubcommandHelp(text)) => text,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    #[test]
    fn help_subcommand_and_subcommand_help_like_clap() {
        // clap adds a `help` subcommand: `help` is the top-level help, never
        // a file named "help".
        assert_eq!(parse_launch_args(["help"]), Ok(LaunchCommand::Help));
        assert_eq!(run(&["--", "help"]).file, Some(PathBuf::from("help")));
        assert_eq!(
            fail(&["--local", "help"]),
            "the subcommand 'help' cannot be used with '--local'"
        );
        assert!(help_text("claude-history-gui").contains(
            "  help          Print this message or the help of the given subcommand(s)\n"
        ));

        // `<name> --help` prints that subcommand's help, as `help <name>`.
        let update = subcommand_help_text(&["update", "--help"]);
        assert_eq!(
            update,
            "Check for a newer release\n\nUsage: claude-history-gui update\n\nOptions:\n  -h, --help  Print help\n"
        );
        assert_eq!(subcommand_help_text(&["update", "-h"]), update);
        assert_eq!(subcommand_help_text(&["help", "update"]), update);
        let delete_empty = subcommand_help_text(&["delete-empty", "--help"]);
        assert!(
            delete_empty.starts_with(
                "Delete transcript files with no Claude messages\n\nUsage: claude-history-gui delete-empty [OPTIONS]\n"
            ),
            "{delete_empty}"
        );
        for line in [
            "      --yes    Delete matching transcripts instead of printing a dry run\n",
            "      --local  Only inspect the current workspace\n",
            "      --all    Inspect all workspaces\n",
            "  -h, --help   Print help\n",
        ] {
            assert!(delete_empty.contains(line), "{line}");
        }
        assert_eq!(
            subcommand_help_text(&["delete-empty", "--yes", "-h"]),
            delete_empty
        );
        assert_eq!(
            subcommand_help_text(&["help", "delete-empty"]),
            delete_empty
        );
        assert!(
            subcommand_help_text(&["help", "help"])
                .contains("Usage: claude-history-gui help [COMMAND]...\n")
        );

        // `help agent [<name>]` is the agent parser's help.
        assert_eq!(
            parse_launch_args(["help", "agent"]),
            parse_launch_args(["agent", "--help"])
        );
        assert_eq!(
            parse_launch_args(["help", "agent", "search"]),
            parse_launch_args(["agent", "search", "--help"])
        );
        assert!(matches!(
            parse_launch_args(["help", "agent", "search"]),
            Ok(LaunchCommand::Agent(crate::agent::cli::AgentCli::Help(_)))
        ));

        assert_eq!(fail(&["help", "bogus"]), "unrecognized subcommand 'bogus'");
        assert_eq!(
            fail(&["help", "update", "x"]),
            "unrecognized subcommand 'x'"
        );
        assert_eq!(fail(&["help", "--x"]), "unexpected argument '--x' found");
    }
}
