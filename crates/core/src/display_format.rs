//! Pure list/viewer formatting helpers extracted from upstream `tui/ui.rs`
//! (plan section 5.9; `inventory-list-mode.md` sections 4.4-4.6 and 13).
//!
//! Everything here is independent of any UI toolkit:
//!
//! * highlight helpers return **byte ranges** into the text they were given
//!   (upstream built ratatui `Span`s); [`highlight_segments`] splits a text
//!   into plain/highlighted pieces for renderers that want runs;
//! * timestamps carry a [`Recency`] bucket instead of a color;
//! * widths are budgets in terminal cells (`unicode-width`), exactly as
//!   upstream. A GUI with proportional fonts can pass a character budget
//!   (for example `row_width_px / average_char_width_px`) and let its text
//!   element ellipsize the rest.
//!
//! [`build_row_preview`] implements the complete "which text, which
//! truncation, which context line" decision of a list row (inventory 4.6),
//! and [`build_row_header`] the header composition (inventory 4.3).

use crate::browser::SemanticResultMetadata;
use crate::history::Conversation;
use crate::search::LexicalEvidence;
use crate::search::evidence::select_hidden_context_ranges;
use crate::search::literal::{Literal, match_literal_ranges};
use crate::search::normalize_for_search;
use crate::search::query::ParsedQuery;
use crate::semantic::types::SemanticRationaleKind;
use chrono::{DateTime, Local};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ---------------------------------------------------------------------------
// Viewer header values
// ---------------------------------------------------------------------------

/// Format a model name for display (e.g. `"claude-opus-4-5-20251101"` →
/// `"opus-4.5"`). Unknown names longer than 20 characters are cut to 19
/// characters plus `…`.
///
/// Upstream sliced the first 19 *bytes*, which panics on non-ASCII names;
/// core cuts by characters (identical for ASCII names, W18-style fix).
pub fn format_model_name(model: &str) -> String {
    const DATED_MODELS: [(&str, &str); 7] = [
        // Handle claude-opus-4-5-YYYYMMDD format
        ("claude-opus-4-5-", "opus-4.5"),
        // Handle claude-sonnet-4-YYYYMMDD format
        ("claude-sonnet-4-", "sonnet-4"),
        // Handle claude-3-5-sonnet-YYYYMMDD format
        ("claude-3-5-sonnet-", "sonnet-3.5"),
        // Handle claude-3-5-haiku-YYYYMMDD format
        ("claude-3-5-haiku-", "haiku-3.5"),
        // Handle claude-3-opus-YYYYMMDD format
        ("claude-3-opus-", "opus-3"),
        // Handle claude-3-sonnet-YYYYMMDD format
        ("claude-3-sonnet-", "sonnet-3"),
        // Handle claude-3-haiku-YYYYMMDD format
        ("claude-3-haiku-", "haiku-3"),
    ];
    for (prefix, short) in DATED_MODELS {
        if let Some(rest) = model.strip_prefix(prefix)
            && rest.chars().all(|c| c.is_ascii_digit())
        {
            return short.to_string();
        }
    }

    // Unknown format - truncate if too long
    if model.chars().count() > 20 {
        format!("{}…", model.chars().take(19).collect::<String>())
    } else {
        model.to_string()
    }
}

/// Token count with a K/M suffix (short form, e.g. `"926k"`, `"1.2M"`).
pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// Token count with suffix and label (long form, e.g. `"926k tokens"`).
pub fn format_tokens_long(tokens: u64) -> String {
    format!("{} tokens", format_tokens(tokens))
}

/// List-row message count: `"1 msg"` / `"<n> msgs"`.
pub fn format_msg_count_short(count: usize) -> String {
    if count == 1 {
        "1 msg".to_string()
    } else {
        format!("{} msgs", count)
    }
}

/// Viewer-header message count: `"1 message"` / `"<n> messages"`.
pub fn format_msg_count_long(count: usize) -> String {
    if count == 1 {
        "1 message".to_string()
    } else {
        format!("{} messages", count)
    }
}

/// Conversation duration: `"<m>m"` below an hour, else `"<h>h <m>m"`.
pub fn format_duration_minutes(minutes: u64) -> String {
    if minutes >= 60 {
        format!("{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("{}m", minutes)
    }
}

/// Viewer-header timestamp: `"%Y-%m-%d %H:%M"` (local time).
pub fn format_header_timestamp(timestamp: DateTime<Local>) -> String {
    timestamp.format("%Y-%m-%d %H:%M").to_string()
}

/// Whether the viewer header (with summary) fits on one line of
/// `terminal_width` cells (upstream byte-length heuristic, kept verbatim).
pub fn header_fits_single_line(conv: &Conversation, terminal_width: u16) -> bool {
    let summary = match &conv.summary {
        Some(s) => s,
        None => return true, // No summary means it's already single line
    };

    let project = conv.project_name.as_deref().unwrap_or("Unknown");

    // Calculate custom title length if present
    let custom_title_len = conv
        .custom_title
        .as_ref()
        .map(|t| t.chars().count() + 3) // + " · "
        .unwrap_or(0);

    // Calculate model length if present
    let model_len = conv
        .model
        .as_ref()
        .map(|m| format_model_name(m).len() + 3) // + " · "
        .unwrap_or(0);

    let msg_count_len = format_msg_count_long(conv.message_count).len();

    // Calculate tokens length if present (use long form for single-line check)
    let tokens_len = if conv.total_tokens > 0 {
        format_tokens_long(conv.total_tokens).len() + 3 // + " · "
    } else {
        0
    };

    // timestamp is "YYYY-MM-DD HH:MM" = 16 chars
    let timestamp_len = 16;

    // Duration length (if present): " · Xm" or " · Xh Ym" etc.
    let duration_len = conv
        .duration_minutes
        .map_or(0, |m| 3 + format_duration_minutes(m).len());

    // Format: "  project · custom_title · model · msg_count · duration · tokens · timestamp · summary"
    let total_len = 2
        + project.len()
        + 3
        + custom_title_len
        + model_len
        + msg_count_len
        + duration_len
        + 3
        + tokens_len
        + timestamp_len
        + 3
        + summary.len();

    total_len <= terminal_width as usize
}

/// Width of the ledger name column ([`crate::view::NAME_WIDTH`]).
pub const LEDGER_NAME_WIDTH: usize = crate::view::NAME_WIDTH;
/// Width of the ledger focus gutter ([`crate::view::GUTTER_WIDTH`]).
pub const LEDGER_GUTTER_WIDTH: usize = crate::view::GUTTER_WIDTH;
/// Width of the ledger timing column ([`crate::view::TIMESTAMP_WIDTH`]).
pub const LEDGER_TIMESTAMP_WIDTH: usize = crate::view::TIMESTAMP_WIDTH;

/// Content width of the monospace ledger viewer for a frame `frame_width`
/// cells wide (upstream `runtime.rs`): the frame minus the name column, the
/// `" │ "` separator, the focus gutter and, with timing on, the timestamp
/// column. Same as [`crate::view::content_width_for_frame`].
pub fn calculate_content_width(frame_width: usize, show_timing: bool) -> usize {
    crate::view::content_width_for_frame(frame_width, show_timing)
}

// ---------------------------------------------------------------------------
// Relative timestamps
// ---------------------------------------------------------------------------

/// Age bucket of a list timestamp (upstream picked the timestamp color
/// from it: `timestamp_now/minutes/hours/days`, `text_secondary` for old).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Recency {
    Now,
    Minutes,
    Hours,
    Days,
    Old,
}

/// Relative time for recent timestamps, absolute for older ones:
/// `"just now"`, `"<m> min ago"`, `"1 hour ago"`/`"<h> hours ago"`,
/// `"yesterday"` (calendar day), `"<d> days ago"` (under a week), else
/// `"%b %d, %H:%M"`. Future timestamps (clock skew) are absolute and
/// [`Recency::Old`]. Compute `now` once per frame.
pub fn format_timestamp(timestamp: DateTime<Local>, now: DateTime<Local>) -> (String, Recency) {
    let age = now.signed_duration_since(timestamp);

    // Future timestamps (clock skew): show absolute
    if age.num_seconds() < 0 {
        return (timestamp.format("%b %d, %H:%M").to_string(), Recency::Old);
    }

    let seconds = age.num_seconds();
    let minutes = age.num_minutes();
    let hours = age.num_hours();

    if seconds < 60 {
        return ("just now".to_string(), Recency::Now);
    }
    if minutes < 60 {
        return (format!("{minutes} min ago"), Recency::Minutes);
    }
    if hours < 24 {
        return (
            format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" }),
            Recency::Hours,
        );
    }

    // Use calendar day difference for "yesterday" accuracy
    let day_diff = now
        .date_naive()
        .signed_duration_since(timestamp.date_naive())
        .num_days();
    if day_diff == 1 {
        return ("yesterday".to_string(), Recency::Days);
    }
    if day_diff < 7 {
        return (format!("{day_diff} days ago"), Recency::Days);
    }

    (timestamp.format("%b %d, %H:%M").to_string(), Recency::Old)
}

/// Alias of [`format_timestamp`] (the name used by the extraction plan).
pub fn format_relative_timestamp(
    timestamp: DateTime<Local>,
    now: DateTime<Local>,
) -> (String, Recency) {
    format_timestamp(timestamp, now)
}

// ---------------------------------------------------------------------------
// Semantic labels
// ---------------------------------------------------------------------------

/// Rationale label of a semantic hit: `"semantic"`, `"lex boost"`, `"weak"`.
pub fn semantic_rationale_label(metadata: &SemanticResultMetadata) -> &'static str {
    match metadata.explanation.rationale_kind {
        SemanticRationaleKind::SemanticOnly => "semantic",
        SemanticRationaleKind::LexicalBoosted => "lex boost",
        SemanticRationaleKind::WeakMatch => "weak",
    }
}

/// Compact score shown on semantic rows: the hybrid score, two decimals.
pub fn semantic_row_metadata(metadata: &SemanticResultMetadata) -> String {
    format!("{:.2}", metadata.score_breakdown.hybrid)
}

// ---------------------------------------------------------------------------
// Truncation
// ---------------------------------------------------------------------------

/// Sanitize preview text: drop everything between `<` and `>` (tag-like
/// text), collapse newlines, tabs and runs of spaces to one space, trim.
pub fn sanitize_preview(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut last_was_space = false;

    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            '\n' | '\r' | '\t' => {
                if !last_was_space {
                    result.push(' ');
                    last_was_space = true;
                }
            }
            ' ' => {
                if !last_was_space {
                    result.push(' ');
                    last_was_space = true;
                }
            }
            _ => {
                result.push(ch);
                last_was_space = false;
            }
        }
    }

    result.trim().to_string()
}

/// Keep a prefix of `text` that fits `max_width` cells, ending in `…` when
/// truncated.
pub fn simple_truncate(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut result = String::new();
    let ellipsis_width = UnicodeWidthChar::width('…').unwrap_or(1);
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width + ellipsis_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result.push('…');
    result
}

/// Keep a suffix of `text` that fits `max_width` cells, starting with `…`
/// when truncated.
pub fn truncate_start(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let ellipsis_width = UnicodeWidthChar::width('…').unwrap_or(1);
    let mut chars = Vec::new();
    let mut width = 0;
    for ch in text.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width + ellipsis_width > max_width {
            break;
        }
        chars.push(ch);
        width += ch_width;
    }
    chars.reverse();
    format!("…{}", chars.into_iter().collect::<String>())
}

/// Center the first normalized match of `query_normalized` in `max_width`
/// cells, with `…` on cut sides.
pub fn truncate_around_match(text: &str, query_normalized: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let ranges = find_normalized_match_ranges(text, query_normalized);
    let Some((start, end)) = ranges.first().copied() else {
        return simple_truncate(text, max_width);
    };
    let matched = &text[start..end];
    let matched_width = UnicodeWidthStr::width(matched);
    if matched_width >= max_width {
        return simple_truncate(matched, max_width);
    }

    let ellipsis_budget = usize::from(start > 0) + usize::from(end < text.len());
    let context_budget = max_width.saturating_sub(matched_width + ellipsis_budget);
    let left_budget = context_budget / 2;
    let right_budget = context_budget - left_budget;
    format!(
        "{}{}{}",
        truncate_start(&text[..start], left_budget + usize::from(start > 0)),
        matched,
        simple_truncate(&text[end..], right_budget + usize::from(end < text.len()))
    )
}

// ---------------------------------------------------------------------------
// Windows around matches
// ---------------------------------------------------------------------------

/// Context around each match of `query` (normalized and literal terms),
/// joined by `…`. Falls back to [`simple_truncate`] when every match fits.
pub fn build_match_segments_for_query(
    text: &str,
    query: &HighlightQuery,
    max_width: usize,
) -> String {
    build_match_segments_with_ranges(text, query.match_ranges(text), max_width, || {
        simple_truncate(text, max_width)
    })
}

/// Context around each normalized match of `query_normalized` (see
/// [`HighlightQuery::context_text`]), joined by `…`; falls back to
/// [`truncate_around_match`] when the segments do not fit.
pub fn build_match_segments(text: &str, query_normalized: &str, max_width: usize) -> String {
    if query_normalized.is_empty() || max_width == 0 {
        return simple_truncate(text, max_width);
    }

    let ranges = find_normalized_match_ranges(text, query_normalized);
    build_match_segments_with_ranges(text, ranges, max_width, || {
        truncate_around_match(text, query_normalized, max_width)
    })
}

/// Windows around byte `ranges` of `text`: when every match ends within
/// the budget, a plain prefix; otherwise matches closer than 20 characters
/// are clustered (at most 3 clusters), the remaining width is spread evenly
/// around them, and segments are joined by `…`. If the result is still too
/// wide, `fallback` is used.
pub fn build_match_segments_with_ranges(
    text: &str,
    ranges: Vec<(usize, usize)>,
    max_width: usize,
    fallback: impl Fn() -> String,
) -> String {
    if ranges.is_empty() {
        return simple_truncate(text, max_width);
    }

    // Convert byte ranges to char ranges for width budgeting
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();
    let text_char_len = char_indices.len();

    // Map byte offset → char index
    let byte_to_char = |byte_pos: usize| -> usize {
        char_indices
            .iter()
            .position(|(b, _)| *b >= byte_pos)
            .unwrap_or(text_char_len)
    };

    let char_ranges: Vec<(usize, usize)> = ranges
        .iter()
        .map(|(s, e)| (byte_to_char(*s), byte_to_char(*e)))
        .collect();

    // If all matches fit within simple truncation, use that
    let last_match_end = char_ranges.last().map(|(_, e)| *e).unwrap_or(0);
    if last_match_end <= max_width.saturating_sub(1) {
        return simple_truncate(text, max_width);
    }

    // Cluster nearby matches (gap < 20 chars)
    let merge_gap = 20;
    let mut clusters: Vec<(usize, usize)> = Vec::new(); // (char_start, char_end) of cluster
    for &(cs, ce) in &char_ranges {
        if let Some(last) = clusters.last_mut()
            && cs <= last.1 + merge_gap
        {
            last.1 = last.1.max(ce);
            continue;
        }
        clusters.push((cs, ce));
    }

    // Cap at 3 clusters
    clusters.truncate(3);

    // Calculate how many ellipsis chars we need
    let num_clusters = clusters.len();
    // Ellipsis between clusters + possibly leading + possibly trailing
    let match_chars: usize = clusters.iter().map(|(s, e)| e - s).sum();
    // We need at least 1 ellipsis between each pair + leading if first doesn't start at 0
    // + trailing (assume we always need trailing since text was too long)
    let max_ellipsis = num_clusters + 1; // worst case: leading + between each + trailing
    let available_context = max_width
        .saturating_sub(match_chars)
        .saturating_sub(max_ellipsis);
    let padding_per_side = if num_clusters > 0 {
        available_context / (num_clusters * 2)
    } else {
        0
    };

    // Build segments, tracking last position to prevent overlap
    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut last_seg_end: usize = 0;

    for (i, &(cl_start, cl_end)) in clusters.iter().enumerate() {
        let mut seg_start = cl_start.saturating_sub(padding_per_side);
        let seg_end = (cl_end + padding_per_side).min(text_char_len);

        // Prevent overlapping with previous segment
        if i > 0 {
            seg_start = seg_start.max(last_seg_end);
        }

        if (i == 0 && seg_start > 0) || (i > 0 && seg_start > last_seg_end) {
            result.push('…');
        }

        let segment: String = chars[seg_start..seg_end].iter().collect();
        result.push_str(&segment);
        last_seg_end = seg_end;
    }

    // Add trailing ellipsis if we didn't reach the end
    let last_cluster_end = clusters.last().map(|(_, e)| *e).unwrap_or(0);
    if last_cluster_end + padding_per_side < text_char_len {
        result.push('…');
    }

    if UnicodeWidthStr::width(result.as_str()) > max_width {
        return fallback();
    }

    result
}

// ---------------------------------------------------------------------------
// Context snippets from the full text
// ---------------------------------------------------------------------------

/// Context line for hidden **literal** matches (quoted literals and
/// unquoted `_` identifiers): snippets from `full_text` around literal hits
/// that are not visible in `preview`, chosen by cluster ranking (missing
/// terms covered, adjacent term pairs, unique terms, tighter span, earlier
/// position). `None` when nothing needs showing.
pub fn build_literal_context_segments(
    full_text: &str,
    preview: &str,
    query: &HighlightQuery,
    max_width: usize,
) -> Option<String> {
    build_context_segments_with_specs(
        full_text,
        preview,
        &query.literal_context_specs(),
        max_width,
    )
}

/// Like [`build_literal_context_segments`], for every term of `query`.
pub fn build_context_segments_for_query(
    full_text: &str,
    preview: &str,
    query: &HighlightQuery,
    max_width: usize,
) -> Option<String> {
    build_context_segments_with_specs(full_text, preview, &query.context_specs(), max_width)
}

/// Like [`build_context_segments_for_query`], for normalized terms only
/// (`query_normalized` is split on whitespace).
pub fn build_context_segments(
    full_text: &str,
    preview: &str,
    query_normalized: &str,
    max_width: usize,
) -> Option<String> {
    build_context_segments_with_specs(
        full_text,
        preview,
        &HighlightQuery::normalized_context_specs(query_normalized),
        max_width,
    )
}

/// Snippets of `full_text` around precomputed byte ranges (lexical evidence
/// `context_ranges`).
pub fn build_context_segments_from_ranges(
    full_text: &str,
    hidden_matches: &[(usize, usize)],
    max_width: usize,
) -> Option<String> {
    if hidden_matches.is_empty() || max_width == 0 {
        return None;
    }

    build_context_segments_from_ranges_unchecked(full_text, hidden_matches, max_width)
}

/// Snippets around the hits of `specs` in `full_text` that are hidden from
/// `preview`. Specs are deduped (case-insensitively, at most 64). When every
/// spec is visible in the preview, a snippet is produced only if
/// `full_text` has more hits than the preview.
pub fn build_context_segments_with_specs(
    full_text: &str,
    preview: &str,
    specs: &[ContextMatchSpec],
    max_width: usize,
) -> Option<String> {
    if specs.is_empty() || max_width == 0 {
        return None;
    }

    let specs = dedupe_context_specs(specs);
    if specs.is_empty() {
        return None;
    }

    let has_normalized_specs = specs
        .iter()
        .any(|spec| matches!(spec, ContextMatchSpec::Normalized(_)));
    let preview_normalized = has_normalized_specs.then(|| NormalizedText::new(preview));
    let full_text_normalized = has_normalized_specs.then(|| NormalizedText::new(full_text));

    let mut missing_mask: u64 = 0;
    let mut missing_count = 0u32;
    for (i, spec) in specs.iter().enumerate() {
        if spec
            .find_ranges(preview, preview_normalized.as_ref())
            .is_empty()
        {
            missing_mask |= 1 << i;
            missing_count += 1;
        }
    }

    let all_hits = find_all_spec_hits(full_text, &specs, full_text_normalized.as_ref());
    if all_hits.is_empty() {
        return None;
    }

    // Fallback: if every term is already visible in the preview, only emit a
    // context line when full_text contains *more* hits than the preview does.
    // We don't try to skip positionally — `preview` is sanitized/truncated and
    // `full_text` is raw, so positional alignment between the two hit streams
    // is unreliable. Instead we let the cluster ranker pick the most
    // informative cluster across the whole document. Worst case it picks one
    // that overlaps preview content, which is still the best snippet we have.
    if missing_count == 0 {
        let preview_hit_count =
            find_all_spec_hits(preview, &specs, preview_normalized.as_ref()).len();
        if all_hits.len() <= preview_hit_count {
            return None;
        }
    }

    let hit_spans: Vec<(usize, usize, usize)> = all_hits
        .iter()
        .map(|hit| (hit.start, hit.end, hit.term_idx))
        .collect();
    let hidden_matches =
        select_hidden_context_ranges(full_text, &hit_spans, missing_mask, missing_count)?;

    build_context_segments_from_ranges_unchecked(full_text, &hidden_matches, max_width)
}

fn build_context_segments_from_ranges_unchecked(
    full_text: &str,
    hidden_matches: &[(usize, usize)],
    max_width: usize,
) -> Option<String> {
    // For each hidden match cluster, extract a context window from raw full_text,
    // then sanitize just that slice
    let num_segments = hidden_matches.len();
    let budget_per_segment = max_width.saturating_sub(num_segments + 1) / num_segments; // reserve for ellipsis

    let mut result = String::new();
    let mut remaining_width = max_width;
    let mut prev_end_byte: usize = 0;

    for (i, &(match_start, match_end)) in hidden_matches.iter().enumerate() {
        let match_char_len = full_text[match_start..match_end].chars().count();
        let context_chars = budget_per_segment
            .saturating_sub(match_char_len)
            .saturating_sub(2) // reserve for "…" on each side
            / 2;

        // Find char boundaries for the context window in raw full_text
        let mut start_byte = full_text[..match_start]
            .char_indices()
            .rev()
            .nth(context_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(0);

        // Prevent overlapping with previous segment
        start_byte = start_byte.max(prev_end_byte);

        let end_byte = full_text[match_end..]
            .char_indices()
            .nth(context_chars)
            .map(|(idx, _)| match_end + idx)
            .unwrap_or(full_text.len())
            .min(full_text.len());

        let snippet = &full_text[start_byte..end_byte];
        let sanitized = sanitize_preview(snippet);

        // Add ellipsis if there's a gap before this segment
        let has_gap = if i == 0 {
            start_byte > 0
        } else {
            start_byte > prev_end_byte
        };
        if has_gap {
            result.push('…');
            remaining_width = remaining_width.saturating_sub(1);
        }

        prev_end_byte = end_byte;

        // Append segment, truncating if needed
        let seg_char_count = sanitized.chars().count();
        if seg_char_count <= remaining_width {
            result.push_str(&sanitized);
            remaining_width = remaining_width.saturating_sub(seg_char_count);
        } else {
            // Truncate this segment to fit
            let budget = remaining_width.saturating_sub(1);
            let trunc: String = sanitized.chars().take(budget).collect();
            result.push_str(&trunc);
            result.push('…');
            remaining_width = 0;
            break;
        }
    }

    // Add trailing ellipsis if last match didn't reach end of full_text
    if remaining_width > 0 {
        let last_end = hidden_matches.last().map(|(_, e)| *e).unwrap_or(0);
        if last_end < full_text.len() {
            result.push('…');
        }
    }

    if result.is_empty() {
        None
    } else {
        // Final safety truncation
        Some(simple_truncate(&result, max_width))
    }
}

// ---------------------------------------------------------------------------
// Normalized matching
// ---------------------------------------------------------------------------

/// One match used to compute term coverage and phrase density per cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TermHit {
    pub start: usize,
    pub end: usize,
    pub term_idx: usize,
}

/// Text normalized for matching (lowercase, `_` as space) with a mapping
/// back to byte ranges of the original text, for repeated searches.
#[derive(Clone, Debug)]
pub struct NormalizedText {
    norm_chars: Vec<char>,
    char_map: Vec<(usize, usize)>,
}

impl NormalizedText {
    pub fn new(text: &str) -> Self {
        let mut norm_chars: Vec<char> = Vec::new();
        let mut char_map: Vec<(usize, usize)> = Vec::new();

        let mut iter = text.char_indices().peekable();
        while let Some((byte_start, ch)) = iter.next() {
            let byte_end = iter.peek().map_or(text.len(), |(i, _)| *i);
            if ch == '_' {
                norm_chars.push(' ');
                char_map.push((byte_start, byte_end));
            } else {
                for lc in ch.to_lowercase() {
                    norm_chars.push(lc);
                    char_map.push((byte_start, byte_end));
                }
            }
        }

        Self {
            norm_chars,
            char_map,
        }
    }

    /// All non-overlapping matches of one normalized term, requiring a word
    /// start when the term starts with an alphanumeric character (`red`
    /// matches `redaction` but not `fired`). Byte ranges in the original.
    pub fn find_term_ranges(&self, term: &str) -> Vec<(usize, usize)> {
        let query_chars: Vec<char> = term.chars().collect();
        if query_chars.is_empty() {
            return Vec::new();
        }

        let query_starts_alnum = query_chars.first().is_some_and(|c| c.is_alphanumeric());
        let mut matches = Vec::new();

        let mut i = 0;
        while i + query_chars.len() <= self.norm_chars.len() {
            if self.norm_chars[i..i + query_chars.len()] == query_chars[..] {
                let prev_is_alnum = i > 0 && self.norm_chars[i - 1].is_alphanumeric();
                let valid_start = !query_starts_alnum || !prev_is_alnum;

                if valid_start {
                    let start_byte = self.char_map[i].0;
                    let end_byte = self.char_map[i + query_chars.len() - 1].1;
                    matches.push((start_byte, end_byte));
                    i += query_chars.len();
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }

        matches
    }

    /// Matches of every whitespace-separated term, sorted and merged;
    /// ranges separated only by ` _ - /` are merged into one (so
    /// `run with loader` highlights `run_with_loader` as one span).
    pub fn find_all_ranges(&self, query_normalized: &str) -> Vec<(usize, usize)> {
        let terms: Vec<&str> = query_normalized.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let mut all_matches = Vec::new();
        for term in &terms {
            all_matches.extend(self.find_term_ranges(term));
        }

        // Sort and merge overlapping or separator-adjacent ranges.
        // This ensures "run_with_loader" highlights as one span including the underscores
        // when searching "run with loader" (underscores normalized to spaces).
        all_matches.sort_unstable_by_key(|m| m.0);
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(all_matches.len());
        for m in all_matches {
            if let Some(last) = merged.last_mut() {
                if m.0 <= last.1 {
                    // Overlapping — merge
                    last.1 = last.1.max(m.1);
                    continue;
                }
                // Check if the gap between ranges is only separators (_, -, /)
                let gap = &self.norm_chars[..];
                let gap_start = self.byte_to_char_index(last.1);
                let gap_end = self.byte_to_char_index(m.0);
                if gap_start < gap_end
                    && gap[gap_start..gap_end]
                        .iter()
                        .all(|c| *c == ' ' || *c == '_' || *c == '-' || *c == '/')
                {
                    last.1 = m.1;
                    continue;
                }
            }
            merged.push(m);
        }

        merged
    }

    /// Convert a byte offset in the original text to a char index in norm_chars
    fn byte_to_char_index(&self, byte_offset: usize) -> usize {
        self.char_map
            .iter()
            .position(|(start, _)| *start >= byte_offset)
            .unwrap_or(self.char_map.len())
    }
}

/// All matches of `query_normalized` in `text` (see
/// [`NormalizedText::find_all_ranges`]), as byte ranges of `text`.
pub fn find_normalized_match_ranges(text: &str, query_normalized: &str) -> Vec<(usize, usize)> {
    NormalizedText::new(text).find_all_ranges(query_normalized)
}

// ---------------------------------------------------------------------------
// Highlight query
// ---------------------------------------------------------------------------

/// A list query prepared for highlighting.
///
/// Literals are the quoted literals plus unquoted terms containing `_`
/// (identifier literals: exact substring, smart case). The remaining
/// unquoted terms are normalized with
/// [`crate::search::normalize_for_search`].
#[derive(Clone, Debug, Default)]
pub struct HighlightQuery {
    unquoted: String,
    literals: Vec<Literal>,
}

impl HighlightQuery {
    pub fn parse(query: &str) -> Self {
        let parsed = ParsedQuery::parse(query);
        let unquoted_terms = parsed.unquoted().split_whitespace().collect::<Vec<_>>();
        let identifier_literals = unquoted_terms
            .iter()
            .copied()
            .filter(|term| term.contains('_'))
            .map(|term| Literal::new(term.to_string()))
            .collect::<Vec<_>>();
        let unquoted = unquoted_terms
            .into_iter()
            .filter(|term| !term.contains('_'))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            unquoted: normalize_highlight_query(&unquoted),
            literals: parsed
                .literals()
                .iter()
                .cloned()
                .chain(identifier_literals)
                .collect(),
        }
    }

    /// The normalized unquoted terms, space-separated.
    pub fn unquoted(&self) -> &str {
        &self.unquoted
    }

    /// Quoted literals followed by identifier literals.
    pub fn literals(&self) -> &[Literal] {
        &self.literals
    }

    /// Every term (unquoted and literal) normalized into one string; empty
    /// when the query has no terms.
    pub fn context_text(&self) -> String {
        normalize_highlight_query(
            [self.unquoted.as_str()]
                .into_iter()
                .chain(self.literals.iter().map(Literal::text))
                .collect::<Vec<_>>()
                .join(" ")
                .as_str(),
        )
    }

    /// Whether the query has no terms at all (no highlighting).
    pub fn is_empty(&self) -> bool {
        self.context_text().is_empty()
    }

    /// Byte ranges of `text` to highlight: normalized-term and literal
    /// matches, merged, within `text` (upstream `highlight`).
    pub fn highlight_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let mut ranges = find_normalized_match_ranges(text, &self.unquoted);
        ranges.extend(match_literal_ranges(text, &self.literals));
        merge_match_ranges(ranges)
            .into_iter()
            .filter(|(_, end)| *end <= text.len())
            .collect()
    }

    /// Normalized-term specs followed by literal specs.
    pub fn context_specs(&self) -> Vec<ContextMatchSpec> {
        Self::normalized_context_specs(&self.unquoted)
            .into_iter()
            .chain(self.literals.iter().cloned().map(ContextMatchSpec::Literal))
            .collect()
    }

    /// Literal specs only (the literal context line).
    pub fn literal_context_specs(&self) -> Vec<ContextMatchSpec> {
        self.literals
            .iter()
            .cloned()
            .map(ContextMatchSpec::Literal)
            .collect()
    }

    /// Whether any term or literal matches `text`.
    pub fn has_match(&self, text: &str) -> bool {
        !find_normalized_match_ranges(text, &self.unquoted).is_empty()
            || self.has_literal_match(text)
    }

    /// Whether any literal matches `text`.
    pub fn has_literal_match(&self, text: &str) -> bool {
        self.literals.iter().any(|literal| literal.matches(text))
    }

    /// Whether some literal is not visible in `preview` (a literal context
    /// line may then help).
    pub fn needs_literal_context(&self, preview: &str) -> bool {
        self.literals
            .iter()
            .any(|literal| !literal.matches(preview))
    }

    /// Merged byte ranges of every term and literal match in `text`.
    pub fn match_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let mut ranges = find_normalized_match_ranges(text, &self.unquoted);
        ranges.extend(match_literal_ranges(text, &self.literals));
        merge_match_ranges(ranges)
    }

    /// One normalized spec per whitespace-separated term.
    pub fn normalized_context_specs(query: &str) -> Vec<ContextMatchSpec> {
        query
            .split_whitespace()
            .map(|term| ContextMatchSpec::Normalized(term.to_string()))
            .collect()
    }
}

/// A term searched for in context snippets.
#[derive(Clone, Debug)]
pub enum ContextMatchSpec {
    /// A normalized term (word-start, case-insensitive matching).
    Normalized(String),
    /// A literal (exact substring, smart case).
    Literal(Literal),
}

impl ContextMatchSpec {
    /// The spec text and whether it is a literal.
    pub fn key(&self) -> (&str, bool) {
        match self {
            Self::Normalized(term) => (term.as_str(), false),
            Self::Literal(literal) => (literal.text(), true),
        }
    }

    /// Byte ranges of `text` matching this spec; `normalized` is a cached
    /// [`NormalizedText`] of `text`.
    pub fn find_ranges(
        &self,
        text: &str,
        normalized: Option<&NormalizedText>,
    ) -> Vec<(usize, usize)> {
        match self {
            Self::Normalized(term) => normalized
                .map(|normalized| normalized.find_all_ranges(term))
                .unwrap_or_else(|| find_normalized_match_ranges(text, term)),
            Self::Literal(literal) => literal.match_ranges(text),
        }
    }
}

/// Drop specs equal (ASCII case-insensitively, same kind) to an earlier
/// one; keep at most 64.
pub fn dedupe_context_specs(specs: &[ContextMatchSpec]) -> Vec<ContextMatchSpec> {
    let mut deduped = Vec::new();
    for spec in specs {
        let (text, is_literal) = spec.key();
        if !deduped.iter().any(|existing: &ContextMatchSpec| {
            let (existing_text, existing_is_literal) = existing.key();
            existing_is_literal == is_literal && existing_text.eq_ignore_ascii_case(text)
        }) {
            deduped.push(spec.clone());
            if deduped.len() == 64 {
                break;
            }
        }
    }
    deduped
}

/// Every hit of every spec in `text`, sorted by start.
pub fn find_all_spec_hits(
    text: &str,
    specs: &[ContextMatchSpec],
    normalized: Option<&NormalizedText>,
) -> Vec<TermHit> {
    let mut all_hits = Vec::new();
    for (term_idx, spec) in specs.iter().enumerate() {
        all_hits.extend(
            spec.find_ranges(text, normalized)
                .into_iter()
                .map(|(start, end)| TermHit {
                    start,
                    end,
                    term_idx,
                }),
        );
    }
    all_hits.sort_unstable_by_key(|hit| hit.start);
    all_hits
}

fn normalize_highlight_query(query: &str) -> String {
    normalize_for_search(query)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Sort byte ranges, drop empty ones, and merge overlapping ones.
pub fn merge_match_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable_by_key(|range| range.0);
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in ranges {
        if start >= end {
            continue;
        }
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

/// Split `text` into consecutive `(piece, highlighted)` parts by byte
/// `ranges` (merged first; ranges past the end are ignored). This is
/// upstream's `highlight_ranges` without the terminal styles.
pub fn highlight_segments<'a>(text: &'a str, ranges: &[(usize, usize)]) -> Vec<(&'a str, bool)> {
    let merged = merge_match_ranges(ranges.to_vec())
        .into_iter()
        .filter(|(_, end)| *end <= text.len())
        .collect::<Vec<_>>();
    if merged.is_empty() {
        return vec![(text, false)];
    }

    let mut segments = Vec::new();
    let mut pos = 0;
    for (start, end) in merged {
        if start > pos {
            segments.push((&text[pos..start], false));
        }
        segments.push((&text[start..end], true));
        pos = end;
    }
    if pos < text.len() {
        segments.push((&text[pos..], false));
    }
    segments
}

// ---------------------------------------------------------------------------
// List rows
// ---------------------------------------------------------------------------

/// Text plus the byte ranges to highlight in it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HighlightedText {
    pub text: String,
    pub highlights: Vec<(usize, usize)>,
}

impl HighlightedText {
    /// `text` with the highlights of `query`.
    pub fn new(text: String, query: &HighlightQuery) -> Self {
        let highlights = query.highlight_ranges(&text);
        Self { text, highlights }
    }

    /// `text` without highlights.
    pub fn plain(text: String) -> Self {
        Self {
            text,
            highlights: Vec::new(),
        }
    }

    /// Plain/highlighted pieces (see [`highlight_segments`]).
    pub fn segments(&self) -> Vec<(&str, bool)> {
        highlight_segments(&self.text, &self.highlights)
    }
}

/// Where a row's preview line comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewSource {
    /// The semantic hit's evidence passage.
    SemanticEvidence,
    /// Snippets of the full text around hidden lexical matches.
    LexicalContext,
    /// The conversation preview (first or last messages).
    Preview,
}

/// Inputs of [`build_row_preview`].
#[derive(Clone, Copy, Debug)]
pub struct RowPreviewInput<'a> {
    pub conversation: &'a Conversation,
    /// The query, parsed once per frame.
    pub query: &'a HighlightQuery,
    /// Whether the list is in semantic mode.
    pub semantic_mode: bool,
    /// [`crate::browser::Browser::semantic_result_metadata`] of the row.
    pub semantic_metadata: Option<&'a SemanticResultMetadata>,
    /// [`crate::browser::Browser::lexical_evidence`] of the row.
    pub lexical_evidence: Option<&'a LexicalEvidence>,
    /// Budget of the preview and context lines (upstream: row width - 4).
    pub max_width: usize,
}

/// The preview line of a list row and its optional literal-context line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowPreview {
    pub preview: HighlightedText,
    pub source: PreviewSource,
    /// Snippets around literals that the preview does not show (upstream's
    /// third row line). Can also appear for unquoted `_` identifiers, whose
    /// queries [`crate::browser::query_has_literals`] does not count
    /// (upstream quirk 10); a GUI with variable row heights simply shows it.
    pub context: Option<HighlightedText>,
}

/// Decide a row's preview text, its truncation, and the context line
/// (upstream `render_list`, inventory 4.6):
///
/// 1. Text: in semantic mode with a query and metadata, the sanitized
///    semantic evidence; else snippets around the cached lexical evidence
///    (lexical evidence is ignored for semantic rows with metadata); else
///    the sanitized conversation preview.
/// 2. Truncation: no query → prefix; semantic mode with a visible match →
///    windows around the matches; semantic mode or lexical snippets →
///    prefix; else windows around normalized matches.
/// 3. Context line: unless the preview already is a lexical snippet, when
///    some literal is not visible in the truncated preview.
pub fn build_row_preview(input: &RowPreviewInput<'_>) -> RowPreview {
    let conv = input.conversation;
    let highlight_query = input.query;
    let query_normalized = highlight_query.context_text();
    let semantic_mode = input.semantic_mode;
    let semantic_metadata = input.semantic_metadata;
    let max_preview_len = input.max_width;

    let lexical_evidence = (!semantic_mode || semantic_metadata.is_none())
        .then_some(input.lexical_evidence)
        .flatten();
    let lexical_context = lexical_evidence.and_then(|evidence| {
        build_context_segments_from_ranges(
            &conv.full_text,
            &evidence.context_ranges,
            max_preview_len,
        )
    });
    let semantic_preview = semantic_metadata
        .filter(|_| semantic_mode && !query_normalized.is_empty())
        .map(|metadata| sanitize_preview(&metadata.explanation.evidence_preview));
    let (preview_text, source) = if let Some(preview) = semantic_preview {
        (preview, PreviewSource::SemanticEvidence)
    } else if let Some(context) = lexical_context.as_ref() {
        (context.clone(), PreviewSource::LexicalContext)
    } else {
        (sanitize_preview(&conv.preview), PreviewSource::Preview)
    };
    let truncated_preview = if query_normalized.is_empty() {
        simple_truncate(&preview_text, max_preview_len)
    } else if semantic_mode && highlight_query.has_match(&preview_text) {
        build_match_segments_for_query(&preview_text, highlight_query, max_preview_len)
    } else if semantic_mode || lexical_context.is_some() {
        simple_truncate(&preview_text, max_preview_len)
    } else {
        build_match_segments(&preview_text, &query_normalized, max_preview_len)
    };

    let allow_literal_context = lexical_context.is_none();
    let context =
        if allow_literal_context && highlight_query.needs_literal_context(&truncated_preview) {
            build_literal_context_segments(
                &conv.full_text,
                &truncated_preview,
                highlight_query,
                max_preview_len,
            )
            .map(|context_text| HighlightedText::new(context_text, highlight_query))
        } else {
            None
        };

    RowPreview {
        preview: HighlightedText::new(truncated_preview, highlight_query),
        source,
        context,
    }
}

/// Inputs of [`build_row_header`].
#[derive(Clone, Copy, Debug)]
pub struct RowHeaderInput<'a> {
    pub conversation: &'a Conversation,
    /// The query, parsed once per frame.
    pub query: &'a HighlightQuery,
    /// Row width in cells.
    pub width: usize,
    /// Prefix the project with the source label
    /// ([`crate::browser::Browser::has_multiple_sources`]).
    pub show_source_label: bool,
    /// Whether the list is in semantic mode.
    pub semantic_mode: bool,
    /// [`crate::browser::Browser::semantic_result_metadata`] of the row.
    pub semantic_metadata: Option<&'a SemanticResultMetadata>,
    /// The current time, computed once per frame.
    pub now: DateTime<Local>,
}

/// The parts of a list row's header line, already fitted to the width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowHeader {
    /// `project_name`, prefixed with `"CC  · "`, `"Pi  · "` or `"OMP · "`
    /// for mixed sources; truncated with `…`. Empty without a project name.
    pub project: HighlightedText,
    /// Custom title (shown after `" · "`), when there is room for it.
    pub custom_title: Option<HighlightedText>,
    /// Generated summary (shown after `" · "`), when there is room for it.
    pub summary: Option<HighlightedText>,
    /// `"1 msg"` / `"<n> msgs"`.
    pub message_count: String,
    /// Hybrid semantic score (semantic mode, rows at least 70 cells wide).
    pub semantic_score: Option<String>,
    /// Conversation duration, if known.
    pub duration: Option<String>,
    /// Relative timestamp and its recency bucket.
    pub timestamp: String,
    pub recency: Recency,
    /// Cells of padding between the left part and the right-aligned block
    /// in upstream's layout.
    pub padding: usize,
}

/// Compose a row header for `width` cells (upstream `render_list`,
/// inventory 4.3): the right block (message count, semantic score,
/// duration, timestamp, separated by `" · "`) is kept whole; the project
/// name is truncated first, rows narrower than 90 cells reserve
/// `clamp(left/3, 10, 24)` cells for the title/summary, the title is shown
/// when more than 4 cells remain and the summary when more than 5 remain.
pub fn build_row_header(input: &RowHeaderInput<'_>) -> RowHeader {
    const INDICATOR_WIDTH: usize = 3; // " ▌ "
    const MIN_PADDING: usize = 3;

    let conv = input.conversation;
    let width = input.width;
    let (timestamp, recency) = format_timestamp(conv.timestamp, input.now);
    let message_count = format_msg_count_short(conv.message_count);
    let duration = conv.duration_minutes.map(format_duration_minutes);

    let semantic_score = (input.semantic_mode && width >= 70)
        .then(|| input.semantic_metadata.map(semantic_row_metadata))
        .flatten();
    let semantic_meta_len = semantic_score
        .as_ref()
        .map(|s| UnicodeWidthStr::width(s.as_str()) + 3)
        .unwrap_or(0);
    let duration_len = duration
        .as_ref()
        .map(|d| UnicodeWidthStr::width(d.as_str()) + 3)
        .unwrap_or(0);
    let right_len = UnicodeWidthStr::width(message_count.as_str())
        + duration_len
        + semantic_meta_len
        + 3
        + UnicodeWidthStr::width(timestamp.as_str());
    let left_budget = width.saturating_sub(INDICATOR_WIDTH + right_len + MIN_PADDING);

    let raw_project_part = conv
        .project_name
        .as_ref()
        .map(|name| {
            if input.show_source_label {
                format!("{:<3} · {name}", conv.source.list_label())
            } else {
                name.to_string()
            }
        })
        .unwrap_or_default();
    let has_title_or_summary = conv.custom_title.as_ref().is_some_and(|s| !s.is_empty())
        || conv.summary.as_ref().is_some_and(|s| !s.is_empty());
    let raw_project_width = UnicodeWidthStr::width(raw_project_part.as_str());
    let reserved_left_detail = if width < 90 && has_title_or_summary {
        (left_budget / 3).clamp(10, 24)
    } else {
        0
    };
    let project_budget = raw_project_width.min(left_budget.saturating_sub(reserved_left_detail));
    let project_part = simple_truncate(&raw_project_part, project_budget);
    let project_len = UnicodeWidthStr::width(project_part.as_str());

    let title_budget = left_budget.saturating_sub(project_len + 3);
    let custom_title_part = conv
        .custom_title
        .as_ref()
        .filter(|s| !s.is_empty() && title_budget > 4)
        .map(|s| simple_truncate(s, title_budget));
    let custom_title_len = custom_title_part
        .as_ref()
        .map(|s| UnicodeWidthStr::width(s.as_str()) + 3)
        .unwrap_or(0);

    let available_for_summary = width.saturating_sub(
        INDICATOR_WIDTH + project_len + custom_title_len + right_len + MIN_PADDING + 4,
    );
    let summary_part = conv
        .summary
        .as_ref()
        .filter(|s| !s.is_empty() && available_for_summary > 5)
        .map(|s| simple_truncate(s, available_for_summary));
    let summary_len = summary_part
        .as_ref()
        .map(|s| UnicodeWidthStr::width(s.as_str()) + 3)
        .unwrap_or(0);

    let left_len = INDICATOR_WIDTH + project_len + custom_title_len + summary_len;
    let padding = width.saturating_sub(left_len + right_len + 1);

    RowHeader {
        project: HighlightedText::new(project_part, input.query),
        custom_title: custom_title_part.map(|text| HighlightedText::new(text, input.query)),
        summary: summary_part.map(|text| HighlightedText::new(text, input.query)),
        message_count,
        semantic_score,
        duration,
        timestamp,
        recency,
        padding,
    }
}

#[cfg(test)]
mod tests;
