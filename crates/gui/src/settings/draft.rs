//! `SettingsDraft` (DESIGN §11.18): the edited gui.toml values, their sources,
//! the refresh of unedited fields from `AppState`. WP6. Pure logic (no gpui):
//! the dialog and the tests drive it directly.
//!
//! The draft holds two copies of `gui.toml`'s content: `base` (what the
//! dialog started from, refreshed when another writer such as the View menu
//! changes the file) and `edited`. Saving passes both to core's change-set
//! save (K12), which writes only the leaf settings that differ, so unedited
//! keys, comments and concurrent changes of the file survive.

use std::path::PathBuf;

use claude_history_core::config::{
    AgentOverrides, AgentScopeConfig, ConfigFile, DisplayConfig, GuiConfigFile, GuiSettings,
    KeyBindings, KeyChord, KeysConfig, LayeredConfig, ResumeConfig, SearchConfig, TuiOverrides,
    parse_key_binding,
};
use claude_history_core::search::mode::SearchMode;
use claude_history_core::theme::ThemePreference;

use crate::model::{KeyAction, SettingsPage};
use crate::persist::{RememberedSearchMode, RememberedToolDisplay, UiState};

/// One editable setting. Ordered like [`Field::ALL`] (page by page).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Theme,
    Terminal,
    ClaudePath,
    PiPath,
    OmpPath,
    DefaultWorkspace,
    RestoreSession,
    PreviewLast,
    ToolDisplay,
    ShowThinking,
    Plain,
    SearchMode,
    ExcludeProjects,
    Key(KeyAction),
    ResumeArgs,
    AgentScope,
    AgentMode,
    AgentOutputChars,
    AgentTop,
    AgentWithinTop,
    AgentHits,
    AgentExclude,
    AgentTools,
    AgentToolResults,
    AgentThinking,
    AgentSubagents,
}

impl PartialOrd for Field {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Field {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.index().cmp(&other.index())
    }
}

impl Field {
    /// The position in [`Field::ALL`].
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(usize::MAX)
    }

    /// Every field, page by page, in display order.
    pub const ALL: [Field; 30] = [
        Field::Theme,
        Field::Terminal,
        Field::ClaudePath,
        Field::PiPath,
        Field::OmpPath,
        Field::DefaultWorkspace,
        Field::RestoreSession,
        Field::PreviewLast,
        Field::ToolDisplay,
        Field::ShowThinking,
        Field::Plain,
        Field::SearchMode,
        Field::ExcludeProjects,
        Field::Key(KeyAction::Resume),
        Field::Key(KeyAction::Fork),
        Field::Key(KeyAction::Rename),
        Field::Key(KeyAction::Delete),
        Field::Key(KeyAction::Refresh),
        Field::ResumeArgs,
        Field::AgentScope,
        Field::AgentMode,
        Field::AgentOutputChars,
        Field::AgentTop,
        Field::AgentWithinTop,
        Field::AgentHits,
        Field::AgentExclude,
        Field::AgentTools,
        Field::AgentToolResults,
        Field::AgentThinking,
        Field::AgentSubagents,
    ];

    /// The page the field is on.
    pub fn page(self) -> SettingsPage {
        match self {
            Field::Theme
            | Field::Terminal
            | Field::ClaudePath
            | Field::PiPath
            | Field::OmpPath
            | Field::DefaultWorkspace
            | Field::RestoreSession => SettingsPage::General,
            Field::PreviewLast | Field::ToolDisplay | Field::ShowThinking | Field::Plain => {
                SettingsPage::Display
            }
            Field::SearchMode | Field::ExcludeProjects => SettingsPage::Search,
            Field::Key(_) => SettingsPage::Keys,
            Field::ResumeArgs => SettingsPage::Resume,
            _ => SettingsPage::Agent,
        }
    }

    /// The row label.
    pub fn label(self) -> &'static str {
        match self {
            Field::Theme => "Theme",
            Field::Terminal => "Terminal",
            Field::ClaudePath => "claude program",
            Field::PiPath => "pi program",
            Field::OmpPath => "omp program",
            Field::DefaultWorkspace => "Default workspace",
            Field::RestoreSession => "Restore last session",
            Field::PreviewLast => "Preview shows",
            Field::ToolDisplay => "Default tool display",
            Field::ShowThinking => "Show thinking and subagents by default",
            Field::Plain => "Ctrl+O prints plain text",
            Field::SearchMode => "Default search mode",
            Field::ExcludeProjects => "Excluded projects",
            Field::Key(action) => action.label(),
            Field::ResumeArgs => "Default arguments",
            Field::AgentScope => "Scope",
            Field::AgentMode => "Mode",
            Field::AgentOutputChars => "Output budget (chars)",
            Field::AgentTop => "Top",
            Field::AgentWithinTop => "Within top",
            Field::AgentHits => "Hits per conversation",
            Field::AgentExclude => "Excluded projects (encoded dir names)",
            Field::AgentTools => "Include tools",
            Field::AgentToolResults => "Include tool results",
            Field::AgentThinking => "Include thinking",
            Field::AgentSubagents => "Include subagents",
        }
    }

    /// The gui.toml key (`[section] key`).
    pub fn key_path(self) -> &'static str {
        match self {
            Field::Theme => "[gui] theme",
            Field::Terminal => "[gui] terminal",
            Field::ClaudePath => "[gui] claude_path",
            Field::PiPath => "[gui] pi_path",
            Field::OmpPath => "[gui] omp_path",
            Field::DefaultWorkspace => "[gui] default_workspace",
            Field::RestoreSession => "[gui] restore_session",
            Field::PreviewLast => "[display] last",
            Field::ToolDisplay => "[display] no_tools",
            Field::ShowThinking => "[display] show_thinking",
            Field::Plain => "[display] plain",
            Field::SearchMode => "[search] mode",
            Field::ExcludeProjects => "[tui] exclude_projects",
            Field::Key(KeyAction::Resume) => "[keys] resume",
            Field::Key(KeyAction::Fork) => "[keys] fork",
            Field::Key(KeyAction::Rename) => "[keys] rename",
            Field::Key(KeyAction::Delete) => "[keys] delete",
            Field::Key(KeyAction::Refresh) => "[keys] refresh",
            Field::ResumeArgs => "[resume] default_args",
            Field::AgentScope => "[agent] scope",
            Field::AgentMode => "[agent] mode",
            Field::AgentOutputChars => "[agent] output_chars",
            Field::AgentTop => "[agent] top",
            Field::AgentWithinTop => "[agent] within_top",
            Field::AgentHits => "[agent] hits_per_conversation",
            Field::AgentExclude => "[agent] exclude_projects",
            Field::AgentTools => "[agent] tools",
            Field::AgentToolResults => "[agent] tool_results",
            Field::AgentThinking => "[agent] thinking",
            Field::AgentSubagents => "[agent] subagents",
        }
    }

    /// The QA element id suffix: the gui.toml key without spaces
    /// (`keys.resume`, `gui.terminal`).
    pub fn qa_name(self) -> String {
        let path = self.key_path();
        let section = path
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or_default();
        let key = path.rsplit(' ').next().unwrap_or_default();
        format!("{section}.{key}")
    }

    /// When a saved change takes effect (§13.2 "Applies").
    pub fn applies(self) -> &'static str {
        match self {
            Field::Theme | Field::ExcludeProjects | Field::Key(_) => "Applies immediately",
            Field::Terminal
            | Field::ClaudePath
            | Field::PiPath
            | Field::OmpPath
            | Field::ResumeArgs => "Applies to the next resume",
            Field::DefaultWorkspace | Field::RestoreSession => "Applies at the next start",
            Field::PreviewLast => "Applies when the list refreshes",
            Field::ToolDisplay | Field::ShowThinking => {
                "Applies to conversations opened later and at the next start"
            }
            Field::Plain => "Applies to the next Ctrl+O",
            Field::SearchMode => "Applies at the next start; Deep search at its next run",
            _ => "Applies to the next agent run",
        }
    }

    /// Fields stored in gui.toml's `[gui]` table. Since C-033 they are
    /// optional like every other field: Reset removes the key.
    pub fn is_gui_table_field(self) -> bool {
        matches!(self, Field::Theme | Field::RestoreSession)
    }
}

/// The value of a [`Field`]: [`Value::Unset`] = the key is absent.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Unset,
    Bool(bool),
    Text(String),
    List(Vec<String>),
    Number(usize),
    Theme(ThemePreference),
    Mode(SearchMode),
    Scope(AgentScopeConfig),
    Key(KeyChord),
}

impl Value {
    pub fn is_unset(&self) -> bool {
        matches!(self, Value::Unset)
    }
}

fn some_text(path: &Option<PathBuf>) -> Value {
    path.as_ref()
        .map_or(Value::Unset, |path| Value::Text(path.display().to_string()))
}

fn opt<T>(value: Option<T>, make: impl FnOnce(T) -> Value) -> Value {
    value.map_or(Value::Unset, make)
}

fn text_or_unset(text: Option<&String>) -> Value {
    text.map_or(Value::Unset, |text| Value::Text(text.clone()))
}

/// The value of `field` in a gui.toml model.
pub fn get(field: Field, gui: &GuiConfigFile) -> Value {
    let settings = gui.gui_settings();
    let display = gui.display.clone().unwrap_or_default();
    let keys = gui.keys.clone().unwrap_or_default();
    let agent = gui.agent.clone().unwrap_or_default();
    match field {
        Field::Theme => opt(settings.theme, Value::Theme),
        Field::Terminal => text_or_unset(settings.terminal.as_ref()),
        Field::ClaudePath => some_text(&settings.claude_path),
        Field::PiPath => some_text(&settings.pi_path),
        Field::OmpPath => some_text(&settings.omp_path),
        Field::DefaultWorkspace => some_text(&settings.default_workspace),
        Field::RestoreSession => opt(settings.restore_session, Value::Bool),
        Field::PreviewLast => opt(display.last, Value::Bool),
        Field::ToolDisplay => opt(display.no_tools, Value::Bool),
        Field::ShowThinking => opt(display.show_thinking, Value::Bool),
        Field::Plain => opt(display.plain, Value::Bool),
        Field::SearchMode => opt(gui.search.as_ref().and_then(|s| s.mode), Value::Mode),
        Field::ExcludeProjects => opt(
            gui.tui
                .as_ref()
                .and_then(|tui| tui.exclude_projects.clone()),
            Value::List,
        ),
        Field::Key(KeyAction::Resume) => opt(keys.resume, Value::Key),
        Field::Key(KeyAction::Fork) => opt(keys.fork, Value::Key),
        Field::Key(KeyAction::Rename) => opt(keys.rename, Value::Key),
        Field::Key(KeyAction::Delete) => opt(keys.delete, Value::Key),
        Field::Key(KeyAction::Refresh) => opt(keys.refresh, Value::Key),
        Field::ResumeArgs => opt(
            gui.resume.as_ref().and_then(|r| r.default_args.clone()),
            Value::List,
        ),
        Field::AgentScope => opt(agent.scope, Value::Scope),
        Field::AgentMode => opt(agent.mode, Value::Mode),
        Field::AgentOutputChars => opt(agent.output_chars, Value::Number),
        Field::AgentTop => opt(agent.top, Value::Number),
        Field::AgentWithinTop => opt(agent.within_top, Value::Number),
        Field::AgentHits => opt(agent.hits_per_conversation, Value::Number),
        Field::AgentExclude => opt(agent.exclude_projects, Value::List),
        Field::AgentTools => opt(agent.tools, Value::Bool),
        Field::AgentToolResults => opt(agent.tool_results, Value::Bool),
        Field::AgentThinking => opt(agent.thinking, Value::Bool),
        Field::AgentSubagents => opt(agent.subagents, Value::Bool),
    }
}

/// Set `field` in a gui.toml model ([`Value::Unset`] removes the key, also
/// `[gui] theme` and `restore_session`, C-033). A value of the wrong kind for
/// the field is ignored.
pub fn set(field: Field, gui: &mut GuiConfigFile, value: Value) {
    fn display(gui: &mut GuiConfigFile) -> &mut DisplayConfig {
        gui.display.get_or_insert_with(DisplayConfig::default)
    }
    fn keys(gui: &mut GuiConfigFile) -> &mut KeysConfig {
        gui.keys.get_or_insert_with(KeysConfig::default)
    }
    fn agent(gui: &mut GuiConfigFile) -> &mut AgentOverrides {
        gui.agent.get_or_insert_with(AgentOverrides::default)
    }
    fn gui_table(gui: &mut GuiConfigFile) -> &mut GuiSettings {
        gui.gui.get_or_insert_with(GuiSettings::default)
    }
    fn text(value: &Value) -> Option<String> {
        match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        }
    }
    fn path(value: &Value) -> Option<PathBuf> {
        text(value).map(PathBuf::from)
    }
    fn flag(value: &Value) -> Option<bool> {
        match value {
            Value::Bool(flag) => Some(*flag),
            _ => None,
        }
    }
    fn number(value: &Value) -> Option<usize> {
        match value {
            Value::Number(number) => Some(*number),
            _ => None,
        }
    }
    fn list(value: &Value) -> Option<Vec<String>> {
        match value {
            Value::List(list) => Some(list.clone()),
            _ => None,
        }
    }
    fn mode(value: &Value) -> Option<SearchMode> {
        match value {
            Value::Mode(mode) => Some(*mode),
            _ => None,
        }
    }
    fn key(value: &Value) -> Option<KeyChord> {
        match value {
            Value::Key(chord) => Some(*chord),
            _ => None,
        }
    }
    match field {
        Field::Theme => {
            gui_table(gui).theme = match value {
                Value::Theme(theme) => Some(theme),
                _ => None,
            }
        }
        Field::RestoreSession => gui_table(gui).restore_session = flag(&value),
        Field::Terminal => gui_table(gui).terminal = text(&value),
        Field::ClaudePath => gui_table(gui).claude_path = path(&value),
        Field::PiPath => gui_table(gui).pi_path = path(&value),
        Field::OmpPath => gui_table(gui).omp_path = path(&value),
        Field::DefaultWorkspace => gui_table(gui).default_workspace = path(&value),
        Field::PreviewLast => display(gui).last = flag(&value),
        Field::ToolDisplay => display(gui).no_tools = flag(&value),
        Field::ShowThinking => display(gui).show_thinking = flag(&value),
        Field::Plain => display(gui).plain = flag(&value),
        Field::SearchMode => {
            gui.search.get_or_insert_with(SearchConfig::default).mode = mode(&value)
        }
        Field::ExcludeProjects => {
            gui.tui
                .get_or_insert_with(TuiOverrides::default)
                .exclude_projects = list(&value)
        }
        Field::Key(KeyAction::Resume) => keys(gui).resume = key(&value),
        Field::Key(KeyAction::Fork) => keys(gui).fork = key(&value),
        Field::Key(KeyAction::Rename) => keys(gui).rename = key(&value),
        Field::Key(KeyAction::Delete) => keys(gui).delete = key(&value),
        Field::Key(KeyAction::Refresh) => keys(gui).refresh = key(&value),
        Field::ResumeArgs => {
            gui.resume
                .get_or_insert_with(ResumeConfig::default)
                .default_args = list(&value)
        }
        Field::AgentScope => {
            agent(gui).scope = match value {
                Value::Scope(scope) => Some(scope),
                _ => None,
            }
        }
        Field::AgentMode => agent(gui).mode = mode(&value),
        Field::AgentOutputChars => agent(gui).output_chars = number(&value),
        Field::AgentTop => agent(gui).top = number(&value),
        Field::AgentWithinTop => agent(gui).within_top = number(&value),
        Field::AgentHits => agent(gui).hits_per_conversation = number(&value),
        Field::AgentExclude => agent(gui).exclude_projects = list(&value),
        Field::AgentTools => agent(gui).tools = flag(&value),
        Field::AgentToolResults => agent(gui).tool_results = flag(&value),
        Field::AgentThinking => agent(gui).thinking = flag(&value),
        Field::AgentSubagents => agent(gui).subagents = flag(&value),
    }
}

/// The value of `field` in `config.toml` (read-only; `Unset` when absent or
/// when the key has no counterpart there).
pub fn cli_value(field: Field, cli: &ConfigFile) -> Value {
    let display = cli.display.clone().unwrap_or_default();
    let keys = cli.keys.clone().unwrap_or_default();
    let agent = cli.agent.clone().unwrap_or_default();
    let non_empty = |list: Vec<String>| {
        if list.is_empty() {
            Value::Unset
        } else {
            Value::List(list)
        }
    };
    match field {
        Field::PreviewLast => opt(display.last, Value::Bool),
        Field::ToolDisplay => opt(display.no_tools, Value::Bool),
        Field::ShowThinking => opt(display.show_thinking, Value::Bool),
        Field::Plain => opt(display.plain, Value::Bool),
        Field::SearchMode => opt(cli.search.as_ref().and_then(|s| s.mode), Value::Mode),
        Field::ExcludeProjects => non_empty(
            cli.tui
                .as_ref()
                .map(|tui| tui.exclude_projects.clone())
                .unwrap_or_default(),
        ),
        Field::Key(KeyAction::Resume) => opt(keys.resume, Value::Key),
        Field::Key(KeyAction::Fork) => opt(keys.fork, Value::Key),
        Field::Key(KeyAction::Rename) => opt(keys.rename, Value::Key),
        Field::Key(KeyAction::Delete) => opt(keys.delete, Value::Key),
        Field::Key(KeyAction::Refresh) => opt(keys.refresh, Value::Key),
        Field::ResumeArgs => opt(
            cli.resume.as_ref().and_then(|r| r.default_args.clone()),
            Value::List,
        ),
        Field::AgentScope => opt(agent.scope, Value::Scope),
        Field::AgentMode => opt(agent.mode, Value::Mode),
        Field::AgentOutputChars => opt(agent.output_chars, Value::Number),
        Field::AgentTop => opt(agent.top, Value::Number),
        Field::AgentWithinTop => opt(agent.within_top, Value::Number),
        Field::AgentHits => opt(agent.hits_per_conversation, Value::Number),
        Field::AgentExclude => non_empty(agent.exclude_projects),
        Field::AgentTools => opt(agent.tools, Value::Bool),
        Field::AgentToolResults => opt(agent.tool_results, Value::Bool),
        Field::AgentThinking => opt(agent.thinking, Value::Bool),
        Field::AgentSubagents => opt(agent.subagents, Value::Bool),
        _ => Value::Unset,
    }
}

/// The built-in default of `field` as a value (what an unset field means).
pub fn default_value(field: Field) -> Value {
    let keys = KeyBindings::default();
    match field {
        Field::Theme => Value::Theme(ThemePreference::System),
        Field::RestoreSession => Value::Bool(true),
        Field::PreviewLast => Value::Bool(true),
        Field::ToolDisplay => Value::Bool(true),
        Field::ShowThinking | Field::Plain => Value::Bool(false),
        Field::SearchMode => Value::Mode(SearchMode::Lexical),
        Field::Key(KeyAction::Resume) => Value::Key(keys.resume),
        Field::Key(KeyAction::Fork) => Value::Key(keys.fork),
        Field::Key(KeyAction::Rename) => Value::Key(keys.rename),
        Field::Key(KeyAction::Delete) => Value::Key(keys.delete),
        Field::Key(KeyAction::Refresh) => Value::Key(keys.refresh),
        Field::AgentScope => Value::Scope(AgentScopeConfig::Global),
        Field::AgentOutputChars => Value::Number(6000),
        Field::AgentTop => Value::Number(10),
        Field::AgentWithinTop => Value::Number(20),
        Field::AgentHits => Value::Number(2),
        Field::AgentTools
        | Field::AgentToolResults
        | Field::AgentThinking
        | Field::AgentSubagents => Value::Bool(false),
        Field::Terminal => Value::Text("auto".to_string()),
        _ => Value::Unset,
    }
}

/// A value as shown in a source line (`Full`, `ctrl+r`, `exact`, `on`).
pub fn describe(field: Field, value: &Value) -> String {
    match value {
        Value::Unset => match field {
            Field::ClaudePath | Field::PiPath | Field::OmpPath => "resolved from PATH".into(),
            Field::AgentMode => "from [search] mode, else automatic".into(),
            _ => "not set".into(),
        },
        Value::Bool(flag) => match field {
            Field::PreviewLast => if *flag { "Last" } else { "First" }.into(),
            // `no_tools = true` shows the summary.
            Field::ToolDisplay => if *flag { "Summary" } else { "Full" }.into(),
            _ => if *flag { "on" } else { "off" }.into(),
        },
        Value::Text(text) => text.clone(),
        Value::List(list) => {
            if list.is_empty() {
                "none".into()
            } else {
                list.join(", ")
            }
        }
        Value::Number(number) => number.to_string(),
        Value::Theme(theme) => match theme {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        }
        .into(),
        Value::Mode(mode) => mode.as_str().into(),
        Value::Scope(scope) => match scope {
            AgentScopeConfig::Global => "global",
            AgentScopeConfig::Local => "local",
        }
        .into(),
        Value::Key(chord) => chord.config_string(),
    }
}

/// Where the value in effect comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Gui,
    Config,
    Remembered,
    Default,
}

/// The effective value of a field and its source line (`From gui.toml`,
/// `From config.toml: <v>`, `Remembered from last session: <v>`,
/// `Default: <v>`; §13.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub kind: SourceKind,
    pub value: Value,
    pub text: String,
}

fn remembered(field: Field, ui: &UiState) -> Value {
    match field {
        Field::ToolDisplay => match ui.view.tool_display {
            // The Settings control is Summary/Full; Truncated reads as Summary.
            Some(RememberedToolDisplay::Full) => Value::Bool(false),
            Some(_) => Value::Bool(true),
            None => Value::Unset,
        },
        Field::ShowThinking => opt(ui.view.show_thinking, Value::Bool),
        Field::SearchMode => match ui.search_mode {
            Some(RememberedSearchMode::Lexical) => Value::Mode(SearchMode::Lexical),
            Some(RememberedSearchMode::Semantic) => Value::Mode(SearchMode::Semantic),
            None => Value::Unset,
        },
        _ => Value::Unset,
    }
}

/// The deprecated `[tui] semantic_search` key as the source of the search mode
/// (core's `resolve_search_mode` uses it when `[search] mode` is unset; it
/// beats the remembered value like any config key, PD17/C-040).
fn legacy_search_mode_source(config: &LayeredConfig) -> Option<Source> {
    let gui = config
        .gui_file
        .tui
        .as_ref()
        .and_then(|tui| tui.semantic_search);
    let cli = config.cli.tui.as_ref().and_then(|tui| tui.semantic_search);
    let (flag, kind, origin) = match (gui, cli) {
        (Some(flag), _) => (flag, SourceKind::Gui, "gui.toml"),
        (None, Some(flag)) => (flag, SourceKind::Config, "config.toml"),
        (None, None) => return None,
    };
    let mode = if flag {
        SearchMode::Semantic
    } else {
        SearchMode::Lexical
    };
    let value = Value::Mode(mode);
    Some(Source {
        kind,
        text: format!(
            "From {origin}: {} ([tui] semantic_search)",
            describe(Field::SearchMode, &value)
        ),
        value,
    })
}

/// The source of `field`'s effective value in the saved configuration.
pub fn source_of(field: Field, config: &LayeredConfig, ui: &UiState) -> Source {
    if field == Field::SearchMode
        && get(field, &config.gui_file).is_unset()
        && cli_value(field, &config.cli).is_unset()
        && let Some(source) = legacy_search_mode_source(config)
    {
        return source;
    }
    let from_gui = get(field, &config.gui_file);
    if !from_gui.is_unset() {
        return Source {
            kind: SourceKind::Gui,
            value: from_gui,
            text: "From gui.toml".to_string(),
        };
    }
    let from_cli = cli_value(field, &config.cli);
    if !from_cli.is_unset() {
        let text = format!("From config.toml: {}", describe(field, &from_cli));
        return Source {
            kind: SourceKind::Config,
            value: from_cli,
            text,
        };
    }
    if config.gui.restore_session() {
        let value = remembered(field, ui);
        if !value.is_unset() {
            let text = format!("Remembered from last session: {}", describe(field, &value));
            return Source {
                kind: SourceKind::Remembered,
                value,
                text,
            };
        }
    }
    let value = default_value(field);
    let text = format!("Default: {}", describe(field, &value));
    Source {
        kind: SourceKind::Default,
        value,
        text,
    }
}

/// The value a control shows: the draft's own value, else the effective one.
pub fn shown_value(field: Field, draft: &GuiConfigFile, source: &Source) -> Value {
    let own = get(field, draft);
    if own.is_unset() {
        source.value.clone()
    } else {
        own
    }
}

/// A read-only note about a deprecated `config.toml` key (C-024).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeprecatedNote {
    pub key: &'static str,
    pub value: String,
    pub note: &'static str,
}

/// The deprecated keys present in `config.toml` (`global`,
/// `display.relative_time`, `tui.semantic_search`), shown read-only.
pub fn deprecated_notes(cli: &ConfigFile) -> Vec<DeprecatedNote> {
    let mut notes = Vec::new();
    if let Some(global) = cli.global {
        notes.push(DeprecatedNote {
            key: "global",
            value: global.to_string(),
            note: "deprecated, ignored",
        });
    }
    if let Some(relative) = cli.display.as_ref().and_then(|d| d.relative_time) {
        notes.push(DeprecatedNote {
            key: "[display] relative_time",
            value: relative.to_string(),
            note: "deprecated, ignored",
        });
    }
    if let Some(semantic) = cli.tui.as_ref().and_then(|t| t.semantic_search) {
        notes.push(DeprecatedNote {
            key: "[tui] semantic_search",
            value: semantic.to_string(),
            note: "used when [search] mode is unset",
        });
    }
    notes
}

/// The key bindings in effect for a draft: the draft's keys, else
/// `config.toml`'s, else the defaults.
pub fn effective_keys(cli: &ConfigFile, draft: &GuiConfigFile) -> KeyBindings {
    let cli_keys = cli.keys.clone().unwrap_or_default();
    let gui_keys = draft.keys.clone().unwrap_or_default();
    KeyBindings::from_config(Some(KeysConfig {
        resume: gui_keys.resume.or(cli_keys.resume),
        fork: gui_keys.fork.or(cli_keys.fork),
        rename: gui_keys.rename.or(cli_keys.rename),
        delete: gui_keys.delete.or(cli_keys.delete),
        refresh: gui_keys.refresh.or(cli_keys.refresh),
    }))
}

/// Parse the text of a key field (`ctrl+r`, `f2`, `alt+r`): core's parser,
/// so the messages are `Unknown modifier: hyper`, `Function keys do not
/// support modifiers: ctrl+f2`, `Invalid key binding: …`, `Unknown key: …`.
pub fn parse_key_text(text: &str) -> Result<KeyChord, String> {
    parse_key_binding(text.trim())
}

/// Parse the text of a number field (a whole number of 1 or more).
pub fn parse_number_text(text: &str) -> Result<usize, String> {
    match text.trim().parse::<usize>() {
        Ok(number) if number >= 1 => Ok(number),
        _ => Err("Enter a whole number of 1 or more".to_string()),
    }
}

/// The items of a list field: one per line, trimmed, empty lines dropped.
pub fn lines_to_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// A list as the lines of its editor.
pub fn list_to_lines(list: &[String]) -> String {
    list.join("\n")
}

/// Check a path field: it must exist when set (`must_be_dir`: a folder, else
/// a file). Empty text is "not set" and always fine.
pub fn check_path_text(text: &str, must_be_dir: bool) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    let path = std::path::Path::new(text);
    if must_be_dir {
        if !path.is_absolute() {
            return Err(format!("{text} is not an absolute path"));
        }
        if !path.is_dir() {
            return Err(format!("Folder not found: {text}"));
        }
    } else if !path.is_file() {
        return Err(format!("File not found: {text}"));
    }
    Ok(())
}

/// The edits of an open Settings dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsDraft {
    /// gui.toml as the dialog started from it (or as of the last refresh).
    pub base: GuiConfigFile,
    pub edited: GuiConfigFile,
}

impl SettingsDraft {
    pub fn new(gui_file: &GuiConfigFile) -> Self {
        Self {
            base: gui_file.clone(),
            edited: gui_file.clone(),
        }
    }

    /// The draft's own value (what gui.toml will hold after Save).
    pub fn value(&self, field: Field) -> Value {
        get(field, &self.edited)
    }

    /// Whether the user changed `field` since the draft's base.
    pub fn is_edited(&self, field: Field) -> bool {
        get(field, &self.base) != get(field, &self.edited)
    }

    /// Whether any field differs from the base (Save has something to write).
    pub fn has_changes(&self) -> bool {
        Field::ALL.iter().any(|field| self.is_edited(*field))
    }

    pub fn set(&mut self, field: Field, value: Value) {
        set(field, &mut self.edited, value);
    }

    /// Reset: remove the gui.toml key (the field falls back to config.toml,
    /// the remembered value or the default).
    pub fn reset(&mut self, field: Field) {
        set(field, &mut self.edited, Value::Unset);
    }

    /// `AppState` changed (the View menu saved a theme, another Settings
    /// save): every field the user has not edited follows the new gui.toml;
    /// edited fields keep the user's value. The base becomes the new content.
    /// Returns the fields whose draft value changed (their controls refresh).
    pub fn refresh_from(&mut self, new_base: &GuiConfigFile) -> Vec<Field> {
        let mut changed = Vec::new();
        let mut merged = new_base.clone();
        for field in Field::ALL {
            let edited = self.is_edited(field);
            if edited {
                set(field, &mut merged, get(field, &self.edited));
            } else if get(field, &self.edited) != get(field, new_base) {
                changed.push(field);
            }
        }
        self.base = new_base.clone();
        self.edited = merged;
        changed
    }
}
