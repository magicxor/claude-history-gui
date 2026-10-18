//! Tests of the core gaps owned by the browser (DESIGN §4.3): K1 shared
//! corpus and off-thread preparation, K2 UUID jump without disk access, K3
//! rename in two halves, K4 `remove_paths`, K6 semantic suspend/resume and
//! K18 refresh policies. Every file lives in a sandbox.

use super::test_helpers::*;
use super::*;
use crate::history::{LoaderMessage, Source};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic_worker::{SemanticSearchMessage, SemanticWorkerCommand};
use chrono::{Duration as ChronoDuration, Local, TimeZone};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const UUID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const UUID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

fn conversation(path: PathBuf, text: &str) -> Conversation {
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
        preview: text.to_string(),
        preview_first: text.to_string(),
        preview_last: text.to_string(),
        search_text_lower: search::normalize_for_search(text),
        semantic_turns: vec![text.to_string()],
        semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
        full_text: text.to_string(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        project_name: Some("project".to_string()),
        project_path: None,
        cwd: None,
        message_count: 1,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn dated(path: PathBuf, text: &str, day: u32) -> Conversation {
    let mut conversation = conversation(path, text);
    conversation.timestamp = Local.with_ymd_and_hms(2024, 1, day, 0, 0, 0).unwrap();
    conversation
}

/// A Claude transcript with a cwd and distinct first/last previews.
fn write_claude_session(path: &Path, cwd: &Path, first: &str, last: &str) {
    let cwd = cwd.to_string_lossy();
    let lines = [
        serde_json::json!({
            "type": "user", "cwd": cwd, "timestamp": "2024-01-01T00:00:00Z",
            "message": {"role": "user", "content": first}
        }),
        serde_json::json!({
            "type": "assistant", "timestamp": "2024-01-01T00:01:00Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "middle reply"}]}
        }),
        serde_json::json!({
            "type": "user", "cwd": cwd, "timestamp": "2024-01-01T00:02:00Z",
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

/// Capture what the browser sends to its lexical worker.
fn capture_search_commands(app: &mut Browser) -> mpsc::Receiver<SearchCommand> {
    let (tx, rx) = mpsc::channel();
    app.search_tx = tx;
    rx
}

/// The snapshots of the last query sent to the lexical worker.
fn last_update(rx: &mpsc::Receiver<SearchCommand>) -> Option<(CorpusSnapshot, SearchSnapshot)> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .last()
        .map(|command| (command.conversations, command.searchable))
}

fn injected(loader: &mut HistoryLoader, messages: Vec<LoaderMessage>, refresh: bool) {
    let (tx, rx) = mpsc::channel();
    for message in messages {
        tx.send(message).unwrap();
    }
    loader.start_with_receiver_for_test(rx, refresh);
}

fn poll_until_event(loader: &mut HistoryLoader, browser: &mut Browser) -> LoadEvent {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(event) = loader.poll(browser).event {
            return event;
        }
        assert!(Instant::now() < deadline, "loader did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

// --- K1: one shared corpus ---

#[test]
fn corpus_and_search_data_are_shared_with_both_workers() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(
        vec![
            conversation(sandbox.path().join("a.jsonl"), "alpha"),
            conversation(sandbox.path().join("b.jsonl"), "beta"),
            conversation(sandbox.path().join("c.jsonl"), "gamma"),
        ],
        sandbox
            .options()
            .with_default_mode(ListSearchMode::Semantic),
    );
    let lexical = capture_search_commands(&mut app);
    let (_request_tx, semantic, _response_tx) = connect_semantic_search_channels(&mut app);
    let before = app.corpus_snapshot();

    // A removal replaces the pointer vector only: every remaining
    // conversation is the same allocation as before.
    assert_eq!(app.remove_paths(&[sandbox.path().join("b.jsonl")]), 1);
    let after = app.corpus_snapshot();
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(before.len(), 3, "an earlier snapshot never changes");
    assert!(Arc::ptr_eq(&before[0], &after[0]));
    assert!(Arc::ptr_eq(&before[2], &after[1]));

    // The lexical worker gets exactly the browser's corpus and search data
    // with the query (here the lexical fallback of a semantic query).
    assert!(last_update(&lexical).is_none(), "nothing until a query");
    app.set_query("alpha");
    let (conversations, searchable) = last_update(&lexical).expect("lexical query");
    assert!(Arc::ptr_eq(&conversations, &after));
    assert!(Arc::ptr_eq(&searchable, &app.searchable));
    assert_eq!(searchable.len(), 2);

    // So does the semantic worker.
    let corpus = std::iter::from_fn(|| semantic.try_recv().ok())
        .find_map(|command| match command {
            SemanticWorkerCommand::UpdateCorpus { conversations, .. } => Some(conversations),
            _ => None,
        })
        .expect("semantic corpus");
    assert!(Arc::ptr_eq(&corpus, &app.corpus_snapshot()));
    assert!(Arc::ptr_eq(&corpus, &conversations));
}

#[test]
fn search_after_a_removal_maps_to_the_shifted_indices() {
    // The search data is positional: a removal shifts the shared entries
    // without rebuilding them.
    let sandbox = Sandbox::new();
    let mut app = Browser::new(
        vec![
            conversation(sandbox.path().join("a.jsonl"), "first needle"),
            conversation(sandbox.path().join("b.jsonl"), "other"),
            conversation(sandbox.path().join("c.jsonl"), "third needle"),
        ],
        sandbox.options(),
    );
    app.remove_paths(&[sandbox.path().join("a.jsonl")]);
    app.set_query("needle");
    wait_for_search(&mut app);
    assert_eq!(app.filtered(), &[1]);
    assert_eq!(app.row(0).unwrap().path, sandbox.path().join("c.jsonl"));
}

#[test]
fn prepare_corpus_sorts_reindexes_and_dedupes_on_request() {
    let sandbox = Sandbox::new();
    let older = dated(sandbox.path().join("older.jsonl"), "older", 1);
    let newer = dated(sandbox.path().join("newer.jsonl"), "newer", 2);
    let prepared =
        Browser::prepare_corpus(vec![older.clone(), newer.clone(), older.clone()], false);
    assert_eq!(prepared.len(), 3, "the initial load does not dedupe (C2)");
    let prepared = Browser::prepare_corpus(vec![older.clone(), newer.clone(), older], true);
    assert_eq!(prepared.len(), 2, "a refresh dedupes by path");

    let mut app = Browser::new_loading(sandbox.options());
    app.install_prepared(prepared);
    assert!(!app.is_loading());
    let sessions = app
        .conversations()
        .iter()
        .map(|conversation| (conversation.session_id.as_str(), conversation.index))
        .collect::<Vec<_>>();
    assert_eq!(sessions, [("newer", 0), ("older", 1)]);
    assert_eq!(app.searchable.len(), 2);
    assert_eq!(app.count_text(), "1/2");

    // On a ready browser it replaces the corpus and keeps the selection.
    app.select(1);
    let refreshed = Browser::prepare_corpus(
        vec![
            dated(sandbox.path().join("older.jsonl"), "older", 1),
            dated(sandbox.path().join("newest.jsonl"), "newest", 3),
        ],
        true,
    );
    app.install_prepared(refreshed);
    assert_eq!(app.conversations().len(), 2);
    assert_eq!(
        app.selected_path(),
        Some(sandbox.path().join("older.jsonl"))
    );
    assert_eq!(app.conversations()[0].session_id, "newest");
}

#[test]
fn loader_prepares_off_thread_and_installs_on_a_later_poll() {
    let sandbox = Sandbox::new();
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
    let gate = loader.gate_next_preparation_for_test();
    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![
                dated(sandbox.path().join("older.jsonl"), "older", 1),
                dated(sandbox.path().join("newer.jsonl"), "newer", 2),
            ]),
            LoaderMessage::Done,
        ],
        false,
    );

    let poll = loader.poll(&mut browser);
    assert_eq!(poll.batches, 1);
    assert!(poll.event.is_none());
    assert!(poll.preparing);
    assert!(loader.is_running() && loader.is_preparing());
    // The loading rows stay visible (arrival order) while preparing.
    assert!(browser.is_loading());
    assert_eq!(browser.row(0).unwrap().session_id, "older");
    assert!(!loader.start_refresh(), "one load at a time");
    let again = loader.poll(&mut browser);
    assert!(again.preparing && again.event.is_none());

    gate.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while wakes.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the waker fires after preparing");
        std::thread::sleep(Duration::from_millis(1));
    }
    let event = poll_until_event(&mut loader, &mut browser);
    assert!(matches!(
        event,
        LoadEvent::Loaded {
            conversations: 2,
            project_errors: 0
        }
    ));
    assert!(!browser.is_loading());
    assert_eq!(browser.row(0).unwrap().session_id, "newer");
    assert_eq!(browser.conversations()[1].index, 1);
}

#[test]
fn poll_stays_fast_while_a_20k_corpus_is_prepared() {
    let sandbox = Sandbox::new();
    let body = "lorem ipsum dolor sit amet consectetur ".repeat(40);
    let corpus = (0..20_000)
        .map(|index| {
            let mut conversation = conversation(
                PathBuf::from(format!("/synthetic/p{}/{index:05}.jsonl", index % 50)),
                &format!("{body} item {index}"),
            );
            conversation.timestamp = Local::now() - ChronoDuration::minutes(index as i64 % 997);
            conversation
        })
        .collect::<Vec<_>>();
    let mut browser = Browser::new_loading(sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let (tx, rx) = mpsc::channel();
    tx.send(LoaderMessage::Batch(corpus)).unwrap();
    loader.start_with_receiver_for_test(rx, false);
    assert_eq!(loader.poll(&mut browser).batches, 1);
    assert_eq!(browser.conversations().len(), 20_000);

    tx.send(LoaderMessage::Done).unwrap();
    let mut preparing_polls = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    let event = loop {
        let started = Instant::now();
        let poll = loader.poll(&mut browser);
        let elapsed = started.elapsed();
        if let Some(event) = poll.event {
            break event;
        }
        assert!(poll.preparing);
        preparing_polls.push(elapsed);
        assert!(Instant::now() < deadline, "preparation did not finish");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(
        event,
        LoadEvent::Loaded {
            conversations: 20_000,
            ..
        }
    ));
    assert!(
        !preparing_polls.is_empty(),
        "the poll returned while preparing"
    );
    // Every poll is O(1) while the corpus is prepared; allow a rare
    // scheduler hiccup on a busy test machine.
    let slow = preparing_polls
        .iter()
        .filter(|elapsed| **elapsed > Duration::from_millis(5))
        .count();
    assert!(
        slow <= preparing_polls.len() / 10,
        "{slow} of {} polls took more than 5 ms: {preparing_polls:?}",
        preparing_polls.len()
    );
    // Sorted newest first after the install.
    let conversations = browser.conversations();
    assert!(
        conversations
            .windows(2)
            .all(|pair| pair[0].timestamp >= pair[1].timestamp)
    );
}

#[test]
fn rows_deleted_or_renamed_while_preparing_are_replayed() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let deleted = root.join("-p").join(format!("{UUID_A}.jsonl"));
    let renamed = root.join("-p").join(format!("{UUID_B}.jsonl"));
    write_claude_session(&deleted, sandbox.path(), "one", "two");
    write_claude_session(&renamed, sandbox.path(), "three", "four");
    let mut browser = Browser::new(
        vec![
            conversation(deleted.clone(), "deleted"),
            conversation(renamed.clone(), "renamed"),
        ],
        sandbox.options(),
    );
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let gate = loader.gate_next_preparation_for_test();
    // The refresh read both files before the edits below.
    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![
                conversation(deleted.clone(), "deleted"),
                conversation(renamed.clone(), "renamed"),
            ]),
            LoaderMessage::Done,
        ],
        true,
    );
    assert!(loader.poll(&mut browser).preparing);

    browser.delete_conversation(&deleted).unwrap();
    browser
        .rename_conversation(&renamed, "fresh title")
        .unwrap();
    gate.send(()).unwrap();
    let event = poll_until_event(&mut loader, &mut browser);

    assert!(matches!(
        event,
        LoadEvent::Refreshed {
            old: 1,
            new: 1,
            project_errors: 0
        }
    ));
    assert_eq!(browser.conversations().len(), 1);
    assert_eq!(browser.conversations()[0].path, renamed);
    assert_eq!(
        browser.conversations()[0].custom_title.as_deref(),
        Some("fresh title")
    );
}

#[test]
fn rows_deleted_or_renamed_while_a_refresh_streams_are_replayed() {
    // K1: the list stays interactive during a refresh (DESIGN 11.3), so
    // edits made while the loader still reads and buffers files, even
    // before its first poll, must survive the install.
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let deleted = root.join("-p").join(format!("{UUID_A}.jsonl"));
    let renamed = root.join("-p").join(format!("{UUID_B}.jsonl"));
    let added = sandbox.path().join("added.jsonl");
    write_claude_session(&deleted, sandbox.path(), "one", "two");
    write_claude_session(&renamed, sandbox.path(), "three", "four");
    let mut browser = Browser::new(
        vec![
            conversation(deleted.clone(), "deleted"),
            conversation(renamed.clone(), "renamed"),
        ],
        sandbox.options(),
    );
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let (tx, rx) = mpsc::channel();
    loader.start_with_receiver_for_test(rx, true);
    // The loader read both files before the edits below.
    tx.send(LoaderMessage::Batch(vec![
        conversation(deleted.clone(), "deleted"),
        conversation(renamed.clone(), "renamed"),
    ]))
    .unwrap();
    // Renamed before the loader's first poll ...
    browser
        .rename_conversation(&renamed, "fresh title")
        .unwrap();
    let first = loader.poll(&mut browser);
    assert!(first.event.is_none() && !first.preparing);
    // ... deleted while the refresh still streams.
    assert_eq!(browser.remove_paths(std::slice::from_ref(&deleted)), 1);
    tx.send(LoaderMessage::Batch(vec![conversation(
        added.clone(),
        "added",
    )]))
    .unwrap();
    tx.send(LoaderMessage::Done).unwrap();
    let event = poll_until_event(&mut loader, &mut browser);

    assert!(
        matches!(
            event,
            LoadEvent::Refreshed {
                old: 1,
                new: 2,
                project_errors: 0
            }
        ),
        "{event:?}"
    );
    assert!(
        browser.conversation_by_path(&deleted).is_none(),
        "a deleted row does not come back"
    );
    let (_, kept) = browser.conversation_by_path(&renamed).unwrap();
    assert_eq!(kept.custom_title.as_deref(), Some("fresh title"));
    assert!(browser.conversation_by_path(&added).is_some());
    // The search data follows the replayed corpus.
    assert_eq!(browser.searchable.len(), browser.conversations().len());
    for (index, conversation) in browser.conversations().iter().enumerate() {
        assert_eq!(conversation.index, index);
    }
    // The next refresh no longer replays them.
    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![conversation(deleted.clone(), "back again")]),
            LoaderMessage::Done,
        ],
        true,
    );
    poll_until_event(&mut loader, &mut browser);
    assert!(browser.conversation_by_path(&deleted).is_some());
}

#[test]
fn edits_made_before_a_load_started_are_not_replayed() {
    // A rename made long before a refresh must not override what the
    // refresh read (for example a title another program set later), and a
    // session deleted before it may legitimately come back.
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let renamed = root.join("-p").join(format!("{UUID_B}.jsonl"));
    let deleted = sandbox.path().join("deleted.jsonl");
    write_claude_session(&renamed, sandbox.path(), "three", "four");
    let mut browser = Browser::new(
        vec![
            conversation(renamed.clone(), "renamed"),
            conversation(deleted.clone(), "deleted"),
        ],
        sandbox.options(),
    );
    browser.rename_conversation(&renamed, "ours").unwrap();
    assert_eq!(browser.remove_paths(std::slice::from_ref(&deleted)), 1);

    let mut later = conversation(renamed.clone(), "renamed");
    later.custom_title = Some("theirs".to_string());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![later, conversation(deleted.clone(), "deleted")]),
            LoaderMessage::Done,
        ],
        true,
    );
    poll_until_event(&mut loader, &mut browser);
    let (_, kept) = browser.conversation_by_path(&renamed).unwrap();
    assert_eq!(kept.custom_title.as_deref(), Some("theirs"));
    assert!(browser.conversation_by_path(&deleted).is_some());
}

#[test]
fn a_refresh_that_keeps_the_rows_stops_tracking_edits() {
    let sandbox = Sandbox::new();
    let a = sandbox.path().join("a.jsonl");
    let mut browser = Browser::new(vec![conversation(a.clone(), "a")], sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let (tx, rx) = mpsc::channel();
    loader.start_with_receiver_for_test(rx, true);
    tx.send(LoaderMessage::Batch(vec![conversation(a.clone(), "a")]))
        .unwrap();
    loader.poll(&mut browser);
    assert_eq!(browser.remove_paths(std::slice::from_ref(&a)), 1);
    tx.send(LoaderMessage::Fatal(
        crate::error::AppError::ProjectsDirNotFound("projects".into()),
    ))
    .unwrap();
    assert!(matches!(
        poll_until_event(&mut loader, &mut browser),
        LoadEvent::RefreshFailed(_)
    ));
    assert!(browser.corpus_edits.since.is_none());
    assert!(browser.conversations().is_empty());
}

#[test]
fn a_query_is_never_searched_on_the_callers_thread() {
    // End of a load: the typed query goes to the worker; the loaded rows
    // stay until the answer arrives.
    let sandbox = Sandbox::new();
    let mut app = Browser::new_loading(sandbox.options());
    app.append_conversations(vec![
        conversation(sandbox.path().join("a.jsonl"), "needle"),
        conversation(sandbox.path().join("b.jsonl"), "hay"),
    ]);
    app.set_query("needle");
    app.finish_loading();
    assert!(app.has_search_work_in_flight());
    assert_eq!(app.filtered().len(), 2);
    wait_for_search(&mut app);
    assert_eq!(app.filtered().len(), 1);
    assert_eq!(app.row(0).unwrap().session_id, "a");

    // A scope change: the current rows are re-filtered at once, the query
    // is re-run in the background.
    app.set_excluded_projects(vec!["project".to_string()]);
    assert!(app.filtered().is_empty());
    wait_for_search(&mut app);
    assert!(app.filtered().is_empty());
    app.set_excluded_projects(Vec::new());
    wait_for_search(&mut app);
    assert_eq!(app.filtered().len(), 1);
}

/// S-007: while a typed query is pending the previous rows stay, whatever
/// intermediate queries were typed; only the final answer replaces them.
#[test]
fn typing_keeps_the_old_rows_until_the_final_answer() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(
        vec![
            conversation(sandbox.path().join("a.jsonl"), "needle"),
            conversation(sandbox.path().join("b.jsonl"), "hay"),
        ],
        sandbox.options(),
    );
    assert_eq!(app.filtered().len(), 2);
    for query in ["n", "ne", "nee", "needle"] {
        app.set_query(query);
        assert!(app.has_search_work_in_flight());
        assert_eq!(app.filtered().len(), 2, "{query}: the old rows stay");
    }
    wait_for_search(&mut app);
    assert_eq!(app.filtered().len(), 1);
    assert_eq!(app.row(0).unwrap().session_id, "a");
}

/// S-005: the worker's coalescing keeps only the latest queued command.
#[test]
fn the_search_worker_coalesces_to_the_latest_query() {
    let sandbox = Sandbox::new();
    let app = Browser::new(
        vec![conversation(sandbox.path().join("a.jsonl"), "needle")],
        sandbox.options(),
    );
    let (tx, rx) = mpsc::channel();
    let command = |generation: u64, query: &str| SearchCommand {
        query: query.to_string(),
        generation,
        mode: ListSearchMode::Lexical,
        conversations: app.corpus_snapshot(),
        searchable: app.searchable.clone(),
    };
    for (generation, query) in [(1, "n"), (2, "ne"), (3, "need")] {
        tx.send(command(generation, query)).unwrap();
    }
    let first = rx.recv().unwrap();
    assert_eq!(first.generation, 1);
    let latest = super::search_state::latest_command(first, &rx);
    assert_eq!((latest.generation, latest.query.as_str()), (3, "need"));
    assert!(rx.try_recv().is_err(), "the queue was drained");
    // A lone command is searched as is.
    tx.send(command(4, "x")).unwrap();
    let only = rx.recv().unwrap();
    assert_eq!(super::search_state::latest_command(only, &rx).generation, 4);
}

// --- K2: UUID jump without disk access ---

#[test]
fn uuid_jump_is_pending_until_resolved_and_ignores_stale_results() {
    let sandbox = Sandbox::new();
    let path = sandbox
        .projects_root()
        .join("-code-demo")
        .join(format!("{UUID_A}.jsonl"));
    write_claude_session(&path, sandbox.path(), "opening", "closing");
    let listed = sandbox.path().join("listed.jsonl");
    let mut app = Browser::new(vec![conversation(listed, "listed")], sandbox.options());
    let loaded = crate::history::load_session_by_uuid(&sandbox.env, UUID_A, true)
        .unwrap()
        .unwrap();

    app.set_query(UUID_A);
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Pending {
            uuid: UUID_A.to_string()
        })
    );
    // The user typed on before the background lookup finished.
    app.set_query(UUID_B);
    app.resolve_uuid_lookup(UUID_A, Some(loaded.clone()));
    assert_eq!(app.conversations().len(), 1, "a stale result is ignored");
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Pending {
            uuid: UUID_B.to_string()
        })
    );
    app.resolve_uuid_lookup(UUID_B, None);
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::NotFound {
            uuid: UUID_B.to_string()
        })
    );
    assert_eq!(app.filtered(), &[0], "not found keeps the rows");
    // A second resolve for a finished lookup does nothing.
    app.resolve_uuid_lookup(UUID_B, Some(loaded.clone()));
    assert_eq!(app.conversations().len(), 1);

    app.set_query(UUID_A);
    app.resolve_uuid_lookup(UUID_A, Some(loaded));
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 1,
            loaded_from_disk: true
        })
    );
    // Appended incrementally, with its search data.
    assert_eq!(app.searchable.len(), 2);
    assert_eq!(app.filtered(), &[1]);
    assert_eq!(app.conversations()[1].index, 1);
    // Typing the UUID again finds the loaded session in memory.
    app.set_query("");
    app.set_query(UUID_A.to_ascii_uppercase());
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 1,
            loaded_from_disk: false
        })
    );
}

#[test]
fn uuid_resolution_prefers_a_session_loaded_meanwhile() {
    let sandbox = Sandbox::new();
    let path = sandbox.path().join(format!("{UUID_A}.jsonl"));
    let mut app = Browser::new(
        vec![conversation(sandbox.path().join("x.jsonl"), "x")],
        sandbox.options(),
    );
    app.set_query(UUID_A);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Pending { .. })
    ));
    // The session arrived in the corpus before the lookup finished.
    app.push_conversation(conversation(path.clone(), "late"));
    app.corpus_changed();
    app.resolve_uuid_lookup(UUID_A, Some(conversation(path.clone(), "late")));
    assert_eq!(app.conversations().len(), 2, "no duplicate is appended");
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 1,
            loaded_from_disk: false
        })
    );
    assert_eq!(app.selected_path(), Some(path.clone()));

    // A refresh re-runs a pending jump against the new corpus.
    let mut app = Browser::new(
        vec![conversation(sandbox.path().join("x.jsonl"), "x")],
        sandbox.options(),
    );
    app.set_query(UUID_A);
    app.replace_conversations(vec![
        conversation(sandbox.path().join("x.jsonl"), "x"),
        conversation(path.clone(), "late"),
    ]);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Found {
            loaded_from_disk: false,
            ..
        })
    ));
    assert_eq!(app.selected_path(), Some(path));
}

#[test]
fn uuid_disk_lookup_uses_the_typed_text_and_the_preview_mode() {
    // S-017 / ADD C1: the file name is built from the query as typed, so
    // the match follows the file system's case rules; the preview honors
    // show_last.
    let sandbox = Sandbox::new();
    let cwd = sandbox.path().join("code").join("demo");
    let path = sandbox
        .projects_root()
        .join("-code-demo")
        .join(format!("{UUID_A}.jsonl"));
    write_claude_session(&path, &cwd, "opening question", "closing question");
    for show_last in [true, false] {
        let loaded = crate::history::load_session_by_uuid(&sandbox.env, UUID_A, show_last)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.path, path);
        assert_eq!(loaded.project_name.as_deref(), Some("demo"));
        assert_eq!(loaded.project_path.as_deref(), Some(cwd.as_path()));
        let expected = if show_last {
            &loaded.preview_last
        } else {
            &loaded.preview_first
        };
        assert_eq!(&loaded.preview, expected);
    }
    let upper = UUID_A.to_ascii_uppercase();
    let case_insensitive_fs = path.with_file_name(format!("{upper}.jsonl")).exists();
    let found = crate::history::load_session_by_uuid(&sandbox.env, &upper, true).unwrap();
    assert_eq!(found.is_some(), case_insensitive_fs);
    assert!(
        crate::history::load_session_by_uuid(&sandbox.env, UUID_B, true)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_uuid_jump_survives_a_refresh_or_reload_without_its_session() {
    // K2: upstream re-read the session from disk when a refresh re-ran
    // the UUID query. Core no longer reads the disk there, so the shown
    // session is kept instead of falling back to a Pending nobody resolves.
    let sandbox = Sandbox::new();
    let path = sandbox
        .projects_root()
        .join("-code-demo")
        .join(format!("{UUID_A}.jsonl"));
    write_claude_session(&path, sandbox.path(), "opening", "closing");
    let x = sandbox.path().join("x.jsonl");
    let mut app = Browser::new(vec![conversation(x.clone(), "x")], sandbox.options());
    app.set_query(UUID_A);
    let loaded = crate::history::load_session_by_uuid(&sandbox.env, UUID_A, true).unwrap();
    app.resolve_uuid_lookup(UUID_A, loaded);
    let found = Some(UuidLookup::Found {
        conversation_index: 1,
        loaded_from_disk: true,
    });
    assert_eq!(app.uuid_lookup(), found.as_ref());

    // A refresh (or a time-filter reload) that does not load the session.
    app.replace_conversations(vec![conversation(x.clone(), "x")]);
    assert_eq!(app.uuid_lookup(), found.as_ref());
    assert_eq!(app.filtered(), &[1]);
    assert_eq!(app.selected_path(), Some(path.clone()));
    assert_eq!(app.conversations().len(), 2);
    assert_eq!(app.searchable.len(), 2);
    assert_eq!(app.conversations()[1].index, 1);

    // Through the loader too, and when the jump is resolved while the
    // refresh streams.
    app.set_query("");
    app.replace_conversations(vec![conversation(x.clone(), "x")]);
    assert_eq!(
        app.conversations().len(),
        1,
        "the session leaves the browse list"
    );
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let (tx, rx) = mpsc::channel();
    loader.start_with_receiver_for_test(rx, true);
    tx.send(LoaderMessage::Batch(vec![conversation(x.clone(), "x")]))
        .unwrap();
    loader.poll(&mut app);
    app.set_query(UUID_A);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Pending { .. })
    ));
    let loaded = crate::history::load_session_by_uuid(&sandbox.env, UUID_A, true).unwrap();
    app.resolve_uuid_lookup(UUID_A, loaded);
    tx.send(LoaderMessage::Done).unwrap();
    poll_until_event(&mut loader, &mut app);
    assert_eq!(app.uuid_lookup(), found.as_ref());
    assert_eq!(app.selected_path(), Some(path.clone()));

    // A refresh that loads the session shows the loaded one, once.
    app.replace_conversations(vec![
        conversation(x.clone(), "x"),
        conversation(path.clone(), "loaded"),
    ]);
    assert_eq!(app.conversations().len(), 2);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Found {
            loaded_from_disk: false,
            ..
        })
    ));
    assert_eq!(app.selected_conversation().unwrap().preview, "loaded");

    // A session found in the corpus that a narrower reload drops stays.
    app.replace_conversations(vec![conversation(x.clone(), "x")]);
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 1,
            loaded_from_disk: false
        })
    );
    assert_eq!(app.selected_path(), Some(path.clone()));

    // Not found stays not found (rows kept); pending stays pending until
    // its lookup resolves.
    app.set_query("");
    app.set_query(UUID_B);
    app.resolve_uuid_lookup(UUID_B, None);
    app.replace_conversations(vec![conversation(x.clone(), "x")]);
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::NotFound {
            uuid: UUID_B.to_string()
        })
    );
    assert_eq!(app.filtered(), &[0]);
    app.set_query("");
    app.set_query(UUID_B);
    app.replace_conversations(vec![conversation(x.clone(), "x")]);
    assert_eq!(
        app.uuid_lookup(),
        Some(&UuidLookup::Pending {
            uuid: UUID_B.to_string()
        })
    );
    app.resolve_uuid_lookup(UUID_B, None);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::NotFound { .. })
    ));

    // A deleted session is not carried.
    app.set_query(UUID_A);
    let loaded = crate::history::load_session_by_uuid(&sandbox.env, UUID_A, true).unwrap();
    app.resolve_uuid_lookup(UUID_A, loaded);
    assert_eq!(app.remove_paths(std::slice::from_ref(&path)), 1);
    app.replace_conversations(vec![conversation(x.clone(), "x")]);
    assert!(app.conversation_by_path(&path).is_none());
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Pending { .. })
    ));
}

// --- K3: rename in two halves ---

fn rename_fixture(sandbox: &Sandbox) -> (Vec<Conversation>, PathBuf) {
    let first = sandbox.path().join(format!("{UUID_A}.jsonl"));
    let second = sandbox.path().join(format!("{UUID_B}.jsonl"));
    write_claude_session(&first, sandbox.path(), "first opening", "first closing");
    write_claude_session(&second, sandbox.path(), "second opening", "second closing");
    let mut first_conversation = conversation(first, "first");
    first_conversation.project_name = Some("alpha".into());
    first_conversation.project_path = Some(PathBuf::from("/work/alpha"));
    let mut second_conversation = conversation(second.clone(), "second");
    second_conversation.index = 1;
    second_conversation.project_name = Some("beta".into());
    second_conversation.project_path = Some(PathBuf::from("/work/beta"));
    (vec![first_conversation, second_conversation], second)
}

#[test]
fn apply_renamed_is_equivalent_to_rename_conversation() {
    let sandbox_a = Sandbox::new();
    let (conversations, target) = rename_fixture(&sandbox_a);
    let mut sync = Browser::new(conversations, sandbox_a.options().with_show_last(false));
    sync.rename_conversation(&target, "  Renamed  ").unwrap();

    let sandbox_b = Sandbox::new();
    let (conversations, target_b) = rename_fixture(&sandbox_b);
    let mut split = Browser::new(conversations, sandbox_b.options().with_show_last(false));
    let renamed = crate::history::rename_session(
        &sandbox_b.env,
        &target_b,
        Source::Claude,
        "  Renamed  ",
        false,
    )
    .unwrap();
    assert_eq!(renamed.custom_title.as_deref(), Some("Renamed"));
    assert_eq!(renamed.preview, renamed.preview_first);
    split.apply_renamed(&target_b, renamed).unwrap();

    for (left, right) in sync.conversations().iter().zip(split.conversations()) {
        assert_eq!(left.custom_title, right.custom_title);
        assert_eq!(left.preview, right.preview);
        assert_eq!(left.index, right.index);
        assert_eq!(left.project_name, right.project_name);
        assert_eq!(left.project_path, right.project_path);
        assert_eq!(left.path.file_name(), right.path.file_name());
    }
    assert_eq!(sync.filtered(), split.filtered());
    assert_eq!(sync.selected(), split.selected());
    // The renamed one kept its place, index and project (ADD B3).
    let renamed = &split.conversations()[1];
    assert_eq!(renamed.index, 1);
    assert_eq!(renamed.project_name.as_deref(), Some("beta"));
    assert_eq!(
        renamed.project_path.as_deref(),
        Some(Path::new("/work/beta"))
    );
    assert_eq!(split.searchable.len(), 2);
    assert!(
        search::search(
            split.conversations(),
            &split.searchable,
            "renamed",
            Local::now()
        )
        .contains(&1)
    );
}

#[test]
fn apply_renamed_reports_no_target_when_the_session_left_the_list() {
    let sandbox = Sandbox::new();
    let (conversations, target) = rename_fixture(&sandbox);
    let mut app = Browser::new(conversations, sandbox.options());
    let renamed =
        crate::history::rename_session(&sandbox.env, &target, Source::Claude, "x", true).unwrap();
    app.remove_paths(std::slice::from_ref(&target));
    assert!(matches!(
        app.apply_renamed(&target, renamed),
        Err(RenameError::NoTarget)
    ));
    assert_eq!(app.conversations().len(), 1);
}

#[test]
fn rename_clears_semantic_metadata_keeps_the_row_and_stamps_claude_time() {
    // A-021: re-parse keeps index and project, the row does not move, the
    // Claude timestamp becomes "now" (re-parsed without an mtime), semantic
    // metadata is cleared.
    let sandbox = Sandbox::new();
    let (conversations, target) = rename_fixture(&sandbox);
    let mut app = Browser::new(
        conversations,
        sandbox
            .options()
            .with_default_mode(ListSearchMode::Semantic),
    );
    app.semantic_search.results = HashMap::from([(1, test_semantic_metadata(1, "old evidence"))]);
    let before = Local::now();
    app.rename_conversation(&target, "titled").unwrap();
    assert!(app.semantic_result_metadata(1).is_none());
    assert_eq!(app.filtered(), &[0, 1]);
    assert_eq!(app.selected_path(), Some(target.clone()));
    assert!(app.conversations()[1].timestamp >= before - ChronoDuration::seconds(1));
}

#[test]
fn rename_with_a_lexical_query_re_searches_in_the_background() {
    let sandbox = Sandbox::new();
    let (conversations, target) = rename_fixture(&sandbox);
    let mut app = Browser::new(conversations, sandbox.options());
    app.set_query("second");
    wait_for_search(&mut app);
    assert_eq!(app.filtered(), &[1]);

    app.rename_conversation(&target, "zebra").unwrap();
    assert!(app.has_search_work_in_flight());
    assert_eq!(app.selected_path(), Some(target.clone()));
    wait_for_search(&mut app);
    assert_eq!(app.selected_path(), Some(target));
}

#[test]
fn pi_rename_terminates_an_unterminated_last_line() {
    // A-023: upstream appended the session_info entry to the last line.
    let sandbox = Sandbox::new();
    let path = sandbox.path().join("pi-session.jsonl");
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v1.jsonl"),
    )
    .unwrap();
    std::fs::write(&path, fixture.trim_end_matches('\n')).unwrap();

    let renamed =
        crate::history::rename_session(&sandbox.env, &path, Source::Pi, "Pi title", true).unwrap();
    assert_eq!(renamed.custom_title.as_deref(), Some("Pi title"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'));
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("every line is JSON");
    }
    assert_eq!(text.lines().count(), fixture.lines().count() + 1);
}

// --- K4: remove_paths ---

#[test]
fn remove_paths_removes_rows_keeps_the_selection_and_clears_metadata() {
    let sandbox = Sandbox::new();
    let paths = (0..5)
        .map(|index| sandbox.path().join(format!("{index}.jsonl")))
        .collect::<Vec<_>>();
    let mut app = Browser::new(
        paths
            .iter()
            .map(|path| conversation(path.clone(), "body"))
            .collect(),
        sandbox
            .options()
            .with_default_mode(ListSearchMode::Semantic),
    );
    app.semantic_search.results = HashMap::from([(3, test_semantic_metadata(3, "x"))]);
    app.select(3);

    // Rows before the selection go: the same conversation stays selected.
    assert_eq!(app.remove_paths(&[paths[0].clone(), paths[1].clone()]), 2);
    assert_eq!(app.selected_path(), Some(paths[3].clone()));
    assert_eq!(app.selected(), Some(1));
    assert!(app.semantic_search.results.is_empty());
    assert_eq!(app.searchable.len(), 3);

    // The selected row goes: the selection keeps its position.
    assert_eq!(app.remove_paths(std::slice::from_ref(&paths[3])), 1);
    assert_eq!(app.selected(), Some(1));
    assert_eq!(app.selected_path(), Some(paths[4].clone()));

    // The last row goes: the selection is clamped; unknown paths count 0.
    assert_eq!(
        app.remove_paths(&[paths[4].clone(), sandbox.path().join("none.jsonl")]),
        1
    );
    assert_eq!(app.selected(), Some(0));
    assert_eq!(app.remove_paths(std::slice::from_ref(&paths[2])), 1);
    assert_eq!(app.selected(), None);
    assert_eq!(app.count_text(), "0/0");
}

// --- K6: semantic suspend / resume ---

struct CountingEmbedder {
    passages: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl SemanticEmbedder for CountingEmbedder {
    fn embed_passages(&mut self, passages: &[String]) -> crate::error::Result<Vec<Vec<f32>>> {
        self.passages.fetch_add(passages.len(), Ordering::SeqCst);
        Ok(passages.iter().map(|text| embedding(text)).collect())
    }

    fn embed_query(&mut self, query: &str) -> crate::error::Result<Option<Vec<f32>>> {
        Ok(Some(embedding(query)))
    }
}

impl Drop for CountingEmbedder {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn embedding(text: &str) -> Vec<f32> {
    if text.contains("beta") {
        vec![0.0, 1.0]
    } else {
        vec![1.0, 0.0]
    }
}

struct Counters {
    factory: SemanticEmbedderFactory,
    passages: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

fn counting_factory() -> Counters {
    let passages = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let (p, d) = (passages.clone(), drops.clone());
    let factory: SemanticEmbedderFactory = Arc::new(move || {
        Ok(Box::new(CountingEmbedder {
            passages: p.clone(),
            drops: d.clone(),
        }) as Box<dyn SemanticEmbedder>)
    });
    Counters {
        factory,
        passages,
        drops,
    }
}

fn semantic_app(sandbox: &Sandbox, factory: SemanticEmbedderFactory) -> Browser {
    Browser::new(
        vec![
            conversation(sandbox.path().join("a.jsonl"), "visible alpha"),
            conversation(sandbox.path().join("b.jsonl"), "visible beta"),
        ],
        BrowserOptions::new(sandbox.env.clone())
            .with_embedder_factory(factory)
            .with_default_mode(ListSearchMode::Semantic),
    )
}

fn wait_for_semantic(app: &mut Browser) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.has_search_work_in_flight() {
        assert!(Instant::now() < deadline, "semantic search did not finish");
        if !app.poll() {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[test]
fn suspend_joins_the_worker_and_drops_its_embedder() {
    let sandbox = Sandbox::new();
    let counters = counting_factory();
    let mut app = semantic_app(&sandbox, counters.factory.clone());
    app.set_query("beta");
    wait_for_semantic(&mut app);
    assert_eq!(app.row(0).unwrap().session_id, "b");
    assert_eq!(counters.drops.load(Ordering::SeqCst), 0);
    assert!(app.semantic_search.worker_thread.is_some());

    app.suspend_semantic();
    // Joined: the worker thread has ended and dropped its embedder.
    assert!(app.semantic_search.worker_thread.is_none());
    assert!(app.semantic_search.worker_tx.is_none());
    assert_eq!(counters.drops.load(Ordering::SeqCst), 1);
    assert!(app.is_semantic_suspended());
    assert_eq!(app.semantic_status_text().as_deref(), Some("sem paused"));
    assert_eq!(app.search_status_text(), "sem 1/2 sem paused");

    // Queries still get the lexical fallback; no worker is started.
    app.set_query("alpha");
    assert_eq!(app.semantic_progress(), &SemanticProgress::Suspended);
    wait_for_semantic(&mut app);
    assert!(app.semantic_search.worker_tx.is_none());
    assert_eq!(app.filtered().len(), 1);
    assert_eq!(app.row(0).unwrap().session_id, "a");
    app.set_query("");
    assert!(app.semantic_search.worker_tx.is_none(), "no prewarm either");
    app.suspend_semantic(); // no-op
}

#[test]
fn resume_reloads_a_cache_written_meanwhile() {
    let sandbox = Sandbox::new();
    let counters = counting_factory();
    let mut app = semantic_app(&sandbox, counters.factory.clone());
    app.suspend_semantic();

    // The exclusive operation: generate the cache for the same corpus.
    let corpus = app.into_conversations();
    let factory = counters.factory.clone();
    let mut embedder = move || factory();
    crate::semantic_ops::generate_semantic_cache(
        &sandbox.env,
        &corpus,
        None,
        &crate::time_filter::TimeFilter::default(),
        &mut embedder,
        &mut |_| {},
        &crate::semantic::types::SemanticCancellationToken::new(),
    )
    .unwrap();
    let generated = counters.passages.load(Ordering::SeqCst);
    assert!(generated > 0);
    assert!(sandbox.env.embedding_cache_file().unwrap().exists());

    let mut app = semantic_app(&sandbox, counters.factory.clone());
    app.suspend_semantic();
    app.set_query("beta");
    wait_for_semantic(&mut app);
    app.resume_semantic();
    assert!(!app.is_semantic_suspended());
    assert!(
        app.has_search_work_in_flight(),
        "the query is dispatched again"
    );
    wait_for_semantic(&mut app);
    assert_eq!(app.row(0).unwrap().session_id, "b");
    assert_eq!(app.semantic_status_text(), None);
    // Everything came from the cache file: no passage was embedded again.
    assert_eq!(counters.passages.load(Ordering::SeqCst), generated);
}

#[test]
fn semantic_messages_from_a_suspended_worker_are_ignored() {
    let sandbox = Sandbox::new();
    let mut app = semantic_app(&sandbox, counting_factory().factory);
    let (_request_tx, _request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.suspend_semantic();
    // The suspended browser dropped the worker's channel.
    let sent = response_tx.send(SemanticSearchMessage::Progress {
        generation: app.search_generation,
        progress: SemanticProgress::Ranking,
    });
    assert!(sent.is_err());
    assert!(!app.poll());
    assert_eq!(app.semantic_progress(), &SemanticProgress::Suspended);
}

// --- K18: refresh policies ---

#[test]
fn apply_partial_refresh_applies_results_and_counts_project_errors() {
    let sandbox = Sandbox::new();
    let mut browser = Browser::new(
        vec![conversation(sandbox.path().join("old.jsonl"), "old")],
        sandbox.options(),
    );
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let (tx, rx) = mpsc::channel();
    for message in [
        LoaderMessage::Batch(vec![
            conversation(sandbox.path().join("a.jsonl"), "a"),
            conversation(sandbox.path().join("b.jsonl"), "b"),
        ]),
        LoaderMessage::ProjectError,
        LoaderMessage::Done,
    ] {
        tx.send(message).unwrap();
    }
    loader.start_with_receiver_and_policy_for_test(rx, true, RefreshPolicy::ApplyPartial);
    assert!(loader.is_refreshing());
    let event = poll_until_event(&mut loader, &mut browser);
    match &event {
        LoadEvent::Refreshed {
            old,
            new,
            project_errors,
        } => assert_eq!((*old, *new, *project_errors), (1, 2, 1)),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        event.status_message().as_deref(),
        Some("Refreshed sessions: 1 -> 2")
    );
    assert_eq!(browser.conversations().len(), 2);

    // The default policy keeps the sessions on a project error.
    let (tx, rx) = mpsc::channel();
    tx.send(LoaderMessage::Batch(vec![conversation(
        sandbox.path().join("c.jsonl"),
        "c",
    )]))
    .unwrap();
    tx.send(LoaderMessage::ProjectError).unwrap();
    tx.send(LoaderMessage::Done).unwrap();
    loader.start_with_receiver_and_policy_for_test(rx, true, RefreshPolicy::KeepOnProjectError);
    assert!(matches!(
        poll_until_event(&mut loader, &mut browser),
        LoadEvent::RefreshIncomplete
    ));
    assert_eq!(browser.conversations().len(), 2);
    assert_eq!(RefreshPolicy::default(), RefreshPolicy::KeepOnProjectError);
}

#[test]
fn start_refresh_with_runs_one_load_at_a_time() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.projects_root()).unwrap();
    let mut browser = Browser::new(Vec::new(), sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    assert!(loader.start_refresh_with(RefreshPolicy::ApplyPartial));
    assert!(!loader.start_refresh_with(RefreshPolicy::ApplyPartial));
    assert!(!loader.start_refresh());
    let event = poll_until_event(&mut loader, &mut browser);
    assert!(matches!(
        event,
        LoadEvent::Refreshed {
            old: 0,
            new: 0,
            project_errors: 0
        }
    ));
    assert!(!loader.is_running());
}

#[test]
fn semantic_worker_messages_types_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<PreparedCorpus>();
    assert_send::<CorpusSnapshot>();
    assert_send::<SemanticSearchMessage>();
}

// --- FEATURES rows owned by WP-C (core behavior of the list) ---

#[test]
fn l045_pi_and_omp_rows_need_a_working_directory_inside_the_workspace() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut inside = conversation(sandbox.path().join("pi-in.jsonl"), "in");
    inside.source = Source::Pi;
    inside.project_path = Some(workspace.clone());
    let mut cwd_only = conversation(sandbox.path().join("omp-cwd.jsonl"), "cwd");
    cwd_only.source = Source::Omp;
    cwd_only.cwd = Some(workspace.join(".").join("..").join("workspace"));
    let mut no_cwd = conversation(sandbox.path().join("pi-none.jsonl"), "none");
    no_cwd.source = Source::Pi;
    let mut elsewhere = conversation(sandbox.path().join("omp-out.jsonl"), "out");
    elsewhere.source = Source::Omp;
    elsewhere.project_path = Some(sandbox.path().to_path_buf());
    let mut app = Browser::new(
        vec![inside, cwd_only, no_cwd, elsewhere],
        sandbox
            .options()
            .with_workspace(Some(WorkspaceScope::new(&workspace).unwrap()))
            .with_workspace_filter(true),
    );
    // Canonical path equality; a row without any cwd is outside.
    assert_eq!(app.filtered(), &[0, 1]);
    app.set_workspace_filter(false);
    assert_eq!(app.filtered().len(), 4);
}

#[test]
fn l061_s008_navigation_and_edits_clear_the_anchor_so_results_select_row_zero() {
    let sandbox = Sandbox::new();
    let paths = (0..4)
        .map(|index| sandbox.path().join(format!("{index}.jsonl")))
        .collect::<Vec<_>>();
    let mut app = Browser::new(
        paths
            .iter()
            .map(|path| conversation(path.clone(), "needle"))
            .collect(),
        sandbox.options(),
    );
    // apply_filtered: the anchor's row when present ...
    app.selection_anchor = Some(paths[2].clone());
    app.apply_filtered(vec![3, 2, 1]);
    assert_eq!(app.selected(), Some(1));
    assert_eq!(app.selection_anchor, None, "the anchor is used once");
    // ... else row 0.
    app.selection_anchor = Some(paths[0].clone());
    app.apply_filtered(vec![3, 2]);
    assert_eq!(app.selected(), Some(0));

    // Every navigation and query edit clears it (upstream list_state).
    type Navigate = Box<dyn Fn(&mut Browser)>;
    let navigations: Vec<(&str, Navigate)> = vec![
        (
            "select",
            Box::new(|app| {
                app.select(1);
            }),
        ),
        ("select_prev", Box::new(|app| app.select_prev())),
        ("select_next", Box::new(|app| app.select_next())),
        ("select_first", Box::new(|app| app.select_first())),
        ("select_last", Box::new(|app| app.select_last())),
        ("select_page_up", Box::new(|app| app.select_page_up())),
        ("select_page_down", Box::new(|app| app.select_page_down())),
        (
            "select_half_page_down",
            Box::new(|app| app.select_half_page_down(4)),
        ),
        ("scroll_list", Box::new(|app| app.scroll_list(1))),
        ("set_query", Box::new(|app| type_char(app, 'n'))),
    ];
    for (name, navigate) in navigations {
        app.selection_anchor = Some(paths[3].clone());
        navigate(&mut app);
        assert_eq!(app.selection_anchor, None, "{name}");
    }
    // So the next result set selects the first row.
    app.set_query("needle");
    wait_for_search(&mut app);
    app.select(2);
    app.set_query("needl");
    wait_for_search(&mut app);
    assert_eq!(app.selected(), Some(0));
}

#[test]
fn l077_initial_load_keeps_duplicate_paths_and_refresh_dedupes_them() {
    let sandbox = Sandbox::new();
    let path = sandbox.path().join("same.jsonl");
    let mut loading = Browser::new_loading(sandbox.options());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![
                conversation(path.clone(), "first copy"),
                conversation(path.clone(), "second copy"),
            ]),
            LoaderMessage::Done,
        ],
        false,
    );
    assert!(matches!(
        poll_until_event(&mut loader, &mut loading),
        LoadEvent::Loaded {
            conversations: 2,
            ..
        }
    ));
    injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![
                conversation(path.clone(), "first copy"),
                conversation(path.clone(), "second copy"),
            ]),
            LoaderMessage::Done,
        ],
        true,
    );
    assert!(matches!(
        poll_until_event(&mut loader, &mut loading),
        LoadEvent::Refreshed { old: 2, new: 1, .. }
    ));
}

#[test]
fn s021_a_dead_semantic_worker_is_respawned_once() {
    let sandbox = Sandbox::new();
    let mut app = semantic_app(&sandbox, counting_factory().factory);
    // A worker whose channel is gone (it died): the dispatch respawns it.
    let (dead_tx, dead_rx) = mpsc::channel();
    drop(dead_rx);
    app.semantic_search.worker_tx = Some(dead_tx);
    app.semantic_search.worker_rx = Some(mpsc::channel().1);
    app.set_query("beta");
    assert!(app.semantic_error().is_none());
    wait_for_semantic(&mut app);
    assert!(app.semantic_error().is_none(), "{:?}", app.semantic_error());
    assert_eq!(app.row(0).unwrap().session_id, "b");
    assert!(app.semantic_search.worker_thread.is_some());
}
