//! The CLI `--plain` stdout format (upstream `display.rs` `PlainFormatter`
//! and the shared entry walk `process_entry` / `process_user_message` /
//! `process_assistant_message` / `process_agent_message`).
//!
//! This is a different format from [`super::ExportFormat::Plain`]:
//! - `You: {text}` and `Claude: {text}` (the assistant is always labeled
//!   `Claude`, also for Pi/OMP sessions), user text filtered by
//!   [`crate::commands::process_command_message`], no markdown rendering;
//! - tool calls as `Claude: {header}` with the body indented by 2 spaces,
//!   formatted for an 80-column width (Bash commands wrap at 74);
//! - tool results as `Tool: <Result>` followed by the content (strings
//!   verbatim, anything else pretty-printed JSON);
//! - thinking as `Thinking: {text}` (top-level assistants only);
//! - subagent and `agent_progress` entries as `  [{id}] User: …` /
//!   `  [{id}] Agent: …`, their tool bodies and results indented by 4;
//! - a blank line after every message that printed something.
//!
//! The terminal sinks (stdout, pager, colors, ANSI ledger) are not ported;
//! the text is returned as a `String`. Malformed JSONL lines are skipped, as
//! upstream did without `--debug`.

use super::ExportOptions;
use crate::claude::{AssistantMessage, ContentBlock, LogEntry, UserContent};
use crate::commands::process_command_message;
use crate::tool_format;
use std::fmt::Write as _;
use std::path::Path;

/// Default content width for plain text output
const PLAIN_CONTENT_WIDTH: usize = 80;

/// Render the conversation at `source` in the CLI `--plain` format.
///
/// `options.show_tools == false` is upstream's `no_tools` (calls and results
/// are omitted entirely; there is no summary mode); `options.show_thinking`
/// shows thinking blocks, subagent entries and `agent_progress` entries.
pub fn plain_display(source: &Path, options: ExportOptions) -> std::io::Result<String> {
    let entries = crate::history::normalized_log_entries(source).map_err(|error| match error {
        crate::error::AppError::Io(error) => error,
        other => std::io::Error::other(other.to_string()),
    })?;
    Ok(plain_display_entries(
        entries.iter().map(|(_, entry)| entry),
        options,
    ))
}

/// A raw transcript line that does not parse as a Claude log entry (what
/// upstream's `--plain`/`--render` reported with `--debug`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayParseError {
    /// 1-based line number in the file (blank lines count).
    pub line_number: usize,
    /// The JSON error text.
    pub error: String,
    /// The line as read.
    pub line: String,
}

/// The raw JSONL lines of `source` that do not parse as a log entry
/// (upstream `display.rs` `process_raw_log_entries`; K11): each non-blank
/// line in file order, with upstream's line number and error text. The
/// caller reports them like upstream with `--debug`: the stderr line
/// `Failed to parse line {line_number}: {error}` at error level and
/// [`crate::debug_log::log_display_error`].
///
/// Pi and OMP files are checked line by line too, as upstream did (their
/// raw records are not Claude log entries, so most lines are reported).
/// Reading errors (including invalid UTF-8) are returned.
pub fn display_parse_errors(source: &Path) -> std::io::Result<Vec<DisplayParseError>> {
    use std::io::BufRead;
    let reader = std::io::BufReader::new(std::fs::File::open(source)?);
    let mut errors = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Err(error) = serde_json::from_str::<LogEntry>(&line) {
            errors.push(DisplayParseError {
                line_number: index + 1,
                error: error.to_string(),
                line,
            });
        }
    }
    Ok(errors)
}

/// [`plain_display`] over already parsed entries (for example the cached
/// entries of an open viewer).
pub fn plain_display_entries<'a>(
    entries: impl IntoIterator<Item = &'a LogEntry>,
    options: ExportOptions,
) -> String {
    let mut formatter = PlainFormatter::default();
    for entry in entries {
        process_entry(
            &mut formatter,
            entry,
            !options.show_tools,
            options.show_thinking,
        );
    }
    formatter.output
}

/// Plain text formatter without formatting or alignment
#[derive(Default)]
struct PlainFormatter {
    output: String,
}

impl PlainFormatter {
    fn format_user_text(&mut self, text: &str) {
        let _ = writeln!(self.output, "You: {}", text);
    }

    fn format_assistant_text(&mut self, text: &str) {
        let _ = writeln!(self.output, "Claude: {}", text);
    }

    fn format_tool_call(&mut self, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, PLAIN_CONTENT_WIDTH);
        let _ = writeln!(self.output, "Claude: {}", formatted.header);
        if let Some(body) = formatted.body {
            for line in body.lines() {
                let _ = writeln!(self.output, "  {}", line);
            }
        }
    }

    fn format_tool_result(&mut self, content: Option<&serde_json::Value>) {
        let _ = writeln!(self.output, "Tool: <Result>");
        let content_str = format_tool_content(content);
        let _ = writeln!(self.output, "{}", content_str);
    }

    fn format_thinking(&mut self, thought: &str) {
        let _ = writeln!(self.output, "Thinking: {}", thought);
    }

    fn end_message(&mut self) {
        let _ = writeln!(self.output);
    }

    fn format_agent_user_text(&mut self, agent_id: &str, text: &str) {
        let _ = writeln!(
            self.output,
            "  [{}] User: {}",
            short_agent_id(agent_id),
            text
        );
    }

    fn format_agent_assistant_text(&mut self, agent_id: &str, text: &str) {
        let _ = writeln!(
            self.output,
            "  [{}] Agent: {}",
            short_agent_id(agent_id),
            text
        );
    }

    fn format_agent_tool_call(&mut self, agent_id: &str, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, PLAIN_CONTENT_WIDTH);
        let _ = writeln!(
            self.output,
            "  [{}] Agent: {}",
            short_agent_id(agent_id),
            formatted.header
        );
        if let Some(body) = formatted.body {
            for line in body.lines() {
                let _ = writeln!(self.output, "    {}", line);
            }
        }
    }

    fn format_agent_tool_result(&mut self, _agent_id: &str, content: Option<&serde_json::Value>) {
        let _ = writeln!(self.output, "    Tool: <Result>");
        let content_str = format_tool_content(content);
        for line in content_str.lines() {
            let _ = writeln!(self.output, "    {}", line);
        }
    }
}

/// Format tool result content to a string
fn format_tool_content(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(value) => {
            if let Some(s) = value.as_str() {
                s.to_string()
            } else if let Ok(formatted) = serde_json::to_string_pretty(value) {
                formatted
            } else {
                "<invalid content>".to_string()
            }
        }
        None => "<no content>".to_string(),
    }
}

/// Create a display ID for subagent entries from a parent_tool_use_id.
fn subagent_display_id(parent_tool_use_id: &str) -> String {
    crate::claude::short_parent_id(parent_tool_use_id)
}

/// Get a truncated agent ID for display (max 7 characters). Sliced by
/// characters; upstream sliced bytes and panicked on non-ASCII ids (W18).
fn short_agent_id(agent_id: &str) -> &str {
    crate::claude::short_agent_id(agent_id)
}

/// Process a log entry using the provided formatter
fn process_entry(
    formatter: &mut PlainFormatter,
    entry: &LogEntry,
    no_tools: bool,
    show_thinking: bool,
) {
    match entry {
        LogEntry::Summary { .. }
        | LogEntry::FileHistorySnapshot { .. }
        | LogEntry::System { .. }
        | LogEntry::CustomTitle { .. }
        | LogEntry::AiTitle { .. }
        | LogEntry::AgentName { .. }
        | LogEntry::PermissionMode { .. }
        | LogEntry::PiMetadata {
            searchable: false, ..
        }
        | LogEntry::Unknown => {
            // Skip metadata entries
        }
        LogEntry::PiMetadata {
            label,
            text,
            searchable: true,
            ..
        } => {
            let content = if text.is_empty() {
                format!("[{label}]")
            } else {
                format!("[{label}] {text}")
            };
            process_user_message(
                formatter,
                &crate::claude::UserMessage {
                    role: "user".to_owned(),
                    content: UserContent::String(content),
                },
                no_tools,
                None,
            );
        }
        LogEntry::Progress { data, .. } => {
            // Handle agent_progress entries (only when show_thinking is enabled)
            if show_thinking && let Some(agent_progress) = crate::claude::parse_agent_progress(data)
            {
                process_agent_message(formatter, &agent_progress, no_tools);
            }
        }
        LogEntry::User {
            message,
            parent_tool_use_id,
            ..
        } => {
            if parent_tool_use_id.is_some() && !show_thinking {
                return;
            }
            process_user_message(formatter, message, no_tools, parent_tool_use_id.as_deref());
        }
        LogEntry::Assistant {
            message,
            parent_tool_use_id,
            ..
        } => {
            if parent_tool_use_id.is_some() && !show_thinking {
                return;
            }
            process_assistant_message(
                formatter,
                message,
                no_tools,
                show_thinking,
                parent_tool_use_id.as_deref(),
            );
        }
    }
}

/// Process a user message using the provided formatter
// Kept verbatim from upstream `display.rs`.
#[allow(clippy::collapsible_match)]
fn process_user_message(
    formatter: &mut PlainFormatter,
    message: &crate::claude::UserMessage,
    no_tools: bool,
    parent_id: Option<&str>,
) {
    let agent_id = parent_id.map(subagent_display_id);

    match &message.content {
        UserContent::String(text) => {
            if let Some(processed) = process_command_message(text) {
                if let Some(ref id) = agent_id {
                    formatter.format_agent_user_text(id, &processed);
                } else {
                    formatter.format_user_text(&processed);
                }
                formatter.end_message();
            }
        }
        UserContent::Blocks(blocks) => {
            let mut printed_content = false;
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        if let Some(processed) = process_command_message(text) {
                            if let Some(ref id) = agent_id {
                                formatter.format_agent_user_text(id, &processed);
                            } else {
                                formatter.format_user_text(&processed);
                            }
                            printed_content = true;
                        }
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        if !no_tools {
                            if let Some(ref id) = agent_id {
                                formatter.format_agent_tool_result(id, content.as_ref());
                            } else {
                                formatter.format_tool_result(content.as_ref());
                            }
                            printed_content = true;
                        }
                    }
                    _ => {}
                }
            }
            if printed_content {
                formatter.end_message();
            }
        }
    }
}

/// Helper struct to categorize assistant message content
struct FormattedMessage<'a> {
    text_blocks: Vec<&'a str>,
    tool_calls: Vec<(&'a str, &'a serde_json::Value)>,
    thinking_steps: Vec<&'a str>,
}

impl<'a> From<&'a AssistantMessage> for FormattedMessage<'a> {
    fn from(msg: &'a AssistantMessage) -> Self {
        let mut text_blocks = Vec::new();
        let mut tool_calls = Vec::new();
        let mut thinking_steps = Vec::new();

        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => text_blocks.push(text.as_str()),
                ContentBlock::ToolUse { name, input, .. } => {
                    tool_calls.push((name.as_str(), input))
                }
                ContentBlock::Thinking { thinking, .. } => thinking_steps.push(thinking.as_str()),
                _ => {}
            }
        }

        Self {
            text_blocks,
            tool_calls,
            thinking_steps,
        }
    }
}

/// Process an assistant message using the provided formatter
fn process_assistant_message(
    formatter: &mut PlainFormatter,
    message: &AssistantMessage,
    no_tools: bool,
    show_thinking: bool,
    parent_id: Option<&str>,
) {
    let formatted = FormattedMessage::from(message);
    let mut printed_content = false;
    let agent_id = parent_id.map(subagent_display_id);

    // Print text blocks
    for text in formatted.text_blocks {
        if text.trim().is_empty() {
            continue;
        }
        if let Some(ref id) = agent_id {
            formatter.format_agent_assistant_text(id, text);
        } else {
            formatter.format_assistant_text(text);
        }
        printed_content = true;
    }

    // Print tool calls
    if !no_tools {
        for (tool_name, tool_input) in formatted.tool_calls {
            if let Some(ref id) = agent_id {
                formatter.format_agent_tool_call(id, tool_name, tool_input);
            } else {
                formatter.format_tool_call(tool_name, tool_input);
            }
            printed_content = true;
        }
    }

    // Print thinking blocks (skip for subagents)
    if show_thinking && agent_id.is_none() {
        for thought in formatted.thinking_steps {
            if thought.is_empty() {
                continue;
            }
            formatter.format_thinking(thought);
            printed_content = true;
        }
    }

    // Only add spacing if we printed something
    if printed_content {
        formatter.end_message();
    }
}

/// Aggregate text content blocks and render them with the caller-specific formatter.
fn render_agent_text_blocks(
    blocks: &[crate::claude::ContentBlock],
    mut format_text: impl FnMut(&str),
) -> bool {
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|block| match block {
            crate::claude::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    if texts.is_empty() {
        return false;
    }

    let combined = texts.join("\n\n");
    format_text(&combined);
    true
}

/// Process an agent progress message using the provided formatter
fn process_agent_message(
    formatter: &mut PlainFormatter,
    agent_progress: &crate::claude::AgentProgressData,
    no_tools: bool,
) {
    use crate::claude::{AgentContent, ContentBlock};

    let agent_id = &agent_progress.agent_id;
    let msg = &agent_progress.message;

    match msg.message_type.as_str() {
        "user" => {
            // User messages in agent context are typically tool results or the initial prompt
            let AgentContent::Blocks(blocks) = &msg.message.content;
            let mut printed = false;

            printed |= render_agent_text_blocks(blocks, |text| {
                formatter.format_agent_user_text(agent_id, text);
            });

            // Tool results
            for block in blocks {
                if let ContentBlock::ToolResult { content, .. } = block
                    && !no_tools
                {
                    formatter.format_agent_tool_result(agent_id, content.as_ref());
                    printed = true;
                }
            }

            if printed {
                formatter.end_message();
            }
        }
        "assistant" => {
            let AgentContent::Blocks(blocks) = &msg.message.content;
            let mut printed = false;

            printed |= render_agent_text_blocks(blocks, |text| {
                formatter.format_agent_assistant_text(agent_id, text);
            });

            // Tool calls
            for block in blocks {
                if let ContentBlock::ToolUse { name, input, .. } = block
                    && !no_tools
                {
                    formatter.format_agent_tool_call(agent_id, name, input);
                    printed = true;
                }
            }

            if printed {
                formatter.end_message();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(path: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    /// Upstream `display.rs` test options: `no_tools: true`,
    /// `show_thinking: false`.
    fn options() -> ExportOptions {
        ExportOptions {
            show_tools: false,
            show_thinking: false,
        }
    }

    fn render(path: &Path, options: &ExportOptions) -> String {
        plain_display(path, *options).unwrap()
    }

    #[test]
    fn normalized_display_preserves_claude_plain_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("claude.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"question\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]}}\n"
            ),
        )
        .unwrap();

        let output = render(&path, &options());
        assert_eq!(output, "You: question\n\nClaude: answer\n\n");
    }

    #[test]
    fn normalized_display_renders_active_pi_branch_and_visibility() {
        let path = fixture("tests/fixtures/pi/v3-branched.jsonl");
        let output = render(&path, &options());
        assert!(output.contains("You: active root question"));
        assert!(output.contains("Claude: root answer"));
        assert!(output.contains("You: [notice] visible custom searchable"));
        assert!(!output.contains("ABANDONED_BRANCH_SENTINEL"));
        assert!(!output.contains("HIDDEN_CUSTOM_SENTINEL"));
        assert!(!output.contains("private reasoning"));
        assert!(!output.contains("tool output searchable"));
        assert!(!output.contains("TOOL_BASE64_SECRET"));

        let visible = ExportOptions {
            show_tools: true,
            show_thinking: true,
        };
        let output = render(&path, &visible);
        assert!(output.contains("Thinking: private reasoning"));
        assert!(output.contains("tool output searchable"));
        assert!(output.contains("bash output searchable"));
    }

    #[test]
    fn normalized_display_renders_active_omp_branch_without_relabeling() {
        let path = fixture("tests/fixtures/omp/v3.jsonl");
        let output = render(&path, &options());
        assert!(output.contains("You: OMP active question"));
        assert!(output.contains("Claude: OMP active answer"));
        assert!(!output.contains("OMP_ABANDONED_SENTINEL"));
        assert!(!output.contains("OMP fixture title"));
    }

    #[test]
    fn normalized_display_skips_malformed_records_without_debug() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pi-malformed.jsonl");
        let fixture =
            std::fs::read_to_string(fixture("tests/fixtures/pi/v3-branched.jsonl")).unwrap();
        std::fs::write(&path, format!("{fixture}not-json\n")).unwrap();

        let output = render(&path, &options());
        assert!(output.contains("active root question"));
        assert!(!output.contains("not-json"));
    }

    // -----------------------------------------------------------------
    // claude-history-core additions: exact layouts of the plain format.
    // -----------------------------------------------------------------

    #[test]
    fn display_parse_errors_report_raw_lines_like_upstream_debug() {
        // K11: upstream process_raw_log_entries reported every non-blank
        // raw line that is not a log entry, with its 1-based line number.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("malformed.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"type":"user","message":{"role":"user","content":"ok"}}"#,
                "{not json",
                "",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"fine"}]}}"#,
                r#"{"type":"user"}"#,
                "   ",
                "[1,2]",
            ]
            .join("\n"),
        )
        .unwrap();

        let errors = display_parse_errors(&path).unwrap();
        let lines = errors
            .iter()
            .map(|error| (error.line_number, error.line.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            [(2, "{not json"), (5, r#"{"type":"user"}"#), (7, "[1,2]")]
        );
        for error in &errors {
            let expected = serde_json::from_str::<LogEntry>(&error.line)
                .unwrap_err()
                .to_string();
            assert_eq!(error.error, expected);
        }
        // The rendering itself skips them.
        let output = render(&path, &options());
        assert!(output.contains("You: ok") && output.contains("Claude: fine"));

        // Pi/OMP files are checked on their raw lines, as upstream did:
        // exactly the raw records that are not log entries are reported.
        for name in [
            "tests/fixtures/omp/v3.jsonl",
            "tests/fixtures/pi/v3-branched.jsonl",
        ] {
            let raw = std::fs::read_to_string(fixture(name)).unwrap();
            let expected = raw
                .lines()
                .enumerate()
                .filter(|(_, line)| {
                    !line.trim().is_empty() && serde_json::from_str::<LogEntry>(line).is_err()
                })
                .map(|(index, _)| index + 1)
                .collect::<Vec<_>>();
            let reported = display_parse_errors(&fixture(name))
                .unwrap()
                .into_iter()
                .map(|error| error.line_number)
                .collect::<Vec<_>>();
            assert_eq!(reported, expected, "{name}");
        }
        let clean = directory.path().join("clean.jsonl");
        std::fs::write(
            &clean,
            r#"{"type":"user","message":{"role":"user","content":"ok"}}"#,
        )
        .unwrap();
        assert!(display_parse_errors(&clean).unwrap().is_empty());
        assert_eq!(
            display_parse_errors(&directory.path().join("missing.jsonl"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn missing_input_is_reported_as_not_found() {
        let directory = tempfile::tempdir().unwrap();
        let error = plain_display(&directory.path().join("missing.jsonl"), options()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn tools_thinking_and_subagents_use_cli_plain_layout() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("claude.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<command-name>/review</command-name><command-args>pr 7</command-args>"},{"type":"text","text":"<local-command-caveat>c</local-command-caveat>"}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"plan","signature":"s"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo one\necho two"}},{"type":"text","text":"done"}]}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"one\ntwo"}]}}"#,
                "\n",
                r#"{"type":"assistant","parent_tool_use_id":"toolu_parentXYZ","message":{"role":"assistant","content":[{"type":"text","text":"sub"},{"type":"tool_use","id":"toolu_2","name":"Edit","input":{"file_path":"a.rs","old_string":"x","new_string":"y"}}]}}"#,
                "\n",
                r#"{"type":"user","parent_tool_use_id":"toolu_parentXYZ","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_2","content":{"ok":true}}]}}"#,
                "\n",
                r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef123","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"},{"type":"tool_use","id":"toolu_3","name":"Read","input":{"file_path":"b.rs"}}]}}}}"#,
                "\n",
            ),
        )
        .unwrap();

        let all = ExportOptions {
            show_tools: true,
            show_thinking: true,
        };
        assert_eq!(
            plain_display(&path, all).unwrap(),
            concat!(
                "You: /review pr 7\n",
                "\n",
                "Claude: done\n",
                "Claude: Bash: echo one\n",
                "  echo two\n",
                "Thinking: plan\n",
                "\n",
                "Tool: <Result>\n",
                "one\ntwo\n",
                "\n",
                "  [parentX] Agent: sub\n",
                "  [parentX] Agent: Edit: a.rs\n",
                "    - x\n",
                "    + y\n",
                "\n",
                "    Tool: <Result>\n",
                "    {\n",
                "      \"ok\": true\n",
                "    }\n",
                "\n",
                "  [agent-a] Agent: working\n",
                "  [agent-a] Agent: Read: b.rs\n",
                "\n",
            )
        );

        // Without tools and thinking: no calls/results, no subagents or
        // agent progress, and a tool-result-only user message prints nothing.
        assert_eq!(
            plain_display(&path, options()).unwrap(),
            "You: /review pr 7\n\nClaude: done\n\n"
        );
    }

    #[test]
    fn plain_display_entries_matches_file_rendering() {
        let path = fixture("tests/fixtures/pi/v3-branched.jsonl");
        let entries = crate::history::normalized_log_entries(&path).unwrap();
        let all = ExportOptions {
            show_tools: true,
            show_thinking: true,
        };
        assert_eq!(
            plain_display_entries(entries.iter().map(|(_, entry)| entry), all),
            plain_display(&path, all).unwrap()
        );
    }
}
