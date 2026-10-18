//! Conversation viewer rendering for TUI display.
//!
//! This module renders conversation JSONL files to `Vec<RenderedLine>` for display
//! in the TUI viewer. It produces styled spans that ratatui can render directly,
//! without using ANSI escape codes.
//!
//! Moved from upstream `tui/viewer/**` (claude-history v0.1.76). Edits against
//! upstream, all mechanical: the palette comes from [`crate::theme::current`]
//! instead of terminal detection (R3); `process_command_message` lives in
//! [`crate::commands`]; a few items are re-exported `pub(crate)` for the
//! layout-free [`crate::view::document`] builder; the agent-progress label is
//! sliced by characters instead of bytes (W18). Rendering output is unchanged.

use crate::claude::LogEntry;
use std::collections::BTreeSet;
use std::path::Path;

use crate::theme::{self, Theme};

mod entry;

pub use crate::commands::process_command_message;
// Upstream layout (`tui/viewer/ledger.rs`).
#[allow(clippy::module_inception)]
mod ledger;
mod markdown;
mod output;
mod style;
mod summary;
mod timing;
mod tools;

pub use ledger::fitted_name;
pub use output::{LineStyle, RenderedLine};

use entry::render_entry;
use summary::{PendingToolSummary, flush_tool_summary};
// Shared with `view::document`, which builds the layout-free model by the
// same rules (single source of truth for grouping, ids, texts and timing).
pub(crate) use markdown::render_markdown_to_lines;
pub(crate) use style::{assistant_label, subagent_label};
pub(crate) use summary::{
    ToolActivitySummary, summarize_tool_calls, tool_only_assistant_summary,
    user_entry_is_only_tool_results,
};
pub(crate) use timing::{RowTiming, TimingSlot};
pub(crate) use tools::{
    ToolOutputKind, format_tool_result_content, make_tool_output_id, make_tool_summary_output_id,
    tool_result_display_text,
};

/// Width of the focus gutter indicator (▌ + space)
pub const GUTTER_WIDTH: usize = 2;

/// Width of the right-aligned name (label) column.
pub const NAME_WIDTH: usize = 9;
/// Width of timestamp prefix when timing is enabled (space + Mon DD HH:MM + space)
pub const TIMESTAMP_WIDTH: usize = 14;

/// Get the active palette ([`crate::theme::current`], dark unless the GUI
/// selected light).
fn th() -> &'static Theme {
    theme::current()
}

/// Maximum body lines shown in truncated tool call mode
pub const TRUNCATED_BODY_LINES: usize = 3;
/// Maximum result lines shown in truncated tool result mode
pub const TRUNCATED_RESULT_LINES: usize = 4;

/// Content width (in terminal columns) for a ledger view `frame_width`
/// columns wide: the frame minus the name column, the `" │ "` separator,
/// the focus gutter and, with timing on, the timestamp column.
///
/// Upstream `tui/runtime.rs` `calculate_content_width` (`width - 9 - 3 - 2 -
/// (14 if timing)`).
pub fn content_width_for_frame(frame_width: usize, show_timing: bool) -> usize {
    let timing_width = if show_timing { TIMESTAMP_WIDTH } else { 0 };
    frame_width.saturating_sub(NAME_WIDTH + 3 + GUTTER_WIDTH + timing_width)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolOutputId(pub String);

/// Controls how tool calls and results are displayed
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolDisplayMode {
    #[default]
    Hidden,
    Truncated,
    Full,
}

impl ToolDisplayMode {
    /// Cycle to the next mode: Summary → Truncated → Full → Summary
    pub fn next(self) -> Self {
        match self {
            Self::Hidden => Self::Truncated,
            Self::Truncated => Self::Full,
            Self::Full => Self::Hidden,
        }
    }

    pub fn is_summary(self) -> bool {
        matches!(self, Self::Hidden)
    }

    /// Whether full or truncated tool details should be rendered
    pub fn shows_details(self) -> bool {
        !matches!(self, Self::Hidden)
    }

    /// Whether tools should be included in exported text
    pub fn is_visible(self) -> bool {
        self.shows_details()
    }

    /// Fixed-width label for the status bar (3 chars each)
    pub fn status_label(self) -> &'static str {
        match self {
            Self::Hidden => "sum",
            Self::Truncated => "trn",
            Self::Full => "all",
        }
    }
}

/// Options for rendering a conversation
#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    pub tool_display: ToolDisplayMode,
    pub show_thinking: bool,
    pub show_timing: bool,
    pub content_width: usize,
    pub expanded_tool_outputs: BTreeSet<ToolOutputId>,
}

/// Tracks the line range of a single message (User or Assistant entry) in the rendered output
#[derive(Clone, Debug)]
pub struct MessageRange {
    /// Index of the JSONL entry (line number in the file, 0-based, counting only parsed entries)
    pub entry_index: usize,
    /// Start line in rendered output (inclusive)
    pub start_line: usize,
    /// End line in rendered output (exclusive, excludes trailing blank)
    pub end_line: usize,
}

/// Result of rendering a conversation
#[derive(Clone, Debug)]
pub struct RenderedConversation {
    pub lines: Vec<RenderedLine>,
    pub messages: Vec<MessageRange>,
}

/// Format an ISO 8601 timestamp to Mon DD HH:MM in local time
pub fn format_timestamp(iso_timestamp: &str) -> Option<String> {
    use chrono::{DateTime, Local};
    // Parse RFC 3339 timestamp (handles timezone offsets) and convert to local time
    DateTime::parse_from_rfc3339(iso_timestamp)
        .ok()
        .map(|dt| dt.with_timezone(&Local).format("%b %d %H:%M").to_string())
}

#[derive(Debug)]
pub struct RenderableEntry {
    pub entry_index: usize,
    entry: LogEntry,
}

impl RenderableEntry {
    /// An entry at `entry_index` of the normalized entry list (see
    /// [`parse_conversation_file`]).
    pub fn new(entry_index: usize, entry: LogEntry) -> Self {
        Self { entry_index, entry }
    }

    /// The parsed log entry.
    pub fn entry(&self) -> &LogEntry {
        &self.entry
    }
}

pub fn parse_conversation_file(file_path: &Path) -> std::io::Result<Vec<RenderableEntry>> {
    Ok(parse_conversation_file_cancellable(file_path, &|| false)?.unwrap_or_default())
}

/// [`parse_conversation_file`] that stops early (`Ok(None)`) when `cancelled`
/// returns true (polled every 64 lines).
pub fn parse_conversation_file_cancellable(
    file_path: &Path,
    cancelled: &dyn Fn() -> bool,
) -> std::io::Result<Option<Vec<RenderableEntry>>> {
    let Some(normalized) = crate::history::normalized_log_entries_cancellable(file_path, cancelled)
        .map_err(|error| std::io::Error::other(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(
        normalized
            .into_iter()
            .map(|(_, entry)| entry)
            .enumerate()
            .filter_map(|(entry_index, entry)| {
                (!matches!(entry, LogEntry::FileHistorySnapshot { .. }))
                    .then_some(RenderableEntry { entry_index, entry })
            })
            .collect(),
    ))
}

pub fn render_parsed_conversation(
    entries: &[RenderableEntry],
    options: &RenderOptions,
) -> RenderedConversation {
    let mut lines = Vec::new();
    let mut messages = Vec::new();
    let mut pending_tool_summary: Option<PendingToolSummary> = None;

    for (parsed_idx, parsed) in entries.iter().enumerate() {
        if options.tool_display.is_summary()
            && try_extend_or_start_pending_summary(
                &mut lines,
                &mut messages,
                &mut pending_tool_summary,
                entries,
                parsed_idx,
                options,
            )
        {
            continue;
        }

        flush_tool_summary(
            &mut lines,
            &mut messages,
            &mut pending_tool_summary,
            entries,
            options,
        );

        render_entry_with_range(&mut lines, &mut messages, parsed, options);
    }

    flush_tool_summary(
        &mut lines,
        &mut messages,
        &mut pending_tool_summary,
        entries,
        options,
    );

    postprocess_blank_lines(&mut lines, &mut messages);

    RenderedConversation { lines, messages }
}

/// Handle a parsed entry while in summary tool-display mode.
///
/// Returns `true` when the entry was absorbed into (or started) a pending
/// summary group and should be skipped by the normal render path.
fn try_extend_or_start_pending_summary(
    lines: &mut Vec<RenderedLine>,
    messages: &mut Vec<MessageRange>,
    pending: &mut Option<PendingToolSummary>,
    entries: &[RenderableEntry],
    parsed_idx: usize,
    options: &RenderOptions,
) -> bool {
    let parsed = &entries[parsed_idx];
    let entry_index = parsed.entry_index;
    let entry = &parsed.entry;

    if let Some((parent_id, agent, timestamp, summary)) =
        tool_only_assistant_summary(entry, options)
    {
        match pending {
            Some(p) if p.parent_id.as_deref() == parent_id && p.agent.as_deref() == agent => {
                p.last_parsed_idx = parsed_idx;
                p.summary.merge(summary);
            }
            _ => {
                flush_tool_summary(lines, messages, pending, entries, options);
                *pending = Some(PendingToolSummary {
                    id: make_tool_summary_output_id(entry_index, parent_id),
                    first_entry_index: entry_index,
                    first_parsed_idx: parsed_idx,
                    last_parsed_idx: parsed_idx,
                    parent_id: parent_id.map(str::to_string),
                    agent: agent.map(str::to_string),
                    timestamp: timestamp.map(str::to_string),
                    summary,
                });
            }
        }
        return true;
    }

    if user_entry_is_only_tool_results(entry, options) {
        if let Some(p) = pending {
            p.last_parsed_idx = parsed_idx;
        }
        return true;
    }

    false
}

/// Render one parsed entry and, if it produced a navigable message,
/// append a `MessageRange` that excludes any trailing blank line.
fn render_entry_with_range(
    lines: &mut Vec<RenderedLine>,
    messages: &mut Vec<MessageRange>,
    parsed: &RenderableEntry,
    options: &RenderOptions,
) {
    let entry_index = parsed.entry_index;
    let entry = &parsed.entry;
    let is_message = matches!(entry, LogEntry::User { .. } | LogEntry::Assistant { .. })
        || matches!(entry, LogEntry::Progress { data, .. }
            if options.show_thinking && crate::claude::parse_agent_progress(data).is_some());

    let start_line = lines.len();
    render_entry(lines, entry_index, entry, options);
    let end_line = lines.len();

    if !is_message {
        return;
    }
    if let Some(range) =
        message_range_excluding_trailing_blank(lines, start_line, end_line, entry_index)
    {
        messages.push(range);
    }
}

/// If the rendered slice produced any non-blank lines, return a
/// `MessageRange` whose `end_line` excludes a trailing blank.
fn message_range_excluding_trailing_blank(
    lines: &[RenderedLine],
    start_line: usize,
    end_line: usize,
    entry_index: usize,
) -> Option<MessageRange> {
    if end_line <= start_line {
        return None;
    }
    let effective_end = if lines.get(end_line - 1).is_some_and(|l| l.spans.is_empty()) {
        end_line - 1
    } else {
        end_line
    };
    if effective_end <= start_line {
        return None;
    }
    Some(MessageRange {
        entry_index,
        start_line,
        end_line: effective_end,
    })
}

/// Collapse consecutive blank rendered lines and remap message ranges so
/// they continue to point at their original visible content.
///
/// Multiple render helpers each push a trailing blank line, which can
/// produce adjacent blanks when a tool result emits empty output. The
/// dedup pass removes any blank line whose immediate predecessor is also
/// blank, and the remap pass shifts every range start/end onto the new
/// line indices, clamping ranges that ended on a removed blank.
fn postprocess_blank_lines(lines: &mut Vec<RenderedLine>, messages: &mut Vec<MessageRange>) {
    let mut removed = vec![false; lines.len()];
    let mut i = 1;
    while i < lines.len() {
        if lines[i].spans.is_empty() && lines[i - 1].spans.is_empty() {
            removed[i] = true;
        }
        i += 1;
    }

    // Build index mapping: old line index -> new line index. Removed
    // entries get the index they would collapse onto; they are never
    // dereferenced for surviving ranges because the remap below walks
    // backward off any removed terminator first.
    let mut new_index = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for (idx, &is_removed) in removed.iter().enumerate() {
        if is_removed {
            new_index.push(idx - offset);
            offset += 1;
        } else {
            new_index.push(idx - offset);
        }
    }
    let total_after = lines.len() - offset;

    // Compact in place.
    let mut write = 0;
    for (read, &is_removed) in removed.iter().enumerate() {
        if !is_removed {
            if write != read {
                lines.swap(write, read);
            }
            write += 1;
        }
    }
    lines.truncate(total_after);

    for msg in messages.iter_mut() {
        msg.start_line = new_index[msg.start_line];
        if msg.end_line > 0 && msg.end_line <= new_index.len() {
            // end_line is exclusive — find the new index of the last
            // non-removed line before it and add 1.
            let mut last = msg.end_line - 1;
            while last > msg.start_line && removed[last] {
                last -= 1;
            }
            msg.end_line = new_index[last] + 1;
        } else if msg.end_line == new_index.len() {
            msg.end_line = total_after;
        }
        msg.end_line = msg.end_line.min(total_after);
        msg.start_line = msg.start_line.min(msg.end_line);
    }

    messages.retain(|m| m.start_line < m.end_line);
}

/// Render a conversation file to lines for display in the TUI viewer
pub fn render_conversation(
    file_path: &Path,
    options: &RenderOptions,
) -> std::io::Result<RenderedConversation> {
    let entries = parse_conversation_file(file_path)?;
    Ok(render_parsed_conversation(&entries, options))
}

#[cfg(test)]
mod tests;
