//! Where a search result is in a conversation (upstream 411e227
//! `tui/app/list_preview.rs`, with the line-level refinements of 917758a
//! `tui/preview_worker.rs`).
//!
//! While the list has a query, the selected result is shown scrolled to its
//! hit: the line holding the semantic evidence the row shows, else the first
//! line matching the query ([`anchor`]), with the start of its message on
//! top when that keeps the hit in the upper two thirds
//! ([`PreviewDoc::scroll_offset`]). A [`PreviewDoc`] is a conversation as
//! lines of text with the [`MessageRange`]s of its messages: the ledger's
//! rendered lines ([`PreviewDoc::from_rendered`], what upstream's pane
//! shows), or any other split into lines, such as a GUI's own text.
//!
//! The query's matches are emphasized where the result was found
//! ([`match_ranges`], upstream 949ec4e): the matches of the whole query in
//! the hit message are the hit, every other match is secondary, and every
//! match widens to the whole words it touches ([`widen_to_words`]).
//!
//! # Differences from upstream
//!
//! * Only the UI-neutral rules live here. Upstream's `ListPreview`
//!   (411e227) also renders the selected conversation at the pane's width
//!   and caches it, and 917758a moves that onto a `tui-preview` worker with
//!   a document cache; a GUI renders, caches and lays out its own view, and
//!   whether there is a hit to show at all is `Browser::list_hit`.
//! * [`PreviewDoc`] holds any `AsRef<str>` lines, so a GUI can borrow its
//!   own text instead of copying it.
//! * The fork never had 411e227's per-line word sets on the UI thread: the
//!   evidence matcher is 917758a's (a bit per evidence word, at most
//!   [`EVIDENCE_WORDS`] words, one mask per line), which finds the same
//!   lines for evidence of up to 64 distinct words.
//! * [`match_ranges`] is upstream's `preview_match_ranges` (949ec4e,
//!   `tui/ui.rs`) with a [`MatchEmphasis`] where upstream returns a ratatui
//!   `Style` (filled block for the hit, colored text for the rest), so each
//!   UI draws the two its own way; it finds the hit message by
//!   [`PreviewDoc::message_at`] and classifies the hit message's lines once.

use std::collections::HashMap;
use std::ops::Range;

use super::ledger::{MessageRange, RenderedLine};
use crate::display_format::ViewSearchQuery;

/// A conversation as lines of text, for finding a search result in it.
#[derive(Clone, Debug)]
pub struct PreviewDoc<T = Box<str>> {
    /// Text of each line
    pub texts: Vec<T>,
    /// Lines of each message, in order
    pub messages: Vec<MessageRange>,
}

impl<T: AsRef<str>> PreviewDoc<T> {
    pub fn new(texts: Vec<T>, messages: Vec<MessageRange>) -> Self {
        Self { texts, messages }
    }

    /// First line to show in a pane `height` rows tall: the start of the
    /// message holding `anchor` when that keeps it in the upper two thirds,
    /// else `anchor` a third of the way down.
    pub fn scroll_offset(&self, anchor: Option<usize>, height: usize) -> usize {
        let Some(anchor) = anchor else {
            return 0;
        };
        let message_start = self
            .message_at(anchor)
            .map(|m| m.start_line)
            .filter(|&start| anchor - start < height * 2 / 3);
        let offset = message_start.unwrap_or_else(|| anchor.saturating_sub(height / 3));
        offset.min(self.texts.len().saturating_sub(height))
    }

    /// The message holding `line`, if any.
    pub fn message_at(&self, line: usize) -> Option<&MessageRange> {
        let idx = self.messages.partition_point(|m| m.end_line <= line);
        self.messages
            .get(idx)
            .filter(|m| m.start_line <= line && line < m.end_line)
    }
}

impl PreviewDoc {
    /// The ledger's rendered lines (what upstream's preview pane shows),
    /// each line's spans joined.
    pub fn from_rendered(lines: &[RenderedLine], messages: Vec<MessageRange>) -> Self {
        let texts = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<String>()
                    .into_boxed_str()
            })
            .collect();
        Self::new(texts, messages)
    }
}

/// Line the search result is at: the line `evidence` (the semantic evidence
/// shown in the row) points at, else the first line matching `query`. The
/// semantic evidence is what the row shows, so it wins over the first
/// literal match.
pub fn anchor<T: AsRef<str>>(
    doc: &PreviewDoc<T>,
    query: &str,
    evidence: Option<&str>,
) -> Option<usize> {
    evidence
        .and_then(|evidence| evidence_anchor(doc, evidence))
        .or_else(|| query_anchor(doc, query))
}

/// First line matching `query`.
fn query_anchor<T: AsRef<str>>(doc: &PreviewDoc<T>, query: &str) -> Option<usize> {
    let query = ViewSearchQuery::parse(query);
    if query.is_empty() {
        return None;
    }
    doc.texts
        .iter()
        .position(|text| query.has_match(text.as_ref()))
}

/// Most words compared from evidence; more add little to telling passages
/// apart.
pub const EVIDENCE_WORDS: usize = 64;

/// Line of the message sharing the most words with `evidence`, at the line
/// of it sharing the most; earliest on ties. `None` when no message shares
/// enough to be the passage the evidence was taken from.
fn evidence_anchor<T: AsRef<str>>(doc: &PreviewDoc<T>, evidence: &str) -> Option<usize> {
    let mut wanted: HashMap<String, u32> = HashMap::new();
    for word in distinctive_words(evidence) {
        let next = wanted.len() as u32;
        if next as usize == EVIDENCE_WORDS {
            break;
        }
        wanted.entry(word.to_lowercase()).or_insert(next);
    }
    if wanted.is_empty() {
        return None;
    }

    let mut lowered = String::new();
    let masks: Vec<u64> = doc
        .texts
        .iter()
        .map(|text| {
            distinctive_words(text.as_ref()).fold(0u64, |mask, word| {
                lowered.clear();
                lowered.extend(word.chars().flat_map(char::to_lowercase));
                wanted
                    .get(lowered.as_str())
                    .map_or(mask, |&bit| mask | 1 << bit)
            })
        })
        .collect();
    let shared = |range: std::ops::Range<usize>| {
        masks[range]
            .iter()
            .fold(0u64, |acc, mask| acc | mask)
            .count_ones() as usize
    };

    let (count, message) = doc
        .messages
        .iter()
        .map(|m| (shared(m.start_line..m.end_line), m))
        .max_by_key(|(count, m)| (*count, std::cmp::Reverse(m.start_line)))?;
    if count == 0 || count < wanted.len().min(3) {
        return None;
    }
    (message.start_line..message.end_line)
        .max_by_key(|&line| (masks[line].count_ones(), std::cmp::Reverse(line)))
}

/// Words of `text` long enough to tell passages apart.
fn distinctive_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().nth(3).is_some())
}

/// How a match of the query is drawn where the result was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MatchEmphasis {
    /// The hit: a match of the whole query in the message holding the
    /// anchor, or a match on the anchor line when that message has none
    /// (upstream: a filled block).
    Hit,
    /// Every other match (upstream: colored text, like matches in the list).
    Secondary,
}

/// The query's matches on each line of `lines`: byte ranges into the line,
/// sorted and disjoint, each with its [`MatchEmphasis`] (upstream 949ec4e
/// `preview_match_ranges`).
///
/// The hit, the message the result was found by (the one holding `anchor`),
/// has its matches of the whole query as the hit: or, when it has none, the
/// matches on the `anchor` line. Every other match is secondary. Matches
/// widen to whole words, so a prefix match on `tests` does not stop
/// mid-word.
pub fn match_ranges<T: AsRef<str>>(
    doc: &PreviewDoc<T>,
    lines: Range<usize>,
    anchor: Option<usize>,
    query: &ViewSearchQuery,
) -> Vec<Vec<(usize, usize, MatchEmphasis)>> {
    if query.is_empty() {
        return lines.map(|_| Vec::new()).collect();
    }
    let ranges_at = |idx: usize| {
        let text = doc.texts[idx].as_ref();
        widen_to_words(text, query.match_ranges(text))
    };
    let covers_query = |idx: usize, (start, end): (usize, usize)| {
        query.covers_all_terms(&doc.texts[idx].as_ref()[start..end])
    };
    let hit = anchor
        .and_then(|line| doc.message_at(line))
        .map_or(0..0, |m| m.start_line..m.end_line);
    let hit_ranges: Vec<Vec<(usize, usize)>> = hit.clone().map(ranges_at).collect();
    // Sorted: the hit's lines in order.
    let full_match_lines: Vec<usize> = hit
        .clone()
        .zip(&hit_ranges)
        .filter(|(idx, ranges)| ranges.iter().any(|&range| covers_query(*idx, range)))
        .map(|(idx, _)| idx)
        .collect();
    let is_hit = |idx: usize, range: (usize, usize)| {
        if full_match_lines.is_empty() {
            anchor == Some(idx)
        } else {
            full_match_lines.binary_search(&idx).is_ok() && covers_query(idx, range)
        }
    };

    lines
        .map(|idx| {
            let ranges = if hit.contains(&idx) {
                hit_ranges[idx - hit.start].clone()
            } else {
                ranges_at(idx)
            };
            ranges
                .into_iter()
                .map(|(start, end)| {
                    let emphasis = if is_hit(idx, (start, end)) {
                        MatchEmphasis::Hit
                    } else {
                        MatchEmphasis::Secondary
                    };
                    (start, end, emphasis)
                })
                .collect()
        })
        .collect()
}

/// Sorted `ranges` of `text` widened to the whole words they touch, merged
/// where they then meet.
pub fn widen_to_words(text: &str, ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    let is_word_char = |c: char| c.is_alphanumeric();
    let mut widened: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        let start = text[..start]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_word_char(c))
            .last()
            .map_or(start, |(i, _)| i);
        let end = text[end..]
            .char_indices()
            .find(|&(_, c)| !is_word_char(c))
            .map_or(text.len(), |(i, _)| end + i);
        match widened.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => widened.push((start, end)),
        }
    }
    widened
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(texts: &[&'static str], messages: &[(usize, usize)]) -> PreviewDoc<&'static str> {
        PreviewDoc::new(
            texts.to_vec(),
            messages
                .iter()
                .enumerate()
                .map(|(entry_index, &(start_line, end_line))| MessageRange {
                    entry_index,
                    start_line,
                    end_line,
                })
                .collect(),
        )
    }

    #[test]
    fn evidence_anchors_on_the_message_it_was_taken_from() {
        let doc = doc(
            &[
                "the cache is warm",
                "",
                "background daemon keeps",
                "semantic embeddings warm while idle",
                "",
                "unrelated reply",
            ],
            &[(0, 1), (2, 4), (5, 6)],
        );

        let anchor = evidence_anchor(&doc, "…a background daemon keeps semantic embeddings warm…");

        assert_eq!(anchor, Some(2));
    }

    #[test]
    fn evidence_matches_words_case_insensitively() {
        let doc = doc(&["intro", "Daemon Keeps Embeddings"], &[(0, 1), (1, 2)]);

        assert_eq!(evidence_anchor(&doc, "daemon keeps embeddings"), Some(1));
    }

    #[test]
    fn evidence_sharing_too_little_anchors_nowhere() {
        let doc = doc(&["the cache is warm", "unrelated reply"], &[(0, 1), (1, 2)]);

        assert_eq!(evidence_anchor(&doc, "daemon embeddings reconcile"), None);
    }

    #[test]
    fn query_anchors_on_the_first_matching_line() {
        let doc = doc(
            &["set up the tunnel", "rotate the token", "token again"],
            &[],
        );

        assert_eq!(query_anchor(&doc, "token"), Some(1));
        assert_eq!(query_anchor(&doc, "absent"), None);
    }

    #[test]
    fn evidence_wins_over_the_first_match_and_the_query_is_the_fallback() {
        let doc = doc(
            &[
                "the token was rotated",
                "background daemon keeps semantic embeddings",
            ],
            &[(0, 1), (1, 2)],
        );

        assert_eq!(
            anchor(&doc, "token", Some("daemon keeps semantic embeddings")),
            Some(1)
        );
        assert_eq!(anchor(&doc, "token", Some("unrelated words only")), Some(0));
        assert_eq!(anchor(&doc, "token", None), Some(0));
        assert_eq!(anchor(&doc, " ", None), None);
    }

    #[test]
    fn scroll_starts_at_the_message_when_the_match_stays_high() {
        let doc = doc(&["x"; 40], &[(0, 10), (10, 40)]);

        assert_eq!(doc.scroll_offset(Some(14), 12), 10);
        // Too deep into its message: the match goes a third of the way down.
        assert_eq!(doc.scroll_offset(Some(30), 12), 26);
        assert_eq!(doc.scroll_offset(None, 12), 0);
    }

    /// The matched text of each line and whether it is the hit (upstream
    /// 949ec4e `preview_styles`, comparing to `filled_match_style()`).
    fn emphases(
        lines: &[&'static str],
        anchor: Option<usize>,
        messages: &[(usize, usize)],
        query: &str,
    ) -> Vec<Vec<(String, bool)>> {
        let doc = doc(lines, messages);
        let query = ViewSearchQuery::parse(query);
        match_ranges(&doc, 0..doc.texts.len(), anchor, &query)
            .into_iter()
            .zip(&doc.texts)
            .map(|(ranges, text)| {
                ranges
                    .into_iter()
                    .map(|(start, end, emphasis)| {
                        (text[start..end].to_string(), emphasis == MatchEmphasis::Hit)
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn preview_fills_only_whole_query_matches_in_the_hit_message() {
        let styles = emphases(
            &[
                "failed on one flaky test: navigation",
                "3,125 tests passed",
                "Fix the flakyness and a flaky test",
            ],
            Some(1),
            &[(0, 2), (2, 3)],
            "flaky test",
        );
        assert_eq!(styles[0], vec![("flaky test".to_string(), true)]);
        assert_eq!(styles[1], vec![("tests".to_string(), false)]);
        assert_eq!(
            styles[2],
            vec![
                ("flakyness".to_string(), false),
                ("flaky test".to_string(), false)
            ]
        );
    }

    #[test]
    fn preview_fills_anchor_line_when_hit_lacks_whole_query_match() {
        let styles = emphases(
            &["a flaky run", "the test failed"],
            Some(1),
            &[(0, 2)],
            "flaky test",
        );
        assert_eq!(styles[0], vec![("flaky".to_string(), false)]);
        assert_eq!(styles[1], vec![("test".to_string(), true)]);
    }

    /// The fork classifies any window of lines, as the GUI does for a whole
    /// conversation and upstream for the visible rows: the same emphasis
    /// either way, and an empty query or no anchor has no hit.
    #[test]
    fn match_ranges_of_a_window_agree_with_the_whole_and_need_an_anchor() {
        let lines = [
            "failed on one flaky test: navigation",
            "3,125 tests passed",
            "Fix the flakyness and a flaky test",
        ];
        let doc = doc(&lines, &[(0, 2), (2, 3)]);
        let query = ViewSearchQuery::parse("flaky test");
        let whole = match_ranges(&doc, 0..3, Some(1), &query);
        assert_eq!(match_ranges(&doc, 1..3, Some(1), &query), whole[1..]);
        assert!(
            match_ranges(&doc, 0..3, None, &query)
                .iter()
                .flatten()
                .all(|&(.., emphasis)| emphasis == MatchEmphasis::Secondary)
        );
        let blank = ViewSearchQuery::parse("  ");
        assert_eq!(
            match_ranges(&doc, 0..3, Some(1), &blank),
            vec![Vec::new(); 3]
        );
    }

    #[test]
    fn widen_to_words_covers_whole_words() {
        let text = "src/tui/app_tests/nav and untested";
        let ranges = widen_to_words(
            text,
            [text.find("test").unwrap(), text.rfind("test").unwrap()]
                .into_iter()
                .map(|start| (start, start + 4))
                .collect(),
        );
        let words: Vec<_> = ranges.iter().map(|&(s, e)| &text[s..e]).collect();
        assert_eq!(words, vec!["tests", "untested"]);
    }

    /// Upstream `list_preview_anchors_on_the_first_match`, over the ledger's
    /// rendered lines at a 60-column pane.
    #[test]
    fn list_preview_anchors_on_the_first_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dialogue.jsonl");
        let turns = [
            ("user", "set up the tunnel"),
            ("assistant", "the tunnel is up"),
            ("user", "now rotate the token"),
            ("assistant", "rotated the token and restarted"),
        ];
        let lines: Vec<String> = turns
            .iter()
            .map(|(role, text)| {
                serde_json::json!({
                    "type": role,
                    "timestamp": "2024-01-01T00:00:00Z",
                    "message": {"role": role, "content": text}
                })
                .to_string()
            })
            .collect();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let history = crate::history::service::HistoryService::default();
        let transcript = crate::view::open_conversation(&history, &path).unwrap();
        let options = crate::view::ViewPrefs::default()
            .render_options(60, &std::collections::BTreeSet::new());
        let rendered = crate::view::render_conversation(&transcript, &options);
        let doc = PreviewDoc::from_rendered(&rendered.lines, rendered.messages);

        let anchor = anchor(&doc, "token", None).unwrap();

        assert!(
            doc.texts[anchor].contains("now rotate the token"),
            "{}",
            doc.texts[anchor]
        );
        assert!(doc.scroll_offset(Some(anchor), 20) <= anchor);
    }
}
