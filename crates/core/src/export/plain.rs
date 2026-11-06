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
//! - tool results as `Tool: <Result>` followed by the content formatted by
//!   [`tool_format::format_tool_result`] (block text, `[image: image/png]`
//!   style placeholders, `Error: ...` for tool errors, other JSON
//!   pretty-printed, upstream 6afae1c);
//! - thinking as `Thinking: {text}` (top-level assistants only);
//! - subagent and `agent_progress` entries as `  [{id}] User: …` /
//!   `  [{id}] Agent: …`, their tool bodies and results indented by 4;
//! - a blank line after every message that printed something.
//!
//! The terminal sinks (stdout, pager, colors, ANSI ledger) are not ported;
//! the text is returned as a `String`. It walks the entries of a decoded
//! transcript ([`crate::history::view::entries`], upstream 9597669), so
//! malformed records are skipped, as upstream did without `--debug`.

use super::ExportOptions;
use crate::commands::process_command_message;
use crate::history::model::{Message, Part, Role, Transcript};
use crate::history::view::{self, Entry, EntryKind};
use crate::tool_format;
use std::fmt::Write as _;

/// Default content width for plain text output
const PLAIN_CONTENT_WIDTH: usize = 80;

/// Render `transcript` in the CLI `--plain` format.
///
/// `options.show_tools == false` is upstream's `no_tools` (calls and results
/// are omitted entirely; there is no summary mode); `options.show_thinking`
/// shows thinking blocks, subagent entries and `agent_progress` entries.
pub fn plain_display(transcript: &Transcript, options: ExportOptions) -> String {
    plain_display_entries(view::entries(transcript), options)
}

/// A record of a transcript that could not be decoded (what upstream's
/// `--plain`/`--render` reported with `--debug`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayParseError {
    /// 1-based line number in the file (blank lines count).
    pub line_number: usize,
    /// The error text.
    pub error: String,
    /// The line as read (empty for a malformed Pi or OMP record).
    pub line: String,
}

/// The records of `transcript` that could not be decoded (upstream
/// `display.rs` reports the transcript's diagnostics; K11), in file order.
/// The caller reports them like upstream with `--debug`: the stderr line
/// `Failed to parse line {line_number}: {error}` at error level and
/// [`crate::debug_log::log_display_error`].
pub fn display_parse_errors(transcript: &Transcript) -> Vec<DisplayParseError> {
    transcript
        .diagnostics
        .iter()
        .map(|diagnostic| DisplayParseError {
            line_number: diagnostic.line_number,
            error: diagnostic.error_message.clone(),
            line: diagnostic.line_content.clone(),
        })
        .collect()
}

/// [`plain_display`] over entries (for example those of an open viewer).
pub fn plain_display_entries<'a>(
    entries: impl IntoIterator<Item = Entry<'a>>,
    options: ExportOptions,
) -> String {
    let mut formatter = PlainFormatter::default();
    for entry in entries {
        process_entry(
            &mut formatter,
            &entry,
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
        let content_str = tool_format::format_tool_result(content);
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
        let content_str = tool_format::format_tool_result(content);
        for line in content_str.lines() {
            let _ = writeln!(self.output, "    {}", line);
        }
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
    entry: &Entry<'_>,
    no_tools: bool,
    show_thinking: bool,
) {
    match entry.kind {
        EntryKind::Hidden
        | EntryKind::Note {
            searchable: false, ..
        } => {
            // Skip metadata entries
        }
        EntryKind::Note {
            label,
            text,
            searchable: true,
        } => {
            let content = if text.is_empty() {
                format!("[{label}]")
            } else {
                format!("[{label}] {text}")
            };
            process_user_message(formatter, &[Part::Text(content)], no_tools, None);
        }
        EntryKind::Subagent { agent_id, message } => {
            // Subagent progress is shown only when show_thinking is enabled
            if show_thinking {
                process_agent_message(formatter, agent_id, message, no_tools);
            }
        }
        EntryKind::Message { message, .. } => {
            let parent_id = message.parent_tool_use_id.as_deref();
            if parent_id.is_some() && !show_thinking {
                return;
            }
            match message.role {
                Role::User => process_user_message(formatter, &message.parts, no_tools, parent_id),
                Role::Assistant => process_assistant_message(
                    formatter,
                    &message.parts,
                    no_tools,
                    show_thinking,
                    parent_id,
                ),
            }
        }
    }
}

/// Process a user message using the provided formatter
// Kept verbatim from upstream `display.rs`.
#[allow(clippy::collapsible_match)]
fn process_user_message(
    formatter: &mut PlainFormatter,
    parts: &[Part],
    no_tools: bool,
    parent_id: Option<&str>,
) {
    let agent_id = parent_id.map(subagent_display_id);

    let mut printed_content = false;
    for part in parts {
        match part {
            Part::Text(text) => {
                if let Some(processed) = process_command_message(text) {
                    if let Some(ref id) = agent_id {
                        formatter.format_agent_user_text(id, &processed);
                    } else {
                        formatter.format_user_text(&processed);
                    }
                    printed_content = true;
                }
            }
            Part::ToolResult { output, .. } => {
                if !no_tools {
                    if let Some(ref id) = agent_id {
                        formatter.format_agent_tool_result(id, output.as_ref());
                    } else {
                        formatter.format_tool_result(output.as_ref());
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

/// Helper struct to categorize assistant message content
struct FormattedMessage<'a> {
    text_blocks: Vec<&'a str>,
    tool_calls: Vec<(&'a str, &'a serde_json::Value)>,
    thinking_steps: Vec<&'a str>,
}

impl<'a> From<&'a [Part]> for FormattedMessage<'a> {
    fn from(parts: &'a [Part]) -> Self {
        let mut text_blocks = Vec::new();
        let mut tool_calls = Vec::new();
        let mut thinking_steps = Vec::new();

        for part in parts {
            match part {
                Part::Text(text) => text_blocks.push(text.as_str()),
                Part::ToolCall { name, input, .. } => tool_calls.push((name.as_str(), input)),
                Part::Thinking(thinking) => thinking_steps.push(thinking.as_str()),
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
    parts: &[Part],
    no_tools: bool,
    show_thinking: bool,
    parent_id: Option<&str>,
) {
    let formatted = FormattedMessage::from(parts);
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

/// Aggregate text parts and render them with the caller-specific formatter.
fn render_agent_text_blocks(parts: &[Part], mut format_text: impl FnMut(&str)) -> bool {
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.as_str()),
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
    agent_id: &str,
    message: &Message,
    no_tools: bool,
) {
    let parts = &message.parts;
    let mut printed = false;
    match message.role {
        Role::User => {
            // User messages in agent context are typically tool results or the initial prompt
            printed |= render_agent_text_blocks(parts, |text| {
                formatter.format_agent_user_text(agent_id, text);
            });

            // Tool results
            for part in parts {
                if let Part::ToolResult { output, .. } = part
                    && !no_tools
                {
                    formatter.format_agent_tool_result(agent_id, output.as_ref());
                    printed = true;
                }
            }
        }
        Role::Assistant => {
            printed |= render_agent_text_blocks(parts, |text| {
                formatter.format_agent_assistant_text(agent_id, text);
            });

            // Tool calls
            for part in parts {
                if let Part::ToolCall { name, input, .. } = part
                    && !no_tools
                {
                    formatter.format_agent_tool_call(agent_id, name, input);
                    printed = true;
                }
            }
        }
    }
    if printed {
        formatter.end_message();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

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

    fn decode(path: &Path) -> Transcript {
        crate::history::sources::decode_path(path, None, false).unwrap()
    }

    fn render(path: &Path, options: &ExportOptions) -> String {
        plain_display(&decode(path), *options)
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
    fn display_parse_errors_report_the_transcripts_diagnostics() {
        // K11: upstream reports every record the decoder could not read,
        // with its 1-based line number (since 9597669 from the transcript's
        // diagnostics instead of a second pass over the raw lines).
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

        let errors = display_parse_errors(&decode(&path));
        let lines = errors
            .iter()
            .map(|error| (error.line_number, error.line.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            [(2, "{not json"), (5, r#"{"type":"user"}"#), (7, "[1,2]")]
        );
        for error in &errors {
            let expected = serde_json::from_str::<crate::claude::LogEntry>(&error.line)
                .unwrap_err()
                .to_string();
            assert_eq!(error.error, expected);
        }
        // The rendering itself skips them.
        let output = render(&path, &options());
        assert!(output.contains("You: ok") && output.contains("Claude: fine"));

        // A Pi session reports its malformed records only.
        let pi = directory.path().join("pi-malformed.jsonl");
        let raw = std::fs::read_to_string(fixture("tests/fixtures/pi/v3-branched.jsonl")).unwrap();
        std::fs::write(&pi, format!("{raw}not-json\n")).unwrap();
        let reported = display_parse_errors(&decode(&pi))
            .into_iter()
            .map(|error| error.line_number)
            .collect::<Vec<_>>();
        assert_eq!(reported, [raw.lines().count() + 1]);
        assert!(display_parse_errors(&decode(&fixture("tests/fixtures/omp/v3.jsonl"))).is_empty());

        let clean = directory.path().join("clean.jsonl");
        std::fs::write(
            &clean,
            r#"{"type":"user","message":{"role":"user","content":"ok"}}"#,
        )
        .unwrap();
        assert!(display_parse_errors(&decode(&clean)).is_empty());
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
            plain_display(&decode(&path), all),
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
            plain_display(&decode(&path), options()),
            "You: /review pr 7\n\nClaude: done\n\n"
        );
    }

    #[test]
    fn plain_display_entries_matches_transcript_rendering() {
        let transcript = decode(&fixture("tests/fixtures/pi/v3-branched.jsonl"));
        let all = ExportOptions {
            show_tools: true,
            show_thinking: true,
        };
        assert_eq!(
            plain_display_entries(view::entries(&transcript), all),
            plain_display(&transcript, all)
        );
    }
}
