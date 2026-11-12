//! Configuration: the CLI's `config.toml` (read-only) layered with the GUI's
//! own `gui.toml`.
//!
//! * `config.toml` keeps upstream's exact schema, including
//!   `#[serde(deny_unknown_fields)]` on every table: the installed CLI aborts
//!   on any key it does not know, so core never writes to it and never adds
//!   GUI keys to its schema ([`ConfigFile`]). Fork: the Copilot table
//!   `[resume.copilot]` is GUI-only, so [`load_config_from`] rejects it in
//!   `config.toml` like an unknown key; `gui.toml` accepts it.
//! * `gui.toml` ([`GuiConfigFile`]) may override any shared section
//!   (`[display]`, `[resume]`, `[keys]`, `[tui]`, `[search]`, `[agent]`) and
//!   holds the GUI-only `[gui]` table ([`GuiSettings`]). It is the only file
//!   core writes ([`save_gui_config`], atomic temp file + rename;
//!   [`save_gui_config_changes`] edits only the changed keys and keeps the
//!   file's comments and formatting).
//! * Precedence for every setting: `gui.toml` > `config.toml` > defaults
//!   ([`load_layered_config`]). The program that starts an agent (upstream
//!   8a99ccc's `[resume] command`, `[resume.pi|omp|codex] command`, fork: `[resume.copilot] command`) also
//!   weighs the GUI-only `[gui] claude_path`, ...: `gui.toml`'s `command` >
//!   `[gui] <agent>_path` > `config.toml`'s `command` > `PATH`
//!   ([`agent_launcher`], [`LayeredConfig::launcher`]); upstream's
//!   `ResumeConfig::{launcher, launch}` are not ported (one file there).
//!   [`AgentCommand`] also serializes (a one-word command as a string), so
//!   `gui.toml` can hold it.
//!
//! Key bindings use a neutral [`KeyChord`] (refactor R4) instead of
//! crossterm key codes, with upstream's parsing rules, labels and errors.
//! One default differs: `[keys] user_only` is Alt+U, not upstream's Ctrl+V,
//! which pastes into the GUI's search field.

use crate::env::{CLI_CONFIG_FILE_NAME, HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history::actions::{ConfiguredAt, Launch, Launcher, config_table};
use crate::search::mode::SearchMode;
use crate::theme::ThemePreference;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Defines the structure of the config.toml file.
/// Using `Option` allows distinguishing between a value being unset
/// vs. explicitly set to `false`.
#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Deprecated: global is now the default. Use `--local` flag or Tab toggle instead.
    /// Kept for backwards compatibility with existing config files.
    #[allow(dead_code)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<KeysConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tui: Option<TuiConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfig>,
    /// Upstream's background-daemon table (939c20b). Accepted so a shared
    /// `config.toml` written for the CLI still loads; the GUI has no daemon
    /// (decisions.md #22) and ignores it, and `gui.toml` has no such table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticConfig>,
}

/// `[semantic]`: settings of upstream's opt-in background daemon. Parsed for
/// compatibility only; the GUI neither starts a daemon nor reads these.
#[derive(Clone, Copy, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfig {
    /// Keep the caches warm in a background process that any launch starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon: Option<bool>,
    /// Whether the background process pauses embedding on battery power.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_on_battery: Option<bool>,
}

impl SemanticConfig {
    pub fn daemon(&self) -> bool {
        self.daemon.unwrap_or(false)
    }

    pub fn pause_on_battery(&self) -> bool {
        self.pause_on_battery.unwrap_or(true)
    }
}

#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<SearchMode>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentScopeConfig {
    Global,
    Local,
}

#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<AgentScopeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<SearchMode>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_chars: Option<usize>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub top: Option<usize>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub within_top: Option<usize>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub hits_per_conversation: Option<usize>,
    #[serde(default)]
    pub exclude_projects: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<bool>,
}

fn deserialize_nonzero_option<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<usize>::deserialize(deserializer)?;
    if value == Some(0) {
        return Err(serde::de::Error::custom("value must be greater than zero"));
    }
    Ok(value)
}

#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    #[serde(default)]
    pub exclude_projects: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_search: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<bool>,
    /// Deprecated: timestamps now always use hybrid relative/absolute format.
    /// Kept for backwards compatibility with existing config files.
    #[allow(dead_code)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_time: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pager: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResumeConfig {
    /// The command Claude Code is started with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<AgentCommand>,
    /// Arguments Claude Code is resumed with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pi: Option<LauncherConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omp: Option<LauncherConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex: Option<AgentResumeConfig>,
    /// Fork: GUI-only, so `config.toml` may not hold it
    /// ([`load_config_from`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot: Option<AgentResumeConfig>,
}

/// How the conversations of another agent are resumed.
#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentResumeConfig {
    /// The command the agent is started with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<AgentCommand>,
    /// Arguments the agent is resumed with, before its own subcommand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_args: Option<Vec<String>>,
}

/// The command an agent is started with.
#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LauncherConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<AgentCommand>,
}

/// A program and the arguments that precede the agent's own: a string
/// names the program alone (never split on whitespace), an array the
/// program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommand(Vec<String>);

impl AgentCommand {
    /// Fork: the command of `words` (the program first), `None` when it
    /// names no program (empty, or a blank first word), which a config file
    /// rejects.
    pub fn new(words: Vec<String>) -> Option<Self> {
        words
            .first()
            .is_some_and(|program| !program.trim().is_empty())
            .then_some(Self(words))
    }

    /// Fork: the program and its arguments.
    pub fn words(&self) -> &[String] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for AgentCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Program(String),
            Command(Vec<String>),
        }
        let command = match Raw::deserialize(deserializer).map_err(|_| {
            serde::de::Error::custom(
                "command must be a program (\"my-claude\") or an array of a program and its arguments ([\"mise\", \"exec\", \"--\", \"claude\"])",
            )
        })? {
            Raw::Program(program) => vec![program],
            Raw::Command(command) => command,
        };
        if command
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            return Err(serde::de::Error::custom("command must name a program"));
        }
        Ok(Self(command))
    }
}

/// Fork (`gui.toml` is written): a program alone as a string, as a user
/// would write it, else the array.
impl Serialize for AgentCommand {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0.as_slice() {
            [program] => serializer.serialize_str(program),
            words => words.serialize(serializer),
        }
    }
}

impl ResumeConfig {
    /// The configured arguments for resuming a conversation of `source`.
    pub fn default_args(&self, source: crate::history::model::SourceKind) -> &[String] {
        use crate::history::model::SourceKind;
        let args = match source {
            SourceKind::Claude => self.default_args.as_ref(),
            SourceKind::Codex => self
                .codex
                .as_ref()
                .and_then(|codex| codex.default_args.as_ref()),
            SourceKind::Copilot => self
                .copilot
                .as_ref()
                .and_then(|copilot| copilot.default_args.as_ref()),
            SourceKind::Pi | SourceKind::Omp => None,
        };
        args.map(Vec::as_slice).unwrap_or(&[])
    }

    /// The configured command that starts the agent of `source` (upstream
    /// 8a99ccc's `ResumeConfig::launcher` reads the same keys; the fork
    /// builds the launcher from both files with [`agent_launcher`]).
    pub fn command(&self, source: crate::history::model::SourceKind) -> Option<&AgentCommand> {
        use crate::history::model::SourceKind;
        match source {
            SourceKind::Claude => self.command.as_ref(),
            SourceKind::Pi => self.pi.as_ref().and_then(|pi| pi.command.as_ref()),
            SourceKind::Omp => self.omp.as_ref().and_then(|omp| omp.command.as_ref()),
            SourceKind::Codex => self.codex.as_ref().and_then(|codex| codex.command.as_ref()),
            SourceKind::Copilot => self
                .copilot
                .as_ref()
                .and_then(|copilot| copilot.command.as_ref()),
        }
    }
}

/// The program that starts the agent of `source` (upstream 8a99ccc's
/// `ResumeConfig::launcher`), from `config.toml` (`cli`) and `gui.toml`
/// (`gui`). Fork (decision 17), first that is set:
///
/// 1. `command` under the agent's table (`[resume]`, `[resume.pi]`,
///    `[resume.omp]`, `[resume.codex]`, `[resume.copilot]`) of `gui.toml`;
/// 2. the agent's program path under `[gui]` of `gui.toml` (`claude_path`,
///    `pi_path`, `omp_path`, `codex_path`, `copilot_path`);
/// 3. `command` under the agent's table of `config.toml`;
/// 4. the agent's own binary on `PATH`.
///
/// GUI settings beat the CLI's shared file (a `config.toml` wrapper may be
/// a shell script the GUI's platform cannot run), and a `[gui]` path is the
/// GUI's spelling of a command that is one program. Relative paths and `~/`
/// resolve with `env` ([`Launcher::configured`]); nothing is read from disk.
pub fn agent_launcher(
    env: &HistoryEnv,
    cli: &ConfigFile,
    gui: &GuiConfigFile,
    source: crate::history::model::SourceKind,
) -> Launcher {
    let table = config_table(source);
    if let Some(command) = gui
        .resume
        .as_ref()
        .and_then(|resume| resume.command(source))
    {
        return Launcher::configured(env, command.words(), ConfiguredAt::Gui(table));
    }
    if let Some(settings) = &gui.gui
        && let Some(path) = settings.program_path(source)
    {
        return Launcher::program_path(
            env,
            path,
            ConfiguredAt::GuiPath(GuiSettings::program_path_key(source)),
        );
    }
    if let Some(command) = cli
        .resume
        .as_ref()
        .and_then(|resume| resume.command(source))
    {
        return Launcher::configured(env, command.words(), ConfiguredAt::Config(table));
    }
    Launcher::default_for(source)
}

#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<KeyBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<KeyBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rename: Option<KeyBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<KeyBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<KeyBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_kinds: Option<KeyBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_only: Option<KeyBinding>,
}

// ---------------------------------------------------------------------------
// Key bindings (R4)
// ---------------------------------------------------------------------------

/// A bindable key. The config grammar only produces single characters
/// (stored lower-case, as parsed) and function keys F1..F12.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    F(u8),
}

/// Modifier set. The config grammar allows at most one modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeyMods {
    pub ctrl: bool,
    pub alt: bool,
}

impl KeyMods {
    pub const NONE: KeyMods = KeyMods {
        ctrl: false,
        alt: false,
    };
    pub const CTRL: KeyMods = KeyMods {
        ctrl: true,
        alt: false,
    };
    pub const ALT: KeyMods = KeyMods {
        ctrl: false,
        alt: true,
    };

    pub fn is_empty(self) -> bool {
        !self.ctrl && !self.alt
    }
}

/// A key plus modifiers, UI-toolkit neutral (replaces upstream's crossterm
/// `KeyCode` + `KeyModifiers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub mods: KeyMods,
}

/// Upstream name, kept for minimal churn.
pub type KeyBinding = KeyChord;

impl KeyChord {
    pub const fn new(key: Key, mods: KeyMods) -> Self {
        Self { key, mods }
    }

    /// `Ctrl+<c>`
    pub const fn ctrl(c: char) -> Self {
        Self::new(Key::Char(c), KeyMods::CTRL)
    }

    /// `Alt+<c>`
    pub const fn alt(c: char) -> Self {
        Self::new(Key::Char(c), KeyMods::ALT)
    }

    /// A key without modifiers.
    pub const fn plain(key: Key) -> Self {
        Self::new(key, KeyMods::NONE)
    }

    /// Exact equality of key and modifier set (upstream `matches`).
    pub fn matches(&self, key: Key, mods: KeyMods) -> bool {
        self.key == key && self.mods == mods
    }

    /// Format for status bar display (e.g. "^F", "M-F", "F2")
    pub fn short_label(&self) -> String {
        self.format_label("^", "M-")
    }

    /// Format for help overlay (e.g. "Ctrl+F", "Alt+F", "F2")
    pub fn help_label(&self) -> String {
        self.format_label("Ctrl+", "Alt+")
    }

    fn format_label(&self, ctrl_prefix: &str, alt_prefix: &str) -> String {
        let prefix = if self.mods.ctrl {
            ctrl_prefix
        } else if self.mods.alt {
            alt_prefix
        } else {
            ""
        };
        match self.key {
            Key::Char(c) => format!("{}{}", prefix, c.to_ascii_uppercase()),
            Key::F(n) if self.mods.is_empty() => format!("F{}", n),
            _ => String::new(),
        }
    }

    /// The keystroke in gpui's `Keystroke::parse` format: `ctrl-r`,
    /// `alt-f`, `f2`.
    pub fn gpui_keystroke(&self) -> String {
        let mut out = String::new();
        if self.mods.ctrl {
            out.push_str("ctrl-");
        }
        if self.mods.alt {
            out.push_str("alt-");
        }
        match self.key {
            Key::Char(c) => out.push(c),
            Key::F(n) => out.push_str(&format!("f{n}")),
        }
        out
    }

    /// The binding in config-file syntax (`ctrl+r`, `alt+f`, `f2`), which
    /// [`parse_key_binding`] reads back.
    pub fn config_string(&self) -> String {
        let mut out = String::new();
        if self.mods.ctrl {
            out.push_str("ctrl+");
        }
        if self.mods.alt {
            out.push_str("alt+");
        }
        match self.key {
            Key::Char(c) => out.push(c),
            Key::F(n) => out.push_str(&format!("f{n}")),
        }
        out
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.config_string())
    }
}

impl FromStr for KeyChord {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_key_binding(s)
    }
}

impl<'de> Deserialize<'de> for KeyChord {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_key_binding(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for KeyChord {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.config_string())
    }
}

fn parse_key_code(key: &str) -> std::result::Result<Key, String> {
    match key.to_lowercase().as_str() {
        k if k.len() == 1 => Ok(Key::Char(k.chars().next().unwrap())),
        k if k.starts_with('f') => {
            let number = k[1..]
                .parse::<u8>()
                .map_err(|_| format!("Unknown key: {key}"))?;
            if (1..=12).contains(&number) {
                Ok(Key::F(number))
            } else {
                Err(format!("Unknown key: {key}"))
            }
        }
        _ => Err(format!("Unknown key: {key}")),
    }
}

/// Parse a binding string (`"<key>"` or `"<modifier>+<key>"`) exactly like
/// upstream: parts split on `+` and trimmed; modifiers `ctrl`/`control` and
/// `alt`/`meta` (case-insensitive); keys are one (byte-length) character or
/// `f1`..`f12`; function keys take no modifier.
pub fn parse_key_binding(s: &str) -> std::result::Result<KeyBinding, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    match parts.as_slice() {
        [modifier, key] => {
            let mods = match modifier.to_lowercase().as_str() {
                "ctrl" | "control" => KeyMods::CTRL,
                "alt" | "meta" => KeyMods::ALT,
                _ => return Err(format!("Unknown modifier: {modifier}")),
            };
            let key = parse_key_code(key)?;
            if matches!(key, Key::F(_)) {
                return Err(format!("Function keys do not support modifiers: {s}"));
            }
            Ok(KeyChord { key, mods })
        }
        [key] => {
            let key = parse_key_code(key)?;
            Ok(KeyChord {
                key,
                mods: KeyMods::NONE,
            })
        }
        _ => Err(format!("Invalid key binding: {s}")),
    }
}

/// Resolved keybindings with defaults applied
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBindings {
    pub resume: KeyBinding,
    pub fork: KeyBinding,
    pub rename: KeyBinding,
    pub delete: KeyBinding,
    pub refresh: KeyBinding,
    /// Lists or hides automated and child conversations
    pub all_kinds: KeyBinding,
    /// Searches only the user's own messages, or everything
    pub user_only: KeyBinding,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            resume: KeyChord::ctrl('r'),
            fork: KeyChord::ctrl('f'),
            rename: KeyChord::plain(Key::F(2)),
            delete: KeyChord::ctrl('x'),
            refresh: KeyChord::ctrl('l'),
            all_kinds: KeyChord::ctrl('g'),
            // Fork: upstream binds Ctrl+V, which is paste in the GUI's search
            // field (a reserved key, DESIGN §8.5).
            user_only: KeyChord::alt('u'),
        }
    }
}

impl KeyBindings {
    pub fn from_config(config: Option<KeysConfig>) -> Self {
        let defaults = Self::default();
        match config {
            None => defaults,
            Some(cfg) => Self {
                resume: cfg.resume.unwrap_or(defaults.resume),
                fork: cfg.fork.unwrap_or(defaults.fork),
                rename: cfg.rename.unwrap_or(defaults.rename),
                delete: cfg.delete.unwrap_or(defaults.delete),
                refresh: cfg.refresh.unwrap_or(defaults.refresh),
                all_kinds: cfg.all_kinds.unwrap_or(defaults.all_kinds),
                user_only: cfg.user_only.unwrap_or(defaults.user_only),
            },
        }
    }

    /// Whether any action is bound to `Ctrl+T`. Upstream disables the
    /// lexical/semantic toggle (Ctrl+T) in that case. `all_kinds` and
    /// `user_only` count too (fork: upstream's `semantic_toggle_available`
    /// does not list them, but their keys are checked before Ctrl+T, so the
    /// toggle never runs either).
    pub fn conflicts_with_ctrl_t(&self) -> bool {
        let ctrl_t = (Key::Char('t'), KeyMods::CTRL);
        [
            self.rename,
            self.delete,
            self.resume,
            self.fork,
            self.refresh,
            self.all_kinds,
            self.user_only,
        ]
        .iter()
        .any(|binding| binding.matches(ctrl_t.0, ctrl_t.1))
    }
}

// ---------------------------------------------------------------------------
// gui.toml
// ---------------------------------------------------------------------------

/// GUI-only settings: the `[gui]` table of `gui.toml`.
///
/// Every field is optional: `None` means the key is absent from the file,
/// so a GUI can tell a value set in `gui.toml` (even the default one) from
/// "not set", and [`save_gui_config_changes`] removes a key whose field
/// becomes `None` (Reset). [`GuiSettings::theme`] and
/// [`GuiSettings::restore_session`] give the values in effect.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct GuiSettings {
    /// `"system"`, `"light"` or `"dark"`; absent = `"system"`
    /// ([`GuiSettings::theme`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemePreference>,
    /// Terminal command template used to launch resume/fork commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// Explicit path of the `claude` executable (else resolved via PATH).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_path: Option<PathBuf>,
    /// Explicit path of the `pi` executable (else resolved via PATH).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pi_path: Option<PathBuf>,
    /// Explicit path of the `omp` executable (else resolved via PATH).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omp_path: Option<PathBuf>,
    /// Explicit path of the `codex` executable (else resolved via PATH).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_path: Option<PathBuf>,
    /// Fork: explicit path of the `copilot` executable (else resolved via
    /// PATH).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot_path: Option<PathBuf>,
    /// Workspace directory to start with. It must be an absolute path; see
    /// [`GuiSettings::default_workspace_scope`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_workspace: Option<PathBuf>,
    /// Restore the previous session (window, workspace, ...) on start;
    /// absent = on ([`GuiSettings::restore_session`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_session: Option<bool>,
}

impl GuiSettings {
    /// The theme when `theme` is absent.
    pub const DEFAULT_THEME: ThemePreference = ThemePreference::System;
    /// Whether the previous session is restored when `restore_session` is
    /// absent.
    pub const DEFAULT_RESTORE_SESSION: bool = true;

    /// The theme preference in effect (the default when not set).
    pub fn theme(&self) -> ThemePreference {
        self.theme.unwrap_or(Self::DEFAULT_THEME)
    }

    /// Whether the previous session is restored (the default when not set).
    pub fn restore_session(&self) -> bool {
        self.restore_session
            .unwrap_or(Self::DEFAULT_RESTORE_SESSION)
    }

    /// The program path set for the agent of `source` (`claude_path`, ...).
    pub fn program_path(&self, source: crate::history::model::SourceKind) -> Option<&Path> {
        use crate::history::model::SourceKind;
        match source {
            SourceKind::Claude => self.claude_path.as_deref(),
            SourceKind::Pi => self.pi_path.as_deref(),
            SourceKind::Omp => self.omp_path.as_deref(),
            SourceKind::Codex => self.codex_path.as_deref(),
            SourceKind::Copilot => self.copilot_path.as_deref(),
        }
    }

    /// The `[gui]` key of [`Self::program_path`].
    pub fn program_path_key(source: crate::history::model::SourceKind) -> &'static str {
        use crate::history::model::SourceKind;
        match source {
            SourceKind::Claude => "claude_path",
            SourceKind::Pi => "pi_path",
            SourceKind::Omp => "omp_path",
            SourceKind::Codex => "codex_path",
            SourceKind::Copilot => "copilot_path",
        }
    }

    /// `default_workspace` as a validated, normalized [`WorkspaceScope`]
    /// (`Ok(None)` when unset). A relative path is an
    /// [`AppError::InvalidWorkspace`] error: there is no directory a GUI could
    /// meaningfully resolve it against.
    pub fn default_workspace_scope(&self) -> Result<Option<WorkspaceScope>> {
        self.default_workspace
            .as_deref()
            .map(WorkspaceScope::new)
            .transpose()
    }
}

/// `[tui]` overrides in `gui.toml`. Unlike [`TuiConfig`], an explicitly
/// empty `exclude_projects = []` or `exclude_paths = []` is distinguishable
/// from "not set".
#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TuiOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_projects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_search: Option<bool>,
}

/// `[agent]` overrides in `gui.toml` (same keys and validation as
/// [`AgentConfig`]; `exclude_projects` is optional so that `[]` overrides).
#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<AgentScopeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<SearchMode>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_chars: Option<usize>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub top: Option<usize>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub within_top: Option<usize>,
    #[serde(
        default,
        deserialize_with = "deserialize_nonzero_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub hits_per_conversation: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_projects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<bool>,
}

/// Contents of `gui.toml`: optional overrides of every shared section plus
/// the GUI-only `[gui]` table. Unknown keys are rejected, as in `config.toml`.
#[derive(Clone, Deserialize, Serialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuiConfigFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<KeysConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tui: Option<TuiOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gui: Option<GuiSettings>,
}

impl GuiConfigFile {
    /// The `[gui]` settings with defaults applied.
    pub fn gui_settings(&self) -> GuiSettings {
        self.gui.clone().unwrap_or_default()
    }
}

fn overlay<T: Clone>(base: Option<T>, top: &Option<T>) -> Option<T> {
    top.clone().or(base)
}

impl ConfigFile {
    /// Apply `gui.toml` overrides on top of this (`config.toml`)
    /// configuration, field by field: a value set in `gui` wins, an unset one
    /// keeps the `config.toml` value.
    pub fn overlaid_with(self, gui: &GuiConfigFile) -> ConfigFile {
        let ConfigFile {
            global,
            display,
            resume,
            keys,
            tui,
            search,
            agent,
            semantic,
        } = self;

        let display = match &gui.display {
            None => display,
            Some(top) => {
                let base = display.unwrap_or_default();
                Some(DisplayConfig {
                    no_tools: overlay(base.no_tools, &top.no_tools),
                    last: overlay(base.last, &top.last),
                    relative_time: overlay(base.relative_time, &top.relative_time),
                    show_thinking: overlay(base.show_thinking, &top.show_thinking),
                    plain: overlay(base.plain, &top.plain),
                    pager: overlay(base.pager, &top.pager),
                })
            }
        };
        // `command` overlays like any key here; which program starts an
        // agent also depends on `[gui] *_path` (see `agent_launcher`).
        let resume = match &gui.resume {
            None => resume,
            Some(top) => {
                let base = resume.unwrap_or_default();
                let agent =
                    |base: Option<AgentResumeConfig>, top: &Option<AgentResumeConfig>| match top {
                        None => base,
                        Some(top) => {
                            let base = base.unwrap_or_default();
                            Some(AgentResumeConfig {
                                command: overlay(base.command, &top.command),
                                default_args: overlay(base.default_args, &top.default_args),
                            })
                        }
                    };
                let launcher =
                    |base: Option<LauncherConfig>, top: &Option<LauncherConfig>| match top {
                        None => base,
                        Some(top) => Some(LauncherConfig {
                            command: overlay(base.unwrap_or_default().command, &top.command),
                        }),
                    };
                Some(ResumeConfig {
                    command: overlay(base.command, &top.command),
                    default_args: overlay(base.default_args, &top.default_args),
                    pi: launcher(base.pi, &top.pi),
                    omp: launcher(base.omp, &top.omp),
                    codex: agent(base.codex, &top.codex),
                    copilot: agent(base.copilot, &top.copilot),
                })
            }
        };
        let keys = match &gui.keys {
            None => keys,
            Some(top) => {
                let base = keys.unwrap_or_default();
                Some(KeysConfig {
                    resume: overlay(base.resume, &top.resume),
                    fork: overlay(base.fork, &top.fork),
                    rename: overlay(base.rename, &top.rename),
                    delete: overlay(base.delete, &top.delete),
                    refresh: overlay(base.refresh, &top.refresh),
                    all_kinds: overlay(base.all_kinds, &top.all_kinds),
                    user_only: overlay(base.user_only, &top.user_only),
                })
            }
        };
        let tui = match &gui.tui {
            None => tui,
            Some(top) => {
                let base = tui.unwrap_or_default();
                Some(TuiConfig {
                    exclude_projects: top
                        .exclude_projects
                        .clone()
                        .unwrap_or(base.exclude_projects),
                    exclude_paths: top.exclude_paths.clone().unwrap_or(base.exclude_paths),
                    semantic_search: overlay(base.semantic_search, &top.semantic_search),
                })
            }
        };
        let search = match &gui.search {
            None => search,
            Some(top) => {
                let base = search.unwrap_or_default();
                Some(SearchConfig {
                    mode: overlay(base.mode, &top.mode),
                })
            }
        };
        let agent = match &gui.agent {
            None => agent,
            Some(top) => {
                let base = agent.unwrap_or_default();
                Some(AgentConfig {
                    scope: overlay(base.scope, &top.scope),
                    mode: overlay(base.mode, &top.mode),
                    output_chars: overlay(base.output_chars, &top.output_chars),
                    top: overlay(base.top, &top.top),
                    within_top: overlay(base.within_top, &top.within_top),
                    hits_per_conversation: overlay(
                        base.hits_per_conversation,
                        &top.hits_per_conversation,
                    ),
                    exclude_projects: top
                        .exclude_projects
                        .clone()
                        .unwrap_or(base.exclude_projects),
                    tools: overlay(base.tools, &top.tools),
                    tool_results: overlay(base.tool_results, &top.tool_results),
                    thinking: overlay(base.thinking, &top.thinking),
                    subagents: overlay(base.subagents, &top.subagents),
                })
            }
        };

        ConfigFile {
            global,
            display,
            resume,
            keys,
            tui,
            search,
            agent,
            semantic,
        }
    }
}

/// Both config files plus the effective (merged) settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayeredConfig {
    /// `config.toml` as parsed (defaults when missing). Read-only.
    pub cli: ConfigFile,
    /// `gui.toml` as parsed (empty when missing).
    pub gui_file: GuiConfigFile,
    /// Effective shared settings: `gui.toml` > `config.toml` > defaults.
    pub effective: ConfigFile,
    /// Effective GUI settings (`[gui]` with defaults applied).
    pub gui: GuiSettings,
}

impl LayeredConfig {
    /// The program that starts the agent of `source`: [`agent_launcher`]
    /// of both files.
    pub fn launcher(
        &self,
        env: &HistoryEnv,
        source: crate::history::model::SourceKind,
    ) -> Launcher {
        agent_launcher(env, &self.cli, &self.gui_file, source)
    }

    /// How to start the agent of `source` to resume or fork one of its
    /// conversations (upstream `ResumeConfig::launch`): [`Self::launcher`]
    /// and the effective `default_args`.
    pub fn launch(&self, env: &HistoryEnv, source: crate::history::model::SourceKind) -> Launch {
        Launch {
            launcher: self.launcher(env, source),
            default_args: self
                .effective
                .resume
                .as_ref()
                .map(|resume| resume.default_args(source).to_vec())
                .unwrap_or_default(),
        }
    }
}

/// Load `config.toml` and `gui.toml` from the environment's locations and
/// merge them. Fails if either file exists but cannot be read or parsed;
/// the error names the file.
pub fn load_layered_config(env: &HistoryEnv) -> Result<LayeredConfig> {
    load_layered_config_from(
        env.config_file().as_deref(),
        env.gui_config_file().as_deref(),
    )
}

/// [`load_layered_config`] with explicit paths (`None` = no such file).
pub fn load_layered_config_from(
    config_path: Option<&Path>,
    gui_path: Option<&Path>,
) -> Result<LayeredConfig> {
    let cli = load_config_from(config_path)?;
    let gui_file = load_gui_config_from(gui_path)?;
    let effective = cli.clone().overlaid_with(&gui_file);
    let gui = gui_file.gui_settings();
    Ok(LayeredConfig {
        cli,
        gui_file,
        effective,
        gui,
    })
}

fn read_optional_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None); // Config is optional.
    }
    fs::read_to_string(path).map(Some).map_err(|e| {
        AppError::ConfigError(format!(
            "Failed to read config file at '{}': {}",
            path.display(),
            e
        ))
    })
}

fn parse_error(path: &Path, error: impl fmt::Display) -> AppError {
    AppError::ConfigError(format!(
        "Failed to parse config file at '{}': {}",
        path.display(),
        error
    ))
}

/// Loads the CLI configuration (`config.toml`) from `path`.
///
/// Returns a default `ConfigFile` if there is no path or the file doesn't
/// exist. Returns an error if the file exists but cannot be read or parsed
/// (same messages as upstream).
pub fn load_config_from(path: Option<&Path>) -> Result<ConfigFile> {
    let Some(path) = path else {
        return Ok(ConfigFile::default()); // No home dir, so no config.
    };
    match read_optional_file(path)? {
        None => Ok(ConfigFile::default()),
        Some(content) => {
            let config: ConfigFile = toml::from_str(&content).map_err(|e| parse_error(path, e))?;
            // Fork: `[resume.copilot]` is GUI-only; the installed CLI aborts
            // on keys it does not know, so it is refused like one.
            if config.resume.as_ref().is_some_and(|r| r.copilot.is_some()) {
                return Err(parse_error(
                    path,
                    "unknown field `copilot` in [resume], expected one of `command`, \
                     `default_args`, `pi`, `omp`, `codex`; `[resume.copilot]` belongs in gui.toml",
                ));
            }
            Ok(config)
        }
    }
}

/// Loads the CLI configuration from the environment's `config.toml`.
pub fn load_config(env: &HistoryEnv) -> Result<ConfigFile> {
    load_config_from(env.config_file().as_deref())
}

/// Loads `gui.toml` from `path` (empty when there is no path or file).
pub fn load_gui_config_from(path: Option<&Path>) -> Result<GuiConfigFile> {
    let Some(path) = path else {
        return Ok(GuiConfigFile::default());
    };
    match read_optional_file(path)? {
        None => Ok(GuiConfigFile::default()),
        Some(content) => toml::from_str(&content).map_err(|e| parse_error(path, e)),
    }
}

/// Serialize `gui` to TOML text (the exact content [`save_gui_config`] writes).
pub fn gui_config_to_string(gui: &GuiConfigFile) -> Result<String> {
    toml::to_string_pretty(gui)
        .map_err(|e| AppError::ConfigError(format!("Failed to serialize GUI config: {e}")))
}

/// Whether `name` names a `config.toml` on a Windows or macOS file system:
/// compared without ASCII case, ignoring the trailing dots and spaces that
/// Win32 strips (`CONFIG.TOML`, `config.toml.`).
fn is_cli_config_name(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy()
        .trim_end_matches(['.', ' '])
        .eq_ignore_ascii_case(CLI_CONFIG_FILE_NAME)
}

/// Whether writing `path` could replace a CLI `config.toml`: by its name
/// (any case, trailing dots or spaces), or, when it exists, by the name of
/// the file it resolves to (8.3 short names, symbolic links). A hard link
/// needs no check: the atomic rename replaces the directory entry, never
/// the content the links share.
fn may_be_cli_config(path: &Path) -> bool {
    if path.file_name().is_some_and(is_cli_config_name) {
        return true;
    }
    fs::canonicalize(path)
        .is_ok_and(|canonical| canonical.file_name().is_some_and(is_cli_config_name))
}

fn refuse_cli_config(path: &Path) -> AppError {
    AppError::ConfigError(format!(
        "Refusing to write '{}': the claude-history CLI config is read-only",
        path.display()
    ))
}

/// Atomically write `gui.toml` at `path` (temp file in the same directory,
/// then rename over the old file). Refuses to write a file that is or may
/// be a `config.toml` (in any letter case, with trailing dots or spaces, or
/// resolving to one): the CLI's config is never modified.
pub fn save_gui_config(path: &Path, gui: &GuiConfigFile) -> Result<()> {
    if may_be_cli_config(path) {
        return Err(refuse_cli_config(path));
    }
    let content = gui_config_to_string(gui)?;
    write_config_atomically(path, &content)
}

/// Write `content` to `path` through a temp file in the same directory.
fn write_config_atomically(path: &Path, content: &str) -> Result<()> {
    let write_error = |e: &dyn fmt::Display| {
        AppError::ConfigError(format!(
            "Failed to write config file at '{}': {}",
            path.display(),
            e
        ))
    };
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&parent).map_err(|e| write_error(&e))?;
    let mut tmp = tempfile::NamedTempFile::new_in(&parent).map_err(|e| write_error(&e))?;
    tmp.write_all(content.as_bytes())
        .and_then(|()| tmp.as_file().sync_all())
        .map_err(|e| write_error(&e))?;
    tmp.persist(path).map_err(|e| write_error(&e.error))?;
    Ok(())
}

/// Save `gui.toml` at the environment's GUI config location. Refuses when
/// that location is, or resolves to the same file as, the CLI's
/// `config.toml` (see [`save_gui_config`] for the name checks).
pub fn save_gui_config_for(env: &HistoryEnv, gui: &GuiConfigFile) -> Result<PathBuf> {
    let path = writable_gui_config_path(env)?;
    save_gui_config(&path, gui)?;
    Ok(path)
}

/// The environment's `gui.toml`, unless it is (or may be) the CLI's
/// `config.toml`.
fn writable_gui_config_path(env: &HistoryEnv) -> Result<PathBuf> {
    let path = env.gui_config_file().ok_or_else(|| {
        AppError::ConfigError("Could not determine the GUI config file location".to_string())
    })?;
    if let Some(cli) = env.config_file()
        && (cli == path || same_existing_file(&cli, &path))
    {
        return Err(refuse_cli_config(&path));
    }
    if may_be_cli_config(&path) {
        return Err(refuse_cli_config(&path));
    }
    Ok(path)
}

/// Save the settings the user changed into `gui.toml`, keeping everything
/// else of the file as it is now (K12).
///
/// `base` is the `gui.toml` content the edit started from, `edited` the
/// result of the edit. The file is read again (a missing file is an empty
/// document); when it does not parse as a [`GuiConfigFile`] nothing is
/// written and the error is `gui.toml has errors; fix or remove it before
/// saving: <e>`. Then every leaf setting (`[section] key`) whose value
/// differs between `base` and `edited` is set (or removed, when `edited`
/// has no value) in the document. Every `[gui]` field is optional (C-033):
/// a missing `[gui]` table equals an empty one, adding the table writes
/// only the keys the edit set, and setting a field to `None` (Reset)
/// removes its key, also for `theme` and `restore_session`. Comments,
/// formatting, the order of keys and every other key are preserved,
/// including keys another writer changed since `base` was read. The result
/// is validated by parsing it and written atomically, with the same refusal
/// of the CLI's `config.toml` as [`save_gui_config_for`]. Returns the path
/// written.
pub fn save_gui_config_changes(
    env: &HistoryEnv,
    base: &GuiConfigFile,
    edited: &GuiConfigFile,
) -> Result<PathBuf> {
    let path = writable_gui_config_path(env)?;
    let current = read_optional_file(&path)?.unwrap_or_default();
    if let Err(error) = toml::from_str::<GuiConfigFile>(&current) {
        return Err(AppError::ConfigError(format!(
            "gui.toml has errors; fix or remove it before saving: {error}"
        )));
    }
    let mut document = current.parse::<toml_edit::DocumentMut>().map_err(|error| {
        AppError::ConfigError(format!(
            "gui.toml has errors; fix or remove it before saving: {error}"
        ))
    })?;

    // Every gui.toml key is optional in the model (an absent `[gui]` table
    // and an empty one have no leaves), so a leaf missing on one side is a
    // key that side does not set: it is removed when the edit dropped it.
    let mut base_leaves = Vec::new();
    flatten_settings(&settings_value(base)?, &mut Vec::new(), &mut base_leaves);
    let mut edited_leaves = Vec::new();
    flatten_settings(
        &settings_value(edited)?,
        &mut Vec::new(),
        &mut edited_leaves,
    );
    let lookup = |leaves: &[(Vec<String>, toml::Value)], key: &[String]| {
        leaves
            .iter()
            .find(|(path, _)| path.as_slice() == key)
            .map(|(_, value)| value.clone())
    };
    let mut seen = std::collections::HashSet::new();
    let keys = base_leaves
        .iter()
        .chain(edited_leaves.iter())
        .map(|(path, _)| path.clone())
        .filter(|path| seen.insert(path.clone()))
        .collect::<Vec<_>>();
    for key in keys {
        let before = lookup(&base_leaves, &key);
        let after = lookup(&edited_leaves, &key);
        if before == after {
            continue;
        }
        match after {
            Some(value) => set_document_value(&mut document, &key, &value)?,
            None => remove_document_value(&mut document, &key),
        }
    }

    let content = document.to_string();
    toml::from_str::<GuiConfigFile>(&content).map_err(|error| {
        AppError::ConfigError(format!("Failed to update the GUI config: {error}"))
    })?;
    write_config_atomically(&path, &content)?;
    Ok(path)
}

fn settings_value(gui: &GuiConfigFile) -> Result<toml::Value> {
    toml::Value::try_from(gui)
        .map_err(|e| AppError::ConfigError(format!("Failed to serialize GUI config: {e}")))
}

/// Every non-table value of `value` with its key path, in order.
fn flatten_settings(
    value: &toml::Value,
    prefix: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, toml::Value)>,
) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                prefix.push(key.clone());
                flatten_settings(value, prefix, out);
                prefix.pop();
            }
        }
        leaf => out.push((prefix.clone(), leaf.clone())),
    }
}

fn edit_value(value: &toml::Value) -> Result<toml_edit::Value> {
    Ok(match value {
        toml::Value::String(text) => text.as_str().into(),
        toml::Value::Integer(number) => (*number).into(),
        toml::Value::Float(number) => (*number).into(),
        toml::Value::Boolean(flag) => (*flag).into(),
        toml::Value::Datetime(datetime) => datetime
            .to_string()
            .parse::<toml_edit::Value>()
            .map_err(|e| AppError::ConfigError(format!("Failed to update the GUI config: {e}")))?,
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(edit_value(item)?);
            }
            toml_edit::Value::Array(array)
        }
        toml::Value::Table(table) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, item) in table {
                inline.insert(key, edit_value(item)?);
            }
            toml_edit::Value::InlineTable(inline)
        }
    })
}

/// Set `key` (a path of table names and the leaf key) in `document`,
/// creating missing tables; an existing value keeps its decor (comments).
fn set_document_value(
    document: &mut toml_edit::DocumentMut,
    key: &[String],
    value: &toml::Value,
) -> Result<()> {
    let Some((leaf, tables)) = key.split_last() else {
        return Ok(());
    };
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    for name in tables {
        let entry = table
            .entry(name)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        if !entry.is_table_like() {
            *entry = toml_edit::Item::Table(toml_edit::Table::new());
        }
        table = entry
            .as_table_like_mut()
            .expect("the entry was made a table above");
    }
    let mut new_value = edit_value(value)?;
    if let Some(existing) = table.get_mut(leaf).and_then(toml_edit::Item::as_value_mut) {
        *new_value.decor_mut() = existing.decor().clone();
        *existing = new_value;
    } else {
        table.insert(leaf, toml_edit::Item::Value(new_value));
    }
    Ok(())
}

/// Remove `key` from `document` (nothing when it is absent).
fn remove_document_value(document: &mut toml_edit::DocumentMut, key: &[String]) {
    let Some((leaf, tables)) = key.split_last() else {
        return;
    };
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    for name in tables {
        match table
            .get_mut(name)
            .and_then(toml_edit::Item::as_table_like_mut)
        {
            Some(inner) => table = inner,
            None => return,
        }
    }
    table.remove(leaf);
}

/// Whether both paths exist and resolve to the same location.
fn same_existing_file(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_tui_defaults_to_no_excluded_projects() {
        let config: ConfigFile = toml::from_str("").unwrap();
        assert!(config.tui.unwrap_or_default().exclude_projects.is_empty());
    }

    #[test]
    fn empty_tui_table_defaults_to_no_excluded_projects() {
        let config: ConfigFile = toml::from_str("[tui]\n").unwrap();
        assert!(config.tui.unwrap_or_default().exclude_projects.is_empty());
    }

    #[test]
    fn tui_exclude_projects_preserves_exact_strings() {
        let config: ConfigFile = toml::from_str(
            r#"
[tui]
exclude_projects = ["Hidden", "hidden", " spaced "]
"#,
        )
        .unwrap();

        assert_eq!(
            config.tui.unwrap().exclude_projects,
            vec!["Hidden", "hidden", " spaced "]
        );
    }

    #[test]
    fn tui_exclude_paths_defaults_and_preserves_literal_prefixes() {
        for input in ["", "[tui]", "[tui]\nexclude_paths = []"] {
            let config: ConfigFile = toml::from_str(input).unwrap();
            assert!(config.tui.unwrap_or_default().exclude_paths.is_empty());
        }
        let config: ConfigFile = toml::from_str(
            r#"[tui]
exclude_paths = ["/tmp/", "/private/tmp/", "", " spaced "]
"#,
        )
        .unwrap();
        assert_eq!(
            config.tui.unwrap().exclude_paths,
            ["/tmp/", "/private/tmp/", "", " spaced "]
        );
        assert!(toml::from_str::<ConfigFile>("[tui]\nexclude_paths = [1]").is_err());
    }

    #[test]
    fn tui_semantic_search_parses_default_mode() {
        let config: ConfigFile = toml::from_str(
            r#"
[tui]
semantic_search = true
"#,
        )
        .unwrap();

        assert_eq!(config.tui.unwrap().semantic_search, Some(true));
    }

    #[test]
    fn semantic_daemon_is_off_and_pauses_on_battery_by_default() {
        let config: ConfigFile = toml::from_str("").unwrap();
        let semantic = config.semantic.unwrap_or_default();
        assert!(!semantic.daemon());
        assert!(semantic.pause_on_battery());

        let config: ConfigFile = toml::from_str(
            "[semantic]
daemon = true
pause_on_battery = false
",
        )
        .unwrap();
        let semantic = config.semantic.unwrap();
        assert!(semantic.daemon());
        assert!(!semantic.pause_on_battery());
        assert!(
            toml::from_str::<ConfigFile>(
                "[semantic]
deamon = true
"
            )
            .is_err()
        );
    }

    #[test]
    fn search_mode_parses_hybrid_mode() {
        let config: ConfigFile = toml::from_str(
            r#"
[search]
mode = "hybrid"
"#,
        )
        .unwrap();

        assert_eq!(config.search.unwrap().mode, Some(SearchMode::Hybrid));
    }

    #[test]
    fn search_mode_rejects_unknown_mode() {
        let err = toml::from_str::<ConfigFile>(
            r#"
[search]
mode = "vector"
"#,
        )
        .expect_err("unknown search mode should fail");

        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn codex_resume_args_are_separate_from_claude_ones() {
        let config: ConfigFile = toml::from_str(
            r#"
[resume]
default_args = ["--dangerously-skip-permissions"]

[resume.codex]
default_args = ["--yolo"]
"#,
        )
        .unwrap();
        let resume = config.resume.unwrap();
        use crate::history::model::SourceKind;
        assert_eq!(
            resume.default_args(SourceKind::Claude),
            ["--dangerously-skip-permissions"]
        );
        assert_eq!(resume.default_args(SourceKind::Codex), ["--yolo"]);
        assert!(resume.default_args(SourceKind::Pi).is_empty());
        assert!(
            ResumeConfig::default()
                .default_args(SourceKind::Codex)
                .is_empty()
        );
    }

    /// Fork: Copilot's `[resume.copilot]` is GUI-only: `config.toml` refuses
    /// it like an unknown key, `gui.toml` takes it and layers it like Codex's.
    #[test]
    fn copilot_resume_table_is_gui_only() {
        use crate::history::model::SourceKind;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[resume.copilot]\ndefault_args = [\"--yolo\"]\n").unwrap();
        let error = load_config_from(Some(&path)).unwrap_err().to_string();
        assert!(error.contains("unknown field `copilot`"), "{error}");
        assert!(error.contains(&path.display().to_string()), "{error}");
        // The other tables still load from the same file.
        std::fs::write(&path, "[resume.codex]\ndefault_args = [\"--yolo\"]\n").unwrap();
        assert!(load_config_from(Some(&path)).is_ok());

        let gui_path = dir.path().join("gui.toml");
        std::fs::write(
            &gui_path,
            "[resume.copilot]\ncommand = [\"mise\", \"x\"]\ndefault_args = [\"--yolo\"]\n\n\
             [gui]\ncopilot_path = \"/opt/copilot\"\n",
        )
        .unwrap();
        let gui = load_gui_config_from(Some(&gui_path)).unwrap();
        let resume = gui.resume.as_ref().unwrap();
        assert_eq!(resume.default_args(SourceKind::Copilot), ["--yolo"]);
        assert_eq!(
            resume.command(SourceKind::Copilot).unwrap().words(),
            ["mise", "x"]
        );
        assert_eq!(
            gui.gui_settings().program_path(SourceKind::Copilot),
            Some(Path::new("/opt/copilot"))
        );
        assert_eq!(
            GuiSettings::program_path_key(SourceKind::Copilot),
            "copilot_path"
        );

        // The overlay merges key by key, like Codex's table.
        let cli = ConfigFile::default().overlaid_with(&gui);
        let overlaid = cli.resume.unwrap();
        assert_eq!(overlaid.default_args(SourceKind::Copilot), ["--yolo"]);
        let top: GuiConfigFile =
            toml::from_str("[resume.copilot]\ndefault_args = [\"--other\"]\n").unwrap();
        let merged = ConfigFile {
            resume: Some(overlaid),
            ..ConfigFile::default()
        }
        .overlaid_with(&top)
        .resume
        .unwrap();
        assert_eq!(merged.default_args(SourceKind::Copilot), ["--other"]);
        assert_eq!(
            merged.command(SourceKind::Copilot).unwrap().words(),
            ["mise", "x"]
        );
        assert!(merged.default_args(SourceKind::Codex).is_empty());
    }

    /// Fork: `gui.toml` overrides `[resume.codex] default_args` on its own,
    /// and `[gui] codex_path` is a GUI key.
    #[test]
    fn codex_resume_args_and_path_layer_like_the_other_keys() {
        let cli: ConfigFile = toml::from_str(
            "[resume]\ndefault_args = [\"--verbose\"]\n\n[resume.codex]\ndefault_args = [\"--yolo\"]\n",
        )
        .unwrap();
        let gui: GuiConfigFile = toml::from_str(
            "[resume.codex]\ndefault_args = [\"--full-auto\"]\n\n[gui]\ncodex_path = \"C:/tools/codex.exe\"\n",
        )
        .unwrap();
        let resume = cli.clone().overlaid_with(&gui).resume.unwrap();
        use crate::history::model::SourceKind;
        assert_eq!(resume.default_args(SourceKind::Claude), ["--verbose"]);
        assert_eq!(resume.default_args(SourceKind::Codex), ["--full-auto"]);
        let untouched = cli.overlaid_with(&GuiConfigFile {
            resume: Some(ResumeConfig::default()),
            ..GuiConfigFile::default()
        });
        assert_eq!(
            untouched.resume.unwrap().default_args(SourceKind::Codex),
            ["--yolo"]
        );
        assert_eq!(
            gui.gui_settings().codex_path,
            Some(PathBuf::from("C:/tools/codex.exe"))
        );
        assert!(toml::from_str::<ConfigFile>("[resume.codex]\nmodel = \"x\"\n").is_err());
    }

    #[test]
    fn semantic_daemon_table_from_the_cli_config_loads_and_survives_the_overlay() {
        let cli: ConfigFile = toml::from_str(
            "[semantic]
daemon = true
pause_on_battery = false
",
        )
        .unwrap();
        let gui: GuiConfigFile = toml::from_str(
            "[display]
plain = true
",
        )
        .unwrap();
        let effective = cli.clone().overlaid_with(&gui);
        assert_eq!(effective.semantic, cli.semantic);
        assert!(effective.semantic.unwrap().daemon());
        // gui.toml has no `[semantic]` table: the GUI ignores the daemon.
        assert!(
            toml::from_str::<GuiConfigFile>(
                "[semantic]
daemon = false
"
            )
            .is_err()
        );
    }

    /// `config.toml` alone, as the layered configuration of a GUI without
    /// `gui.toml`.
    fn layered_cli(cli: ConfigFile) -> LayeredConfig {
        LayeredConfig {
            effective: cli.clone(),
            cli,
            gui_file: GuiConfigFile::default(),
            gui: GuiSettings::default(),
        }
    }

    #[test]
    fn agent_commands_default_to_the_agent_binaries() {
        use crate::history::model::SourceKind;
        let env = HistoryEnv::sandbox(Path::new("/home/me"));
        let config = layered_cli(ConfigFile {
            resume: Some(ResumeConfig::default()),
            ..ConfigFile::default()
        });
        for source in [
            SourceKind::Claude,
            SourceKind::Pi,
            SourceKind::Omp,
            SourceKind::Codex,
            SourceKind::Copilot,
        ] {
            assert_eq!(config.launcher(&env, source), Launcher::default_for(source));
            assert_eq!(
                LayeredConfig::default().launch(&env, source),
                Launch::default_for(source)
            );
        }
    }

    #[test]
    fn agent_commands_are_configured_per_agent() {
        use crate::history::model::SourceKind;
        let config: ConfigFile = toml::from_str(
            r#"
[resume]
command = "my claude"
default_args = ["--verbose"]

[resume.pi]
command = ["mise", "exec", "--", "pi"]

[resume.omp]
command = "/opt/omp/bin/omp"

[resume.codex]
command = ["codex-wrapper", "--profile", "work"]
default_args = ["--yolo"]
"#,
        )
        .unwrap();
        let config = layered_cli(config);
        let env = HistoryEnv::sandbox(Path::new("/home/me"));

        let claude = config.launch(&env, SourceKind::Claude);
        assert_eq!(claude.launcher.program, PathBuf::from("my claude"));
        assert!(claude.launcher.args.is_empty());
        assert_eq!(
            claude.launcher.configured_at,
            Some(ConfiguredAt::Config("[resume]"))
        );
        assert_eq!(claude.default_args, ["--verbose"]);

        let pi = config.launch(&env, SourceKind::Pi);
        assert_eq!(pi.launcher.program, PathBuf::from("mise"));
        assert_eq!(pi.launcher.args, ["exec", "--", "pi"]);
        assert_eq!(
            pi.launcher.configured_at,
            Some(ConfiguredAt::Config("[resume.pi]"))
        );
        assert!(pi.default_args.is_empty());

        let omp = config.launcher(&env, SourceKind::Omp);
        assert_eq!(omp.program, PathBuf::from("/opt/omp/bin/omp"));
        assert_eq!(
            omp.configured_at,
            Some(ConfiguredAt::Config("[resume.omp]"))
        );

        let codex = config.launch(&env, SourceKind::Codex);
        assert_eq!(codex.launcher.program, PathBuf::from("codex-wrapper"));
        assert_eq!(codex.launcher.args, ["--profile", "work"]);
        assert_eq!(codex.default_args, ["--yolo"]);
    }

    #[test]
    fn agent_commands_must_name_a_program() {
        for command in [r#""""#, "[]", r#"[" "]"#, "1"] {
            let error =
                toml::from_str::<ConfigFile>(&format!("[resume.pi]\ncommand = {command}\n"))
                    .expect_err(command);
            assert!(error.to_string().contains("command must"), "{error}");
        }
        assert!(toml::from_str::<ConfigFile>("[resume.pi]\ndefault_args = []\n").is_err());
        // Fork: gui.toml takes the same keys, and the model refuses an
        // empty command too.
        assert!(
            toml::from_str::<GuiConfigFile>("[resume.omp]\ncommand = \"\"\n")
                .unwrap_err()
                .to_string()
                .contains("command must name a program")
        );
        assert_eq!(AgentCommand::new(Vec::new()), None);
        assert_eq!(AgentCommand::new(vec![" ".into(), "x".into()]), None);
        assert_eq!(
            AgentCommand::new(vec!["pi".into()]).unwrap().words(),
            ["pi"]
        );
    }

    /// Fork (decision 17): gui.toml's `command` > `[gui] <agent>_path` >
    /// config.toml's `command` > the agent's binary; each agent on its own.
    #[test]
    fn agent_commands_of_gui_toml_and_program_paths_take_precedence() {
        use crate::history::model::SourceKind;
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let cli: ConfigFile = toml::from_str(
            "[resume]\ncommand = \"cli-claude\"\n\n[resume.pi]\ncommand = \"cli-pi\"\n\n\
             [resume.omp]\ncommand = [\"cli-omp\", \"--x\"]\n",
        )
        .unwrap();
        let gui: GuiConfigFile = toml::from_str(
            "[resume]\ncommand = [\"gui-claude\", \"--y\"]\n\n[gui]\nclaude_path = \"/opt/claude\"\n\
             pi_path = \"~/bin/pi\"\ncodex_path = \"/opt/codex\"\n",
        )
        .unwrap();
        let launcher = |source| agent_launcher(&env, &cli, &gui, source);
        let with_command: GuiConfigFile = toml::from_str(
            "[resume.copilot]\ncommand = \"copilot-wrapper\"\n\n[gui]\ncopilot_path = \"/opt/copilot\"\n",
        )
        .unwrap();
        // `[resume.copilot] command` > `[gui] copilot_path` > the binary.
        let command = agent_launcher(&env, &cli, &with_command, SourceKind::Copilot);
        assert_eq!(command.program, Path::new("copilot-wrapper"));
        assert_eq!(
            command.configured_at,
            Some(ConfiguredAt::Gui("[resume.copilot]"))
        );
        let path_only: GuiConfigFile =
            toml::from_str("[gui]\ncopilot_path = \"/opt/copilot\"\n").unwrap();
        let path = agent_launcher(&env, &cli, &path_only, SourceKind::Copilot);
        assert_eq!(path.program, Path::new("/opt/copilot"));
        assert_eq!(
            path.configured_at,
            Some(ConfiguredAt::GuiPath("copilot_path"))
        );
        let default = launcher(SourceKind::Copilot);
        assert_eq!(default, Launcher::default_for(SourceKind::Copilot));
        assert_eq!(default.program, Path::new("copilot"));

        let claude = launcher(SourceKind::Claude);
        assert_eq!(claude.program, Path::new("gui-claude"));
        assert_eq!(claude.args, ["--y"]);
        assert_eq!(claude.configured_at, Some(ConfiguredAt::Gui("[resume]")));
        let pi = launcher(SourceKind::Pi);
        assert_eq!(pi.program, root.path().join("bin/pi"));
        assert!(pi.args.is_empty());
        assert_eq!(pi.configured_at, Some(ConfiguredAt::GuiPath("pi_path")));
        let omp = launcher(SourceKind::Omp);
        assert_eq!(omp.words(), ["cli-omp", "--x"]);
        assert_eq!(
            omp.configured_at,
            Some(ConfiguredAt::Config("[resume.omp]"))
        );
        let codex = launcher(SourceKind::Codex);
        assert_eq!(codex.program, Path::new("/opt/codex"));
        assert_eq!(
            codex.configured_at.unwrap().to_string(),
            "`codex_path` under [gui] in gui.toml"
        );

        // The overlay merges the commands key by key; the layered config
        // applies the same precedence.
        let layered = LayeredConfig {
            effective: cli.clone().overlaid_with(&gui),
            cli: cli.clone(),
            gui: gui.gui_settings(),
            gui_file: gui.clone(),
        };
        let effective = layered.effective.resume.as_ref().unwrap();
        assert_eq!(
            effective.command(SourceKind::Claude).unwrap().words(),
            ["gui-claude", "--y"]
        );
        assert_eq!(
            effective.command(SourceKind::Pi).unwrap().words(),
            ["cli-pi"]
        );
        assert_eq!(layered.launcher(&env, SourceKind::Pi), pi);
        assert_eq!(
            layered.launcher(&env, SourceKind::Codex).configured_at,
            Some(ConfiguredAt::GuiPath("codex_path"))
        );
    }

    /// Fork: gui.toml stores a command as written: one program as a string,
    /// a program with arguments as an array.
    #[test]
    fn agent_commands_round_trip_through_gui_toml() {
        let gui = GuiConfigFile {
            resume: Some(ResumeConfig {
                command: AgentCommand::new(vec!["my-claude".into()]),
                pi: Some(LauncherConfig {
                    command: AgentCommand::new(vec!["mise".into(), "exec".into()]),
                }),
                ..ResumeConfig::default()
            }),
            ..GuiConfigFile::default()
        };
        let text = gui_config_to_string(&gui).unwrap();
        assert!(text.contains("command = \"my-claude\""), "{text}");
        assert!(text.contains("[resume.pi]"), "{text}");
        assert!(text.contains("\"mise\""), "{text}");
        assert_eq!(toml::from_str::<GuiConfigFile>(&text).unwrap(), gui);
    }

    #[test]
    fn agent_settings_parse_independently_from_tui() {
        let config: ConfigFile = toml::from_str(
            r#"
[search]
mode = "lexical"

[tui]
semantic_search = true
exclude_projects = ["tui-only"]

[agent]
scope = "local"
mode = "hybrid"
output_chars = 9000
top = 12
within_top = 24
hits_per_conversation = 3
exclude_projects = ["agent-only"]
tools = true
tool_results = true
thinking = true
subagents = true
"#,
        )
        .unwrap();

        let agent = config.agent.unwrap();
        assert_eq!(agent.scope, Some(AgentScopeConfig::Local));
        assert_eq!(agent.mode, Some(SearchMode::Hybrid));
        assert_eq!(agent.output_chars, Some(9000));
        assert_eq!(agent.top, Some(12));
        assert_eq!(agent.within_top, Some(24));
        assert_eq!(agent.hits_per_conversation, Some(3));
        assert_eq!(agent.exclude_projects, vec!["agent-only"]);
        assert_eq!(agent.tools, Some(true));
        assert_eq!(agent.tool_results, Some(true));
        assert_eq!(agent.thinking, Some(true));
        assert_eq!(agent.subagents, Some(true));
    }

    #[test]
    fn agent_counts_reject_zero() {
        let error = toml::from_str::<ConfigFile>("[agent]\noutput_chars = 0\n")
            .expect_err("zero output budget should fail");

        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn parses_function_key_binding() {
        let binding = parse_key_binding("f2").unwrap();
        assert_eq!(binding.key, Key::F(2));
        assert_eq!(binding.mods, KeyMods::NONE);
        assert_eq!(binding.short_label(), "F2");
        assert_eq!(binding.help_label(), "F2");
    }

    #[test]
    fn applies_rename_key_config() {
        let keys = KeyBindings::from_config(Some(KeysConfig {
            rename: Some(parse_key_binding("alt+r").unwrap()),
            ..Default::default()
        }));

        assert!(keys.rename.matches(Key::Char('r'), KeyMods::ALT));
    }

    #[test]
    fn applies_refresh_key_config() {
        let keys = KeyBindings::from_config(Some(KeysConfig {
            refresh: Some(parse_key_binding("alt+u").unwrap()),
            ..Default::default()
        }));

        assert!(keys.refresh.matches(Key::Char('u'), KeyMods::ALT));
    }

    #[test]
    fn applies_all_kinds_key_config() {
        // Upstream cb990e6: `[keys] all_kinds`, Ctrl+G by default.
        assert_eq!(KeyBindings::default().all_kinds, KeyChord::ctrl('g'));
        let config: ConfigFile = toml::from_str("[keys]\nall_kinds = \"alt+k\"\n").unwrap();
        let keys = KeyBindings::from_config(config.keys);
        assert!(keys.all_kinds.matches(Key::Char('k'), KeyMods::ALT));
        assert_eq!(keys.refresh, KeyChord::ctrl('l'));
        let config: ConfigFile = toml::from_str("[keys]\nall_kinds = \"ctrl+t\"\n").unwrap();
        assert!(KeyBindings::from_config(config.keys).conflicts_with_ctrl_t());
    }

    #[test]
    fn applies_user_only_key_config() {
        // Upstream ae7ae39: `[keys] user_only` (upstream default Ctrl+V; the
        // fork's is Alt+U, since Ctrl+V pastes into the search field).
        assert_eq!(KeyBindings::default().user_only, KeyChord::alt('u'));
        let config: ConfigFile = toml::from_str("[keys]\nuser_only = \"ctrl+v\"\n").unwrap();
        let keys = KeyBindings::from_config(config.keys);
        assert!(keys.user_only.matches(Key::Char('v'), KeyMods::CTRL));
        assert_eq!(keys.all_kinds, KeyChord::ctrl('g'));
        let config: ConfigFile = toml::from_str("[keys]\nuser_only = \"ctrl+t\"\n").unwrap();
        assert!(KeyBindings::from_config(config.keys).conflicts_with_ctrl_t());
    }

    // --- KeyChord parity with upstream config.rs:261-402 -------------------

    #[test]
    fn key_binding_grammar_matches_upstream() {
        assert_eq!(parse_key_binding("ctrl+r"), Ok(KeyChord::ctrl('r')));
        assert_eq!(parse_key_binding(" Control + R "), Ok(KeyChord::ctrl('r')));
        assert_eq!(parse_key_binding("META+x"), Ok(KeyChord::alt('x')));
        assert_eq!(parse_key_binding("alt+F"), Ok(KeyChord::alt('f')));
        assert_eq!(parse_key_binding("F12"), Ok(KeyChord::plain(Key::F(12))));
        assert_eq!(parse_key_binding("f"), Ok(KeyChord::plain(Key::Char('f'))));
        assert_eq!(
            parse_key_binding("shift+r"),
            Err("Unknown modifier: shift".to_string())
        );
        assert_eq!(
            parse_key_binding("ctrl+f2"),
            Err("Function keys do not support modifiers: ctrl+f2".to_string())
        );
        assert_eq!(
            parse_key_binding("ctrl+alt+r"),
            Err("Invalid key binding: ctrl+alt+r".to_string())
        );
        for (input, key) in [
            ("f0", "f0"),
            ("f13", "f13"),
            ("fx", "fx"),
            ("enter", "enter"),
            ("é", "é"),
            ("ctrl+Tab", "Tab"),
        ] {
            assert_eq!(parse_key_binding(input), Err(format!("Unknown key: {key}")));
        }
    }

    #[test]
    fn key_binding_labels_match_upstream() {
        assert_eq!(KeyChord::ctrl('r').short_label(), "^R");
        assert_eq!(KeyChord::ctrl('r').help_label(), "Ctrl+R");
        assert_eq!(KeyChord::alt('f').short_label(), "M-F");
        assert_eq!(KeyChord::alt('f').help_label(), "Alt+F");
        assert_eq!(KeyChord::plain(Key::Char('q')).short_label(), "Q");
        // A function key with a modifier cannot be parsed and has no label.
        assert_eq!(KeyChord::new(Key::F(2), KeyMods::CTRL).help_label(), "");
    }

    #[test]
    fn key_binding_gpui_and_config_spellings() {
        for (input, gpui, config) in [
            ("ctrl+r", "ctrl-r", "ctrl+r"),
            ("Alt+F", "alt-f", "alt+f"),
            ("f2", "f2", "f2"),
            ("x", "x", "x"),
        ] {
            let chord: KeyChord = input.parse().unwrap();
            assert_eq!(chord.gpui_keystroke(), gpui);
            assert_eq!(chord.config_string(), config);
            assert_eq!(chord.to_string().parse::<KeyChord>(), Ok(chord));
        }
    }

    #[test]
    fn default_bindings_and_ctrl_t_conflict() {
        let defaults = KeyBindings::default();
        assert_eq!(defaults.resume, KeyChord::ctrl('r'));
        assert_eq!(defaults.fork, KeyChord::ctrl('f'));
        assert_eq!(defaults.rename, KeyChord::plain(Key::F(2)));
        assert_eq!(defaults.delete, KeyChord::ctrl('x'));
        assert_eq!(defaults.refresh, KeyChord::ctrl('l'));
        assert!(!defaults.conflicts_with_ctrl_t());

        let config: ConfigFile = toml::from_str("[keys]\nrefresh = \"ctrl+t\"\n").unwrap();
        assert!(KeyBindings::from_config(config.keys).conflicts_with_ctrl_t());
        let config: ConfigFile = toml::from_str("[keys]\nfork = \"alt+t\"\n").unwrap();
        assert!(!KeyBindings::from_config(config.keys).conflicts_with_ctrl_t());
    }

    #[test]
    fn invalid_key_binding_is_a_config_parse_error() {
        let error = toml::from_str::<ConfigFile>("[keys]\nresume = \"hyper+r\"\n").unwrap_err();
        assert!(error.to_string().contains("Unknown modifier: hyper"));
    }

    // --- loading and layering ----------------------------------------------

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn missing_files_and_paths_yield_defaults() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let layered = load_layered_config(&env).unwrap();
        assert_eq!(layered, LayeredConfig::default());
        assert_eq!(layered.gui, GuiSettings::default());
        assert!(layered.gui.restore_session());
        assert_eq!(layered.gui.restore_session, None);
        assert_eq!(layered.gui.theme(), ThemePreference::System);
        assert_eq!(layered.gui.theme, None);
        assert_eq!(load_config_from(None).unwrap(), ConfigFile::default());
        assert_eq!(
            load_gui_config_from(None).unwrap(),
            GuiConfigFile::default()
        );
    }

    #[test]
    fn cli_config_keeps_strict_upstream_schema() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let config = env.config_file().unwrap();
        // A [gui] table in config.toml breaks the CLI, so core rejects it too.
        write(&config, "[gui]\ntheme = \"dark\"\n");
        let error = load_layered_config(&env).unwrap_err().to_string();
        assert!(error.starts_with("Configuration error: Failed to parse config file at '"));
        assert!(error.contains("config.toml"));
    }

    #[test]
    fn gui_config_rejects_unknown_keys_and_names_its_file() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        write(&env.gui_config_file().unwrap(), "[gui]\ncolour = \"red\"\n");
        let error = load_layered_config(&env).unwrap_err().to_string();
        assert!(error.contains("Failed to parse config file at"));
        assert!(error.contains("gui.toml"));
        write(&env.gui_config_file().unwrap(), "[agent]\ntop = 0\n");
        assert!(
            load_layered_config(&env)
                .unwrap_err()
                .to_string()
                .contains("greater than zero")
        );
    }

    #[test]
    fn gui_toml_overrides_config_toml_which_overrides_defaults() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        write(
            &env.config_file().unwrap(),
            r#"
global = true

[display]
no_tools = true
last = false
show_thinking = true

[resume]
default_args = ["--verbose"]

[keys]
resume = "alt+r"
fork = "alt+f"

[tui]
exclude_projects = ["cli-hidden"]
semantic_search = true

[search]
mode = "semantic"

[agent]
scope = "local"
top = 5
exclude_projects = ["cli-agent"]
tools = true
"#,
        );
        write(
            &env.gui_config_file().unwrap(),
            r#"
[display]
last = true

[keys]
fork = "ctrl+g"

[tui]
exclude_projects = []

[agent]
top = 7

[gui]
theme = "dark"
terminal = "wt.exe -d {cwd} {command}"
claude_path = "C:/tools/claude.exe"
default_workspace = "C:/code/project"
restore_session = false
"#,
        );

        let layered = load_layered_config(&env).unwrap();
        let effective = &layered.effective;

        let display = effective.display.as_ref().unwrap();
        assert_eq!(display.no_tools, Some(true)); // config.toml
        assert_eq!(display.last, Some(true)); // gui.toml wins
        assert_eq!(display.show_thinking, Some(true));
        assert_eq!(display.plain, None); // default

        assert_eq!(
            effective.resume.as_ref().unwrap().default_args,
            Some(vec!["--verbose".to_string()])
        );

        let keys = KeyBindings::from_config(effective.keys.clone());
        assert_eq!(keys.resume, KeyChord::alt('r'));
        assert_eq!(keys.fork, KeyChord::ctrl('g'));
        assert_eq!(keys.rename, KeyChord::plain(Key::F(2)));

        let tui = effective.tui.as_ref().unwrap();
        assert!(tui.exclude_projects.is_empty(), "explicit [] overrides");
        assert_eq!(tui.semantic_search, Some(true));
        assert_eq!(
            effective.search.as_ref().unwrap().mode,
            Some(SearchMode::Semantic)
        );

        let agent = effective.agent.as_ref().unwrap();
        assert_eq!(agent.scope, Some(AgentScopeConfig::Local));
        assert_eq!(agent.top, Some(7));
        assert_eq!(agent.exclude_projects, vec!["cli-agent"]);
        assert_eq!(agent.tools, Some(true));
        assert_eq!(effective.global, Some(true));

        // The CLI layer is kept as parsed.
        assert_eq!(layered.cli.display.as_ref().unwrap().last, Some(false));

        assert_eq!(layered.gui.theme, Some(ThemePreference::Dark));
        assert_eq!(layered.gui.theme(), ThemePreference::Dark);
        assert_eq!(
            layered.gui.terminal.as_deref(),
            Some("wt.exe -d {cwd} {command}")
        );
        assert_eq!(
            layered.gui.claude_path,
            Some(PathBuf::from("C:/tools/claude.exe"))
        );
        assert_eq!(layered.gui.pi_path, None);
        assert_eq!(
            layered.gui.default_workspace,
            Some(PathBuf::from("C:/code/project"))
        );
        assert_eq!(layered.gui.restore_session, Some(false));
        assert!(!layered.gui.restore_session());
    }

    #[test]
    fn default_workspace_scope_is_validated_and_normalized() {
        let settings = |dir: Option<&str>| GuiSettings {
            default_workspace: dir.map(PathBuf::from),
            ..GuiSettings::default()
        };
        assert_eq!(settings(None).default_workspace_scope().unwrap(), None);
        assert!(matches!(
            settings(Some("project")).default_workspace_scope(),
            Err(AppError::InvalidWorkspace(_))
        ));
        let (dir, with_separator) = if cfg!(windows) {
            (r"C:\code\project", "C:/code/project/")
        } else {
            ("/code/project", "/code/project/")
        };
        let scope = settings(Some(with_separator))
            .default_workspace_scope()
            .unwrap()
            .unwrap();
        assert_eq!(scope.dir(), Path::new(dir));
        assert_eq!(scope, WorkspaceScope::new(dir).unwrap());
    }

    #[test]
    fn gui_sections_without_cli_sections_still_apply() {
        let gui: GuiConfigFile = toml::from_str("[search]\nmode = \"hybrid\"\n").unwrap();
        let effective = ConfigFile::default().overlaid_with(&gui);
        assert_eq!(effective.search.unwrap().mode, Some(SearchMode::Hybrid));
        assert_eq!(effective.display, None);
        // An omitted list keeps the config.toml value.
        let cli: ConfigFile = toml::from_str("[tui]\nexclude_projects = [\"x\"]\n").unwrap();
        let gui: GuiConfigFile = toml::from_str("[tui]\nsemantic_search = false\n").unwrap();
        let tui = cli.overlaid_with(&gui).tui.unwrap();
        assert_eq!(tui.exclude_projects, vec!["x"]);
        assert_eq!(tui.semantic_search, Some(false));
    }

    /// `[tui] exclude_paths` (upstream 982aea9) layers like
    /// `exclude_projects`: omitted in gui.toml keeps config.toml's prefixes,
    /// a gui.toml list wins and `[]` clears them.
    #[test]
    fn gui_toml_overrides_exclude_paths_and_an_empty_list_clears_them() {
        let cli: ConfigFile =
            toml::from_str("[tui]\nexclude_paths = [\"/tmp/\"]\nexclude_projects = [\"x\"]\n")
                .unwrap();
        let gui: GuiConfigFile = toml::from_str("[tui]\nexclude_projects = []\n").unwrap();
        let tui = cli.clone().overlaid_with(&gui).tui.unwrap();
        assert_eq!(tui.exclude_paths, vec!["/tmp/"]);
        assert!(tui.exclude_projects.is_empty());

        let gui: GuiConfigFile =
            toml::from_str("[tui]\nexclude_paths = ['C:\\Temp\\', \"/var/tmp/\"]\n").unwrap();
        let tui = cli.clone().overlaid_with(&gui).tui.unwrap();
        assert_eq!(tui.exclude_paths, vec![r"C:\Temp\", "/var/tmp/"]);
        assert_eq!(tui.exclude_projects, vec!["x"]);

        let gui: GuiConfigFile = toml::from_str("[tui]\nexclude_paths = []\n").unwrap();
        assert!(
            cli.overlaid_with(&gui)
                .tui
                .unwrap()
                .exclude_paths
                .is_empty()
        );
        assert!(toml::from_str::<GuiConfigFile>("[tui]\nexclude_paths = [1]").is_err());
    }

    #[test]
    fn save_writes_only_gui_toml_and_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let cli_path = env.config_file().unwrap();
        let cli_text = "[display]\nlast = false\n";
        write(&cli_path, cli_text);

        let gui = GuiConfigFile {
            display: Some(DisplayConfig {
                show_thinking: Some(true),
                ..Default::default()
            }),
            keys: Some(KeysConfig {
                rename: Some(KeyChord::alt('n')),
                ..Default::default()
            }),
            tui: Some(TuiOverrides {
                exclude_projects: Some(Vec::new()),
                exclude_paths: Some(vec!["/tmp/".to_string()]),
                semantic_search: None,
            }),
            search: Some(SearchConfig {
                mode: Some(SearchMode::Exact),
            }),
            agent: Some(AgentOverrides {
                scope: Some(AgentScopeConfig::Global),
                output_chars: Some(9000),
                ..Default::default()
            }),
            gui: Some(GuiSettings {
                theme: Some(ThemePreference::Light),
                omp_path: Some(PathBuf::from("/usr/local/bin/omp")),
                ..Default::default()
            }),
            ..Default::default()
        };
        let saved = save_gui_config_for(&env, &gui).unwrap();
        assert_eq!(saved, env.gui_config_file().unwrap());

        let text = fs::read_to_string(&saved).unwrap();
        assert!(text.contains("rename = \"alt+n\""), "{text}");
        assert!(text.contains("theme = \"light\""), "{text}");
        assert!(text.contains("exclude_projects = []"), "{text}");
        assert!(text.contains("exclude_paths = [\"/tmp/\"]"), "{text}");
        assert!(
            !text.contains("no_tools"),
            "unset values are omitted: {text}"
        );
        assert_eq!(load_gui_config_from(Some(&saved)).unwrap(), gui);
        // The CLI file is untouched and nothing else appeared next to it.
        assert_eq!(fs::read_to_string(&cli_path).unwrap(), cli_text);
        let mut names = fs::read_dir(saved.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["config.toml", "gui.toml"]);

        // Saving again replaces the file atomically.
        save_gui_config(&saved, &GuiConfigFile::default()).unwrap();
        assert_eq!(fs::read_to_string(&saved).unwrap(), "");
        let layered = load_layered_config(&env).unwrap();
        assert_eq!(layered.effective.display.unwrap().last, Some(false));
    }

    #[test]
    fn save_refuses_to_touch_the_cli_config() {
        let root = tempfile::tempdir().unwrap();
        let mut env = HistoryEnv::sandbox(root.path());
        let cli_path = env.config_file().unwrap();
        assert!(save_gui_config(&cli_path, &GuiConfigFile::default()).is_err());
        assert!(!cli_path.exists());

        env.gui_config_file = Some(root.path().join("same.toml"));
        env.config_file = Some(root.path().join("same.toml"));
        assert!(save_gui_config_for(&env, &GuiConfigFile::default()).is_err());
        assert!(!root.path().join("same.toml").exists());
    }

    #[test]
    fn save_refuses_aliases_of_the_cli_config() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let cli_path = env.config_file().unwrap();
        let cli_text = "[display]\nlast = false\n";
        write(&cli_path, cli_text);
        let dir = cli_path.parent().unwrap().to_path_buf();

        // Windows (and macOS) file systems ignore case, and Win32 strips
        // trailing dots and spaces: all of these name the CLI file there.
        for alias in [
            "CONFIG.TOML",
            "Config.toml",
            "config.TOML",
            "config.toml.",
            "config.toml. .",
        ] {
            assert!(
                save_gui_config(&dir.join(alias), &GuiConfigFile::default()).is_err(),
                "{alias}"
            );
            let mut aliased = HistoryEnv::sandbox(root.path());
            aliased.gui_config_file = Some(dir.join(alias));
            assert!(
                save_gui_config_for(&aliased, &GuiConfigFile::default()).is_err(),
                "{alias}"
            );
        }
        assert_eq!(fs::read_to_string(&cli_path).unwrap(), cli_text);

        // A CLI config at another name: a path resolving to the same file is
        // refused too (Windows resolves the letter case).
        let custom_cli = dir.join("cli-settings.toml");
        write(&custom_cli, cli_text);
        let mut custom = HistoryEnv::sandbox(root.path());
        custom.config_file = Some(custom_cli.clone());
        custom.gui_config_file = Some(custom_cli.clone());
        assert!(save_gui_config_for(&custom, &GuiConfigFile::default()).is_err());
        if cfg!(windows) {
            custom.gui_config_file = Some(dir.join("CLI-SETTINGS.TOML"));
            assert!(save_gui_config_for(&custom, &GuiConfigFile::default()).is_err());
        }
        assert_eq!(fs::read_to_string(&custom_cli).unwrap(), cli_text);

        // The regular GUI file is still written.
        assert!(save_gui_config_for(&env, &GuiConfigFile::default()).is_ok());
        assert_eq!(fs::read_to_string(&cli_path).unwrap(), cli_text);
    }

    // --- K12: format-preserving saves ---

    fn themed(theme: ThemePreference) -> GuiConfigFile {
        GuiConfigFile {
            gui: Some(GuiSettings {
                theme: Some(theme),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn save_changes_preserves_comments_order_and_untouched_keys() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let path = env.gui_config_file().unwrap();
        let original = "\
# My GUI settings
[display]
last = false # keep first messages

[gui]
# the look
theme = \"dark\" # night owl
restore_session = true
terminal = \"wt -w 0\"
";
        write(&path, original);
        let base = load_gui_config_from(Some(&path)).unwrap();
        let mut edited = base.clone();
        let gui = edited.gui.as_mut().unwrap();
        gui.theme = Some(ThemePreference::Light);
        gui.terminal = None;
        edited.keys = Some(KeysConfig {
            rename: Some(parse_key_binding("alt+n").unwrap()),
            ..Default::default()
        });

        let saved = save_gui_config_changes(&env, &base, &edited).unwrap();
        assert_eq!(saved, path);
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# My GUI settings\n[display]\nlast = false # keep first messages\n"),
            "{text}"
        );
        assert!(
            text.contains("# the look\ntheme = \"light\" # night owl\n"),
            "{text}"
        );
        assert!(!text.contains("terminal"), "{text}");
        assert!(text.contains("[keys]\nrename = \"alt+n\""), "{text}");
        assert_eq!(load_gui_config_from(Some(&path)).unwrap(), edited);

        // Nothing changed: the file is left byte for byte.
        let before = fs::read(&path).unwrap();
        save_gui_config_changes(&env, &edited, &edited).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn save_changes_keeps_keys_changed_by_another_writer() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let path = env.gui_config_file().unwrap();
        write(&path, "[gui]\ntheme = \"system\"\n");
        let base = load_gui_config_from(Some(&path)).unwrap();

        // Meanwhile the View menu switched the theme (another writer).
        save_gui_config_changes(&env, &base, &themed(ThemePreference::Dark)).unwrap();
        // The settings dialog, opened before, only changes the exclusions.
        let mut edited = base.clone();
        edited.tui = Some(TuiOverrides {
            exclude_projects: Some(vec!["scratch".to_string()]),
            semantic_search: None,
            ..Default::default()
        });
        save_gui_config_changes(&env, &base, &edited).unwrap();

        let saved = load_gui_config_from(Some(&path)).unwrap();
        assert_eq!(saved.gui.unwrap().theme, Some(ThemePreference::Dark));
        assert_eq!(
            saved.tui.unwrap().exclude_projects,
            Some(vec!["scratch".to_string()])
        );
    }

    #[test]
    fn save_changes_from_a_base_without_a_gui_table_writes_only_the_changed_keys() {
        // K12 / PD13 on a fresh install: no gui.toml (or one without
        // [gui]) when the dialog opens.
        for original in [None, Some("# mine\n[display]\nlast = true\n")] {
            let root = tempfile::tempdir().unwrap();
            let env = HistoryEnv::sandbox(root.path());
            let path = env.gui_config_file().unwrap();
            if let Some(text) = original {
                write(&path, text);
            }
            let base = load_gui_config_from(Some(&path)).unwrap();
            assert!(base.gui.is_none());

            // The View menu switches the theme (another writer) ...
            let mut theme_edit = base.clone();
            theme_edit.gui = Some(GuiSettings {
                theme: Some(ThemePreference::Dark),
                ..Default::default()
            });
            save_gui_config_changes(&env, &base, &theme_edit).unwrap();
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains("theme = \"dark\""), "{text}");
            assert!(
                !text.contains("restore_session"),
                "a default nobody changed is not written: {text}"
            );
            // ... and a third writer turns session restore off.
            let mut restore_edit = base.clone();
            restore_edit.gui = Some(GuiSettings {
                restore_session: Some(false),
                ..Default::default()
            });
            save_gui_config_changes(&env, &base, &restore_edit).unwrap();

            // The settings dialog, opened from the same base, saves a
            // terminal change only.
            let mut edited = base.clone();
            edited.gui = Some(GuiSettings {
                terminal: Some("wt".into()),
                ..Default::default()
            });
            save_gui_config_changes(&env, &base, &edited).unwrap();

            let saved = load_gui_config_from(Some(&path)).unwrap().gui_settings();
            assert_eq!(
                saved.theme,
                Some(ThemePreference::Dark),
                "concurrent theme kept"
            );
            assert_eq!(
                saved.restore_session,
                Some(false),
                "concurrent restore_session kept"
            );
            assert_eq!(saved.terminal.as_deref(), Some("wt"));
            let text = fs::read_to_string(&path).unwrap();
            if let Some(original) = original {
                assert!(text.starts_with(original), "{text}");
            }
        }

        // Dropping the [gui] table from an edit restores the defaults in
        // effect: only the keys that were not default change.
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let path = env.gui_config_file().unwrap();
        write(&path, "[gui]\ntheme = \"dark\"\nterminal = \"wt\"\n");
        let base = load_gui_config_from(Some(&path)).unwrap();
        let mut edited = base.clone();
        edited.gui = None;
        save_gui_config_changes(&env, &base, &edited).unwrap();
        let saved = load_gui_config_from(Some(&path)).unwrap().gui_settings();
        assert_eq!(saved, GuiSettings::default());
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("restore_session")
        );
    }

    /// C-033: Reset (a `[gui]` field set back to `None`) removes the key,
    /// also for `theme` and `restore_session`, keeping the comments and the
    /// other keys; an explicit default stays distinguishable from "not set".
    #[test]
    fn reset_removes_theme_and_restore_session_keys() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let path = env.gui_config_file().unwrap();
        let original = "# GUI settings
[gui]
theme = \"system\" # explicit default
restore_session = true
terminal = \"wt\"
";
        write(&path, original);
        let base = load_gui_config_from(Some(&path)).unwrap();
        let settings = base.gui_settings();
        assert_eq!(
            settings.theme,
            Some(ThemePreference::System),
            "set in gui.toml"
        );
        assert_eq!(settings.restore_session, Some(true), "set in gui.toml");
        assert_eq!(settings.theme(), GuiSettings::DEFAULT_THEME);
        assert_eq!(
            settings.restore_session(),
            GuiSettings::DEFAULT_RESTORE_SESSION
        );

        let mut edited = base.clone();
        let gui = edited.gui.as_mut().unwrap();
        gui.theme = None;
        gui.restore_session = None;
        save_gui_config_changes(&env, &base, &edited).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "# GUI settings
[gui]
terminal = \"wt\"
"
        );
        let saved = load_gui_config_from(Some(&path)).unwrap().gui_settings();
        assert_eq!(saved.theme, None);
        assert_eq!(saved.restore_session, None);
        assert_eq!(saved.terminal.as_deref(), Some("wt"));

        // Setting the default explicitly writes the key again.
        let base = load_gui_config_from(Some(&path)).unwrap();
        let mut edited = base.clone();
        edited.gui.as_mut().unwrap().restore_session = Some(true);
        save_gui_config_changes(&env, &base, &edited).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("restore_session = true"),
        );
        assert_eq!(
            load_gui_config_from(Some(&path))
                .unwrap()
                .gui_settings()
                .restore_session,
            Some(true)
        );
    }

    #[test]
    fn unset_gui_fields_are_not_serialized() {
        assert_eq!(toml::to_string(&GuiSettings::default()).unwrap(), "");
        let file = GuiConfigFile {
            gui: Some(GuiSettings {
                restore_session: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            toml::to_string(&file).unwrap(),
            "[gui]
restore_session = false
"
        );
        let parsed: GuiConfigFile = toml::from_str(
            "[gui]
theme = \"light\"
",
        )
        .unwrap();
        assert_eq!(
            parsed.gui_settings(),
            GuiSettings {
                theme: Some(ThemePreference::Light),
                ..Default::default()
            }
        );
    }

    #[test]
    fn save_changes_refuses_an_unparseable_file_and_leaves_it_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let path = env.gui_config_file().unwrap();
        for broken in [
            "[gui]\ntheme = \"dark\"\nunknown_key = 1\n",
            "[gui\ntheme = ",
        ] {
            write(&path, broken);
            let error = save_gui_config_changes(
                &env,
                &GuiConfigFile::default(),
                &themed(ThemePreference::Light),
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("gui.toml has errors; fix or remove it before saving: "),
                "{error}"
            );
            assert_eq!(fs::read(&path).unwrap(), broken.as_bytes());
        }
    }

    #[test]
    fn save_changes_creates_a_missing_file_and_refuses_the_cli_config() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let path = env.gui_config_file().unwrap();
        assert!(!path.exists());
        save_gui_config_changes(
            &env,
            &GuiConfigFile::default(),
            &themed(ThemePreference::Dark),
        )
        .unwrap();
        assert_eq!(
            load_gui_config_from(Some(&path))
                .unwrap()
                .gui
                .unwrap()
                .theme,
            Some(ThemePreference::Dark)
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[gui]\ntheme = \"dark\"\n",
            "only the changed key"
        );

        // X-005: never config.toml, under any alias.
        let cli_path = env.config_file().unwrap();
        let mut aliased = HistoryEnv::sandbox(root.path());
        aliased.gui_config_file = Some(cli_path.with_file_name("CONFIG.TOML"));
        assert!(
            save_gui_config_changes(
                &aliased,
                &GuiConfigFile::default(),
                &themed(ThemePreference::Light)
            )
            .is_err()
        );
        let mut same = HistoryEnv::sandbox(root.path());
        same.gui_config_file = Some(cli_path.clone());
        assert!(
            save_gui_config_changes(
                &same,
                &GuiConfigFile::default(),
                &themed(ThemePreference::Light)
            )
            .is_err()
        );
        assert!(!cli_path.exists());
    }
}
