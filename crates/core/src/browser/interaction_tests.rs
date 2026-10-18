//! Rename, delete, refresh and loader tests (upstream
//! `tui/app/interaction_tests.rs` without the terminal row math and viewer
//! tests), plus tests of the documented fixes. Every file lives in a
//! sandbox.

use super::test_helpers::*;
use super::*;
use crate::error::AppError;
use crate::history::{LoaderMessage, Source};
use crate::time_filter::TimeFilter;
use chrono::{Local, TimeZone};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn test_conversation(path: PathBuf, custom_title: Option<String>) -> Conversation {
    let mut full_text = "hello body".to_string();
    if let Some(title) = &custom_title {
        full_text = format!("{} {}", title, full_text);
    }
    Conversation {
        source: Source::Claude,
        session_id: path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        path,
        index: 0,
        timestamp: Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        preview: "hello body".to_string(),
        preview_first: "hello body".to_string(),
        preview_last: "hello body".to_string(),
        search_text_lower: search::normalize_for_search(&full_text),
        semantic_turns: vec!["hello body".to_string()],
        semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
        full_text,
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        project_name: Some("project".to_string()),
        project_path: None,
        cwd: None,
        message_count: 1,
        parse_errors: Vec::new(),
        summary: None,
        custom_title,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn app_with_conversation(
    sandbox: &Sandbox,
    path: PathBuf,
    custom_title: Option<String>,
) -> Browser {
    Browser::new(
        vec![test_conversation(path, custom_title)],
        sandbox.options(),
    )
}

fn write_conversation(path: &Path, title: Option<&str>) {
    let mut lines = vec![r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hello body"}}"#.to_string()];
    if let Some(title) = title {
        lines.push(format!(
            r#"{{"type":"custom-title","customTitle":"{}","sessionId":"abc123"}}"#,
            title
        ));
    }
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn write_named_conversation(path: &Path, text: &str) {
    let line = serde_json::json!({
        "type": "user",
        "timestamp": "2024-01-01T00:00:00Z",
        "message": {"role": "user", "content": text}
    })
    .to_string();
    std::fs::write(path, format!("{line}\n")).unwrap();
}

/// A Claude session with a cwd and three preview parts, so the first- and
/// last-message previews differ.
fn write_claude_session(path: &Path, cwd: &Path, first: &str, last: &str) {
    let cwd = cwd.to_string_lossy();
    let lines = [
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "timestamp": "2024-01-01T00:00:00Z",
            "message": {"role": "user", "content": first}
        }),
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2024-01-01T00:01:00Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "middle reply"}]}
        }),
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "timestamp": "2024-01-01T00:02:00Z",
            "message": {"role": "user", "content": last}
        }),
    ];
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, text + "\n").unwrap();
}

fn selected_path(app: &Browser) -> Option<PathBuf> {
    app.selected_path()
}

// --- list → viewer selection (upstream semantic_ranked_selection_...) ---

#[test]
fn semantic_ranked_selection_targets_the_ranked_row() {
    let sandbox = Sandbox::new();
    let first = sandbox.path().join("first.jsonl");
    let second = sandbox.path().join("second.jsonl");
    write_named_conversation(&first, "first body");
    write_named_conversation(&second, "second body");
    let mut app = Browser::new(
        vec![
            test_conversation(first.clone(), None),
            test_conversation(second.clone(), None),
        ],
        sandbox
            .options()
            .with_default_mode(ListSearchMode::Semantic),
    );
    let (_request_tx, _request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);

    send_semantic_complete_response(
        &response_tx,
        7,
        vec![1, 0],
        HashMap::from([(1, test_semantic_metadata(1, "second semantic preview"))]),
        SemanticProgress::Complete,
    );

    assert!(app.poll());
    assert_eq!(app.filtered(), &[1, 0]);
    assert_eq!(app.selected(), Some(0));
    assert_eq!(selected_path(&app), Some(second.clone()));
    assert_eq!(app.row(0).map(|conv| conv.path.clone()), Some(second));
    assert_eq!(app.row(1).map(|conv| conv.path.clone()), Some(first));
    assert!(app.row(2).is_none());
}

// --- rename (upstream dialog tests) ---

#[test]
fn cancel_rename_keeps_existing_title() {
    let sandbox = Sandbox::new();
    let path = sandbox.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let before = std::fs::read(&path).unwrap();
    let app = app_with_conversation(&sandbox, path.clone(), Some("old".to_string()));

    // Opening the dialog only reads the prefill; cancelling writes nothing.
    assert_eq!(app.rename_prefill().as_deref(), Some("old"));
    assert_eq!(app.conversations[0].custom_title, Some("old".to_string()));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn submit_rename_reparses_and_updates_search_index() {
    let sandbox = Sandbox::new();
    let path = sandbox.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(&sandbox, path.clone(), Some("old".to_string()));

    app.rename_selected("new").unwrap();

    assert_eq!(app.conversations[0].custom_title, Some("new".to_string()));
    assert!(search::search(&app.conversations, &app.searchable, "new", Local::now()).contains(&0));
    assert!(search::search(&app.conversations, &app.searchable, "old", Local::now()).is_empty());
    assert_eq!(RENAMED_STATUS, "Session renamed");
}

#[test]
fn submit_rename_preserves_selected_path() {
    let sandbox = Sandbox::new();
    let first = sandbox.path().join("first.jsonl");
    let second = sandbox.path().join("second.jsonl");
    write_conversation(&first, None);
    write_conversation(&second, None);
    let mut app = Browser::new(
        vec![
            test_conversation(first, None),
            test_conversation(second.clone(), None),
        ],
        sandbox.options(),
    );
    app.select(1);

    app.rename_selected("n").unwrap();

    assert_eq!(selected_path(&app).as_deref(), Some(second.as_path()));
}

#[test]
fn submit_empty_rename_clears_searchable_title() {
    let sandbox = Sandbox::new();
    let path = sandbox.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(&sandbox, path.clone(), Some("old".to_string()));

    app.rename_selected("   ").unwrap();

    assert_eq!(app.conversations[0].custom_title, None);
    assert!(search::search(&app.conversations, &app.searchable, "old", Local::now()).is_empty());
}

#[test]
fn rename_honors_show_last_for_the_reparsed_preview() {
    // Quirk 9: upstream re-parsed with the first-message preview regardless
    // of `--last`.
    let sandbox = Sandbox::new();
    let path = sandbox.path().join("session.jsonl");
    write_claude_session(
        &path,
        sandbox.path(),
        "opening question",
        "closing question",
    );
    for show_last in [true, false] {
        let mut app = Browser::new(
            vec![test_conversation(path.clone(), None)],
            sandbox.options().with_show_last(show_last),
        );
        app.rename_selected("titled").unwrap();
        let conv = &app.conversations()[0];
        assert_ne!(conv.preview_first, conv.preview_last);
        let expected = if show_last {
            &conv.preview_last
        } else {
            &conv.preview_first
        };
        assert_eq!(&conv.preview, expected);
        assert!(conv.preview_last.starts_with("closing question"));
    }
}

#[test]
fn rename_targets_the_captured_path_even_after_the_selection_moved() {
    // Addendum A1: the dialog's target is captured when it opens.
    let sandbox = Sandbox::new();
    let first = sandbox.path().join("first.jsonl");
    let second = sandbox.path().join("second.jsonl");
    write_conversation(&first, None);
    write_conversation(&second, None);
    let mut app = Browser::new(
        vec![
            test_conversation(first.clone(), None),
            test_conversation(second.clone(), None),
        ],
        sandbox.options(),
    );
    app.select(1);
    let captured = app.selected_path().unwrap();
    app.select(0);

    app.rename_conversation(&captured, "captured").unwrap();

    let (_, renamed) = app.conversation_by_path(&second).unwrap();
    assert_eq!(renamed.custom_title.as_deref(), Some("captured"));
    let (_, untouched) = app.conversation_by_path(&first).unwrap();
    assert_eq!(untouched.custom_title, None);
    // As upstream, the renamed session is selected afterwards.
    assert_eq!(app.selected_path(), Some(second));
}

#[test]
fn rename_errors_use_upstream_messages() {
    let sandbox = Sandbox::new();
    let missing = sandbox.path().join("missing.jsonl");
    let mut app = app_with_conversation(&sandbox, missing.clone(), None);

    let error = app.rename_selected("x").unwrap_err();
    assert!(matches!(error, RenameError::Failed(_)));
    assert!(
        error.to_string().starts_with("Failed to rename: "),
        "{error}"
    );

    assert!(matches!(
        app.rename_conversation(&sandbox.path().join("other.jsonl"), "x"),
        Err(RenameError::NoTarget)
    ));
    app.set_query("zzz-no-match");
    wait_for_search(&mut app);
    assert!(app.selected().is_none());
    assert!(matches!(
        app.rename_selected("x"),
        Err(RenameError::NoTarget)
    ));
    assert_eq!(app.rename_prefill(), None);
}

// --- refresh (upstream replace_conversations tests) ---

#[test]
fn replacing_conversations_preserves_selection_by_path() {
    let sandbox = Sandbox::new();
    let first = sandbox.path().join("first.jsonl");
    let second = sandbox.path().join("second.jsonl");
    let mut app = Browser::new(
        vec![
            test_conversation(first.clone(), None),
            test_conversation(second.clone(), None),
        ],
        sandbox.options(),
    );
    app.select(1);

    let mut refreshed_second = test_conversation(second.clone(), Some("renamed".to_string()));
    refreshed_second.timestamp = Local.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();
    app.replace_conversations(vec![test_conversation(first, None), refreshed_second]);

    assert_eq!(selected_path(&app).as_deref(), Some(second.as_path()));
    let selected = app.selected_conversation_index().unwrap();
    assert_eq!(
        app.conversations[selected].custom_title.as_deref(),
        Some("renamed")
    );
}

#[test]
fn replacing_conversations_removes_deleted_sessions_and_deduplicates_paths() {
    let sandbox = Sandbox::new();
    let old = sandbox.path().join("old.jsonl");
    let new = sandbox.path().join("new.jsonl");
    let mut app = app_with_conversation(&sandbox, old, None);

    app.replace_conversations(vec![
        test_conversation(new.clone(), None),
        test_conversation(new.clone(), None),
    ]);

    assert_eq!(app.conversations.len(), 1);
    assert_eq!(selected_path(&app).as_deref(), Some(new.as_path()));
}

#[test]
fn refresh_with_query_keeps_previous_rows_until_results_arrive() {
    // Quirk 16: upstream cleared the list until the worker answered.
    let sandbox = Sandbox::new();
    let a = sandbox.path().join("a.jsonl");
    let b = sandbox.path().join("b.jsonl");
    let c = sandbox.path().join("c.jsonl");
    let mut app = Browser::new(
        vec![
            test_conversation(a.clone(), None),
            test_conversation(b.clone(), None),
            test_conversation(c.clone(), None),
        ],
        sandbox.options(),
    );
    app.set_query("hello");
    wait_for_search(&mut app);
    assert_eq!(app.filtered().len(), 3);
    let b_row = app
        .filtered()
        .iter()
        .position(|&index| app.conversations()[index].path == b)
        .unwrap();
    app.select(b_row);

    // `a` disappeared, `d` is new.
    let d = sandbox.path().join("d.jsonl");
    app.replace_conversations(vec![
        test_conversation(b.clone(), None),
        test_conversation(c.clone(), None),
        test_conversation(d.clone(), None),
    ]);

    let shown = app
        .filtered()
        .iter()
        .map(|&index| app.conversations()[index].path.clone())
        .collect::<Vec<_>>();
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(shown.contains(&b) && shown.contains(&c));
    assert_eq!(app.selected_path(), Some(b.clone()));
    assert!(app.has_search_work_in_flight());

    wait_for_search(&mut app);
    assert_eq!(app.filtered().len(), 3);
    assert_eq!(app.selected_path(), Some(b));
}

// --- delete ---

const UUID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const UUID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

#[test]
fn delete_removes_every_copy_of_a_claude_session_and_its_row() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let project_a = root.join("-work-a");
    let project_b = root.join("-work-b");
    let copy_a = project_a.join(format!("{UUID_A}.jsonl"));
    let copy_b = project_b.join(format!("{UUID_A}.jsonl"));
    let other = project_a.join(format!("{UUID_B}.jsonl"));
    write_claude_session(&copy_a, sandbox.path(), "one", "two");
    write_claude_session(&copy_b, sandbox.path(), "one", "two");
    write_claude_session(&other, sandbox.path(), "three", "four");
    let session_dir = project_a.join(UUID_A).join("subagents");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("agent.jsonl"), "{}\n").unwrap();

    let mut app = Browser::new(
        vec![
            test_conversation(copy_a.clone(), None),
            test_conversation(other.clone(), None),
        ],
        sandbox.options(),
    );

    let deleted = app.delete_selected().unwrap();

    assert_eq!(deleted.path, copy_a);
    assert_eq!(deleted.source, Source::Claude);
    assert_eq!(deleted.files_removed, 2);
    assert!(!copy_a.exists());
    assert!(!copy_b.exists());
    assert!(!project_a.join(UUID_A).exists());
    assert!(other.exists());
    assert_eq!(app.conversations().len(), 1);
    assert_eq!(app.selected_path(), Some(other));
}

#[test]
fn delete_uses_the_source_of_the_captured_conversation() {
    // Addendum A2: a Pi session goes through the Pi delete, even when the
    // selection points at a Claude row with the same file stem.
    let sandbox = Sandbox::new();
    let pi_dir = sandbox.path().join("pi-sessions");
    std::fs::create_dir_all(&pi_dir).unwrap();
    let pi_path = pi_dir.join(format!("{UUID_A}.jsonl"));
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v1.jsonl"),
        &pi_path,
    )
    .unwrap();
    let claude_path = sandbox
        .projects_root()
        .join("-work")
        .join(format!("{UUID_A}.jsonl"));
    write_claude_session(&claude_path, sandbox.path(), "one", "two");

    let mut pi = test_conversation(pi_path.clone(), None);
    pi.source = Source::Pi;
    let mut app = Browser::new(
        vec![test_conversation(claude_path.clone(), None), pi],
        sandbox.options(),
    );
    app.select(0);

    let deleted = app.delete_conversation(&pi_path).unwrap();

    assert_eq!(deleted.source, Source::Pi);
    assert_eq!(deleted.files_removed, 1);
    assert!(!pi_path.exists());
    assert!(claude_path.exists(), "the Claude copy must survive");
    assert_eq!(app.conversations().len(), 1);
    assert_eq!(app.selected_path(), Some(claude_path));
}

#[test]
fn deleting_another_row_keeps_the_selected_conversation() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let first = root.join("-p").join(format!("{UUID_A}.jsonl"));
    let second = root.join("-p").join(format!("{UUID_B}.jsonl"));
    write_claude_session(&first, sandbox.path(), "one", "two");
    write_claude_session(&second, sandbox.path(), "three", "four");
    let mut app = Browser::new(
        vec![
            test_conversation(first.clone(), None),
            test_conversation(second.clone(), None),
        ],
        sandbox.options(),
    );
    app.select(1);

    app.delete_conversation(&first).unwrap();

    assert_eq!(app.selected(), Some(0));
    assert_eq!(app.selected_path(), Some(second));
}

#[test]
fn failed_delete_is_reported_logged_and_keeps_the_row() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.projects_root()).unwrap();
    let path = sandbox
        .path()
        .join("elsewhere")
        .join(format!("{UUID_A}.jsonl"));
    let mut app = app_with_conversation(&sandbox, path.clone(), None);

    let error = app.delete_selected().unwrap_err();

    assert!(matches!(error, DeleteError::Failed { .. }));
    let message = error.to_string();
    assert!(
        message.starts_with(&format!("Failed to delete session {}", path.display())),
        "{message}"
    );
    assert_eq!(app.conversations().len(), 1);
    assert_eq!(app.selected_path(), Some(path));
    let log = std::fs::read_to_string(sandbox.env.debug_log_file().unwrap()).unwrap();
    assert!(log.contains(&message), "{log}");
}

#[test]
fn delete_is_unavailable_in_single_file_mode_and_without_a_target() {
    let sandbox = Sandbox::new();
    let path = sandbox.path().join(format!("{UUID_A}.jsonl"));
    write_conversation(&path, None);
    let mut single = Browser::new_single_file(path.clone(), sandbox.options());
    assert_eq!(single.conversations().len(), 1);
    assert!(single.single_file_error().is_none());
    assert!(matches!(
        single.delete_selected(),
        Err(DeleteError::SingleFileMode)
    ));
    assert!(matches!(
        single.delete_conversation(&path),
        Err(DeleteError::SingleFileMode)
    ));
    assert!(path.exists());

    let mut app = app_with_conversation(&sandbox, path.clone(), None);
    assert!(matches!(
        app.delete_conversation(&sandbox.path().join("other.jsonl")),
        Err(DeleteError::NoTarget)
    ));
    assert!(path.exists());
}

// --- single-file mode ---

fn write_jsonl(path: &Path, lines: &[serde_json::Value]) {
    let text = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    std::fs::write(path, text).unwrap();
}

#[test]
fn single_file_that_is_only_left_out_of_the_list_is_not_an_error() {
    // Addendum D1: a Warmup prompt and its reply have no listable preview,
    // but the viewer renders both messages, as upstream did.
    let sandbox = Sandbox::new();
    let warmup = sandbox.path().join("warmup.jsonl");
    write_jsonl(
        &warmup,
        &[
            serde_json::json!({
                "type": "user",
                "timestamp": "2024-01-01T00:00:00Z",
                "message": {"role": "user", "content": "Warmup"}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:01:00Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "ready"}]}
            }),
        ],
    );
    let assistant_only = sandbox.path().join("assistant-only.jsonl");
    write_jsonl(
        &assistant_only,
        &[serde_json::json!({
            "type": "assistant",
            "timestamp": "2024-01-01T00:01:00Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
        })],
    );

    for (path, messages) in [(&warmup, 2), (&assistant_only, 1)] {
        let app = Browser::new_single_file(path.clone(), sandbox.options());
        assert!(app.conversations().is_empty(), "{}", path.display());
        assert_eq!(app.single_file_error(), None, "{}", path.display());
        let viewer = crate::view::ViewerModel::open(path, Default::default()).unwrap();
        assert_eq!(viewer.document().messages.len(), messages);
    }

    // A file the viewer would render empty is still reported.
    let empty = sandbox.path().join("empty.jsonl");
    std::fs::write(&empty, "").unwrap();
    let app = Browser::new_single_file(empty.clone(), sandbox.options());
    assert!(app.conversations().is_empty());
    assert_eq!(
        app.single_file_error(),
        Some(format!("{} contains no conversation messages", empty.display()).as_str())
    );
}

// --- UUID jump ---

/// What a GUI does for a [`UuidLookup::Pending`] jump: the blocking disk
/// lookup (normally on a background thread), then the resolve.
fn resolve_pending_uuid(app: &mut Browser) {
    let Some(UuidLookup::Pending { uuid }) = app.uuid_lookup().cloned() else {
        panic!("no pending UUID lookup: {:?}", app.uuid_lookup());
    };
    let loaded = crate::history::load_session_by_uuid(app.env(), &uuid, app.show_last()).unwrap();
    app.resolve_uuid_lookup(&uuid, loaded);
}

#[test]
fn unknown_uuid_reports_not_found_and_keeps_rows() {
    // Quirk 6.
    let sandbox = Sandbox::new();
    let path = sandbox.path().join("first.jsonl");
    let mut app = app_with_conversation(&sandbox, path, None);
    let uuid = "99999999-9999-4999-8999-999999999999";

    app.set_query(uuid);
    // K2: set_query never reads the disk; the rows stay while pending.
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Pending {
            uuid: uuid.to_string()
        })
    );
    assert_eq!(app.uuid_lookup().unwrap().status_message(), None);
    assert_eq!(app.filtered(), &[0]);
    resolve_pending_uuid(&mut app);

    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::NotFound {
            uuid: uuid.to_string()
        })
    );
    assert_eq!(
        app.uuid_lookup().unwrap().status_message().as_deref(),
        Some(format!("Session not found: {uuid}").as_str())
    );
    assert_eq!(app.filtered(), &[0]);

    app.set_query("hello");
    assert_eq!(app.uuid_lookup(), None);
}

#[test]
fn uuid_jump_loads_an_unlisted_session_from_the_sandbox() {
    let sandbox = Sandbox::new();
    let cwd = sandbox.path().join("code").join("demo");
    let path = sandbox
        .projects_root()
        .join("-code-demo")
        .join(format!("{UUID_A}.jsonl"));
    write_claude_session(&path, &cwd, "opening question", "closing question");
    let listed = sandbox.path().join("listed.jsonl");
    let mut app = Browser::new(
        vec![test_conversation(listed, None)],
        sandbox.options().with_show_last(true),
    );

    app.set_query(UUID_A);
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Pending {
            uuid: UUID_A.to_string()
        })
    );
    assert_eq!(app.conversations().len(), 1);
    assert_eq!(app.filtered(), &[0]);
    resolve_pending_uuid(&mut app);

    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 1,
            loaded_from_disk: true
        })
    );
    assert_eq!(app.filtered(), &[1]);
    let loaded = &app.conversations()[1];
    assert_eq!(loaded.path, path);
    assert_eq!(loaded.project_name.as_deref(), Some("demo"));
    assert_eq!(loaded.project_path.as_deref(), Some(cwd.as_path()));
    // Addendum C1: the preview honors show_last.
    assert_eq!(loaded.preview, loaded.preview_last);
    assert!(loaded.preview.starts_with("closing question"));

    app.set_query("");
    assert_eq!(app.conversations().len(), 2);
}

#[test]
fn deleting_sessions_keeps_the_uuid_lookup_index_valid() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let jumped = root.join("-code-demo").join(format!("{UUID_A}.jsonl"));
    write_claude_session(
        &jumped,
        sandbox.path(),
        "opening question",
        "closing question",
    );
    let listed = root.join("-code-other").join(format!("{UUID_B}.jsonl"));
    write_claude_session(&listed, sandbox.path(), "listed", "listed");
    let mut app = Browser::new(
        vec![test_conversation(listed.clone(), None)],
        sandbox.options(),
    );

    // The jump appends the session from disk at index 1.
    app.set_query(UUID_A);
    resolve_pending_uuid(&mut app);
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 1,
            loaded_from_disk: true
        })
    );

    // Deleting a lower-index session (a captured path, addendum A1) shifts
    // the found session down; the lookup must follow it.
    app.delete_conversation(&listed).unwrap();
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 0,
            loaded_from_disk: true
        })
    );
    assert_eq!(app.conversations()[0].path, jumped);
    assert_eq!(app.filtered(), &[0]);

    // Deleting the found session itself clears the lookup instead of
    // leaving an out-of-bounds index.
    app.delete_selected().unwrap();
    assert!(app.conversations().is_empty());
    assert_eq!(app.uuid_lookup(), None);
    assert!(!jumped.exists());
}

// --- loading-time fixes ---

#[test]
fn scope_toggle_while_loading_keeps_scope_filtered_rows() {
    // Quirk 14: upstream filtered against the not-yet-built index.
    let sandbox = Sandbox::new();
    let workspace = WorkspaceScope::from_parts("/tmp/visible", "-tmp-visible");
    let mut app = Browser::new_loading(sandbox.options().with_workspace(Some(workspace)));
    let mut inside = test_conversation(PathBuf::from("/x/-tmp-visible/a.jsonl"), None);
    inside.project_name = Some("visible".to_string());
    let mut outside = test_conversation(PathBuf::from("/x/-tmp-other/b.jsonl"), None);
    outside.project_name = Some("other".to_string());
    app.append_conversations(vec![inside, outside]);
    app.set_query("nomatch");
    assert_eq!(app.filtered().len(), 2);

    app.toggle_workspace_filter();
    assert!(app.workspace_filter());
    assert_eq!(app.filtered(), &[0]);

    app.toggle_list_search_mode();
    assert_eq!(app.list_search_mode(), ListSearchMode::Semantic);
    assert_eq!(app.filtered(), &[0]);
    assert!(app.semantic_search.worker_tx.is_none());

    app.toggle_list_search_mode();
    app.toggle_workspace_filter();
    assert_eq!(app.filtered().len(), 2);

    // The query is applied when loading finishes.
    app.finish_loading();
    wait_for_search(&mut app);
    assert!(app.filtered().is_empty());
}

#[test]
fn escape_clears_the_query_or_quits() {
    let sandbox = Sandbox::new();
    let mut app = app_with_conversation(&sandbox, sandbox.path().join("a.jsonl"), None);
    app.set_query("zzz");
    wait_for_search(&mut app);
    assert!(app.filtered().is_empty());

    assert_eq!(app.escape(), EscapeOutcome::Cleared);
    assert_eq!(app.query(), "");
    assert_eq!(app.filtered(), &[0]);
    assert_eq!(app.escape(), EscapeOutcome::Quit);
}

#[test]
fn workspace_and_exclusions_can_change_at_runtime() {
    let sandbox = Sandbox::new();
    let mut inside = test_conversation(PathBuf::from("/x/-tmp-visible/a.jsonl"), None);
    inside.project_name = Some("visible".to_string());
    let mut outside = test_conversation(PathBuf::from("/x/-tmp-other/b.jsonl"), None);
    outside.project_name = Some("other".to_string());
    let mut app = Browser::new(
        vec![inside, outside],
        sandbox.options().with_workspace(None),
    );
    assert!(!app.has_project_context());
    app.toggle_workspace_filter();
    assert!(!app.workspace_filter());

    app.set_workspace(Some(WorkspaceScope::from_parts(
        "/tmp/visible",
        "-tmp-visible",
    )));
    assert_eq!(app.filtered().len(), 2);
    app.set_workspace_filter(true);
    assert_eq!(app.filtered(), &[0]);

    app.set_workspace(None);
    assert!(!app.workspace_filter());
    assert_eq!(app.filtered().len(), 2);

    app.set_excluded_projects(vec!["other".to_string()]);
    assert_eq!(app.filtered(), &[0]);
    assert!(app.excluded_projects().contains("other"));
}

#[test]
fn selection_navigation_is_clamped() {
    let sandbox = Sandbox::new();
    let conversations = (0..25)
        .map(|index| test_conversation(sandbox.path().join(format!("{index}.jsonl")), None))
        .collect::<Vec<_>>();
    let mut app = Browser::new(conversations, sandbox.options());
    assert_eq!(app.selected(), Some(0));
    app.select_prev();
    assert_eq!(app.selected(), Some(0));
    app.select_page_down();
    assert_eq!(app.selected(), Some(10));
    app.select_half_page_down(9);
    assert_eq!(app.selected(), Some(14));
    app.select_last();
    assert_eq!(app.selected(), Some(24));
    app.select_next();
    assert_eq!(app.selected(), Some(24));
    app.scroll_list(5);
    assert_eq!(app.selected(), Some(24));
    app.scroll_list(-1);
    assert_eq!(app.selected(), Some(23));
    app.select_page_up();
    assert_eq!(app.selected(), Some(13));
    app.select_first();
    assert_eq!(app.selected(), Some(0));
    assert!(!app.select(25));
    assert!(app.select(3));
    assert_eq!(app.count_text(), "4/25");
    assert_eq!(app.selected_source(), Some(Source::Claude));
}

#[cfg(windows)]
#[test]
fn pi_workspace_match_ignores_verbatim_prefix_and_drive_case() {
    // W1: a recorded `c:\...` cwd that no longer exists still matches a
    // canonical `\\?\C:\...` workspace.
    let sandbox = Sandbox::new();
    let mut pi = test_conversation(sandbox.path().join("pi.jsonl"), None);
    pi.source = Source::Pi;
    pi.project_path = Some(PathBuf::from(r"c:\no-such-dir\proj"));
    let mut app = Browser::new(
        vec![pi],
        sandbox
            .options()
            .with_workspace(Some(
                WorkspaceScope::new(r"\\?\C:\no-such-dir\proj").unwrap(),
            ))
            .with_workspace_filter(true),
    );
    assert_eq!(app.filtered(), &[0]);
    app.set_workspace(Some(WorkspaceScope::new(r"C:\no-such-dir\other").unwrap()));
    assert!(app.filtered().is_empty());
}

// --- loader driver ---

fn run_until_event(loader: &mut HistoryLoader, browser: &mut Browser) -> (LoadEvent, usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut batches = 0;
    loop {
        let poll = loader.poll(browser);
        batches += poll.batches;
        if let Some(event) = poll.event {
            assert!(!poll.preparing);
            assert!(!loader.is_running());
            assert!(!loader.is_preparing());
            return (event, batches);
        }
        assert_eq!(poll.preparing, loader.is_preparing());
        assert!(Instant::now() < deadline, "loader did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn initial_load_streams_sessions_from_the_sandbox() {
    let sandbox = Sandbox::new();
    let cwd = sandbox.path().join("code").join("demo");
    write_claude_session(
        &sandbox
            .projects_root()
            .join("-code-demo")
            .join(format!("{UUID_A}.jsonl")),
        &cwd,
        "first question",
        "last question",
    );
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let waker: Waker = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let mut browser = Browser::new_loading(sandbox.options());
    let mut loader = HistoryLoader::new(
        sandbox.env.clone(),
        LoaderOptions::default().with_waker(waker),
    );
    assert!(loader.start_initial_load());
    assert!(loader.is_running());
    assert!(!loader.is_refreshing());
    assert!(!loader.start_initial_load());
    assert!(!loader.start_refresh(), "refresh is ignored while loading");

    let (event, batches) = run_until_event(&mut loader, &mut browser);

    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: 1,
                project_errors: 0
            }
        ),
        "{event:?}"
    );
    assert_eq!(event.status_message(), None);
    assert_eq!(batches, 1);
    assert!(wakes.load(Ordering::SeqCst) >= 1);
    assert!(!browser.is_loading());
    let conv = &browser.conversations()[0];
    assert_eq!(conv.project_name.as_deref(), Some("demo"));
    assert!(conv.preview.starts_with("last question"));
    // Caches are written inside the sandbox.
    assert!(sandbox.env.history_cache_dir().unwrap().exists());
}

#[test]
fn initial_load_reports_missing_and_empty_history() {
    let sandbox = Sandbox::new();
    let mut browser = Browser::new_loading(sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    assert!(loader.start_initial_load());
    let (event, _) = run_until_event(&mut loader, &mut browser);
    match &event {
        LoadEvent::LoadFailed(AppError::ProjectsDirNotFound(path)) => {
            assert!(path.contains("projects"), "{path}");
        }
        other => panic!("unexpected {other:?}"),
    }
    // The failed load is over: the browser is ready and empty.
    assert!(!browser.is_loading());
    assert_eq!(browser.count_text(), "0/0");

    std::fs::create_dir_all(sandbox.projects_root()).unwrap();
    for (time_filter, expected) in [
        (
            TimeFilter::default(),
            "No conversation history found in selected scope",
        ),
        (
            TimeFilter {
                after: Some(Local::now()),
                before: None,
            },
            "No conversation history found in selected scope within the requested time range",
        ),
    ] {
        let mut browser = Browser::new_loading(sandbox.options());
        let mut loader = HistoryLoader::new(
            sandbox.env.clone(),
            LoaderOptions::default().with_time_filter(time_filter),
        );
        loader.start_initial_load();
        let (event, _) = run_until_event(&mut loader, &mut browser);
        match event {
            LoadEvent::NoHistory(error) => assert_eq!(error.to_string(), expected),
            other => panic!("unexpected {other:?}"),
        }
        assert!(!browser.is_loading());
    }
}

#[test]
fn refresh_replaces_the_corpus_and_reports_counts() {
    let sandbox = Sandbox::new();
    let cwd = sandbox.path().join("code").join("demo");
    let project = sandbox.projects_root().join("-code-demo");
    write_claude_session(
        &project.join(format!("{UUID_A}.jsonl")),
        &cwd,
        "first",
        "second",
    );
    let mut browser = Browser::new_loading(sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    loader.start_initial_load();
    let _ = run_until_event(&mut loader, &mut browser);
    assert_eq!(browser.conversations().len(), 1);

    write_claude_session(
        &project.join(format!("{UUID_B}.jsonl")),
        &cwd,
        "third",
        "fourth",
    );
    assert!(loader.start_refresh());
    assert!(loader.is_refreshing());
    assert_eq!(REFRESHING_STATUS, "Refreshing sessions...");
    let (event, batches) = run_until_event(&mut loader, &mut browser);

    assert_eq!(batches, 0, "refresh batches are buffered");
    assert!(event.is_refresh());
    assert_eq!(
        event.status_message().as_deref(),
        Some("Refreshed sessions: 1 -> 2")
    );
    assert_eq!(browser.conversations().len(), 2);
    assert!(!browser.is_loading());
}

#[test]
fn refresh_after_a_failed_initial_load_finishes_loading() {
    // First run: `~/.claude/projects` does not exist yet.
    let sandbox = Sandbox::new();
    let mut browser = Browser::new_loading(sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    assert!(loader.start_initial_load());
    let (event, _) = run_until_event(&mut loader, &mut browser);
    assert!(matches!(event, LoadEvent::LoadFailed(_)), "{event:?}");

    // The user starts Claude Code, then retries with a refresh.
    let cwd = sandbox.path().join("code").join("demo");
    let project = sandbox.projects_root().join("-code-demo");
    write_claude_session(
        &project.join(format!("{UUID_A}.jsonl")),
        &cwd,
        "older question",
        "older needle",
    );
    write_claude_session(
        &project.join(format!("{UUID_B}.jsonl")),
        &cwd,
        "newer question",
        "newer answer",
    );
    assert!(loader.start_refresh());
    let (event, _) = run_until_event(&mut loader, &mut browser);
    assert_eq!(
        event.status_message().as_deref(),
        Some("Refreshed sessions: 0 -> 2")
    );
    assert!(!browser.is_loading());
    assert_eq!(browser.count_text(), "1/2");

    // Searching works (the search index was built).
    browser.set_query("needle");
    wait_for_search(&mut browser);
    assert_eq!(browser.filtered().len(), 1);
    assert_eq!(browser.row(0).unwrap().session_id, UUID_A);
}

#[test]
fn replacing_the_corpus_of_a_loading_browser_finishes_loading() {
    let sandbox = Sandbox::new();
    let mut browser = Browser::new_loading(sandbox.options());
    browser.set_query("body");
    let mut older = test_conversation(sandbox.path().join("older.jsonl"), None);
    older.timestamp = Local.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
    let newer = test_conversation(sandbox.path().join("newer.jsonl"), None);
    browser.replace_conversations(vec![older, newer]);

    assert!(!browser.is_loading());
    // Sorted newest first, reindexed, and the typed query was applied.
    assert_eq!(browser.conversations()[0].session_id, "newer");
    assert_eq!(browser.conversations()[1].index, 1);
    wait_for_search(&mut browser);
    let mut rows = browser.filtered().to_vec();
    rows.sort_unstable();
    assert_eq!(rows, [0, 1]);
    assert_eq!(browser.count_text(), "1/2");
}

fn injected(loader: &mut HistoryLoader, messages: Vec<LoaderMessage>, refresh: bool, close: bool) {
    let (tx, rx) = std::sync::mpsc::channel();
    for message in messages {
        tx.send(message).unwrap();
    }
    if !close {
        // Keep the sender alive: the stream is still "running".
        std::mem::forget(tx);
    }
    loader.start_with_receiver_for_test(rx, refresh);
}

#[test]
fn refresh_failures_keep_the_existing_sessions() {
    let sandbox = Sandbox::new();
    let existing = sandbox.path().join("existing.jsonl");
    let mut browser = app_with_conversation(&sandbox, existing.clone(), None);
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let fresh = || test_conversation(sandbox.path().join("fresh.jsonl"), None);

    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![fresh()]),
            LoaderMessage::ProjectError,
            LoaderMessage::Done,
        ],
        true,
        true,
    );
    let poll = loader.poll(&mut browser);
    assert!(poll.changed());
    let event = poll.event.unwrap();
    assert!(matches!(event, LoadEvent::RefreshIncomplete));
    assert_eq!(
        event.status_message().as_deref(),
        Some("Refresh incomplete; keeping existing sessions")
    );
    assert_eq!(browser.selected_path(), Some(existing.clone()));

    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![fresh()]),
            LoaderMessage::Fatal(AppError::ProjectsDirNotFound("gone".to_string())),
        ],
        true,
        true,
    );
    let event = loader.poll(&mut browser).event.unwrap();
    assert_eq!(
        event.status_message().as_deref(),
        Some("Refresh failed: Claude projects directory not found at gone")
    );
    assert_eq!(browser.selected_path(), Some(existing.clone()));

    injected(
        &mut loader,
        vec![LoaderMessage::Batch(vec![fresh()])],
        true,
        true,
    );
    let event = loader.poll(&mut browser).event.unwrap();
    assert!(matches!(event, LoadEvent::RefreshInterrupted));
    assert_eq!(
        event.status_message().as_deref(),
        Some("Refresh failed; keeping existing sessions")
    );
    assert_eq!(browser.conversations().len(), 1);
    assert_eq!(browser.selected_path(), Some(existing));

    // A pending stream yields nothing yet and stays running.
    injected(&mut loader, vec![], true, false);
    let poll = loader.poll(&mut browser);
    assert!(!poll.changed());
    assert!(loader.is_refreshing());
}

#[test]
fn initial_stream_disconnect_finishes_loading() {
    let sandbox = Sandbox::new();
    let mut browser = Browser::new_loading(sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![test_conversation(
                sandbox.path().join("a.jsonl"),
                None,
            )]),
            LoaderMessage::ProjectError,
        ],
        false,
        true,
    );
    let (event, batches) = run_until_event(&mut loader, &mut browser);
    assert_eq!(batches, 1);
    assert!(matches!(
        event,
        LoadEvent::Loaded {
            conversations: 1,
            project_errors: 1
        }
    ));
    assert!(!browser.is_loading());

    let mut browser = Browser::new_loading(sandbox.options());
    injected(
        &mut loader,
        vec![LoaderMessage::Fatal(AppError::ProjectsDirNotFound(
            "x".to_string(),
        ))],
        false,
        true,
    );
    assert!(matches!(
        run_until_event(&mut loader, &mut browser).0,
        LoadEvent::LoadFailed(_)
    ));
    // A failed initial load still ends loading (a refresh can retry).
    assert!(!browser.is_loading());
}

#[test]
fn loader_time_filter_applies_to_later_loads() {
    let sandbox = Sandbox::new();
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::new(false));
    assert!(!loader.options().show_last);
    assert!(!loader.time_filter().is_active());
    let filter = TimeFilter {
        after: Some(Local::now()),
        before: None,
    };
    loader.set_time_filter(filter);
    assert_eq!(loader.time_filter(), &filter);
    assert!(LoaderOptions::default().show_last);
}

// --- display wiring ---

#[test]
fn row_inputs_wire_mode_metadata_and_sources() {
    use crate::display_format::{
        HighlightQuery, PreviewSource, build_row_header, build_row_preview,
    };
    let sandbox = Sandbox::new();
    let mut pi = test_conversation(sandbox.path().join("pi.jsonl"), None);
    pi.source = Source::Pi;
    let mut app = Browser::new(
        vec![test_conversation(sandbox.path().join("a.jsonl"), None), pi],
        sandbox
            .options()
            .with_default_mode(ListSearchMode::Semantic),
    );
    let (_request_tx, _request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    set_query_for_test(&mut app, "hello");
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);
    send_semantic_complete_response(
        &response_tx,
        7,
        vec![1, 0],
        HashMap::from([(1, test_semantic_metadata(1, "semantic hello evidence"))]),
        SemanticProgress::Complete,
    );
    assert!(app.poll());

    let query = HighlightQuery::parse(app.query());
    let input = app.row_preview_input(0, &query, 76).unwrap();
    assert!(input.semantic_mode);
    let preview = build_row_preview(&input);
    assert_eq!(preview.source, PreviewSource::SemanticEvidence);
    assert_eq!(preview.preview.text, "semantic hello evidence");

    let header = build_row_header(&app.row_header_input(0, &query, 120, Local::now()).unwrap());
    assert_eq!(header.project.text, "Pi  · project");
    assert_eq!(header.semantic_score.as_deref(), Some("1.00"));

    let second = build_row_preview(&app.row_preview_input(1, &query, 76).unwrap());
    assert_eq!(second.source, PreviewSource::Preview);
    assert!(app.row_preview_input(2, &query, 76).is_none());
    assert!(app.row_header_input(2, &query, 120, Local::now()).is_none());
}

#[test]
fn controller_types_can_move_to_other_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<Browser>();
    assert_send::<HistoryLoader>();
    assert_send::<BrowserOptions>();
    assert_send::<LoadEvent>();
}
