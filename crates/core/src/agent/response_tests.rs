//! [`crate::agent::execute`]: the structured answer must describe exactly
//! what the protocol text says, and the text must equal the CLI output.

use crate::agent::args::{
    AgentOutlineArgs, AgentReadArgs, AgentRequest, AgentSearchArgs, AgentWithinArgs,
};
use crate::agent::diagnostic::AgentErrorKind;
use crate::agent::protocol::{OutlineItem, ReadSlice};
use crate::agent::response::{AgentResponse, AgentResult, AgentSearchResult};
use crate::agent::service::{AgentNotice, AgentService, execute, execute_text};
use crate::agent::test_support::{assistant_jsonl_line, user_jsonl_line};
use crate::agent::transcript::AgentMessageRole;
use crate::config::ConfigFile;
use crate::env::HistoryEnv;
use crate::error::Result;
use crate::history::Source;
use crate::semantic::embed::SemanticEmbedder;
use std::path::{Path, PathBuf};

const UUID_A: &str = "11111111-2222-4333-8444-555555555555";
const UUID_B: &str = "66666666-7777-4888-9999-aaaaaaaaaaaa";

struct Sandbox {
    _home: tempfile::TempDir,
    env: HistoryEnv,
    project: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(home.path());
        let project = env.claude_projects_root().unwrap().join("-home-user-demo");
        std::fs::create_dir_all(&project).unwrap();
        Self {
            _home: home,
            env,
            project,
        }
    }

    fn transcript(&self, uuid: &str, lines: &[String]) -> PathBuf {
        let path = self.project.join(format!("{uuid}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn run(&self, request: AgentRequest) -> AgentResponse {
        let response = execute(&self.env, &ConfigFile::default(), request.clone());
        // The text is exactly what the text-only entry point returns.
        match execute_text(&self.env, &ConfigFile::default(), request) {
            Ok(text) => assert_eq!(response.stdout, text),
            Err(error) => assert_eq!(response.stderr, error.to_string()),
        }
        response
    }
}

fn conversation_lines(turns: usize, topic: &str) -> Vec<String> {
    (1..=turns)
        .flat_map(|turn| {
            [
                user_jsonl_line(&format!("{topic} question {turn}")),
                assistant_jsonl_line(&format!("{topic} answer {turn}\nsecond line {turn}")),
            ]
        })
        .collect()
}

fn search_result(response: &AgentResponse) -> &AgentSearchResult {
    match response.result.as_ref().expect("success") {
        AgentResult::Search(result) | AgentResult::Within(result) => result,
        other => panic!("expected a search result, got {other:?}"),
    }
}

/// Every hit and group resolves to a conversation, and the protocol text
/// mentions every structured hit.
fn assert_search_consistent(response: &AgentResponse) {
    let result = search_result(response);
    let hits = result.hits();
    assert!(!hits.is_empty(), "{}", response.stdout);
    for hit in hits {
        let conversation = result
            .conversation(&hit.conversation_ref)
            .unwrap_or_else(|| panic!("unresolved {}", hit.conversation_ref));
        assert!(conversation.path.is_file(), "{:?}", conversation.path);
        assert_eq!(conversation.uuid, hit.conversation_uuid);
        assert!(
            response.stdout.contains(&format!(
                "read ref={}:m{}..m{} focus=m{}..m{}",
                hit.conversation_ref,
                hit.read_range.start,
                hit.read_range.end,
                hit.focus_range.start,
                hit.focus_range.end
            )),
            "{}",
            response.stdout
        );
        for anchor in &hit.anchors {
            assert!(response.stdout.contains(anchor.as_str()));
        }
    }
    for group in &result.output.groups {
        assert!(result.conversation(&group.conversation_ref).is_some());
    }
}

#[test]
fn search_returns_ranked_conversations_with_paths() {
    let sandbox = Sandbox::new();
    let a = sandbox.transcript(UUID_A, &conversation_lines(2, "kangaroo"));
    let b = sandbox.transcript(UUID_B, &conversation_lines(1, "kangaroo wombat"));

    let response = sandbox.run(AgentRequest::Search(AgentSearchArgs::new("kangaroo")));
    assert!(response.is_success(), "{}", response.stderr);
    assert_eq!(response.exit_code, 0);
    assert!(response.stderr.is_empty());
    assert_eq!(response.text(), response.stdout);
    assert!(
        response
            .stdout
            .starts_with("protocol agent-search mode=lexical")
    );
    let AgentResult::Search(result) = response.result.as_ref().unwrap() else {
        panic!("expected search");
    };
    assert_eq!(result.output.query, "kangaroo");
    assert_eq!(result.output.groups.len(), 2);
    let paths: Vec<&Path> = result
        .conversations
        .iter()
        .map(|conversation| conversation.path.as_path())
        .collect();
    assert!(paths.contains(&a.as_path()) && paths.contains(&b.as_path()));
    assert!(
        result
            .conversations
            .iter()
            .all(|c| c.source == Source::Claude)
    );
    assert_search_consistent(&response);

    let flat = sandbox.run(AgentRequest::Search(AgentSearchArgs {
        flat: true,
        ..AgentSearchArgs::new("wombat")
    }));
    let result = search_result(&flat);
    assert!(result.output.flat && result.output.groups.is_empty());
    assert_eq!(result.conversations.len(), 1);
    assert_eq!(result.conversations[0].path, b);
    assert_search_consistent(&flat);
}

#[test]
fn search_hits_survive_a_budget_cut_in_the_structured_result() {
    let sandbox = Sandbox::new();
    for (index, uuid) in [UUID_A, UUID_B].into_iter().enumerate() {
        sandbox.transcript(uuid, &conversation_lines(3, &format!("platypus {index}")));
    }
    let response = sandbox.run(AgentRequest::Search(AgentSearchArgs {
        budget: Some(300),
        ..AgentSearchArgs::new("platypus")
    }));
    assert!(response.is_success(), "{}", response.stderr);
    assert!(response.stdout.contains("cut=tail"), "{}", response.stdout);
    assert!(response.stdout.chars().count() <= 300);
    assert_eq!(search_result(&response).output.groups.len(), 2);
}

#[test]
fn within_reports_the_target_conversation() {
    let sandbox = Sandbox::new();
    let path = sandbox.transcript(UUID_A, &conversation_lines(4, "echidna"));

    let response = sandbox.run(AgentRequest::Within(AgentWithinArgs::new(
        UUID_A,
        "echidna answer 3",
    )));
    assert!(response.is_success(), "{}", response.stderr);
    let AgentResult::Within(result) = response.result.as_ref().unwrap() else {
        panic!("expected within");
    };
    let target = result.output.target.as_ref().unwrap();
    assert_eq!(target.conversation_uuid, UUID_A);
    let conversation = result.conversation(&target.conversation_ref).unwrap();
    assert_eq!(conversation.path, path);
    assert_eq!(conversation.session_filename, format!("{UUID_A}.jsonl"));
    assert_search_consistent(&response);
}

#[test]
fn read_returns_rendered_messages_with_anchors_and_focus() {
    let sandbox = Sandbox::new();
    let path = sandbox.transcript(UUID_A, &conversation_lines(3, "quokka"));

    let mut read = AgentReadArgs::new(vec![format!("{UUID_A}:m2..m5")]);
    read.focus = Some("m3".to_string());
    let response = sandbox.run(AgentRequest::Read(read));
    assert!(response.is_success(), "{}", response.stderr);
    let AgentResult::Read(result) = response.result.as_ref().unwrap() else {
        panic!("expected read");
    };
    assert_eq!(result.conversations.len(), 1);
    assert_eq!(result.conversations[0].path, path);
    let ordinals: Vec<usize> = result.messages.iter().map(|m| m.ordinal).collect();
    assert_eq!(ordinals, vec![2, 3, 4, 5]);
    assert_eq!(result.messages[0].role, AgentMessageRole::Assistant);
    assert_eq!(result.messages[1].role, AgentMessageRole::User);
    assert_eq!(result.messages[1].body, "quokka question 2");
    assert_eq!(result.messages[0].body, "quokka answer 1\nsecond line 1");
    let focused: Vec<usize> = result
        .messages
        .iter()
        .filter(|message| message.focused)
        .map(|message| message.ordinal)
        .collect();
    assert_eq!(focused, vec![3]);
    assert_eq!(result.focus.as_ref().unwrap().range.start, 3);
    assert!(result.slice.is_none());
    assert_eq!(result.options.budget, Some(6000));
    for message in &result.messages {
        assert_eq!(message.conversation_ref, result.conversations[0].reference);
        assert!(!message.subagent);
        assert!(
            response
                .stdout
                .contains(&format!("message m{} role=", message.ordinal)),
            "{}",
            response.stdout
        );
        assert!(
            response
                .stdout
                .contains(&format!("anchor={}", message.anchor))
        );
        for line in message.body.lines() {
            assert!(response.stdout.contains(&format!("| {line}\n")));
        }
    }
}

#[test]
fn read_with_match_slices_the_message() {
    let sandbox = Sandbox::new();
    sandbox.transcript(UUID_A, &conversation_lines(1, "bilby"));
    let mut read = AgentReadArgs::new(vec![format!("{UUID_A}:m2")]);
    read.match_query = Some("second".to_string());
    read.context = 0;
    let response = sandbox.run(AgentRequest::Read(read));
    assert!(response.is_success(), "{}", response.stderr);
    let AgentResult::Read(result) = response.result.as_ref().unwrap() else {
        panic!("expected read");
    };
    assert_eq!(
        result.slice,
        Some(ReadSlice::Match {
            query: "second".to_string(),
            context: 0
        })
    );
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].body, ">2: second line 1");
    assert_eq!(
        result.messages[0].slice.as_deref(),
        Some("match=second context=0 hits=1")
    );
    assert!(
        response
            .stdout
            .contains("slice=match=second context=0 hits=1")
    );
}

#[test]
fn outline_returns_message_records_or_segments() {
    let sandbox = Sandbox::new();
    let path = sandbox.transcript(UUID_A, &conversation_lines(2, "dingo"));
    let response = sandbox.run(AgentRequest::Outline(AgentOutlineArgs::new(UUID_A)));
    assert!(response.is_success(), "{}", response.stderr);
    let AgentResult::Outline(result) = response.result.as_ref().unwrap() else {
        panic!("expected outline");
    };
    assert_eq!(result.conversation.path, path);
    assert_eq!(result.total_messages, 4);
    assert_eq!(result.visible_messages, 4);
    assert_eq!(result.items.len(), 4);
    assert!(matches!(
        &result.items[0],
        OutlineItem::Message { ordinal: 1, role: AgentMessageRole::User, snippet, .. }
            if snippet == "dingo question 1"
    ));
    for item in &result.items {
        assert!(
            response.stdout.contains(&item.record()),
            "{}",
            response.stdout
        );
    }

    // More than 20 visible messages are outlined in segments of ten.
    sandbox.transcript(UUID_B, &conversation_lines(12, "numbat"));
    let response = sandbox.run(AgentRequest::Outline(AgentOutlineArgs::new(UUID_B)));
    let AgentResult::Outline(result) = response.result.as_ref().unwrap() else {
        panic!("expected outline");
    };
    assert_eq!(result.visible_messages, 24);
    assert_eq!(result.items.len(), 3);
    assert!(matches!(
        result.items[2],
        OutlineItem::Segment {
            start: 21,
            end: 24,
            ..
        }
    ));
    for item in &result.items {
        assert!(response.stdout.contains(&item.record()));
    }
}

#[test]
fn failures_carry_the_structured_error_and_the_envelope() {
    let sandbox = Sandbox::new();
    sandbox.transcript(UUID_A, &conversation_lines(1, "x"));

    let response = sandbox.run(AgentRequest::Read(AgentReadArgs::new(vec![format!(
        "{UUID_A}:m1..m9"
    )])));
    assert!(!response.is_success());
    assert_eq!(response.exit_code, 1);
    assert!(response.result.is_none() && response.stdout.is_empty());
    let error = response.error.as_ref().unwrap();
    assert_eq!(error.kind, AgentErrorKind::OutOfRange);
    assert!(
        response
            .stderr
            .starts_with("protocol agent-error kind=out-of-range")
    );
    assert_eq!(response.text(), response.stderr);
    assert_eq!(response.error_text(), response.stderr);

    let missing = sandbox.run(AgentRequest::Outline(AgentOutlineArgs::new(UUID_B)));
    assert_eq!(
        missing.error.as_ref().unwrap().kind,
        AgentErrorKind::NotFound
    );

    let invalid = sandbox.run(AgentRequest::Search(AgentSearchArgs::new("   ")));
    assert_eq!(
        invalid.error.as_ref().unwrap().kind,
        AgentErrorKind::InvalidRef
    );
}

#[test]
fn pi_sessions_resolve_to_their_transcripts() {
    let sandbox = Sandbox::new();
    let sessions = sandbox.project.parent().unwrap().join("pi-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v3-branched.jsonl");
    let pi = sessions.join("pi.jsonl");
    std::fs::copy(fixture, &pi).unwrap();
    let env = sandbox.env.clone().with_pi_session_dir(&sessions);

    let response = execute(
        &env,
        &ConfigFile::default(),
        AgentRequest::Search(AgentSearchArgs {
            search_mode: crate::agent::args::AgentSearchModeArgs {
                lexical: true,
                ..Default::default()
            },
            ..AgentSearchArgs::new("active root question")
        }),
    );
    assert!(response.is_success(), "{}", response.stderr);
    let result = search_result(&response);
    let conversation = result
        .conversations
        .iter()
        .find(|conversation| conversation.path == pi)
        .unwrap_or_else(|| panic!("{:?}", result.conversations));
    assert!(matches!(conversation.source, Source::Pi | Source::Omp));
    assert_eq!(conversation.uuid, "01912345-6789-7abc-8def-0123456789ab");
}

struct ConstantEmbedder;

impl SemanticEmbedder for ConstantEmbedder {
    fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(passages.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
        Ok(Some(vec![1.0, 0.0]))
    }
}

#[test]
fn semantic_notices_go_to_stderr_and_into_the_response() {
    let sandbox = Sandbox::new();
    sandbox.transcript(UUID_A, &conversation_lines(2, "semantic needle"));
    let mut within = AgentWithinArgs::new(UUID_A, "semantic needle");
    within.search_mode.semantic = true;

    let mut service = AgentService::new(sandbox.env.clone(), ConfigFile::default())
        .with_embedder_factory(Box::new(|| Ok(Box::new(ConstantEmbedder))));
    let response = service.respond(AgentRequest::Within(within));
    assert!(response.is_success(), "{}", response.stderr);
    assert!(
        response
            .stdout
            .starts_with("protocol agent-within mode=semantic")
    );
    assert!(!response.notices.is_empty());
    assert!(
        response
            .notices
            .iter()
            .any(|notice| matches!(notice, AgentNotice::SemanticProgress(_)))
    );
    let expected: String = response
        .notices
        .iter()
        .map(|notice| format!("{notice}\n"))
        .collect();
    assert_eq!(response.stderr, expected);
    assert_eq!(response.error_text(), "");
    // The response took the notices.
    assert!(service.take_notices().is_empty());
    assert_search_consistent(&response);
}
