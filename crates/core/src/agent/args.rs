//! Agent request arguments (refactor R5).
//!
//! Upstream defined these as clap argument structs in `cli.rs:76-282`
//! (`AgentCommand`, `AgentSearchArgs`, ...). Core keeps the same field names
//! and helper methods without clap; the validation clap enforced at parse
//! time is available as `validate()` / `normalized()` on each struct, and
//! [`crate::agent::service::execute`] applies it before running a request.

use crate::agent::protocol::MessageLineRange;
use crate::search::mode::SearchMode;
use crate::time_filter::{TimeFilter, TimeFilterError, TimePoint};

/// Default `--context` of `agent read --match`.
pub const DEFAULT_READ_CONTEXT: usize = 3;

/// One agent request (upstream `AgentCommand`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRequest {
    /// Search transcript content across all conversations (known UUID? use outline/read/within)
    Search(AgentSearchArgs),
    /// Search within one conversation
    Within(AgentWithinArgs),
    /// Read transcript message ranges
    Read(AgentReadArgs),
    /// Outline a conversation transcript
    Outline(AgentOutlineArgs),
}

impl AgentRequest {
    /// Check the constraints clap enforced (without modifying anything).
    pub fn validate(&self) -> Result<(), String> {
        match self {
            AgentRequest::Search(args) => args.validate(),
            AgentRequest::Within(args) => args.validate(),
            AgentRequest::Read(args) => args.validate(),
            AgentRequest::Outline(args) => args.validate(),
        }
    }

    /// Trim the free-text values (as clap's `non_empty_string` did) and
    /// validate. This is what `execute` runs on every request.
    pub fn normalized(self) -> Result<Self, String> {
        Ok(match self {
            AgentRequest::Search(args) => AgentRequest::Search(args.normalized()?),
            AgentRequest::Within(args) => AgentRequest::Within(args.normalized()?),
            AgentRequest::Read(args) => AgentRequest::Read(args.normalized()?),
            AgentRequest::Outline(args) => AgentRequest::Outline(args.normalized()?),
        })
    }
}

/// Mode override of `agent search` / `agent within`: `--mode <MODE>` or one
/// of the legacy aliases (`--lexical`, `--semantic`, `--exact`, `--hybrid`).
/// At most one may be set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSearchModeArgs {
    /// Search mode for this invocation
    pub mode: Option<SearchMode>,
    /// Alias for --mode lexical
    pub lexical: bool,
    /// Alias for --mode semantic
    pub semantic: bool,
    /// Alias for --mode exact
    pub exact: bool,
    /// Alias for --mode hybrid. Combine lexical and semantic message ranking
    pub hybrid: bool,
}

impl AgentSearchModeArgs {
    pub fn explicit(mode: SearchMode) -> Self {
        Self {
            mode: Some(mode),
            ..Self::default()
        }
    }

    pub fn override_value(&self) -> Option<SearchMode> {
        self.mode.or_else(|| {
            [
                (self.lexical, SearchMode::Lexical),
                (self.semantic, SearchMode::Semantic),
                (self.exact, SearchMode::Exact),
                (self.hybrid, SearchMode::Hybrid),
            ]
            .into_iter()
            .find_map(|(selected, mode)| selected.then_some(mode))
        })
    }

    /// clap group `agent_search_mode` (`multiple = false`).
    pub fn validate(&self) -> Result<(), String> {
        let selected = [
            self.mode.is_some().then_some("--mode"),
            self.lexical.then_some("--lexical"),
            self.semantic.then_some("--semantic"),
            self.exact.then_some("--exact"),
            self.hybrid.then_some("--hybrid"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        match selected.as_slice() {
            [first, second, ..] => Err(conflict(first, second)),
            _ => Ok(()),
        }
    }
}

/// `agent search <QUERY>`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSearchArgs {
    /// Search query
    pub query: String,
    /// Maximum conversations, or message hits with --flat
    pub top: Option<usize>,
    /// Output budget in Unicode characters
    pub budget: Option<usize>,
    /// Disable output budgeting
    pub no_budget: bool,
    /// Rank message hits across conversations; --top counts hits
    pub flat: bool,
    /// Maximum evidence hits to show per conversation in grouped output
    pub hits_per_conv: Option<usize>,
    /// Disable duplicate suppression inside grouped conversation results
    pub all_hits: bool,
    /// Search only the current workspace
    pub local: bool,
    /// Search all workspaces
    pub all: bool,
    pub search_mode: AgentSearchModeArgs,
    pub time: TimeRangeArgs,
}

impl AgentSearchArgs {
    /// A search with every option at its default.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Self::default()
        }
    }

    pub fn mode_override(&self) -> Option<SearchMode> {
        self.search_mode.override_value()
    }

    pub fn validate(&self) -> Result<(), String> {
        non_empty("<QUERY>", &self.query)?;
        positive("--top", self.top)?;
        positive("--budget", self.budget)?;
        positive("--hits-per-conv", self.hits_per_conv)?;
        budget_conflict(self.budget, self.no_budget)?;
        if self.local && self.all {
            return Err(conflict("--local", "--all"));
        }
        self.search_mode.validate()?;
        self.time.validate()
    }

    pub fn normalized(mut self) -> Result<Self, String> {
        self.query = self.query.trim().to_string();
        self.validate()?;
        Ok(self)
    }
}

/// `agent within <CONVERSATION> <QUERY>`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentWithinArgs {
    /// Conversation: ch_ handle, full UUID, or UUID-based session basename/filename
    pub conversation: String,
    /// Search query
    pub query: String,
    /// Maximum number of results
    pub top: Option<usize>,
    /// Output budget in Unicode characters
    pub budget: Option<usize>,
    /// Disable output budgeting
    pub no_budget: bool,
    pub search_mode: AgentSearchModeArgs,
}

impl AgentWithinArgs {
    pub fn new(conversation: impl Into<String>, query: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            query: query.into(),
            ..Self::default()
        }
    }

    pub fn mode_override(&self) -> Option<SearchMode> {
        self.search_mode.override_value()
    }

    pub fn validate(&self) -> Result<(), String> {
        non_empty("<CONVERSATION>", &self.conversation)?;
        non_empty("<QUERY>", &self.query)?;
        positive("--top", self.top)?;
        positive("--budget", self.budget)?;
        budget_conflict(self.budget, self.no_budget)?;
        self.search_mode.validate()
    }

    pub fn normalized(mut self) -> Result<Self, String> {
        self.conversation = self.conversation.trim().to_string();
        self.query = self.query.trim().to_string();
        self.validate()?;
        Ok(self)
    }
}

/// Output options shared by `agent read` and `agent outline`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOutputFlags {
    /// Output budget in Unicode characters
    pub budget: Option<usize>,
    /// Disable output budgeting
    pub no_budget: bool,
    /// Include tool calls
    pub tools: bool,
    /// Include tool results
    pub tool_results: bool,
    /// Include thinking blocks
    pub thinking: bool,
    /// Include subagent internals
    pub subagents: bool,
}

impl AgentOutputFlags {
    pub fn validate(&self) -> Result<(), String> {
        positive("--budget", self.budget)?;
        budget_conflict(self.budget, self.no_budget)
    }
}

/// `agent read <REF>...`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReadArgs {
    /// ch_ handles, full UUIDs, or UUID-based session basenames/filenames, optionally :mN..mM
    pub refs: Vec<String>,
    /// Durable message anchor to read within one conversation
    pub anchor: Option<String>,
    /// Message or range to prioritize when budgeted output is truncated
    pub focus: Option<String>,
    /// Inclusive content line range within one message
    pub lines: Option<MessageLineRange>,
    /// Find text within one message and return matching lines with context
    pub match_query: Option<String>,
    /// Content lines to include before and after each match
    /// (default [`DEFAULT_READ_CONTEXT`]; only meaningful with `match_query`)
    pub context: usize,
    pub output: AgentOutputFlags,
}

impl Default for AgentReadArgs {
    fn default() -> Self {
        Self {
            refs: Vec::new(),
            anchor: None,
            focus: None,
            lines: None,
            match_query: None,
            context: DEFAULT_READ_CONTEXT,
            output: AgentOutputFlags::default(),
        }
    }
}

impl AgentReadArgs {
    pub fn new(refs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            refs: refs.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.refs.is_empty() {
            return Err("the following required arguments were not provided: <REFS>...".into());
        }
        for reference in &self.refs {
            non_empty("<REFS>", reference)?;
        }
        if let Some(anchor) = &self.anchor {
            non_empty("--anchor", anchor)?;
        }
        if let Some(focus) = &self.focus {
            non_empty("--focus", focus)?;
        }
        if let Some(query) = &self.match_query {
            non_empty("--match", query)?;
        }
        if let Some(lines) = &self.lines {
            validate_line_range(lines)?;
        }
        if self.lines.is_some() && self.match_query.is_some() {
            return Err(conflict("--lines", "--match"));
        }
        // clap: `--context` `requires = "match_query"`. A plain struct cannot
        // tell an explicit default from an omitted value, so only a
        // non-default context without a match query is rejected.
        if self.context != DEFAULT_READ_CONTEXT && self.match_query.is_none() {
            return Err(
                "the following required arguments were not provided: --match <MATCH_QUERY> \
                 (required by --context)"
                    .into(),
            );
        }
        self.output.validate()
    }

    pub fn normalized(mut self) -> Result<Self, String> {
        for reference in &mut self.refs {
            *reference = reference.trim().to_string();
        }
        for value in [&mut self.anchor, &mut self.focus, &mut self.match_query]
            .into_iter()
            .flatten()
        {
            *value = value.trim().to_string();
        }
        self.validate()?;
        Ok(self)
    }
}

/// `agent outline <CONVERSATION>`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOutlineArgs {
    /// Conversation to outline: ch_ handle, full UUID, or UUID-based session basename/filename
    pub conversation: String,
    pub output: AgentOutputFlags,
}

impl AgentOutlineArgs {
    pub fn new(conversation: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        non_empty("<CONVERSATION>", &self.conversation)?;
        self.output.validate()
    }

    pub fn normalized(mut self) -> Result<Self, String> {
        self.conversation = self.conversation.trim().to_string();
        self.validate()?;
        Ok(self)
    }
}

/// Bounds that narrow the conversation corpus by timestamp before ranking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeRangeArgs {
    /// Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)
    pub since: Option<TimePoint>,
    /// Alias for --since
    pub after: Option<TimePoint>,
    /// Only conversations older than this duration or date
    pub before: Option<TimePoint>,
}

impl TimeRangeArgs {
    /// Resolve the flags into a filter, relative to the current instant.
    pub fn resolve(&self) -> std::result::Result<TimeFilter, TimeFilterError> {
        TimeFilter::resolve(
            self.since.as_ref(),
            self.after.as_ref(),
            self.before.as_ref(),
        )
    }

    /// clap group `time_lower_bound`: `--since` and `--after` are exclusive.
    pub fn validate(&self) -> Result<(), String> {
        if self.since.is_some() && self.after.is_some() {
            return Err(conflict("--since", "--after"));
        }
        Ok(())
    }
}

fn conflict(first: &str, second: &str) -> String {
    format!("the argument '{first}' cannot be used with '{second}'")
}

/// clap `value_parser = non_empty_string` (cli.rs:486-493).
fn non_empty(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!(
            "invalid value '{value}' for '{name}': value must not be empty"
        ))
    } else {
        Ok(())
    }
}

/// clap `value_parser = non_zero_usize` (cli.rs:495-504).
fn positive(name: &str, value: Option<usize>) -> Result<(), String> {
    if value == Some(0) {
        Err(format!(
            "invalid value '0' for '{name}': value must be greater than zero"
        ))
    } else {
        Ok(())
    }
}

fn budget_conflict(budget: Option<usize>, no_budget: bool) -> Result<(), String> {
    if budget.is_some() && no_budget {
        Err(conflict("--budget", "--no-budget"))
    } else {
        Ok(())
    }
}

/// The checks of `MessageLineRange::from_str` for a directly built range.
fn validate_line_range(range: &MessageLineRange) -> Result<(), String> {
    if range.start == 0 || range.end == 0 {
        return Err("invalid value for '--lines': line range starts at line 1".into());
    }
    if range.start > range.end {
        return Err("invalid value for '--lines': line range start must not exceed its end".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Re-expression of upstream's clap parser tests (`cli.rs:540-1141`)
    //! for the validation subset that is not clap-specific.
    use super::*;

    fn point(value: &str) -> TimePoint {
        value.parse().unwrap()
    }

    #[test]
    fn search_defaults_to_global_scope_and_config_mode() {
        let search = AgentSearchArgs::new("cache");
        assert_eq!(search.top, None);
        assert!(!search.flat);
        assert_eq!(search.hits_per_conv, None);
        assert!(!search.all_hits);
        assert!(!search.local);
        assert!(!search.all);
        assert_eq!(search.mode_override(), None);
        assert_eq!(search.time.resolve().unwrap(), TimeFilter::default());
        assert!(search.validate().is_ok());
    }

    #[test]
    fn legacy_aliases_and_mode_share_one_override() {
        let mut search = AgentSearchArgs::new("cache");
        search.search_mode.hybrid = true;
        assert_eq!(search.mode_override(), Some(SearchMode::Hybrid));
        let within = AgentWithinArgs {
            search_mode: AgentSearchModeArgs::explicit(SearchMode::Hybrid),
            ..AgentWithinArgs::new("ch_abc123", "cache")
        };
        assert_eq!(within.mode_override(), Some(SearchMode::Hybrid));
        let semantic = AgentSearchModeArgs {
            semantic: true,
            ..Default::default()
        };
        assert_eq!(semantic.override_value(), Some(SearchMode::Semantic));
    }

    #[test]
    fn mode_value_conflicts_with_legacy_alias_and_aliases_conflict() {
        let mut search = AgentSearchArgs::new("cache");
        search.search_mode.mode = Some(SearchMode::Semantic);
        search.search_mode.hybrid = true;
        assert!(
            search
                .validate()
                .unwrap_err()
                .contains("cannot be used with")
        );

        let mut search = AgentSearchArgs::new("cache");
        search.search_mode.semantic = true;
        search.search_mode.hybrid = true;
        assert!(
            search
                .validate()
                .unwrap_err()
                .contains("cannot be used with")
        );
    }

    #[test]
    fn local_and_all_are_exclusive() {
        let mut search = AgentSearchArgs::new("cache");
        search.local = true;
        assert!(search.validate().is_ok());
        search.all = true;
        assert!(
            search
                .validate()
                .unwrap_err()
                .contains("cannot be used with")
        );
    }

    #[test]
    fn since_and_after_are_mutually_exclusive_but_since_and_before_combine() {
        let mut search = AgentSearchArgs::new("needle");
        search.time.since = Some(point("2d"));
        search.time.after = Some(point("3d"));
        assert!(
            search
                .validate()
                .unwrap_err()
                .contains("cannot be used with")
        );

        let time = TimeRangeArgs {
            since: Some(point("2026-01-01")),
            after: None,
            before: Some(point("2026-07-20")),
        };
        assert!(time.validate().is_ok());
        let filter = time.resolve().unwrap();
        assert!(filter.after.is_some());
        assert!(filter.before.is_some());
    }

    #[test]
    fn inverted_time_range_is_rejected_at_resolve_time() {
        let time = TimeRangeArgs {
            since: None,
            after: Some(point("2026-07-20")),
            before: Some(point("2026-01-01")),
        };
        assert!(time.validate().is_ok());
        assert!(time.resolve().is_err());
    }

    #[test]
    fn invalid_time_value_reports_the_time_filter_error() {
        let error = "30x".parse::<TimePoint>().unwrap_err();
        assert!(error.to_string().contains("unknown unit 'x'"), "{error}");
    }

    #[test]
    fn counts_must_be_positive() {
        let within = AgentWithinArgs {
            top: Some(0),
            ..AgentWithinArgs::new("ch_abc123", "cache")
        };
        assert!(
            within
                .validate()
                .unwrap_err()
                .contains("value must be greater than zero")
        );
        let outline = AgentOutlineArgs {
            output: AgentOutputFlags {
                budget: Some(0),
                ..Default::default()
            },
            ..AgentOutlineArgs::new("ch_abc123")
        };
        assert!(
            outline
                .validate()
                .unwrap_err()
                .contains("value must be greater than zero")
        );
        let search = AgentSearchArgs {
            hits_per_conv: Some(0),
            ..AgentSearchArgs::new("cache")
        };
        assert!(search.validate().is_err());
    }

    #[test]
    fn budget_and_no_budget_conflict() {
        let read = AgentReadArgs {
            output: AgentOutputFlags {
                budget: Some(10),
                no_budget: true,
                ..Default::default()
            },
            ..AgentReadArgs::new(["ch_abc123"])
        };
        assert!(read.validate().unwrap_err().contains("cannot be used with"));
    }

    #[test]
    fn queries_and_refs_must_not_be_empty_and_are_trimmed() {
        let error = AgentSearchArgs::new("   ").normalized().unwrap_err();
        assert!(error.contains("value must not be empty"));
        assert_eq!(
            AgentSearchArgs::new("  cache warming ")
                .normalized()
                .unwrap()
                .query,
            "cache warming"
        );
        assert!(AgentReadArgs::new(Vec::<String>::new()).validate().is_err());
        assert!(AgentReadArgs::new([" "]).validate().is_err());
        let read = AgentReadArgs {
            focus: Some(" m2 ".to_string()),
            ..AgentReadArgs::new([" ch_abc123:m1..m3 "])
        }
        .normalized()
        .unwrap();
        assert_eq!(read.refs, vec!["ch_abc123:m1..m3"]);
        assert_eq!(read.focus.as_deref(), Some("m2"));
        assert!(
            AgentOutlineArgs::new("")
                .validate()
                .unwrap_err()
                .contains("value must not be empty")
        );
    }

    #[test]
    fn read_defaults_to_budgeted_minimal_output() {
        let read = AgentReadArgs::new(["ch_abc123"]);
        assert_eq!(read.output, AgentOutputFlags::default());
        assert_eq!(read.output.budget, None);
        assert!(!read.output.no_budget);
        assert_eq!(read.context, 3);
        assert!(read.validate().is_ok());
    }

    #[test]
    fn read_slice_options_validate_arguments() {
        let conflict = AgentReadArgs {
            lines: Some("1..2".parse().unwrap()),
            match_query: Some("needle".to_string()),
            ..AgentReadArgs::new(["ch_abc123:m7"])
        };
        assert!(
            conflict
                .validate()
                .unwrap_err()
                .contains("cannot be used with")
        );

        let missing_match = AgentReadArgs {
            context: 2,
            ..AgentReadArgs::new(["ch_abc123:m7"])
        };
        assert!(missing_match.validate().unwrap_err().contains("required"));

        assert!(
            "4..2"
                .parse::<MessageLineRange>()
                .unwrap_err()
                .contains("line range")
        );
        let reversed = AgentReadArgs {
            lines: Some(MessageLineRange { start: 4, end: 2 }),
            ..AgentReadArgs::new(["ch_abc123:m7"])
        };
        assert!(reversed.validate().unwrap_err().contains("line range"));

        let matching = AgentReadArgs {
            match_query: Some("historical correction".to_string()),
            context: 12,
            ..AgentReadArgs::new(["ch_abc123:m7"])
        };
        assert!(matching.validate().is_ok());
    }

    #[test]
    fn request_normalizes_and_validates_each_variant() {
        let request = AgentRequest::Within(AgentWithinArgs::new(" ch_abc123 ", " cache "));
        assert_eq!(
            request.normalized().unwrap(),
            AgentRequest::Within(AgentWithinArgs::new("ch_abc123", "cache"))
        );
        assert!(
            AgentRequest::Outline(AgentOutlineArgs::new(" "))
                .validate()
                .is_err()
        );
        assert!(
            AgentRequest::Read(AgentReadArgs::new(["ch_abc123"]))
                .validate()
                .is_ok()
        );
        assert!(
            AgentRequest::Search(AgentSearchArgs::new(""))
                .normalized()
                .is_err()
        );
    }
}
