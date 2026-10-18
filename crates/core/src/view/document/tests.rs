//! Document-model tests. Most fixtures are the ones of the ledger tests
//! (`view/ledger/tests.rs`); every case also runs [`assert_ledger_parity`],
//! which renders the same entries with the ledger and checks that both
//! agree on navigable messages, labels, clickable ids, truncation counts and
//! the first timing slot.

use super::*;
use crate::theme;
use crate::view::ledger::{
    LineStyle, NAME_WIDTH, RenderedConversation, RenderedLine, TIMESTAMP_WIDTH,
    render_parsed_conversation,
};
use crate::view::{fitted_name, parse_conversation_file};
use chrono::{Local, TimeZone};
use std::path::PathBuf;

// -----------------------------------------------------------------
// Fixtures and helpers
// -----------------------------------------------------------------

fn entry(entry_index: usize, json: &str) -> RenderableEntry {
    RenderableEntry::new(entry_index, serde_json::from_str(json).unwrap())
}

fn user_entry(entry_index: usize, text: &str, timestamp: Option<&str>) -> RenderableEntry {
    let ts_field = match timestamp {
        Some(t) => format!(r#","timestamp":"{}""#, t),
        None => String::new(),
    };
    entry(
        entry_index,
        &format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{}"}}{}}}"#,
            text, ts_field
        ),
    )
}

fn assistant_text_entry(
    entry_index: usize,
    text: &str,
    timestamp: Option<&str>,
) -> RenderableEntry {
    let ts_field = match timestamp {
        Some(t) => format!(r#","timestamp":"{}""#, t),
        None => String::new(),
    };
    entry(
        entry_index,
        &format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}}{}}}"#,
            text, ts_field
        ),
    )
}

fn tool_summary_entries() -> Vec<RenderableEntry> {
    vec![
        entry(
            0,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"one"}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"grep result"}]}}"#,
        ),
        entry(
            2,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"src/main.rs"}},{"type":"tool_use","id":"toolu_3","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        ),
        entry(
            3,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_3","content":"bash result"}]}}"#,
        ),
    ]
}

fn pi_thinking_entries() -> Vec<RenderableEntry> {
    vec![
        entry(
            0,
            r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"thinking","thinking":"inspect files","signature":""},{"type":"tool_use","id":"call_1","name":"bash","input":{"command":"pwd"}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"/tmp/project"}]}}"#,
        ),
        entry(
            2,
            r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"thinking","thinking":"inspect status","signature":""},{"type":"tool_use","id":"call_2","name":"bash","input":{"command":"git status"}}]}}"#,
        ),
        entry(
            3,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_2","content":"clean"}]}}"#,
        ),
    ]
}

fn subagent_entries() -> Vec<RenderableEntry> {
    vec![
        entry(
            0,
            r#"{"type":"assistant","parent_tool_use_id":"toolu_parent_abc","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"one"}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","parent_tool_use_id":"toolu_parent_abc","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"grep result"}]}}"#,
        ),
    ]
}

fn reordered_blocks_entry() -> RenderableEntry {
    entry(
        0,
        r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"THINK_BLOCK","signature":"sig"},
            {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}},
            {"type":"text","text":"TEXT_BLOCK"}
        ]}}"#,
    )
}

fn agent_progress_entries() -> Vec<RenderableEntry> {
    vec![
        entry(
            0,
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abc1234","message":{"type":"user","message":{"role":"user","content":[
                {"type":"text","text":"agent says hi"},
                {"type":"tool_result","tool_use_id":"toolu_1","content":"result body"}
            ]}}}}"#,
        ),
        entry(
            1,
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abc1234","message":{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"a.rs"}},
                {"type":"tool_use","id":"toolu_3","name":"Bash","input":{"command":"one\ntwo\nthree\nfour\nfive"}}
            ]}}}}"#,
        ),
        entry(
            2,
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abc1234","message":{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"text","text":"progress text"},
                {"type":"thinking","thinking":"hidden thought","signature":"s"}
            ]}}}}"#,
        ),
    ]
}

/// A conversation exercising every message kind and truncation path.
fn mixed_entries() -> Vec<RenderableEntry> {
    vec![
        user_entry(0, "Hello **there**", Some("2026-09-04T12:34:56Z")),
        entry(
            1,
            r#"{"type":"assistant","timestamp":"2026-09-04T12:35:00Z","message":{"role":"assistant","content":[
                {"type":"text","text":"First text"},
                {"type":"thinking","thinking":"pondering","signature":"s"},
                {"type":"text","text":"Second text"},
                {"type":"tool_use","id":"toolu_a","name":"Edit","input":{"file_path":"a.rs","old_string":"one\ntwo","new_string":"uno\ndos\ntres"}},
                {"type":"tool_use","id":"toolu_b","name":"Read","input":{"file_path":"b.rs"}}
            ]}}"#,
        ),
        entry(
            2,
            r#"{"type":"user","timestamp":"2026-09-04T12:35:10Z","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu_a","content":"l1\nl2\nl3\nl4\nl5\nl6"},
                {"type":"tool_result","tool_use_id":"toolu_b","content":[{"type":"text","text":"short"}]}
            ]}}"#,
        ),
        entry(
            3,
            r#"{"type":"assistant","parent_tool_use_id":"toolu_parent_xyz","timestamp":"2026-09-04T12:36:00Z","message":{"role":"assistant","content":[
                {"type":"text","text":"sub text"},
                {"type":"tool_use","id":"toolu_c","name":"Task","input":{"description":"d","prompt":"p1\np2\np3\np4"}}
            ]}}"#,
        ),
        entry(
            4,
            r#"{"type":"user","parent_tool_use_id":"toolu_parent_xyz","message":{"role":"user","content":[
                {"type":"text","text":"sub user"},
                {"type":"tool_result","tool_use_id":"toolu_c","content":{"a":1,"b":[1,2]}}
            ]}}"#,
        ),
        entry(
            5,
            r#"{"type":"pi-metadata","label":"notice","text":"custom note","timestamp":"2026-09-04T12:37:00Z"}"#,
        ),
        entry(
            6,
            r#"{"type":"pi-metadata","label":"Compaction","text":"hidden","searchable":false}"#,
        ),
        entry(
            7,
            r#"{"type":"user","message":{"role":"user","content":"Base directory for this skill: /x\n\nDo things."}}"#,
        ),
        entry(
            8,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"image","source":{}}]}}"#,
        ),
        assistant_text_entry(9, "   ", None),
        entry(
            10,
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-zz99","message":{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"r1\nr2\nr3\nr4\nr5"}]}}}}"#,
        ),
        entry(
            11,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_d","name":"Glob","input":{"pattern":"*.rs"}}]}}"#,
        ),
        entry(
            12,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_d","content":"x.rs"}]}}"#,
        ),
        entry(
            13,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_e","name":"WebSearch","input":{"query":"q"}}]}}"#,
        ),
        user_entry(14, "<local-command-stdout>out</local-command-stdout>", None),
        user_entry(15, "<local-command-caveat>c</local-command-caveat>", None),
        entry(
            16,
            r#"{"type":"user","message":{"role":"user","content":[
                {"type":"text","text":"<command-name>/model</command-name><command-args>opus</command-args>"},
                {"type":"text","text":"<command-name>/clear</command-name>"},
                {"type":"text","text":"tail"}
            ]}}"#,
        ),
    ]
}

fn fixture_entries(path: &str) -> Vec<RenderableEntry> {
    parse_conversation_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

fn prefs(tool_display: ToolDisplayMode) -> ViewPrefs {
    ViewPrefs {
        tool_display,
        show_thinking: false,
        show_timing: false,
    }
}

fn build(entries: &[RenderableEntry], prefs: &ViewPrefs) -> ConversationDocument {
    build_document(entries, prefs, &BTreeSet::new())
}

fn all_blocks(doc: &ConversationDocument) -> Vec<&DocBlock> {
    fn walk<'a>(blocks: &'a [DocBlock], out: &mut Vec<&'a DocBlock>) {
        for block in blocks {
            out.push(block);
            if let DocBlockKind::ToolSummaryDetails { items, .. } = &block.kind {
                walk(items, out);
            }
        }
    }
    let mut out = Vec::new();
    for message in &doc.messages {
        walk(&message.blocks, &mut out);
    }
    out
}

/// All visible text of the document (labels and block texts).
fn doc_text(doc: &ConversationDocument) -> String {
    let mut text = String::new();
    for message in &doc.messages {
        text.push_str(&message.label);
        text.push('\n');
        for block in &message.blocks {
            text.push_str(&block.plain_text());
            text.push('\n');
        }
    }
    text
}

fn line_text(line: &RenderedLine) -> String {
    line.spans.iter().map(|(text, _)| text.as_str()).collect()
}

fn rendered_text(conversation: &RenderedConversation) -> String {
    conversation
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The name-column span of a ledger row (the span before `" │ "`).
fn name_span(line: &RenderedLine) -> &(String, LineStyle) {
    let separator = line
        .spans
        .iter()
        .position(|(text, _)| text == " │ ")
        .unwrap_or_else(|| panic!("no separator in {:?}", line_text(line)));
    &line.spans[separator - 1]
}

/// The ledger label of a message's first row, derived from its first block.
fn expected_first_label(message: &DocMessage) -> String {
    let mut block = &message.blocks[0];
    while let DocBlockKind::ToolSummaryDetails { items, .. } = &block.kind {
        block = &items[0];
    }
    match &block.kind {
        DocBlockKind::ToolResult(_) => "↳ Result".to_string(),
        DocBlockKind::SubagentToolResult(_) => "  ↳ Tool".to_string(),
        DocBlockKind::Thinking { .. } => "Thinking".to_string(),
        _ => message.label.clone(),
    }
}

fn indicator_counts(rendered: &RenderedConversation) -> Vec<usize> {
    rendered
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter_map(|(text, _)| {
            text.strip_prefix('(')?
                .strip_suffix(" more lines...)")?
                .parse()
                .ok()
        })
        .collect()
}

fn ledger_clickable_ids(rendered: &RenderedConversation) -> BTreeSet<ToolOutputId> {
    rendered
        .lines
        .iter()
        .filter(|line| line.clickable)
        .filter_map(|line| line.tool_output_id.clone())
        .collect()
}

fn doc_toggle_ids(doc: &ConversationDocument) -> BTreeSet<ToolOutputId> {
    all_blocks(doc)
        .into_iter()
        .filter_map(|block| block.toggle_id().cloned())
        .collect()
}

fn doc_hidden_counts(doc: &ConversationDocument) -> Vec<usize> {
    all_blocks(doc)
        .into_iter()
        .filter_map(|block| match &block.kind {
            DocBlockKind::ToolCall(call) => call.hidden_lines(),
            DocBlockKind::ToolResult(result) => result.hidden_lines(),
            DocBlockKind::SubagentToolResult(result) => result.hidden_lines(),
            _ => None,
        })
        .collect()
}

/// Check that the document and the ledger (at width 80) agree for these
/// entries, preferences and expanded set. Returns the document.
fn assert_ledger_parity(
    entries: &[RenderableEntry],
    prefs: &ViewPrefs,
    expanded: &BTreeSet<ToolOutputId>,
) -> ConversationDocument {
    let doc = build_document(entries, prefs, expanded);
    let rendered = render_parsed_conversation(entries, &prefs.render_options(80, expanded));
    let context = format!(
        "prefs {:?}, expanded {:?}\nledger:\n{}\ndocument:\n{:#?}",
        prefs,
        expanded,
        rendered_text(&rendered),
        doc
    );

    // Navigable messages are the ledger's message ranges, in order.
    let nav: Vec<usize> = doc.navigable_entry_indices();
    let ranges: Vec<usize> = rendered.messages.iter().map(|m| m.entry_index).collect();
    assert_eq!(nav, ranges, "navigable messages\n{context}");

    // No empty messages; a rendered message renders something.
    assert!(
        doc.messages
            .iter()
            .all(|message| !message.blocks.is_empty()),
        "{context}"
    );
    assert_eq!(
        doc.messages.is_empty(),
        rendered.lines.is_empty(),
        "{context}"
    );

    // First row label, label style and timing slot of each navigable message.
    for (position, range) in rendered.messages.iter().enumerate() {
        let message = &doc.messages[doc.message_at_nav_position(position).unwrap()];
        let first = &rendered.lines[range.start_line];
        let (name, style) = name_span(first);
        assert_eq!(
            name.trim_start(),
            fitted_name(&expected_first_label(message)).trim_start(),
            "label of entry {}\n{context}",
            message.entry_index
        );
        if matches!(message.blocks[0].kind, DocBlockKind::Markdown { .. }) {
            assert_eq!(
                style.fg,
                Some(message.label_role.color(theme::current())),
                "{context}"
            );
            assert_eq!(style.bold, message.bold, "{context}");
            assert_eq!(style.dimmed, message.dimmed, "{context}");
        }
        match &message.blocks[0].timing {
            DocTiming::Stamp(stamp) => assert_eq!(first.spans[0].0, format!(" {stamp} ")),
            DocTiming::Pad => assert_eq!(first.spans[0].0, " ".repeat(TIMESTAMP_WIDTH)),
            DocTiming::Disabled => assert_eq!(first.spans[0].0.chars().count(), NAME_WIDTH),
        }
    }

    // Clickable outputs and "(N more lines...)" counts. The fixtures keep
    // every line narrower than 80 columns, so source lines equal rows.
    assert_eq!(
        doc_toggle_ids(&doc),
        ledger_clickable_ids(&rendered),
        "clickable ids\n{context}"
    );
    assert_eq!(
        doc_hidden_counts(&doc),
        indicator_counts(&rendered),
        "hidden line counts\n{context}"
    );
    for block in all_blocks(&doc) {
        match &block.kind {
            DocBlockKind::ToolCall(call) => {
                assert_eq!(call.ledger_hidden_lines(80), call.hidden_lines());
                assert_eq!(call.ledger_expandable(80), call.expandable());
            }
            DocBlockKind::ToolResult(result) => {
                assert_eq!(result.ledger_hidden_lines(80), result.hidden_lines());
                assert_eq!(result.ledger_expandable(80), result.expandable());
            }
            _ => {}
        }
    }

    doc
}

const MODES: [ToolDisplayMode; 3] = [
    ToolDisplayMode::Hidden,
    ToolDisplayMode::Truncated,
    ToolDisplayMode::Full,
];

/// Parity for every mode / thinking / timing combination, collapsed, with
/// each toggle id expanded on its own, and with all of them expanded.
fn assert_parity_everywhere(entries: &[RenderableEntry]) {
    for tool_display in MODES {
        for show_thinking in [false, true] {
            for show_timing in [false, true] {
                let prefs = ViewPrefs {
                    tool_display,
                    show_thinking,
                    show_timing,
                };
                let collapsed = assert_ledger_parity(entries, &prefs, &BTreeSet::new());
                let ids = doc_toggle_ids(&collapsed);
                for id in &ids {
                    assert_ledger_parity(entries, &prefs, &BTreeSet::from([id.clone()]));
                }
                let expanded = assert_ledger_parity(entries, &prefs, &ids);
                // Expanding everything reveals nested ids (summary details).
                assert_ledger_parity(entries, &prefs, &doc_toggle_ids(&expanded));
            }
        }
    }
}

// -----------------------------------------------------------------
// Parity sweeps
// -----------------------------------------------------------------

#[test]
fn parity_with_ledger_for_all_fixtures_and_preferences() {
    let fixtures: Vec<Vec<RenderableEntry>> = vec![
        tool_summary_entries(),
        pi_thinking_entries(),
        subagent_entries(),
        vec![reordered_blocks_entry()],
        agent_progress_entries(),
        mixed_entries(),
        vec![
            user_entry(0, "Hello", None),
            assistant_text_entry(1, "Hi there", None),
        ],
    ];
    for entries in fixtures {
        assert_parity_everywhere(&entries);
    }
}

#[test]
fn parity_with_ledger_for_pi_and_omp_fixture_files() {
    for path in [
        "tests/fixtures/pi/v1.jsonl",
        "tests/fixtures/pi/v2.jsonl",
        "tests/fixtures/pi/v3-branched.jsonl",
        "tests/fixtures/omp/v3.jsonl",
    ] {
        assert_parity_everywhere(&fixture_entries(path));
    }
}

// -----------------------------------------------------------------
// Ports of the ledger tests (plan 6.1, `tui/viewer/tests.rs`)
// -----------------------------------------------------------------

#[test]
fn hidden_tool_mode_renders_activity_summary() {
    let entries = [entry(
        0,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"one"}},{"type":"tool_use","id":"toolu_2","name":"Grep","input":{"pattern":"two"}},{"type":"tool_use","id":"toolu_3","name":"Read","input":{"file_path":"src/main.rs"}}]}}"#,
    )];
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));

    assert_eq!(doc.messages.len(), 1);
    let message = &doc.messages[0];
    assert_eq!(message.kind, DocMessageKind::ToolSummaryGroup);
    assert_eq!(
        message.blocks[0].kind,
        DocBlockKind::ToolSummary {
            id: Some(make_tool_summary_output_id(0, None)),
            sentence: "Searched for 2 patterns, read 1 file".to_string(),
        }
    );
    let text = doc_text(&doc);
    assert!(!text.contains("Grep:"));
    assert!(!text.contains("Read:"));
}

#[test]
fn hidden_tool_mode_coalesces_tool_only_entries_across_results() {
    let doc = build(&tool_summary_entries(), &prefs(ToolDisplayMode::Hidden));

    assert_eq!(doc.messages.len(), 1);
    let message = &doc.messages[0];
    assert_eq!(message.label, "Claude");
    assert_eq!(message.label_role, LabelRole::AccentDim);
    assert!(!message.bold && !message.dimmed && message.navigable);
    assert_eq!(message.entry_index, 0);
    assert_eq!(
        message.blocks[0].plain_text(),
        "Searched for 1 pattern, read 1 file, ran 1 shell command"
    );
    assert!(!doc_text(&doc).contains("Result"));
}

#[test]
fn collapsed_tool_summary_uses_fixed_timestamp_format() {
    let timestamp = Local.with_ymd_and_hms(2026, 9, 4, 13, 59, 0).unwrap();
    let entries = [entry(
        0,
        &format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"Bash","input":{{"command":"pwd"}}}}]}}}}"#,
            timestamp.to_rfc3339()
        ),
    )];
    let mut prefs = prefs(ToolDisplayMode::Hidden);
    prefs.show_timing = true;

    let doc = assert_ledger_parity(&entries, &prefs, &BTreeSet::new());
    assert_eq!(doc.messages[0].timestamp.as_deref(), Some("Sep 04 13:59"));
    assert_eq!(
        doc.messages[0].blocks[0].timing,
        DocTiming::Stamp("Sep 04 13:59".to_string())
    );
    assert_eq!(
        doc.messages[0].raw_timestamp.as_deref(),
        Some(timestamp.to_rfc3339().as_str())
    );
}

#[test]
fn tool_summary_uses_source_agent_label() {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"pwd"}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"result"}]}}"#,
        ),
    ];
    let summary_id = make_tool_summary_output_id(0, None);

    let collapsed = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert_eq!(collapsed.messages.len(), 1);
    assert_eq!(collapsed.messages[0].label, "Pi");
    assert_eq!(
        collapsed.messages[0].blocks[0].plain_text(),
        "Called 1 tool"
    );

    let expanded = build_document(
        &entries,
        &prefs(ToolDisplayMode::Hidden),
        &BTreeSet::from([summary_id.clone()]),
    );
    assert_eq!(expanded.messages[0].label, "Pi");
    let DocBlockKind::ToolSummaryDetails { id, items } = &expanded.messages[0].blocks[0].kind
    else {
        panic!("expected details: {expanded:#?}");
    };
    assert_eq!(id, &summary_id);
    assert_eq!(items.len(), 2);
    assert!(!doc_text(&expanded).contains("Claude"));
}

#[test]
fn hidden_pi_thinking_allows_clickable_grouped_tool_summary() {
    let entries = pi_thinking_entries();
    let summary_id = make_tool_summary_output_id(0, None);

    let collapsed = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert_eq!(collapsed.messages.len(), 1);
    assert_eq!(collapsed.messages[0].label, "Pi");
    assert_eq!(
        collapsed.messages[0].blocks[0].toggle_id(),
        Some(&summary_id)
    );
    assert_eq!(
        collapsed.messages[0].blocks[0].plain_text(),
        "Called 2 tools"
    );

    let expanded = build_document(
        &entries,
        &prefs(ToolDisplayMode::Hidden),
        &BTreeSet::from([summary_id]),
    );
    let text = doc_text(&expanded);
    assert!(text.contains("pwd"));
    assert!(text.contains("git status"));
    assert!(text.contains("/tmp/project"));
    assert!(text.contains("clean"));
    // Thinking is never part of an expanded group.
    assert!(!text.contains("inspect"));
}

#[test]
fn expanded_tool_summary_renders_truncated_details() {
    let entries = tool_summary_entries();
    let summary_id = make_tool_summary_output_id(0, None);
    let doc = build_document(
        &entries,
        &prefs(ToolDisplayMode::Hidden),
        &BTreeSet::from([summary_id.clone()]),
    );

    assert_eq!(doc.messages.len(), 1);
    let DocBlockKind::ToolSummaryDetails { id, items } = &doc.messages[0].blocks[0].kind else {
        panic!("expected details: {doc:#?}");
    };
    assert_eq!(id, &summary_id);
    let headers: Vec<String> = items.iter().map(DocBlock::plain_text).collect();
    assert_eq!(
        headers,
        [
            "Grep: \"one\" in .",
            "grep result",
            "Read: src/main.rs",
            "Bash: cargo test",
            "bash result",
        ]
    );
    for item in items {
        match &item.kind {
            DocBlockKind::ToolCall(call) => {
                assert!(call.dimmed);
                assert_eq!(call.fold, Fold::Collapsed { limit: 3 });
            }
            DocBlockKind::ToolResult(result) => {
                assert_eq!(result.fold, Fold::Collapsed { limit: 4 });
            }
            other => panic!("unexpected item {other:?}"),
        }
    }
    assert!(!doc_text(&doc).contains("Searched for 1 pattern"));
}

#[test]
fn subagent_summary_label_parity() {
    let entries = subagent_entries();
    let summary_id = make_tool_summary_output_id(0, Some("toolu_parent_abc"));
    let mut prefs = prefs(ToolDisplayMode::Hidden);
    prefs.show_thinking = true;

    let collapsed = build(&entries, &prefs);
    assert_eq!(collapsed.messages.len(), 1);
    let message = &collapsed.messages[0];
    assert_eq!(message.label, "↳parent_");
    assert!(message.dimmed && message.is_subagent);
    assert_eq!(message.blocks[0].toggle_id(), Some(&summary_id));

    let expanded = build_document(&entries, &prefs, &BTreeSet::from([summary_id]));
    assert_eq!(expanded.messages[0].label, "↳parent_");
    let text = doc_text(&expanded);
    assert!(text.contains("Grep: \"one\" in ."));
    assert!(!text.contains("Claude"));
}

#[test]
fn tool_output_ids_use_stable_literal_format() {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","parent_tool_use_id":"toolu_parent","message":{"role":"user","content":[{"type":"text","text":"x"},{"type":"text","text":"y"},{"type":"tool_result","tool_use_id":"toolu_2","content":"r"}]}}"#,
        ),
    ];
    let mut prefs = prefs(ToolDisplayMode::Truncated);
    prefs.show_thinking = true;
    let doc = build(&entries, &prefs);
    let ids: Vec<&str> = all_blocks(&doc)
        .into_iter()
        .filter_map(|block| block.tool_output_id())
        .map(|id| id.0.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "entry:0:parent:top:block:0:kind:call:id:toolu_1",
            "entry:1:parent:toolu_parent:block:2:kind:result:id:toolu_2",
        ]
    );

    let grouped = build(&entries[..1], &self::prefs(ToolDisplayMode::Hidden));
    assert_eq!(
        grouped.messages[0].blocks[0].tool_output_id().unwrap().0,
        "entry:0:parent:top:kind:summary"
    );
}

#[test]
fn parse_conversation_file_preserves_entry_indices() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conversation.jsonl");
    std::fs::write(
        &path,
        concat!(
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"first"}}"#,
            "\n",
            "not json\n",
            r#"{"type":"file-history-snapshot","messageId":"m1","snapshot":{},"isSnapshotUpdate":false}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"second"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let entries = parse_conversation_file(&path).unwrap();
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));

    let indices: Vec<usize> = doc.messages.iter().map(|m| m.entry_index).collect();
    assert_eq!(indices, [0, 2]);
    // The same index space as the export copy of a single message.
    let options = crate::export::ExportOptions::default();
    assert_eq!(
        crate::export::extract_message_text(&path, 2, options).unwrap(),
        "second"
    );
}

#[test]
fn show_thinking_controls_subagent_entries() {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","parent_tool_use_id":"toolu_parent","message":{"role":"assistant","content":[{"type":"text","text":"subagent text"}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef123456","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"agent progress text"}]}}}}"#,
        ),
    ];
    let hidden = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert!(hidden.messages.is_empty());

    let mut prefs = prefs(ToolDisplayMode::Hidden);
    prefs.show_thinking = true;
    let shown = build(&entries, &prefs);
    assert_eq!(shown.messages.len(), 2);
    assert_eq!(shown.messages[0].kind, DocMessageKind::Assistant);
    assert_eq!(shown.messages[0].label, "↳parent");
    assert!(shown.messages[0].is_subagent && shown.messages[0].dimmed);
    assert_eq!(
        shown.messages[1].kind,
        DocMessageKind::AgentProgressAssistant
    );
    assert_eq!(shown.messages[1].label, "↳agent-a");
    let text = doc_text(&shown);
    assert!(text.contains("subagent text"));
    assert!(text.contains("agent progress text"));
}

#[test]
fn tool_call_metadata_tracks_truncated_and_expanded_state() {
    let entries = [entry(
        0,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"one\ntwo\nthree\nfour\nfive"}}]}}"#,
    )];
    let id = make_tool_output_id(0, None, 0, ToolOutputKind::ToolCall, Some("toolu_1"));

    let collapsed = build(&entries, &prefs(ToolDisplayMode::Truncated));
    let DocBlockKind::ToolCall(call) = &collapsed.messages[0].blocks[0].kind else {
        panic!("expected a tool call: {collapsed:#?}");
    };
    assert_eq!(call.id, id);
    assert_eq!(call.header, "Bash: one");
    assert_eq!(call.total_lines(), 4);
    assert_eq!(call.hidden_lines(), Some(1));
    assert!(call.expandable() && !call.expanded());
    let visible: Vec<&str> = call
        .visible_body_lines()
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(visible, ["two", "three", "four"]);
    assert!(
        !collapsed.messages[0].blocks[0]
            .plain_text()
            .contains("five")
    );

    let expanded = build_document(
        &entries,
        &prefs(ToolDisplayMode::Truncated),
        &BTreeSet::from([id]),
    );
    let DocBlockKind::ToolCall(call) = &expanded.messages[0].blocks[0].kind else {
        panic!("expected a tool call");
    };
    assert_eq!(call.hidden_lines(), None);
    assert!(call.expandable() && call.expanded());
    assert!(expanded.messages[0].blocks[0].plain_text().contains("five"));
}

#[test]
fn full_tool_mode_blocks_are_not_expandable() {
    let entries = [entry(
        0,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"one\ntwo\nthree\nfour\nfive"}}]}}"#,
    )];
    let doc = build(&entries, &prefs(ToolDisplayMode::Full));
    assert!(doc_toggle_ids(&doc).is_empty());
    assert!(doc_text(&doc).contains("five"));
    assert!(doc_hidden_counts(&doc).is_empty());
}

#[test]
fn truncated_tool_call_carries_expected_tool_output_id() {
    let entries = [entry(
        7,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_xyz","name":"Bash","input":{"command":"ls"}}]}}"#,
    )];
    let doc = build(&entries, &prefs(ToolDisplayMode::Truncated));
    assert_eq!(
        doc.messages[0].blocks[0].tool_output_id().unwrap().0,
        "entry:7:parent:top:block:0:kind:call:id:toolu_xyz"
    );
    // A short call has nothing to expand.
    assert_eq!(doc.messages[0].blocks[0].toggle_id(), None);
}

#[test]
fn tool_result_string_content_renders_as_text() {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"hello-world-output"}]}}"#,
        ),
    ];
    let doc = build(&entries, &prefs(ToolDisplayMode::Truncated));
    assert_eq!(doc.messages.len(), 2);
    let DocBlockKind::ToolResult(result) = &doc.messages[1].blocks[0].kind else {
        panic!("expected a result: {doc:#?}");
    };
    assert_eq!(result.text, "hello-world-output");
    assert_eq!(result.markdown, "```text\nhello-world-output\n```");
    assert!(result.is_fenced());
    assert_eq!(doc.messages[1].label, "You");
}

fn single_result_document(content: &str, prefs: &ViewPrefs, expanded: bool) -> DocToolResult {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cat big"}}]}}"#,
        ),
        entry(
            1,
            &serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": content}
                ]}
            })
            .to_string(),
        ),
    ];
    let collapsed = build(&entries, prefs);
    let DocBlockKind::ToolResult(result) = &collapsed.messages[1].blocks[0].kind else {
        panic!("expected a result: {collapsed:#?}");
    };
    if !expanded {
        return result.clone();
    }
    let ids = BTreeSet::from([result.id.clone()]);
    let doc = build_document(&entries, prefs, &ids);
    let DocBlockKind::ToolResult(result) = &doc.messages[1].blocks[0].kind else {
        panic!("expected a result: {doc:#?}");
    };
    result.clone()
}

#[test]
fn ledger_visible_rows_truncate_a_huge_single_line_by_visual_rows() {
    // K9: a 50 KB single-line result counts as one unwrapped row, but the
    // ledger hard-wraps it; collapsed it shows at most 4 wrapped rows.
    let line = "0123456789abcdefghij".repeat(2_500);
    assert_eq!(line.len(), 50_000);
    let truncated = prefs(ToolDisplayMode::Truncated);
    let result = single_result_document(&line, &truncated, false);
    assert_eq!(result.rows, 1);
    assert_eq!(result.fold, Fold::Collapsed { limit: 4 });
    for width in [40, 120] {
        let rows = result.ledger_visible_rows(width);
        assert_eq!(rows.len(), 4, "width {width}");
        assert!(rows.iter().all(|row| row.chars().count() <= width));
        assert_eq!(rows.concat(), line[..rows.concat().len()]);
        assert!(rows.concat().len() >= 4 * (width - 1));
        assert_eq!(
            result.ledger_hidden_lines(width),
            Some(result.ledger_lines(width) - 4)
        );
    }
    // Expanded and full mode: every wrapped row.
    let expanded = single_result_document(&line, &truncated, true);
    assert_eq!(expanded.fold, Fold::Expanded);
    for width in [40, 120] {
        let rows = expanded.ledger_visible_rows(width);
        assert_eq!(rows.len(), expanded.ledger_lines(width));
        assert_eq!(rows.concat(), line);
    }
    let full = single_result_document(&line, &prefs(ToolDisplayMode::Full), false);
    assert_eq!(full.ledger_visible_rows(120).concat(), line);
}

#[test]
fn ledger_visible_rows_follow_the_ledger_rows_of_short_results() {
    let text = "one\ntwo\nthree\nfour\nfive\nsix";
    let result = single_result_document(text, &prefs(ToolDisplayMode::Truncated), false);
    for width in [40, 120] {
        assert_eq!(
            result.ledger_visible_rows(width),
            vec!["one", "two", "three", "four"]
        );
    }
    let short = single_result_document("a\nb", &prefs(ToolDisplayMode::Truncated), false);
    assert_eq!(short.ledger_visible_rows(40), vec!["a", "b"]);
    // A long line among short ones wraps at the width.
    let mixed = format!("short\n{}\nend", "x".repeat(100));
    let result = single_result_document(&mixed, &prefs(ToolDisplayMode::Truncated), false);
    let rows = result.ledger_visible_rows(40);
    assert_eq!(rows[0], "short");
    assert_eq!(rows.len(), 4);
    assert!(rows[1..].iter().all(|row| row.chars().all(|c| c == 'x')));
    assert_eq!(result.ledger_visible_rows(120).len(), 3);
}

#[test]
fn message_ranges_track_user_and_assistant_entries() {
    let entries = [
        user_entry(0, "Hello", None),
        assistant_text_entry(1, "Hi there", None),
    ];
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert_eq!(doc.messages.len(), 2);
    assert_eq!(doc.messages[0].entry_index, 0);
    assert_eq!(doc.messages[0].kind, DocMessageKind::User);
    assert_eq!(doc.messages[1].entry_index, 1);
    assert_eq!(doc.messages[1].kind, DocMessageKind::Assistant);
    assert!(doc.messages.iter().all(|m| m.navigable));
    assert_eq!(doc.navigable_indices(), [0, 1]);
}

#[test]
fn message_ranges_skip_non_message_entries() {
    let entries = [
        entry(0, r#"{"type":"summary","summary":"ignored"}"#),
        user_entry(1, "Hello", None),
    ];
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert_eq!(doc.messages.len(), 1);
    assert_eq!(doc.messages[0].entry_index, 1);
}

#[test]
fn timing_enabled_stamps_first_block_and_disabled_omits_it() {
    let timestamp = Local.with_ymd_and_hms(2026, 9, 4, 12, 34, 56).unwrap();
    let entries = [user_entry(0, "Hello", Some(&timestamp.to_rfc3339()))];
    let mut timed = prefs(ToolDisplayMode::Hidden);
    timed.show_timing = true;

    let doc = build(&entries, &timed);
    assert_eq!(doc.messages[0].timestamp.as_deref(), Some("Sep 04 12:34"));
    assert_eq!(
        doc.messages[0].blocks[0].timing,
        DocTiming::Stamp("Sep 04 12:34".to_string())
    );

    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert_eq!(doc.messages[0].timestamp, None);
    assert_eq!(doc.messages[0].blocks[0].timing, DocTiming::Disabled);
    assert!(doc.messages[0].raw_timestamp.is_some());
}

#[test]
fn invalid_timestamp_disables_timing_column() {
    let entries = [user_entry(0, "Hello", Some("not-a-timestamp"))];
    let mut prefs = prefs(ToolDisplayMode::Hidden);
    prefs.show_timing = true;
    let doc = assert_ledger_parity(&entries, &prefs, &BTreeSet::new());
    assert_eq!(doc.messages[0].timestamp, None);
    assert_eq!(doc.messages[0].blocks[0].timing, DocTiming::Disabled);
    assert_eq!(
        doc.messages[0].raw_timestamp.as_deref(),
        Some("not-a-timestamp")
    );
}

#[test]
fn timing_slots_follow_row_timing_semantics() {
    // Second assistant text block: no column (upstream quirk); later tool
    // rows pad; thinking pads; the next entry's result consumes its own stamp.
    let entries = mixed_entries();
    let prefs = ViewPrefs {
        tool_display: ToolDisplayMode::Truncated,
        show_thinking: true,
        show_timing: true,
    };
    let doc = assert_ledger_parity(&entries, &prefs, &BTreeSet::new());
    let assistant = &doc.messages[1];
    let stamp = assistant.timestamp.clone().unwrap();
    let slots: Vec<&DocTiming> = assistant.blocks.iter().map(|b| &b.timing).collect();
    assert_eq!(
        slots,
        [
            &DocTiming::Stamp(stamp),
            &DocTiming::Disabled,
            &DocTiming::Pad,
            &DocTiming::Pad,
            &DocTiming::Pad,
        ]
    );
    let results = &doc.messages[2];
    assert_eq!(
        results.blocks[0].timing.stamp(),
        results.timestamp.as_deref()
    );
    assert_eq!(results.blocks[1].timing, DocTiming::Pad);
    // Dimmed blocks never carry their stamp.
    let subagent = &doc.messages[3];
    assert!(subagent.timestamp.is_some());
    assert!(
        subagent
            .blocks
            .iter()
            .all(|block| block.timing == DocTiming::Pad)
    );
}

#[test]
fn user_and_assistant_label_roles() {
    let doc = build(
        &[
            user_entry(0, "Hello", None),
            assistant_text_entry(1, "Hi", None),
        ],
        &prefs(ToolDisplayMode::Hidden),
    );
    assert_eq!(doc.messages[0].label, "You");
    assert_eq!(doc.messages[0].label_role, LabelRole::TextPrimary);
    assert!(doc.messages[0].bold && !doc.messages[0].dimmed);
    assert_eq!(doc.messages[1].label, "Claude");
    assert_eq!(doc.messages[1].label_role, LabelRole::Accent);
    assert!(doc.messages[1].bold);
    assert_eq!(
        LabelRole::Accent.color(theme::current()),
        theme::current().accent
    );
}

#[test]
fn assistant_block_order_hidden_mode_text_then_summary_then_thinking() {
    let mut prefs = prefs(ToolDisplayMode::Hidden);
    prefs.show_thinking = true;
    let doc = build(&[reordered_blocks_entry()], &prefs);
    let kinds: Vec<String> = doc.messages[0]
        .blocks
        .iter()
        .map(|block| block.plain_text())
        .collect();
    assert_eq!(kinds, ["TEXT_BLOCK", "Ran 1 shell command", "THINK_BLOCK"]);
    // A mixed text + tools message has a non-clickable sentence.
    assert_eq!(doc.messages[0].blocks[1].toggle_id(), None);
}

#[test]
fn assistant_block_order_truncated_mode_text_then_tools_then_thinking() {
    let mut prefs = prefs(ToolDisplayMode::Truncated);
    prefs.show_thinking = true;
    let doc = build(&[reordered_blocks_entry()], &prefs);
    let blocks = &doc.messages[0].blocks;
    assert!(matches!(&blocks[0].kind, DocBlockKind::Markdown { text } if text == "TEXT_BLOCK"));
    assert!(
        matches!(&blocks[1].kind, DocBlockKind::ToolCall(call) if call.header == "Bash: echo hi")
    );
    assert!(matches!(&blocks[2].kind, DocBlockKind::Thinking { text } if text == "THINK_BLOCK"));
}

#[test]
fn assistant_block_order_full_mode_text_then_tools_then_thinking() {
    let mut prefs = prefs(ToolDisplayMode::Full);
    prefs.show_thinking = true;
    let doc = build(&[reordered_blocks_entry()], &prefs);
    let texts: Vec<String> = doc.messages[0]
        .blocks
        .iter()
        .map(DocBlock::plain_text)
        .collect();
    assert_eq!(texts, ["TEXT_BLOCK", "Bash: echo hi", "THINK_BLOCK"]);
}

#[test]
fn assistant_template_order_is_text_then_tool_then_thinking() {
    let entries = [entry(
        0,
        r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"deep thought","signature":"sig"},
            {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}},
            {"type":"text","text":"text reply"}
        ]}}"#,
    )];
    let mut prefs = prefs(ToolDisplayMode::Truncated);
    prefs.show_thinking = true;
    let doc = build(&entries, &prefs);
    let texts: Vec<String> = doc.messages[0]
        .blocks
        .iter()
        .map(DocBlock::plain_text)
        .collect();
    assert_eq!(texts, ["text reply", "Bash: ls", "deep thought"]);
}

#[test]
fn skill_marker_user_message_renders_dimmed_but_top_level() {
    let entries = [entry(
        0,
        r#"{"type":"user","message":{"role":"user","content":"Base directory for this skill: /tmp/x"}}"#,
    )];
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));
    let message = &doc.messages[0];
    assert_eq!(message.label, "You");
    assert!(message.dimmed);
    assert!(!message.bold);
    assert!(!message.is_subagent);
    assert_eq!(
        message.blocks[0].kind,
        DocBlockKind::Markdown {
            text: "*Skill: invoked*".to_string()
        }
    );
}

#[test]
fn dimmed_skill_message_keeps_its_tool_results_un_muted_like_the_ledger() {
    // `DocMessage::dimmed` mutes the label and the message's own text, not
    // every block: a top-level ToolResult keeps the normal style, exactly
    // as the ledger's render_tool_result draws it.
    let entries = [entry(
        0,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Base directory for this skill: /tmp/x"},{"type":"tool_result","tool_use_id":"toolu_1","content":"result line"}]}}"#,
    )];
    let prefs = prefs(ToolDisplayMode::Full);
    let doc = assert_ledger_parity(&entries, &prefs, &BTreeSet::new());
    let message = &doc.messages[0];
    assert!(message.dimmed && !message.is_subagent, "{doc:#?}");
    assert!(matches!(
        message.blocks[0].kind,
        DocBlockKind::Markdown { .. }
    ));
    assert!(
        message
            .blocks
            .iter()
            .any(|block| matches!(&block.kind, DocBlockKind::ToolResult(result) if result.text == "result line")),
        "{doc:#?}"
    );

    let rendered =
        render_parsed_conversation(&entries, &prefs.render_options(80, &BTreeSet::new()));
    // The message's own text row is muted...
    assert!(name_span(&rendered.lines[0]).1.dimmed);
    // ...but the result row is not: neither its label nor its text.
    let result_row = rendered
        .lines
        .iter()
        .find(|line| {
            line.spans
                .iter()
                .any(|(text, _)| text.contains("result line"))
        })
        .expect("result row");
    let (label, label_style) = name_span(result_row);
    assert!(label.contains("Result"), "{label:?}");
    assert!(!label_style.dimmed);
    assert!(
        result_row
            .spans
            .iter()
            .filter(|(text, _)| text.contains("result line"))
            .all(|(_, style)| !style.dimmed)
    );
}

#[test]
fn agent_progress_user_with_text_and_result_keeps_template_order() {
    let mut prefs = prefs(ToolDisplayMode::Truncated);
    prefs.show_thinking = true;
    let doc = build(&agent_progress_entries()[..1], &prefs);
    let message = &doc.messages[0];
    assert_eq!(message.kind, DocMessageKind::AgentProgressUser);
    assert_eq!(message.label, "↳agent-a");
    assert_eq!(message.label_role, LabelRole::TextPrimary);
    assert_eq!(message.timestamp, None);
    assert!(
        matches!(&message.blocks[0].kind, DocBlockKind::Markdown { text } if text == "agent says hi")
    );
    let DocBlockKind::SubagentToolResult(result) = &message.blocks[1].kind else {
        panic!("expected a subagent result: {doc:#?}");
    };
    // Subagent results are the raw content as pretty JSON.
    assert_eq!(result.text, "\"result body\"");
    assert_eq!(
        result.id.0,
        "entry:0:parent:agent-abc1234:block:1:kind:result:id:toolu_1"
    );
}

#[test]
fn agent_progress_assistant_has_summary_calls_and_no_thinking() {
    let mut prefs = prefs(ToolDisplayMode::Hidden);
    prefs.show_thinking = true;
    let doc = build(&agent_progress_entries(), &prefs);
    // Agent-progress entries are never grouped.
    assert_eq!(doc.messages.len(), 3);
    let tools = &doc.messages[1];
    assert_eq!(tools.kind, DocMessageKind::AgentProgressAssistant);
    assert_eq!(
        tools.blocks[0].kind,
        DocBlockKind::ToolSummary {
            id: None,
            sentence: "Read 1 file, ran 1 shell command".to_string(),
        }
    );
    let text_only = &doc.messages[2];
    assert_eq!(text_only.blocks.len(), 1);
    assert!(!doc_text(&doc).contains("hidden thought"));

    prefs.tool_display = ToolDisplayMode::Truncated;
    let doc = build(&agent_progress_entries(), &prefs);
    let DocBlockKind::ToolCall(call) = &doc.messages[1].blocks[1].kind else {
        panic!("expected a tool call: {doc:#?}");
    };
    assert_eq!(
        call.id.0,
        "entry:1:parent:agent-abc1234:block:1:kind:call:id:toolu_3"
    );
    assert!(call.dimmed);
    assert_eq!(call.hidden_lines(), Some(1));
}

#[test]
fn agent_progress_keeps_raw_timestamp_but_never_stamps() {
    let entries = [entry(
        0,
        r#"{"type":"progress","timestamp":"2026-09-04T12:34:56Z","data":{"type":"agent_progress","agentId":"agent-1","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}}}"#,
    )];
    let prefs = ViewPrefs {
        tool_display: ToolDisplayMode::Hidden,
        show_thinking: true,
        show_timing: true,
    };
    let doc = assert_ledger_parity(&entries, &prefs, &BTreeSet::new());
    let message = &doc.messages[0];
    assert_eq!(message.timestamp, None);
    assert_eq!(
        message.raw_timestamp.as_deref(),
        Some("2026-09-04T12:34:56Z")
    );
    assert_eq!(message.blocks[0].timing, DocTiming::Pad);
}

#[test]
fn excluded_entry_kinds_produce_no_messages() {
    let entries = [
        entry(0, r#"{"type":"summary","summary":"x"}"#),
        entry(1, r#"{"type":"system","subtype":"info","message":"sys"}"#),
        entry(2, r#"{"type":"custom-title","customTitle":"t"}"#),
        entry(3, r#"{"type":"ai-title","aiTitle":"t"}"#),
        entry(4, r#"{"type":"agent-name","agentName":"n"}"#),
        entry(5, r#"{"type":"permission-mode","mode":"plan"}"#),
        entry(
            6,
            r#"{"type":"pi-metadata","label":"Model","text":"gpt","searchable":false}"#,
        ),
        entry(7, r#"{"type":"something-new"}"#),
    ];
    for tool_display in MODES {
        let doc = build(
            &entries,
            &ViewPrefs {
                tool_display,
                show_thinking: true,
                show_timing: true,
            },
        );
        assert!(doc.messages.is_empty());
    }
}

#[test]
fn pending_summary_flushes_at_eof() {
    let entries = [entry(
        0,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"x"}}]}}"#,
    )];
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert_eq!(doc.messages.len(), 1);
    assert_eq!(doc.messages[0].entry_index, 0);
    assert_eq!(
        doc.messages[0].blocks[0].plain_text(),
        "Searched for 1 pattern"
    );
}

#[test]
fn pending_summary_flushes_before_non_tool_message() {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"x"}}]}}"#,
        ),
        user_entry(1, "follow up", None),
    ];
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));
    let indices: Vec<usize> = doc.messages.iter().map(|m| m.entry_index).collect();
    assert_eq!(indices, [0, 1]);
    assert_eq!(doc.messages[0].kind, DocMessageKind::ToolSummaryGroup);
    assert_eq!(doc.messages[1].blocks[0].plain_text(), "follow up");
}

// -----------------------------------------------------------------
// Document-specific behavior
// -----------------------------------------------------------------

#[test]
fn groups_split_on_parent_or_agent_change_and_absorb_results() {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"a","name":"Read","input":{}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"tool_use","id":"b","name":"Read","input":{}}]}}"#,
        ),
        entry(
            2,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b","content":"x"}]}}"#,
        ),
        entry(
            3,
            r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"tool_use","id":"c","name":"Write","input":{}}]}}"#,
        ),
    ];
    let doc = assert_ledger_parity(&entries, &prefs(ToolDisplayMode::Hidden), &BTreeSet::new());
    let summary: Vec<(usize, &str, String)> = doc
        .messages
        .iter()
        .map(|m| (m.entry_index, m.label.as_str(), m.blocks[0].plain_text()))
        .collect();
    assert_eq!(
        summary,
        [
            (0, "Claude", "Read 1 file".to_string()),
            (1, "Pi", "Read 1 file, wrote 1 file".to_string()),
        ]
    );
}

#[test]
fn user_text_blocks_are_command_processed_and_joined() {
    let doc = build(&mixed_entries(), &prefs(ToolDisplayMode::Hidden));
    let texts: Vec<(usize, String)> = doc
        .messages
        .iter()
        .filter(|m| m.kind == DocMessageKind::User)
        .map(|m| (m.entry_index, m.blocks[0].plain_text()))
        .collect();
    assert_eq!(
        texts,
        [
            (0, "Hello there".to_string()),
            (7, "Skill: Do things.".to_string()),
            (14, "out".to_string()),
            (16, "/model opus\n\ntail".to_string()),
        ]
    );
    // The caveat-only message, the image-only message and the blank
    // assistant text render nothing; hidden Pi metadata is skipped.
    for skipped in [6, 8, 9, 15] {
        assert!(doc.messages.iter().all(|m| m.entry_index != skipped));
    }
    let metadata = doc.messages.iter().find(|m| m.entry_index == 5).unwrap();
    assert_eq!(metadata.kind, DocMessageKind::PiMetadata);
    assert_eq!(metadata.label, "notice");
    assert_eq!(metadata.label_role, LabelRole::TextSecondary);
    assert!(metadata.dimmed && !metadata.navigable);
}

#[test]
fn tool_bodies_classify_diff_lines() {
    let doc = build(&mixed_entries(), &prefs(ToolDisplayMode::Full));
    let edit = all_blocks(&doc)
        .into_iter()
        .find_map(|block| match &block.kind {
            DocBlockKind::ToolCall(call) if call.name == "Edit" => Some(call),
            _ => None,
        })
        .unwrap();
    let body: Vec<(BodyLineKind, &str)> = edit
        .body
        .as_ref()
        .unwrap()
        .lines
        .iter()
        .map(|line| (line.kind, line.text.as_str()))
        .collect();
    assert_eq!(
        body,
        [
            (BodyLineKind::Remove, "- one"),
            (BodyLineKind::Remove, "- two"),
            (BodyLineKind::Add, "+ uno"),
            (BodyLineKind::Add, "+ dos"),
            (BodyLineKind::Add, "+ tres"),
        ]
    );
    assert_eq!(edit.fold, Fold::Full);
    assert!(!edit.dimmed);
}

#[test]
fn truncation_counts_unwrapped_rows_and_ledger_helpers_count_wrapped_rows() {
    // One long line: a single source line, but several ledger rows at a
    // narrow width (and a wrapped Bash command gains body rows).
    let long = "word ".repeat(40);
    let entries = [
        entry(
            0,
            &format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"{}"}}}}]}}}}"#,
                long.trim()
            ),
        ),
        entry(
            1,
            &format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"{}"}}]}}}}"#,
                long.trim()
            ),
        ),
    ];
    let doc = build(&entries, &prefs(ToolDisplayMode::Truncated));
    let DocBlockKind::ToolCall(call) = &doc.messages[0].blocks[0].kind else {
        panic!("expected a call");
    };
    assert_eq!(call.header, format!("Bash: {}", long.trim()));
    assert_eq!(call.total_lines(), 0);
    assert_eq!(call.hidden_lines(), None);
    assert!(!call.expandable());
    // At 40 columns the ledger wraps the command at 34 into 6 rows: the
    // header plus 5 body rows, 2 of them hidden.
    assert_eq!(call.ledger_body_lines(40), 5);
    assert_eq!(call.ledger_hidden_lines(40), Some(2));
    assert!(call.ledger_expandable(40));

    let DocBlockKind::ToolResult(result) = &doc.messages[1].blocks[0].kind else {
        panic!("expected a result");
    };
    assert_eq!(result.total_lines(), 1);
    assert_eq!(result.hidden_lines(), None);
    assert!(!result.expandable());
    assert_eq!(result.ledger_lines(40), 5);
    assert_eq!(result.ledger_hidden_lines(40), Some(1));
    assert!(result.ledger_expandable(40));

    // The ledger agrees with the helpers at that width.
    let rendered = render_parsed_conversation(
        &entries,
        &prefs(ToolDisplayMode::Truncated).render_options(40, &BTreeSet::new()),
    );
    assert_eq!(indicator_counts(&rendered), [2, 1]);
}

#[test]
fn result_visible_text_and_markdown_follow_the_fold() {
    let doc = build(&mixed_entries(), &prefs(ToolDisplayMode::Truncated));
    let results: Vec<&DocToolResult> = all_blocks(&doc)
        .into_iter()
        .filter_map(|block| match &block.kind {
            DocBlockKind::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    let long = results[0];
    assert_eq!(long.total_lines(), 6);
    assert_eq!(long.hidden_lines(), Some(2));
    assert_eq!(long.visible_text(), "l1\nl2\nl3\nl4");
    assert_eq!(long.visible_markdown(), "```text\nl1\nl2\nl3\nl4\n```");
    let short = results[1];
    assert_eq!(short.text, "short");
    assert!(!short.expandable());

    // Subagent results count unwrapped lines, like the ledger.
    let mut thinking = prefs(ToolDisplayMode::Truncated);
    thinking.show_thinking = true;
    let doc = build(&mixed_entries(), &thinking);
    let subagent: Vec<&DocSubagentToolResult> = all_blocks(&doc)
        .into_iter()
        .filter_map(|block| match &block.kind {
            DocBlockKind::SubagentToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(
        subagent[0].text,
        "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ]\n}"
    );
    assert_eq!(subagent[0].hidden_lines(), Some(3));
    assert_eq!(
        subagent[0].visible_text(),
        "{\n  \"a\": 1,\n  \"b\": [\n    1,"
    );
    assert_eq!(subagent[1].text, "\"r1\\nr2\\nr3\\nr4\\nr5\"");
    assert_eq!(subagent[1].hidden_lines(), None);
}

#[test]
fn markdown_plain_text_hides_markup_like_the_ledger() {
    assert_eq!(
        markdown_plain_text(
            "# Title\n\nSome **bold** [link](http://x.y) and `code`.\n\n```rust\nlet x = 1;\n```"
        ),
        "Title\n\nSome bold link and code.\n\nlet x = 1;"
    );
    assert_eq!(markdown_plain_text("- a\n- b"), "- a\n- b");
}

#[test]
fn nav_positions_map_between_document_and_ledger() {
    let doc = build(&mixed_entries(), &prefs(ToolDisplayMode::Hidden));
    let navigable = doc.navigable_indices();
    for (position, &index) in navigable.iter().enumerate() {
        assert_eq!(doc.nav_position(index), Some(position));
        assert_eq!(doc.message_at_nav_position(position), Some(index));
    }
    let metadata = doc
        .messages
        .iter()
        .position(|m| m.kind == DocMessageKind::PiMetadata)
        .unwrap();
    assert_eq!(doc.nav_position(metadata), None);
    assert_eq!(doc.message_at_nav_position(navigable.len()), None);
    assert_eq!(doc.nav_position(doc.messages.len()), None);
}

// -----------------------------------------------------------------
// Tool-result rows (truncation counts rendered rows, not source lines)
// -----------------------------------------------------------------

/// A user entry holding one tool result with this string content.
fn tool_result_entry(entry_index: usize, tool_use_id: &str, content: &str) -> RenderableEntry {
    entry(
        entry_index,
        &serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": tool_use_id, "content": content}
            ]}
        })
        .to_string(),
    )
}

fn tool_results(doc: &ConversationDocument) -> Vec<&DocToolResult> {
    all_blocks(doc)
        .into_iter()
        .filter_map(|block| match &block.kind {
            DocBlockKind::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect()
}

fn row_entries() -> Vec<RenderableEntry> {
    vec![
        user_entry(0, "hi", None),
        // Trailing line break: the ledger shows 4 rows and "(1 more lines...)".
        tool_result_entry(1, "t1", "a\nb\nc\nd\n"),
        // Blank lines: 6 rows, 2 hidden.
        tool_result_entry(2, "t2", "a\n\n\nb\n\n"),
        // Markdown with two fenced blocks: 3 rows (a, blank, b), nothing hidden.
        tool_result_entry(3, "t3", "```\na\n```\n```\nb\n```"),
        // Results that render no row at all.
        tool_result_entry(4, "t4", "```"),
        tool_result_entry(5, "t5", "```\n```"),
        assistant_text_entry(6, "ok", None),
    ]
}

#[test]
fn tool_result_truncation_counts_rendered_rows() {
    let entries = row_entries();
    assert_parity_everywhere(&entries);

    let doc = build(&entries, &prefs(ToolDisplayMode::Truncated));
    let results = tool_results(&doc);
    assert_eq!(results.len(), 3, "zero-row results are left out");

    let trailing = results[0];
    assert_eq!(trailing.rows, 5);
    assert_eq!(trailing.total_lines(), 5);
    assert_eq!(trailing.hidden_lines(), Some(1));
    assert!(trailing.expandable());
    assert_eq!(trailing.visible_text(), "a\nb\nc\nd");
    assert_eq!(trailing.visible_markdown(), "```text\na\nb\nc\nd\n```");

    let blanks = results[1];
    assert_eq!(blanks.rows, 6);
    assert_eq!(blanks.hidden_lines(), Some(2));
    assert_eq!(blanks.visible_text(), "a\n\n\nb");

    let fences = results[2];
    assert!(!fences.is_fenced());
    assert_eq!(fences.rows, 3);
    assert_eq!(fences.hidden_lines(), None);
    assert!(!fences.expandable());
    assert_eq!(fences.visible_text(), fences.text);
    assert_eq!(fences.visible_markdown(), fences.text);
    assert_eq!(fences.visible_plain_text(), "a\n\nb");
    assert_eq!(fences.ledger_lines(80), 3);

    // Expanded: everything is shown and the click collapses again.
    let expanded: BTreeSet<ToolOutputId> = BTreeSet::from([trailing.id.clone()]);
    let doc = build_document(&entries, &prefs(ToolDisplayMode::Truncated), &expanded);
    let trailing = tool_results(&doc)[0];
    assert_eq!(trailing.hidden_lines(), None);
    assert!(trailing.expandable());
    assert_eq!(trailing.visible_text(), "a\nb\nc\nd\n");
}

#[test]
fn zero_row_tool_results_create_no_navigable_message() {
    let entries = row_entries();
    for tool_display in MODES {
        let doc = build(&entries, &prefs(tool_display));
        let navigable = doc.navigable_entry_indices();
        assert!(
            !navigable.contains(&4) && !navigable.contains(&5),
            "{tool_display:?}: {navigable:?}"
        );
    }
    let doc = build(&entries, &prefs(ToolDisplayMode::Truncated));
    assert_eq!(doc.navigable_entry_indices(), vec![0, 1, 2, 3, 6]);

    // Expanded, a zero-row result is still not clickable (it is absent).
    let ids: BTreeSet<ToolOutputId> = (0..entries.len())
        .map(|index| {
            make_tool_output_id(
                index,
                None,
                0,
                ToolOutputKind::ToolResult,
                Some(&format!("t{index}")),
            )
        })
        .collect();
    let doc = assert_ledger_parity(&entries, &prefs(ToolDisplayMode::Truncated), &ids);
    assert_eq!(doc_toggle_ids(&doc).len(), 3);

    // A zero-row result next to a visible one is dropped but still consumes
    // the timing slot, like the ledger.
    let mixed = vec![
        entry(
            0,
            &serde_json::json!({
                "type": "user",
                "timestamp": "2026-09-04T12:34:56Z",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "z", "content": "```"},
                    {"type": "tool_result", "tool_use_id": "y", "content": "visible"}
                ]}
            })
            .to_string(),
        ),
        assistant_text_entry(1, "ok", None),
    ];
    assert_parity_everywhere(&mixed);
    let mut timed = prefs(ToolDisplayMode::Truncated);
    timed.show_timing = true;
    let doc = build(&mixed, &timed);
    assert_eq!(doc.messages[0].blocks.len(), 1);
    assert_eq!(doc.messages[0].blocks[0].timing, DocTiming::Pad);
}

#[test]
fn collapsed_markdown_results_show_the_rows_the_ledger_shows() {
    let cases = [
        "```rust\nl1\nl2\nl3\nl4\nl5\nl6\n```",
        "## Summary\n\nI found:\n\n- one\n- two\n- three\n\n```\ncode\n```",
        "intro\n\n```\na\n\nb\n\nc\n```\nafter",
        "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\n```\nx\n```",
        "para one\nstill one\n\npara two\n\npara three\n\npara four\n\n```\nz\n```",
    ];
    for (index, text) in cases.iter().enumerate() {
        let entries = vec![tool_result_entry(0, "t", text)];
        assert_parity_everywhere(&entries);
        let doc = build(&entries, &prefs(ToolDisplayMode::Truncated));
        let result = tool_results(&doc)[0];
        assert!(
            result.rows > TRUNCATED_RESULT_LINES,
            "case {index}: {result:?}"
        );
        assert_eq!(result.hidden_lines(), Some(result.rows - 4), "case {index}");

        // The visible markdown is a source prefix whose rendering starts
        // with exactly the rows the ledger shows (trailing blanks aside).
        let visible = result.visible_markdown();
        assert!(
            result.text.starts_with(&visible),
            "case {index}: {visible:?}"
        );
        let full = row_keys(&result.markdown);
        let mut shown = &full[..4];
        while shown.last().is_some_and(|(blank, _)| *blank) {
            shown = &shown[..shown.len() - 1];
        }
        let rendered = row_keys(&visible);
        assert!(
            rendered.len() >= shown.len() && rendered[..shown.len()] == *shown,
            "case {index}: {visible:?}"
        );
        // The plain text is exactly the shown rows.
        assert_eq!(
            result.visible_plain_text(),
            markdown_plain_text(&result.markdown)
                .split('\n')
                .take(4)
                .collect::<Vec<_>>()
                .join("\n"),
            "case {index}"
        );
    }

    // A long code block is cut inside the block, exactly at the 4th row.
    let doc = build(
        &[tool_result_entry(0, "t", cases[0])],
        &prefs(ToolDisplayMode::Truncated),
    );
    let result = tool_results(&doc)[0];
    assert_eq!(result.rows, 6);
    assert_eq!(result.visible_text(), "```rust\nl1\nl2\nl3\nl4\n");
    assert_eq!(unwrapped_rows(&result.visible_text()), 4);
}

#[test]
fn fenced_result_rows_are_its_lines() {
    // One row per line of plain text, so the first `k` lines render
    // exactly `k` rows.
    for text in [
        "a\nb\nc\nd\ne",
        "a\nb\nc\nd\ne\n",
        "\n\n\n\n\n",
        "tab\there\nx\ny\nz\nw",
        "crlf\r\nb\r\nc\r\nd\r\ne\r\n",
        "lone\rcarriage\nb\nc\nd\ne",
        "юникод\n日本語\n✓\n\u{0}\nend",
        "   indented\n\tTabbed\n# not a heading\n- not a list\n> nor a quote\n~~~\nx",
    ] {
        let markdown = fence_text(text);
        let rows = unwrapped_rows(&markdown);
        assert_eq!(rows, text.split('\n').count(), "{text:?}");
        for k in 1..rows {
            assert_eq!(
                unwrapped_rows(&fence_text(&visible_prefix(text, k))),
                k,
                "{text:?} k={k}"
            );
        }
    }
}

// -----------------------------------------------------------------
// FEATURES rows owned by WP-C (viewer core behavior)
// -----------------------------------------------------------------

#[test]
fn v023_agent_progress_entries_render_only_with_show_thinking() {
    let entries = agent_progress_entries();
    let hidden = build(&entries, &prefs(ToolDisplayMode::Truncated));
    assert!(hidden.messages.is_empty(), "{hidden:#?}");
    let shown = build(
        &entries,
        &ViewPrefs {
            show_thinking: true,
            ..prefs(ToolDisplayMode::Truncated)
        },
    );
    assert!(!shown.messages.is_empty());
    assert!(shown.messages.iter().all(|message| matches!(
        message.kind,
        DocMessageKind::AgentProgressUser | DocMessageKind::AgentProgressAssistant
    )));
}

#[test]
fn v026_image_and_document_blocks_produce_no_rows() {
    let entries = [
        entry(
            0,
            r#"{"type":"user","message":{"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"SECRETPIXELS"}},
                {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"SECRETPDF"}},
                {"type":"text","text":"look at this"}
            ]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","message":{"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"ONLYIMAGE"}}
            ]}}"#,
        ),
    ];
    for mode in [ToolDisplayMode::Hidden, ToolDisplayMode::Full] {
        let doc = build(&entries, &prefs(mode));
        let text = doc_text(&doc);
        assert_eq!(doc.messages.len(), 1, "{text}");
        assert!(text.contains("look at this"));
        for secret in ["SECRETPIXELS", "SECRETPDF", "ONLYIMAGE", "image/png"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
}

#[test]
fn v030_warmup_exchange_is_rendered_in_the_viewer() {
    // The list drops a Warmup-only session's preview; the viewer shows it.
    let entries = [
        user_entry(0, "Warmup", None),
        assistant_text_entry(1, "Ready to help", None),
        user_entry(2, "real question", None),
    ];
    let doc = build(&entries, &prefs(ToolDisplayMode::Hidden));
    assert_eq!(doc.messages.len(), 3);
    let text = doc_text(&doc);
    assert!(
        text.contains("Warmup") && text.contains("Ready to help"),
        "{text}"
    );
}

#[test]
fn v058_image_only_read_result_shows_pretty_json_with_the_base64_data() {
    let entries = [
        entry(
            0,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"shot.png"}}]}}"#,
        ),
        entry(
            1,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"QkFTRTY0REFUQQ=="}}]}]}}"#,
        ),
    ];
    let doc = build(&entries, &prefs(ToolDisplayMode::Full));
    let DocBlockKind::ToolResult(result) = &doc.messages[1].blocks[0].kind else {
        panic!("expected a result: {doc:#?}");
    };
    let parsed: serde_json::Value = serde_json::from_str(&result.text).expect("pretty JSON");
    assert_eq!(
        parsed[0]["source"]["data"],
        serde_json::json!("QkFTRTY0REFUQQ==")
    );
    assert!(
        result.text.contains('\n'),
        "pretty-printed: {}",
        result.text
    );
}
