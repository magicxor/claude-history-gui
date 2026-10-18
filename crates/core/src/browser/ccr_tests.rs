//! Tests of the WP-C2 core change requests on the browser (DESIGN §4.4,
//! §17.11): CCR-1 (the install of a load or refresh uses what the
//! preparation computed off the UI thread), CCR-2 (paths and session ids are
//! indexed; every edit keeps the indices consistent) and CCR-3 (no search
//! work stays in flight for an empty semantic corpus). The timings are in
//! `perf_tests` (release).

use super::test_helpers::*;
use super::*;
use crate::history::{LoaderMessage, Source};
use crate::semantic_worker::{SemanticSearchMessage, SemanticWorkerCommand};
use chrono::{Local, TimeZone};
use std::time::{Duration, Instant};

const UUID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const UUID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const UUID_C: &str = "cccccccc-3333-4333-8333-333333333333";

/// A conversation in Claude project `project` with `text`, `day` its date.
fn session(root: &Path, project: &str, uuid: &str, text: &str, day: u32) -> Conversation {
    let mut conversation = plain_conversation(
        root.join(format!("-{project}"))
            .join(format!("{uuid}.jsonl")),
    );
    conversation.project_name = Some(project.to_string());
    conversation.timestamp = Local.with_ymd_and_hms(2024, 1, day, 0, 0, 0).unwrap();
    conversation.preview = text.to_string();
    conversation.full_text = text.to_string();
    conversation.search_text_lower = search::normalize_for_search(text);
    conversation.semantic_turns = vec![text.to_string()];
    conversation.semantic_turn_ranges = vec![crate::agent::refs::MessageRange::single(1)];
    conversation
}

/// A corpus with copies of one session in several projects, two sources
/// and an excluded project.
fn mixed_corpus(root: &Path) -> Vec<Conversation> {
    let mut pi = session(root, "pi", UUID_C, "pi session", 9);
    pi.source = Source::Pi;
    vec![
        session(root, "alpha", UUID_A, "first needle", 1),
        session(root, "beta", UUID_A, "copy needle", 2),
        session(root, "alpha", UUID_B, "second", 3),
        session(
            root,
            "hidden",
            "dddddddd-4444-4444-8444-444444444444",
            "hidden needle",
            4,
        ),
        session(root, "gamma", UUID_A, "third copy", 5),
        session(
            root,
            "beta",
            "eeeeeeee-5555-4555-8555-555555555555",
            "fifth needle",
            6,
        ),
        pi,
    ]
}

/// Every lookup the index answers equals a scan of the corpus.
fn assert_index_consistent(browser: &Browser) {
    let conversations = browser.conversations();
    for (position, conversation) in conversations.iter().enumerate() {
        let first_path = conversations
            .iter()
            .position(|other| other.path == conversation.path);
        assert_eq!(
            browser.position_of_path(&conversation.path),
            first_path,
            "path of {position}"
        );
        let all_paths = conversations
            .iter()
            .enumerate()
            .filter(|(_, other)| other.path == conversation.path)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(
            browser
                .index
                .path_positions(conversations, &conversation.path)
                .collect::<Vec<_>>(),
            all_paths
        );
        let upper = conversation.session_id.to_ascii_uppercase();
        let first_session = conversations
            .iter()
            .position(|other| other.session_id.eq_ignore_ascii_case(&upper));
        assert_eq!(
            browser.index.first_session(conversations, &upper),
            first_session,
            "session of {position}"
        );
    }
    let mixed = conversations
        .iter()
        .any(|conversation| conversation.source != conversations[0].source);
    assert_eq!(browser.has_multiple_sources(), mixed);
    if browser.scope_is_current() {
        let scan = list_state::filter_conversation_indices(
            0..conversations.len(),
            conversations,
            &browser.excluded_projects,
            browser.workspace_filter,
            browser.workspace.as_ref(),
        );
        assert_eq!(browser.index.scope.rows().as_ref(), &scan);
    }
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

fn row_paths(browser: &Browser) -> Vec<PathBuf> {
    (0..browser.filtered().len())
        .map(|row| browser.row(row).unwrap().path.clone())
        .collect()
}

// --- CCR-2: indexed lookups, consistent under every edit ---

#[test]
fn every_edit_keeps_the_indices_consistent() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new(
        mixed_corpus(&root),
        sandbox
            .options()
            .with_exclude_projects(vec!["hidden".to_string()]),
    );
    assert_index_consistent(&browser);
    assert!(browser.has_multiple_sources());

    // Rename the second copy of UUID_A: same path, new title.
    let target = browser.conversations()[1].clone();
    let mut renamed = Conversation::clone(&target);
    renamed.custom_title = Some("Renamed copy".to_string());
    browser.apply_renamed(&target.path, renamed).unwrap();
    assert_index_consistent(&browser);
    assert_eq!(
        browser.conversations()[1].custom_title.as_deref(),
        Some("Renamed copy")
    );

    // Remove the first copy and the Pi session: the others move up.
    let first = browser.conversations()[0].path.clone();
    let pi = browser.conversations()[6].path.clone();
    assert_eq!(browser.remove_paths(&[pi, first.clone()]), 2);
    assert_index_consistent(&browser);
    assert!(!browser.has_multiple_sources());
    assert_eq!(browser.position_of_path(&first), None);
    assert_eq!(
        browser.index.first_session(browser.conversations(), UUID_A),
        Some(0)
    );

    // A UUID jump of a session loaded from disk is appended and indexed.
    let loaded = session(
        &root,
        "delta",
        "ffffffff-6666-4666-8666-666666666666",
        "disk",
        7,
    );
    browser.set_query("ffffffff-6666-4666-8666-666666666666");
    browser.resolve_uuid_lookup("ffffffff-6666-4666-8666-666666666666", Some(loaded));
    assert_index_consistent(&browser);
    assert!(matches!(
        browser.uuid_lookup(),
        Some(UuidLookup::Found {
            loaded_from_disk: true,
            ..
        })
    ));

    // A scope change recomputes the scope once; edits follow it.
    browser.set_query("");
    browser.set_excluded_projects(vec!["beta".to_string()]);
    assert!(browser.scope_is_current());
    assert_index_consistent(&browser);
    let last = browser.conversations().last().unwrap().path.clone();
    browser.remove_paths(&[last]);
    assert_index_consistent(&browser);
    assert!(
        row_paths(&browser)
            .iter()
            .all(|path| !path.starts_with(root.join("-beta")))
    );
}

#[test]
fn an_idle_lexical_worker_pins_no_snapshot_so_edits_are_in_place() {
    // The lexical worker releases a query's snapshots before it answers: a
    // rename or a delete after the answer edits the browser's vectors in
    // place instead of copying 2 x n pointers (each a reference count on its
    // own cache line).
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new(mixed_corpus(&root), sandbox.options());
    browser.set_query("needle");
    wait_for_search(&mut browser);
    assert_eq!(browser.filtered().len(), 4);
    let corpus = Arc::as_ptr(&browser.conversations);
    let searchable = Arc::as_ptr(&browser.searchable);

    let target = browser.row(0).unwrap().clone();
    let mut renamed = target.clone();
    renamed.custom_title = Some("Renamed".to_string());
    browser.apply_renamed(&target.path, renamed).unwrap();
    assert_eq!(Arc::as_ptr(&browser.conversations), corpus, "rename");
    assert_eq!(Arc::as_ptr(&browser.searchable), searchable, "rename");
    wait_for_search(&mut browser);

    let victim = row_paths(&browser)
        .into_iter()
        .find(|path| *path != target.path)
        .unwrap();
    assert_eq!(browser.remove_paths(std::slice::from_ref(&victim)), 1);
    assert_eq!(Arc::as_ptr(&browser.conversations), corpus, "delete");
    assert_eq!(Arc::as_ptr(&browser.searchable), searchable, "delete");
    assert_index_consistent(&browser);

    // The next query searches the edited corpus.
    browser.set_query("");
    browser.set_query("needle");
    wait_for_search(&mut browser);
    let rows = row_paths(&browser);
    assert_eq!(rows.len(), 3);
    assert!(!rows.contains(&victim));
    assert!(rows.contains(&target.path));
    assert_eq!(
        browser
            .conversation_by_path(&target.path)
            .unwrap()
            .1
            .custom_title
            .as_deref(),
        Some("Renamed")
    );
}

#[test]
fn lookups_follow_path_equality_and_ignore_session_case() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let browser = Browser::new(mixed_corpus(&root), sandbox.options());
    let path = browser.conversations()[2].path.clone();
    let parent = path.parent().unwrap();
    let respelled = parent.join(".").join(path.file_name().unwrap());
    assert_eq!(respelled, path, "Path equality ignores `.` components");
    let (index, found) = browser.conversation_by_path(&respelled).unwrap();
    assert_eq!((index, &found.path), (2, &path));
    assert!(
        browser
            .conversation_by_path(&root.join("-alpha").join("missing.jsonl"))
            .is_none()
    );

    // The UUID jump: the first copy in corpus order, whatever the case.
    let mut browser = browser;
    browser.set_query(UUID_A.to_ascii_uppercase());
    assert_eq!(
        browser.uuid_lookup(),
        Some(&UuidLookup::Found {
            conversation_index: 0,
            loaded_from_disk: false
        })
    );
}

#[test]
fn remove_paths_removes_every_row_of_a_path_listed_twice() {
    // The initial load does not dedupe (C2): a path can be listed twice.
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let twice = session(&root, "alpha", UUID_A, "twice", 1);
    let other = session(&root, "alpha", UUID_B, "other", 2);
    let mut browser = Browser::new(vec![twice.clone(), other, twice.clone()], sandbox.options());
    assert_eq!(
        browser
            .index
            .path_positions(browser.conversations(), &twice.path)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    browser.select(2);
    assert_eq!(browser.remove_paths(std::slice::from_ref(&twice.path)), 2);
    assert_eq!(browser.conversations().len(), 1);
    assert_eq!(browser.selected(), Some(0));
    assert_index_consistent(&browser);
}

#[test]
fn delete_and_rename_by_captured_path_use_the_index() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new(mixed_corpus(&root), sandbox.options());
    let gone = root.join("-alpha").join(format!("{UUID_C}.jsonl"));
    assert!(matches!(
        browser.delete_conversation(&gone),
        Err(DeleteError::NoTarget)
    ));
    assert!(matches!(
        browser.apply_renamed(&gone, session(&root, "alpha", UUID_C, "x", 1)),
        Err(RenameError::NoTarget)
    ));
    assert_index_consistent(&browser);
}

// --- CCR-1: the install uses what the preparation computed ---

#[test]
fn an_initial_load_installs_the_scope_prepared_off_thread() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new_loading(
        sandbox
            .options()
            .with_exclude_projects(vec!["hidden".to_string()]),
    );
    browser.append_conversations(mixed_corpus(&root));
    assert_index_consistent(&browser);
    let prepared = Browser::prepare_shared(
        browser.corpus_snapshot(),
        false,
        Some(browser.prepare_context(false)),
    );
    let rows = prepared.index.scope.rows().clone();
    browser.install_prepared(prepared);
    assert!(!browser.is_loading());
    assert!(
        Arc::ptr_eq(browser.index.scope.rows(), &rows),
        "the prepared scope is installed as is"
    );
    assert_index_consistent(&browser);
    // Sorted newest first, the excluded project left out, indices set.
    assert_eq!(browser.filtered().len(), 6);
    assert_eq!(browser.row(0).unwrap().source, Source::Pi);
    assert!(
        browser
            .conversations()
            .iter()
            .enumerate()
            .all(|(index, conversation)| conversation.index == index)
    );
}

#[test]
fn a_delete_while_loading_does_not_make_the_install_copy_conversations() {
    // A tool deletion can remove a loading row. The lexical worker must not
    // keep the loading rows shared: the install would then copy every
    // conversation to set its index (O(corpus) on the UI thread).
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new_loading(sandbox.options());
    browser.append_conversations(mixed_corpus(&root));
    let gone = browser.conversations()[0].path.clone();
    assert_eq!(browser.remove_paths(std::slice::from_ref(&gone)), 1);
    assert!(browser.is_loading());
    let loaded = browser
        .conversations()
        .iter()
        .map(Arc::as_ptr)
        .collect::<HashSet<_>>();
    browser.finish_loading();
    assert!(!browser.is_loading());
    assert_eq!(browser.conversations().len(), 6);
    assert_index_consistent(&browser);
    for (index, conversation) in browser.conversations().iter().enumerate() {
        assert_eq!(conversation.index, index);
        assert!(
            loaded.contains(&Arc::as_ptr(conversation)),
            "conversation {index} was copied by the install"
        );
    }
    // The finished corpus reaches the lexical worker.
    browser.set_query("needle");
    wait_for_search(&mut browser);
    assert_eq!(browser.filtered().len(), 3, "the deleted row stays gone");
}

#[test]
fn settings_changed_while_preparing_are_applied_at_install() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new_loading(sandbox.options());
    browser.append_conversations(mixed_corpus(&root));
    let prepared = Browser::prepare_shared(
        browser.corpus_snapshot(),
        false,
        Some(browser.prepare_context(false)),
    );
    // The exclusions change after the preparation started.
    browser.set_excluded_projects(vec!["alpha".to_string(), "hidden".to_string()]);
    browser.install_prepared(prepared);
    assert_index_consistent(&browser);
    assert!(browser.scope_is_current());
    let projects = (0..browser.filtered().len())
        .map(|row| browser.row(row).unwrap().project_name.clone().unwrap())
        .collect::<Vec<_>>();
    assert!(
        projects
            .iter()
            .all(|name| name != "alpha" && name != "hidden"),
        "{projects:?}"
    );
    assert_eq!(projects.len(), 4);
}

#[test]
fn a_refresh_keeps_the_query_rows_through_the_prepared_remap() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new(mixed_corpus(&root), sandbox.options());
    browser.set_query("needle");
    wait_for_search(&mut browser);
    let before = row_paths(&browser);
    assert_eq!(before.len(), 4);
    browser.select(2);
    let selected = browser.selected_path().unwrap();

    // The refreshed corpus lists the same sessions in another order plus a
    // new one; the remap moves the rows without looking up their paths.
    let mut refreshed = mixed_corpus(&root);
    refreshed.reverse();
    refreshed.push(session(
        &root,
        "omega",
        "99999999-9999-4999-8999-999999999999",
        "new needle",
        20,
    ));
    let prepared = Browser::prepare_shared(
        Arc::new(refreshed.into_iter().map(Arc::new).collect()),
        true,
        Some(browser.prepare_context(true)),
    );
    assert!(prepared.remap.is_some());
    browser.install_prepared(prepared);
    // Quirk 16: the previous rows stay until the new results arrive.
    let mut kept = row_paths(&browser);
    kept.sort();
    let mut expected = before.clone();
    expected.sort();
    assert_eq!(kept, expected);
    assert_eq!(browser.selected_path(), Some(selected.clone()));
    assert_index_consistent(&browser);
    wait_for_search(&mut browser);
    assert_eq!(browser.filtered().len(), 5);
    assert_eq!(browser.selected_path(), Some(selected));
}

#[test]
fn a_refresh_after_an_edit_maps_the_rows_by_path() {
    // An edit while the refresh prepares replaces the corpus the remap was
    // built for: the install maps the rows by path instead, and replays the
    // edit.
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let corpus = mixed_corpus(&root);
    let mut browser = Browser::new(corpus.clone(), sandbox.options());
    browser.set_query("needle");
    wait_for_search(&mut browser);
    assert_eq!(browser.filtered().len(), 4);

    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let gate = loader.gate_next_preparation_for_test();
    let (tx, rx) = mpsc::channel();
    tx.send(LoaderMessage::Batch(corpus)).unwrap();
    tx.send(LoaderMessage::Done).unwrap();
    loader.start_with_receiver_for_test(rx, true);
    assert!(loader.poll(&mut browser).preparing);

    let deleted = browser.row(0).unwrap().path.clone();
    assert_eq!(browser.remove_paths(std::slice::from_ref(&deleted)), 1);
    gate.send(()).unwrap();
    let event = poll_until_event(&mut loader, &mut browser);
    assert!(
        matches!(event, LoadEvent::Refreshed { old: 6, new: 6, .. }),
        "{event:?}"
    );
    assert_index_consistent(&browser);
    let rows = row_paths(&browser);
    assert_eq!(rows.len(), 3);
    assert!(!rows.contains(&deleted));
    assert!(browser.position_of_path(&deleted).is_none());
    assert!(
        browser
            .conversations()
            .iter()
            .enumerate()
            .all(|(index, conversation)| conversation.index == index),
        "the replayed removal renumbers the corpus"
    );
}

#[test]
fn a_refresh_releases_the_previous_corpus() {
    // The previous conversations and their search data are freed (on the
    // corpus-release thread, so not on the caller's), not leaked.
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new(mixed_corpus(&root), sandbox.options());
    let old = Arc::downgrade(&browser.conversations()[0]);
    let old_search = Arc::downgrade(&browser.searchable[0]);
    browser.replace_conversations(mixed_corpus(&root));
    assert_eq!(browser.conversations().len(), 7);
    let deadline = Instant::now() + Duration::from_secs(10);
    while old.strong_count() > 0 || old_search.strong_count() > 0 {
        assert!(
            Instant::now() < deadline,
            "the previous corpus was not released"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_index_consistent(&browser);
}

#[test]
fn a_carried_uuid_result_survives_a_refresh_through_the_remap() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new(mixed_corpus(&root), sandbox.options());
    let id = "12345678-1234-4234-8234-123456789012";
    browser.set_query(id);
    browser.resolve_uuid_lookup(id, Some(session(&root, "disk", id, "from disk", 8)));
    let carried_path = browser.row(0).unwrap().path.clone();
    let prepared = Browser::prepare_shared(
        Arc::new(mixed_corpus(&root).into_iter().map(Arc::new).collect()),
        true,
        Some(browser.prepare_context(true)),
    );
    browser.install_prepared(prepared);
    assert_eq!(row_paths(&browser), [carried_path]);
    assert!(matches!(
        browser.uuid_lookup(),
        Some(UuidLookup::Found {
            loaded_from_disk: true,
            ..
        })
    ));
    assert_index_consistent(&browser);
}

// --- CCR-3: no search work stays in flight for an empty semantic corpus ---

#[test]
fn a_query_answer_ends_a_prewarm_the_worker_dropped() {
    // Ctrl+T then typing: the worker coalesces the prewarm with the query
    // and answers only the query. Without a corpus to embed no progress
    // message arrives that would end the prewarm.
    let sandbox = Sandbox::new();
    let mut app = Browser::new(
        vec![plain_conversation(sandbox.path().join("a.jsonl"))],
        sandbox.options(),
    );
    let (_request_tx, request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.toggle_list_search_mode();
    let prewarm = app
        .semantic_search
        .prewarm_generation
        .expect("prewarm sent");
    app.set_query("anything");
    let query = app.search_generation;
    assert!(query > prewarm);
    drop(request_rx);

    send_semantic_complete_response(
        &response_tx,
        query,
        Vec::new(),
        HashMap::new(),
        SemanticProgress::EmptyCorpus,
    );
    wait_for_search(&mut app);
    assert!(!app.has_search_work_in_flight());
    assert_eq!(app.semantic_search.prewarm_generation, None);
    assert_eq!(app.semantic_progress(), &SemanticProgress::EmptyCorpus);
    assert_eq!(app.semantic_status_text().as_deref(), Some("sem no text"));
}

#[test]
fn an_answer_older_than_the_prewarm_keeps_it_in_flight() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(
        vec![plain_conversation(sandbox.path().join("a.jsonl"))],
        sandbox.options(),
    );
    let (_request_tx, _request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.toggle_list_search_mode();
    let prewarm = app
        .semantic_search
        .prewarm_generation
        .expect("prewarm sent");
    response_tx
        .send(SemanticSearchMessage::Complete(
            crate::semantic_worker::SemanticSearchResponse {
                generation: prewarm - 1,
                filtered: Vec::new(),
                metadata: HashMap::new(),
                error: None,
                progress: SemanticProgress::EmptyCorpus,
                prewarm: false,
            },
        ))
        .unwrap();
    app.poll();
    assert_eq!(app.semantic_search.prewarm_generation, Some(prewarm));
    assert!(app.has_search_work_in_flight());
}

#[test]
fn an_empty_semantic_corpus_goes_idle_with_the_real_worker() {
    // End to end with the semantic worker thread (fake embedder): a corpus
    // without text, Ctrl+T and an immediate query. Whether or not the worker
    // coalesces the prewarm with the query, the search work ends.
    for corpus in [
        vec![plain_conversation(PathBuf::from("/x/no-text.jsonl"))],
        Vec::new(),
    ] {
        let sandbox = Sandbox::new();
        let mut app = Browser::new(corpus, sandbox.options());
        app.toggle_list_search_mode();
        app.set_query("anything");
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.has_search_work_in_flight() {
            assert!(
                Instant::now() < deadline,
                "search work still in flight: progress {:?}",
                app.semantic_progress()
            );
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.semantic_progress(), &SemanticProgress::EmptyCorpus);
        assert_eq!(app.semantic_status_text().as_deref(), Some("sem no text"));
    }
}

#[test]
fn the_semantic_scope_is_shared_not_copied() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut app = Browser::new(
        mixed_corpus(&root),
        sandbox
            .options()
            .with_exclude_projects(vec!["hidden".to_string()]),
    );
    let (_request_tx, request_rx, _response_tx) = connect_semantic_search_channels(&mut app);
    app.toggle_list_search_mode();
    app.set_query("needle");
    let scope = std::iter::from_fn(|| request_rx.try_recv().ok())
        .find_map(|command| match command {
            SemanticWorkerCommand::UpdateScope { indices, .. } => Some(indices),
            _ => None,
        })
        .expect("scope sent");
    assert!(Arc::ptr_eq(&scope, app.index.scope.rows()));
    assert_eq!(scope.len(), 6);
}

#[test]
fn a_delete_during_an_in_flight_lexical_search_re_dispatches_it() {
    // Seen on master: the delete dropped the pending answer and the old
    // rows (here a UUID row out of the query) stayed.
    let sandbox = Sandbox::new();
    let root = sandbox.projects_root();
    let mut browser = Browser::new_loading(sandbox.options());
    browser.append_conversations(mixed_corpus(&root));
    browser.finish_loading();
    browser.set_query(UUID_B);
    assert!(!browser.filtered().is_empty());
    browser.set_query("needle");
    let gone = browser.conversations()[1].path.clone();
    assert_eq!(browser.remove_paths(std::slice::from_ref(&gone)), 1);
    wait_for_search(&mut browser);
    assert_index_consistent(&browser);
    let rows = browser.filtered().to_vec();
    assert!(!rows.is_empty());
    for index in rows {
        assert!(
            browser.conversations()[index].full_text.contains("needle"),
            "row {index} does not match the query"
        );
    }
}
