//! Shared value types of the GUI (DESIGN §7.8, §7.9). Frozen: written by WP0,
//! afterwards changed only by the integrator.
//!
//! Core types are used directly (`ToolDisplayMode`, `ViewPrefs`,
//! `ExportFormat`, `ExportOptions`, `PickerAction`, `ListSearchMode`,
//! `TerminalCommand`, `LaunchSpec`, `UpdateStatus`); this module never mirrors
//! them.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use claude_history_core::agent::AgentConversationInfo;
use claude_history_core::display_format::sanitize_preview;
use claude_history_core::export::ExportFormat;
use claude_history_core::history::{Conversation, Source};
use claude_history_core::settings::{GuiLaunchArgs, PickerAction};
use claude_history_core::time_filter::TimeFilter;
use gpui_kit::SharedString;
use serde::{Deserialize, Serialize};

/// Longest conversation title the GUI derives (characters, `…` included).
pub const TITLE_MAX_CHARS: usize = 80;

/// The conversation an action, dialog, viewer or result set acts on,
/// captured when it starts (PD15, ADD A1/A2). Code never re-reads the live
/// list selection to find the object an action acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationTarget {
    pub path: PathBuf,
    /// `None` = unknown: Resume/Fork/Rename/Delete are disabled
    /// ("Unknown conversation type").
    pub source: Option<Source>,
    /// `Conversation.session_id` (Pi/OMP: header id); the file stem when unknown.
    pub session_id: String,
    /// `path.file_stem()` (lossy): the TUI's "session ID".
    pub file_stem: String,
    /// custom title → summary → sanitized preview → file stem, at most
    /// [`TITLE_MAX_CHARS`] characters.
    pub title: SharedString,
    pub project_name: Option<SharedString>,
    pub project_path: Option<PathBuf>,
    pub custom_title: Option<String>,
}

impl ConversationTarget {
    /// A target for a loaded conversation (`source = Some(c.source)`).
    pub fn from_conversation(c: &Conversation) -> Self {
        let file_stem = file_stem_lossy(&c.path);
        let session_id = if c.session_id.is_empty() {
            file_stem.clone()
        } else {
            c.session_id.clone()
        };
        Self {
            path: c.path.clone(),
            source: Some(c.source),
            session_id,
            title: conversation_title(c, &file_stem),
            file_stem,
            project_name: c.project_name.clone().map(SharedString::from),
            project_path: c.project_path.clone(),
            custom_title: non_empty(c.custom_title.as_deref()).map(str::to_string),
        }
    }

    /// A target for a Deep Search hit or agent result: source and session id
    /// come from core, never re-detected (ADD A2). `title` is the display
    /// title the result knows, if any.
    pub fn from_agent_info(info: &AgentConversationInfo, title: Option<&str>) -> Self {
        let file_stem = file_stem_lossy(&info.path);
        let session_id = if info.session_id.is_empty() {
            file_stem.clone()
        } else {
            info.session_id.clone()
        };
        let title = match non_empty(title) {
            Some(title) => truncate_chars(title, TITLE_MAX_CHARS),
            None => file_stem.clone(),
        };
        Self {
            path: info.path.clone(),
            source: Some(info.source),
            session_id,
            file_stem,
            title: title.into(),
            project_name: None,
            project_path: None,
            custom_title: None,
        }
    }

    /// Single-file windows: the source comes from the parsed conversation
    /// when parsing succeeded (in the background), else it stays unknown.
    pub fn from_parsed_file(path: &Path, parsed: Option<&Conversation>) -> Self {
        match parsed {
            Some(conversation) => {
                let mut target = Self::from_conversation(conversation);
                target.path = path.to_path_buf();
                target.file_stem = file_stem_lossy(path);
                target
            }
            None => {
                let file_stem = file_stem_lossy(path);
                Self {
                    path: path.to_path_buf(),
                    source: None,
                    session_id: file_stem.clone(),
                    title: file_stem.clone().into(),
                    file_stem,
                    project_name: None,
                    project_path: None,
                    custom_title: None,
                }
            }
        }
    }

    /// `proj · Fix login bug · 3f2a19c0-…` (project omitted when unknown;
    /// the session ID shortened by [`short_session_id`], omitted when the
    /// title is the session ID itself).
    pub fn describe(&self) -> String {
        let short_id = short_session_id(&self.session_id);
        let mut parts: Vec<&str> = Vec::with_capacity(3);
        if let Some(project) = &self.project_name {
            parts.push(project.as_ref());
        }
        parts.push(self.title.as_ref());
        if self.session_id != self.title.as_ref() {
            parts.push(&short_id);
        }
        parts.join(" · ")
    }

    /// Pi/OMP: the header id differs from the file stem ("Copy header session ID").
    pub fn header_id_differs(&self) -> bool {
        self.session_id != self.file_stem
    }

    /// Resume, Fork, Rename and Delete need a known source.
    pub fn can_modify(&self) -> bool {
        self.source.is_some()
    }
}

/// The viewer header's data (DESIGN §9.10), taken from the live corpus.
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderInfo {
    pub title: SharedString,
    pub project_name: Option<SharedString>,
    pub custom_title: Option<SharedString>,
    pub model: Option<String>,
    pub message_count: usize,
    pub duration_minutes: Option<u64>,
    pub total_tokens: u64,
    pub timestamp: DateTime<Local>,
    pub summary: Option<SharedString>,
    pub parse_error_count: usize,
    pub file_stem: SharedString,
}

impl HeaderInfo {
    pub fn from_conversation(c: &Conversation) -> Self {
        let file_stem = file_stem_lossy(&c.path);
        Self {
            title: conversation_title(c, &file_stem),
            project_name: c.project_name.clone().map(SharedString::from),
            custom_title: non_empty(c.custom_title.as_deref())
                .map(|t| SharedString::from(t.to_string())),
            model: c.model.clone(),
            message_count: c.message_count,
            duration_minutes: c.duration_minutes,
            total_tokens: c.total_tokens,
            timestamp: c.timestamp,
            summary: non_empty(c.summary.as_deref()).map(|s| SharedString::from(s.to_string())),
            parse_error_count: c.parse_errors.len(),
            file_stem: file_stem.into(),
        }
    }

    /// A conversation that is not in the corpus: the header shows the file
    /// stem only (V-006). Every other field is empty; `timestamp` is the Unix
    /// epoch and must not be shown ([`HeaderInfo::is_stem_only`]).
    pub fn stem_only(path: &Path) -> Self {
        let file_stem: SharedString = file_stem_lossy(path).into();
        Self {
            title: file_stem.clone(),
            project_name: None,
            custom_title: None,
            model: None,
            message_count: 0,
            duration_minutes: None,
            total_tokens: 0,
            timestamp: DateTime::<Local>::from(std::time::SystemTime::UNIX_EPOCH),
            summary: None,
            parse_error_count: 0,
            file_stem,
        }
    }

    /// Whether this header was built by [`HeaderInfo::stem_only`].
    pub fn is_stem_only(&self) -> bool {
        self.project_name.is_none()
            && self.message_count == 0
            && self.timestamp == DateTime::<Local>::from(std::time::SystemTime::UNIX_EPOCH)
    }
}

/// Open a conversation in a viewer (Workspace → `ViewerPane::open`).
#[derive(Clone, Debug, PartialEq)]
pub struct OpenRequest {
    pub target: ConversationTarget,
    /// Move keyboard focus into the viewer once the conversation is shown.
    pub focus: bool,
    pub reveal: Option<RevealRequest>,
    /// Also show the agent panel on this tab and focus it (row menu "Search
    /// within conversation…" / "Outline", Session → Outline / Search within /
    /// Read range from the list or Deep search, D-029). The panel then has
    /// the focus whatever `focus` says.
    pub agent_tab: Option<AgentTab>,
}

impl OpenRequest {
    /// Show `target` (keyboard focus moves into the viewer when `focus`).
    pub fn new(target: ConversationTarget, focus: bool) -> Self {
        Self {
            target,
            focus,
            reveal: None,
            agent_tab: None,
        }
    }

    pub fn with_reveal(mut self, reveal: RevealRequest) -> Self {
        self.reveal = Some(reveal);
        self
    }

    pub fn with_agent_tab(mut self, tab: Option<AgentTab>) -> Self {
        self.agent_tab = tab;
        self
    }
}

/// Reveal one message of a conversation (Deep Search hit, agent panel row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevealRequest {
    pub path: PathBuf,
    /// The full `ch_…` reference of the conversation, when known.
    pub full_ref: Option<String>,
    pub kind: RevealKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealKind {
    /// An agent message ordinal (`m12`) plus the visibility its result used.
    AgentOrdinal {
        ordinal: usize,
        hint: ContentVisibilityHint,
    },
    /// A rendered entry index of the viewer model.
    EntryIndex(usize),
}

/// Which hidden content the revealing result included (applied to that view only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ContentVisibilityHint {
    pub tools: bool,
    pub tool_results: bool,
    pub thinking: bool,
    pub subagents: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ViewerMode {
    /// The right pane of the main window.
    #[default]
    Main,
    /// A single-file viewer window.
    SingleFile,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTab {
    #[default]
    Outline,
    Within,
    Read,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftTab {
    #[default]
    Conversations,
    DeepSearch,
}

/// The region that last received focus in a window (`ActivePane`, §7.8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PaneFocus {
    #[default]
    List,
    ListToolbar,
    DeepSearch,
    Viewer,
    AgentPanel,
    Other,
}

/// Sections of the Keyboard shortcuts dialog (§8.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HelpSection {
    List,
    Viewer,
    AgentPanel,
    DeepSearch,
    Global,
    Dialogs,
}

impl HelpSection {
    pub const ALL: [HelpSection; 6] = [
        HelpSection::List,
        HelpSection::Viewer,
        HelpSection::AgentPanel,
        HelpSection::DeepSearch,
        HelpSection::Global,
        HelpSection::Dialogs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            HelpSection::List => "Conversation list",
            HelpSection::Viewer => "Viewer",
            HelpSection::AgentPanel => "Agent panel",
            HelpSection::DeepSearch => "Deep search",
            HelpSection::Global => "Global",
            HelpSection::Dialogs => "Dialogs",
        }
    }
}

/// Pages of the Settings dialog (§13.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SettingsPage {
    #[default]
    General,
    Display,
    Search,
    Keys,
    Resume,
    Agent,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 6] = [
        SettingsPage::General,
        SettingsPage::Display,
        SettingsPage::Search,
        SettingsPage::Keys,
        SettingsPage::Resume,
        SettingsPage::Agent,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Display => "Display",
            SettingsPage::Search => "Search",
            SettingsPage::Keys => "Keys",
            SettingsPage::Resume => "Resume",
            SettingsPage::Agent => "Agent",
        }
    }
}

/// The five user-configurable key actions (`[keys]`, §8.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyAction {
    Resume,
    Fork,
    Rename,
    Delete,
    Refresh,
}

impl KeyAction {
    /// Display order (Settings → Keys).
    pub const ALL: [KeyAction; 5] = [
        KeyAction::Resume,
        KeyAction::Fork,
        KeyAction::Rename,
        KeyAction::Delete,
        KeyAction::Refresh,
    ];

    /// The TUI's check order, highest priority first: delete > resume > fork
    /// > refresh > rename (C-028). With equal keys the earlier one wins.
    pub const PRIORITY: [KeyAction; 5] = [
        KeyAction::Delete,
        KeyAction::Resume,
        KeyAction::Fork,
        KeyAction::Refresh,
        KeyAction::Rename,
    ];

    pub fn label(self) -> &'static str {
        match self {
            KeyAction::Resume => "Resume",
            KeyAction::Fork => "Fork",
            KeyAction::Rename => "Rename",
            KeyAction::Delete => "Delete",
            KeyAction::Refresh => "Refresh",
        }
    }

    /// The `[keys]` table key in config files.
    pub fn config_key(self) -> &'static str {
        match self {
            KeyAction::Resume => "resume",
            KeyAction::Fork => "fork",
            KeyAction::Rename => "rename",
            KeyAction::Delete => "delete",
            KeyAction::Refresh => "refresh",
        }
    }

    /// Position in [`KeyAction::PRIORITY`] (0 = highest).
    pub fn priority(self) -> usize {
        Self::PRIORITY
            .iter()
            .position(|action| *action == self)
            .unwrap_or(Self::PRIORITY.len())
    }
}

/// Result of validating `[keys]` (Settings → Keys and startup, §8.5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyValidation {
    /// Rejected bindings (the previous value is kept).
    pub errors: Vec<(KeyAction, String)>,
    /// Accepted bindings with a caveat.
    pub warnings: Vec<String>,
}

impl KeyValidation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SemanticReportMode {
    Search { top: usize },
    DebugDump,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DebugLogTab {
    #[default]
    Messages,
    ParseErrors,
    File,
}

/// Operations that hold the semantic gate exclusively (§7.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SemanticOp {
    Generate,
    Clear,
}

/// A Tools dialog opened after startup by a one-shot CLI flag (AD14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupTool {
    ShowDir,
    DeleteSession(String),
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

/// Everything decided by the command line, mapped by `workspace::startup`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaunchContext {
    /// The parsed command line (core).
    pub args: GuiLaunchArgs,
    /// The process working directory, read once in `main`.
    pub launch_cwd: Option<PathBuf>,
    /// `--since/--after/--before`, resolved at startup (never persisted).
    pub time_filter: TimeFilter,
    pub picker: Option<PickerAction>,
    pub startup_tool: Option<StartupTool>,
    /// `[FILE]` (or `--render FILE` without redirection): single-file window.
    pub file: Option<PathBuf>,
    /// QA runner arguments (feature `qa`).
    pub qa: Option<QaArgs>,
    /// `$TERMINAL` (Linux launcher detection, §11.10 step 6, §14.1), read once
    /// by `main`; `app::init` stores it for `ops::resume` (integrator, Phase B).
    pub terminal_var: Option<std::ffi::OsString>,
}

impl LaunchContext {
    /// Picker mode (`--show-path`, `--show-id`, `--resume`).
    pub fn is_picker(&self) -> bool {
        self.picker.is_some()
    }
}

/// `--qa-scenario`, `--qa-out`, `--qa-model-dir`, `--qa-real-console`
/// (feature `qa`, DESIGN §15.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QaArgs {
    pub scenario: PathBuf,
    pub out_dir: PathBuf,
    pub model_dir: Option<PathBuf>,
    /// `--qa-real-console`: stdout/stderr go to the real console (every other
    /// side effect stays recorded).
    pub real_console: bool,
}

/// Per-file configuration errors: `config.toml` and `gui.toml` are loaded
/// independently; a failing file contributes defaults (§11.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigErrors {
    /// `config.toml` (the CLI's file).
    pub cli: Option<String>,
    /// `gui.toml`; while set, every gui.toml write is blocked.
    pub gui: Option<String>,
}

impl ConfigErrors {
    pub fn is_empty(&self) -> bool {
        self.cli.is_none() && self.gui.is_none()
    }
}

/// What the process's stdout is connected to (§14.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StdoutKind {
    /// A file or a pipe.
    Redirected,
    /// An interactive console or terminal.
    Terminal,
    /// Nothing (started from a desktop shortcut, Finder, …).
    #[default]
    None,
}

/// A native open-file / choose-folder prompt (`PlatformServices::prompt_open_paths`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPathsOptions {
    pub files: bool,
    pub directories: bool,
    pub multiple: bool,
    pub prompt: SharedString,
}

/// Items of the conversation row context menu (§5.2), dispatched through
/// `actions::RowMenu` with the token of the target captured when the menu was built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowMenuItem {
    Open,
    Resume,
    Fork,
    Rename,
    Export,
    CopyAs(ExportFormat),
    CopyPath,
    CopySessionId,
    CopyHeaderSessionId,
    Reveal,
    SearchWithin,
    Outline,
    Delete,
}

/// The file stem of `path` (lossy UTF-8), empty when there is none.
pub fn file_stem_lossy(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A session ID shortened for display: a UUID keeps its first group
/// (`3f2a19c0-…`), any other ID longer than 12 characters its first 8
/// characters plus `…`; short IDs stay as they are.
pub fn short_session_id(id: &str) -> String {
    if id.chars().count() <= 12 {
        return id.to_string();
    }
    let head: String = id.chars().take(9).collect();
    if head.len() == 9 && head.ends_with('-') && head[..8].chars().all(|c| c.is_ascii_hexdigit()) {
        return format!("{head}…");
    }
    let head: String = id.chars().take(8).collect();
    format!("{head}…")
}

/// At most `max` characters of `text`, ending in `…` when shortened.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn non_empty(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty())
}

/// custom title → summary → sanitized preview → file stem (≤ 80 chars).
fn conversation_title(c: &Conversation, file_stem: &str) -> SharedString {
    if let Some(title) = non_empty(c.custom_title.as_deref()) {
        return truncate_chars(title, TITLE_MAX_CHARS).into();
    }
    if let Some(summary) = non_empty(c.summary.as_deref()) {
        return truncate_chars(summary, TITLE_MAX_CHARS).into();
    }
    let preview = sanitize_preview(&c.preview);
    if let Some(preview) = non_empty(Some(preview.as_str())) {
        return truncate_chars(preview, TITLE_MAX_CHARS).into();
    }
    file_stem.to_string().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_text_and_ellipsizes_long_text() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("abcdef", 5), "abcd…");
        assert_eq!(truncate_chars("äöüßé", 5), "äöüßé");
    }

    #[test]
    fn parsed_file_without_conversation_has_unknown_source() {
        let target = ConversationTarget::from_parsed_file(Path::new("/tmp/abc.jsonl"), None);
        assert_eq!(target.source, None);
        assert!(!target.can_modify());
        assert_eq!(target.session_id, "abc");
        assert_eq!(target.file_stem, "abc");
        assert_eq!(target.title.as_ref(), "abc");
        assert!(!target.header_id_differs());
        assert_eq!(target.describe(), "abc");
    }

    #[test]
    fn describe_shortens_the_session_id() {
        let mut target = ConversationTarget::from_parsed_file(
            Path::new("/tmp/3f2a19c0-1c2d-4e5f-8a9b-0c1d2e3f4a5b.jsonl"),
            None,
        );
        target.title = "Fix login bug".into();
        target.project_name = Some("proj".into());
        assert_eq!(target.describe(), "proj · Fix login bug · 3f2a19c0-…");
        assert_eq!(short_session_id("omp_session_custom_id"), "omp_sess…");
        assert_eq!(short_session_id("abc"), "abc");
        assert_eq!(
            short_session_id("01912345-6789-7abc-8def-0123456789ab"),
            "01912345-…"
        );
    }

    #[test]
    fn stem_only_header_is_recognized() {
        let header = HeaderInfo::stem_only(Path::new("/tmp/3f2a.jsonl"));
        assert_eq!(header.file_stem.as_ref(), "3f2a");
        assert!(header.is_stem_only());
    }

    #[test]
    fn key_action_priority_follows_the_tui_check_order() {
        assert_eq!(KeyAction::Delete.priority(), 0);
        assert_eq!(KeyAction::Rename.priority(), 4);
        assert!(KeyAction::Resume.priority() < KeyAction::Fork.priority());
    }
}
