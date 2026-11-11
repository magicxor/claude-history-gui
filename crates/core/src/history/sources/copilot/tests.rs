use super::*;
use crate::history::adapter::{Completeness, is_cancelled};
use crate::history::index;
use crate::history::model::{
    EventKind, ExecutionMode, HeadStatus, Message, Part, Relationship, Role, Usage, Visibility,
};
use crate::history::sources::Registry;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::fs;

const PLAIN: &str = "0c0a0000-0000-4000-8000-000000000001";
const SUBAGENTS: &str = "0c0a0000-0000-4000-8000-000000000002";
const MOVED: &str = "0c0a0000-0000-4000-8000-000000000003";
const UNUSED: &str = "0c0a0000-0000-4000-8000-000000000004";
const TORN: &str = "0c0a0000-0000-4000-8000-000000000005";
/// A session id of no fixture, for paths that are never read.
const LAYOUT_ID: &str = "0f8e7d6c-5b4a-4932-8170-a1b2c3d4e5f6";

pub(crate) fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/copilot")
        .join(SESSION_STATE_DIR)
}

/// A copy of the fixture sessions in `home`'s `session-state`, for tests
/// that change them or read them through an env.
pub(crate) fn root_copy(home: &Path) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let target = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy(&path, &target);
            } else {
                fs::copy(&path, &target).unwrap();
            }
        }
    }
    let root = home.join(SESSION_STATE_DIR);
    copy(&fixture_root(), &root);
    root
}

fn adapter() -> CopilotAdapter {
    CopilotAdapter::new(fixture_root())
}

fn far_future() -> SystemTime {
    SystemTime::now() + std::time::Duration::from_secs(3600)
}

fn decoded_in(adapter: &CopilotAdapter, session: &str) -> Transcript {
    let located = adapter.resolve(session, far_future()).unwrap();
    adapter.decode(&located).unwrap().unwrap()
}

fn decoded(session: &str) -> Transcript {
    decoded_in(&adapter(), session)
}

/// Writes a session of `root` with these log lines and `workspace.yaml`.
fn session(root: &Path, id: &str, lines: &[serde_json::Value], workspace: Option<&str>) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    let log: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(dir.join(EVENTS_FILE), log).unwrap();
    if let Some(workspace) = workspace {
        fs::write(dir.join(WORKSPACE_FILE), workspace).unwrap();
    }
}

fn start(id: &str) -> serde_json::Value {
    json!({ "type": "session.start", "id": "e1", "parentId": null,
            "data": { "sessionId": id, "context": { "cwd": "/work/demo" } } })
}

fn time(text: &str) -> Option<DateTime<Utc>> {
    Some(
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc),
    )
}

/// The main agent's messages of this visibility: role and parts.
fn messages(transcript: &Transcript, visibility: Visibility) -> Vec<(Role, &[Part])> {
    transcript
        .events
        .iter()
        .filter(|event| event.visibility == visibility)
        .filter_map(|event| match &event.kind {
            EventKind::Message(Message { role, parts, .. }) => Some((*role, parts.as_slice())),
            _ => None,
        })
        .collect()
}

fn text(text: &str) -> Part {
    Part::Text(text.to_owned())
}

#[test]
fn discovery_lists_the_uuid_directories_that_hold_a_log() {
    let discovery = adapter().discover(far_future());
    assert_eq!(discovery.completeness, Completeness::Complete);
    let mut ids: Vec<&str> = discovery
        .found
        .iter()
        .map(|located| located.key.native_id.as_str())
        .collect();
    ids.sort_unstable();
    // Not the session that was never used, nor the directory that is not
    // named by a UUID.
    assert_eq!(ids, [PLAIN, SUBAGENTS, MOVED, TORN]);
    for located in &discovery.found {
        assert_eq!(located.key.instance.kind, SourceKind::Copilot);
        assert_eq!(
            located.path,
            fixture_root()
                .join(&located.key.native_id)
                .join(EVENTS_FILE)
        );
        assert!(located.stamp.is_known());
    }

    let missing = CopilotAdapter::new(fixture_root().join("missing")).discover(far_future());
    assert!(missing.found.is_empty());
    assert_eq!(missing.completeness, Completeness::Complete);

    let resolved = adapter().resolve(PLAIN, far_future()).unwrap();
    assert_eq!(resolved.path, fixture_root().join(PLAIN).join(EVENTS_FILE));
    for absent in [
        UNUSED,
        "not-a-session",
        "..",
        "0c0a0000-0000-4000-8000-0000000000ff",
    ] {
        assert!(adapter().resolve(absent, far_future()).is_err(), "{absent}");
    }
}

#[test]
fn the_stamp_covers_the_log_and_workspace_yaml() {
    let home = tempfile::tempdir().unwrap();
    let adapter = CopilotAdapter::new(root_copy(home.path()));
    let before = adapter.resolve(PLAIN, far_future()).unwrap();
    let log = fs::metadata(&before.path).unwrap().len();
    assert_eq!(before.size, log);
    assert!(before.stamp.size > log);

    let workspace = before.path.with_file_name(WORKSPACE_FILE);
    fs::write(&workspace, "name: Renamed in the CLI\nuser_named: true\n").unwrap();
    let after = adapter.resolve(PLAIN, far_future()).unwrap();
    assert_ne!(after.stamp, before.stamp);
    assert_eq!(
        decoded_in(&adapter, PLAIN).meta.custom_title.as_deref(),
        Some("Renamed in the CLI")
    );

    // A session without the file is stamped by its log alone.
    let torn = adapter.resolve(TORN, far_future()).unwrap();
    assert_eq!(torn.stamp.size, torn.size);
}

#[test]
fn messages_reasoning_tool_calls_and_results_decode() {
    let transcript = decoded(PLAIN);
    assert_eq!(transcript.source, SourceKind::Copilot);
    assert_eq!(transcript.key.native_id, PLAIN);
    assert_eq!(transcript.head, HeadStatus::Current);
    assert_eq!(transcript.mode, ExecutionMode::Unknown);
    assert_eq!(transcript.relationship, Relationship::Root);
    assert_eq!(
        transcript.path(),
        fixture_root().join(PLAIN).join(EVENTS_FILE)
    );
    assert!(transcript.diagnostics.is_empty());

    let view = Part::ToolCall {
        id: "call_0001".to_owned(),
        name: "view".to_owned(),
        input: json!({ "path": "/work/demo/src/cache.rs", "view_range": [1, 40] }),
    };
    let shell = Part::ToolCall {
        id: "call_0002".to_owned(),
        name: "powershell".to_owned(),
        input: json!({ "command": "cargo test eviction",
                       "description": "Run the eviction test", "initial_wait": 30 }),
    };
    let listing = Part::ToolResult {
        call_id: "call_0001".to_owned(),
        output: Some(json!(
            "1. pub struct Cache { entries: Vec<Entry> }\n2. // evicts the least recently used entry"
        )),
    };
    let failure = Part::ToolResult {
        call_id: "call_0002".to_owned(),
        output: Some(json!("Error: cargo: command not found")),
    };
    assert_eq!(
        messages(&transcript, Visibility::Dialogue),
        [
            (
                Role::User,
                &[text("Explain how the ocelot cache evicts entries")][..]
            ),
            (
                Role::Assistant,
                &[
                    Part::Thinking(
                        "The user wants the eviction policy; cache.rs should hold it.".to_owned()
                    ),
                    text("I'll read the cache module first."),
                    view,
                ][..]
            ),
            (Role::User, &[listing][..]),
            (
                Role::Assistant,
                &[text(
                    "The ocelot cache evicts the least recently used entry once it is full."
                )][..]
            ),
            (Role::User, &[text("Thanks, now run the eviction test")][..]),
            (Role::Assistant, &[shell][..]),
            (Role::User, &[failure][..]),
            (
                Role::Assistant,
                &[text(
                    "I could not run cargo here; the eviction test lives in tests/eviction.rs."
                )][..]
            ),
        ]
    );

    // A message carries its id and model, and its line in the log.
    let first_answer = transcript
        .events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::Message(message) if message.role == Role::Assistant => {
                Some((event, message))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(first_answer.0.origin.line, 6);
    assert_eq!(first_answer.0.timestamp, time("2026-10-01T08:00:10Z"));
    assert_eq!(
        first_answer.1.id.as_deref(),
        Some("m0000001-0000-4000-8000-000000000001")
    );
    assert_eq!(first_answer.1.model.as_deref(), Some("claude-sonnet-4.5"));

    let conversation = index::index(&transcript).unwrap();
    assert_eq!(conversation.source, SourceKind::Copilot);
    assert_eq!(conversation.tool_call_count, 2);
    assert!(conversation.full_text.contains("ocelot"));
    assert!(conversation.full_text.contains("least recently used entry"));
}

#[test]
fn hooks_and_unknown_records_yield_no_events() {
    let transcript = decoded(PLAIN);
    assert_eq!(transcript.records, 18);
    assert_eq!(transcript.events.len(), 10);
    assert!(
        transcript
            .events
            .iter()
            .all(|event| !matches!(event.kind, EventKind::Other))
    );
    let notes: Vec<(&str, &str, Option<Usage>)> = transcript
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Note {
                label,
                text,
                searchable: false,
                usage,
            } => Some((label.as_str(), text.as_str(), *usage)),
            _ => None,
        })
        .collect();
    // The input count of a usage record includes the cached tokens.
    let usage = Usage {
        input: 300,
        output: 42,
        cache_creation: 100,
        cache_read: 800,
    };
    assert_eq!(
        notes,
        [("Token count", "", Some(usage)), ("Model", "gpt-5.4", None)]
    );
}

#[test]
fn the_session_is_described_by_its_start_and_workspace_yaml() {
    let plain = decoded(PLAIN).meta;
    assert_eq!(plain.session_id, PLAIN);
    assert_eq!(plain.cwd.as_deref(), Some(Path::new("/work/demo")));
    assert_eq!(plain.project, plain.cwd);
    assert_eq!(plain.cwds, [PathBuf::from("/work/demo")]);
    // The model it last changed to.
    assert_eq!(plain.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(plain.custom_title.as_deref(), Some("Ocelot cache eviction"));
    assert_eq!(plain.summary, None);
    // Its last message, not the shutdown record after it.
    assert_eq!(plain.updated_at, time("2026-10-01T08:03:00Z"));

    let moved = decoded(MOVED).meta;
    assert_eq!(
        moved.cwds,
        ["/work/demo", "/work/other", "/work/third"].map(PathBuf::from)
    );
    assert_eq!(moved.cwd.as_deref(), Some(Path::new("/work/demo")));
    // The model it was resumed with.
    assert_eq!(moved.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(moved.custom_title, None);
    assert_eq!(
        moved.summary.as_deref(),
        Some("Refactor the heron scheduler: \"queue\" first")
    );
    assert_eq!(moved.updated_at, time("2026-10-03T09:00:10Z"));

    // A block scalar is one line as a title.
    assert_eq!(
        decoded(SUBAGENTS).meta.summary.as_deref(),
        Some("Survey the lemur parser with subagents")
    );
}

#[test]
fn names_are_custom_titles_only_when_the_user_chose_them() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(SESSION_STATE_DIR);
    let lines = [start(PLAIN)];
    let title = |workspace: &str| {
        session(&root, PLAIN, &lines, Some(workspace));
        let meta = decoded_in(&CopilotAdapter::new(root.clone()), PLAIN).meta;
        (meta.custom_title, meta.summary)
    };
    let some = |text: &str| Some(text.to_owned());
    assert_eq!(
        title("name: Generated name\nuser_named: false\n"),
        (None, some("Generated name"))
    );
    assert_eq!(
        title("name: Generated name\n"),
        (None, some("Generated name"))
    );
    assert_eq!(
        title("name: My name\nuser_named: true\nsummary: Older summary\n"),
        (some("My name"), some("Older summary"))
    );
    assert_eq!(
        title("summary: |-\n  First line\n  second line\n"),
        (None, some("First line second line"))
    );
    assert_eq!(title("user_named: true\n"), (None, None));
    assert_eq!(title("\u{0}not yaml: [\n"), (None, None));
}

#[test]
fn subagent_records_are_attributed_to_their_agent() {
    let transcript = decoded(SUBAGENTS);
    let subagents: Vec<(&str, Role, &[Part])> = transcript
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Subagent { agent_id, message } => {
                assert_eq!(event.visibility, Visibility::Dialogue);
                Some((agent_id.as_str(), message.role, message.parts.as_slice()))
            }
            _ => None,
        })
        .collect();
    let search = Part::ToolCall {
        id: "call_sub1".to_owned(),
        name: "grep".to_owned(),
        input: json!({ "pattern": "pub fn parse_", "path": "/work/demo/src",
                       "output_mode": "content" }),
    };
    let found = Part::ToolResult {
        call_id: "call_sub1".to_owned(),
        output: Some(json!(
            "src/parser.rs:12:pub fn parse_document\nsrc/parser.rs:40:pub fn parse_fragment"
        )),
    };
    assert_eq!(
        subagents,
        [
            // Marked by its agent id alone, by both marks, and (an older
            // CLI) by the tool call that started it alone.
            (
                "explore",
                Role::User,
                &[text("List the lemur parser entry points")][..]
            ),
            (
                "explore",
                Role::Assistant,
                &[text("Searching for the gibbon entry points."), search][..]
            ),
            ("explore", Role::User, &[found][..]),
            (
                "general-purpose",
                Role::Assistant,
                &[text("The tarsier tests are in tests/parser.rs.")][..]
            ),
        ]
    );

    // The main agent's own dialogue, with the results of its task calls.
    // The user's prompt names a parent task (`parentAgentTaskId`), as a
    // newer CLI's prompts do: that does not make it a subagent's.
    let main = messages(&transcript, Visibility::Dialogue);
    assert_eq!(main.len(), 5);
    assert_eq!(
        main[0],
        (
            Role::User,
            &[text("Survey the lemur parser with subagents")][..]
        )
    );
    assert!(matches!(main[2].1, [Part::ToolResult { call_id, .. }] if call_id == "call_task1"));

    // Subagent text is found by agent search, not by the list's.
    let conversation = index::index(&transcript).unwrap();
    assert!(conversation.full_text.contains("lemur"));
    for word in ["gibbon", "tarsier"] {
        assert!(!conversation.full_text.contains(word), "{word}");
        assert!(conversation.agent_search_text.contains(word), "{word}");
    }
}

#[test]
fn a_subagent_without_a_recorded_start_is_still_a_subagent() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(SESSION_STATE_DIR);
    session(
        &root,
        PLAIN,
        &[
            start(PLAIN),
            json!({ "type": "user.message", "id": "e2", "data": { "content": "Ask the okapi" } }),
            json!({ "type": "assistant.message", "id": "e3",
                    "data": { "parentToolCallId": "call_lost", "content": "Okapi here" } }),
        ],
        None,
    );
    let transcript = decoded_in(&CopilotAdapter::new(root), PLAIN);
    assert!(matches!(
        &transcript.events[1].kind,
        EventKind::Subagent { agent_id, .. } if agent_id == "agent"
    ));
}

#[test]
fn what_the_cli_tells_the_model_is_not_the_users_dialogue() {
    let transcript = decoded(MOVED);
    assert_eq!(
        messages(&transcript, Visibility::Injected),
        [(
            Role::User,
            &[text("The conversation was compacted to save context.")][..]
        )]
    );
    assert_eq!(
        messages(&transcript, Visibility::Dialogue)
            .iter()
            .filter(|(role, _)| *role == Role::User)
            .count(),
        2
    );
    assert!(transcript.events.iter().any(|event| matches!(
        &event.kind,
        EventKind::Note { label, text, searchable: false, usage: None }
            if label == "Compaction" && text.contains("pelican queue")
    )));
    let conversation = index::index(&transcript).unwrap();
    assert!(conversation.full_text.contains("heron"));
    assert!(!conversation.full_text.contains("compacted to save context"));
    assert!(!conversation.full_text.contains("pelican"));
}

#[test]
fn malformed_and_torn_records_are_soft_diagnostics() {
    let transcript = decoded(TORN);
    let diagnostics: Vec<(usize, &str)> = transcript
        .diagnostics
        .iter()
        .map(|error| (error.line_number, error.error_message.as_str()))
        .collect();
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].0, 3);
    assert!(diagnostics[0].1.starts_with("malformed Copilot event"));
    assert_eq!(
        diagnostics[1],
        (5, "incomplete last record, still being written")
    );
    assert_eq!(transcript.records, 5);
    assert_eq!(
        messages(&transcript, Visibility::Dialogue),
        [
            (Role::User, &[text("Count the ibis nests")][..]),
            (Role::Assistant, &[text("There are three ibis nests.")][..]),
        ]
    );
    // No `workspace.yaml`: no title, and the conversation still indexes.
    assert_eq!(transcript.meta.custom_title, None);
    assert_eq!(transcript.meta.summary, None);
    assert!(
        index::index(&transcript)
            .unwrap()
            .full_text
            .contains("ibis")
    );
}

#[test]
fn sessions_without_messages_were_last_active_at_their_last_record() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(SESSION_STATE_DIR);
    let adapter = CopilotAdapter::new(root.clone());
    let info = json!({ "type": "session.info", "id": "e2", "data": {},
                       "timestamp": "2026-10-07T10:00:00.000Z" });

    session(
        &root,
        PLAIN,
        &[start(PLAIN), info],
        Some("updated_at: 2026-10-08T10:00:00.000Z\n"),
    );
    assert_eq!(
        decoded_in(&adapter, PLAIN).meta.updated_at,
        time("2026-10-07T10:00:00Z")
    );

    // Without a timestamp in the log, the time `workspace.yaml` names.
    session(
        &root,
        SUBAGENTS,
        &[start(SUBAGENTS)],
        Some("updated_at: 2026-10-08T10:00:00.000Z\n"),
    );
    assert_eq!(
        decoded_in(&adapter, SUBAGENTS).meta.updated_at,
        time("2026-10-08T10:00:00Z")
    );

    // Else the log's mtime.
    session(&root, MOVED, &[start(MOVED)], None);
    let located = adapter.resolve(MOVED, far_future()).unwrap();
    assert_eq!(
        decoded_in(&adapter, MOVED).meta.updated_at,
        located.modified.map(DateTime::<Utc>::from)
    );
}

#[test]
fn a_log_that_does_not_start_with_an_event_holds_no_conversation() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(SESSION_STATE_DIR);
    let adapter = CopilotAdapter::new(root.clone());
    session(
        &root,
        PLAIN,
        &[json!({ "type": "user", "message": { "role": "user", "content": "hello" } })],
        None,
    );
    let located = adapter.resolve(PLAIN, far_future()).unwrap();
    assert!(adapter.decode(&located).unwrap().is_none());

    session(&root, SUBAGENTS, &[], None);
    let located = adapter.resolve(SUBAGENTS, far_future()).unwrap();
    assert!(adapter.decode(&located).unwrap().is_none());
}

#[test]
fn decoding_stops_when_it_is_cancelled() {
    let adapter = adapter();
    let located = adapter.resolve(PLAIN, far_future()).unwrap();
    let error = adapter.decode_cancellable(&located, &|| true).unwrap_err();
    assert!(is_cancelled(&error));
    let transcript = adapter
        .decode_cancellable(&located, &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(transcript.events, decoded(PLAIN).events);
}

#[test]
fn registries_key_logs_by_session_id() {
    let root = fixture_root();
    let registry = Registry::with_roots(None, None, None).with_copilot(Some(root.clone()));
    let key = registry
        .key_of(&root.join(PLAIN).join(EVENTS_FILE))
        .unwrap();
    assert_eq!(key.instance.kind, SourceKind::Copilot);
    assert_eq!(key.instance.root, root);
    assert_eq!(key.native_id, PLAIN);
    assert_eq!(
        registry.key_of(&root.join(PLAIN).join(WORKSPACE_FILE)),
        None
    );
    assert_eq!(
        registry.key_of(&root.join("not-a-session").join(EVENTS_FILE)),
        None
    );
}

#[test]
fn the_env_selects_the_home() {
    let home = tempfile::tempdir().unwrap();
    let env = HistoryEnv::sandbox(home.path());
    let root = root_copy(&home.path().join(".copilot"));
    let registry = Registry::from_env(&env);
    let adapter = registry
        .adapters()
        .iter()
        .find(|adapter| adapter.instance().kind == SourceKind::Copilot)
        .unwrap();
    assert_eq!(adapter.instance().root, root);
    assert_eq!(adapter.discover(far_future()).found.len(), 4);

    let elsewhere = tempfile::tempdir().unwrap();
    let env = env.with_copilot_home(elsewhere.path().to_path_buf());
    let adapter = CopilotAdapter::from_env(&env).unwrap();
    assert_eq!(
        adapter.instance().root,
        elsewhere.path().join(SESSION_STATE_DIR)
    );
    assert!(adapter.discover(far_future()).found.is_empty());
}

#[test]
fn logs_opened_by_path_decode_as_their_session() {
    let path = fixture_root().join(PLAIN).join(EVENTS_FILE);
    let transcript = crate::history::sources::decode_path(&path, None, false).unwrap();
    let through_adapter = decoded(PLAIN);
    assert_eq!(transcript.key, through_adapter.key);
    assert_eq!(transcript.revision, through_adapter.revision);
    assert_eq!(transcript.meta, through_adapter.meta);
    assert_eq!(transcript.events, through_adapter.events);
    assert!(!crate::history::view::entries(&transcript).is_empty());

    let conversation = crate::history::process_conversation_file(path.clone(), None, None)
        .unwrap()
        .unwrap();
    assert_eq!(conversation.source, SourceKind::Copilot);
    assert_eq!(conversation.session_id, PLAIN);
    assert_eq!(
        conversation.custom_title.as_deref(),
        Some("Ocelot cache eviction")
    );

    let cancelled =
        crate::history::sources::decode_path_cancellable(&path, None, false, &|| true).unwrap_err();
    assert!(is_cancelled(&cancelled));

    assert_eq!(
        view::workspace_file_of(&path),
        Some(fixture_root().join(PLAIN).join(WORKSPACE_FILE))
    );
    assert_eq!(
        view::workspace_file_of(&path.with_file_name("other.jsonl")),
        None
    );
}

/// Decoding passes over a line that is no record, and so does telling
/// whether a file opened by path is a Copilot log.
#[test]
fn a_log_is_recognized_past_a_malformed_first_line() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(SESSION_STATE_DIR);
    let session_dir = root.join(PLAIN);
    fs::create_dir_all(&session_dir).unwrap();
    let path = session_dir.join(EVENTS_FILE);
    let question =
        json!({ "type": "user.message", "id": "e2", "data": { "content": "Ask the quokka" } });
    fs::write(&path, format!("not json\n\n{}\n{question}\n", start(PLAIN))).unwrap();

    let by_path = crate::history::sources::decode_path(&path, None, false).unwrap();
    assert_eq!(by_path.source, SourceKind::Copilot);
    assert_eq!(by_path.diagnostics.len(), 1);
    assert_eq!(
        messages(&by_path, Visibility::Dialogue),
        [(Role::User, &[text("Ask the quokka")][..])]
    );
    let through_adapter = decoded_in(&CopilotAdapter::new(root), PLAIN);
    assert_eq!(by_path.key, through_adapter.key);
    assert_eq!(by_path.events, through_adapter.events);

    // Another tool's records after such a line are still not a Copilot log.
    let other = dir.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let path = other.join(EVENTS_FILE);
    fs::write(
        &path,
        "not json\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
    )
    .unwrap();
    let transcript = crate::history::sources::decode_path(&path, None, false).unwrap();
    assert_eq!(transcript.source, SourceKind::Claude);
}

#[test]
fn a_copied_log_decodes_without_its_session_directory() {
    let dir = tempfile::tempdir().unwrap();
    let loose = dir.path().join("exported");
    fs::create_dir_all(&loose).unwrap();
    let path = loose.join(EVENTS_FILE);
    fs::copy(fixture_root().join(PLAIN).join(EVENTS_FILE), &path).unwrap();
    let transcript = crate::history::sources::decode_path(&path, None, false).unwrap();
    assert_eq!(transcript.source, SourceKind::Copilot);
    assert_eq!(transcript.key.instance.root, dir.path());
    assert_eq!(transcript.key.native_id, "exported");
    // The session id is the one the log records.
    assert_eq!(transcript.meta.session_id, PLAIN);
    assert_eq!(transcript.meta.custom_title, None);
    assert_eq!(transcript.events, decoded(PLAIN).events);

    // Another tool's file of that name is not a Copilot log.
    let claude = dir.path().join("claude");
    fs::create_dir_all(&claude).unwrap();
    let path = claude.join(EVENTS_FILE);
    fs::write(
        &path,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
    )
    .unwrap();
    let transcript = crate::history::sources::decode_path(&path, None, false).unwrap();
    assert_eq!(transcript.source, SourceKind::Claude);
}

fn layout_root() -> PathBuf {
    Path::new("home").join(".copilot").join(SESSION_STATE_DIR)
}

#[test]
fn session_of_accepts_only_the_exact_layout() {
    let events = layout_root().join(LAYOUT_ID).join(EVENTS_FILE);
    assert_eq!(
        session_of(&events),
        Some((layout_root(), LAYOUT_ID.to_owned()))
    );
    let upper = layout_root()
        .join(LAYOUT_ID.to_uppercase())
        .join(EVENTS_FILE);
    assert_eq!(
        session_of(&upper),
        Some((layout_root(), LAYOUT_ID.to_uppercase()))
    );

    assert_eq!(
        session_of(&layout_root().join(LAYOUT_ID).join("workspace.yaml")),
        None
    );
    assert_eq!(
        session_of(&layout_root().join("not-a-uuid").join(EVENTS_FILE)),
        None
    );
    assert_eq!(
        session_of(&layout_root().join(&LAYOUT_ID[1..]).join(EVENTS_FILE)),
        None
    );
    assert_eq!(
        session_of(
            &layout_root()
                .join(LAYOUT_ID.replace('-', "_"))
                .join(EVENTS_FILE)
        ),
        None
    );
    assert_eq!(session_of(&layout_root().join(EVENTS_FILE)), None);
    assert_eq!(session_of(Path::new(EVENTS_FILE)), None);
}

#[test]
fn native_id_is_the_uuid_of_a_log_directly_under_the_root() {
    let adapter = CopilotAdapter::new(layout_root());
    let inside = layout_root().join(LAYOUT_ID).join(EVENTS_FILE);
    assert_eq!(adapter.native_id_of(&inside), Some(LAYOUT_ID.to_owned()));

    let other = Path::new("elsewhere").join(LAYOUT_ID).join(EVENTS_FILE);
    assert_eq!(adapter.native_id_of(&other), None);
    let nested = layout_root()
        .join("deeper")
        .join(LAYOUT_ID)
        .join(EVENTS_FILE);
    assert_eq!(adapter.native_id_of(&nested), None);
    assert_eq!(
        adapter.native_id_of(&layout_root().join(LAYOUT_ID).join("x.jsonl")),
        None
    );
}

#[test]
fn only_resume_is_supported() {
    let adapter = CopilotAdapter::new(layout_root());
    assert_eq!(
        adapter.capabilities(),
        Capabilities {
            resume: true,
            fork: false,
            delete: false,
            rename: false,
        }
    );
    assert_eq!(adapter.instance().kind, SourceKind::Copilot);
    assert!(adapter.rename(Path::new("x"), "t").is_err());
}

#[test]
fn from_env_uses_the_session_state_of_the_copilot_home() {
    let env = HistoryEnv::sandbox(Path::new("home"));
    let adapter = CopilotAdapter::from_env(&env).unwrap();
    assert_eq!(adapter.instance().root, layout_root());
    assert!(CopilotAdapter::from_env(&HistoryEnv::default()).is_none());
}
