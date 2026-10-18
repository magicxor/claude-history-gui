//! Append-only debug log file (upstream `~/.local/state/claude-history/debug.log`).
//!
//! The log path is a parameter: callers pass
//! [`crate::env::HistoryEnv::debug_log_file`] (default
//! `~/.local/state/claude-history-gui/debug.log`). The block formats are
//! unchanged from upstream.

use crate::history::{Conversation, ParseError};
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

fn open_debug_log_file(log_path: &Path) -> std::io::Result<fs::File> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    OpenOptions::new().create(true).append(true).open(log_path)
}

/// Log parse errors for a conversation to the debug log file.
///
/// Only writes to the log if there are parse errors. The log is appended to,
/// so errors accumulate over time for debugging.
pub fn log_parse_errors(log_path: &Path, conversation: &Conversation) -> std::io::Result<()> {
    if conversation.parse_errors.is_empty() {
        return Ok(());
    }

    let mut file = open_debug_log_file(log_path)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    writeln!(file, "=== Parse Errors: {} ===", timestamp)?;
    writeln!(file, "File: {}", conversation.path.display())?;
    writeln!(file, "Errors: {}", conversation.parse_errors.len())?;
    writeln!(file)?;

    for error in &conversation.parse_errors {
        write_parse_error(&mut file, error)?;
    }

    writeln!(file, "---")?;
    writeln!(file)?;

    Ok(())
}

/// Write a single parse error with context to the log file
fn write_parse_error(file: &mut fs::File, error: &ParseError) -> std::io::Result<()> {
    writeln!(file, "Line {}: {}", error.line_number, error.error_message)?;
    writeln!(file)?;

    // Context lines before the error
    let ctx_start = error.line_number.saturating_sub(error.context_before.len());
    for (i, ctx) in error.context_before.iter().enumerate() {
        writeln!(file, "  {:>4} | {}", ctx_start + i, truncate_line(ctx, 200))?;
    }

    // The failing line (marked with >)
    writeln!(
        file,
        "> {:>4} | {}",
        error.line_number,
        truncate_line(&error.line_content, 200)
    )?;

    // Context lines after the error
    for (i, ctx) in error.context_after.iter().enumerate() {
        writeln!(
            file,
            "  {:>4} | {}",
            error.line_number + 1 + i,
            truncate_line(ctx, 200)
        )?;
    }

    writeln!(file)?;
    Ok(())
}

/// Truncate a line to a maximum number of characters for readable logs.
/// Uses char-aware truncation to avoid panicking on multi-byte UTF-8.
fn truncate_line(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect::<String>() + "..."
    }
}

/// Open the debug log file and call `f` with the opened file.
fn with_debug_log(
    log_path: &Path,
    f: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut file = open_debug_log_file(log_path)?;
    f(&mut file)
}

/// Write a timestamped message `[timestamp] message` to the debug log file.
fn write_timestamped(file: &mut fs::File, message: &str) -> std::io::Result<()> {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    writeln!(file, "[{}] {}", timestamp, message)
}

/// Log a debug message to the debug log file.
pub fn log_debug(log_path: &Path, message: &str) -> std::io::Result<()> {
    with_debug_log(log_path, |file| write_timestamped(file, message))
}

/// Log a display-time parse error to the debug log file.
pub fn log_display_error(
    log_path: &Path,
    file_path: &Path,
    line_number: usize,
    error: &str,
    line_content: &str,
) -> std::io::Result<()> {
    with_debug_log(log_path, |file| {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        writeln!(file, "=== Display Parse Error: {} ===", timestamp)?;
        writeln!(file, "File: {}", file_path.display())?;
        writeln!(file, "Line {}: {}", line_number, error)?;
        writeln!(file, "Content: {}", truncate_line(line_content, 200))?;
        writeln!(file, "---")?;
        writeln!(file)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{Conversation, ParseError, Source};
    use std::path::PathBuf;

    fn conversation_with_errors() -> Conversation {
        Conversation {
            source: Source::Claude,
            session_id: "session".to_string(),
            path: PathBuf::from("session.jsonl"),
            index: 0,
            timestamp: chrono::Local::now(),
            preview: String::new(),
            preview_first: String::new(),
            preview_last: String::new(),
            full_text: String::new(),
            agent_search_text: String::new(),
            semantic_route_text: String::new(),
            semantic_turns: Vec::new(),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: String::new(),
            project_name: None,
            project_path: None,
            cwd: None,
            message_count: 0,
            parse_errors: vec![ParseError {
                line_number: 3,
                line_content: "x".repeat(250),
                error_message: "expected value".to_string(),
                context_before: vec!["a".to_string(), "b".to_string()],
                context_after: vec!["d".to_string()],
            }],
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        }
    }

    #[test]
    fn writes_upstream_blocks_to_the_given_log_path() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("state").join("debug.log");

        log_parse_errors(&log, &conversation_with_errors()).unwrap();
        log_debug(&log, "Failed to delete session x: denied").unwrap();
        log_display_error(&log, Path::new("f.jsonl"), 7, "bad", "content").unwrap();

        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("=== Parse Errors: "));
        assert!(text.contains("File: session.jsonl\nErrors: 1\n\nLine 3: expected value\n\n"));
        assert!(text.contains("     1 | a\n     2 | b\n>    3 | "));
        assert!(text.contains(&format!("{}...", "x".repeat(200))));
        assert!(text.contains("     4 | d\n\n---\n\n"));
        assert!(text.contains("] Failed to delete session x: denied\n"));
        assert!(text.contains("=== Display Parse Error: "));
        assert!(text.contains("File: f.jsonl\nLine 7: bad\nContent: content\n---\n\n"));
    }

    #[test]
    fn conversation_without_errors_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("debug.log");
        let mut conversation = conversation_with_errors();
        conversation.parse_errors.clear();

        log_parse_errors(&log, &conversation).unwrap();
        assert!(!log.exists());
    }
}
