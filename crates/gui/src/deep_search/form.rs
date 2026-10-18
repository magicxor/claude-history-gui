//! The agent forms (DESIGN §5.4, §11.14): the Deep search options, the
//! Within form, the output flags (visibility + budget) shared by Outline and
//! Read, and the Read form. Plain data that maps to the core's argument
//! structs and validates with them (`args.validate()` errors are shown
//! inline); no gpui here, so the tables are unit-tested. WP4.

use std::str::FromStr as _;

use claude_history_core::agent::args::{
    AgentOutlineArgs, AgentOutputFlags, AgentReadArgs, AgentSearchArgs, AgentSearchModeArgs,
    AgentWithinArgs, DEFAULT_READ_CONTEXT, TimeRangeArgs,
};
use claude_history_core::agent::protocol::MessageLineRange;
use claude_history_core::config::{AgentScopeConfig, ConfigFile};
use claude_history_core::search::mode::SearchMode;
use claude_history_core::time_filter::TimePoint;

/// Canonical help texts of the agent options (ADD E4: the product's own
/// wording, the clap doc comments of `claude-history agent …`); the controls'
/// tooltips use them verbatim.
pub mod help {
    pub const TOP: &str = "Maximum conversations, or message hits with --flat";
    pub const WITHIN_TOP: &str = "Maximum number of results";
    pub const FLAT: &str = "Rank message hits across conversations; --top counts hits";
    pub const HITS_PER_CONV: &str =
        "Maximum evidence hits to show per conversation in grouped output";
    pub const ALL_HITS: &str = "Disable duplicate suppression inside grouped conversation results";
    pub const BUDGET: &str = "Output budget in Unicode characters";
    pub const NO_BUDGET: &str = "Disable output budgeting";
    pub const LOCAL: &str = "Search only the current workspace";
    pub const ALL: &str = "Search all workspaces";
    pub const MODE: &str =
        "Search mode for this invocation [possible values: lexical, semantic, exact, hybrid]";
    pub const HYBRID: &str =
        "Alias for --mode hybrid. Combine lexical and semantic message ranking";
    pub const SINCE: &str =
        "Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)";
    pub const BEFORE: &str = "Only conversations older than this duration or date";
    pub const ANCHOR: &str = "Durable message anchor to read within one conversation";
    pub const FOCUS: &str = "Message or range to prioritize when budgeted output is truncated";
    pub const LINES: &str = "Inclusive content line range within one message";
    pub const MATCH: &str = "Find text within one message and return matching lines with context";
    pub const CONTEXT: &str = "Content lines to include before and after each match";
    pub const REFS: &str =
        "ch_ handles, full UUIDs, or UUID-based session basenames/filenames, optionally :mN..mM";
    pub const TOOLS: &str = "Include tool calls";
    pub const TOOL_RESULTS: &str = "Include tool results";
    pub const THINKING: &str = "Include thinking blocks";
    pub const SUBAGENTS: &str = "Include subagent internals";

    /// Every text with its option, in the order of the CLI help (the P test
    /// pins them against ADD E4).
    pub const TABLE: &[(&str, &str)] = &[
        ("top", TOP),
        ("within-top", WITHIN_TOP),
        ("flat", FLAT),
        ("hits-per-conv", HITS_PER_CONV),
        ("all-hits", ALL_HITS),
        ("budget", BUDGET),
        ("no-budget", NO_BUDGET),
        ("local", LOCAL),
        ("all", ALL),
        ("mode", MODE),
        ("hybrid", HYBRID),
        ("since", SINCE),
        ("before", BEFORE),
        ("anchor", ANCHOR),
        ("focus", FOCUS),
        ("lines", LINES),
        ("match", MATCH),
        ("context", CONTEXT),
        ("refs", REFS),
        ("tools", TOOLS),
        ("tool-results", TOOL_RESULTS),
        ("thinking", THINKING),
        ("subagents", SUBAGENTS),
    ];
}

/// The search mode control: Default (the resolved `[agent].mode` →
/// `[search].mode` → lexical) or an explicit mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModeChoice {
    #[default]
    Default,
    Lexical,
    Semantic,
    Exact,
    Hybrid,
}

impl ModeChoice {
    pub const ALL: [ModeChoice; 5] = [
        ModeChoice::Default,
        ModeChoice::Lexical,
        ModeChoice::Semantic,
        ModeChoice::Exact,
        ModeChoice::Hybrid,
    ];

    pub fn mode(self) -> Option<SearchMode> {
        match self {
            ModeChoice::Default => None,
            ModeChoice::Lexical => Some(SearchMode::Lexical),
            ModeChoice::Semantic => Some(SearchMode::Semantic),
            ModeChoice::Exact => Some(SearchMode::Exact),
            ModeChoice::Hybrid => Some(SearchMode::Hybrid),
        }
    }

    /// The segment label; Default shows the resolved default.
    pub fn label(self, defaults: &Defaults) -> String {
        match self {
            ModeChoice::Default => format!("Default ({})", defaults.mode),
            ModeChoice::Lexical => "Lexical".to_string(),
            ModeChoice::Semantic => "Semantic".to_string(),
            ModeChoice::Exact => "Exact".to_string(),
            ModeChoice::Hybrid => "Hybrid".to_string(),
        }
    }

    /// The probe/test name (`default`, `lexical`, …).
    pub fn name(self) -> &'static str {
        match self {
            ModeChoice::Default => "default",
            ModeChoice::Lexical => "lexical",
            ModeChoice::Semantic => "semantic",
            ModeChoice::Exact => "exact",
            ModeChoice::Hybrid => "hybrid",
        }
    }

    fn modes(self) -> AgentSearchModeArgs {
        match self.mode() {
            Some(mode) => AgentSearchModeArgs::explicit(mode),
            None => AgentSearchModeArgs::default(),
        }
    }
}

/// The scope control: Default (neither `--local` nor `--all`, so core uses
/// `[agent] scope`), All or Workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScopeChoice {
    #[default]
    Default,
    All,
    Workspace,
}

impl ScopeChoice {
    pub const ALL: [ScopeChoice; 3] = [
        ScopeChoice::Default,
        ScopeChoice::All,
        ScopeChoice::Workspace,
    ];

    pub fn label(self, defaults: &Defaults) -> String {
        match self {
            ScopeChoice::Default => format!("Default ({})", defaults.scope),
            ScopeChoice::All => "All".to_string(),
            ScopeChoice::Workspace => "Workspace".to_string(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ScopeChoice::Default => "default",
            ScopeChoice::All => "all",
            ScopeChoice::Workspace => "workspace",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            ScopeChoice::Default => "Use the [agent] scope of the configuration",
            ScopeChoice::All => help::ALL,
            ScopeChoice::Workspace => help::LOCAL,
        }
    }
}

/// The values the form shows as defaults, resolved from the effective
/// `[agent]` / `[search]` configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defaults {
    /// `All` or `Workspace` (`[agent] scope`).
    pub scope: &'static str,
    /// `lexical`, `exact`, … (`[agent] mode` → `[search] mode` → lexical).
    pub mode: &'static str,
    pub top: usize,
    pub within_top: usize,
    pub hits_per_conv: usize,
    pub budget: usize,
}

/// The built-in defaults of core's `agent` service.
pub const BUILTIN_TOP: usize = 10;
pub const BUILTIN_WITHIN_TOP: usize = 20;
pub const BUILTIN_HITS_PER_CONV: usize = 2;
pub const BUILTIN_BUDGET: usize = 6000;

impl Defaults {
    pub fn resolve(config: &ConfigFile) -> Defaults {
        let agent = config.agent.clone().unwrap_or_default();
        let mode = agent
            .mode
            .or(config.search.as_ref().and_then(|search| search.mode))
            .unwrap_or_default();
        Defaults {
            scope: match agent.scope {
                Some(AgentScopeConfig::Local) => "Workspace",
                _ => "All",
            },
            mode: mode.as_str(),
            top: agent.top.unwrap_or(BUILTIN_TOP),
            within_top: agent.within_top.unwrap_or(BUILTIN_WITHIN_TOP),
            hits_per_conv: agent.hits_per_conversation.unwrap_or(BUILTIN_HITS_PER_CONV),
            budget: agent.output_chars.unwrap_or(BUILTIN_BUDGET),
        }
    }
}

/// `Default (<n>)`, the placeholder of a number field.
pub fn default_placeholder(value: usize) -> String {
    format!("Default ({value})")
}

/// The time range of the form as the two texts the dropdown works with
/// (`2d`, `2026-07-20`); empty = no bound.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeTexts {
    pub since: String,
    pub before: String,
}

impl TimeTexts {
    /// The chip label (`Since 2d · Before 2026-07-20`), `None` = Any time.
    pub fn label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.since.trim().is_empty() {
            parts.push(format!("Since {}", self.since.trim()));
        }
        if !self.before.trim().is_empty() {
            parts.push(format!("Before {}", self.before.trim()));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// Read the texts back from a chip label of the time widget.
    pub fn from_label(label: Option<&str>) -> TimeTexts {
        let mut texts = TimeTexts::default();
        for part in label.unwrap_or_default().split(" · ") {
            if let Some(text) = part.strip_prefix("Since ") {
                texts.since = text.to_string();
            } else if let Some(text) = part.strip_prefix("Before ") {
                texts.before = text.to_string();
            }
        }
        texts
    }

    /// The core arguments; a bound that does not parse is the inline error.
    pub fn to_args(&self) -> Result<TimeRangeArgs, String> {
        let parse = |text: &str| -> Result<Option<TimePoint>, String> {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            TimePoint::from_str(text)
                .map(Some)
                .map_err(|error| error.to_string())
        };
        Ok(TimeRangeArgs {
            since: parse(&self.since)?,
            after: None,
            before: parse(&self.before)?,
        })
    }
}

/// The Deep search options (§5.4). Numbers are the field texts: empty =
/// default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchForm {
    pub mode: ModeChoice,
    pub scope: ScopeChoice,
    pub time: TimeTexts,
    pub top: String,
    pub flat: bool,
    pub hits_per_conv: String,
    pub all_hits: bool,
    pub budget: String,
    pub no_budget: bool,
}

impl SearchForm {
    /// `AgentSearchArgs` for `query` (trimmed); the error is the inline text
    /// (a field that is not a number, or core's `validate()` message).
    pub fn to_args(&self, query: &str) -> Result<AgentSearchArgs, String> {
        let args = AgentSearchArgs {
            query: query.trim().to_string(),
            top: parse_count("--top", &self.top)?,
            budget: parse_count("--budget", &self.budget)?,
            no_budget: self.no_budget,
            flat: self.flat,
            hits_per_conv: parse_count("--hits-per-conv", &self.hits_per_conv)?,
            all_hits: self.all_hits,
            local: self.scope == ScopeChoice::Workspace,
            all: self.scope == ScopeChoice::All,
            search_mode: self.mode.modes(),
            time: self.time.to_args()?,
        };
        args.validate()?;
        Ok(args)
    }
}

/// The Within tab's form: query, mode, top and budget (the conversation is
/// the viewer's, never a field).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WithinForm {
    pub mode: ModeChoice,
    pub top: String,
    pub budget: String,
    pub no_budget: bool,
}

impl WithinForm {
    pub fn to_args(&self, conversation: &str, query: &str) -> Result<AgentWithinArgs, String> {
        let args = AgentWithinArgs {
            conversation: conversation.trim().to_string(),
            query: query.trim().to_string(),
            top: parse_count("--top", &self.top)?,
            budget: parse_count("--budget", &self.budget)?,
            no_budget: self.no_budget,
            search_mode: self.mode.modes(),
        };
        args.validate()?;
        Ok(args)
    }
}

/// The visibility toggles and the budget of Outline and Read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputForm {
    pub tools: bool,
    pub tool_results: bool,
    pub thinking: bool,
    pub subagents: bool,
    pub budget: String,
    pub no_budget: bool,
}

/// One visibility flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    Tools,
    ToolResults,
    Thinking,
    Subagents,
}

impl Flag {
    pub const ALL: [Flag; 4] = [
        Flag::Tools,
        Flag::ToolResults,
        Flag::Thinking,
        Flag::Subagents,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Flag::Tools => "tools",
            Flag::ToolResults => "tool results",
            Flag::Thinking => "thinking",
            Flag::Subagents => "subagents",
        }
    }

    /// The `[agent]` key that forces the flag on.
    pub fn config_key(self) -> &'static str {
        match self {
            Flag::Tools => "tools",
            Flag::ToolResults => "tool_results",
            Flag::Thinking => "thinking",
            Flag::Subagents => "subagents",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Flag::Tools => help::TOOLS,
            Flag::ToolResults => help::TOOL_RESULTS,
            Flag::Thinking => help::THINKING,
            Flag::Subagents => help::SUBAGENTS,
        }
    }

    /// Whether `[agent]` forces the flag on (core ORs it into every read
    /// and outline).
    pub fn forced_by_config(self, config: &ConfigFile) -> bool {
        let Some(agent) = &config.agent else {
            return false;
        };
        match self {
            Flag::Tools => agent.tools,
            Flag::ToolResults => agent.tool_results,
            Flag::Thinking => agent.thinking,
            Flag::Subagents => agent.subagents,
        }
        .unwrap_or(false)
    }

    /// The tooltip of a flag that config forces on.
    pub fn forced_tooltip(self) -> String {
        format!("Enabled by [agent] {} in config", self.config_key())
    }
}

impl OutputForm {
    pub fn get(&self, flag: Flag) -> bool {
        match flag {
            Flag::Tools => self.tools,
            Flag::ToolResults => self.tool_results,
            Flag::Thinking => self.thinking,
            Flag::Subagents => self.subagents,
        }
    }

    pub fn set(&mut self, flag: Flag, value: bool) {
        match flag {
            Flag::Tools => self.tools = value,
            Flag::ToolResults => self.tool_results = value,
            Flag::Thinking => self.thinking = value,
            Flag::Subagents => self.subagents = value,
        }
    }

    /// The core flags (the form's own toggles; core ORs the `[agent]` ones).
    pub fn to_flags(&self) -> Result<AgentOutputFlags, String> {
        let flags = AgentOutputFlags {
            budget: parse_count("--budget", &self.budget)?,
            no_budget: self.no_budget,
            tools: self.tools,
            tool_results: self.tool_results,
            thinking: self.thinking,
            subagents: self.subagents,
        };
        flags.validate()?;
        Ok(flags)
    }

    pub fn to_outline_args(&self, conversation: &str) -> Result<AgentOutlineArgs, String> {
        let args = AgentOutlineArgs {
            conversation: conversation.trim().to_string(),
            output: self.to_flags()?,
        };
        args.validate()?;
        Ok(args)
    }
}

/// The Read tab's form (besides the output flags).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadForm {
    /// One or more refs separated by whitespace or commas; empty = the
    /// viewer's conversation.
    pub refs: String,
    pub anchor: String,
    pub focus: String,
    /// `START..END` (content lines of one message).
    pub lines: String,
    /// Text to find in one message.
    pub match_query: String,
    /// Lines around each match; empty = 3.
    pub context: String,
    pub output: OutputForm,
}

/// The refs of the Refs field: split on whitespace and commas.
pub fn parse_refs(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

impl ReadForm {
    /// `AgentReadArgs`; `default_ref` (the viewer's full ref) stands in for
    /// an empty Refs field.
    pub fn to_args(&self, default_ref: Option<&str>) -> Result<AgentReadArgs, String> {
        let mut refs = parse_refs(&self.refs);
        if refs.is_empty()
            && let Some(default_ref) = default_ref
        {
            refs.push(default_ref.to_string());
        }
        let text = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let lines = match text(&self.lines) {
            Some(lines) => Some(
                MessageLineRange::from_str(&lines)
                    .map_err(|error| format!("invalid value '{lines}' for '--lines': {error}"))?,
            ),
            None => None,
        };
        let context = parse_count("--context", &self.context)?.unwrap_or(DEFAULT_READ_CONTEXT);
        let args = AgentReadArgs {
            refs,
            anchor: text(&self.anchor),
            focus: text(&self.focus),
            lines,
            match_query: text(&self.match_query),
            context,
            output: self.output.to_flags()?,
        };
        args.validate()?;
        Ok(args)
    }
}

/// A count field: empty = `None`; otherwise a non-negative integer (zero is
/// rejected by core's `validate()`).
fn parse_count(flag: &str, text: &str) -> Result<Option<usize>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<usize>()
        .map(Some)
        .map_err(|error| format!("invalid value '{text}' for '{flag}': {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_history_core::config::AgentConfig;

    fn config(agent: AgentConfig) -> ConfigFile {
        ConfigFile {
            agent: Some(agent),
            ..ConfigFile::default()
        }
    }

    /// D-002: each control sets its field; Scope Default → neither flag.
    #[test]
    fn the_search_form_maps_every_control_to_its_argument() {
        let form = SearchForm::default();
        let args = form.to_args("  token refresh ").unwrap();
        assert_eq!(args.query, "token refresh");
        assert!(!args.local && !args.all, "Default scope sets neither flag");
        assert_eq!(args.mode_override(), None);
        assert_eq!(
            (args.top, args.budget, args.hits_per_conv),
            (None, None, None)
        );
        assert!(!args.flat && !args.all_hits && !args.no_budget);

        let form = SearchForm {
            mode: ModeChoice::Hybrid,
            scope: ScopeChoice::Workspace,
            time: TimeTexts {
                since: "2d".into(),
                before: String::new(),
            },
            top: "3".into(),
            flat: true,
            hits_per_conv: "1".into(),
            all_hits: true,
            budget: "200".into(),
            no_budget: false,
        };
        let args = form.to_args("q").unwrap();
        assert!(args.local && !args.all);
        assert_eq!(args.mode_override(), Some(SearchMode::Hybrid));
        assert_eq!(args.top, Some(3));
        assert_eq!(args.hits_per_conv, Some(1));
        assert_eq!(args.budget, Some(200));
        assert!(args.flat && args.all_hits);
        assert!(args.time.since.is_some() && args.time.before.is_none());

        let all = SearchForm {
            scope: ScopeChoice::All,
            no_budget: true,
            ..SearchForm::default()
        };
        let args = all.to_args("q").unwrap();
        assert!(args.all && !args.local && args.no_budget);
        for (mode, expected) in [
            (ModeChoice::Lexical, SearchMode::Lexical),
            (ModeChoice::Semantic, SearchMode::Semantic),
            (ModeChoice::Exact, SearchMode::Exact),
        ] {
            let form = SearchForm {
                mode,
                ..SearchForm::default()
            };
            assert_eq!(form.to_args("q").unwrap().mode_override(), Some(expected));
        }
    }

    /// D-002: `budget = 0` is core's `--budget` error; text that is not a
    /// number and budget + no budget are inline errors too.
    #[test]
    fn the_search_form_validates_with_core() {
        let form = SearchForm {
            budget: "0".into(),
            ..SearchForm::default()
        };
        let error = form.to_args("q").unwrap_err();
        assert!(
            error.contains("'--budget'") && error.contains("greater than zero"),
            "{error}"
        );
        let form = SearchForm {
            top: "many".into(),
            ..SearchForm::default()
        };
        let error = form.to_args("q").unwrap_err();
        assert!(
            error.contains("'--top'") && error.contains("invalid digit"),
            "{error}"
        );
        let form = SearchForm {
            budget: "10".into(),
            no_budget: true,
            ..SearchForm::default()
        };
        assert!(
            form.to_args("q")
                .unwrap_err()
                .contains("cannot be used with")
        );
        assert!(SearchForm::default().to_args("   ").is_err());
        let form = SearchForm {
            time: TimeTexts {
                since: "soon".into(),
                before: String::new(),
            },
            ..SearchForm::default()
        };
        assert!(form.to_args("q").is_err(), "a bad time bound is inline");
    }

    /// D-003: the form shows the resolved defaults.
    #[test]
    fn defaults_resolve_from_the_agent_and_search_config() {
        let none = Defaults::resolve(&ConfigFile::default());
        assert_eq!((none.scope, none.mode, none.top), ("All", "lexical", 10));
        assert_eq!(
            ModeChoice::Default.label(&none),
            "Default (lexical)",
            "built-in default"
        );
        let agent = config(AgentConfig {
            scope: Some(AgentScopeConfig::Local),
            mode: Some(SearchMode::Exact),
            top: Some(3),
            within_top: Some(7),
            hits_per_conversation: Some(4),
            output_chars: Some(900),
            ..AgentConfig::default()
        });
        let resolved = Defaults::resolve(&agent);
        assert_eq!(ScopeChoice::Default.label(&resolved), "Default (Workspace)");
        assert_eq!(ModeChoice::Default.label(&resolved), "Default (exact)");
        assert_eq!(default_placeholder(resolved.top), "Default (3)");
        assert_eq!(
            (resolved.within_top, resolved.hits_per_conv, resolved.budget),
            (7, 4, 900)
        );
        // `[search] mode` applies below `[agent] mode`.
        let search = ConfigFile {
            search: Some(claude_history_core::config::SearchConfig {
                mode: Some(SearchMode::Hybrid),
            }),
            ..ConfigFile::default()
        };
        assert_eq!(Defaults::resolve(&search).mode, "hybrid");
    }

    /// D-028: the tooltip table equals the ADD E4 strings.
    #[test]
    fn the_help_texts_equal_the_product_wording() {
        let expected = [
            ("top", "Maximum conversations, or message hits with --flat"),
            (
                "flat",
                "Rank message hits across conversations; --top counts hits",
            ),
            (
                "hits-per-conv",
                "Maximum evidence hits to show per conversation in grouped output",
            ),
            (
                "all-hits",
                "Disable duplicate suppression inside grouped conversation results",
            ),
            (
                "hybrid",
                "Alias for --mode hybrid. Combine lexical and semantic message ranking",
            ),
            ("budget", "Output budget in Unicode characters"),
            (
                "anchor",
                "Durable message anchor to read within one conversation",
            ),
            (
                "focus",
                "Message or range to prioritize when budgeted output is truncated",
            ),
            ("lines", "Inclusive content line range within one message"),
            (
                "match",
                "Find text within one message and return matching lines with context",
            ),
            (
                "context",
                "Content lines to include before and after each match",
            ),
            (
                "since",
                "Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)",
            ),
            (
                "before",
                "Only conversations older than this duration or date",
            ),
        ];
        for (name, text) in expected {
            let (_, actual) = help::TABLE
                .iter()
                .find(|(key, _)| *key == name)
                .unwrap_or_else(|| panic!("{name} is in the table"));
            assert_eq!(*actual, text, "{name}");
        }
        // The time widget shares the same wording.
        assert_eq!(help::SINCE, crate::widgets::time_range::SINCE_HELP);
        assert_eq!(help::BEFORE, crate::widgets::time_range::BEFORE_HELP);
    }

    #[test]
    fn time_texts_round_trip_the_widget_label() {
        let texts = TimeTexts::from_label(Some("Since 2d · Before 2026-07-20"));
        assert_eq!(texts.since, "2d");
        assert_eq!(texts.before, "2026-07-20");
        assert_eq!(
            texts.label().as_deref(),
            Some("Since 2d · Before 2026-07-20")
        );
        assert_eq!(TimeTexts::from_label(None).label(), None);
        assert!(texts.to_args().is_ok());
    }

    /// D-020, D-023: Refs split on whitespace and commas; the default is the
    /// viewer's ref; Lines + Match conflict as in the CLI.
    #[test]
    fn the_read_form_maps_to_read_arguments() {
        assert_eq!(parse_refs("a b,c ,, d"), vec!["a", "b", "c", "d"]);
        let form = ReadForm::default();
        let args = form.to_args(Some("ch_full")).unwrap();
        assert_eq!(args.refs, vec!["ch_full"]);
        assert!(form.to_args(None).is_err(), "no refs and no default");
        let form = ReadForm {
            refs: "ch_a:m2..m3 ch_b".into(),
            anchor: "ma_1".into(),
            focus: "m2".into(),
            output: OutputForm {
                tools: true,
                budget: "500".into(),
                ..OutputForm::default()
            },
            ..ReadForm::default()
        };
        let args = form.to_args(Some("ignored")).unwrap();
        assert_eq!(args.refs, vec!["ch_a:m2..m3", "ch_b"]);
        assert_eq!(args.anchor.as_deref(), Some("ma_1"));
        assert_eq!(args.focus.as_deref(), Some("m2"));
        assert!(args.output.tools && args.output.budget == Some(500));

        let both = ReadForm {
            lines: "1..3".into(),
            match_query: "retry".into(),
            ..ReadForm::default()
        };
        assert_eq!(
            both.to_args(Some("ch_x")).unwrap_err(),
            "the argument '--lines' cannot be used with '--match'"
        );
        let lines = ReadForm {
            lines: "2..4".into(),
            ..ReadForm::default()
        };
        let args = lines.to_args(Some("ch_x")).unwrap();
        assert_eq!(args.lines.map(|l| (l.start, l.end)), Some((2, 4)));
        let bad = ReadForm {
            lines: "x".into(),
            ..ReadForm::default()
        };
        assert!(bad.to_args(Some("ch_x")).unwrap_err().contains("'--lines'"));
        let matching = ReadForm {
            match_query: "retry".into(),
            context: "5".into(),
            ..ReadForm::default()
        };
        let args = matching.to_args(Some("ch_x")).unwrap();
        assert_eq!(
            (args.match_query.as_deref(), args.context),
            (Some("retry"), 5)
        );
    }

    #[test]
    fn within_and_outline_forms_map_their_fields() {
        let form = WithinForm {
            mode: ModeChoice::Exact,
            top: "5".into(),
            budget: "200".into(),
            no_budget: false,
        };
        let args = form.to_args(" ch_full ", " zzz ").unwrap();
        assert_eq!(args.conversation, "ch_full");
        assert_eq!(args.query, "zzz");
        assert_eq!((args.top, args.budget), (Some(5), Some(200)));
        assert_eq!(args.mode_override(), Some(SearchMode::Exact));
        assert!(WithinForm::default().to_args("c", "").is_err());

        let output = OutputForm {
            thinking: true,
            no_budget: true,
            ..OutputForm::default()
        };
        let outline = output.to_outline_args("ch_full").unwrap();
        assert!(outline.output.thinking && outline.output.no_budget);
    }

    /// D-036: config-true flags are reported as forced.
    #[test]
    fn config_forced_flags_are_detected() {
        let forced = config(AgentConfig {
            tools: Some(true),
            ..AgentConfig::default()
        });
        assert!(Flag::Tools.forced_by_config(&forced));
        assert!(!Flag::Thinking.forced_by_config(&forced));
        assert!(!Flag::Tools.forced_by_config(&ConfigFile::default()));
        assert_eq!(
            Flag::Tools.forced_tooltip(),
            "Enabled by [agent] tools in config"
        );
        assert_eq!(
            Flag::ToolResults.forced_tooltip(),
            "Enabled by [agent] tool_results in config"
        );
    }
}
