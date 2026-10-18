//! In-viewer search over item segments (DESIGN §9.8) with core
//! `view::ViewSearch<SegMatch>`; hidden tool output auto-expands. WP2.
//!
//! Matching is case-insensitive over the rendered segment text (no label,
//! timestamp or separator lanes; AD7, quirk 12 fixed) and counts every
//! occurrence. The per-segment search texts are built lazily on the first
//! search after an open or rebuild (V-102). For a collapsed tool output the
//! occurrences in its full text beyond the visible ones are extra matches
//! with `hidden: Some(id)`: revealing one expands the output (PD7, V-078).

use std::collections::HashMap;
use std::ops::Range;

use claude_history_core::view::{ConversationDocument, ToolOutputId};

use super::items::{ViewerItem, hidden_full_text};
use super::segments::{SearchText, fold_query, search_text, search_text_of};

/// One occurrence of the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegMatch {
    pub item_ix: usize,
    pub segment_ix: usize,
    /// Byte range in the segment text (empty for a hidden match).
    pub range: Range<usize>,
    /// The collapsed output that holds this occurrence in its hidden lines.
    pub hidden: Option<ToolOutputId>,
}

/// The lazily built search texts of the installed items.
#[derive(Default)]
pub struct SearchIndex {
    texts: Vec<Vec<SearchText>>,
    hidden: HashMap<usize, SearchText>,
}

impl SearchIndex {
    /// Build the search texts of every segment.
    pub fn build(items: &[ViewerItem]) -> Self {
        let texts = items
            .iter()
            .map(|item| item.segments.iter().map(search_text).collect())
            .collect();
        Self {
            texts,
            hidden: HashMap::new(),
        }
    }

    /// Lowercased bytes held (probe).
    pub fn bytes(&self) -> usize {
        self.texts
            .iter()
            .flatten()
            .map(|text| text.lower.len())
            .sum::<usize>()
            + self
                .hidden
                .values()
                .map(|text| text.lower.len())
                .sum::<usize>()
    }

    /// Every occurrence of `query` in document order.
    pub fn find(
        &mut self,
        items: &[ViewerItem],
        doc: &ConversationDocument,
        query: &str,
    ) -> Vec<SegMatch> {
        let folded = fold_query(query);
        if folded.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for (item_ix, item) in items.iter().enumerate() {
            let Some(texts) = self.texts.get(item_ix) else {
                break;
            };
            let mut visible = 0usize;
            for (segment_ix, (segment, text)) in item.segments.iter().zip(texts).enumerate() {
                for range in text.find(&folded, &segment.text) {
                    visible += 1;
                    matches.push(SegMatch {
                        item_ix,
                        segment_ix,
                        range,
                        hidden: None,
                    });
                }
            }
            if item.more.is_some()
                && let Some(id) = &item.toggle
            {
                let full = self.hidden.entry(item_ix).or_insert_with(|| {
                    search_text_of(&hidden_full_text(doc, item.block).unwrap_or_default())
                });
                let total = full.count(&folded);
                let last_segment = item.segments.len().saturating_sub(1);
                for _ in visible..total {
                    matches.push(SegMatch {
                        item_ix,
                        segment_ix: last_segment,
                        range: 0..0,
                        hidden: Some(id.clone()),
                    });
                }
            }
        }
        matches
    }
}

/// Visible matches per item: `(segment, range, is_current)`.
pub type MatchesByItem = HashMap<usize, Vec<(usize, Range<usize>, bool)>>;

/// Group the visible matches by item for rendering.
pub fn by_item(matches: &[SegMatch], current: Option<usize>) -> MatchesByItem {
    let mut out: MatchesByItem = HashMap::new();
    for (ix, m) in matches.iter().enumerate() {
        if m.hidden.is_some() || m.range.is_empty() {
            continue;
        }
        out.entry(m.item_ix).or_default().push((
            m.segment_ix,
            m.range.clone(),
            current == Some(ix),
        ));
    }
    out
}
