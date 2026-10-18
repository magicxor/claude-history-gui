//! Conversation viewer.
//!
//! Three layers, all UI-agnostic:
//!
//! - [`ledger`]: upstream's TUI viewer renderer (`tui/viewer/**`), moved
//!   verbatim. It produces monospace rows of styled spans
//!   ([`RenderedLine`]) plus [`MessageRange`]s and clickable
//!   [`ToolOutputId`]s, laid out for a column width. A GUI can draw it with a
//!   monospace font for exact parity with the terminal app.
//! - [`document`]: a semantic, layout-free [`ConversationDocument`] built by
//!   the same rules (skipped entry kinds, templates and fixed block order,
//!   summary-mode grouping, tool output ids, truncation and expansion,
//!   subagent / agent-progress / Pi metadata handling, timing slots). A GUI
//!   renders it natively (markdown, collapsible tool cards).
//! - [`state`]: [`ViewerModel`] (cached parsed entries, preferences,
//!   expanded outputs) and the view-state rules of upstream
//!   `tui/app/view_state.rs`: in-view search, message navigation, re-render
//!   anchoring and the Esc order.
//!
//! ## Typical flow (document renderer)
//!
//! 1. [`ViewerModel::open`] once; keep the [`ConversationDocument`] from
//!    [`ViewerModel::document`] until preferences or the expanded set
//!    change (it is rebuilt from the cached entries, not the file).
//! 2. A click on a block with a [`DocBlock::toggle_id`] calls
//!    [`ViewerModel::toggle_expanded`]; a click inside a navigable message
//!    calls [`MessageNav::focus`] with its
//!    [`ConversationDocument::nav_position`] (upstream does both on one click).
//! 3. Before `t` / `T` / `i` / an expansion, capture the anchor with
//!    [`capture_anchor_entry`]; after rebuilding, resolve it with
//!    [`find_entry_or_prev`], [`MessageNav::refocus`] the result and scroll
//!    it back to the same place, then [`ViewSearch::refresh`] an active
//!    search.
//! 4. In-view search: build a [`DocSearchIndex`] per document and drive
//!    [`ViewSearch`] with `|q| index.find(q)`; jump to and focus the returned
//!    match's message. Esc goes through [`escape`].
//! 5. Copy / export: [`ViewerModel::message_text`] (focused message),
//!    [`crate::export::generate_export`] / [`crate::export::export_to_path`]
//!    with [`ViewerModel::export_options`], and
//!    [`ViewerModel::plain_display`] for the CLI `--plain` text.
//!
//! The ledger renderer uses the same model with [`ViewerModel::ledger`] and
//! the line-based helpers ([`search_lines`], [`capture_anchor`] /
//! [`resolve_scroll`], [`first_message_in_viewport`], [`scroll_to_show`],
//! [`message_idx_at_line`]).

pub mod document;
pub mod ledger;
pub mod state;

use std::collections::BTreeSet;

pub use crate::commands::process_command_message;
pub use document::{
    BodyLineKind, ConversationDocument, DocBlock, DocBlockKind, DocMessage, DocMessageKind,
    DocSubagentToolResult, DocTiming, DocToolCall, DocToolResult, Fold, LabelRole, ToolBody,
    ToolBodyLine, build_document, markdown_plain_text,
};
pub use ledger::{
    GUTTER_WIDTH, LineStyle, MessageRange, NAME_WIDTH, RenderOptions, RenderableEntry,
    RenderedConversation, RenderedLine, TIMESTAMP_WIDTH, TRUNCATED_BODY_LINES,
    TRUNCATED_RESULT_LINES, ToolDisplayMode, ToolOutputId, content_width_for_frame, fitted_name,
    format_timestamp, parse_conversation_file, render_conversation, render_parsed_conversation,
};
pub use state::{
    DocSearchIndex, MessageNav, ScrollAnchor, SearchMatch, ViewEscape, ViewSearch, ViewSearchMode,
    ViewerModel, capture_anchor, capture_anchor_entry, escape, find_entry_or_prev,
    find_message_idx_or_prev, first_message_in_viewport, line_matches_query, message_idx_at_line,
    resolve_scroll, scroll_to_show, search_lines,
};

/// Viewer display preferences: the `t` / `T` / `i` toggles.
///
/// Upstream keeps these at application level so they survive opening
/// another conversation in the same session (`tui/app.rs:74-79`); timing
/// always starts off there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewPrefs {
    /// Summary (`Hidden`) / truncated / full tool display.
    pub tool_display: ToolDisplayMode,
    /// Thinking blocks, subagent entries and `agent_progress` entries.
    pub show_thinking: bool,
    /// The timestamp column.
    pub show_timing: bool,
}

impl ViewPrefs {
    /// Ledger render options for a content width (in columns; see
    /// [`content_width_for_frame`]) and the expanded tool outputs.
    pub fn render_options(
        &self,
        content_width: usize,
        expanded: &BTreeSet<ToolOutputId>,
    ) -> RenderOptions {
        RenderOptions {
            tool_display: self.tool_display,
            show_thinking: self.show_thinking,
            show_timing: self.show_timing,
            content_width,
            expanded_tool_outputs: expanded.clone(),
        }
    }

    /// Export / copy options matching the view (upstream `perform_export`
    /// and `copy_focused_message`): tools are included in full whenever
    /// they are visible (truncated or full mode), never summarized.
    pub fn export_options(&self) -> crate::export::ExportOptions {
        crate::export::ExportOptions {
            show_tools: self.tool_display.is_visible(),
            show_thinking: self.show_thinking,
        }
    }
}
