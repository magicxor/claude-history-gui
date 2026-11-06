//! The list's search hit in the shown conversation (upstream 411e227 list
//! preview; DESIGN §9.8a, L-109). Gpui-free: the build worker and a
//! background task run it.
//!
//! The installed items are split into lines (each segment's text at its
//! `\n`s), every navigable message into the range of its items' lines, and
//! core's [`preview::anchor`] picks the line: the one the row's semantic
//! evidence points at, else the first line matching the query. The query's
//! matches are the `/` search's (core `ViewSearchQuery`, AD7), widened to
//! whole words and emphasized by core's [`preview::match_ranges`] (upstream
//! 949ec4e): the matches of the whole query in the hit's message (or, when
//! it has none, the matches on the anchor line) are the hit, drawn with the
//! fill of the `/` search's other matches; every other match is secondary,
//! bold accent text (upstream also marks the anchor line in the gutter; the
//! viewer focuses its message instead). They are classified once per hit,
//! here, for the whole conversation, not per paint.
//!
//! As built: the lines are segment text, without the ledger's name column,
//! timestamp and wrapping, so a query matching only a label (`claude`) finds
//! no line here where upstream's pane anchors on one; text the GUI clips (a
//! long call or summary row) is not searched, as in the `/` search.

use std::ops::Range;

use claude_history_core::browser::ListHit;
use claude_history_core::display_format::ViewSearchQuery;
use claude_history_core::view::MessageRange;
use claude_history_core::view::preview::{self, MatchEmphasis, PreviewDoc};

use super::items::{BuildInfo, ViewerItem};
use super::nav;
use super::search::{Emphasis, MatchesByItem};

/// The line the hit is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HitAnchor {
    pub item: usize,
    pub segment: usize,
    /// Byte range of the line in the segment's text.
    pub line: Range<usize>,
    /// Where the hit is revealed: the line's first match, else its start.
    pub byte: usize,
    /// Navigation position of the message holding the line.
    pub position: Option<usize>,
}

/// What [`find`] found for a hit in a build's items.
#[derive(Clone, Debug, Default)]
pub struct HitFound {
    pub anchor: Option<HitAnchor>,
    /// The query's matches per item, each [`Emphasis::Hit`] or
    /// [`Emphasis::Secondary`].
    pub matches: MatchesByItem,
}

/// One line of the items: bytes `range` of segment `segment` of `item`.
struct LineRef {
    item: usize,
    segment: usize,
    range: Range<usize>,
}

/// Find `hit` in `items` (built with `info`).
pub fn find(items: &[ViewerItem], info: &BuildInfo, hit: &ListHit) -> HitFound {
    let mut lines: Vec<LineRef> = Vec::new();
    let mut texts: Vec<&str> = Vec::new();
    // The first line of each item, then the line count.
    let mut item_lines: Vec<usize> = Vec::with_capacity(items.len() + 1);
    for (item_ix, item) in items.iter().enumerate() {
        item_lines.push(lines.len());
        for (segment_ix, segment) in item.segments.iter().enumerate() {
            let mut start = 0;
            for text in segment.text.split('\n') {
                let end = start + text.len();
                lines.push(LineRef {
                    item: item_ix,
                    segment: segment_ix,
                    range: start..end,
                });
                texts.push(text);
                start = end + 1;
            }
        }
    }
    item_lines.push(lines.len());
    let messages: Vec<MessageRange> = (0..info.nav_count)
        .filter_map(|position| {
            let (first, last) = nav::item_range_of(items, info, position)?;
            // A navigable message without items has no lines.
            (items[first].nav_position == Some(position)).then(|| MessageRange {
                entry_index: info.nav_entries[position],
                start_line: item_lines[first],
                end_line: item_lines[last + 1],
            })
        })
        .filter(|message| message.start_line < message.end_line)
        .collect();
    let doc = PreviewDoc::new(texts, messages);
    let anchor_ix = preview::anchor(&doc, &hit.query, hit.evidence.as_deref())
        .filter(|&line| line < lines.len());
    let anchor_line = anchor_ix.map(|line| &lines[line]);

    let query = ViewSearchQuery::parse(&hit.query);
    let mut matches = MatchesByItem::new();
    let mut first_on_anchor = None;
    let ranges = preview::match_ranges(&doc, 0..lines.len(), anchor_ix, &query);
    // Lines are in item, segment and byte order, so each item's matches are.
    for (line_ix, (line, ranges)) in lines.iter().zip(ranges).enumerate() {
        for (start, end, emphasis) in ranges {
            let (start, end) = (line.range.start + start, line.range.start + end);
            if anchor_ix == Some(line_ix) && first_on_anchor.is_none() {
                first_on_anchor = Some(start);
            }
            let emphasis = match emphasis {
                MatchEmphasis::Hit => Emphasis::Hit,
                MatchEmphasis::Secondary => Emphasis::Secondary,
            };
            matches
                .entry(line.item)
                .or_default()
                .push((line.segment, start..end, emphasis));
        }
    }
    let anchor = anchor_line.map(|line| HitAnchor {
        item: line.item,
        segment: line.segment,
        line: line.range.clone(),
        byte: first_on_anchor.unwrap_or(line.range.start),
        position: items[line.item].nav_position,
    });
    HitFound { anchor, matches }
}
