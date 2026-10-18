//! Message navigation with core `view::MessageNav` (DESIGN §9.9): J/K,
//! focus mark, reveal by agent ordinal (K5). WP2.
//!
//! Positions are the navigable messages (`ConversationDocument::
//! navigable_indices`, V-071); every item carries its message's position.
//! These helpers map between items, positions and the viewport; the state
//! machine itself is core's (`MessageNav`, quirk 11 kept by core).

use super::items::{BuildInfo, ViewerItem};

/// The navigation position of the message owning item `ix`, else the first
/// navigable message after it (the `first_visible` argument of
/// `MessageNav`).
pub fn position_at(items: &[ViewerItem], info: &BuildInfo, ix: usize) -> Option<usize> {
    if let Some(position) = items.get(ix).and_then(|item| item.nav_position) {
        return Some(position);
    }
    let next = info.nav_first_item.partition_point(|&first| first < ix);
    (next < info.nav_count).then_some(next)
}

/// The first item of navigation position `position`.
pub fn first_item_of(info: &BuildInfo, position: usize) -> Option<usize> {
    info.nav_first_item.get(position).copied()
}

/// The navigation position of the first navigable message whose top is at
/// or below the top of the viewport (`top_ix` with `offset_zero` = the top
/// item starts exactly at the viewport top) — `capture_anchor_entry`'s
/// `first_at_or_below_top`.
pub fn first_at_or_below_top(info: &BuildInfo, top_ix: usize, offset_zero: bool) -> Option<usize> {
    let position = info.nav_first_item.partition_point(|&first| {
        if offset_zero {
            first < top_ix
        } else {
            first <= top_ix
        }
    });
    (position < info.nav_count).then_some(position)
}

/// The `[i/N]` scroll indicator (V-009): N navigable messages, i = 1 + the
/// position of the message at the top of the viewport; `[1/1]` when empty.
pub fn indicator(items: &[ViewerItem], info: &BuildInfo, top_ix: usize) -> String {
    let total = info.nav_count;
    if total == 0 {
        return "[1/1]".to_string();
    }
    let position = position_at(items, info, top_ix).unwrap_or(total - 1);
    format!("[{}/{}]", position.min(total - 1) + 1, total)
}

/// The item range `[first, last]` of navigation position `position`.
pub fn item_range_of(
    items: &[ViewerItem],
    info: &BuildInfo,
    position: usize,
) -> Option<(usize, usize)> {
    let first = first_item_of(info, position)?;
    let message = items.get(first)?.message_index;
    let mut last = first;
    while last + 1 < items.len() && items[last + 1].message_index == message {
        last += 1;
    }
    Some((first, last))
}

/// The navigation position of the entry `entry_index`, else of the next
/// navigable entry after it (agent reveals, §9.9).
pub fn position_of_entry_or_next(info: &BuildInfo, entry_index: usize) -> Option<usize> {
    if let Some(position) = info.nav_entries.iter().position(|&e| e == entry_index) {
        return Some(position);
    }
    info.nav_entries.iter().position(|&e| e > entry_index)
}
