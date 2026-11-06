//! Layout-free conversation document.
//!
//! [`build_document`] turns a transcript's entries
//! ([`crate::history::view::entries`]) into a [`ConversationDocument`]:
//! messages in render order, each with a label, presentation flags and a list
//! of semantic blocks (markdown text, thinking, tool summary, call lines,
//! tool calls and results). It follows exactly the rules of the
//! [ledger](super::ledger) renderer and reuses its helpers, so both agree on:
//!
//! - which entries render (hidden records and notes that are not
//!   searchable never do; subagent entries and `agent_progress` entries only
//!   with `show_thinking`);
//! - the user template (text after [`process_command_message`], text
//!   blocks joined with a blank line, then tool results) and the assistant
//!   template in fixed order regardless of the JSON block order (text blocks,
//!   tool summary in summary mode or one call line per call in calls mode,
//!   tool calls in truncated/full mode, thinking for top-level assistants);
//!   off mode (upstream d0aca95) has no tool summary, call or result, so a
//!   tool-only message has no block and is left out (see below);
//! - summary- and calls-mode grouping of consecutive tool-only assistant
//!   entries with the same parent and speaker, absorbing tool-result-only
//!   user entries, the summary sentence (`"Searched for 2 patterns, read 1
//!   file"`) and the call lines (`"Bash: cargo test"`, upstream a60dfe9),
//!   where an expanded summary group shows all its calls and results and an
//!   expanded call line just that call and its result (upstream a1c88f8);
//! - tool output ids ([`ToolOutputId`]), which are stable across re-renders
//!   and shared with the ledger, so one expanded set drives both;
//! - truncation and expansion ([`Fold`]), label text, roles and dimming;
//! - timestamps: [`DocMessage::timestamp`] is the entry's stamp in the
//!   ledger format (`"%b %d %H:%M"`, local time) and [`DocBlock::timing`]
//!   mirrors the ledger's timing column slot for the block's first row,
//!   including upstream's quirks (dimmed blocks never carry the stamp, a
//!   second assistant text block has no column).
//!
//! ## Truncation counts: unwrapped rows vs ledger rows
//!
//! The document has no width, so truncation is counted with **no
//! wrapping**: tool-call bodies in the lines of the body formatted with no
//! Bash wrapping, top-level tool results in the rows of their markdown
//! rendered at unbounded width ([`DocToolResult::rows`]: fence rows dropped,
//! blank rows kept, a trailing line break of plain text adds an empty row),
//! subagent results in the lines of their text. The ledger counts the same
//! rows at its content width, where long lines hard-wrap and a Bash command
//! is wrapped at `content_width - 6`. Both agree whenever no line is wider
//! than the content width. The ledger-equivalent numbers at a given width
//! are available through [`DocToolCall::ledger_hidden_lines`] and
//! [`DocToolResult::ledger_hidden_lines`] (and the `ledger_*` siblings);
//! subagent results are never wrapped, so their counts are identical.
//!
//! A top-level tool result whose markdown renders no row at all (a bare
//! fence) shows nothing in the ledger, so it is left out of the document;
//! a message left with no block is left out too (the ledger records no
//! message range for it).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use super::ViewPrefs;
use super::ledger::{
    RenderOptions, RenderableEntry, RowTiming, TRUNCATED_BODY_LINES, TRUNCATED_RESULT_LINES,
    TimingSlot, ToolActivitySummary, ToolDisplayMode, ToolOutputId, ToolOutputKind,
    assistant_label, call_line, entry_shows_nothing, format_timestamp, group_call_result,
    group_messages, group_summary_line, make_tool_output_id, make_tool_summary_output_id,
    render_markdown_to_lines, subagent_label, summarize_tool_calls, summary_rows,
    tool_only_assistant_summary, tool_result_display_text, user_entry_is_only_tool_results,
};
use crate::commands::process_command_message;
use crate::history::model::{Message, Part, Role};
use crate::history::view::EntryKind;
use crate::theme::{Rgb, Theme};
use crate::tool_format;

/// A conversation as a list of messages in render order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConversationDocument {
    pub messages: Vec<DocMessage>,
}

/// What produced a [`DocMessage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocMessageKind {
    /// A user entry (top-level, skill expansion, or subagent with
    /// [`DocMessage::is_subagent`]).
    User,
    /// An assistant entry (top-level or subagent).
    Assistant,
    /// A searchable note of the source (a Pi/OMP custom message, the start
    /// of a Codex fork's own records), rendered with the user template. Not
    /// navigable.
    Note,
    /// The user side of a Claude `agent_progress` record.
    AgentProgressUser,
    /// The assistant side of a Claude `agent_progress` record.
    AgentProgressAssistant,
    /// Consecutive tool-only assistant entries collapsed into one summary
    /// (summary tool-display mode) or one call line per call (calls mode).
    ToolSummaryGroup,
}

/// The palette role of a message label (ledger name column color).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelRole {
    /// `You` and subagent / agent-progress user labels.
    TextPrimary,
    /// Assistant labels (`Claude`, `Pi`, `OMP`, `↳…`).
    Accent,
    /// Tool summary group labels.
    AccentDim,
    /// Note labels.
    TextSecondary,
}

impl LabelRole {
    /// The color of this role in `theme`.
    pub fn color(self, theme: &Theme) -> Rgb {
        match self {
            LabelRole::TextPrimary => theme.text_primary,
            LabelRole::Accent => theme.accent,
            LabelRole::AccentDim => theme.accent_dim,
            LabelRole::TextSecondary => theme.text_secondary,
        }
    }
}

/// The ledger's timing column for the first row of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocTiming {
    /// No timing column: timing is off, the entry's timestamp is missing or
    /// invalid, or the block is a later assistant text block (upstream
    /// quirk).
    Disabled,
    /// The column is present but blank (continuation under an earlier
    /// stamp, nested or dimmed blocks, agent progress).
    Pad,
    /// The column carries this stamp (`"%b %d %H:%M"`, local time).
    Stamp(String),
}

impl DocTiming {
    fn from_slot(slot: TimingSlot<'_>) -> Self {
        match slot {
            TimingSlot::Disabled => DocTiming::Disabled,
            TimingSlot::Pad => DocTiming::Pad,
            TimingSlot::Stamp(stamp) => DocTiming::Stamp(stamp.to_string()),
        }
    }

    /// The stamp text, when this block carries it.
    pub fn stamp(&self) -> Option<&str> {
        match self {
            DocTiming::Stamp(stamp) => Some(stamp),
            _ => None,
        }
    }
}

/// One rendered message.
#[derive(Clone, Debug, PartialEq)]
pub struct DocMessage {
    /// Index among the transcript's entries
    /// ([`RenderableEntry::entry_index`]). For a summary group, the group's
    /// first entry. The same index space as
    /// [`crate::export::extract_message_text`].
    pub entry_index: usize,
    pub kind: DocMessageKind,
    /// Full label text: `You`, the assistant label (the source's assistant:
    /// `Claude`, `Pi`, `OMP`, `Codex`, or a Codex subagent's name),
    /// `↳{short parent id}` for subagents, `↳{agent id, 7 chars}` for agent
    /// progress, or the note's label.
    /// The ledger shows it fitted to 9 cells ([`super::fitted_name`]).
    pub label: String,
    pub label_role: LabelRole,
    /// Bold label (top-level `You` and assistant labels, except skill
    /// expansions).
    pub bold: bool,
    /// The ledger mutes the label and the message's own text, i.e. its
    /// [`DocBlockKind::Markdown`] blocks (subagents, agent progress, skill
    /// expansions, notes, subagent summary groups).
    ///
    /// It is not a style for every block. A [`DocToolCall`] carries its own
    /// [`DocToolCall::dimmed`]; a [`DocBlockKind::ToolSummary`] sentence, a
    /// [`DocBlockKind::ToolCallLine`] and a
    /// [`DocBlockKind::SubagentToolResult`] are always muted; a
    /// [`DocBlockKind::Thinking`] block keeps its thinking style; and a
    /// [`DocBlockKind::ToolResult`] always keeps the normal, un-muted style,
    /// including the results of a skill-expansion user message and those
    /// inside an expanded subagent summary group (the ledger's
    /// `render_tool_result`, as upstream).
    pub dimmed: bool,
    /// Nested (subagent / agent progress) message: tool results use the
    /// subagent presentation and thinking is never shown.
    pub is_subagent: bool,
    /// The entry's timestamp formatted like the ledger column
    /// (`"%b %d %H:%M"`, local time); only with `show_timing`, `None` for
    /// agent progress and for missing or invalid timestamps. Which block
    /// shows it in the ledger is in [`DocBlock::timing`].
    pub timestamp: Option<String>,
    /// The entry's time, whatever the preferences; for a summary group the
    /// first entry's. Agent progress has none (its record's time is not the
    /// subagent message's), and the ledger never shows one for it.
    pub raw_timestamp: Option<DateTime<Utc>>,
    /// Whether the message is a navigation / copy unit, i.e. the ledger
    /// records a [`super::MessageRange`] for it. Navigable messages appear in
    /// the same order as the ledger's message ranges.
    pub navigable: bool,
    pub blocks: Vec<DocBlock>,
}

/// One block of a message.
#[derive(Clone, Debug, PartialEq)]
pub struct DocBlock {
    /// The ledger timing slot of the block's first row.
    pub timing: DocTiming,
    pub kind: DocBlockKind,
}

/// The content of a block.
#[derive(Clone, Debug, PartialEq)]
pub enum DocBlockKind {
    /// Markdown source: user text after command-tag processing (text blocks
    /// joined by a blank line), one assistant text block, aggregated
    /// agent-progress text (not command-processed), or a note's text.
    Markdown {
        text: String,
    },
    /// A thinking block (markdown; the ledger renders it italic in the
    /// `thinking_text` color under a `Thinking` label).
    Thinking {
        text: String,
    },
    /// Tool activity sentence (summary mode). `id` is set for a collapsed
    /// summary group, which a click expands; a mixed text + tools assistant
    /// message gets a non-clickable sentence (`id: None`).
    ToolSummary {
        id: Option<ToolOutputId>,
        sentence: String,
    },
    /// One tool call on one line, without its result (calls mode, upstream
    /// a60dfe9): the call's header as formatted at width 0, whole (the
    /// ledger cuts it to the content width with `…`). Each call of a group
    /// is one block with its own row id (`…:kind:row:…`, upstream a1c88f8),
    /// so a click expands just that call (into a
    /// [`DocBlockKind::ToolSummaryDetails`] with the call and its result)
    /// while the other calls of the group stay on one line; a mixed text +
    /// tools assistant message gets non-clickable lines (`id: None`). Like
    /// the summary sentence, later lines of a group take the continuation
    /// timing slot.
    ToolCallLine {
        id: Option<ToolOutputId>,
        header: String,
    },
    /// An expanded summary group or calls row. A summary group (summary
    /// mode) shows every call and result of the group; a calls row (calls
    /// mode, upstream a1c88f8) shows its call followed by the call's result
    /// when the group holds it. The items are in truncated mode and each is
    /// individually expandable; a click on the group or row (the ledger: its
    /// first row) collapses it again via `id`.
    ToolSummaryDetails {
        id: ToolOutputId,
        items: Vec<DocBlock>,
    },
    ToolCall(DocToolCall),
    ToolResult(DocToolResult),
    SubagentToolResult(DocSubagentToolResult),
}

/// Truncation state of a tool body, in unwrapped lines (rows for a
/// top-level result; see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// Full tool display: everything shown, nothing to toggle.
    Full,
    /// Truncated tool display, not expanded: at most `limit` leading lines
    /// are shown (3 for call bodies, 4 for results).
    Collapsed { limit: usize },
    /// Truncated tool display, expanded: everything shown; toggling
    /// collapses again.
    Expanded,
}

impl Fold {
    fn for_mode(mode: ToolDisplayMode, expanded: bool, limit: usize) -> Self {
        match mode {
            ToolDisplayMode::Truncated if expanded => Fold::Expanded,
            ToolDisplayMode::Truncated => Fold::Collapsed { limit },
            ToolDisplayMode::Full
            | ToolDisplayMode::Summary
            | ToolDisplayMode::Calls
            | ToolDisplayMode::Off => Fold::Full,
        }
    }

    /// Lines hidden behind `"(N more lines...)"` for a body of
    /// `total_lines` lines.
    pub fn hidden_lines(self, total_lines: usize) -> Option<usize> {
        match self {
            Fold::Collapsed { limit } if total_lines > limit => Some(total_lines - limit),
            _ => None,
        }
    }

    /// Lines shown for a body of `total_lines` lines.
    pub fn visible_lines(self, total_lines: usize) -> usize {
        total_lines - self.hidden_lines(total_lines).unwrap_or(0)
    }

    /// Whether a click toggles the expanded state (the ledger's
    /// `clickable`): a truncated body, or an expanded one.
    pub fn is_toggleable(self, total_lines: usize) -> bool {
        match self {
            Fold::Full => false,
            Fold::Collapsed { limit } => total_lines > limit,
            Fold::Expanded => true,
        }
    }

    pub fn is_expanded(self) -> bool {
        matches!(self, Fold::Expanded)
    }
}

/// How the ledger colors a tool body line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyLineKind {
    /// Starts with `"+ "` (`diff_add`).
    Add,
    /// Starts with `"- "` (`diff_remove`).
    Remove,
    /// Anything else (muted).
    Plain,
}

/// One line of a tool call body, verbatim (including any `"+ "`/`"- "`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolBodyLine {
    pub kind: BodyLineKind,
    pub text: String,
}

/// A tool call body (Task prompt, Bash continuation lines, Edit diff, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolBody {
    pub lines: Vec<ToolBodyLine>,
}

impl ToolBody {
    fn from_text(text: &str) -> Self {
        let lines = text
            .lines()
            .map(|line| ToolBodyLine {
                kind: if line.starts_with("+ ") {
                    BodyLineKind::Add
                } else if line.starts_with("- ") {
                    BodyLineKind::Remove
                } else {
                    BodyLineKind::Plain
                },
                text: line.to_string(),
            })
            .collect();
        Self { lines }
    }
}

/// A tool call, formatted with [`tool_format::format_tool_call`] at width 0
/// (no Bash wrapping).
#[derive(Clone, Debug, PartialEq)]
pub struct DocToolCall {
    pub id: ToolOutputId,
    pub name: String,
    /// The raw tool input.
    pub input: serde_json::Value,
    /// `"Bash: ls"`, `"Read: src/main.rs"`, `"Grep: \"x\" in ."`, …
    pub header: String,
    pub body: Option<ToolBody>,
    /// The ledger renders the call muted (subagent messages and expanded
    /// summary groups).
    pub dimmed: bool,
    pub fold: Fold,
}

impl DocToolCall {
    /// Body lines (source lines; 0 without a body).
    pub fn total_lines(&self) -> usize {
        self.body.as_ref().map_or(0, |body| body.lines.len())
    }

    /// Body lines shown in the current state.
    pub fn visible_body_lines(&self) -> &[ToolBodyLine] {
        match &self.body {
            Some(body) => &body.lines[..self.fold.visible_lines(body.lines.len())],
            None => &[],
        }
    }

    /// Source lines hidden behind `"(N more lines...)"`.
    pub fn hidden_lines(&self) -> Option<usize> {
        self.fold.hidden_lines(self.total_lines())
    }

    /// Whether a click toggles the expanded state.
    pub fn expandable(&self) -> bool {
        let total = self.total_lines();
        total > 0 && self.fold.is_toggleable(total)
    }

    pub fn expanded(&self) -> bool {
        self.fold.is_expanded()
    }

    /// Body rows of the ledger at `content_width` (the Bash header/body
    /// split depends on the width).
    pub fn ledger_body_lines(&self, content_width: usize) -> usize {
        tool_format::format_tool_call(&self.name, &self.input, content_width)
            .body
            .map_or(0, |body| body.lines().count())
    }

    /// The `"(N more lines...)"` count the ledger shows at `content_width`.
    pub fn ledger_hidden_lines(&self, content_width: usize) -> Option<usize> {
        self.fold
            .hidden_lines(self.ledger_body_lines(content_width))
    }

    /// Whether the ledger at `content_width` makes the call clickable.
    pub fn ledger_expandable(&self, content_width: usize) -> bool {
        let total = self.ledger_body_lines(content_width);
        total > 0 && self.fold.is_toggleable(total)
    }
}

/// A top-level tool result. Always rendered un-muted, even inside a
/// [`DocMessage::dimmed`] message.
///
/// Truncation counts the **rows** the ledger renders for
/// [`DocToolResult::markdown`] (fence rows dropped, blank rows kept), at
/// unbounded width ([`DocToolResult::rows`]); see the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocToolResult {
    pub id: ToolOutputId,
    /// Display text ([`crate::tool_format::format_tool_result`], upstream
    /// 6afae1c): content blocks joined by a blank line, with images,
    /// documents and loaded tools as one-line placeholders
    /// (`[image: image/png]`, `Loaded tool: X`); a tool error as
    /// `Error: ...`; a one-line JSON string and any other value
    /// pretty-printed; `<no content>` without content.
    pub text: String,
    /// What the ledger renders as markdown: `text` fenced as
    /// `` ```text `` unless it already contains a fence.
    pub markdown: String,
    /// Rows of the ledger's rendering of `markdown` with no wrapping: for a
    /// fenced result one row per line of `text` (a trailing line break adds
    /// an empty row), for a markdown result the rendered markdown rows
    /// (fence lines dropped, blank separator rows kept). Never 0: a result
    /// that renders nothing is left out of the document, as the ledger
    /// shows nothing for it.
    pub rows: usize,
    pub fold: Fold,
}

impl DocToolResult {
    /// Whether `text` is rendered verbatim (fenced) rather than as markdown.
    pub fn is_fenced(&self) -> bool {
        !self.text.contains("```")
    }

    /// Rendered rows ([`Self::rows`]), the unit of truncation.
    pub fn total_lines(&self) -> usize {
        self.rows
    }

    /// Rows shown in the current state.
    pub fn visible_rows(&self) -> usize {
        self.fold.visible_lines(self.rows)
    }

    /// The part of `text` shown in the current state: all of it when
    /// nothing is hidden. Otherwise the source that renders the visible
    /// rows: the first lines of a fenced result (one row per line), or the
    /// shortest run of leading lines of a markdown result whose rendering
    /// begins with the visible rows (it may render a few more rows when the
    /// last line also starts the next block, e.g. a table).
    pub fn visible_text(&self) -> String {
        let visible = self.visible_rows();
        if visible >= self.rows {
            self.text.clone()
        } else if self.is_fenced() {
            visible_prefix(&self.text, visible)
        } else {
            markdown_prefix_for_rows(&self.text, visible)
        }
    }

    /// [`Self::visible_text`] in the form the ledger renders it (fenced
    /// unless it contains a fence).
    pub fn visible_markdown(&self) -> String {
        let visible = self.visible_text();
        if self.is_fenced() {
            fence_text(&visible)
        } else {
            visible
        }
    }

    /// The visible rows as plain text, exactly as the ledger shows them
    /// unwrapped (joined by `\n`). A fenced result keeps its source lines
    /// verbatim (tabs are not expanded).
    pub fn visible_plain_text(&self) -> String {
        let visible = self.visible_rows();
        if self.is_fenced() {
            if visible >= self.rows {
                self.text.clone()
            } else {
                visible_prefix(&self.text, visible)
            }
        } else {
            render_markdown_to_lines(&self.markdown, usize::MAX)
                .iter()
                .take(visible)
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|(text, _)| text.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// Rows hidden behind `"(N more lines...)"`.
    pub fn hidden_lines(&self) -> Option<usize> {
        self.fold.hidden_lines(self.rows)
    }

    /// Whether a click toggles the expanded state (the ledger's
    /// `clickable`: some row is shown and it is truncated or expanded).
    pub fn expandable(&self) -> bool {
        self.rows > 0 && self.fold.is_toggleable(self.rows)
    }

    pub fn expanded(&self) -> bool {
        self.fold.is_expanded()
    }

    /// Rows of the ledger's markdown rendering at `content_width` (long
    /// lines wrap, so this can exceed [`Self::rows`]).
    pub fn ledger_lines(&self, content_width: usize) -> usize {
        render_markdown_to_lines(&self.markdown, content_width).len()
    }

    /// The `"(N more lines...)"` count the ledger shows at `content_width`.
    pub fn ledger_hidden_lines(&self, content_width: usize) -> Option<usize> {
        self.fold.hidden_lines(self.ledger_lines(content_width))
    }

    /// Whether the ledger at `content_width` makes the result clickable.
    pub fn ledger_expandable(&self, content_width: usize) -> bool {
        let total = self.ledger_lines(content_width);
        total > 0 && self.fold.is_toggleable(total)
    }

    /// The rows the ledger shows for this result at `content_width` in the
    /// current fold (K9): the markdown hard-wrapped exactly like the ledger
    /// (fence rows dropped, blank rows kept), at most the fold's limit when
    /// collapsed, each row as its plain text. A GUI that soft-wraps text
    /// uses this to truncate by visual rows: a 50 KB single-line result
    /// shows at most 4 rows instead of one huge wrapped line.
    pub fn ledger_visible_rows(&self, content_width: usize) -> Vec<String> {
        let rows = render_markdown_to_lines(&self.markdown, content_width);
        let visible = self.fold.visible_lines(rows.len());
        rows.into_iter()
            .take(visible)
            .map(|row| row.spans.into_iter().map(|(text, _)| text).collect())
            .collect()
    }
}

/// Rows of the ledger's markdown rendering with no wrapping.
fn unwrapped_rows(markdown: &str) -> usize {
    render_markdown_to_lines(markdown, usize::MAX).len()
}

/// A rendered row, comparable: the text and style of each span, and
/// whether the row has no spans at all.
type RowKey = (bool, Vec<(String, bool, bool, bool, Option<Rgb>)>);

fn row_keys(markdown: &str) -> Vec<RowKey> {
    render_markdown_to_lines(markdown, usize::MAX)
        .into_iter()
        .map(|line| {
            (
                line.spans.is_empty(),
                line.spans
                    .into_iter()
                    .map(|(text, style)| (text, style.bold, style.italic, style.dimmed, style.fg))
                    .collect(),
            )
        })
        .collect()
}

/// The shortest run of leading lines of `markdown` whose unwrapped
/// rendering begins with the first `visible` rows of the whole text's
/// rendering (trailing blank rows aside: a rendering never ends with one).
/// The whole text always qualifies, so the result never hides a row the
/// ledger shows.
fn markdown_prefix_for_rows(markdown: &str, visible: usize) -> String {
    let full = row_keys(markdown);
    let mut target = &full[..visible.min(full.len())];
    while target.last().is_some_and(|(blank, _)| *blank) {
        target = &target[..target.len() - 1];
    }
    if target.is_empty() {
        return String::new();
    }
    // Byte offset of the end of each source line, line break included.
    let ends: Vec<usize> = markdown
        .split_inclusive('\n')
        .scan(0, |end, line| {
            *end += line.len();
            Some(*end)
        })
        .collect();
    let starts_with_target = |lines: usize| {
        let rows = row_keys(&markdown[..ends[lines - 1]]);
        rows.len() >= target.len() && rows[..target.len()] == *target
    };
    // Gallop, then bisect. `lo` lines never qualify (0 lines render
    // nothing), `hi` lines do (all of them always do).
    let count = ends.len();
    let (mut lo, mut hi) = (0, 1.min(count));
    while hi < count && !starts_with_target(hi) {
        lo = hi;
        hi = (hi * 2).min(count);
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if starts_with_target(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    markdown[..ends[hi - 1]].to_string()
}

/// A tool result inside a subagent or agent-progress message: the ledger
/// shows `"  ↳ Tool │ <Result>"` and then the same display text as a
/// top-level result ([`DocToolResult::text`], upstream 6afae1c), muted,
/// unwrapped and not rendered as markdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocSubagentToolResult {
    pub id: ToolOutputId,
    pub text: String,
    pub fold: Fold,
}

impl DocSubagentToolResult {
    pub fn total_lines(&self) -> usize {
        self.text.lines().count()
    }

    /// The leading lines shown in the current state.
    pub fn visible_text(&self) -> String {
        visible_prefix(&self.text, self.fold.visible_lines(self.total_lines()))
    }

    /// Lines hidden behind `"(N more lines...)"` (identical in the ledger).
    pub fn hidden_lines(&self) -> Option<usize> {
        self.fold.hidden_lines(self.total_lines())
    }

    pub fn expandable(&self) -> bool {
        let total = self.total_lines();
        total > 0 && self.fold.is_toggleable(total)
    }

    pub fn expanded(&self) -> bool {
        self.fold.is_expanded()
    }
}

fn visible_prefix(text: &str, lines: usize) -> String {
    text.lines().take(lines).collect::<Vec<_>>().join("\n")
}

fn fence_text(text: &str) -> String {
    format!("```text\n{}\n```", text)
}

impl DocBlock {
    /// The tool output id that a click on this block toggles, if any
    /// (expandable calls/results, collapsed or expanded summary groups).
    pub fn toggle_id(&self) -> Option<&ToolOutputId> {
        match &self.kind {
            DocBlockKind::ToolSummary { id, .. } | DocBlockKind::ToolCallLine { id, .. } => {
                id.as_ref()
            }
            DocBlockKind::ToolSummaryDetails { id, .. } => Some(id),
            DocBlockKind::ToolCall(call) => call.expandable().then_some(&call.id),
            DocBlockKind::ToolResult(result) => result.expandable().then_some(&result.id),
            DocBlockKind::SubagentToolResult(result) => result.expandable().then_some(&result.id),
            DocBlockKind::Markdown { .. } | DocBlockKind::Thinking { .. } => None,
        }
    }

    /// The tool output id the block carries (whether or not it is
    /// expandable right now).
    pub fn tool_output_id(&self) -> Option<&ToolOutputId> {
        match &self.kind {
            DocBlockKind::ToolSummary { id, .. } | DocBlockKind::ToolCallLine { id, .. } => {
                id.as_ref()
            }
            DocBlockKind::ToolSummaryDetails { id, .. } => Some(id),
            DocBlockKind::ToolCall(call) => Some(&call.id),
            DocBlockKind::ToolResult(result) => Some(&result.id),
            DocBlockKind::SubagentToolResult(result) => Some(&result.id),
            DocBlockKind::Markdown { .. } | DocBlockKind::Thinking { .. } => None,
        }
    }

    /// The text shown for this block in its current state, as plain text:
    /// markdown and thinking through [`markdown_plain_text`], the summary
    /// sentence, a call line's header, the call header followed by the
    /// visible body lines, the visible result lines. Expanded summary groups
    /// join their items with newlines. In-view search
    /// ([`super::DocSearchIndex`]) matches this text.
    pub fn plain_text(&self) -> String {
        match &self.kind {
            DocBlockKind::Markdown { text } | DocBlockKind::Thinking { text } => {
                markdown_plain_text(text)
            }
            DocBlockKind::ToolSummary { sentence, .. } => sentence.clone(),
            DocBlockKind::ToolCallLine { header, .. } => header.clone(),
            DocBlockKind::ToolSummaryDetails { items, .. } => items
                .iter()
                .map(DocBlock::plain_text)
                .collect::<Vec<_>>()
                .join("\n"),
            DocBlockKind::ToolCall(call) => {
                let mut text = call.header.clone();
                for line in call.visible_body_lines() {
                    text.push('\n');
                    text.push_str(&line.text);
                }
                text
            }
            DocBlockKind::ToolResult(result) => result.visible_plain_text(),
            DocBlockKind::SubagentToolResult(result) => result.visible_text(),
        }
    }
}

impl ConversationDocument {
    /// Indices of the navigable messages, in order. Position `k` of this list
    /// corresponds to the ledger's `messages[k]` for the same preferences.
    pub fn navigable_indices(&self) -> Vec<usize> {
        self.messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.navigable)
            .map(|(index, _)| index)
            .collect()
    }

    /// Entry indices of the navigable messages, in order (the anchor list of
    /// [`super::capture_anchor_entry`]).
    pub fn navigable_entry_indices(&self) -> Vec<usize> {
        self.messages
            .iter()
            .filter(|message| message.navigable)
            .map(|message| message.entry_index)
            .collect()
    }

    /// Navigation position of message `message_index`, if it is navigable.
    pub fn nav_position(&self, message_index: usize) -> Option<usize> {
        if !self.messages.get(message_index)?.navigable {
            return None;
        }
        Some(
            self.messages[..message_index]
                .iter()
                .filter(|message| message.navigable)
                .count(),
        )
    }

    /// Message index of navigation position `position`.
    pub fn message_at_nav_position(&self, position: usize) -> Option<usize> {
        self.messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.navigable)
            .nth(position)
            .map(|(index, _)| index)
    }
}

/// The visible text of rendered markdown, unwrapped: what the ledger shows
/// in its content column (fence rows, link URL suffixes and heading markers
/// hidden; list bullets, quote prefixes and table borders kept), with lines
/// joined by `\n`.
pub fn markdown_plain_text(markdown: &str) -> String {
    render_markdown_to_lines(markdown, usize::MAX)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the document for `entries` (see the module documentation for the
/// rules). `expanded` is the set of expanded tool outputs, shared with the
/// ledger.
pub fn build_document(
    entries: &[RenderableEntry<'_>],
    prefs: &ViewPrefs,
    expanded: &BTreeSet<ToolOutputId>,
) -> ConversationDocument {
    let builder = Builder {
        prefs: *prefs,
        // The shared ledger predicates only read `show_thinking`; the summary
        // rows read `tool_display` and `content_width`, which leaves them
        // whole here (the GUI clips them to its columns).
        options: RenderOptions {
            tool_display: prefs.tool_display,
            show_thinking: prefs.show_thinking,
            show_timing: prefs.show_timing,
            content_width: usize::MAX,
            expanded_tool_outputs: BTreeSet::new(),
        },
        expanded,
    };
    let mut messages = Vec::new();
    let mut pending: Option<PendingGroup> = None;

    // Mirrors `ledger::render_parsed_conversation`.
    for (parsed_idx, parsed) in entries.iter().enumerate() {
        if prefs.tool_display.groups_tools()
            && builder.try_extend_or_start_pending_summary(
                &mut messages,
                &mut pending,
                entries,
                parsed_idx,
            )
        {
            continue;
        }

        builder.flush_tool_summary(&mut messages, &mut pending, entries);

        if let Some(message) = builder.entry_message(parsed) {
            messages.push(message);
        }
    }

    builder.flush_tool_summary(&mut messages, &mut pending, entries);

    ConversationDocument { messages }
}

/// Label and presentation of one message (the ledger's `MessageStyle`).
struct Style {
    label: String,
    role: LabelRole,
    dimmed: bool,
    bold: bool,
    is_subagent: bool,
}

impl Style {
    fn for_user(parent_id: Option<&str>, parts: &[Part]) -> Self {
        if let Some(parent) = parent_id {
            return Self {
                label: subagent_label(parent),
                role: LabelRole::TextPrimary,
                dimmed: true,
                bold: false,
                is_subagent: true,
            };
        }
        let is_skill = parts.iter().any(|part| {
            matches!(part, Part::Text(text)
                if text.trim().starts_with("Base directory for this skill:"))
        });
        Self {
            label: "You".to_string(),
            role: LabelRole::TextPrimary,
            dimmed: is_skill,
            bold: !is_skill,
            is_subagent: false,
        }
    }

    fn for_assistant(parent_id: Option<&str>, speaker: &str) -> Self {
        match parent_id {
            Some(parent) => Self {
                label: subagent_label(parent),
                role: LabelRole::Accent,
                dimmed: true,
                bold: false,
                is_subagent: true,
            },
            None => Self {
                label: speaker.to_string(),
                role: LabelRole::Accent,
                dimmed: false,
                bold: true,
                is_subagent: false,
            },
        }
    }

    fn for_note(label: &str) -> Self {
        Self {
            label: label.to_string(),
            role: LabelRole::TextSecondary,
            dimmed: true,
            bold: false,
            is_subagent: false,
        }
    }

    fn for_agent(short_id: &str, role: LabelRole) -> Self {
        Self {
            label: format!("↳{}", short_id),
            role,
            dimmed: true,
            bold: false,
            is_subagent: true,
        }
    }
}

/// Per-entry context (the ledger's `EntryCtx`).
struct Ctx<'a> {
    style: Style,
    parent_id: Option<&'a str>,
    entry_index: usize,
}

/// A summary group being accumulated (the ledger's `PendingToolSummary`).
struct PendingGroup {
    id: ToolOutputId,
    first_entry_index: usize,
    first_parsed_idx: usize,
    last_parsed_idx: usize,
    parent_id: Option<String>,
    speaker: String,
    timestamp: Option<DateTime<Utc>>,
    summary: ToolActivitySummary,
}

struct Builder<'a> {
    prefs: ViewPrefs,
    options: RenderOptions,
    expanded: &'a BTreeSet<ToolOutputId>,
}

impl Builder<'_> {
    fn entry_timestamp(&self, timestamp: Option<DateTime<Utc>>) -> Option<String> {
        timestamp
            .filter(|_| self.prefs.show_timing)
            .map(format_timestamp)
    }

    fn message(
        &self,
        ctx: Ctx<'_>,
        kind: DocMessageKind,
        timestamp: Option<String>,
        raw_timestamp: Option<DateTime<Utc>>,
        blocks: Vec<DocBlock>,
    ) -> Option<DocMessage> {
        if blocks.is_empty() {
            return None;
        }
        Some(DocMessage {
            entry_index: ctx.entry_index,
            kind,
            label: ctx.style.label,
            label_role: ctx.style.role,
            bold: ctx.style.bold,
            dimmed: ctx.style.dimmed,
            is_subagent: ctx.style.is_subagent,
            timestamp,
            raw_timestamp,
            navigable: kind != DocMessageKind::Note,
            blocks,
        })
    }

    /// The ledger's `render_entry`.
    fn entry_message(&self, parsed: &RenderableEntry<'_>) -> Option<DocMessage> {
        let entry_index = parsed.entry_index;
        let entry = parsed.entry();
        match entry.kind {
            EntryKind::Hidden
            | EntryKind::Note {
                searchable: false, ..
            } => None,
            EntryKind::Note {
                label,
                text,
                searchable: true,
            } => {
                let content = [Part::Text(text.to_owned())];
                let ctx = Ctx {
                    style: Style::for_note(label),
                    parent_id: None,
                    entry_index,
                };
                let ts = self.entry_timestamp(entry.timestamp);
                let mut timing = RowTiming::new(self.prefs.show_timing, ts.as_deref());
                let blocks = self.user_blocks(&ctx, &mut timing, &content);
                self.message(
                    ctx,
                    DocMessageKind::Note,
                    ts.clone(),
                    entry.timestamp,
                    blocks,
                )
            }
            EntryKind::Subagent { agent_id, message } => {
                if !self.prefs.show_thinking {
                    return None;
                }
                self.agent_progress_message(entry_index, agent_id, message, entry.timestamp)
            }
            EntryKind::Message { message, speaker } => {
                if message.parent_tool_use_id.is_some() && !self.prefs.show_thinking {
                    return None;
                }
                let parent_id = message.parent_tool_use_id.as_deref();
                let (style, kind) = match message.role {
                    Role::User => (
                        Style::for_user(parent_id, &message.parts),
                        DocMessageKind::User,
                    ),
                    Role::Assistant => (
                        Style::for_assistant(parent_id, speaker),
                        DocMessageKind::Assistant,
                    ),
                };
                let ctx = Ctx {
                    style,
                    parent_id,
                    entry_index,
                };
                let ts = self.entry_timestamp(entry.timestamp);
                let mut timing = RowTiming::new(self.prefs.show_timing, ts.as_deref());
                let blocks = match message.role {
                    Role::User => self.user_blocks(&ctx, &mut timing, &message.parts),
                    Role::Assistant => self.assistant_blocks(&ctx, &mut timing, &message.parts),
                };
                self.message(ctx, kind, ts.clone(), entry.timestamp, blocks)
            }
        }
    }

    /// User template: text, then tool results (`render_user_message`).
    fn user_blocks(
        &self,
        ctx: &Ctx<'_>,
        timing: &mut RowTiming<'_>,
        parts: &[Part],
    ) -> Vec<DocBlock> {
        let mut blocks = Vec::new();

        // step_user_text
        let texts: Vec<String> = parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => process_command_message(text),
                _ => None,
            })
            .collect();
        let text = (!texts.is_empty()).then(|| texts.join("\n\n"));
        if let Some(text) = text {
            let slot = if ctx.style.dimmed {
                timing.pad()
            } else {
                timing.take_once()
            };
            blocks.push(DocBlock {
                timing: DocTiming::from_slot(slot),
                kind: DocBlockKind::Markdown { text },
            });
            // Top-level slot is now spent regardless of the branch taken.
            let _ = timing.take_once();
        }

        // step_user_tool_results
        if self.prefs.tool_display.shows_details() {
            for (block_idx, part) in parts.iter().enumerate() {
                let Part::ToolResult {
                    output: content,
                    call_id: tool_use_id,
                } = part
                else {
                    continue;
                };
                let id = make_tool_output_id(
                    ctx.entry_index,
                    ctx.parent_id,
                    block_idx,
                    ToolOutputKind::ToolResult,
                    Some(tool_use_id),
                );
                if ctx.style.is_subagent {
                    let slot = timing.pad();
                    blocks.push(self.subagent_tool_result(
                        id,
                        tool_result_display_text(content.as_ref()),
                        slot,
                    ));
                } else {
                    // The slot is consumed even when the result renders no
                    // row (and is left out), like the ledger.
                    let slot = timing.consume();
                    blocks.extend(self.tool_result(
                        id,
                        tool_result_display_text(content.as_ref()),
                        self.prefs.tool_display,
                        slot,
                    ));
                }
            }
        }

        blocks
    }

    /// Assistant template: text → tool summary → tool calls → thinking
    /// (`render_assistant_message`).
    fn assistant_blocks(
        &self,
        ctx: &Ctx<'_>,
        timing: &mut RowTiming<'_>,
        content: &[Part],
    ) -> Vec<DocBlock> {
        let mut blocks = Vec::new();

        // step_assistant_text
        for part in content {
            let Part::Text(text) = part else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            let slot = if ctx.style.dimmed {
                let slot = timing.pad();
                let _ = timing.take_once();
                slot
            } else {
                timing.take_once()
            };
            blocks.push(DocBlock {
                timing: DocTiming::from_slot(slot),
                kind: DocBlockKind::Markdown { text: text.clone() },
            });
        }

        self.push_tool_summary(&mut blocks, timing, content);
        if self.prefs.tool_display.shows_details() {
            self.push_tool_calls(&mut blocks, ctx, timing, content);
        }

        // step_thinking
        if self.prefs.show_thinking && !ctx.style.is_subagent {
            for part in content {
                let Part::Thinking(thinking) = part else {
                    continue;
                };
                if thinking.is_empty() {
                    continue;
                }
                let slot = timing.consume();
                blocks.push(DocBlock {
                    timing: DocTiming::from_slot(slot),
                    kind: DocBlockKind::Thinking {
                        text: thinking.clone(),
                    },
                });
            }
        }

        blocks
    }

    /// step_tool_summary: the non-clickable sentence (summary mode) or call
    /// lines (calls mode) of an assistant entry that is not grouped.
    fn push_tool_summary(
        &self,
        blocks: &mut Vec<DocBlock>,
        timing: &mut RowTiming<'_>,
        content: &[Part],
    ) {
        if !self.prefs.tool_display.groups_tools() {
            return;
        }
        let summary = summarize_tool_calls(content);
        if summary.is_empty() {
            return;
        }
        let rows = summary_rows(&summary, content.iter(), &self.options);
        blocks.extend(self.summary_blocks(None, timing.consume(), rows));
    }

    /// The ledger's `render_tool_activity_summary`: the collapsed rows of a
    /// group or a mixed message as blocks. Summary mode has one row, the
    /// sentence; calls mode one [`DocBlockKind::ToolCallLine`] per call,
    /// where rows after the first take the continuation slot of `slot`.
    fn summary_blocks(
        &self,
        id: Option<&ToolOutputId>,
        slot: TimingSlot<'_>,
        rows: Vec<String>,
    ) -> Vec<DocBlock> {
        let calls = self.prefs.tool_display == ToolDisplayMode::Calls;
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| DocBlock {
                timing: DocTiming::from_slot(if i == 0 { slot } else { slot.continuation() }),
                kind: if calls {
                    DocBlockKind::ToolCallLine {
                        id: id.cloned(),
                        header: row,
                    }
                } else {
                    DocBlockKind::ToolSummary {
                        id: id.cloned(),
                        sentence: row,
                    }
                },
            })
            .collect()
    }

    /// step_tool_calls
    fn push_tool_calls(
        &self,
        blocks: &mut Vec<DocBlock>,
        ctx: &Ctx<'_>,
        timing: &mut RowTiming<'_>,
        content: &[Part],
    ) {
        for (block_idx, part) in content.iter().enumerate() {
            let Part::ToolCall { id, name, input } = part else {
                continue;
            };
            let output_id = make_tool_output_id(
                ctx.entry_index,
                ctx.parent_id,
                block_idx,
                ToolOutputKind::ToolCall,
                Some(id),
            );
            let slot = if ctx.style.is_subagent {
                timing.pad()
            } else {
                timing.consume()
            };
            blocks.push(self.tool_call(
                name,
                input,
                ctx.style.dimmed,
                self.prefs.tool_display,
                output_id,
                slot,
            ));
        }
    }

    fn tool_call(
        &self,
        name: &str,
        input: &serde_json::Value,
        dimmed: bool,
        mode: ToolDisplayMode,
        id: ToolOutputId,
        slot: TimingSlot<'_>,
    ) -> DocBlock {
        let formatted = tool_format::format_tool_call(name, input, 0);
        let expanded = self.expanded.contains(&id);
        DocBlock {
            timing: DocTiming::from_slot(slot),
            kind: DocBlockKind::ToolCall(DocToolCall {
                id,
                name: name.to_string(),
                input: input.clone(),
                header: formatted.header,
                body: formatted.body.as_deref().map(ToolBody::from_text),
                dimmed,
                fold: Fold::for_mode(mode, expanded, TRUNCATED_BODY_LINES),
            }),
        }
    }

    /// A top-level result block, or `None` when its markdown renders no row
    /// (e.g. a bare fence): the ledger shows nothing for it.
    fn tool_result(
        &self,
        id: ToolOutputId,
        text: String,
        mode: ToolDisplayMode,
        slot: TimingSlot<'_>,
    ) -> Option<DocBlock> {
        let expanded = self.expanded.contains(&id);
        let markdown = if text.contains("```") {
            text.clone()
        } else {
            fence_text(&text)
        };
        let rows = unwrapped_rows(&markdown);
        if rows == 0 {
            return None;
        }
        Some(DocBlock {
            timing: DocTiming::from_slot(slot),
            kind: DocBlockKind::ToolResult(DocToolResult {
                id,
                text,
                markdown,
                rows,
                fold: Fold::for_mode(mode, expanded, TRUNCATED_RESULT_LINES),
            }),
        })
    }

    fn subagent_tool_result(
        &self,
        id: ToolOutputId,
        text: String,
        slot: TimingSlot<'_>,
    ) -> DocBlock {
        let expanded = self.expanded.contains(&id);
        DocBlock {
            timing: DocTiming::from_slot(slot),
            kind: DocBlockKind::SubagentToolResult(DocSubagentToolResult {
                id,
                text,
                fold: Fold::for_mode(self.prefs.tool_display, expanded, TRUNCATED_RESULT_LINES),
            }),
        }
    }

    /// The ledger's `render_agent_progress_message`. Agent progress never
    /// shows a stamp; `raw_timestamp` is the entry's time.
    fn agent_progress_message(
        &self,
        entry_index: usize,
        agent_id: &str,
        message: &Message,
        raw_timestamp: Option<DateTime<Utc>>,
    ) -> Option<DocMessage> {
        let short_id = crate::claude::short_agent_id(agent_id);
        let content = &message.parts;
        let (kind, role) = match message.role {
            Role::User => (DocMessageKind::AgentProgressUser, LabelRole::TextPrimary),
            Role::Assistant => (DocMessageKind::AgentProgressAssistant, LabelRole::Accent),
        };
        let ctx = Ctx {
            style: Style::for_agent(short_id, role),
            parent_id: Some(agent_id),
            entry_index,
        };
        let mut timing = RowTiming::column_only(self.prefs.show_timing);
        let mut blocks = Vec::new();

        // step_aggregated_text (no command-tag processing)
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if !texts.is_empty() {
            let slot = timing.pad();
            let _ = timing.take_once();
            blocks.push(DocBlock {
                timing: DocTiming::from_slot(slot),
                kind: DocBlockKind::Markdown {
                    text: texts.join("\n\n"),
                },
            });
        }

        if kind == DocMessageKind::AgentProgressUser {
            // step_agent_tool_results: always the subagent presentation.
            if self.prefs.tool_display.shows_details() {
                for (block_idx, part) in content.iter().enumerate() {
                    let Part::ToolResult {
                        output: content,
                        call_id: tool_use_id,
                    } = part
                    else {
                        continue;
                    };
                    let id = make_tool_output_id(
                        entry_index,
                        ctx.parent_id,
                        block_idx,
                        ToolOutputKind::ToolResult,
                        Some(tool_use_id),
                    );
                    blocks.push(self.subagent_tool_result(
                        id,
                        tool_result_display_text(content.as_ref()),
                        TimingSlot::from_show_timing(self.prefs.show_timing),
                    ));
                }
            }
        } else {
            // Summary row, then tool calls; agent progress has no thinking.
            self.push_tool_summary(&mut blocks, &mut timing, content);
            if self.prefs.tool_display.shows_details() {
                self.push_tool_calls(&mut blocks, &ctx, &mut timing, content);
            }
        }

        self.message(ctx, kind, None, raw_timestamp, blocks)
    }

    /// The ledger's `try_extend_or_start_pending_summary`.
    fn try_extend_or_start_pending_summary(
        &self,
        messages: &mut Vec<DocMessage>,
        pending: &mut Option<PendingGroup>,
        entries: &[RenderableEntry<'_>],
        parsed_idx: usize,
    ) -> bool {
        let parsed = &entries[parsed_idx];
        let entry_index = parsed.entry_index;
        let entry = parsed.entry();

        if let Some((parent_id, speaker, timestamp, summary)) =
            tool_only_assistant_summary(entry, &self.options)
        {
            match pending {
                Some(p) if p.parent_id.as_deref() == parent_id && p.speaker == speaker => {
                    p.last_parsed_idx = parsed_idx;
                    p.summary.merge(summary);
                }
                _ => {
                    self.flush_tool_summary(messages, pending, entries);
                    *pending = Some(PendingGroup {
                        id: make_tool_summary_output_id(entry_index, parent_id),
                        first_entry_index: entry_index,
                        first_parsed_idx: parsed_idx,
                        last_parsed_idx: parsed_idx,
                        parent_id: parent_id.map(str::to_string),
                        speaker: speaker.to_owned(),
                        timestamp,
                        summary,
                    });
                }
            }
            return true;
        }

        // Records that render nothing sit between a tool result and the next
        // call; they must not split one run of tool calls into several rows.
        if pending.is_some() && entry_shows_nothing(entry, &self.options) {
            return true;
        }

        if user_entry_is_only_tool_results(entry, &self.options) {
            if let Some(p) = pending {
                p.last_parsed_idx = parsed_idx;
            }
            return true;
        }

        false
    }

    /// The ledger's `flush_tool_summary`.
    fn flush_tool_summary(
        &self,
        messages: &mut Vec<DocMessage>,
        pending: &mut Option<PendingGroup>,
        entries: &[RenderableEntry<'_>],
    ) {
        let Some(group) = pending.take() else {
            return;
        };

        let label = assistant_label(group.parent_id.as_deref(), &group.speaker).into_owned();
        let ts = if self.prefs.show_timing {
            group.timestamp.map(format_timestamp)
        } else {
            None
        };
        let slot = match ts.as_deref() {
            Some(ts) => TimingSlot::Stamp(ts),
            None => TimingSlot::Disabled,
        };
        let blocks = if self.prefs.tool_display == ToolDisplayMode::Calls {
            // Calls mode ignores the group's own id: each row expands alone.
            let blocks = self.calls_group_blocks(entries, &group, slot);
            if blocks.is_empty() {
                return;
            }
            blocks
        } else if self.expanded.contains(&group.id) {
            let items = self.summary_group_details(entries, &group);
            if items.is_empty() {
                return;
            }
            vec![DocBlock {
                timing: DocTiming::from_slot(TimingSlot::from_show_timing(self.prefs.show_timing)),
                kind: DocBlockKind::ToolSummaryDetails {
                    id: group.id.clone(),
                    items,
                },
            }]
        } else {
            if group.summary.is_empty() {
                return;
            }
            let line = group_summary_line(
                entries,
                group.first_parsed_idx..=group.last_parsed_idx,
                group.parent_id.as_deref(),
                &group.summary,
                self.options.content_width,
            );
            self.summary_blocks(Some(&group.id), slot, vec![line])
        };

        let is_subagent = group.parent_id.is_some();
        messages.push(DocMessage {
            entry_index: group.first_entry_index,
            kind: DocMessageKind::ToolSummaryGroup,
            label,
            label_role: LabelRole::AccentDim,
            bold: false,
            dimmed: is_subagent,
            is_subagent,
            timestamp: ts,
            raw_timestamp: group.timestamp,
            navigable: true,
            blocks,
        });
    }

    /// The ledger's `render_calls_group` (upstream a1c88f8): each call of the
    /// group as a [`DocBlockKind::ToolCallLine`] with its own row id, or,
    /// when that row is expanded, as a [`DocBlockKind::ToolSummaryDetails`]
    /// holding the call and its result. Rows after the first take the
    /// continuation slot of `slot`.
    fn calls_group_blocks(
        &self,
        entries: &[RenderableEntry<'_>],
        group: &PendingGroup,
        slot: TimingSlot<'_>,
    ) -> Vec<DocBlock> {
        let parent_id = group.parent_id.as_deref();
        let range = group.first_parsed_idx..=group.last_parsed_idx;
        let mut blocks = Vec::new();
        for (entry_index, message) in group_messages(entries, range, parent_id, Role::Assistant) {
            for (block_idx, part) in message.parts.iter().enumerate() {
                let Part::ToolCall { id, name, input } = part else {
                    continue;
                };
                let row_id = make_tool_output_id(
                    entry_index,
                    parent_id,
                    block_idx,
                    ToolOutputKind::CallRow,
                    Some(id),
                );
                let row_slot = if blocks.is_empty() {
                    slot
                } else {
                    slot.continuation()
                };
                let kind = if self.expanded.contains(&row_id) {
                    DocBlockKind::ToolSummaryDetails {
                        id: row_id,
                        items: self.call_details(
                            entries,
                            group,
                            (entry_index, block_idx, id, name, input),
                            row_slot,
                        ),
                    }
                } else {
                    DocBlockKind::ToolCallLine {
                        id: Some(row_id),
                        header: call_line(name, input, self.options.content_width),
                    }
                };
                blocks.push(DocBlock {
                    timing: DocTiming::from_slot(row_slot),
                    kind,
                });
            }
        }
        blocks
    }

    /// The ledger's `render_call_details`: one call of the group in
    /// truncated mode, followed by its result when the group holds it.
    fn call_details(
        &self,
        entries: &[RenderableEntry<'_>],
        group: &PendingGroup,
        (entry_index, block_idx, call_id, name, input): (
            usize,
            usize,
            &str,
            &str,
            &serde_json::Value,
        ),
        slot: TimingSlot<'_>,
    ) -> Vec<DocBlock> {
        let parent_id = group.parent_id.as_deref();
        let call_output_id = make_tool_output_id(
            entry_index,
            parent_id,
            block_idx,
            ToolOutputKind::ToolCall,
            Some(call_id),
        );
        let mut items = vec![self.tool_call(
            name,
            input,
            true,
            ToolDisplayMode::Truncated,
            call_output_id,
            slot,
        )];
        let range = group.first_parsed_idx..=group.last_parsed_idx;
        if let Some((entry_index, block_idx, output)) =
            group_call_result(entries, range, parent_id, call_id)
        {
            let result_output_id = make_tool_output_id(
                entry_index,
                parent_id,
                block_idx,
                ToolOutputKind::ToolResult,
                Some(call_id),
            );
            items.extend(self.tool_result(
                result_output_id,
                tool_result_display_text(output),
                ToolDisplayMode::Truncated,
                slot.continuation(),
            ));
        }
        items
    }

    /// The ledger's `render_summary_group_details`: every call and result of
    /// the group (same parent) in truncated mode.
    fn summary_group_details(
        &self,
        entries: &[RenderableEntry<'_>],
        group: &PendingGroup,
    ) -> Vec<DocBlock> {
        let pad = TimingSlot::from_show_timing(self.prefs.show_timing);
        let mut items = Vec::new();
        for parsed in &entries[group.first_parsed_idx..=group.last_parsed_idx] {
            let EntryKind::Message { message, .. } = parsed.entry().kind else {
                continue;
            };
            let parent_tool_use_id = &message.parent_tool_use_id;
            if parent_tool_use_id.as_deref() != group.parent_id.as_deref() {
                continue;
            }
            match message.role {
                Role::Assistant => {
                    for (block_idx, part) in message.parts.iter().enumerate() {
                        if let Part::ToolCall { id, name, input } = part {
                            let output_id = make_tool_output_id(
                                parsed.entry_index,
                                parent_tool_use_id.as_deref(),
                                block_idx,
                                ToolOutputKind::ToolCall,
                                Some(id),
                            );
                            items.push(self.tool_call(
                                name,
                                input,
                                true,
                                ToolDisplayMode::Truncated,
                                output_id,
                                pad,
                            ));
                        }
                    }
                }
                Role::User => {
                    for (block_idx, part) in message.parts.iter().enumerate() {
                        if let Part::ToolResult {
                            output: content,
                            call_id: tool_use_id,
                        } = part
                        {
                            let output_id = make_tool_output_id(
                                parsed.entry_index,
                                parent_tool_use_id.as_deref(),
                                block_idx,
                                ToolOutputKind::ToolResult,
                                Some(tool_use_id),
                            );
                            items.extend(self.tool_result(
                                output_id,
                                tool_result_display_text(content.as_ref()),
                                ToolDisplayMode::Truncated,
                                pad,
                            ));
                        }
                    }
                }
            }
        }
        items
    }
}

#[cfg(test)]
mod tests;
