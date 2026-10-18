//! Tests ported from upstream `tui/ui.rs` (pure helpers), with the
//! rendering tests re-expressed through [`build_row_preview`] and
//! [`build_row_header`] instead of a terminal test backend.

use super::*;
use crate::semantic::types::{
    SemanticChunkIdentity, SemanticExplanation, SemanticQuality, SemanticScoreBreakdown,
};
use chrono::{Duration, TimeZone};
use std::path::PathBuf;

fn test_conversation() -> Conversation {
    Conversation {
        source: crate::history::Source::Claude,
        session_id: "session".to_owned(),
        path: PathBuf::from("/tmp/session.jsonl"),
        index: 0,
        timestamp: Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        preview: "lexical preview sentinel".to_string(),
        preview_first: "lexical preview sentinel".to_string(),
        preview_last: "lexical preview sentinel".to_string(),
        full_text: "tool output sentinel summary sentinel cwd sentinel".to_string(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns: vec!["semantic visible text".to_string()],
        semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
        search_text_lower: "lexical preview sentinel".to_string(),
        project_name: Some("project sentinel".to_string()),
        project_path: None,
        cwd: Some(PathBuf::from("/cwd/sentinel")),
        message_count: 1,
        parse_errors: Vec::new(),
        summary: Some("summary sentinel".to_string()),
        custom_title: Some("title sentinel".to_string()),
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn test_semantic_metadata(evidence_preview: &str) -> SemanticResultMetadata {
    test_semantic_metadata_with_scores(
        evidence_preview,
        SemanticScoreBreakdown {
            hybrid: 1.0,
            semantic: 1.0,
            lexical: 0.0,
        },
        SemanticRationaleKind::SemanticOnly,
    )
}

fn test_semantic_metadata_with_scores(
    evidence_preview: &str,
    score_breakdown: SemanticScoreBreakdown,
    rationale_kind: SemanticRationaleKind,
) -> SemanticResultMetadata {
    SemanticResultMetadata {
        score_breakdown,
        explanation: SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "strong",
            matched_terms: Vec::new(),
            evidence_preview: evidence_preview.to_string(),
            rationale_kind,
            chunk: SemanticChunkIdentity {
                conversation_index: 0,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: "test-session".to_string(),
                chunk_index: 0,
                message_range: crate::agent::refs::MessageRange::single(1),
            },
        },
    }
}

/// Highlighted pieces of `text` for `query_normalized` (upstream
/// `highlight_text` + `span_info`).
fn highlight_text<'a>(text: &'a str, query_normalized: &str) -> Vec<(&'a str, bool)> {
    highlight_segments(text, &find_normalized_match_ranges(text, query_normalized))
}

fn highlighted<'a>(segments: &[(&'a str, bool)]) -> Vec<&'a str> {
    segments
        .iter()
        .filter(|(_, highlighted)| *highlighted)
        .map(|(text, _)| *text)
        .collect()
}

fn query_highlighted<'a>(query: &HighlightQuery, text: &'a str) -> Vec<&'a str> {
    highlighted(&highlight_segments(text, &query.highlight_ranges(text)))
}

/// Row preview for a list `width` cells wide (upstream max = width - 4).
fn row_preview(
    conversation: &Conversation,
    query: &str,
    semantic_mode: bool,
    metadata: Option<&SemanticResultMetadata>,
    evidence: Option<&LexicalEvidence>,
    width: usize,
) -> RowPreview {
    let query = HighlightQuery::parse(query);
    build_row_preview(&RowPreviewInput {
        conversation,
        query: &query,
        semantic_mode,
        semantic_metadata: metadata,
        lexical_evidence: evidence,
        max_width: width.saturating_sub(4),
    })
}

fn row_header(
    conversation: &Conversation,
    width: usize,
    semantic_mode: bool,
    metadata: Option<&SemanticResultMetadata>,
) -> RowHeader {
    let query = HighlightQuery::parse("");
    build_row_header(&RowHeaderInput {
        conversation,
        query: &query,
        width,
        show_source_label: false,
        semantic_mode,
        semantic_metadata: metadata,
        now: Local::now(),
    })
}

/// The header as upstream drew it (indicator, parts, padding, right block).
fn header_line(header: &RowHeader) -> String {
    let mut line = format!(" ▌ {}", header.project.text);
    if let Some(title) = &header.custom_title {
        line.push_str(&format!(" · {}", title.text));
    }
    if let Some(summary) = &header.summary {
        line.push_str(&format!(" · {}", summary.text));
    }
    line.push_str(&" ".repeat(header.padding));
    line.push_str(&header.message_count);
    for part in [&header.semantic_score, &header.duration]
        .into_iter()
        .flatten()
    {
        line.push_str(&format!(" · {part}"));
    }
    line.push_str(&format!(" · {}", header.timestamp));
    line
}

// --- viewer header values ---

#[test]
fn test_format_model_name_opus_45() {
    assert_eq!(format_model_name("claude-opus-4-5-20251101"), "opus-4.5");
}

#[test]
fn test_format_model_name_sonnet_4() {
    assert_eq!(format_model_name("claude-sonnet-4-20250514"), "sonnet-4");
}

#[test]
fn test_format_model_name_sonnet_35() {
    assert_eq!(
        format_model_name("claude-3-5-sonnet-20241022"),
        "sonnet-3.5"
    );
}

#[test]
fn test_format_model_name_haiku_35() {
    assert_eq!(format_model_name("claude-3-5-haiku-20241022"), "haiku-3.5");
}

#[test]
fn test_format_model_name_opus_3() {
    assert_eq!(format_model_name("claude-3-opus-20240229"), "opus-3");
}

#[test]
fn test_format_model_name_other_dated_models() {
    assert_eq!(format_model_name("claude-3-sonnet-20240229"), "sonnet-3");
    assert_eq!(format_model_name("claude-3-haiku-20240307"), "haiku-3");
    // A non-numeric suffix is not a dated model.
    assert_eq!(format_model_name("claude-sonnet-4-x"), "claude-sonnet-4-x");
}

#[test]
fn test_format_model_name_unknown() {
    assert_eq!(format_model_name("custom-model"), "custom-model");
}

#[test]
fn test_format_model_name_truncates_long() {
    let long_name = "very-long-unknown-model-name-that-exceeds-limit";
    let formatted = format_model_name(long_name);
    // 19 chars + ellipsis (3 bytes in UTF-8)
    assert!(formatted.chars().count() <= 20);
    assert!(formatted.ends_with('…'));
    assert_eq!(formatted, "very-long-unknown-m…");
}

#[test]
fn format_model_name_does_not_panic_on_non_ascii() {
    // Upstream sliced 19 bytes and panicked inside a multi-byte character.
    let name = "модель-с-очень-длинным-именем";
    let formatted = format_model_name(name);
    assert_eq!(formatted.chars().count(), 20);
    assert!(formatted.ends_with('…'));
    assert_eq!(format_model_name("короткая"), "короткая");
}

#[test]
fn test_format_tokens_small() {
    assert_eq!(format_tokens(500), "500");
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(999), "999");
}

#[test]
fn test_format_tokens_thousands() {
    assert_eq!(format_tokens(1000), "1k");
    assert_eq!(format_tokens(417000), "417k");
    assert_eq!(format_tokens(999999), "999k");
}

#[test]
fn test_format_tokens_millions() {
    assert_eq!(format_tokens(1_000_000), "1.0M");
    assert_eq!(format_tokens(1_500_000), "1.5M");
    assert_eq!(format_tokens(12_345_678), "12.3M");
}

#[test]
fn test_format_tokens_long() {
    assert_eq!(format_tokens_long(500), "500 tokens");
    assert_eq!(format_tokens_long(1000), "1k tokens");
    assert_eq!(format_tokens_long(926000), "926k tokens");
    assert_eq!(format_tokens_long(1_500_000), "1.5M tokens");
}

#[test]
fn counts_durations_and_header_timestamp() {
    assert_eq!(format_msg_count_short(1), "1 msg");
    assert_eq!(format_msg_count_short(0), "0 msgs");
    assert_eq!(format_msg_count_short(12), "12 msgs");
    assert_eq!(format_msg_count_long(1), "1 message");
    assert_eq!(format_msg_count_long(3), "3 messages");
    assert_eq!(format_duration_minutes(0), "0m");
    assert_eq!(format_duration_minutes(59), "59m");
    assert_eq!(format_duration_minutes(60), "1h 0m");
    assert_eq!(format_duration_minutes(135), "2h 15m");
    let timestamp = Local.with_ymd_and_hms(2024, 3, 9, 7, 5, 0).unwrap();
    assert_eq!(format_header_timestamp(timestamp), "2024-03-09 07:05");
}

#[test]
fn header_fits_single_line_measures_every_part() {
    let mut conversation = test_conversation();
    conversation.summary = None;
    assert!(header_fits_single_line(&conversation, 1));

    let mut conversation = test_conversation();
    conversation.model = Some("claude-opus-4-5-20251101".to_string());
    conversation.total_tokens = 926_000;
    conversation.duration_minutes = Some(75);
    // "  project sentinel · " + "title sentinel · " + "opus-4.5 · " + "1 message"
    // + " · 1h 15m" + " · " + "926k tokens · " + "YYYY-MM-DD HH:MM" + " · " + summary
    let expected = 2 + 16 + 3 + 17 + 11 + 9 + 9 + 3 + 14 + 16 + 3 + 16;
    assert!(header_fits_single_line(&conversation, expected as u16));
    assert!(!header_fits_single_line(&conversation, expected as u16 - 1));
}

#[test]
fn content_width_keeps_timing_off_behavior() {
    assert_eq!(calculate_content_width(60, false), 46);
}

#[test]
fn content_width_reserves_timestamp_prefix_when_timing_is_on() {
    assert_eq!(calculate_content_width(60, true), 32);
    assert_eq!(calculate_content_width(5, true), 0);
}

// --- relative timestamps ---

#[test]
fn relative_timestamps_use_recency_buckets() {
    let now = Local.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
    let cases = [
        (now - Duration::seconds(30), "just now", Recency::Now),
        (now - Duration::minutes(1), "1 min ago", Recency::Minutes),
        (now - Duration::minutes(59), "59 min ago", Recency::Minutes),
        (now - Duration::hours(1), "1 hour ago", Recency::Hours),
        (now - Duration::hours(23), "23 hours ago", Recency::Hours),
        (now - Duration::hours(24), "yesterday", Recency::Days),
        (now - Duration::days(3), "3 days ago", Recency::Days),
        (now - Duration::days(6), "6 days ago", Recency::Days),
        (now - Duration::days(7), "May 03, 12:00", Recency::Old),
        (now + Duration::minutes(5), "May 10, 12:05", Recency::Old),
    ];
    for (timestamp, text, recency) in cases {
        assert_eq!(
            format_timestamp(timestamp, now),
            (text.to_string(), recency),
            "{timestamp}"
        );
    }
    assert_eq!(
        format_relative_timestamp(now - Duration::seconds(1), now).0,
        "just now"
    );
}

#[test]
fn yesterday_follows_the_calendar_day() {
    let now = Local.with_ymd_and_hms(2026, 5, 10, 1, 0, 0).unwrap();
    // 25.5 hours ago is two calendar days back.
    let (text, recency) = format_timestamp(now - Duration::minutes(25 * 60 + 30), now);
    assert_eq!(text, "2 days ago");
    assert_eq!(recency, Recency::Days);
    // 24 hours ago is the previous calendar day.
    let (text, _) = format_timestamp(now - Duration::hours(24), now);
    assert_eq!(text, "yesterday");
}

// --- semantic labels ---

#[test]
fn semantic_labels_match_upstream() {
    let metadata = test_semantic_metadata_with_scores(
        "semantic evidence only",
        SemanticScoreBreakdown {
            hybrid: 1.23,
            semantic: 0.9,
            lexical: 0.1,
        },
        SemanticRationaleKind::LexicalBoosted,
    );
    assert_eq!(semantic_row_metadata(&metadata), "1.23");
    assert_eq!(semantic_rationale_label(&metadata), "lex boost");
    assert_eq!(
        semantic_rationale_label(&test_semantic_metadata("x")),
        "semantic"
    );
    let weak = test_semantic_metadata_with_scores(
        "x",
        SemanticScoreBreakdown {
            hybrid: 0.1,
            semantic: 0.1,
            lexical: 0.0,
        },
        SemanticRationaleKind::WeakMatch,
    );
    assert_eq!(semantic_rationale_label(&weak), "weak");
}

// --- truncation ---

#[test]
fn sanitize_preview_strips_tags_and_collapses_whitespace() {
    assert_eq!(
        sanitize_preview("  <tag attr=\"x\">hello</tag>\n\n\tworld   again  "),
        "hello world again"
    );
    assert_eq!(sanitize_preview("a < b > c"), "a c");
    assert_eq!(sanitize_preview(""), "");
}

#[test]
fn simple_truncate_respects_display_width() {
    assert_eq!(simple_truncate("hello", 5), "hello");
    assert_eq!(simple_truncate("hello world", 6), "hello…");
    assert_eq!(simple_truncate("hello", 0), "");
    // CJK characters are two cells wide.
    assert_eq!(simple_truncate("你好世界", 5), "你好…");
    assert!(UnicodeWidthStr::width(simple_truncate("你好世界", 5).as_str()) <= 5);
}

#[test]
fn truncate_start_keeps_the_end() {
    assert_eq!(truncate_start("hello world", 20), "hello world");
    assert_eq!(truncate_start("hello world", 6), "…world");
    assert_eq!(truncate_start("hello", 0), "");
}

#[test]
fn truncate_around_match_centers_the_first_match() {
    let text = "aaaa bbbb cccc needle dddd eeee ffff";
    let result = truncate_around_match(text, "needle", 16);
    assert!(result.contains("needle"), "{result}");
    assert!(result.starts_with('…') && result.ends_with('…'), "{result}");
    assert!(UnicodeWidthStr::width(result.as_str()) <= 16, "{result}");
    assert_eq!(truncate_around_match(text, "zzz", 8), "aaaa bb…");
    assert_eq!(truncate_around_match(text, "needle", 4), "nee…");
    assert_eq!(truncate_around_match(text, "needle", 0), "");
}

// --- highlight query ---

#[test]
fn highlight_query_uses_unquoted_terms_and_literals_for_context() {
    assert_eq!(
        HighlightQuery::parse("alpha \"DEPLOYMENT_TOKEN\" beta").context_text(),
        "alpha beta deployment token"
    );
    assert!(HighlightQuery::parse("").is_empty());
    assert!(HighlightQuery::parse("\"\"").is_empty());
    let query = HighlightQuery::parse("Alpha snake_case \"Lit\"");
    assert_eq!(query.unquoted(), "alpha");
    assert_eq!(
        query
            .literals()
            .iter()
            .map(|literal| literal.text())
            .collect::<Vec<_>>(),
        vec!["Lit", "snake_case"]
    );
}

#[test]
fn highlight_query_preserves_unquoted_identifier_punctuation() {
    let query = HighlightQuery::parse("deployment_token");
    assert_eq!(
        query_highlighted(&query, "prefix deployment token suffix DEPLOYMENT_TOKEN"),
        vec!["DEPLOYMENT_TOKEN"]
    );
}

#[test]
fn quoted_list_highlighting_matches_literal_text() {
    let query = HighlightQuery::parse("\"DEPLOYMENT_TOKEN\"");
    assert_eq!(
        query_highlighted(&query, "prefix DEPLOYMENT_TOKEN suffix"),
        vec!["DEPLOYMENT_TOKEN"]
    );
}

#[test]
fn quoted_list_highlighting_matches_multiword_literal_phrase() {
    let query = HighlightQuery::parse("alpha \"beta gamma\"");
    assert_eq!(
        query_highlighted(&query, "alpha prefix beta gamma suffix beta-only"),
        vec!["alpha", "beta gamma"]
    );
}

#[test]
fn quoted_list_highlighting_respects_smart_case() {
    let query = HighlightQuery::parse("\"Beta Gamma\"");
    assert_eq!(
        query_highlighted(&query, "beta gamma then Beta Gamma"),
        vec!["Beta Gamma"]
    );
}

#[test]
fn highlight_query_match_helpers() {
    let query = HighlightQuery::parse("alpha \"Exact\"");
    assert!(query.has_match("ALPHA"));
    assert!(query.has_match("an Exact one"));
    assert!(!query.has_match("an exact one"));
    assert!(query.has_literal_match("Exact"));
    assert!(query.needs_literal_context("alpha only"));
    assert!(!query.needs_literal_context("alpha Exact"));
    assert_eq!(query.match_ranges("Exact alpha"), vec![(0, 5), (6, 11)]);
    assert_eq!(query.context_specs().len(), 2);
    assert_eq!(query.literal_context_specs().len(), 1);
}

#[test]
fn semantic_evidence_preview_highlights_query_terms() {
    let metadata = test_semantic_metadata(
        "prefix text before the important semantic needle appears near the end",
    );
    let segments = build_match_segments(&metadata.explanation.evidence_preview, "needle", 40);
    let pieces = highlight_text(&segments, "needle");
    assert_eq!(highlighted(&pieces), vec!["needle"]);
}

#[test]
fn highlight_segments_split_text_by_ranges() {
    let text = "foo bar baz";
    assert_eq!(
        highlight_segments(text, &[(4, 7), (0, 3)]),
        vec![("foo", true), (" ", false), ("bar", true), (" baz", false)]
    );
    assert_eq!(highlight_segments(text, &[]), vec![(text, false)]);
    // Ranges past the end are ignored, overlapping ones merged.
    assert_eq!(highlight_segments(text, &[(8, 40)]), vec![(text, false)]);
    assert_eq!(
        highlight_segments(text, &[(0, 5), (2, 7)]),
        vec![("foo bar", true), (" baz", false)]
    );
}

#[test]
fn merge_match_ranges_sorts_merges_and_drops_empty_ranges() {
    assert_eq!(
        merge_match_ranges(vec![(5, 8), (0, 2), (1, 3), (4, 4), (8, 9)]),
        vec![(0, 3), (5, 9)]
    );
    assert!(merge_match_ranges(Vec::new()).is_empty());
}

#[test]
fn dedupe_context_specs_is_case_insensitive_per_kind_and_capped() {
    let specs = vec![
        ContextMatchSpec::Normalized("Alpha".to_string()),
        ContextMatchSpec::Normalized("alpha".to_string()),
        ContextMatchSpec::Literal(Literal::new("alpha".to_string())),
        ContextMatchSpec::Literal(Literal::new("ALPHA".to_string())),
    ];
    let deduped = dedupe_context_specs(&specs);
    assert_eq!(
        deduped
            .iter()
            .map(ContextMatchSpec::key)
            .collect::<Vec<_>>(),
        vec![("Alpha", false), ("alpha", true)]
    );
    let many = (0..100)
        .map(|index| ContextMatchSpec::Normalized(format!("term{index}")))
        .collect::<Vec<_>>();
    assert_eq!(dedupe_context_specs(&many).len(), 64);
}

#[test]
fn find_all_spec_hits_are_sorted_by_position() {
    let text = "beta alpha beta";
    let specs = vec![
        ContextMatchSpec::Normalized("alpha".to_string()),
        ContextMatchSpec::Literal(Literal::new("beta".to_string())),
    ];
    let hits = find_all_spec_hits(text, &specs, None);
    assert_eq!(
        hits.iter()
            .map(|hit| (hit.start, hit.term_idx))
            .collect::<Vec<_>>(),
        vec![(0, 1), (5, 0), (11, 1)]
    );
    let normalized = NormalizedText::new(text);
    assert_eq!(find_all_spec_hits(text, &specs, Some(&normalized)), hits);
}

// --- highlight_text / find_normalized_match_ranges tests ---

#[test]
fn highlight_word_boundary_prefix() {
    // "red" matches at start of "redaction" (prefix), but not mid-word
    let pieces = highlight_text("Extend log redaction to cover", "red team");
    assert_eq!(highlighted(&pieces), vec!["red"]);
}

#[test]
fn highlight_phrase_exact_match() {
    let pieces = highlight_text(
        "You are being tested as a security red team exercise.",
        "red team",
    );
    // Adjacent words separated by space merge into one highlight span
    assert_eq!(highlighted(&pieces), vec!["red team"]);
}

#[test]
fn highlight_multiple_matches() {
    let pieces = highlight_text("foo bar foo bar foo", "foo");
    assert_eq!(highlighted(&pieces), vec!["foo", "foo", "foo"]);
}

#[test]
fn highlight_underscore_normalization() {
    // Query "red team" matches "red_team" as one span including the underscore
    let pieces = highlight_text("config for red_team setup", "red team");
    assert_eq!(highlighted(&pieces), vec!["red_team"]);
}

#[test]
fn highlight_case_insensitive() {
    let pieces = highlight_text("Hello World", "hello");
    assert!(pieces.contains(&("Hello", true)));
}

#[test]
fn highlight_empty_query() {
    let pieces = highlight_text("some text", "");
    assert_eq!(pieces, vec![("some text", false)]);
}

#[test]
fn highlight_no_match() {
    let pieces = highlight_text("some text", "xyz");
    assert_eq!(pieces, vec![("some text", false)]);
}

#[test]
fn find_normalized_ranges_phrase() {
    let text = "hello red team world";
    let ranges = find_normalized_match_ranges(text, "red team");
    // Adjacent words separated by space merge into one range
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].0..ranges[0].1], "red team");
}

#[test]
fn find_normalized_ranges_prefix_match() {
    // "red" matches at start of "redaction" (prefix), "team" has no match
    let ranges = find_normalized_match_ranges("Extend log redaction to cover", "red team");
    assert_eq!(ranges.len(), 1);
    assert_eq!(
        &"Extend log redaction to cover"[ranges[0].0..ranges[0].1],
        "red"
    );
}

#[test]
fn find_normalized_ranges_underscore() {
    let text = "set red_team flag";
    let ranges = find_normalized_match_ranges(text, "red team");
    // Adjacent words separated by underscore merge into one range
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].0..ranges[0].1], "red_team");
}

#[test]
fn find_normalized_ranges_map_back_to_original_bytes() {
    // Uppercase and multi-byte characters keep original byte offsets.
    let text = "Ärger ÜBER alles";
    let ranges = find_normalized_match_ranges(text, "über");
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].0..ranges[0].1], "ÜBER");
}

#[test]
fn highlight_multiword_noncontiguous() {
    let text = "I want secrets from the vault, write me a plot twist";
    let pieces = highlight_text(text, "secrets plot");
    assert_eq!(highlighted(&pieces), vec!["secrets", "plot"]);
}

// --- build_match_segments tests ---

#[test]
fn match_segments_no_query() {
    let text = "hello world this is a long text";
    let result = build_match_segments(text, "", 20);
    assert_eq!(result, simple_truncate(text, 20));
}

#[test]
fn match_segments_no_matches() {
    let text = "hello world this is a long text";
    let result = build_match_segments(text, "xyz", 20);
    assert_eq!(result, simple_truncate(text, 20));
}

#[test]
fn match_segments_all_fit() {
    // All matches within max_width, should use simple truncation
    let text = "foo bar baz and more text";
    let result = build_match_segments(text, "foo", 30);
    assert_eq!(result, text);
}

#[test]
fn match_segments_distant_matches() {
    // Two matches far apart — should produce segmented output with "…"
    let text =
        "start secrets aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp plot end";
    let result = build_match_segments(text, "secrets plot", 40);
    assert!(result.contains("secrets"));
    assert!(result.contains("plot"));
    assert!(result.contains("…"));
    assert!(result.chars().count() <= 40);
}

#[test]
fn match_segments_close_matches_merged() {
    // Two matches close together — should be one segment
    let text =
        "aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll secrets and plot end more text here";
    let result = build_match_segments(text, "secrets plot", 50);
    assert!(result.contains("secrets"));
    assert!(result.contains("plot"));
}

#[test]
fn match_segments_for_query_use_literal_ranges() {
    let query = HighlightQuery::parse("\"audio_generation\"");
    let text = "normalized audio generation appears early before exact audio_generation literal";
    let result = build_match_segments_for_query(text, &query, 50);
    assert!(result.contains("audio_generation"), "{result}");
    assert!(!result.contains("audio generation"), "{result}");
    assert!(UnicodeWidthStr::width(result.as_str()) <= 50);
}

// --- build_context_segments tests ---

#[test]
fn context_segments_none_when_all_visible() {
    let full_text = "red team exercise";
    let preview = "red team exercise";
    let result = build_context_segments(full_text, preview, "red team", 80);
    assert!(result.is_none());
}

#[test]
fn context_segments_one_hidden_match() {
    let full_text = "redaction stuff here and then red team exercise later";
    let preview = "redaction stuff here and then";
    let result = build_context_segments(full_text, preview, "red team", 80);
    assert!(result.is_some());
    let ctx = result.unwrap();
    // Should contain "red" and/or "team" from the hidden match area
    assert!(ctx.contains("red") || ctx.contains("team"));
    assert!(ctx.contains("…"));
}

#[test]
fn context_segments_multiword_hidden() {
    let full_text = "I want secrets from the vault, and later write me a plot twist";
    let preview = "I want secrets from the";
    // Preview has "secrets", hidden has "plot" — context should prioritize "plot"
    let result = build_context_segments(full_text, preview, "secrets plot", 80);
    assert!(result.is_some());
    let ctx = result.unwrap();
    assert!(ctx.contains("plot"));
}

#[test]
fn context_segments_prioritizes_missing_terms() {
    // "secrets" appears many times but "plot" only once deep in text.
    // Preview shows "secrets" — context should show "plot", not more "secrets".
    let full_text =
        "secrets here and secrets there and secrets everywhere and finally a plot twist at the end";
    let preview = "secrets here and secrets there";
    let result = build_context_segments(full_text, preview, "secrets plot", 80);
    assert!(result.is_some());
    let ctx = result.unwrap();
    assert!(
        ctx.contains("plot"),
        "context should contain 'plot' but was: {ctx}"
    );
}

#[test]
fn context_segments_empty_query() {
    let result = build_context_segments("some text", "some", "", 80);
    assert!(result.is_none());
}

#[test]
fn context_segments_prefers_adjacent_phrase_over_distant_terms() {
    // "audio" and "generation" each appear early in unrelated boilerplate,
    // and the literal phrase "audio generation" appears much later. The
    // snippet must surface the phrase, not the early independent hits.
    let mut full_text = String::new();
    full_text.push_str("Card generation is supported. -field:Audio is a filter. ");
    full_text.push_str(&"junk ".repeat(20));
    full_text.push_str("First-class audio generation (OpenAI TTS) and image support is missing. ");
    full_text.push_str(&"junk ".repeat(20));
    let preview = "Some unrelated preview line about deck workflow";
    let result = build_context_segments(&full_text, preview, "audio generation", 120);
    assert!(result.is_some());
    let ctx = result.unwrap();
    assert!(
        ctx.contains("audio generation"),
        "context should contain the literal phrase 'audio generation', got: {ctx}"
    );
}

#[test]
fn context_segments_phrase_inside_markdown_bold() {
    // The phrase appears inside `**...**` (markdown bold). Adjacency must
    // still be detected — markdown punctuation is a word boundary.
    let mut full_text = String::new();
    full_text.push_str("audio is mentioned. generation is mentioned separately. ");
    full_text.push_str(&"x ".repeat(30));
    full_text.push_str("**Audio generation** is the actual recommendation here.");
    let preview = "boring preview text";
    let result = build_context_segments(&full_text, preview, "audio generation", 120);
    assert!(result.is_some());
    let ctx = result.unwrap();
    let lower = ctx.to_lowercase();
    assert!(
        lower.contains("audio generation"),
        "context should contain the phrase 'audio generation', got: {ctx}"
    );
}

#[test]
fn context_segments_skips_clusters_only_covering_visible_terms() {
    // "audio" is in the preview already. A standalone "audio" cluster
    // should not be selected — the snippet should surface "generation"
    // (the missing term), preferably alongside "audio" if a phrase exists.
    let full_text = "audio first. then later generation alone. and finally audio generation together at the end.";
    let preview = "audio first. then later";
    let result = build_context_segments(full_text, preview, "audio generation", 120);
    assert!(result.is_some());
    let ctx = result.unwrap();
    assert!(
        ctx.contains("generation"),
        "context should contain 'generation', got: {ctx}"
    );
}

#[test]
fn context_segments_distant_terms_dont_count_as_adjacent() {
    // Two clusters with the same unique_count, but only one has the
    // terms actually adjacent. The adjacent one must win.
    let mut full_text = String::new();
    // Cluster A: alpha and beta with junk between (within merge_gap=50
    // bytes but not adjacent — `aaaa` is alphanumeric in the gap).
    full_text.push_str("alpha aaaa bbbb cccc dddd beta ");
    full_text.push_str(&"x ".repeat(40));
    // Cluster B: literal phrase
    full_text.push_str("alpha beta together here");
    let preview = "boring preview line";
    let result = build_context_segments(&full_text, preview, "alpha beta", 100);
    assert!(result.is_some());
    let ctx = result.unwrap();
    assert!(
        ctx.contains("alpha beta together")
            || ctx.ends_with("here")
            || ctx.contains("beta together"),
        "expected the literal phrase cluster to be selected, got: {ctx}"
    );
}

#[test]
fn context_segments_dedupes_query_terms() {
    // Repeated query terms must not inflate uniqueness/adjacency math.
    let full_text = "alpha alpha and later beta the end";
    let preview = "preview only";
    let r1 = build_context_segments(full_text, preview, "alpha alpha beta", 80);
    let r2 = build_context_segments(full_text, preview, "alpha beta", 80);
    // Should produce the same context — duplicates are folded away.
    assert_eq!(r1, r2);
}

#[test]
fn context_segments_underscore_phrase_still_detected() {
    // Underscores normalize to spaces, so `audio_generation` should be
    // detected as the adjacent phrase `audio generation`.
    let mut full_text = String::new();
    full_text.push_str("audio early. generation early. ");
    full_text.push_str(&"x ".repeat(30));
    full_text.push_str("the relevant audio_generation pipeline lives here.");
    let preview = "preview only";
    let result = build_context_segments(&full_text, preview, "audio generation", 120);
    assert!(result.is_some());
    let ctx = result.unwrap();
    assert!(
        ctx.contains("audio_generation") || ctx.contains("audio generation"),
        "context should contain the adjacent occurrence, got: {ctx}"
    );
}

#[test]
fn context_query_quoted_literal_preserves_punctuation_contract() {
    let query = HighlightQuery::parse("\"audio_generation\"");
    let mut full_text = String::new();
    full_text.push_str("punctuation-stripped audio generation appears early. ");
    full_text.push_str(&"x ".repeat(30));
    full_text.push_str("the exact audio_generation literal appears here.");
    let preview = "preview only";

    let result = build_context_segments_for_query(&full_text, preview, &query, 120);

    assert!(result.is_some());
    let ctx = result.unwrap();
    assert!(ctx.contains("audio_generation"), "got: {ctx}");
    assert!(
        !ctx.contains("audio generation appears early"),
        "got: {ctx}"
    );
}

#[test]
fn context_segments_from_ranges_require_ranges_and_width() {
    assert_eq!(build_context_segments_from_ranges("abc", &[], 10), None);
    assert_eq!(
        build_context_segments_from_ranges("abc", &[(0, 1)], 0),
        None
    );
    let full_text = format!("{} needle {}", "a ".repeat(50), "b ".repeat(50));
    let start = full_text.find("needle").unwrap();
    let ctx = build_context_segments_from_ranges(&full_text, &[(start, start + 6)], 30).unwrap();
    assert!(ctx.contains("needle"), "{ctx}");
    assert!(ctx.starts_with('…') && ctx.ends_with('…'), "{ctx}");
    assert!(UnicodeWidthStr::width(ctx.as_str()) <= 30, "{ctx}");
}

#[test]
fn lexical_mixed_query_cached_context_uses_unquoted_and_literals() {
    let mut conversation = test_conversation();
    conversation.preview = "visible preview".to_string();
    conversation.full_text = format!("hidden_unquoted {} exact_literal", "x ".repeat(120));
    let evidence = crate::search::build_lexical_evidence(
        &conversation,
        &ParsedQuery::parse("hidden_unquoted \"exact_literal\""),
    )
    .unwrap();
    let ctx =
        build_context_segments_from_ranges(&conversation.full_text, &evidence.context_ranges, 120)
            .unwrap();

    assert!(ctx.contains("exact_literal"), "{ctx:?}");
    assert!(ctx.contains("hidden_unquoted"), "{ctx:?}");
}

// --- word boundary tests ---

#[test]
fn word_boundary_rejects_mid_word() {
    // "red" should not match inside "fired" (not at word start)
    let ranges = find_normalized_match_ranges("fired and tired", "red");
    assert_eq!(ranges.len(), 0);
}

#[test]
fn word_boundary_allows_prefix() {
    // "red" matches at start of "redaction" (prefix matching)
    let ranges = find_normalized_match_ranges("redaction plan", "red");
    assert_eq!(ranges.len(), 1);
    assert_eq!(&"redaction plan"[ranges[0].0..ranges[0].1], "red");
}

#[test]
fn word_boundary_accepts_whole_word() {
    let ranges = find_normalized_match_ranges("the red fox", "red");
    assert_eq!(ranges.len(), 1);
    assert_eq!(&"the red fox"[ranges[0].0..ranges[0].1], "red");
}

#[test]
fn word_boundary_accepts_punctuation_adjacent() {
    // "red" after punctuation should match
    let ranges = find_normalized_match_ranges("it was (red) not blue", "red");
    assert_eq!(ranges.len(), 1);
}

#[test]
fn word_boundary_start_end_of_string() {
    let ranges = find_normalized_match_ranges("red", "red");
    assert_eq!(ranges.len(), 1);
    let ranges = find_normalized_match_ranges("red fox", "red");
    assert_eq!(ranges.len(), 1);
    let ranges = find_normalized_match_ranges("the red", "red");
    assert_eq!(ranges.len(), 1);
}

// --- row header (upstream render_list tests) ---

#[test]
fn list_truncates_long_project_names_on_narrow_rows() {
    let mut conversation = test_conversation();
    conversation.project_name = Some("claude-history/drop-semantic-feature-gate".to_string());
    conversation.custom_title = Some("semantic status title".to_string());
    conversation.summary = Some("semantic status summary".to_string());
    let header = row_header(&conversation, 70, false, None);
    assert_eq!(header.project.text, "claude-history/drop-semantic…");
    let line = header_line(&header);
    assert!(
        !line.contains("claude-history/drop-semantic-feature-gate"),
        "{line:?}"
    );
}

#[test]
fn list_uses_available_width_for_custom_titles() {
    let mut conversation = test_conversation();
    conversation.project_name = Some("aven".to_string());
    conversation.custom_title = Some(
        "fork lineage alpha beta gamma delta epsilon zeta eta theta iota kappa lambda".to_string(),
    );
    conversation.summary = Some("generated summary remains visible".to_string());
    let header = row_header(&conversation, 160, false, None);
    let line = header_line(&header);
    assert!(
        line.contains(
            "fork lineage alpha beta gamma delta epsilon zeta eta theta iota kappa lambda"
        ),
        "{line:?}"
    );
    assert!(
        line.contains("generated summary remains visible"),
        "{line:?}"
    );
    assert!(line.contains("1 msg · Jan 01, 00:00"), "{line:?}");
    assert_eq!(header.recency, Recency::Old);
}

#[test]
fn list_truncates_custom_titles_to_preserve_metadata() {
    let mut conversation = test_conversation();
    conversation.project_name = Some("aven".to_string());
    conversation.custom_title = Some(
        "fork lineage alpha beta gamma delta epsilon zeta eta theta iota kappa lambda".to_string(),
    );
    conversation.summary = None;
    let header = row_header(&conversation, 72, false, None);
    let line = header_line(&header);
    assert!(
        line.contains("fork lineage alpha beta gamma delta e…"),
        "{line:?}"
    );
    assert!(line.contains("1 msg · Jan 01, 00:00"), "{line:?}");
    // Upstream leaves one trailing cell free.
    assert_eq!(UnicodeWidthStr::width(line.as_str()), 71, "{line:?}");
}

#[test]
fn semantic_list_shows_compact_score_metadata_on_wide_rows() {
    let metadata = test_semantic_metadata_with_scores(
        "semantic evidence only",
        SemanticScoreBreakdown {
            hybrid: 1.23,
            semantic: 1.0,
            lexical: 0.23,
        },
        SemanticRationaleKind::LexicalBoosted,
    );
    let conversation = test_conversation();
    let wide = row_header(&conversation, 70, true, Some(&metadata));
    assert_eq!(wide.semantic_score.as_deref(), Some("1.23"));
    assert!(!header_line(&wide).contains("strong"));
    let narrow = row_header(&conversation, 69, true, Some(&metadata));
    assert_eq!(narrow.semantic_score, None);
    let lexical = row_header(&conversation, 120, false, Some(&metadata));
    assert_eq!(lexical.semantic_score, None);
}

#[test]
fn row_header_shows_source_labels_duration_and_highlights() {
    let mut conversation = test_conversation();
    conversation.source = crate::history::Source::Omp;
    conversation.duration_minutes = Some(75);
    let query = HighlightQuery::parse("sentinel");
    let header = build_row_header(&RowHeaderInput {
        conversation: &conversation,
        query: &query,
        width: 160,
        show_source_label: true,
        semantic_mode: false,
        semantic_metadata: None,
        now: Local::now(),
    });
    assert_eq!(header.project.text, "OMP · project sentinel");
    assert_eq!(highlighted(&header.project.segments()), vec!["sentinel"]);
    assert_eq!(header.duration.as_deref(), Some("1h 15m"));
    assert_eq!(
        header
            .custom_title
            .as_ref()
            .map(|title| title.text.as_str()),
        Some("title sentinel")
    );
    assert_eq!(header.summary.as_ref().unwrap().highlights.len(), 1);

    conversation.source = crate::history::Source::Pi;
    let header = build_row_header(&RowHeaderInput {
        conversation: &conversation,
        query: &query,
        width: 160,
        show_source_label: true,
        semantic_mode: false,
        semantic_metadata: None,
        now: Local::now(),
    });
    assert_eq!(header.project.text, "Pi  · project sentinel");

    conversation.project_name = None;
    let header = row_header(&conversation, 160, false, None);
    assert_eq!(header.project.text, "");
}

// --- row preview (upstream render_list tests) ---

#[test]
fn semantic_list_uses_conversation_preview_without_query() {
    let conversation = test_conversation();
    let preview = row_preview(&conversation, "", true, None, None, 80);
    assert_eq!(preview.preview.text, "lexical preview sentinel");
    assert_eq!(preview.source, PreviewSource::Preview);
    assert!(preview.preview.highlights.is_empty());
    assert_eq!(preview.context, None);
}

#[test]
fn semantic_list_uses_conversation_preview_while_query_has_no_metadata() {
    let conversation = test_conversation();
    let preview = row_preview(&conversation, "sentinel", true, None, None, 80);
    assert_eq!(preview.preview.text, "lexical preview sentinel");
    assert_eq!(highlighted(&preview.preview.segments()), vec!["sentinel"]);
}

#[test]
fn semantic_list_truncates_cleanly_at_narrow_width() {
    let conversation = test_conversation();
    let evidence_preview = format!("{} needle{}", "宽字符前缀".repeat(8), "x".repeat(120));
    let metadata = test_semantic_metadata_with_scores(
        &evidence_preview,
        SemanticScoreBreakdown {
            hybrid: 123.45,
            semantic: 67.89,
            lexical: 55.56,
        },
        SemanticRationaleKind::WeakMatch,
    );
    let width = 28;
    let preview = row_preview(&conversation, "needle", true, Some(&metadata), None, width);
    assert_eq!(preview.source, PreviewSource::SemanticEvidence);
    // The character-based windows overflow the cell budget with wide
    // characters, so upstream falls back to a plain prefix here.
    assert_eq!(preview.preview.text, "宽字符前缀宽字符前缀宽…");
    assert!(UnicodeWidthStr::width(preview.preview.text.as_str()) <= width - 4);
    let truncated = build_match_segments(&evidence_preview, "needle", width - 4);
    assert!(truncated.contains("needle"), "{truncated:?}");
    assert!(UnicodeWidthStr::width(truncated.as_str()) <= width - 4);
    let header = row_header(&conversation, width, true, Some(&metadata));
    assert_eq!(header.semantic_score, None);
}

#[test]
fn lexical_unquoted_render_shows_cached_hidden_full_text_context() {
    let mut conversation = test_conversation();
    conversation.preview = "visible lexical preview".to_string();
    conversation.full_text = format!("visible lexical preview {} hiddenneedle", "x ".repeat(200));
    let evidence =
        crate::search::build_lexical_evidence(&conversation, &ParsedQuery::parse("hiddenneedle"))
            .unwrap();
    let preview = row_preview(
        &conversation,
        "hiddenneedle",
        false,
        None,
        Some(&evidence),
        80,
    );
    assert_eq!(preview.source, PreviewSource::LexicalContext);
    assert!(preview.preview.text.contains("hiddenneedle"), "{preview:?}");
    assert_eq!(
        highlighted(&preview.preview.segments()),
        vec!["hiddenneedle"]
    );
    assert_eq!(preview.context, None);
}

#[test]
fn lexical_quoted_render_shows_hidden_literal_context() {
    let mut conversation = test_conversation();
    conversation.preview = "visible lexical preview".to_string();
    conversation.full_text = format!("visible lexical preview {} hidden_literal", "x ".repeat(80));
    let preview = row_preview(&conversation, "\"hidden_literal\"", false, None, None, 80);
    assert_eq!(preview.preview.text, "visible lexical preview");
    let context = preview.context.expect("literal context line");
    assert!(context.text.contains("hidden_literal"), "{context:?}");
    assert_eq!(highlighted(&context.segments()), vec!["hidden_literal"]);
}

#[test]
fn literal_query_rows_without_context_keep_the_four_line_pitch() {
    // Row 1 shows its literal in the preview (no context line); row 2
    // hides it in full_text (context line). Upstream reserved a fourth
    // line for both.
    let mut visible = test_conversation();
    visible.preview = "preview with hidden_literal shown".to_string();
    visible.full_text = visible.preview.clone();
    let mut hidden = test_conversation();
    hidden.preview = "visible lexical preview".to_string();
    hidden.full_text = format!("visible lexical preview {} hidden_literal", "x ".repeat(80));
    let query = "\"hidden_literal\"";
    assert!(crate::browser::query_has_literals(query));
    assert_eq!(
        row_preview(&visible, query, false, None, None, 80).context,
        None
    );
    assert!(
        row_preview(&hidden, query, false, None, None, 80)
            .context
            .is_some()
    );
}

#[test]
fn semantic_list_uses_semantic_evidence_preview_without_full_text_context() {
    let conversation = test_conversation();
    let metadata = test_semantic_metadata("semantic evidence only");
    let preview = row_preview(&conversation, "sentinel", true, Some(&metadata), None, 80);
    assert_eq!(preview.preview.text, "semantic evidence only");
    assert_eq!(preview.source, PreviewSource::SemanticEvidence);
    assert_eq!(preview.context, None);
}

#[test]
fn semantic_rows_with_metadata_ignore_lexical_evidence() {
    let mut conversation = test_conversation();
    conversation.full_text = format!("{} sentinel", "x ".repeat(100));
    let evidence = LexicalEvidence {
        context_ranges: vec![(200, 208)],
    };
    let metadata = test_semantic_metadata("semantic evidence only");
    let with_metadata = row_preview(
        &conversation,
        "sentinel",
        true,
        Some(&metadata),
        Some(&evidence),
        80,
    );
    assert_eq!(with_metadata.source, PreviewSource::SemanticEvidence);
    // Without metadata (the lexical fallback), the evidence is used.
    let fallback = row_preview(&conversation, "sentinel", true, None, Some(&evidence), 80);
    assert_eq!(fallback.source, PreviewSource::LexicalContext);
    assert!(fallback.preview.text.contains("sentinel"));
}

#[test]
fn semantic_list_shows_literal_context_when_evidence_lacks_literal() {
    let mut conversation = test_conversation();
    conversation.full_text = "tool output sentinel includes audio_generation literal".to_string();
    let preview = row_preview(
        &conversation,
        "semantic \"audio_generation\"",
        true,
        None,
        None,
        80,
    );
    assert_eq!(preview.preview.text, "lexical preview sentinel");
    let context = preview.context.expect("literal context");
    assert!(context.text.contains("audio_generation"), "{context:?}");
}

#[test]
fn semantic_literal_preview_uses_literal_ranges() {
    let conversation = test_conversation();
    let metadata = test_semantic_metadata(
        "normalized audio generation appears early before exact audio_generation literal",
    );
    let preview = row_preview(
        &conversation,
        "\"audio_generation\"",
        true,
        Some(&metadata),
        None,
        54,
    );
    assert!(
        preview.preview.text.contains("audio_generation"),
        "{preview:?}"
    );
    assert!(
        !preview.preview.text.contains("audio generation"),
        "{preview:?}"
    );
    assert_eq!(preview.context, None);
}

#[test]
fn semantic_literal_preview_merges_overlapping_ranges() {
    let conversation = test_conversation();
    let metadata = test_semantic_metadata("prefix audio_generation literal near the front");
    let preview = row_preview(
        &conversation,
        "audio \"audio_generation\"",
        true,
        Some(&metadata),
        None,
        60,
    );
    assert!(
        preview.preview.text.contains("audio_generation"),
        "{preview:?}"
    );
    assert_eq!(
        highlighted(&preview.preview.segments()),
        vec!["audio_generation"]
    );
}

#[test]
fn semantic_literal_context_requires_all_literals_visible() {
    let mut conversation = test_conversation();
    conversation.full_text = "alpha_exact near preview. beta_exact hidden deeper.".to_string();
    let metadata = test_semantic_metadata("semantic alpha_exact only");
    let preview = row_preview(
        &conversation,
        "semantic \"alpha_exact\" \"beta_exact\"",
        true,
        Some(&metadata),
        None,
        80,
    );
    assert!(preview.preview.text.contains("alpha_exact"), "{preview:?}");
    let context = preview.context.expect("context for the hidden literal");
    assert!(context.text.contains("beta_exact"), "{context:?}");
}

#[test]
fn lexical_rows_window_around_normalized_matches() {
    let mut conversation = test_conversation();
    conversation.preview = format!("{} needle at the end", "words ".repeat(30));
    let preview = row_preview(&conversation, "needle", false, None, None, 44);
    assert_eq!(preview.source, PreviewSource::Preview);
    assert!(preview.preview.text.starts_with('…'), "{preview:?}");
    assert!(preview.preview.text.contains("needle"), "{preview:?}");
    assert!(UnicodeWidthStr::width(preview.preview.text.as_str()) <= 40);
}
