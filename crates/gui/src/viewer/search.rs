//! In-viewer search over item segments (DESIGN §9.8) with core
//! `view::ViewSearch<SegMatch>`; hidden tool output auto-expands. WP2.
//!
//! Matching follows the list's keyword rules (core
//! `display_format::ViewSearchQuery`, upstream d2c0f07): any query word
//! matches at word starts, normalized and case-folded (anywhere for CJK
//! queries), and quoted or `_` literals match exactly (smart case). It runs
//! over the rendered segment text (no label, timestamp or separator lanes;
//! AD7, quirk 12 fixed) and counts every match range: words joined by
//! separators on one line, such as `needle_token` for `needle token`, are one
//! match. The per-segment normalized forms are built lazily on the first
//! search after an open or rebuild (V-102). For a collapsed tool output the
//! matches in its full text beyond the visible ones are extra matches with
//! `hidden: Some(id)`: revealing one expands the output (PD7, V-078).

use std::collections::HashMap;
use std::ops::Range;

use claude_history_core::display_format::ViewSearchQuery;
use claude_history_core::text_match::NormalizedForm;
use claude_history_core::view::{ConversationDocument, ToolOutputId};

use super::items::{ViewerItem, hidden_full_text};

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

/// The full text of a collapsed output with its normalized form.
struct HiddenText {
    text: String,
    form: NormalizedForm,
}

/// The lazily built normalized forms of the installed items' segments.
#[derive(Default)]
pub struct SearchIndex {
    forms: Vec<Vec<NormalizedForm>>,
    hidden: HashMap<usize, HiddenText>,
}

impl SearchIndex {
    /// Build the normalized form of every segment.
    pub fn build(items: &[ViewerItem]) -> Self {
        let forms = items
            .iter()
            .map(|item| {
                item.segments
                    .iter()
                    .map(|segment| NormalizedForm::new(&segment.text))
                    .collect()
            })
            .collect();
        Self {
            forms,
            hidden: HashMap::new(),
        }
    }

    /// Heap bytes held (probe).
    pub fn bytes(&self) -> usize {
        self.forms
            .iter()
            .flatten()
            .map(NormalizedForm::heap_bytes)
            .sum::<usize>()
            + self
                .hidden
                .values()
                .map(|hidden| hidden.text.capacity() + hidden.form.heap_bytes())
                .sum::<usize>()
    }

    /// Every match of `query` in document order.
    pub fn find(
        &mut self,
        items: &[ViewerItem],
        doc: &ConversationDocument,
        query: &str,
    ) -> Vec<SegMatch> {
        let query = ViewSearchQuery::parse(query);
        if query.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for (item_ix, item) in items.iter().enumerate() {
            let Some(forms) = self.forms.get(item_ix) else {
                break;
            };
            let mut visible = 0usize;
            for (segment_ix, (segment, form)) in item.segments.iter().zip(forms).enumerate() {
                for (start, end) in query.match_ranges_in(&segment.text, form) {
                    visible += 1;
                    matches.push(SegMatch {
                        item_ix,
                        segment_ix,
                        range: start..end,
                        hidden: None,
                    });
                }
            }
            if item.more.is_some()
                && let Some(id) = &item.toggle
            {
                let full = self.hidden.entry(item_ix).or_insert_with(|| {
                    let text = hidden_full_text(doc, item.block).unwrap_or_default();
                    let form = NormalizedForm::new(&text);
                    HiddenText { text, form }
                });
                let total = query.match_ranges_in(&full.text, &full.form).len();
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

/// How a highlighted match is drawn (`render::MatchStyles`, V-075, L-109).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Emphasis {
    /// The `/` search's current match.
    Current,
    /// Every other match of the `/` search.
    Other,
    /// The list hit's hit (core `preview::MatchEmphasis::Hit`), drawn with
    /// the fill of [`Emphasis::Other`] (upstream 949ec4e: the filled block).
    Hit,
    /// Every other match of the list's query (core
    /// `preview::MatchEmphasis::Secondary`).
    Secondary,
}

/// Visible matches per item: `(segment, range, emphasis)`, in segment and
/// byte order.
pub type MatchesByItem = HashMap<usize, Vec<(usize, Range<usize>, Emphasis)>>;

/// Group the visible matches by item for rendering.
pub fn by_item(matches: &[SegMatch], current: Option<usize>) -> MatchesByItem {
    let mut out: MatchesByItem = HashMap::new();
    for (ix, m) in matches.iter().enumerate() {
        if m.hidden.is_some() || m.range.is_empty() {
            continue;
        }
        let emphasis = if current == Some(ix) {
            Emphasis::Current
        } else {
            Emphasis::Other
        };
        out.entry(m.item_ix)
            .or_default()
            .push((m.segment_ix, m.range.clone(), emphasis));
    }
    out
}
