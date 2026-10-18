//! Conversation export functionality.
//!
//! This module provides functions to export conversations in different formats:
//! - Ledger format (formatted text with speaker names)
//! - Plain text (simple speaker: message format)
//! - Markdown (with headers for speakers)
//! - JSONL (raw format)
//!
//! Export respects the current display settings for thinking blocks and tool calls.
//!
//! Moved from upstream `tui/export.rs` (claude-history v0.1.76) minus the
//! clipboard transport (OSC 52, `wl-copy`/`xclip`/`xsel`, `arboard`,
//! `CLAUDE_HISTORY_CLIPBOARD`): a GUI uses its own clipboard. Every generator
//! returns a `String`; [`export_to_path`] writes to a caller-supplied path
//! instead of a relative file name in the process working directory.
//! [`plain_display`] is the CLI `--plain` stdout format (upstream
//! `display.rs`), which differs from [`ExportFormat::Plain`].

use crate::claude::{self, AgentContent, ContentBlock, LogEntry, UserContent, UserMessage};
use crate::commands::parse_command_name_and_args;
use crate::tool_format;
use chrono::{DateTime, Local};
use std::fs;
use std::path::Path;

mod plain;

pub use plain::{DisplayParseError, display_parse_errors, plain_display, plain_display_entries};

/// Export format options
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    Ledger,
    Plain,
    Markdown,
    Jsonl,
}

impl ExportFormat {
    /// Every format in menu order (upstream export/yank menu, digits 1-4).
    pub const ALL: [Self; 4] = [Self::Ledger, Self::Plain, Self::Markdown, Self::Jsonl];

    /// Get format from menu option index (0-3)
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(ExportFormat::Ledger),
            1 => Some(ExportFormat::Plain),
            2 => Some(ExportFormat::Markdown),
            3 => Some(ExportFormat::Jsonl),
            _ => None,
        }
    }

    /// Menu option index (0-3), the inverse of [`ExportFormat::from_index`].
    pub fn index(self) -> usize {
        match self {
            ExportFormat::Ledger => 0,
            ExportFormat::Plain => 1,
            ExportFormat::Markdown => 2,
            ExportFormat::Jsonl => 3,
        }
    }

    /// Menu label (upstream `tui/app/dialog_state.rs` `EXPORT_OPTIONS`).
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Ledger => "Ledger (formatted)",
            ExportFormat::Plain => "Plain text",
            ExportFormat::Markdown => "Markdown",
            ExportFormat::Jsonl => "JSONL (raw)",
        }
    }

    /// Get file extension for this format
    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Ledger | ExportFormat::Plain => "txt",
            ExportFormat::Markdown => "md",
            ExportFormat::Jsonl => "jsonl",
        }
    }
}

/// Options for export content generation
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExportOptions {
    pub show_tools: bool,
    pub show_thinking: bool,
}

/// Status text after a successful clipboard copy of a whole export
/// (upstream `export_to_clipboard`).
pub const COPIED_TO_CLIPBOARD: &str = "Copied to clipboard";
/// Status text after copying the focused message (upstream `view_state.rs`).
pub const MESSAGE_COPIED_TO_CLIPBOARD: &str = "Message copied to clipboard";
/// Status text when the focused message has no copyable text.
pub const NO_TEXT_CONTENT: &str = "No text content in this message";
/// Error text of [`extract_message_text`] for an entry index past the end.
pub const MESSAGE_NOT_FOUND: &str = "Message not found";
/// Status text after copying the conversation path (upstream `Y`).
pub const PATH_COPIED_TO_CLIPBOARD: &str = "Path copied to clipboard";
/// Status text after copying the session ID (upstream `I`).
pub const SESSION_ID_COPIED_TO_CLIPBOARD: &str = "Session ID copied to clipboard";

/// Status text after a successful export (`"Exported to {name}"`). Upstream
/// showed the relative file name it wrote into the working directory.
pub fn exported_status(name: &str) -> String {
    format!("Exported to {}", name)
}

/// Why [`export_to_path`] failed. `Display` gives upstream's status text.
#[derive(Debug)]
pub enum ExportError {
    /// The conversation could not be read (`"Failed to read: {e}"`).
    Read(std::io::Error),
    /// The destination could not be written (`"Failed to write: {e}"`).
    Write(std::io::Error),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::Read(e) => write!(f, "Failed to read: {}", e),
            ExportError::Write(e) => write!(f, "Failed to write: {}", e),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExportError::Read(e) | ExportError::Write(e) => Some(e),
        }
    }
}

/// Default export file name, `conversation-%Y-%m-%d-%H%M%S.{ext}` (upstream
/// `export_to_file` used it relative to the process working directory).
pub fn default_export_file_name(format: ExportFormat, now: DateTime<Local>) -> String {
    let timestamp = now.format("%Y-%m-%d-%H%M%S");
    let ext = format.extension();
    format!("conversation-{}.{}", timestamp, ext)
}

/// Generate the export text of `source_path` in `format`.
pub fn generate_export(
    source_path: &Path,
    format: ExportFormat,
    options: ExportOptions,
) -> std::io::Result<String> {
    generate_content(source_path, format, options)
}

/// Export the conversation at `source_path` to `dest` (overwritten if it
/// exists, as upstream did).
pub fn export_to_path(
    source_path: &Path,
    format: ExportFormat,
    options: ExportOptions,
    dest: &Path,
) -> Result<(), ExportError> {
    let content = generate_content(source_path, format, options).map_err(ExportError::Read)?;
    fs::write(dest, &content).map_err(ExportError::Write)
}

/// Extract the text content of a single message by its entry index in the JSONL file.
/// Returns the message text suitable for clipboard copying.
pub fn extract_message_text(
    source_path: &Path,
    entry_index: usize,
    options: ExportOptions,
) -> Result<String, String> {
    let entries = crate::history::normalized_log_entries(source_path)
        .map_err(|e| format!("Failed to read: {e}"))?;
    entries
        .into_iter()
        .map(|(_, entry)| entry)
        .nth(entry_index)
        .map(|entry| format_entry_for_clipboard(&entry, options))
        .ok_or_else(|| MESSAGE_NOT_FOUND.to_string())
}

/// The clipboard text of one already parsed entry: the raw message text
/// (markdown source, not the rendered view), as [`extract_message_text`]
/// formats it. Empty when the entry has nothing to copy.
pub fn message_text(entry: &LogEntry, options: ExportOptions) -> String {
    format_entry_for_clipboard(entry, options)
}

/// Format a single log entry as text for clipboard
/// Append text with blank-line separation if output is non-empty.
fn append_separated(output: &mut String, text: &str) {
    if !output.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(text);
}

/// Iterate content blocks and append formatted output for clipboard-style export.
/// Handles Text, ToolUse, ToolResult, and Thinking blocks guarded by options.
fn append_clipboard_blocks(output: &mut String, blocks: &[ContentBlock], options: &ExportOptions) {
    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                append_separated(output, text);
            }
            ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                append_separated(output, &format_tool_call_for_export(name, input));
            }
            ContentBlock::ToolResult { content, .. } if options.show_tools => {
                append_separated(output, &format_tool_result_for_export(content.as_ref()));
            }
            ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                append_separated(output, thinking);
            }
            _ => {}
        }
    }
}

/// Invoke `f` with the formatted content string for each ToolResult block
/// in a user message, when show_tools is enabled.
fn for_user_tool_results(message: &UserMessage, options: &ExportOptions, mut f: impl FnMut(&str)) {
    if options.show_tools
        && let UserContent::Blocks(blocks) = &message.content
    {
        for block in blocks {
            if let ContentBlock::ToolResult { content, .. } = block {
                let content_str = format_tool_result_for_export(content.as_ref());
                f(&content_str);
            }
        }
    }
}

fn format_entry_for_clipboard(entry: &LogEntry, options: ExportOptions) -> String {
    let mut output = String::new();
    match entry {
        LogEntry::User {
            message,
            parent_tool_use_id,
            ..
        } => {
            if let Some(text) = extract_user_text(message) {
                output.push_str(&text);
            }
            for_user_tool_results(message, &options, |content| {
                append_separated(&mut output, content);
            });
            let _ = parent_tool_use_id;
        }
        LogEntry::Assistant {
            message,
            parent_tool_use_id,
            ..
        } => {
            append_clipboard_blocks(&mut output, &message.content, &options);
            let _ = parent_tool_use_id;
        }
        LogEntry::PiMetadata {
            label,
            text,
            searchable: true,
            ..
        } => {
            output.push_str(&format!("[{label}] {text}"));
        }
        LogEntry::Progress { data, .. } => {
            if let Some(agent_progress) = claude::parse_agent_progress(data) {
                let AgentContent::Blocks(blocks) = &agent_progress.message.message.content;
                append_clipboard_blocks(&mut output, blocks, &options);
            }
        }
        _ => {}
    }
    output
}

/// Generate content in the specified format
fn generate_content(
    source_path: &Path,
    format: ExportFormat,
    options: ExportOptions,
) -> std::io::Result<String> {
    match format {
        ExportFormat::Jsonl => fs::read_to_string(source_path),
        ExportFormat::Plain => generate_plain(source_path, options),
        ExportFormat::Markdown => generate_markdown(source_path, options),
        ExportFormat::Ledger => generate_ledger(source_path, options),
    }
}

fn generate_plain_or_markdown_content(
    path: &Path,
    options: ExportOptions,
    mut handle_user_text: impl FnMut(&mut String, &str, &str),
    mut handle_user_tool_result: impl FnMut(&mut String, &str, &str),
    mut handle_assistant_text: impl FnMut(&mut String, &str, &str, &str),
    mut handle_assistant_tool_use: impl FnMut(&mut String, &str, &str, &serde_json::Value),
    mut handle_assistant_thinking: impl FnMut(&mut String, &str, &str),
) -> std::io::Result<String> {
    let entries = crate::history::normalized_log_entries(path)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut output = String::new();

    for (_, entry) in entries {
        match entry {
            LogEntry::User {
                message,
                parent_tool_use_id,
                ..
            } => {
                if parent_tool_use_id.is_some() && !options.show_thinking {
                    continue;
                }
                let prefix = subagent_prefix(&parent_tool_use_id);
                if let Some(text) = extract_user_text(&message) {
                    handle_user_text(&mut output, &prefix, &text);
                }
                // Tool results
                for_user_tool_results(&message, &options, |content| {
                    handle_user_tool_result(&mut output, &prefix, content);
                });
            }
            LogEntry::Assistant {
                message,
                agent,
                parent_tool_use_id,
                ..
            } => {
                if parent_tool_use_id.is_some() && !options.show_thinking {
                    continue;
                }
                let prefix = subagent_prefix(&parent_tool_use_id);
                let speaker = agent.as_deref().unwrap_or("Claude");
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            handle_assistant_text(&mut output, &prefix, speaker, text);
                        }
                        ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                            handle_assistant_tool_use(&mut output, &prefix, name, input);
                        }
                        ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                            handle_assistant_thinking(&mut output, &prefix, thinking);
                        }
                        _ => {}
                    }
                }
            }
            LogEntry::PiMetadata {
                label,
                text,
                searchable: true,
                ..
            } => {
                let rendered = if text.is_empty() {
                    format!("[{label}]")
                } else {
                    format!("[{label}] {text}")
                };
                handle_user_text(&mut output, "", &rendered);
            }
            _ => {}
        }
    }

    Ok(output)
}

/// Generate plain text format (simple "Speaker: message" lines)
fn generate_plain(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    generate_plain_or_markdown_content(
        path,
        options,
        |output, prefix, text| {
            output.push_str(&format!("{}You: {}\n\n", prefix, text));
        },
        |output, prefix, content| {
            output.push_str(&format!("{}Tool Result: {}\n\n", prefix, content));
        },
        |output, prefix, speaker, text| {
            output.push_str(&format!("{}{speaker}: {}\n\n", prefix, text));
        },
        |output, prefix, name, input| {
            let formatted = format_tool_call_for_export(name, input);
            output.push_str(&format!("{}Tool: {}\n\n", prefix, formatted));
        },
        |output, prefix, thinking| {
            output.push_str(&format!("{}Thinking: {}\n\n", prefix, thinking));
        },
    )
}

/// Generate markdown format (with ## headers for speakers)
fn generate_markdown(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    generate_plain_or_markdown_content(
        path,
        options,
        |output, prefix, text| {
            output.push_str(&format!("## {}You\n\n{}\n\n", prefix, text));
        },
        |output, prefix, content| {
            let fenced = markdown_code_fence(content);
            output.push_str(&format!("### {}Tool Result\n\n{}\n\n", prefix, fenced));
        },
        |output, prefix, speaker, text| {
            output.push_str(&format!("## {}{speaker}\n\n{}\n\n", prefix, text));
        },
        |output, prefix, name, input| {
            let formatted = format_tool_call_for_export(name, input);
            let fenced = markdown_code_fence(&formatted);
            output.push_str(&format!("### {}Tool: {}\n\n{}\n\n", prefix, name, fenced));
        },
        |output, prefix, thinking| {
            output.push_str(&format!("### {}Thinking\n\n{}\n\n", prefix, thinking));
        },
    )
}

/// Total line width for ledger export (including name column and separator)
pub const LEDGER_WIDTH: usize = 90;

/// Generate ledger-style format (formatted like the TUI viewer)
fn generate_ledger(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    let entries = crate::history::normalized_log_entries(path)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut output = String::new();

    const NAME_WIDTH: usize = 9;
    // 3 for " │ " separator
    let content_width = LEDGER_WIDTH - NAME_WIDTH - 3;

    for (_, entry) in entries {
        match entry {
            LogEntry::User {
                message,
                parent_tool_use_id,
                ..
            } => {
                if parent_tool_use_id.is_some() && !options.show_thinking {
                    continue;
                }
                let speaker = match &parent_tool_use_id {
                    Some(id) => format!("↳{}", claude::short_parent_id(id)),
                    None => "You".to_string(),
                };
                if let Some(text) = extract_user_text(&message) {
                    let wrapped = wrap_plain_text(&text, content_width);
                    append_ledger_block(&mut output, &speaker, &wrapped, NAME_WIDTH);
                    output.push('\n');
                }
                // Tool results
                for_user_tool_results(&message, &options, |content| {
                    if !content.trim().is_empty() {
                        let wrapped = wrap_plain_text(content, content_width);
                        append_ledger_block(&mut output, "↳ Result", &wrapped, NAME_WIDTH);
                        output.push('\n');
                    }
                });
            }
            LogEntry::Assistant {
                message,
                agent,
                parent_tool_use_id,
                ..
            } => {
                if parent_tool_use_id.is_some() && !options.show_thinking {
                    continue;
                }
                let speaker = match &parent_tool_use_id {
                    Some(id) => format!("↳{}", claude::short_parent_id(id)),
                    None => agent.unwrap_or_else(|| "Claude".to_string()),
                };
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let rendered =
                                crate::markdown::render_markdown_plain(text, content_width);
                            let rendered = rendered.trim_end();
                            append_ledger_block(&mut output, &speaker, rendered, NAME_WIDTH);
                            output.push('\n');
                        }
                        ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                            let formatted = format_tool_call_for_ledger(name, input, content_width);
                            let tool_label = if parent_tool_use_id.is_some() {
                                &speaker
                            } else {
                                "Tool"
                            };
                            append_ledger_block(&mut output, tool_label, &formatted, NAME_WIDTH);
                            output.push('\n');
                        }
                        ContentBlock::Thinking { thinking, .. }
                            if options.show_thinking && !thinking.is_empty() =>
                        {
                            let rendered =
                                crate::markdown::render_markdown_plain(thinking, content_width);
                            let rendered = rendered.trim_end();
                            append_ledger_block(&mut output, "Thinking", rendered, NAME_WIDTH);
                            output.push('\n');
                        }
                        _ => {}
                    }
                }
            }
            LogEntry::PiMetadata {
                label,
                text,
                searchable: true,
                ..
            } => {
                append_ledger_block(&mut output, &label, &text, NAME_WIDTH);
                output.push('\n');
            }
            _ => {}
        }
    }

    Ok(output)
}

/// Append a ledger-formatted block to the output
fn append_ledger_block(output: &mut String, speaker: &str, text: &str, name_width: usize) {
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            output.push_str(&format!(
                "{:>width$} │ {}\n",
                speaker,
                line,
                width = name_width
            ));
        } else {
            output.push_str(&format!("{:>width$} │ {}\n", "", line, width = name_width));
        }
    }
}

/// Generate a prefix string for subagent entries in exports.
/// Returns "[↳ID] " for nested entries, empty string for top-level.
fn subagent_prefix(parent_tool_use_id: &Option<String>) -> String {
    match parent_tool_use_id {
        Some(id) => format!("[↳{}] ", claude::short_parent_id(id)),
        None => String::new(),
    }
}

/// Extract text from a user message, handling command messages
fn extract_user_text(message: &UserMessage) -> Option<String> {
    match &message.content {
        UserContent::String(s) => process_command_text(s),
        UserContent::Blocks(blocks) => {
            for block in blocks {
                if let ContentBlock::Text { text } = block
                    && let Some(processed) = process_command_text(text)
                {
                    return Some(processed);
                }
            }
            None
        }
    }
}

/// Process command message text, extracting content from XML tags
fn process_command_text(text: &str) -> Option<String> {
    let trimmed = text.trim();

    // Handle <local-command-stdout> tags
    if trimmed.starts_with("<local-command-stdout>") && trimmed.ends_with("</local-command-stdout>")
    {
        let inner = &trimmed
            ["<local-command-stdout>".len()..trimmed.len() - "</local-command-stdout>".len()];
        if inner.trim().is_empty() {
            return None;
        }
        return Some(inner.trim().to_string());
    }

    if let Some(processed) = parse_command_name_and_args(trimmed) {
        return Some(processed);
    }

    Some(text.to_string())
}

/// Wrap content in markdown code fence, handling nested backticks
fn markdown_code_fence(content: &str) -> String {
    // Find the longest run of backticks in content and use one more
    let max_backticks = content
        .split(|c| c != '`')
        .map(|s| s.len())
        .max()
        .unwrap_or(0);
    let fence_len = std::cmp::max(3, max_backticks + 1);
    let fence: String = std::iter::repeat_n('`', fence_len).collect();
    format!("{}\n{}\n{}", fence, content, fence)
}

/// Default width for non-ledger export (no wrapping needed for markdown export)
const EXPORT_WIDTH: usize = usize::MAX;

/// Format a tool call for export (non-ledger formats)
fn format_tool_call_for_export(name: &str, input: &serde_json::Value) -> String {
    let formatted = tool_format::format_tool_call(name, input, EXPORT_WIDTH);
    match formatted.body {
        Some(body) => format!("{}\n{}", formatted.header, body),
        None => formatted.header,
    }
}

/// Format a tool call for ledger export with line wrapping
fn format_tool_call_for_ledger(name: &str, input: &serde_json::Value, max_width: usize) -> String {
    let formatted = tool_format::format_tool_call(name, input, max_width);
    let text = match formatted.body {
        Some(body) => format!("{}\n{}", formatted.header, body),
        None => formatted.header,
    };
    // Wrap any remaining long lines
    wrap_plain_text(&text, max_width)
}

/// Wrap plain text to max_width, preserving existing line breaks
fn wrap_plain_text(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        if line.is_empty() {
            continue;
        }
        let wrapped: Vec<_> = textwrap::wrap(line, max_width)
            .into_iter()
            .map(|cow| cow.into_owned())
            .collect();
        for (j, w) in wrapped.iter().enumerate() {
            if j > 0 {
                result.push('\n');
            }
            result.push_str(w);
        }
    }
    result
}

/// Format tool result content for export
fn format_tool_result_for_export(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            // Handle array of content blocks
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect();
            if !texts.is_empty() {
                texts.join("\n\n")
            } else {
                serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "<error>".to_string())
            }
        }
        Some(value) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "<error>".to_string())
        }
        None => "<no content>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_plain_text_preserves_short_lines() {
        let result = wrap_plain_text("short line", 80);
        assert_eq!(result, "short line");
    }

    #[test]
    fn test_wrap_plain_text_wraps_long_line() {
        let long = "word ".repeat(20); // 100 chars
        let result = wrap_plain_text(long.trim(), 40);
        for line in result.lines() {
            assert!(line.len() <= 40, "Line exceeds max_width: {:?}", line);
        }
        // All words should be preserved
        assert_eq!(result.matches("word").count(), 20);
    }

    #[test]
    fn test_wrap_plain_text_preserves_existing_newlines() {
        let text = "line one\nline two\nline three";
        let result = wrap_plain_text(text, 80);
        assert_eq!(result.lines().count(), 3);
    }

    #[test]
    fn test_wrap_plain_text_preserves_empty_lines() {
        let text = "line one\n\nline three";
        let result = wrap_plain_text(text, 80);
        assert_eq!(result, "line one\n\nline three");
    }

    #[test]
    fn test_append_ledger_block_format() {
        let mut output = String::new();
        append_ledger_block(&mut output, "Claude", "Hello\nWorld", 9);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("   Claude │ Hello"));
        assert!(lines[1].starts_with("          │ World"));
    }

    #[test]
    fn test_ledger_line_width() {
        // Verify that a wrapped line fits within LEDGER_WIDTH
        let name_width = 9;
        let content_width = LEDGER_WIDTH - name_width - 3;
        let long_text = "word ".repeat(20);
        let wrapped = wrap_plain_text(long_text.trim(), content_width);
        let mut output = String::new();
        append_ledger_block(&mut output, "Claude", &wrapped, name_width);
        for line in output.lines() {
            // Count display width (name + " │ " + content)
            let width = line.chars().count();
            assert!(
                width <= LEDGER_WIDTH,
                "Ledger line exceeds {} chars (got {}): {:?}",
                LEDGER_WIDTH,
                width,
                line
            );
        }
    }

    #[test]
    fn test_ledger_markdown_rendering() {
        // Verify that markdown is rendered (not raw) in ledger export
        let content_width = LEDGER_WIDTH - 9 - 3;
        let rendered =
            crate::markdown::render_markdown_plain("This has **bold** and `code`", content_width);
        // Should not contain markdown formatting markers for bold
        assert!(
            !rendered.contains("**"),
            "Should strip bold markers: {:?}",
            rendered
        );
        // Should contain backticks for inline code
        assert!(
            rendered.contains("`code`"),
            "Should keep inline code backticks: {:?}",
            rendered
        );
        // Should not contain ANSI codes
        assert!(
            !rendered.contains("\x1b"),
            "Should not contain ANSI codes: {:?}",
            rendered
        );
    }

    #[test]
    fn pi_exports_use_pi_assistant_label() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v3-branched.jsonl");
        let options = ExportOptions::default();

        let plain = generate_plain(&path, options).unwrap();
        let markdown = generate_markdown(&path, options).unwrap();
        let ledger = generate_ledger(&path, options).unwrap();

        assert!(plain.contains("Pi: root answer"));
        assert!(markdown.contains("## Pi\n\nroot answer"));
        assert!(ledger.contains("Pi │ root answer"));
        assert!(!plain.contains("Claude: root answer"));
        for metadata in [
            "Branch summary",
            "Compaction",
            "Thinking level",
            "Model",
            "Label",
            "custom state searchable",
        ] {
            assert!(!plain.contains(metadata));
            assert!(!markdown.contains(metadata));
            assert!(!ledger.contains(metadata));
        }
    }

    #[test]
    fn test_generate_ledger_wraps_and_renders() {
        // Create a sample JSONL with a long assistant message containing markdown
        let long_text = "This is a **really long** sentence that should definitely wrap because it contains many words and exceeds the content width of the ledger format which is 68 characters.";
        let entry = serde_json::json!({
            "type": "assistant",
            "message": {
                "id": "test",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": long_text}],
                "model": "test",
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            },
            "timestamp": "2024-01-01T00:00:00Z"
        });

        // Upstream wrote a fixed file name under std::env::temp_dir(); a
        // private temporary directory keeps the test hermetic.
        let tmpdir = tempfile::tempdir().unwrap();
        let tmppath = tmpdir.path().join("claude-history-test-ledger.jsonl");
        std::fs::write(&tmppath, format!("{}\n", entry)).unwrap();

        let result = generate_ledger(
            &tmppath,
            ExportOptions {
                show_tools: false,
                show_thinking: false,
            },
        )
        .unwrap();

        eprintln!("Ledger output:\n{}", result);

        // Every line should fit within LEDGER_WIDTH
        for line in result.lines() {
            if line.is_empty() {
                continue;
            }
            let width = line.chars().count();
            assert!(
                width <= LEDGER_WIDTH,
                "Ledger line exceeds {} chars (got {}): {:?}",
                LEDGER_WIDTH,
                width,
                line
            );
        }

        // Should contain the speaker name
        assert!(result.contains("Claude"), "Should have speaker name");
        // Should not contain ANSI codes
        assert!(!result.contains("\x1b"), "Should not contain ANSI codes");
        // Bold markers should be stripped (markdown rendered)
        assert!(
            !result.contains("**"),
            "Should not contain raw bold markers"
        );
        // Content should be wrapped across multiple lines
        let content_lines: Vec<&str> = result.lines().filter(|l| !l.is_empty()).collect();
        assert!(
            content_lines.len() > 1,
            "Long text should wrap to multiple lines, got: {:?}",
            content_lines
        );
    }

    // -----------------------------------------------------------------
    // claude-history-core additions: the public API that replaced
    // upstream's cwd-relative `export_to_file` and the menu constants.
    // -----------------------------------------------------------------

    const CONVERSATION: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":"question"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"answer"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file.txt"}]}}"#,
        "\n",
    );

    fn write_conversation(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("conversation.jsonl");
        std::fs::write(&path, CONVERSATION).unwrap();
        path
    }

    #[test]
    fn export_formats_keep_menu_order_labels_and_extensions() {
        let labels: Vec<_> = ExportFormat::ALL.iter().map(|f| f.label()).collect();
        assert_eq!(
            labels,
            [
                "Ledger (formatted)",
                "Plain text",
                "Markdown",
                "JSONL (raw)"
            ]
        );
        let extensions: Vec<_> = ExportFormat::ALL.iter().map(|f| f.extension()).collect();
        assert_eq!(extensions, ["txt", "txt", "md", "jsonl"]);
        for (index, format) in ExportFormat::ALL.into_iter().enumerate() {
            assert_eq!(format.index(), index);
            assert_eq!(ExportFormat::from_index(index), Some(format));
        }
        assert_eq!(ExportFormat::from_index(4), None);
    }

    #[test]
    fn default_export_file_name_uses_upstream_pattern() {
        use chrono::TimeZone;
        let now = Local.with_ymd_and_hms(2026, 3, 7, 9, 5, 2).unwrap();
        assert_eq!(
            default_export_file_name(ExportFormat::Markdown, now),
            "conversation-2026-03-07-090502.md"
        );
        assert_eq!(
            default_export_file_name(ExportFormat::Ledger, now),
            "conversation-2026-03-07-090502.txt"
        );
        assert_eq!(
            default_export_file_name(ExportFormat::Jsonl, now),
            "conversation-2026-03-07-090502.jsonl"
        );
    }

    #[test]
    fn generate_export_honors_tool_and_thinking_options() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_conversation(dir.path());

        let hidden = generate_export(&path, ExportFormat::Plain, ExportOptions::default()).unwrap();
        assert_eq!(hidden, "You: question\n\nClaude: answer\n\n");

        let shown = generate_export(
            &path,
            ExportFormat::Plain,
            ExportOptions {
                show_tools: true,
                show_thinking: false,
            },
        )
        .unwrap();
        assert_eq!(
            shown,
            "You: question\n\nClaude: answer\n\nTool: Bash: ls\n\nTool Result: file.txt\n\n"
        );

        let markdown = generate_export(
            &path,
            ExportFormat::Markdown,
            ExportOptions {
                show_tools: true,
                show_thinking: false,
            },
        )
        .unwrap();
        assert!(markdown.contains("### Tool: Bash\n\n```\nBash: ls\n```\n\n"));
        assert!(markdown.contains("### Tool Result\n\n```\nfile.txt\n```\n\n"));

        let jsonl = generate_export(&path, ExportFormat::Jsonl, ExportOptions::default()).unwrap();
        assert_eq!(jsonl, CONVERSATION);
    }

    #[test]
    fn export_to_path_writes_destination_and_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_conversation(dir.path());
        let dest = dir.path().join("out.md");

        export_to_path(
            &path,
            ExportFormat::Markdown,
            ExportOptions::default(),
            &dest,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "## You\n\nquestion\n\n## Claude\n\nanswer\n\n"
        );

        let missing = dir.path().join("missing.jsonl");
        let error = export_to_path(
            &missing,
            ExportFormat::Jsonl,
            ExportOptions::default(),
            &dest,
        )
        .unwrap_err();
        assert!(matches!(error, ExportError::Read(_)));
        assert!(error.to_string().starts_with("Failed to read: "));

        let unwritable = dir.path().join("no-such-dir").join("out.md");
        let error = export_to_path(
            &path,
            ExportFormat::Plain,
            ExportOptions::default(),
            &unwritable,
        )
        .unwrap_err();
        assert!(matches!(error, ExportError::Write(_)));
        assert!(error.to_string().starts_with("Failed to write: "));
        assert_eq!(exported_status("a.md"), "Exported to a.md");
    }

    #[test]
    fn extract_message_text_uses_normalized_entry_indices() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_conversation(dir.path());
        let tools = ExportOptions {
            show_tools: true,
            show_thinking: false,
        };

        assert_eq!(
            extract_message_text(&path, 0, tools).unwrap(),
            "question".to_string()
        );
        assert_eq!(
            extract_message_text(&path, 1, ExportOptions::default()).unwrap(),
            "answer"
        );
        assert_eq!(
            extract_message_text(&path, 1, tools).unwrap(),
            "answer\n\nBash: ls"
        );
        assert_eq!(extract_message_text(&path, 2, tools).unwrap(), "file.txt");
        assert_eq!(
            extract_message_text(&path, 2, ExportOptions::default()).unwrap(),
            ""
        );
        assert_eq!(
            extract_message_text(&path, 3, tools).unwrap_err(),
            MESSAGE_NOT_FOUND
        );
        assert!(
            extract_message_text(&dir.path().join("missing.jsonl"), 0, tools)
                .unwrap_err()
                .starts_with("Failed to read: ")
        );
    }

    #[test]
    fn export_user_text_keeps_first_block_and_unfiltered_commands() {
        // Export uses the FIRST processed text block and, unlike the viewer,
        // does not skip caveats, /clear or skill expansions.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<local-command-caveat>caveat</local-command-caveat>"},{"type":"text","text":"second"}]}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
                "\n",
            ),
        )
        .unwrap();
        let plain = generate_export(&path, ExportFormat::Plain, ExportOptions::default()).unwrap();
        assert_eq!(
            plain,
            "You: <local-command-caveat>caveat</local-command-caveat>\n\nYou: /clear\n\n"
        );
    }

    #[test]
    fn exports_skip_agent_progress_records_but_clipboard_keeps_them() {
        // VIEW quirk 13: the plain, markdown and ledger walkers ignore
        // `progress` records; the clipboard copy of one includes it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.jsonl");
        let progress = r#"{"type":"progress","data":{"type":"agent_progress","agentId":"a1","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"SUBAGENT-SECRET"}]}}}}"#;
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                r#"{"type":"user","message":{"role":"user","content":"hello"}}"#, progress
            ),
        )
        .unwrap();
        for format in [
            ExportFormat::Plain,
            ExportFormat::Markdown,
            ExportFormat::Ledger,
        ] {
            let out = generate_export(&path, format, ExportOptions::default()).unwrap();
            assert!(out.contains("hello"), "{format:?}");
            assert!(!out.contains("SUBAGENT-SECRET"), "{format:?}: {out}");
        }
        let entry: LogEntry = serde_json::from_str(progress).unwrap();
        let copied = format_entry_for_clipboard(&entry, ExportOptions::default());
        assert!(copied.contains("SUBAGENT-SECRET"), "{copied}");
    }

    #[test]
    fn markdown_fence_grows_past_backtick_runs() {
        assert_eq!(markdown_code_fence("plain"), "```\nplain\n```");
        assert_eq!(markdown_code_fence("a ```` b"), "`````\na ```` b\n`````");
    }
}
