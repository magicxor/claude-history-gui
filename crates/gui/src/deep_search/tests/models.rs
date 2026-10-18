//! P tests: the view models, built from real core responses over the agent
//! corpus (no window).

use std::collections::BTreeMap;

use claude_history_core::agent::args::{
    AgentOutlineArgs, AgentReadArgs, AgentSearchArgs, AgentWithinArgs,
};
use claude_history_core::agent::diagnostic::{AgentWarning, AgentWarningKind};
use claude_history_core::agent::{AgentNotice, AgentRequest, AgentResponse, AgentResult, execute};
use claude_history_core::config::ConfigFile;
use claude_history_core::semantic::index::SemanticIndexProgress;

use crate::deep_search::agent_text::stderr_part;
use crate::deep_search::results::{
    ErrorModel, HitModel, OutlineModel, OutlineRow, ReadModel, SearchModel, SearchRow,
    ambiguous_candidates, format_chars, progress_text, range_label, uncached_banner,
    warnings_summary,
};
use crate::model::{ContentVisibilityHint, RevealKind};
use crate::test_support::TestWorld;
use crate::test_support::fixtures_agent::{agent_corpus, duplicate_uuid_sessions};

fn run(world: &TestWorld, request: AgentRequest) -> AgentResponse {
    execute(&world.env, &ConfigFile::default(), request)
}

fn search(world: &TestWorld, query: &str) -> AgentResponse {
    run(world, AgentRequest::Search(AgentSearchArgs::new(query)))
}

fn search_model(response: &AgentResponse) -> SearchModel {
    match response.result.as_ref().expect("a result") {
        AgentResult::Search(result) | AgentResult::Within(result) => {
            SearchModel::from_result(result)
        }
        other => panic!("not a search result: {other:?}"),
    }
}

/// D-010, D-026: groups, hits and the footer equal the core data and text.
#[test]
fn a_grouped_search_maps_the_core_output() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let response = search(&world, "alpha");
    assert!(response.is_success(), "{}", response.stderr);
    let model = search_model(&response);
    let Some(AgentResult::Search(result)) = &response.result else {
        panic!("a search result");
    };
    assert!(
        model.groups >= 3,
        "retry, login and malformed mention alpha"
    );
    assert_eq!(model.groups, result.output.groups.len());
    assert_eq!(model.hits, result.hits().len());
    // The protocol text agrees.
    let text = response.text();
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("conversation rank="))
            .count(),
        model.groups
    );
    assert_eq!(
        text.lines().filter(|line| line.starts_with("hit ")).count(),
        model.hits
    );
    assert_eq!(
        model.stats_line(),
        format!(
            "{} groups · {} hits · shortlisted {} · loaded {}",
            result.output.groups.len(),
            result.hits().len(),
            result.output.stats.shortlisted,
            result.output.stats.transcripts_loaded
        )
    );
    let SearchRow::Group(first) = &model.rows[0] else {
        panic!("the first row is a group card");
    };
    let group = &result.output.groups[0];
    assert_eq!(first.rank, 1);
    assert_eq!(first.title, group.title);
    assert_eq!(first.project_id, group.project_id);
    assert_eq!(first.score_text(), format!("{:.6}", group.score));
    assert_eq!(first.shown, group.hits.len());
    assert_eq!(first.total, group.total_hits);
    assert!(
        first
            .meta()
            .ends_with(&format!("{}/{} hits", first.shown, first.total))
    );
    let info = result
        .conversation(&group.conversation_ref)
        .expect("its info");
    assert_eq!(
        first.full_ref, info.full_ref,
        "the tooltip holds the full ref"
    );
    assert!(first.full_ref.len() > first.conversation_ref.len() - 1);
}

/// D-008: flat output has hit rows only and no group count.
#[test]
fn a_flat_search_has_hit_rows_only() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let response = run(
        &world,
        AgentRequest::Search(AgentSearchArgs {
            flat: true,
            ..AgentSearchArgs::new("alpha")
        }),
    );
    let model = search_model(&response);
    assert!(model.flat);
    assert!(
        model
            .rows
            .iter()
            .all(|row| matches!(row, SearchRow::Hit(_)))
    );
    assert!(
        model
            .stats_line()
            .starts_with(&format!("{} hits · shortlisted", model.hits))
    );
}

/// D-004: a quoted-only query forces the exact mode.
#[test]
fn a_quoted_only_query_forces_exact() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let quoted = search_model(&search(&world, "\"refresh token\""));
    assert!(quoted.exact_forced);
    assert_eq!(quoted.mode.as_str(), "exact");
    let plain = search_model(&search(&world, "refresh token"));
    assert!(!plain.exact_forced);
}

/// D-019, D-021: within output has a target line (also without hits);
/// reading groups messages by conversation.
#[test]
fn within_and_read_models() {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let response = search(&world, "alpha");
    let model = search_model(&response);
    let SearchRow::Group(group) = &model.rows[0] else {
        panic!("a group");
    };
    let within = run(
        &world,
        AgentRequest::Within(AgentWithinArgs::new(&group.full_ref, "zzz")),
    );
    assert!(within.is_success(), "{}", within.stderr);
    let within_model = search_model(&within);
    assert!(within_model.within && within_model.rows.is_empty());
    let line = within_model.target_line.expect("a target line");
    assert!(
        line.starts_with("conversation project=")
            && line.contains(" uuid=")
            && line.contains(" ref=")
    );

    // Two refs of two conversations: two groups; ranges give the cards.
    let retry_ref = ref_of(&world, &corpus.retry);
    let login_ref = ref_of(&world, &corpus.login);
    let read = run(
        &world,
        AgentRequest::Read(AgentReadArgs::new([
            format!("{retry_ref}:m2..m3"),
            format!("{login_ref}:m1..m1"),
        ])),
    );
    assert!(read.is_success(), "{}", read.stderr);
    let Some(AgentResult::Read(result)) = &read.result else {
        panic!("a read result");
    };
    let model = ReadModel::from_result(result, &BTreeMap::new());
    assert_eq!(model.groups.len(), 2, "one header per conversation");
    assert_eq!(
        model.groups[0]
            .cards
            .iter()
            .map(|c| c.header())
            .take(1)
            .collect::<Vec<_>>()
            .len(),
        1
    );
    let first = &model.groups[0].cards;
    assert_eq!((first[0].ordinal, first[0].role), (2, "assistant"));
    assert_eq!((first[1].ordinal, first[1].role), (3, "user"));
    assert!(first[0].header().starts_with("m2 · assistant · line "));
    assert!(first[0].anchor.starts_with("ma_"));
    assert!(model.range.is_none(), "two conversations: no paging");
}

/// The full ref of the conversation at `path`.
fn ref_of(world: &TestWorld, path: &std::path::Path) -> String {
    let key = claude_history_core::agent::refs::conversation_key_for_path(
        &world.env,
        &ConfigFile::default(),
        path,
    )
    .expect("keys")
    .expect("a key inside the roots");
    key.conversation_ref().full_ref()
}

/// D-022: a 12-message conversation has one row per message, a 25-message
/// one has segments of 10.
#[test]
fn outline_rows_and_segments() {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let outline = |path: &std::path::Path| {
        let response = run(
            &world,
            AgentRequest::Outline(AgentOutlineArgs::new(ref_of(&world, path))),
        );
        assert!(response.is_success(), "{}", response.stderr);
        let Some(AgentResult::Outline(result)) = &response.result else {
            panic!("an outline");
        };
        OutlineModel::from_result(result)
    };
    let short = outline(&corpus.outline12);
    assert_eq!(short.rows.len(), 12);
    assert!(
        short
            .rows
            .iter()
            .all(|row| matches!(row, OutlineRow::Message { .. }))
    );
    assert_eq!(short.summary(), "12 visible messages");
    let long = outline(&corpus.outline25);
    assert_eq!(long.rows.len(), 3);
    let sizes: Vec<(usize, usize)> = long
        .rows
        .iter()
        .map(|row| match row {
            OutlineRow::Segment { start, end, .. } => (*start, *end),
            other => panic!("a segment, got {other:?}"),
        })
        .collect();
    assert_eq!(sizes, vec![(1, 10), (11, 20), (21, 25)]);
    assert_eq!(long.summary(), "25 visible messages");
    assert_eq!(
        OutlineModel::segment_label(1, 10, 8100),
        "seg m1..m10 · 8.1k chars"
    );
}

/// D-012: the read command of a hit with tools on.
#[test]
fn the_read_command_follows_the_hit() {
    let hit = HitModel {
        conversation_ref: "ch_9c1e42a7b3d1".into(),
        full_ref: "ch_9c1e42a7b3d1ffffffffffffffffffff".into(),
        title: "Fix login bug".into(),
        project_id: "pr_1".into(),
        project: "proj".into(),
        anchors: vec!["ma_1f".into(), "ma_2a".into()],
        source: "lexical",
        score: 0.5,
        breakdown: None,
        focus: (12, 12),
        read: (10, 14),
        preview: "retry".into(),
        hint: ContentVisibilityHint {
            tools: true,
            thinking: true,
            ..ContentVisibilityHint::default()
        },
        info: None,
    };
    assert_eq!(
        hit.read_command(),
        "claude-history agent read ch_9c1e42a7b3d1ffffffffffffffffffff:m10..m14 --focus m12..m12 --tools --thinking"
    );
    assert_eq!(hit.focus_label(), "m12");
    assert_eq!(range_label(2, 4), "m2..m4");
    let plain = HitModel {
        hint: ContentVisibilityHint::default(),
        ..hit
    };
    assert!(plain.read_command().ends_with("--focus m12..m12"));
}

/// "Copy as agent records" copies the lines of the agent text of the hit.
#[test]
fn agent_records_are_the_hit_and_read_lines() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let response = search(&world, "alpha");
    let model = search_model(&response);
    let hits: Vec<&HitModel> = model
        .rows
        .iter()
        .filter_map(|row| match row {
            SearchRow::Hit(hit) => Some(hit),
            SearchRow::Group(_) => None,
        })
        .collect();
    assert!(!hits.is_empty());
    for hit in hits {
        let records = hit
            .agent_records(response.text())
            .expect("the hit is in the text");
        let lines: Vec<&str> = records.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("hit ")
                && lines[0].contains(&format!(" ref={} ", hit.conversation_ref))
        );
        assert!(lines[1].starts_with("read ref="));
        assert!(response.text().contains(lines[0]) && response.text().contains(lines[1]));
    }
    let missing = HitModel {
        conversation_ref: "ch_000000000000".into(),
        ..search_model(&response)
            .rows
            .iter()
            .find_map(|row| match row {
                SearchRow::Hit(hit) => Some(hit.clone()),
                SearchRow::Group(_) => None,
            })
            .unwrap()
    };
    assert!(missing.agent_records(response.text()).is_none());
}

/// D-005: the open request of a hit carries the ordinal of its focus.
#[test]
fn a_hit_reveals_its_focus_ordinal_in_its_conversation() {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let response = search(&world, "\"retry loop\"");
    let model = search_model(&response);
    let hit = model
        .rows
        .iter()
        .find_map(|row| match row {
            SearchRow::Hit(hit) => Some(hit),
            SearchRow::Group(_) => None,
        })
        .expect("a hit");
    let spec = hit.reveal().expect("its conversation is known");
    let (target, reveal) = spec.request();
    assert_eq!(target.path, corpus.retry);
    assert_eq!(
        target.source,
        Some(claude_history_core::history::Source::Claude)
    );
    assert_eq!(reveal.path, corpus.retry);
    assert_eq!(reveal.full_ref.as_deref(), Some(hit.full_ref.as_str()));
    let RevealKind::AgentOrdinal { ordinal, hint } = reveal.kind else {
        panic!("an agent ordinal");
    };
    assert_eq!(ordinal, hit.focus.0);
    assert_eq!(ordinal, 2, "the retry answer is message 2");
    assert_eq!(hint, hit.hint);
}

/// D-013: warnings dedupe by (kind, ref) and group in the core's order.
#[test]
fn the_warnings_banner_dedupes_and_orders() {
    let warning = |kind, reference: &str| AgentWarning {
        kind,
        reference: Some(reference.to_string()),
        detail: "detail".to_string(),
    };
    assert!(warnings_summary(&[]).is_none());
    let (summary, details) =
        warnings_summary(&[warning(AgentWarningKind::MalformedTranscript, "ch_a")]).unwrap();
    assert_eq!(summary, "1 warning: malformed-transcript 1");
    assert_eq!(details, vec!["malformed-transcript ch_a: detail"]);
    let (summary, details) = warnings_summary(&[
        warning(AgentWarningKind::Io, "ch_x"),
        warning(AgentWarningKind::MalformedTranscript, "ch_a"),
        warning(AgentWarningKind::MalformedTranscript, "ch_b"),
        warning(AgentWarningKind::MalformedTranscript, "ch_a"),
        warning(AgentWarningKind::Skipped, "ch_s"),
    ])
    .unwrap();
    assert_eq!(
        summary,
        "4 warnings: skipped 1, malformed-transcript 2, io 1"
    );
    assert_eq!(details.len(), 4);
    assert!(details[0].starts_with("skipped ch_s"));
}

/// The real malformed fixture: core reports a warning, the banner counts it.
#[test]
fn a_malformed_transcript_is_one_warning() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let response = search(&world, "alpha");
    let (summary, details) = warnings_summary(&response.warnings).expect("a warning");
    assert_eq!(summary, "1 warning: malformed-transcript 1");
    assert_eq!(details.len(), 1);
}

/// D-006: the banner text, the progress line and the stderr part.
#[test]
fn notices_become_banners_and_status() {
    let uncached = AgentNotice::UncachedPassages {
        count: 120,
        bounded_top_up: false,
    };
    assert_eq!(
        uncached_banner(std::slice::from_ref(&uncached)).as_deref(),
        Some("120 passage(s) are not cached. Generate the semantic cache to include them.")
    );
    assert!(
        uncached_banner(&[AgentNotice::UncachedPassages {
            count: 3,
            bounded_top_up: true
        }])
        .unwrap()
        .contains("remain uncached")
    );
    assert_eq!(
        progress_text(&AgentNotice::SemanticProgress(
            SemanticIndexProgress::Embedding {
                completed: 3,
                total: 10
            }
        ))
        .as_deref(),
        Some("Embedding 3/10")
    );
    assert!(progress_text(&uncached).is_none());
    assert!(uncached_banner(&[]).is_none());
}

/// D-014: typed errors, the cancelled response and the stderr part.
#[test]
fn errors_show_kind_and_detail() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let response = run(&world, AgentRequest::Read(AgentReadArgs::new(["ch_zz"])));
    assert!(!response.is_success());
    let error = ErrorModel::from_response(&response).expect("an error");
    assert!(error.text.starts_with("invalid-ref: "), "{}", error.text);
    assert!(error.candidates.is_empty());
    assert_eq!(stderr_part(&response), "", "no notices before the envelope");

    let cancelled = AgentResponse {
        stdout: String::new(),
        stderr: AgentResponse::CANCELLED_TEXT.to_string(),
        exit_code: 1,
        result: None,
        warnings: vec![],
        error: None,
        notices: vec![],
    };
    assert!(cancelled.is_cancelled());
    let untyped = ErrorModel::from_response(&cancelled).unwrap();
    assert_eq!(untyped.text, "Semantic search cancelled");
    assert!(ErrorModel::from_response(&search(&world, "alpha")).is_none());
}

/// D-014: ambiguous candidates are the handles of the error detail.
#[test]
fn ambiguous_candidates_are_parsed() {
    let detail = "ambiguous conversation ref; retry with one of these ch_ handles; candidates: ch_aaaaaaaaaaaa project=pr_1 source=claude directory=a file=x.jsonl\n  ch_bbbbbbbbbbbb project=pr_2 source=claude directory=b file=x.jsonl";
    assert_eq!(
        ambiguous_candidates(detail),
        vec!["ch_aaaaaaaaaaaa", "ch_bbbbbbbbbbbb"]
    );
    assert!(ambiguous_candidates("no candidates here").is_empty());

    // Real core output: a UUID in two projects.
    let world = TestWorld::new();
    let (first, _second) = duplicate_uuid_sessions(&world, 77);
    let uuid = first.file_stem().unwrap().to_string_lossy().to_string();
    let response = run(&world, AgentRequest::Read(AgentReadArgs::new([uuid])));
    let error = ErrorModel::from_response(&response).expect("an error");
    assert_eq!(error.candidates.len(), 2, "{}", error.text);
    assert!(error.text.starts_with("ambiguous-ref: "));
}

#[test]
fn character_counts_are_compact() {
    assert_eq!(format_chars(312), "312");
    assert_eq!(format_chars(1200), "1.2k");
    assert_eq!(format_chars(8100), "8.1k");
    assert_eq!(format_chars(2000), "2k");
    assert_eq!(format_chars(999), "999");
}
