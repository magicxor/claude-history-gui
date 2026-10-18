//! The `agent` subcommand's command line (K15): upstream's clap surface of
//! `claude-history agent search|within|read|outline` (`cli.rs:76-282`) as a
//! hand-written parser, so the GUI binary can serve
//! `claude-history-gui agent ...` headlessly with the same flags, aliases,
//! value names, help texts and error texts. A parse error is a
//! [`LaunchArgsError`] (exit code 2, like clap).

use crate::agent::args::{
    AgentOutlineArgs, AgentOutputFlags, AgentReadArgs, AgentRequest, AgentSearchArgs,
    AgentSearchModeArgs, AgentWithinArgs, DEFAULT_READ_CONTEXT, TimeRangeArgs,
};
use crate::agent::protocol::MessageLineRange;
use crate::search::mode::SearchMode;
use crate::settings::LaunchArgsError;
use crate::time_filter::TimePoint;
use std::collections::HashSet;
use std::ffi::OsString;

/// The program name the help texts use.
const PROGRAM: &str = "claude-history-gui";

/// A parsed `agent` command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentCli {
    /// Run this request (already validated).
    Request(AgentRequest),
    /// `--help` / `help`: print this text to stdout and exit 0.
    Help(String),
}

/// Parse the arguments that follow `agent` (for example
/// `["search", "cache warming", "--top", "7"]`).
pub fn parse_agent_args<I, T>(args: I) -> Result<AgentCli, LaunchArgsError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into()
                .into_string()
                .map_err(|_| error("invalid UTF-8 was detected in one or more arguments"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some((command, rest)) = args.split_first() else {
        return Err(error(format!(
            "'{PROGRAM} agent' requires a subcommand but one was not provided\n  \
             [subcommands: search, within, read, outline, help]"
        )));
    };
    match command.as_str() {
        "-h" | "--help" => Ok(AgentCli::Help(agent_help())),
        "help" => match rest.first().map(String::as_str) {
            None => Ok(AgentCli::Help(agent_help())),
            Some(name) => SUBCOMMANDS
                .iter()
                .find(|spec| spec.name == name)
                .map(|spec| AgentCli::Help(subcommand_help(spec)))
                .ok_or_else(|| error(format!("unrecognized subcommand '{name}'"))),
        },
        name => {
            let spec = SUBCOMMANDS
                .iter()
                .find(|spec| spec.name == name)
                .ok_or_else(|| {
                    if name.starts_with('-') {
                        error(format!("unexpected argument '{name}' found"))
                    } else {
                        error(format!("unrecognized subcommand '{name}'"))
                    }
                })?;
            parse_subcommand(spec, rest)
        }
    }
}

// ---------------------------------------------------------------------------
// Specification (upstream doc comments are the help texts, ADD E4)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct OptionSpec {
    long: &'static str,
    /// Value name (`TOP`), or `None` for a flag.
    value: Option<&'static str>,
    help: &'static str,
}

const fn flag(long: &'static str, help: &'static str) -> OptionSpec {
    OptionSpec {
        long,
        value: None,
        help,
    }
}

const fn valued(long: &'static str, value: &'static str, help: &'static str) -> OptionSpec {
    OptionSpec {
        long,
        value: Some(value),
        help,
    }
}

struct PositionalSpec {
    name: &'static str,
    help: &'static str,
    /// Takes one or more values (`<REFS>...`).
    multiple: bool,
}

struct SubcommandSpec {
    name: &'static str,
    about: &'static str,
    positionals: &'static [PositionalSpec],
    options: &'static [OptionSpec],
}

const MODE_OPTIONS: [OptionSpec; 5] = [
    valued(
        "mode",
        "MODE",
        "Search mode for this invocation [possible values: lexical, semantic, exact, hybrid]",
    ),
    flag("lexical", "Alias for --mode lexical"),
    flag("semantic", "Alias for --mode semantic"),
    flag("exact", "Alias for --mode exact"),
    flag(
        "hybrid",
        "Alias for --mode hybrid. Combine lexical and semantic message ranking",
    ),
];

const BUDGET: OptionSpec = valued("budget", "BUDGET", "Output budget in Unicode characters");
const NO_BUDGET: OptionSpec = flag("no-budget", "Disable output budgeting");

const SEARCH_OPTIONS: &[OptionSpec] = &[
    valued(
        "top",
        "TOP",
        "Maximum conversations, or message hits with --flat",
    ),
    BUDGET,
    NO_BUDGET,
    flag(
        "flat",
        "Rank message hits across conversations; --top counts hits",
    ),
    valued(
        "hits-per-conv",
        "HITS_PER_CONV",
        "Maximum evidence hits to show per conversation in grouped output",
    ),
    flag(
        "all-hits",
        "Disable duplicate suppression inside grouped conversation results",
    ),
    flag("local", "Search only the current workspace"),
    flag("all", "Search all workspaces"),
    MODE_OPTIONS[0],
    MODE_OPTIONS[1],
    MODE_OPTIONS[2],
    MODE_OPTIONS[3],
    MODE_OPTIONS[4],
    valued(
        "since",
        "WHEN",
        "Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)",
    ),
    valued("after", "WHEN", "Alias for --since"),
    valued(
        "before",
        "WHEN",
        "Only conversations older than this duration or date",
    ),
];

const WITHIN_OPTIONS: &[OptionSpec] = &[
    valued("top", "TOP", "Maximum number of results"),
    BUDGET,
    NO_BUDGET,
    MODE_OPTIONS[0],
    MODE_OPTIONS[1],
    MODE_OPTIONS[2],
    MODE_OPTIONS[3],
    MODE_OPTIONS[4],
];

const OUTPUT_OPTIONS: [OptionSpec; 6] = [
    BUDGET,
    NO_BUDGET,
    flag("tools", "Include tool calls"),
    flag("tool-results", "Include tool results"),
    flag("thinking", "Include thinking blocks"),
    flag("subagents", "Include subagent internals"),
];

const READ_OPTIONS: &[OptionSpec] = &[
    valued(
        "anchor",
        "ANCHOR",
        "Durable message anchor to read within one conversation",
    ),
    valued(
        "focus",
        "FOCUS",
        "Message or range to prioritize when budgeted output is truncated",
    ),
    valued(
        "lines",
        "START..END",
        "Inclusive content line range within one message",
    ),
    valued(
        "match",
        "MATCH_QUERY",
        "Find text within one message and return matching lines with context",
    ),
    valued(
        "context",
        "CONTEXT",
        "Content lines to include before and after each match [default: 3]",
    ),
    OUTPUT_OPTIONS[0],
    OUTPUT_OPTIONS[1],
    OUTPUT_OPTIONS[2],
    OUTPUT_OPTIONS[3],
    OUTPUT_OPTIONS[4],
    OUTPUT_OPTIONS[5],
];

const SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "search",
        about: "Search transcript content across all conversations (known UUID? use outline/read/within)",
        positionals: &[PositionalSpec {
            name: "QUERY",
            help: "Search query",
            multiple: false,
        }],
        options: SEARCH_OPTIONS,
    },
    SubcommandSpec {
        name: "within",
        about: "Search within one conversation",
        positionals: &[
            PositionalSpec {
                name: "CONVERSATION",
                help: "Conversation: ch_ handle, full UUID, or UUID-based session basename/filename",
                multiple: false,
            },
            PositionalSpec {
                name: "QUERY",
                help: "Search query",
                multiple: false,
            },
        ],
        options: WITHIN_OPTIONS,
    },
    SubcommandSpec {
        name: "read",
        about: "Read transcript message ranges",
        positionals: &[PositionalSpec {
            name: "REFS",
            help: "ch_ handles, full UUIDs, or UUID-based session basenames/filenames, optionally :mN..mM",
            multiple: true,
        }],
        options: READ_OPTIONS,
    },
    SubcommandSpec {
        name: "outline",
        about: "Outline a conversation transcript",
        positionals: &[PositionalSpec {
            name: "CONVERSATION",
            help: "Conversation to outline: ch_ handle, full UUID, or UUID-based session basename/filename",
            multiple: false,
        }],
        options: &OUTPUT_OPTIONS,
    },
];

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

/// `claude-history-gui agent --help`.
fn agent_help() -> String {
    let mut text = format!(
        "Run agent-oriented search and transcript commands\n\nUsage: {PROGRAM} agent <COMMAND>\n\nCommands:\n"
    );
    let width = SUBCOMMANDS
        .iter()
        .map(|spec| spec.name.len())
        .max()
        .unwrap_or(0)
        .max("help".len());
    for (name, about) in SUBCOMMANDS
        .iter()
        .map(|spec| (spec.name, spec.about))
        .chain([(
            "help",
            "Print this message or the help of the given subcommand(s)",
        )])
    {
        text.push_str(&format!("  {name:width$}  {about}\n"));
    }
    text.push_str("\nOptions:\n  -h, --help  Print help\n");
    text
}

/// `claude-history-gui agent <name> --help`, laid out like clap.
fn subcommand_help(spec: &SubcommandSpec) -> String {
    let positionals = spec
        .positionals
        .iter()
        .map(|positional| {
            if positional.multiple {
                format!("<{}>...", positional.name)
            } else {
                format!("<{}>", positional.name)
            }
        })
        .collect::<Vec<_>>();
    let mut text = format!(
        "{}\n\nUsage: {PROGRAM} agent {} [OPTIONS] {}\n\nArguments:\n",
        spec.about,
        spec.name,
        positionals.join(" ")
    );
    let width = positionals.iter().map(String::len).max().unwrap_or(0);
    for (usage, positional) in positionals.iter().zip(spec.positionals) {
        text.push_str(&format!("  {usage:width$}  {}\n", positional.help));
    }
    let option_usage = |option: &OptionSpec| match option.value {
        Some(value) => format!("--{} <{value}>", option.long),
        None => format!("--{}", option.long),
    };
    let width = spec
        .options
        .iter()
        .map(|option| option_usage(option).len() + 4)
        .chain(["-h, --help".len()])
        .max()
        .unwrap_or(0);
    text.push_str("\nOptions:\n");
    for option in spec.options {
        let usage = format!("    {}", option_usage(option));
        text.push_str(&format!("  {usage:width$}  {}\n", option.help));
    }
    text.push_str(&format!("  {:width$}  Print help\n", "-h, --help"));
    text
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn error(message: impl Into<String>) -> LaunchArgsError {
    LaunchArgsError(message.into())
}

/// The options and positionals of one subcommand invocation.
#[derive(Default)]
struct Matches {
    values: Vec<(&'static str, String)>,
    flags: HashSet<&'static str>,
    positionals: Vec<String>,
}

impl Matches {
    fn value(&self, long: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| *name == long)
            .map(|(_, value)| value.as_str())
    }

    fn flag(&self, long: &str) -> bool {
        self.flags.contains(long)
    }
}

fn parse_subcommand(spec: &SubcommandSpec, args: &[String]) -> Result<AgentCli, LaunchArgsError> {
    let mut matches = Matches::default();
    let mut seen = HashSet::new();
    let mut only_positionals = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if only_positionals || !arg.starts_with('-') || arg == "-" {
            matches.positionals.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_positionals = true;
            continue;
        }
        if arg == "-h" || arg == "--help" {
            return Ok(AgentCli::Help(subcommand_help(spec)));
        }
        let Some(long) = arg.strip_prefix("--") else {
            return Err(error(format!("unexpected argument '{arg}' found")));
        };
        let (name, inline) = match long.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (long, None),
        };
        let option = spec
            .options
            .iter()
            .find(|option| option.long == name)
            .ok_or_else(|| error(format!("unexpected argument '--{name}' found")))?;
        let display = match option.value {
            Some(value) => format!("--{} <{value}>", option.long),
            None => format!("--{}", option.long),
        };
        if !seen.insert(option.long) {
            return Err(error(format!(
                "the argument '{display}' cannot be used multiple times"
            )));
        }
        match option.value {
            None => {
                if inline.is_some() {
                    return Err(error(format!(
                        "unexpected value '{}' for '{display}' found; no more were expected",
                        inline.unwrap_or_default()
                    )));
                }
                matches.flags.insert(option.long);
            }
            Some(_) => {
                let value = match inline {
                    Some(value) => value,
                    None => iter.next().cloned().ok_or_else(|| {
                        error(format!(
                            "a value is required for '{display}' but none was supplied"
                        ))
                    })?,
                };
                matches.values.push((option.long, value));
            }
        }
    }

    let required = spec
        .positionals
        .iter()
        .filter(|positional| !positional.multiple)
        .count()
        .max(1);
    if matches.positionals.len() < required {
        let missing = spec.positionals[matches.positionals.len().min(spec.positionals.len() - 1)..]
            .iter()
            .map(|positional| {
                if positional.multiple {
                    format!("<{}>...", positional.name)
                } else {
                    format!("<{}>", positional.name)
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        return Err(error(format!(
            "the following required arguments were not provided: {missing}"
        )));
    }
    let takes_many = spec
        .positionals
        .iter()
        .any(|positional| positional.multiple);
    if !takes_many && matches.positionals.len() > spec.positionals.len() {
        return Err(error(format!(
            "unexpected argument '{}' found",
            matches.positionals[spec.positionals.len()]
        )));
    }

    let request = match spec.name {
        "search" => AgentRequest::Search(search_args(&matches)?),
        "within" => AgentRequest::Within(within_args(&matches)?),
        "read" => AgentRequest::Read(read_args(&matches)?),
        _ => AgentRequest::Outline(outline_args(&matches)?),
    };
    let request = request.normalized().map_err(error)?;
    Ok(AgentCli::Request(request))
}

/// clap `value_parser = non_zero_usize` (`cli.rs:495-504`).
fn positive(
    matches: &Matches,
    long: &str,
    value_name: &str,
) -> Result<Option<usize>, LaunchArgsError> {
    let Some(value) = matches.value(long) else {
        return Ok(None);
    };
    let invalid = |reason: String| {
        error(format!(
            "invalid value '{value}' for '--{long} <{value_name}>': {reason}"
        ))
    };
    let parsed = value
        .parse::<usize>()
        .map_err(|_| invalid(format!("invalid positive integer: {value}")))?;
    if parsed == 0 {
        return Err(invalid("value must be greater than zero".to_string()));
    }
    Ok(Some(parsed))
}

fn time_point(matches: &Matches, long: &str) -> Result<Option<TimePoint>, LaunchArgsError> {
    matches
        .value(long)
        .map(|value| {
            value.parse::<TimePoint>().map_err(|err| {
                error(format!(
                    "invalid value '{value}' for '--{long} <WHEN>': {err}"
                ))
            })
        })
        .transpose()
}

fn search_mode(matches: &Matches) -> Result<AgentSearchModeArgs, LaunchArgsError> {
    let mode = matches
        .value("mode")
        .map(|value| {
            value.parse::<SearchMode>().map_err(|_| {
                error(format!(
                    "invalid value '{value}' for '--mode <MODE>'\n  [possible values: lexical, semantic, exact, hybrid]"
                ))
            })
        })
        .transpose()?;
    Ok(AgentSearchModeArgs {
        mode,
        lexical: matches.flag("lexical"),
        semantic: matches.flag("semantic"),
        exact: matches.flag("exact"),
        hybrid: matches.flag("hybrid"),
    })
}

fn search_args(matches: &Matches) -> Result<AgentSearchArgs, LaunchArgsError> {
    Ok(AgentSearchArgs {
        query: matches.positionals[0].clone(),
        top: positive(matches, "top", "TOP")?,
        budget: positive(matches, "budget", "BUDGET")?,
        no_budget: matches.flag("no-budget"),
        flat: matches.flag("flat"),
        hits_per_conv: positive(matches, "hits-per-conv", "HITS_PER_CONV")?,
        all_hits: matches.flag("all-hits"),
        local: matches.flag("local"),
        all: matches.flag("all"),
        search_mode: search_mode(matches)?,
        time: TimeRangeArgs {
            since: time_point(matches, "since")?,
            after: time_point(matches, "after")?,
            before: time_point(matches, "before")?,
        },
    })
}

fn within_args(matches: &Matches) -> Result<AgentWithinArgs, LaunchArgsError> {
    Ok(AgentWithinArgs {
        conversation: matches.positionals[0].clone(),
        query: matches.positionals[1].clone(),
        top: positive(matches, "top", "TOP")?,
        budget: positive(matches, "budget", "BUDGET")?,
        no_budget: matches.flag("no-budget"),
        search_mode: search_mode(matches)?,
    })
}

fn output_flags(matches: &Matches) -> Result<AgentOutputFlags, LaunchArgsError> {
    Ok(AgentOutputFlags {
        budget: positive(matches, "budget", "BUDGET")?,
        no_budget: matches.flag("no-budget"),
        tools: matches.flag("tools"),
        tool_results: matches.flag("tool-results"),
        thinking: matches.flag("thinking"),
        subagents: matches.flag("subagents"),
    })
}

fn read_args(matches: &Matches) -> Result<AgentReadArgs, LaunchArgsError> {
    let lines = matches
        .value("lines")
        .map(|value| {
            value.parse::<MessageLineRange>().map_err(|err| {
                error(format!(
                    "invalid value '{value}' for '--lines <START..END>': {err}"
                ))
            })
        })
        .transpose()?;
    let match_query = matches.value("match").map(str::to_string);
    if lines.is_some() && match_query.is_some() {
        return Err(error(
            "the argument '--lines <START..END>' cannot be used with '--match <MATCH_QUERY>'",
        ));
    }
    // clap `requires = "match_query"`: an explicit --context needs --match.
    let context = match matches.value("context") {
        None => DEFAULT_READ_CONTEXT,
        Some(_) if match_query.is_none() => {
            return Err(error(
                "the following required arguments were not provided: --match <MATCH_QUERY> \
                 (required by --context)",
            ));
        }
        Some(value) => value.parse::<usize>().map_err(|err| {
            error(format!(
                "invalid value '{value}' for '--context <CONTEXT>': {err}"
            ))
        })?,
    };
    Ok(AgentReadArgs {
        refs: matches.positionals.clone(),
        anchor: matches.value("anchor").map(str::to_string),
        focus: matches.value("focus").map(str::to_string),
        lines,
        match_query,
        context,
        output: output_flags(matches)?,
    })
}

fn outline_args(matches: &Matches) -> Result<AgentOutlineArgs, LaunchArgsError> {
    Ok(AgentOutlineArgs {
        conversation: matches.positionals[0].clone(),
        output: output_flags(matches)?,
    })
}

#[cfg(test)]
mod tests {
    //! Upstream `cli.rs` agent parser tests, ported (`cli.rs:540-1160`).
    use super::*;

    fn request(args: &[&str]) -> AgentRequest {
        match parse_agent_args(args.iter().copied()) {
            Ok(AgentCli::Request(request)) => request,
            other => panic!("{args:?} -> {other:?}"),
        }
    }

    fn fail(args: &[&str]) -> String {
        match parse_agent_args(args.iter().copied()) {
            Err(error) => error.0,
            other => panic!("{args:?} should fail, got {other:?}"),
        }
    }

    fn help(args: &[&str]) -> String {
        match parse_agent_args(args.iter().copied()) {
            Ok(AgentCli::Help(text)) => text,
            other => panic!("{args:?} should print help, got {other:?}"),
        }
    }

    #[test]
    fn agent_search_captures_time_range_flags() {
        let AgentRequest::Search(search) = request(&[
            "search",
            "cache warming",
            "--since",
            "2d",
            "--before",
            "2026-07-20",
        ]) else {
            panic!("search expected");
        };
        assert!(search.time.since.is_some());
        assert!(search.time.after.is_none());
        assert!(search.time.before.is_some());
    }

    #[test]
    fn since_and_after_are_mutually_exclusive() {
        assert!(
            fail(&["search", "needle", "--since", "2d", "--after", "3d"])
                .contains("cannot be used with")
        );
        assert!(fail(&["search", "needle", "--since", "30x"]).contains("unknown unit 'x'"));
    }

    #[test]
    fn agent_search_captures_query_top_scope_and_mode_override() {
        let AgentRequest::Search(search) = request(&[
            "search",
            "cache warming",
            "--hybrid",
            "--top",
            "7",
            "--local",
        ]) else {
            panic!("search expected");
        };
        assert_eq!(search.query, "cache warming");
        assert_eq!(search.top, Some(7));
        assert!(!search.flat);
        assert_eq!(search.hits_per_conv, None);
        assert!(!search.all_hits);
        assert!(search.local);
        assert!(!search.all);
        assert_eq!(search.mode_override(), Some(SearchMode::Hybrid));
    }

    #[test]
    fn agent_search_defaults_to_global_scope_top_ten_and_config_mode() {
        let AgentRequest::Search(search) = request(&["search", "cache"]) else {
            panic!("search expected");
        };
        assert_eq!(search, AgentSearchArgs::new("cache"));
    }

    #[test]
    fn agent_search_captures_browsing_flags() {
        let AgentRequest::Search(search) = request(&[
            "search",
            "cache",
            "--flat",
            "--hits-per-conv",
            "5",
            "--all-hits",
        ]) else {
            panic!("search expected");
        };
        assert!(search.flat);
        assert_eq!(search.hits_per_conv, Some(5));
        assert!(search.all_hits);
        assert!(fail(&["search", "cache", "--local", "--all"]).contains("cannot be used with"));
    }

    #[test]
    fn agent_search_help_explains_grouped_and_flat_top_semantics() {
        let text = help(&["search", "--help"]);
        assert!(text.contains("Maximum conversations, or message hits with --flat"));
        assert!(text.contains("Rank message hits across conversations; --top counts hits"));
        assert!(text.contains("Combine lexical and semantic message ranking"));
        assert!(text.contains(
            "Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)"
        ));
        assert!(text.contains("Usage: claude-history-gui agent search [OPTIONS] <QUERY>"));
        assert_eq!(help(&["help", "search"]), text);
        assert_eq!(help(&["search", "x", "-h"]), text);
    }

    #[test]
    fn agent_help_documents_anchor_reads_and_subcommands() {
        let read_help = help(&["read", "--help"]);
        assert!(read_help.contains("--anchor"));
        assert!(read_help.contains("Durable message anchor"));
        assert!(read_help.contains("Output budget in Unicode characters"));
        assert!(!read_help.contains("approximate tokens"));
        assert!(read_help.contains("<REFS>..."));
        let agent = help(&["--help"]);
        for line in [
            "Usage: claude-history-gui agent <COMMAND>",
            "search   Search transcript content across all conversations (known UUID? use outline/read/within)",
            "within   Search within one conversation",
            "read     Read transcript message ranges",
            "outline  Outline a conversation transcript",
        ] {
            assert!(agent.contains(line), "{line}\n{agent}");
        }
        assert_eq!(help(&["help"]), agent);
    }

    #[test]
    fn agent_within_rejects_search_browsing_flags() {
        assert!(
            fail(&["within", "ch_abc123", "cache", "--flat"])
                .contains("unexpected argument '--flat'")
        );
    }

    #[test]
    fn agent_within_captures_ref_query_top_and_mode_override() {
        let AgentRequest::Within(within) =
            request(&["within", "ch_abc123", "cache", "--semantic", "--top", "4"])
        else {
            panic!("within expected");
        };
        assert_eq!(within.conversation, "ch_abc123");
        assert_eq!(within.query, "cache");
        assert_eq!(within.top, Some(4));
        assert_eq!(within.mode_override(), Some(SearchMode::Semantic));
    }

    #[test]
    fn agent_read_captures_refs_and_display_options() {
        let AgentRequest::Read(read) = request(&[
            "read",
            "ch_abc123:m1..m3",
            "ch_def456:m4",
            "--focus",
            "m2",
            "--budget",
            "1234",
            "--tools",
            "--tool-results",
            "--thinking",
            "--subagents",
        ]) else {
            panic!("read expected");
        };
        assert_eq!(read.refs, vec!["ch_abc123:m1..m3", "ch_def456:m4"]);
        assert_eq!(read.anchor, None);
        assert_eq!(read.focus.as_deref(), Some("m2"));
        assert_eq!(read.lines, None);
        assert_eq!(read.match_query, None);
        assert_eq!(read.context, 3);
        assert_eq!(read.output.budget, Some(1234));
        assert!(!read.output.no_budget);
        assert!(read.output.tools);
        assert!(read.output.tool_results);
        assert!(read.output.thinking);
        assert!(read.output.subagents);
    }

    #[test]
    fn agent_read_defaults_to_budgeted_minimal_output() {
        let AgentRequest::Read(read) = request(&["read", "ch_abc123"]) else {
            panic!("read expected");
        };
        assert_eq!(read, AgentReadArgs::new(["ch_abc123"]));
        assert!(
            fail(&["read"])
                .contains("the following required arguments were not provided: <REFS>...")
        );
    }

    #[test]
    fn agent_read_captures_message_slice_options() {
        let AgentRequest::Read(lines) = request(&["read", "ch_abc123:m7", "--lines", "40..120"])
        else {
            panic!("read expected");
        };
        assert_eq!(
            lines.lines,
            Some(MessageLineRange {
                start: 40,
                end: 120
            })
        );
        let AgentRequest::Read(matching) = request(&[
            "read",
            "ch_abc123:m7",
            "--match",
            "historical correction",
            "--context",
            "12",
        ]) else {
            panic!("read expected");
        };
        assert_eq!(
            matching.match_query.as_deref(),
            Some("historical correction")
        );
        assert_eq!(matching.context, 12);
    }

    #[test]
    fn agent_read_slice_options_validate_arguments() {
        assert!(
            fail(&[
                "read",
                "ch_abc123:m7",
                "--lines",
                "1..2",
                "--match",
                "needle"
            ])
            .contains("cannot be used with")
        );
        assert!(fail(&["read", "ch_abc123:m7", "--context", "2"]).contains("required"));
        // An explicit default context still requires --match.
        assert!(fail(&["read", "ch_abc123:m7", "--context", "3"]).contains("required"));
        assert!(fail(&["read", "ch_abc123:m7", "--lines", "4..2"]).contains("line range"));
    }

    #[test]
    fn agent_outline_captures_ref_and_display_options() {
        let AgentRequest::Outline(outline) = request(&[
            "outline",
            "ch_abc123",
            "--no-budget",
            "--tools",
            "--tool-results",
            "--thinking",
            "--subagents",
        ]) else {
            panic!("outline expected");
        };
        assert_eq!(outline.conversation, "ch_abc123");
        assert_eq!(outline.output.budget, None);
        assert!(outline.output.no_budget);
        assert!(outline.output.tools);
        assert!(outline.output.tool_results);
        assert!(outline.output.thinking);
        assert!(outline.output.subagents);
    }

    #[test]
    fn agent_mode_value_and_legacy_aliases_share_one_override() {
        let AgentRequest::Search(canonical) = request(&["search", "cache", "--mode", "hybrid"])
        else {
            panic!("search expected");
        };
        assert_eq!(canonical.mode_override(), Some(SearchMode::Hybrid));
        let AgentRequest::Within(legacy) = request(&["within", "ch_abc123", "cache", "--hybrid"])
        else {
            panic!("within expected");
        };
        assert_eq!(legacy.mode_override(), Some(SearchMode::Hybrid));
        let AgentRequest::Search(inline) = request(&["search", "cache", "--mode=exact"]) else {
            panic!("search expected");
        };
        assert_eq!(inline.mode_override(), Some(SearchMode::Exact));
        assert!(fail(&["search", "cache", "--mode", "fuzzy"]).contains("possible values"));
    }

    #[test]
    fn agent_mode_flags_conflict() {
        assert!(
            fail(&["search", "cache", "--mode", "semantic", "--hybrid"])
                .contains("cannot be used with")
        );
        assert!(
            fail(&["search", "cache", "--semantic", "--hybrid"]).contains("cannot be used with")
        );
    }

    #[test]
    fn agent_budget_and_no_budget_conflict() {
        assert!(
            fail(&["read", "ch_abc123", "--budget", "10", "--no-budget"])
                .contains("cannot be used with")
        );
    }

    #[test]
    fn agent_numbers_must_be_positive_integers() {
        assert!(
            fail(&["within", "ch_abc123", "cache", "--top", "0"])
                .contains("value must be greater than zero")
        );
        assert!(
            fail(&["outline", "ch_abc123", "--budget", "0"])
                .contains("value must be greater than zero")
        );
        assert_eq!(
            fail(&["search", "cache", "--top", "x"]),
            "invalid value 'x' for '--top <TOP>': invalid positive integer: x"
        );
    }

    #[test]
    fn agent_query_must_not_be_empty() {
        assert!(fail(&["search", "   "]).contains("value must not be empty"));
        // Queries are trimmed like clap's non_empty_string did.
        let AgentRequest::Search(search) = request(&["search", "  padded  "]) else {
            panic!("search expected");
        };
        assert_eq!(search.query, "padded");
    }

    #[test]
    fn missing_unknown_and_repeated_arguments_are_clap_style_errors() {
        assert!(fail(&[]).contains("requires a subcommand"));
        assert_eq!(fail(&["find", "x"]), "unrecognized subcommand 'find'");
        assert_eq!(
            fail(&["search"]),
            "the following required arguments were not provided: <QUERY>"
        );
        assert_eq!(
            fail(&["within", "ch_x"]),
            "the following required arguments were not provided: <QUERY>"
        );
        assert_eq!(fail(&["search", "a", "b"]), "unexpected argument 'b' found");
        assert_eq!(
            fail(&["search", "a", "--top", "1", "--top", "2"]),
            "the argument '--top <TOP>' cannot be used multiple times"
        );
        assert_eq!(
            fail(&["search", "a", "--top"]),
            "a value is required for '--top <TOP>' but none was supplied"
        );
        // `--` ends the options: a query may start with a dash.
        let AgentRequest::Search(search) = request(&["search", "--", "-dash"]) else {
            panic!("search expected");
        };
        assert_eq!(search.query, "-dash");
    }
}
