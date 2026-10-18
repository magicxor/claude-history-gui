//! Core tests named by the FEATURES.md rows owned by WP-C for list search
//! (row id in each test name) that no earlier test covered exactly.

use super::lexical::{SearchableConversation, score_text_debug};
use super::query::ParsedQuery;
use super::test_fixtures::one_message_conversation;
use super::{build_lexical_evidence, precompute_search_text, search};
use chrono::{Duration, Local};

#[test]
fn s001_parsed_query_predicates() {
    for (query, effectively_empty, quoted_only) in [
        ("", true, false),
        ("   ", true, false),
        ("\"\"", true, false),
        ("\"  \"", true, false),
        ("\"exact phrase\"", false, true),
        ("\"a\" \"b\"", false, true),
        ("\"unterminated", false, true),
        ("words \"and literal\"", false, false),
        ("plain words", false, false),
    ] {
        let parsed = ParsedQuery::parse(query);
        assert_eq!(
            parsed.is_effectively_empty(),
            effectively_empty,
            "{query:?}"
        );
        assert_eq!(parsed.is_quoted_only(), quoted_only, "{query:?}");
    }
    let parsed = ParsedQuery::parse("intent \"Literal\" \"\"");
    assert_eq!(parsed.literals().len(), 1, "empty quotes are dropped");
    assert_eq!(parsed.literals()[0].text(), "Literal");
}

fn searchable(title: &str, summary: &str, project: &str, body: &str) -> SearchableConversation {
    SearchableConversation {
        text_lower: format!("{body} {project}"),
        title_lower: title.to_string(),
        summary_lower: summary.to_string(),
        project_lower: project.to_string(),
        index: 0,
    }
}

#[test]
fn s013_score_uses_upstream_field_weights_adjacency_and_freshness() {
    let now = Local::now();
    let ln2 = 2f64.ln();
    let s = searchable("alpha", "alpha", "alpha", "alpha");
    let debug = score_text_debug(&s, "alpha", &["alpha"], &[], now, now).unwrap();
    let weights = debug
        .fields
        .iter()
        .map(|field| (field.name, field.weight))
        .collect::<Vec<_>>();
    assert_eq!(
        weights,
        [
            ("title", 5.0),
            ("summary", 3.0),
            ("project", 4.0),
            ("body", 1.0)
        ]
    );
    // One match per field: weight × ln(1 + 1); fresh (age 0) adds 2.
    assert!(
        (debug.total - (13.0 * ln2 + 2.0)).abs() < 1e-9,
        "{}",
        debug.total
    );
    assert!((debug.freshness - 2.0).abs() < 1e-9);

    // Term frequency saturates at 10 matches: ln(11).
    let body = "alpha ".repeat(25);
    let debug = score_text_debug(
        &searchable("", "", "", &body),
        &body,
        &["alpha"],
        &[],
        now,
        now,
    )
    .unwrap();
    assert!((debug.fields[0].tf_score - 11f64.ln()).abs() < 1e-9);

    // Adjacent query words add weight × 2 per adjacent occurrence (≤ 3).
    let pairs = vec!["cache warming".to_string()];
    let debug = score_text_debug(
        &searchable("", "", "", "cache warming"),
        "cache warming",
        &["cache", "warming"],
        &pairs,
        now,
        now,
    )
    .unwrap();
    assert!((debug.fields[0].adjacency_score - 2.0).abs() < 1e-9);

    // Freshness halves every 7 days: 2 · 2^(−age/7).
    let week_old = score_text_debug(
        &searchable("", "", "", "alpha"),
        "alpha",
        &["alpha"],
        &[],
        now - Duration::days(7),
        now,
    )
    .unwrap();
    assert!(
        (week_old.freshness - 1.0).abs() < 1e-6,
        "{}",
        week_old.freshness
    );

    // Equal scores: the newer conversation first.
    let convs = vec![
        one_message_conversation("tie word", now - Duration::days(30), None, None, None),
        one_message_conversation(
            "tie word",
            now - Duration::days(30) + Duration::seconds(1),
            None,
            None,
            None,
        ),
    ];
    let index = precompute_search_text(&convs);
    let freeze = now + Duration::days(3650);
    assert_eq!(search(&convs, &index, "tie", freeze), vec![1, 0]);
}

#[test]
fn s012_every_word_must_start_a_word() {
    let now = Local::now();
    let convs = vec![
        one_message_conversation("please refresh it", now, None, None, None),
        one_message_conversation("we prefer this", now, None, None, None),
    ];
    let index = precompute_search_text(&convs);
    assert_eq!(search(&convs, &index, "ref", now), vec![0]);
    assert_eq!(search(&convs, &index, "ref please", now), vec![0]);
    assert!(search(&convs, &index, "ref missing", now).is_empty());
}

#[test]
fn s019_hidden_match_context_is_clustered_into_at_most_three_ranges() {
    let now = Local::now();
    let far = " filler".repeat(60);
    let text = format!(
        "visible start{far} needle one{far} needle two{far} needle three{far} needle four{far} needle five"
    );
    let mut conversation = one_message_conversation(&text, now, None, None, None);
    conversation.preview = "visible start".to_string();
    let evidence = build_lexical_evidence(&conversation, &ParsedQuery::parse("needle")).unwrap();
    assert_eq!(evidence.context_ranges.len(), 3);
    let mut previous_end = 0;
    for (start, end) in &evidence.context_ranges {
        assert!(*start >= previous_end, "ordered and disjoint");
        assert!(conversation.full_text[*start..*end].contains("needle"));
        previous_end = *end;
    }
    // Hits close together merge into one cluster.
    let close = one_message_conversation("needle needle needle", now, None, None, None);
    let mut close = close;
    close.preview = "other".to_string();
    let evidence = build_lexical_evidence(&close, &ParsedQuery::parse("needle")).unwrap();
    assert_eq!(evidence.context_ranges.len(), 1);
    // Nothing hidden: no evidence.
    let shown = one_message_conversation("needle", now, None, None, None);
    let mut shown = shown;
    shown.preview = "needle".to_string();
    assert!(build_lexical_evidence(&shown, &ParsedQuery::parse("needle")).is_none());
}
