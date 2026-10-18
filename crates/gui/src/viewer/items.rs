//! `ViewerItem`s built from the core `ConversationDocument` (DESIGN §9.2):
//! stable ids, lanes, ≈100-line chunks, visual-row truncation (K9). WP2.
//!
//! One item per ledger block (markdown text, thinking, tool summary, tool
//! call, tool result, subagent result, expanded summary detail); blocks
//! longer than the chunk limits are split into chunk items with stable
//! `(block, chunk)` ids. Gpui-free: the worker thread builds them.
//!
//! Truncation (Truncated mode, collapsed) counts ledger rows at `cols`
//! display cells, never logical lines (P4/K9): a 50 KB single-line result
//! shows at most 4 visual rows plus the `(N more lines…)` row.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use claude_history_core::view::{
    ConversationDocument, DocBlock, DocBlockKind, DocMessage, DocSubagentToolResult, DocToolCall,
    DocToolResult, Fold, LabelRole, ToolOutputId, ViewerModel, fitted_name,
};

use super::segments::{self, SegKind, Segment, Style};

/// Chunk size of big blocks (lines).
pub const CHUNK_LINES: usize = 100;
/// Blocks (code, tool bodies, fenced results, subagent results) longer than
/// this are split into [`CHUNK_LINES`]-line items.
pub const CHUNK_SPLIT_OVER: usize = 120;
/// Markdown blocks longer than this (rendered lines) are split at segment
/// boundaries.
pub const MARKDOWN_CHUNK_LIMIT: usize = 150;

/// The label of a top-level tool result.
pub const RESULT_LABEL: &str = "↳ Result";
/// The label of a subagent tool result.
pub const SUBAGENT_RESULT_LABEL: &str = "↳ Tool";
/// The header row of a subagent tool result.
pub const SUBAGENT_RESULT_HEADER: &str = "<Result>";
/// The label of a thinking block.
pub const THINKING_LABEL: &str = "Thinking";

/// What a text item shows (part of its stable id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TextKind {
    Markdown,
    Thinking,
    /// The non-clickable tool summary sentence of a mixed message.
    Summary,
}

/// Stable identity of a block across rebuilds (preference toggles,
/// expansions, width changes).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKey {
    /// The `ordinal`-th block of `kind` of the entry.
    Text {
        entry: usize,
        kind: TextKind,
        ordinal: u16,
    },
    /// A tool output (call, result, summary group) by its core id.
    Tool(ToolOutputId),
}

/// Stable id of an item: its block and chunk.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId {
    pub key: ItemKey,
    pub chunk: u32,
}

/// Color role of the label lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LaneRole {
    TextPrimary,
    Accent,
    AccentDim,
    TextSecondary,
    ToolText,
}

impl From<LabelRole> for LaneRole {
    fn from(role: LabelRole) -> Self {
        match role {
            LabelRole::TextPrimary => LaneRole::TextPrimary,
            LabelRole::Accent => LaneRole::Accent,
            LabelRole::AccentDim => LaneRole::AccentDim,
            LabelRole::TextSecondary => LaneRole::TextSecondary,
        }
    }
}

/// The ledger lanes of an item: timestamp and label (first chunk only).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Lanes {
    /// The stamp (`%b %d %H:%M`) when this block carries it.
    pub timestamp: Option<String>,
    /// The label fitted to 9 display cells (`Branch s…`, V-014).
    pub label: Option<String>,
    pub role: LaneRole,
    pub bold: bool,
    pub dimmed: bool,
}

impl Lanes {
    fn empty(role: LaneRole, dimmed: bool) -> Self {
        Lanes {
            timestamp: None,
            label: None,
            role,
            bold: false,
            dimmed,
        }
    }
}

/// What an item renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Markdown,
    Thinking,
    ToolSummary,
    ToolCall,
    ToolResult,
    SubagentResult,
}

/// Where an item's block lives in the document (hidden-text search).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockRef {
    pub message: usize,
    pub block: usize,
    /// The item index inside an expanded summary group.
    pub detail: Option<usize>,
}

/// One row of the transcript list.
#[derive(Clone, Debug)]
pub struct ViewerItem {
    pub id: ItemId,
    /// A hash of `id` (stable element ids across rebuilds).
    pub key_hash: u64,
    /// Hash of everything the item renders (changes when expanded or
    /// truncated text changes); unchanged ids with a new version are
    /// re-measured by the splice diff.
    pub version: u64,
    pub kind: ItemKind,
    pub message_index: usize,
    /// `ConversationDocument::nav_position` of its message.
    pub nav_position: Option<usize>,
    pub entry_index: usize,
    pub first_of_message: bool,
    /// Lanes, header and chevron are drawn on the first chunk only.
    pub first_chunk: bool,
    pub last_chunk: bool,
    pub lanes: Lanes,
    pub segments: Arc<Vec<Segment>>,
    /// `segments[0]` is the header row (tool call header, `<Result>`).
    pub has_header: bool,
    /// The output a click on the chevron or indicator toggles.
    pub toggle: Option<ToolOutputId>,
    pub expanded: bool,
    /// `(N more lines…)` (last chunk of a collapsed output).
    pub more: Option<usize>,
    /// An expanded summary group this item collapses (first detail item).
    pub group_toggle: Option<ToolOutputId>,
    pub block: BlockRef,
    /// The item's text depends on the content width (collapsed tool output).
    pub cols_dependent: bool,
}

impl ViewerItem {
    /// The hover group (V-017): the expandable output this item belongs to.
    pub fn hover_id(&self) -> Option<&ToolOutputId> {
        self.toggle.as_ref().or(self.group_toggle.as_ref())
    }
}

/// What a build produced besides the items.
#[derive(Clone, Debug, Default)]
pub struct BuildInfo {
    /// Navigable messages.
    pub nav_count: usize,
    /// Entry index of every navigable message (`capture_anchor_entry`).
    pub nav_entries: Vec<usize>,
    /// First item index of every navigable message (by nav position).
    pub nav_first_item: Vec<usize>,
    /// Some item depends on the content width.
    pub cols_dependent: bool,
}

/// Build the items of `doc` at `cols` content cells. `None` when `cancel`
/// says the work is stale (checked between messages).
pub fn build(
    doc: &ConversationDocument,
    model: &ViewerModel,
    cols: usize,
    cancel: &dyn Fn() -> bool,
) -> Option<Vec<ViewerItem>> {
    build_with_info(doc, model, cols, cancel).map(|(items, _)| items)
}

/// [`build`] plus the navigation tables.
pub fn build_with_info(
    doc: &ConversationDocument,
    model: &ViewerModel,
    cols: usize,
    cancel: &dyn Fn() -> bool,
) -> Option<(Vec<ViewerItem>, BuildInfo)> {
    let cols = cols.max(8);
    let show_timing = model.prefs.show_timing;
    let mut out = Vec::new();
    let mut info = BuildInfo::default();
    for (message_index, message) in doc.messages.iter().enumerate() {
        if message_index % 64 == 0 && cancel() {
            return None;
        }
        let nav_position = message.navigable.then(|| {
            let position = info.nav_count;
            info.nav_count += 1;
            info.nav_entries.push(message.entry_index);
            position
        });
        let start = out.len();
        let mut ctx = MessageCtx {
            message,
            message_index,
            nav_position,
            cols,
            show_timing,
            markdown_ordinal: 0,
            thinking_ordinal: 0,
            out: &mut out,
        };
        for (block_index, block) in message.blocks.iter().enumerate() {
            ctx.push_block(block, block_index);
        }
        if out.len() > start {
            out[start].first_of_message = true;
        }
        if nav_position.is_some() {
            info.nav_first_item
                .push(start.min(out.len().saturating_sub(1)));
        }
    }
    info.cols_dependent = out.iter().any(|item| item.cols_dependent);
    for item in &mut out {
        item.version = item_version(item);
        item.key_hash = id_hash(&item.id);
    }
    Some((out, info))
}

struct MessageCtx<'a> {
    message: &'a DocMessage,
    message_index: usize,
    nav_position: Option<usize>,
    cols: usize,
    show_timing: bool,
    markdown_ordinal: u16,
    thinking_ordinal: u16,
    out: &'a mut Vec<ViewerItem>,
}

/// A block about to be emitted as one or more chunk items.
struct Pending {
    key: ItemKey,
    kind: ItemKind,
    lanes: Lanes,
    chunks: Vec<Vec<Segment>>,
    has_header: bool,
    toggle: Option<ToolOutputId>,
    expanded: bool,
    more: Option<usize>,
    group_toggle: Option<ToolOutputId>,
    block: BlockRef,
    cols_dependent: bool,
}

impl MessageCtx<'_> {
    fn label(&self) -> Option<String> {
        Some(fitted_name(&self.message.label))
    }

    fn stamp(&self, block: &DocBlock) -> Option<String> {
        if !self.show_timing {
            return None;
        }
        block.timing.stamp().map(str::to_string)
    }

    fn block_ref(&self, block: usize, detail: Option<usize>) -> BlockRef {
        BlockRef {
            message: self.message_index,
            block,
            detail,
        }
    }

    fn push_block(&mut self, block: &DocBlock, block_index: usize) {
        let message = self.message;
        match &block.kind {
            DocBlockKind::Markdown { text } => {
                let mut segs = segments::markdown(text);
                if message.dimmed {
                    for segment in &mut segs {
                        segment.overlay(Style::DIMMED);
                    }
                }
                let ordinal = self.markdown_ordinal;
                self.markdown_ordinal += 1;
                let lanes = Lanes {
                    timestamp: self.stamp(block),
                    label: self.label(),
                    role: message.label_role.into(),
                    bold: message.bold,
                    dimmed: message.dimmed,
                };
                self.emit(Pending {
                    key: ItemKey::Text {
                        entry: message.entry_index,
                        kind: TextKind::Markdown,
                        ordinal,
                    },
                    kind: ItemKind::Markdown,
                    lanes,
                    chunks: markdown_chunks(segs),
                    has_header: false,
                    toggle: None,
                    expanded: false,
                    more: None,
                    group_toggle: None,
                    block: self.block_ref(block_index, None),
                    cols_dependent: false,
                });
            }
            DocBlockKind::Thinking { text } => {
                let mut segs = segments::markdown(text);
                for segment in &mut segs {
                    segment.overlay(Style::THINKING | Style::ITALIC);
                }
                let ordinal = self.thinking_ordinal;
                self.thinking_ordinal += 1;
                let lanes = Lanes {
                    timestamp: self.stamp(block),
                    label: Some(fitted_name(THINKING_LABEL)),
                    role: LaneRole::AccentDim,
                    bold: false,
                    dimmed: false,
                };
                self.emit(Pending {
                    key: ItemKey::Text {
                        entry: message.entry_index,
                        kind: TextKind::Thinking,
                        ordinal,
                    },
                    kind: ItemKind::Thinking,
                    lanes,
                    chunks: markdown_chunks(segs),
                    has_header: false,
                    toggle: None,
                    expanded: false,
                    more: None,
                    group_toggle: None,
                    block: self.block_ref(block_index, None),
                    cols_dependent: false,
                });
            }
            DocBlockKind::ToolSummary { id, sentence } => {
                let segment = segments::styled_line(sentence, Style::TOOL | Style::DIMMED);
                let key = match id {
                    Some(id) => ItemKey::Tool(id.clone()),
                    None => ItemKey::Text {
                        entry: message.entry_index,
                        kind: TextKind::Summary,
                        ordinal: 0,
                    },
                };
                let lanes = Lanes {
                    timestamp: self.stamp(block),
                    label: self.label(),
                    role: LaneRole::AccentDim,
                    bold: false,
                    dimmed: message.dimmed,
                };
                self.emit(Pending {
                    key,
                    kind: ItemKind::ToolSummary,
                    lanes,
                    chunks: vec![vec![segment]],
                    has_header: false,
                    toggle: id.clone(),
                    expanded: false,
                    more: None,
                    group_toggle: None,
                    block: self.block_ref(block_index, None),
                    cols_dependent: false,
                });
            }
            DocBlockKind::ToolSummaryDetails { id, items } => {
                for (detail, item) in items.iter().enumerate() {
                    let first = detail == 0;
                    let lanes = if first {
                        Lanes {
                            timestamp: self.stamp(block),
                            label: self.label(),
                            role: LaneRole::AccentDim,
                            bold: false,
                            dimmed: true,
                        }
                    } else {
                        Lanes::empty(LaneRole::AccentDim, true)
                    };
                    let group = first.then(|| id.clone());
                    self.push_tool(item, block_index, Some(detail), Some(lanes), group);
                }
            }
            DocBlockKind::ToolCall(_)
            | DocBlockKind::ToolResult(_)
            | DocBlockKind::SubagentToolResult(_) => {
                self.push_tool(block, block_index, None, None, None);
            }
        }
    }

    /// A tool call / result / subagent result (top level or a summary detail).
    fn push_tool(
        &mut self,
        block: &DocBlock,
        block_index: usize,
        detail: Option<usize>,
        lanes: Option<Lanes>,
        group_toggle: Option<ToolOutputId>,
    ) {
        let block_ref = self.block_ref(block_index, detail);
        let pending = match &block.kind {
            DocBlockKind::ToolCall(call) => {
                let lanes = lanes.unwrap_or_else(|| Lanes {
                    timestamp: self.stamp(block),
                    label: self.label(),
                    role: LaneRole::AccentDim,
                    bold: false,
                    dimmed: self.message.dimmed,
                });
                tool_call_pending(call, lanes, self.cols, block_ref)
            }
            DocBlockKind::ToolResult(result) => {
                let lanes = lanes.unwrap_or_else(|| Lanes {
                    timestamp: self.stamp(block),
                    label: Some(fitted_name(RESULT_LABEL)),
                    role: LaneRole::ToolText,
                    bold: false,
                    dimmed: false,
                });
                tool_result_pending(result, lanes, self.cols, block_ref)
            }
            DocBlockKind::SubagentToolResult(result) => {
                let lanes = lanes.unwrap_or_else(|| Lanes {
                    timestamp: None,
                    label: Some(fitted_name(SUBAGENT_RESULT_LABEL)),
                    role: LaneRole::AccentDim,
                    bold: false,
                    dimmed: true,
                });
                subagent_pending(result, lanes, self.cols, block_ref)
            }
            _ => return,
        };
        self.emit(Pending {
            group_toggle,
            ..pending
        });
    }

    fn emit(&mut self, pending: Pending) {
        let mut chunks = pending.chunks;
        if chunks.is_empty() {
            chunks.push(Vec::new());
        }
        let count = chunks.len();
        for (chunk_ix, mut segs) in chunks.into_iter().enumerate() {
            if segs.is_empty() {
                segs.push(segments::styled_line("", Style::NONE));
            }
            let first = chunk_ix == 0;
            let last = chunk_ix + 1 == count;
            self.out.push(ViewerItem {
                id: ItemId {
                    key: pending.key.clone(),
                    chunk: chunk_ix as u32,
                },
                version: 0,
                key_hash: 0,
                kind: pending.kind,
                message_index: self.message_index,
                nav_position: self.nav_position,
                entry_index: self.message.entry_index,
                first_of_message: false,
                first_chunk: first,
                last_chunk: last,
                lanes: if first {
                    pending.lanes.clone()
                } else {
                    Lanes::empty(pending.lanes.role, pending.lanes.dimmed)
                },
                segments: Arc::new(segs),
                has_header: first && pending.has_header,
                toggle: pending.toggle.clone(),
                expanded: pending.expanded,
                more: if last { pending.more } else { None },
                group_toggle: if first {
                    pending.group_toggle.clone()
                } else {
                    None
                },
                block: pending.block,
                cols_dependent: pending.cols_dependent,
            });
        }
    }
}

/// Markdown segments in chunks (§9.2): big code blocks split by lines, the
/// whole split at segment boundaries when longer than the limit.
fn markdown_chunks(segs: Vec<Segment>) -> Vec<Vec<Segment>> {
    segments::chunk_segments(segs, MARKDOWN_CHUNK_LIMIT, CHUNK_SPLIT_OVER, CHUNK_LINES)
}

/// One segment split into line chunks when longer than the split limit.
fn line_chunks(segment: Segment) -> Vec<Segment> {
    if segment.line_count() > CHUNK_SPLIT_OVER {
        segments::split_lines(&segment, CHUNK_LINES)
    } else {
        vec![segment]
    }
}

fn is_collapsed(fold: Fold) -> bool {
    matches!(fold, Fold::Collapsed { .. })
}

fn tool_call_pending(call: &DocToolCall, lanes: Lanes, cols: usize, block: BlockRef) -> Pending {
    let mut header_style = Style::TOOL;
    if call.dimmed {
        header_style |= Style::DIMMED;
    }
    let header = segments::styled_line(&call.header, header_style);
    let collapsed = is_collapsed(call.fold);
    let expandable = call.ledger_expandable(cols);
    let more = if collapsed {
        call.ledger_hidden_lines(cols)
    } else {
        None
    };
    let mut chunks = Vec::new();
    if collapsed {
        let mut first = vec![header];
        let lines = call.visible_body_lines();
        if !lines.is_empty() {
            first.push(segments::tool_body(lines, Some(cols)));
        }
        chunks.push(first);
    } else {
        let lines: &[_] = call.body.as_ref().map_or(&[], |body| &body.lines);
        if lines.len() > CHUNK_SPLIT_OVER {
            let mut first = Some(header);
            for part in lines.chunks(CHUNK_LINES) {
                let mut chunk = Vec::new();
                if let Some(header) = first.take() {
                    chunk.push(header);
                }
                chunk.push(segments::tool_body(part, None));
                chunks.push(chunk);
            }
        } else {
            let mut first = vec![header];
            if !lines.is_empty() {
                first.push(segments::tool_body(lines, None));
            }
            chunks.push(first);
        }
    }
    Pending {
        key: ItemKey::Tool(call.id.clone()),
        kind: ItemKind::ToolCall,
        lanes,
        chunks,
        has_header: true,
        toggle: expandable.then(|| call.id.clone()),
        expanded: call.expanded(),
        more,
        group_toggle: None,
        block,
        cols_dependent: collapsed,
    }
}

fn tool_result_pending(
    result: &DocToolResult,
    lanes: Lanes,
    cols: usize,
    block: BlockRef,
) -> Pending {
    let collapsed = is_collapsed(result.fold);
    let fenced = result.is_fenced();
    let (chunks, more) = if collapsed {
        let rows = result.ledger_visible_rows(cols).join("\n");
        let segment = if fenced {
            segments::code_block(&rows, "text")
        } else {
            segments::plain(&rows, false, None)
        };
        (vec![vec![segment]], result.ledger_hidden_lines(cols))
    } else if fenced {
        let pieces = line_chunks(segments::code_block(&result.text, "text"));
        (pieces.into_iter().map(|piece| vec![piece]).collect(), None)
    } else {
        (markdown_chunks(segments::markdown(&result.text)), None)
    };
    Pending {
        key: ItemKey::Tool(result.id.clone()),
        kind: ItemKind::ToolResult,
        lanes,
        chunks,
        has_header: false,
        toggle: result.ledger_expandable(cols).then(|| result.id.clone()),
        expanded: result.expanded(),
        more,
        group_toggle: None,
        block,
        cols_dependent: collapsed,
    }
}

fn subagent_pending(
    result: &DocSubagentToolResult,
    lanes: Lanes,
    cols: usize,
    block: BlockRef,
) -> Pending {
    let header = segments::styled_line(SUBAGENT_RESULT_HEADER, Style::DIMMED);
    let collapsed = is_collapsed(result.fold);
    let mut chunks = Vec::new();
    if collapsed {
        let body = segments::plain(&result.visible_text(), true, Some(cols));
        chunks.push(vec![header, body]);
    } else {
        let body = segments::plain(&result.text, true, None);
        let mut first = Some(header);
        for piece in line_chunks(body) {
            let mut chunk = Vec::new();
            if let Some(header) = first.take() {
                chunk.push(header);
            }
            chunk.push(piece);
            chunks.push(chunk);
        }
    }
    Pending {
        key: ItemKey::Tool(result.id.clone()),
        kind: ItemKind::SubagentResult,
        lanes,
        chunks,
        has_header: true,
        toggle: result.expandable().then(|| result.id.clone()),
        expanded: result.expanded(),
        more: if collapsed {
            result.hidden_lines()
        } else {
            None
        },
        group_toggle: None,
        block,
        cols_dependent: collapsed,
    }
}

/// A stable hash of an item id.
pub fn id_hash(id: &ItemId) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

fn item_version(item: &ViewerItem) -> u64 {
    let mut hasher = DefaultHasher::new();
    item.kind.hash(&mut hasher);
    item.nav_position.hash(&mut hasher);
    item.first_of_message.hash(&mut hasher);
    item.first_chunk.hash(&mut hasher);
    item.last_chunk.hash(&mut hasher);
    item.lanes.hash(&mut hasher);
    item.segments.hash(&mut hasher);
    item.has_header.hash(&mut hasher);
    item.toggle.hash(&mut hasher);
    item.expanded.hash(&mut hasher);
    item.more.hash(&mut hasher);
    item.group_toggle.hash(&mut hasher);
    hasher.finish()
}

/// The full text of a collapsed output (search over hidden lines, PD7):
/// the call header and every body line, the whole result, the whole
/// subagent result.
pub fn hidden_full_text(doc: &ConversationDocument, block: BlockRef) -> Option<String> {
    let message = doc.messages.get(block.message)?;
    let mut doc_block = message.blocks.get(block.block)?;
    if let (Some(detail), DocBlockKind::ToolSummaryDetails { items, .. }) =
        (block.detail, &doc_block.kind)
    {
        doc_block = items.get(detail)?;
    }
    match &doc_block.kind {
        DocBlockKind::ToolCall(call) => {
            let mut text = call.header.clone();
            if let Some(body) = &call.body {
                for line in &body.lines {
                    text.push('\n');
                    text.push_str(&line.text);
                }
            }
            Some(text)
        }
        DocBlockKind::ToolResult(result) => Some(if result.is_fenced() {
            result.text.clone()
        } else {
            claude_history_core::view::markdown_plain_text(&result.markdown)
        }),
        DocBlockKind::SubagentToolResult(result) => {
            Some(format!("{SUBAGENT_RESULT_HEADER}\n{}", result.text))
        }
        _ => None,
    }
}

/// Text segments of an item for copy/search (header and body).
pub fn item_text(item: &ViewerItem) -> String {
    segments::plain_text(&item.segments)
}

/// Whether a segment of `item` is its header row.
pub fn is_header(item: &ViewerItem, segment_ix: usize) -> bool {
    item.has_header && segment_ix == 0
}

/// The label lane width in display cells (V-013): core's `NAME_WIDTH`.
pub const LABEL_LANE_CELLS: usize = claude_history_core::view::NAME_WIDTH;

/// Whether the item is a plain-mono kind (tool rows).
pub fn is_tool_item(item: &ViewerItem) -> bool {
    matches!(
        item.kind,
        ItemKind::ToolCall
            | ItemKind::ToolResult
            | ItemKind::SubagentResult
            | ItemKind::ToolSummary
    )
}

/// Every segment kind of the item (tests).
pub fn segment_kinds(item: &ViewerItem) -> Vec<SegKind> {
    item.segments.iter().map(|s| s.kind.clone()).collect()
}

// ---------------------------------------------------------------------------
// Splice diff
// ---------------------------------------------------------------------------

/// One `ListState::splice(range, count)` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub removed: usize,
    pub inserted: usize,
}

/// The splices that turn the list `old` into `new` (DESIGN §9.6): items
/// with the same id and version keep their measured heights; a changed
/// version is re-measured; ids only in one list are removed / inserted.
/// Adjacent operations are merged; the list position is tracked so the
/// calls can be applied in order.
pub fn splice_diff(old: &[(ItemId, u64)], new: &[(ItemId, u64)]) -> Vec<Splice> {
    use std::collections::HashMap;
    let old_pos: HashMap<&ItemId, usize> = old
        .iter()
        .enumerate()
        .map(|(ix, (id, _))| (id, ix))
        .collect();
    let new_pos: HashMap<&ItemId, usize> = new
        .iter()
        .enumerate()
        .map(|(ix, (id, _))| (id, ix))
        .collect();
    let mut out = Vec::new();
    let (mut i, mut j, mut pos) = (0usize, 0usize, 0usize);
    let mut pending: Option<Splice> = None;
    let flush = |pending: &mut Option<Splice>, pos: &mut usize, out: &mut Vec<Splice>| {
        if let Some(op) = pending.take() {
            *pos = op.start + op.inserted;
            out.push(op);
        }
    };
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i].0 == new[j].0 {
            if old[i].1 == new[j].1 {
                flush(&mut pending, &mut pos, &mut out);
                pos += 1;
            } else {
                let op = pending.get_or_insert(Splice {
                    start: pos,
                    removed: 0,
                    inserted: 0,
                });
                op.removed += 1;
                op.inserted += 1;
            }
            i += 1;
            j += 1;
            continue;
        }
        let op = pending.get_or_insert(Splice {
            start: pos,
            removed: 0,
            inserted: 0,
        });
        let remove_old = if i >= old.len() {
            false
        } else if j >= new.len() {
            true
        } else {
            match (new_pos.get(&old[i].0), old_pos.get(&new[j].0)) {
                // The old item is gone, or reappears before the new cursor.
                (None, _) => true,
                (Some(&at), _) if at < j => true,
                // The new item is new, or its old copy is already behind us.
                (_, None) => false,
                (_, Some(&at)) if at < i => false,
                // Both exist further on (a move): drop the old one here.
                _ => true,
            }
        };
        if remove_old {
            op.removed += 1;
            i += 1;
        } else {
            op.inserted += 1;
            j += 1;
        }
    }
    flush(&mut pending, &mut pos, &mut out);
    out
}
