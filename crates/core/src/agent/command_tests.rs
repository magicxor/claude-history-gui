//! Agent command tests moved from upstream `main.rs` (`agent_command_tests`).
//!
//! The resume/fork and delete-empty tests of that module belong to
//! `core::resume` / `core::maintenance` (Modules stage). Reads and outlines
//! go through the default `AgentService` (default config instead of the
//! user's `config.toml`, plan section 6.3).

use crate::agent;
use crate::agent::args::{AgentOutlineArgs, AgentOutputFlags, AgentReadArgs};
use crate::agent::refs::AgentConversationKey;
use crate::agent::service::{
    agent_semantic_candidates, agent_semantic_candidates_matching_tools,
    resolve_agent_conversation_arg, resolve_agent_read_args, run_agent_outline, run_agent_read,
};
use crate::agent::test_support::{assistant_jsonl_line as assistant, user_jsonl_line as user};
use crate::env::HistoryEnv;
use crate::history;
use crate::search;
use crate::search::mode::SearchMode;
use std::path::{Path, PathBuf};

/// The explicit-keys code paths never consult the environment; an empty
/// environment (no home directory) guarantees they cannot reach real data.
fn no_env() -> HistoryEnv {
    HistoryEnv::default()
}

fn key(project: &str, filename: &str) -> AgentConversationKey {
    AgentConversationKey::new(
        project,
        filename,
        PathBuf::from(format!("/{project}/{filename}")),
    )
}

fn default_output() -> AgentOutputFlags {
    AgentOutputFlags {
        budget: Some(6000),
        no_budget: false,
        tools: false,
        tool_results: false,
        thinking: false,
        subagents: false,
    }
}

fn read_args(refs: Vec<String>, focus: Option<String>) -> AgentReadArgs {
    AgentReadArgs {
        refs,
        anchor: None,
        focus,
        lines: None,
        match_query: None,
        context: 3,
        output: default_output(),
    }
}

#[test]
fn read_validation_rejects_focus_outside_range() {
    let keys = vec![key(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
    )];
    let conversation = keys[0].conversation_ref().canonical();
    let args = read_args(
        vec![format!("{conversation}:m2..m4")],
        Some("m5".to_string()),
    );
    let err = resolve_agent_read_args(&no_env(), &args, Some(&keys)).unwrap_err();
    assert!(err.to_string().contains("outside"));
}

fn write_jsonl(dir: &tempfile::TempDir, filename: &str, lines: &[String]) -> PathBuf {
    let path = dir.path().join(filename);
    std::fs::write(&path, lines.join("\n")).unwrap();
    path
}

#[test]
fn command_refs_accept_uuid_args() {
    let keys = vec![key(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
    )];
    let uuid = "12345678-1234-4234-9234-123456789abc";

    let read = read_args(vec![format!("{uuid}:m1..m2")], None);
    assert!(resolve_agent_read_args(&no_env(), &read, Some(&keys)).is_ok());

    let focus = read_args(
        vec![format!("{}:m1..m2", keys[0].conversation_ref().canonical())],
        Some(format!("{uuid}:m1")),
    );
    assert!(resolve_agent_read_args(&no_env(), &focus, Some(&keys)).is_ok());

    assert!(resolve_agent_conversation_arg(&no_env(), uuid, Some(&keys)).is_ok());
}

#[test]
fn read_command_loads_transcript_and_formats_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[user("question"), assistant("answer")],
    );
    let keys = vec![AgentConversationKey::new(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
        path,
    )];
    let conversation = keys[0].conversation_ref().canonical();
    let args = read_args(vec![format!("{conversation}:m1..m2")], None);

    let output = run_agent_read(&args, Some(&keys)).unwrap();

    assert!(output.starts_with("protocol agent-read"));
    assert!(output.contains("conversation project=pr_"));
    assert!(output.contains("uuid=12345678-1234-4234-9234-123456789abc ref=ch_"));
    assert!(output.contains("message m1 role=user line=1"));
    assert!(output.contains("| question\n"));
    assert!(output.contains("message m2 role=assistant line=2"));
    assert!(!output.contains("not implemented"));
}

#[test]
fn read_slice_requires_one_single_message_ref() {
    let keys = vec![key(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
    )];
    let conversation = keys[0].conversation_ref().canonical();
    let mut args = read_args(vec![format!("{conversation}:m1..m2")], None);
    args.match_query = Some("needle".to_string());

    let error = resolve_agent_read_args(&no_env(), &args, Some(&keys)).unwrap_err();

    assert!(error.to_string().contains("single-message ref"));
}

#[test]
fn read_command_slices_a_large_message_around_matches() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[user("one\ntwo\nhistorical correction\nfour\nfive")],
    );
    let keys = vec![AgentConversationKey::new(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
        path,
    )];
    let conversation = keys[0].conversation_ref().canonical();
    let mut args = read_args(vec![format!("{conversation}:m1")], None);
    args.match_query = Some("historical correction".to_string());
    args.context = 1;

    let output = run_agent_read(&args, Some(&keys)).unwrap();

    assert!(output.contains("slice=match=historical%20correction context=1 hits=1"));
    assert!(output.contains("|  2: two\n| >3: historical correction\n|  4: four\n"));
    assert!(!output.contains("1: one"));
    assert!(!output.contains("5: five"));
}

#[test]
fn outline_command_loads_transcript_and_formats_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[user("question"), assistant("answer")],
    );
    let keys = vec![AgentConversationKey::new(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
        path,
    )];
    let args = AgentOutlineArgs {
        conversation: keys[0].conversation_ref().canonical(),
        output: default_output(),
    };

    let output = run_agent_outline(&args, Some(&keys)).unwrap();

    assert!(output.starts_with("protocol agent-outline"));
    assert!(output.contains("conversation project=pr_"));
    assert!(output.contains("uuid=12345678-1234-4234-9234-123456789abc ref=ch_"));
    assert!(output.contains("m1 role=user chars=8 anchor=ma_"));
    assert!(output.contains(" | question"));
    assert!(output.contains("m2 role=assistant chars=6 anchor=ma_"));
    assert!(output.contains(" | answer"));
}

#[test]
fn search_output_emits_read_ref_with_focus_recipe() {
    let output = agent::search::AgentSearchOutput {
        protocol: agent::search::AgentProtocolKind::Search,
        target: None,
        query: "cache warming".to_string(),
        mode: SearchMode::Lexical,
        hits: vec![agent::search::AgentOutputHit {
            conversation_ref: "ch_1234abcd5678".to_string(),
            project_id: "pr_test".to_string(),
            conversation_uuid: "12345678-1234-4234-9234-123456789abc".to_string(),
            session: "12345678-1234-4234-9234-123456789abc.jsonl".to_string(),
            anchors: vec!["ma_0000000000000000".to_string()],
            title: "cache session".to_string(),
            score: 12.5,
            evidence_score: 12.5,
            semantic_score_breakdown: None,
            source: agent::search::AgentHitKind::Lexical,
            evidence_source: agent::retrieval::AgentHitSource::Dialogue,
            render_options: agent::retrieval::AgentHitRenderOptions::default(),
            preview: "cache warming answer".to_string(),
            focus_range: agent::refs::MessageRange::single(2),
            read_range: agent::refs::MessageRange { start: 1, end: 3 },
        }],
        groups: vec![],
        flat: true,
        budget: None,
        stats: agent::search::AgentSearchStats::default(),
    };

    let rendered = agent::search::format_agent_output(&output);

    assert!(rendered.starts_with(
        "protocol agent-search mode=lexical cut=none chars=none policy=per-hit hits=1\n"
    ));
    assert!(rendered.contains(
        "hit project=pr_test uuid=12345678-1234-4234-9234-123456789abc ref=ch_1234abcd5678"
    ));
    assert!(rendered.contains("read ref=ch_1234abcd5678:m1..m3 focus=m2..m2 tools=false tool-results=false thinking=false subagents=false\n"));
}

fn read_args_from_line(read_line: &str) -> AgentReadArgs {
    let mut read_ref = None;
    let mut focus = None;
    let mut tools = false;
    let mut tool_results = false;
    let mut thinking = false;
    let mut subagents = false;
    for field in read_line.split_whitespace().skip(1) {
        if let Some(value) = field.strip_prefix("ref=") {
            read_ref = Some(value.to_string());
        } else if let Some(value) = field.strip_prefix("focus=") {
            focus = Some(value.to_string());
        } else if field == "tools=true" {
            tools = true;
        } else if field == "tool-results=true" {
            tool_results = true;
        } else if field == "thinking=true" {
            thinking = true;
        } else if field == "subagents=true" {
            subagents = true;
        }
    }
    AgentReadArgs {
        refs: vec![read_ref.expect("read ref field")],
        anchor: None,
        focus,
        lines: None,
        match_query: None,
        context: 3,
        output: AgentOutputFlags {
            budget: Some(6000),
            no_budget: false,
            tools,
            tool_results,
            thinking,
            subagents,
        },
    }
}

#[test]
fn within_read_ref_can_drive_focused_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("question"),
            assistant("cache warming answer"),
            user("follow up"),
        ],
    );
    let (keys, resolved) = resolved_test_conversation(path.clone());
    let conversation = stubbed_conversation(path, 3);
    let transcript = load_transcript(&resolved.key.path);
    let within_args = crate::agent::args::AgentWithinArgs {
        conversation: resolved.reference.canonical(),
        query: "cache warming".to_string(),
        top: Some(1),
        budget: Some(6000),
        no_budget: false,
        search_mode: crate::agent::args::AgentSearchModeArgs::explicit(SearchMode::Lexical),
    };
    let within_request = agent::search::AgentWithinRequest {
        query: within_args.query.clone(),
        top: within_args.top.unwrap(),
        cli_mode: within_args.mode_override(),
        config_mode: None,
        tui_semantic_search: None,
        budget: None,
    };
    let within = agent::search::format_agent_output(&agent::search::run_within_search(
        &within_request,
        &conversation,
        &resolved,
        &transcript,
        &[],
    ));
    let read_line = within
        .lines()
        .find(|line| line.starts_with("read ref="))
        .expect("within output should include a read ref");
    let read_args = read_args_from_line(read_line);

    let output = run_agent_read(&read_args, Some(&keys)).unwrap();

    assert!(output.starts_with("protocol agent-read"));
    assert!(output.contains("message m2 role=assistant line=2"));
    assert!(output.contains("| cache warming answer\n"));
}

fn tool_result_user(tool_output: &str) -> String {
    serde_json::json!({
        "type": "user",
        "timestamp": "2024-01-01T00:00:02Z",
        "message": {
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": tool_output}]
        }
    })
    .to_string()
}

fn resolved_test_conversation(
    path: PathBuf,
) -> (Vec<AgentConversationKey>, agent::refs::ResolvedConversation) {
    let key = AgentConversationKey::new(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
        path,
    );
    let resolved = agent::refs::ResolvedConversation {
        key: key.clone(),
        reference: key.conversation_ref(),
    };
    (vec![key], resolved)
}

fn stubbed_conversation(path: PathBuf, message_count: usize) -> history::Conversation {
    history::Conversation {
        source: history::Source::Claude,
        session_id: path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        path,
        index: 0,
        timestamp: chrono::Local::now(),
        preview: "session".to_string(),
        preview_first: "session".to_string(),
        preview_last: "session".to_string(),
        full_text: "session".to_string(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns: vec!["session".to_string()],
        semantic_turn_ranges: vec![agent::refs::MessageRange::single(1)],
        search_text_lower: "session".to_string(),
        project_name: Some("project-a".to_string()),
        project_path: None,
        cwd: None,
        message_count,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: Some("session".to_string()),
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn parsed_conversation(path: &Path) -> history::Conversation {
    history::parser::process_conversation_reader(
        path.to_path_buf(),
        std::io::Cursor::new(std::fs::read_to_string(path).unwrap()),
        None,
        None,
    )
    .unwrap()
    .unwrap()
}

fn load_transcript(path: &Path) -> agent::transcript::AgentTranscript {
    agent::transcript::AgentTranscript::load(path).unwrap()
}

fn semantic_hit_for_test(
    source: crate::semantic::types::SemanticChunkSource,
    message_range: agent::refs::MessageRange,
    evidence_preview: &str,
) -> crate::semantic::types::SemanticHit {
    crate::semantic::types::SemanticHit::new(
        crate::semantic::types::SemanticScoreBreakdown {
            hybrid: 0.9,
            semantic: 0.9,
            lexical: 0.0,
        },
        crate::semantic::types::SemanticExplanation {
            quality: crate::semantic::types::SemanticQuality::Good,
            quality_label: "good",
            matched_terms: vec![],
            evidence_preview: evidence_preview.to_string(),
            rationale_kind: crate::semantic::types::SemanticRationaleKind::SemanticOnly,
            chunk: crate::semantic::types::SemanticChunkIdentity {
                conversation_index: 0,
                source,
                session: "session".to_string(),
                chunk_index: 0,
                message_range,
            },
        },
    )
}

fn progress_text_message(text: &str) -> String {
    serde_json::json!({
        "type": "progress",
        "data": {
            "type": "agent_progress",
            "agentId": "agent-abcdef",
            "message": {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}]
                }
            }
        }
    })
    .to_string()
}

#[test]
fn tool_result_read_recipe_replays_without_manual_flags() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("question"),
            assistant("I'll inspect it"),
            tool_result_user("hidden_exact_tool_needle"),
            assistant("done"),
        ],
    );
    let (keys, resolved) = resolved_test_conversation(path.clone());
    let conversation = stubbed_conversation(path, 4);
    let transcript = load_transcript(&resolved.key.path);
    let within_request = agent::search::AgentWithinRequest {
        query: "\"hidden_exact_tool_needle\"".to_string(),
        top: 1,
        cli_mode: None,
        config_mode: None,
        tui_semantic_search: None,
        budget: None,
    };
    let within = agent::search::format_agent_output(&agent::search::run_within_search(
        &within_request,
        &conversation,
        &resolved,
        &transcript,
        &[],
    ));

    assert!(within.contains("hit project=pr_"));
    assert!(within.contains(" ref="));
    assert!(within.contains("source=tool"));
    let read_line = within
        .lines()
        .find(|line| line.starts_with("read ref="))
        .expect("within output should include a read ref");
    assert!(read_line.contains("tool-results=true"));
    let output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

    assert!(output.contains("hidden_exact_tool_needle"));
}

#[test]
fn semantic_read_ref_uses_canonical_assistant_ordinal() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("first visible"),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:00:01Z",
                "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "pwd"}}]}
            })
            .to_string(),
            tool_result_user("tool output only"),
            progress_text_message("subagent hidden text"),
            assistant("final assistant text"),
        ],
    );
    let (keys, resolved) = resolved_test_conversation(path.clone());
    let conversation = parsed_conversation(&path);
    let transcript = load_transcript(&resolved.key.path);
    let semantic_range = *conversation
        .semantic_turn_ranges
        .iter()
        .find(|range| **range == agent::refs::MessageRange::single(5))
        .expect("assistant text should use canonical m5");
    let semantic_hit = semantic_hit_for_test(
        crate::semantic::types::SemanticChunkSource::VisibleDialogue,
        semantic_range,
        "final assistant text",
    );
    let within_request = agent::search::AgentWithinRequest {
        query: "final assistant".to_string(),
        top: 1,
        cli_mode: Some(SearchMode::Semantic),
        config_mode: None,
        tui_semantic_search: None,
        budget: None,
    };
    let within = agent::search::format_agent_output(&agent::search::run_within_search(
        &within_request,
        &conversation,
        &resolved,
        &transcript,
        &[semantic_hit],
    ));

    assert!(within.contains("focus=m5..m5"));
    let read_line = within
        .lines()
        .find(|line| line.starts_with("read ref="))
        .expect("within output should include a read ref");
    let output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

    assert!(output.contains("message m5 role=assistant"));
    assert!(output.contains("final assistant text"));
}

#[test]
fn semantic_read_ref_skips_assistant_image_only_ordinal() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("first visible"),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:00:01Z",
                "message": {"role": "assistant", "content": [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc"}}]}
            })
            .to_string(),
            assistant("final assistant text"),
        ],
    );
    let (keys, resolved) = resolved_test_conversation(path.clone());
    let conversation = parsed_conversation(&path);
    let transcript = load_transcript(&resolved.key.path);
    let semantic_range = *conversation
        .semantic_turn_ranges
        .iter()
        .find(|range| **range == agent::refs::MessageRange::single(2))
        .expect("assistant text should use canonical m2");
    let semantic_hit = semantic_hit_for_test(
        crate::semantic::types::SemanticChunkSource::VisibleDialogue,
        semantic_range,
        "final assistant text",
    );
    let within_request = agent::search::AgentWithinRequest {
        query: "final assistant".to_string(),
        top: 1,
        cli_mode: Some(SearchMode::Semantic),
        config_mode: None,
        tui_semantic_search: None,
        budget: None,
    };
    let within = agent::search::format_agent_output(&agent::search::run_within_search(
        &within_request,
        &conversation,
        &resolved,
        &transcript,
        &[semantic_hit],
    ));

    assert!(within.contains("focus=m2..m2"));
    let read_line = within
        .lines()
        .find(|line| line.starts_with("read ref="))
        .expect("within output should include a read ref");
    let output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

    assert!(output.contains("message m2 role=assistant"));
    assert!(output.contains("final assistant text"));
}

#[test]
fn global_search_finds_progress_only_subagent_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("question"),
            progress_text_message("subagent_unique_needle"),
            assistant("done"),
        ],
    );
    let (keys, _) = resolved_test_conversation(path.clone());
    let conversation = parsed_conversation(&path);
    assert!(!conversation.full_text.contains("subagent_unique_needle"));
    assert!(
        conversation
            .agent_search_text
            .contains("subagent_unique_needle")
    );
    let conversations = vec![conversation];
    let searchable = search::precompute_agent_search_text(&conversations);
    let ranked = search::agent_search(
        &conversations,
        &searchable,
        "\"subagent_unique_needle\"",
        chrono::Local::now(),
    );
    let request = agent::search::AgentSearchRequest {
        query: "\"subagent_unique_needle\"".to_string(),
        top: 1,
        cli_mode: None,
        config_mode: None,
        tui_semantic_search: None,
        flat: false,
        hits_per_conversation: 2,
        retrieval_hits_per_conversation: None,
        all_hits: false,
        budget: None,
    };
    let output =
        agent::search::run_global_lexical_search(&request, &conversations, &keys, &ranked, |key| {
            agent::transcript::AgentTranscript::load(&key.path)
        })
        .unwrap();
    let rendered = agent::search::format_agent_output(&output);

    assert!(rendered.contains("protocol agent-search"));
    assert!(rendered.contains("conversation rank=1"));
    assert!(rendered.contains("subagents=true"));
    let read_line = rendered
        .lines()
        .find(|line| line.starts_with("read ref="))
        .expect("search output should include a read ref");
    let read_output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

    assert!(read_output.contains("subagent_unique_needle"));
}

#[test]
fn agent_semantic_candidates_preserve_tool_results_and_select_matching_segments() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("question"),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:00:01Z",
                "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/tmp/example"}}]}
            })
            .to_string(),
            tool_result_user(&format!(
                "HEAD{}distinctive_calibration{}TAIL",
                "x".repeat(5_000),
                "y".repeat(5_000)
            )),
            assistant("done"),
        ],
    );
    let (keys, resolved) = resolved_test_conversation(path.clone());
    let conversation = parsed_conversation(&path);
    let input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved,
        original_index: 0,
    };

    let matching_tools_input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved: input.resolved.clone(),
        original_index: 0,
    };
    let unmatched_tools_input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved: input.resolved.clone(),
        original_index: 0,
    };
    let tool_summary_input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved: input.resolved.clone(),
        original_index: 0,
    };
    let candidates = agent_semantic_candidates(&[input]).0;
    let matching_candidates = agent_semantic_candidates_matching_tools(
        &[matching_tools_input],
        "distinctive calibration",
    )
    .0;
    let unmatched_candidates =
        agent_semantic_candidates_matching_tools(&[unmatched_tools_input], "absent needle").0;
    let tool_summary_candidates =
        agent_semantic_candidates_matching_tools(&[tool_summary_input], "Read tool").0;
    let tool = candidates
        .iter()
        .find(|candidate| {
            candidate.source == crate::semantic::types::SemanticChunkSource::AgentTool
        })
        .expect("tool result candidate");

    let matching_tool = matching_candidates
        .iter()
        .find(|candidate| {
            candidate.source == crate::semantic::types::SemanticChunkSource::AgentTool
        })
        .expect("matching tool result candidate");
    let tool_summary = tool_summary_candidates
        .iter()
        .find(|candidate| {
            candidate.source == crate::semantic::types::SemanticChunkSource::AgentTool
        })
        .expect("matching tool summary candidate");

    assert_eq!(keys.len(), 1);
    assert!(tool.conversation.semantic_turns.len() > 1);
    assert!(
        tool.conversation
            .semantic_turns
            .iter()
            .all(|turn| turn.len() <= crate::semantic::types::DEFAULT_CHUNK_TARGET_CHARS)
    );
    let all_tool_text = tool.conversation.semantic_turns.join("");
    assert!(all_tool_text.contains("tool Read"));
    assert!(all_tool_text.contains("HEAD"));
    assert!(all_tool_text.contains("TAIL"));
    assert!(all_tool_text.contains("distinctive_calibration"));
    assert_eq!(matching_tool.conversation.semantic_turns.len(), 1);
    assert!(matching_tool.conversation.semantic_turns[0].contains("distinctive_calibration"));
    assert_eq!(
        tool_summary.conversation.semantic_turns,
        ["tool Read input_keys=file_path"]
    );
    assert!(
        matching_tool.conversation.semantic_turns[0].len()
            <= crate::semantic::types::DEFAULT_CHUNK_TARGET_CHARS
    );

    let config = crate::semantic::types::ChunkConfig::default();
    let all_chunks = crate::semantic::chunk::build_chunks_with_sources(
        [(0, tool.source, tool.conversation.as_ref())],
        config,
    );
    let matching_chunks = crate::semantic::chunk::build_chunks_with_sources(
        [(0, matching_tool.source, matching_tool.conversation.as_ref())],
        config,
    );
    assert!(
        matching_chunks
            .iter()
            .all(|matching| all_chunks.iter().any(|all| all.text == matching.text))
    );
    assert!(unmatched_candidates.iter().all(|candidate| !matches!(
        candidate.source,
        crate::semantic::types::SemanticChunkSource::AgentTool
            | crate::semantic::types::SemanticChunkSource::AgentSubagentTool
    )));
}

#[test]
fn agent_semantic_tool_matching_sanitizes_text_and_handles_unicode_and_short_terms() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("question"),
            tool_result_user("prefix \u{1b}[31mÜber UI\u{1b}[0m menuBar suffix"),
            assistant("done"),
        ],
    );
    let (_, resolved) = resolved_test_conversation(path.clone());
    let conversation = parsed_conversation(&path);
    let input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved: resolved.clone(),
        original_index: 0,
    };
    let camel_case_input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved: resolved.clone(),
        original_index: 0,
    };
    let exact_identifier_input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved: resolved.clone(),
        original_index: 0,
    };
    let stop_words_input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved,
        original_index: 0,
    };

    let candidates = agent_semantic_candidates_matching_tools(&[input], "über ui").0;
    let camel_case_candidates =
        agent_semantic_candidates_matching_tools(&[camel_case_input], "bar").0;
    let exact_identifier_candidates =
        agent_semantic_candidates_matching_tools(&[exact_identifier_input], "menuBar").0;
    let stop_word_candidates =
        agent_semantic_candidates_matching_tools(&[stop_words_input], "how to the an or if").0;
    let tool = candidates
        .iter()
        .find(|candidate| {
            candidate.source == crate::semantic::types::SemanticChunkSource::AgentTool
        })
        .expect("matching Unicode tool result candidate");

    assert_eq!(
        tool.conversation.semantic_turns,
        ["prefix Über UI menuBar suffix"]
    );
    assert!(camel_case_candidates.iter().any(|candidate| {
        candidate.source == crate::semantic::types::SemanticChunkSource::AgentTool
    }));
    assert!(exact_identifier_candidates.iter().any(|candidate| {
        candidate.source == crate::semantic::types::SemanticChunkSource::AgentTool
    }));
    assert!(stop_word_candidates.iter().all(|candidate| !matches!(
        candidate.source,
        crate::semantic::types::SemanticChunkSource::AgentTool
            | crate::semantic::types::SemanticChunkSource::AgentSubagentTool
    )));
}

#[test]
fn agent_semantic_candidates_include_progress_only_subagent_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("question"),
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"progress_only_semantic_needle"}]}}}}"#.to_string(),
            assistant("done"),
        ],
    );
    let key = AgentConversationKey::new(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
        path.clone(),
    );
    let resolved = agent::refs::ResolvedConversation {
        key: key.clone(),
        reference: key.conversation_ref(),
    };
    let conversation = history::parser::process_conversation_reader(
        path.clone(),
        std::io::Cursor::new(std::fs::read_to_string(&path).unwrap()),
        None,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(
        !conversation
            .semantic_turns
            .join(" ")
            .contains("progress_only_semantic_needle")
    );
    let input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved,
        original_index: 0,
    };

    let candidates = agent_semantic_candidates(&[input]).0;
    let candidate = candidates[1].conversation.as_ref();

    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0].source,
        crate::semantic::types::SemanticChunkSource::VisibleDialogue
    );
    assert_eq!(
        candidates[1].source,
        crate::semantic::types::SemanticChunkSource::AgentSubagentDialogue
    );
    assert!(
        candidate
            .semantic_turns
            .join(" ")
            .contains("progress_only_semantic_needle")
    );
    assert_eq!(
        candidate.semantic_turn_ranges.last().copied(),
        Some(agent::refs::MessageRange::single(2))
    );
    assert!(
        !conversation
            .semantic_turns
            .join(" ")
            .contains("progress_only_semantic_needle")
    );
}

#[test]
fn agent_semantic_candidates_require_canonical_transcript_projection() {
    let key = AgentConversationKey::new(
        "project-a",
        "missing.jsonl",
        std::path::PathBuf::from("/missing/session.jsonl"),
    );
    let resolved = agent::refs::ResolvedConversation {
        key: key.clone(),
        reference: key.conversation_ref(),
    };
    let conversation = history::Conversation {
        source: history::Source::Claude,
        session_id: key.session_id.clone(),
        path: key.path.clone(),
        index: 0,
        timestamp: chrono::Local::now(),
        preview: "visible semantic".to_string(),
        preview_first: "visible semantic".to_string(),
        preview_last: "visible semantic".to_string(),
        full_text: "visible semantic".to_string(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns: vec!["visible semantic".to_string()],
        semantic_turn_ranges: vec![agent::refs::MessageRange::single(1)],
        search_text_lower: "visible semantic".to_string(),
        project_name: Some("project-a".to_string()),
        project_path: None,
        cwd: None,
        message_count: 1,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: Some("visible semantic".to_string()),
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    };
    let input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved,
        original_index: 0,
    };

    let (candidates, warnings) = agent_semantic_candidates(&[input]);

    assert!(candidates.is_empty());
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].reference,
        Some(key.conversation_ref().canonical())
    );
}

#[test]
fn agent_semantic_candidates_recover_malformed_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("visible semantic"),
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"progress_only_semantic_needle"}]}}}}"#.to_string(),
            "not json".to_string(),
            assistant("done"),
        ],
    );
    let key = AgentConversationKey::new(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
        path.clone(),
    );
    let resolved = agent::refs::ResolvedConversation {
        key: key.clone(),
        reference: key.conversation_ref(),
    };
    let conversation = history::parser::process_conversation_reader(
        path,
        std::io::Cursor::new([
            user("visible semantic"),
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"progress_only_semantic_needle"}]}}}}"#.to_string(),
            "not json".to_string(),
            assistant("done"),
        ].join("\n")),
        None,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(
        conversation
            .agent_search_text
            .contains("progress_only_semantic_needle")
    );
    let input = agent::search::AgentConversationInput {
        conversation: &conversation,
        resolved,
        original_index: 0,
    };

    let (candidates, warnings) = agent_semantic_candidates(&[input]);

    assert_eq!(candidates.len(), 2);
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].kind,
        agent::diagnostic::AgentWarningKind::MalformedTranscript
    );
    assert_eq!(
        candidates[0].source,
        crate::semantic::types::SemanticChunkSource::VisibleDialogue
    );
    assert_eq!(
        candidates[1].source,
        crate::semantic::types::SemanticChunkSource::AgentSubagentDialogue
    );
    assert!(
        candidates[1]
            .conversation
            .semantic_turns
            .join(" ")
            .contains("progress_only_semantic_needle")
    );
}

#[test]
fn semantic_progress_hit_read_recipe_enables_subagents() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[
            user("question"),
            progress_text_message("progress_only_semantic_needle"),
            assistant("done"),
        ],
    );
    let (keys, resolved) = resolved_test_conversation(path.clone());
    let conversation = parsed_conversation(&path);
    let transcript = load_transcript(&resolved.key.path);
    let semantic_hit = semantic_hit_for_test(
        crate::semantic::types::SemanticChunkSource::AgentSubagentDialogue,
        agent::refs::MessageRange::single(2),
        "progress_only_semantic_needle",
    );
    let within_request = agent::search::AgentWithinRequest {
        query: "progress semantic".to_string(),
        top: 1,
        cli_mode: Some(SearchMode::Semantic),
        config_mode: None,
        tui_semantic_search: None,
        budget: None,
    };
    let rendered = agent::search::format_agent_output(&agent::search::run_within_search(
        &within_request,
        &conversation,
        &resolved,
        &transcript,
        &[semantic_hit],
    ));

    assert!(rendered.contains("focus=m2..m2"));
    assert!(rendered.contains("tools=false tool-results=false thinking=false subagents=true"));
    let read_line = rendered
        .lines()
        .find(|line| line.starts_with("read ref="))
        .expect("within output should include a read ref");
    let read_output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();
    assert!(read_output.contains("progress_only_semantic_needle"));
}

#[test]
fn read_command_rejects_out_of_range_loaded_messages() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_jsonl(
        &dir,
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[user("question")],
    );
    let keys = vec![AgentConversationKey::new(
        "project-a",
        "12345678-1234-4234-9234-123456789abc.jsonl",
        path,
    )];
    let conversation = keys[0].conversation_ref().canonical();
    let args = read_args(vec![format!("{conversation}:m1..m2")], None);

    let err = run_agent_read(&args, Some(&keys)).unwrap_err();

    assert!(err.to_string().contains("exceeds transcript length"));
}
