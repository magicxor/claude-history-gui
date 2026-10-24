//! Snapshot cards shown while loading (upstream `tui/app/snapshot_tests.rs`),
//! re-expressed through the browser API, plus the fork's loader side: the
//! held-back batches installed through the corpus preparation (K1), edits
//! made on cards, and the snapshot write after the initial load.
//!
//! Differences from upstream: the workspace is a [`WorkspaceScope`] in the
//! sandbox (not the process cwd); upstream's key and click gating tests
//! (resume, fork and Ctrl+O on cards, no rename or delete dialog) are GUI
//! tests (`crates/gui/src/list/tests/snapshot.rs`).

use super::test_helpers::{Sandbox, type_char, wait_for_search};
use super::*;
use crate::history::LoaderMessage;
use crate::store::Publish;
use crate::store::test_support::{conversation, unknown_fingerprints};
use crate::time_filter::TimeFilter;
use chrono::TimeZone;
use std::time::{Duration, Instant};

const HOUR_MS: i64 = 3_600_000;

fn publish(conversations: &[Conversation]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let manifest = crate::store::generation::build_manifest(
        conversations,
        &unknown_fingerprints(conversations.len()),
    );
    crate::store::generation::publish(dir.path(), &manifest).unwrap();
    dir
}

/// A loading browser; `workspace` turns the workspace scope on.
fn loading_app(sandbox: &Sandbox, workspace: Option<&Path>, excluded: Vec<&str>) -> Browser {
    Browser::new_loading(
        sandbox
            .options()
            .with_workspace(workspace.map(|dir| WorkspaceScope::new(dir).unwrap()))
            .with_workspace_filter(workspace.is_some())
            .with_exclude_projects(excluded.into_iter().map(str::to_string).collect()),
    )
}

fn show(app: &mut Browser, root: &tempfile::TempDir, time: TimeFilter) {
    let snapshot = crate::store::Snapshot::open(root.path(), true, time).unwrap();
    app.show_snapshot(snapshot);
}

fn listed_paths(app: &Browser) -> Vec<String> {
    app.filtered()
        .iter()
        .map(|&index| app.conversations()[index].path.display().to_string())
        .collect()
}

fn corpus() -> Vec<Conversation> {
    vec![
        conversation("/p/-tmp-alpha/a.jsonl", HOUR_MS),
        conversation("/p/-tmp-beta/b.jsonl", 2 * HOUR_MS),
        conversation("/p/-tmp-alpha/c.jsonl", 3 * HOUR_MS),
    ]
}

#[test]
fn snapshot_rows_show_before_loading_and_swap_keeps_selection() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());

    assert!(app.readiness().list);
    assert!(app.is_loading());
    assert!(app.shows_snapshot());
    assert_eq!(
        listed_paths(&app),
        vec![
            "/p/-tmp-alpha/c.jsonl",
            "/p/-tmp-beta/b.jsonl",
            "/p/-tmp-alpha/a.jsonl"
        ]
    );
    assert_eq!(&*app.conversations()[0].preview, "last");
    assert_eq!(app.count_text(), "Loading... 0");
    app.select_next();

    let mut loaded = corpus();
    loaded.push(conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS));
    app.append_conversations(loaded);
    assert_eq!(app.conversations().len(), 3, "batches wait for the swap");
    assert_eq!(
        app.readiness().reconcile,
        ReconcileState::Running { loaded: 4 }
    );
    assert_eq!(app.count_text(), "Loading... 4");

    app.finish_loading();
    assert_eq!(*app.readiness(), Readiness::ready());
    assert!(!app.shows_snapshot());
    assert_eq!(app.conversations().len(), 4);
    assert_eq!(&*app.conversations()[0].full_text, "full text");
    assert_eq!(
        app.selected_path(),
        Some(PathBuf::from("/p/-tmp-beta/b.jsonl"))
    );
    assert_eq!(app.count_text(), "3/4");
}

#[test]
fn query_typed_over_snapshot_runs_after_loading() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[1].full_text = "needle".into();
    corpus[1].search_text_lower = "needle".into();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());

    for c in "needle".chars() {
        type_char(&mut app, c);
    }
    assert_eq!(app.query(), "needle");
    assert!(!app.has_search_work_in_flight());
    assert_eq!(app.filtered().len(), 3);

    app.append_conversations(corpus);
    app.finish_loading();
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-beta/b.jsonl"]);
}

#[test]
fn snapshot_rows_honor_workspace_exclusions_and_time_window() {
    let sandbox = Sandbox::new();
    let alpha_dir = sandbox.path().join("alpha");
    std::fs::create_dir_all(&alpha_dir).unwrap();
    let alpha = crate::env::workspace_project_dir_name(&alpha_dir);
    let corpus = vec![
        conversation(&format!("/p/{alpha}/a.jsonl"), HOUR_MS),
        conversation("/p/-tmp-beta/b.jsonl", 2 * HOUR_MS),
        conversation(&format!("/p/{alpha}/c.jsonl"), 3 * HOUR_MS),
        conversation("/p/-tmp-gamma/e.jsonl", 5 * HOUR_MS),
    ];
    let root = publish(&corpus);

    let mut app = loading_app(&sandbox, Some(&alpha_dir), vec!["gamma"]);
    let time = TimeFilter {
        after: Some(chrono::Local.timestamp_millis_opt(2 * HOUR_MS).unwrap()),
        before: None,
    };
    show(&mut app, &root, time);
    assert_eq!(listed_paths(&app), vec![format!("/p/{alpha}/c.jsonl")]);

    app.toggle_workspace_filter();
    assert!(!app.workspace_filter());
    assert_eq!(
        listed_paths(&app),
        vec![
            format!("/p/{alpha}/c.jsonl"),
            "/p/-tmp-beta/b.jsonl".to_string()
        ]
    );
    assert!(app.is_loading(), "re-reading the cards keeps loading");

    app.set_excluded_projects(Vec::new());
    assert_eq!(listed_paths(&app)[0], "/p/-tmp-gamma/e.jsonl");
}

#[test]
fn snapshot_rows_carry_what_open_resume_and_fork_need() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[2].project_path = Some(PathBuf::from("/tmp/alpha"));
    corpus[2].cwd = Some(PathBuf::from("/tmp/alpha"));
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());

    let card = app.selected_conversation().unwrap();
    assert_eq!(card.path, PathBuf::from("/p/-tmp-alpha/c.jsonl"));
    assert_eq!(card.source, crate::history::Source::Claude);
    assert_eq!(card.session_id, "c");
    assert_eq!(card.project_path, Some(PathBuf::from("/tmp/alpha")));
    assert_eq!(card.project_name.as_deref(), Some("alpha"));
    assert_eq!(
        app.conversation_by_path(Path::new("/p/-tmp-beta/b.jsonl"))
            .map(|(index, _)| index),
        Some(1)
    );
}

#[test]
fn without_snapshot_rows_wait_for_loading() {
    let sandbox = Sandbox::new();
    let mut app = loading_app(&sandbox, None, vec![]);
    app.append_conversations(corpus());
    assert!(!app.readiness().list);
    assert!(!app.shows_snapshot());
    app.finish_loading();
    assert!(app.readiness().list);
    assert_eq!(listed_paths(&app)[0], "/p/-tmp-alpha/c.jsonl");
}

#[test]
fn snapshot_pi_rows_match_workspace_by_canonical_project_path() {
    let sandbox = Sandbox::new();
    let current = sandbox.path().join("work").join("pi-project");
    std::fs::create_dir_all(&current).unwrap();
    let mut here = conversation("/pi/sessions/here.jsonl", HOUR_MS);
    here.source = crate::history::Source::Pi;
    here.project_path = Some(current.clone());
    let mut elsewhere = conversation("/pi/sessions/elsewhere.jsonl", 2 * HOUR_MS);
    elsewhere.source = crate::history::Source::Pi;
    elsewhere.project_path = Some(sandbox.path().join("elsewhere-project"));
    let root = publish(&[here, elsewhere]);

    let mut app = loading_app(&sandbox, Some(&current), vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert_eq!(listed_paths(&app), vec!["/pi/sessions/here.jsonl"]);
}

// --- the loader side (fork) ---

/// Drive `loader` until it reports a load event.
fn poll_until_event(loader: &mut HistoryLoader, browser: &mut Browser) -> LoadEvent {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(event) = loader.poll(browser).event {
            return event;
        }
        assert!(Instant::now() < deadline, "loader did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn start_injected(loader: &mut HistoryLoader, messages: Vec<LoaderMessage>) {
    let (tx, rx) = mpsc::channel();
    for message in messages {
        tx.send(message).unwrap();
    }
    loader.start_with_receiver_for_test(rx, false);
}

fn snapshot_loader(sandbox: &Sandbox) -> HistoryLoader {
    HistoryLoader::new(
        sandbox.env.clone(),
        LoaderOptions::default().with_snapshot(true),
    )
}

#[test]
fn the_install_swaps_in_the_held_back_batches_not_the_cards() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    app.select_last();

    let mut loaded = corpus();
    loaded.push(conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS));
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    start_injected(
        &mut loader,
        vec![LoaderMessage::Batch(loaded), LoaderMessage::Done],
    );
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: 4,
                project_errors: 0
            }
        ),
        "{event:?}"
    );
    assert!(!app.shows_snapshot());
    assert_eq!(
        listed_paths(&app),
        vec![
            "/p/-tmp-beta/d.jsonl",
            "/p/-tmp-alpha/c.jsonl",
            "/p/-tmp-beta/b.jsonl",
            "/p/-tmp-alpha/a.jsonl"
        ]
    );
    assert!(
        app.conversations()
            .iter()
            .enumerate()
            .all(|(index, conversation)| conversation.index == index
                && &*conversation.full_text == "full text"),
        "the loaded conversations, indexed"
    );
    assert_eq!(
        app.selected_path(),
        Some(PathBuf::from("/p/-tmp-alpha/a.jsonl")),
        "the selected card's session stays selected"
    );
    assert_eq!(
        loader.finish_snapshot_write(),
        None,
        "nothing is written with the snapshot option off"
    );
}

#[test]
fn a_failed_load_over_cards_keeps_what_arrived() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    start_injected(
        &mut loader,
        vec![
            LoaderMessage::Batch(vec![conversation("/p/-tmp-beta/d.jsonl", HOUR_MS)]),
            LoaderMessage::Fatal(crate::error::AppError::SessionNotFound("x".into())),
        ],
    );
    let event = poll_until_event(&mut loader, &mut app);
    assert!(matches!(event, LoadEvent::LoadFailed(_)), "{event:?}");
    assert!(!app.is_loading());
    assert!(!app.shows_snapshot());
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-beta/d.jsonl"]);
}

#[test]
fn edits_made_on_cards_are_replayed_on_the_loaded_corpus() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());

    let (tx, rx) = mpsc::channel();
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    loader.start_with_receiver_for_test(rx, false);
    tx.send(LoaderMessage::Batch(corpus())).unwrap();
    assert!(loader.poll(&mut app).event.is_none());

    assert_eq!(
        app.remove_paths(&[PathBuf::from("/p/-tmp-beta/b.jsonl")]),
        1
    );
    assert_eq!(listed_paths(&app).len(), 2, "the card left the list");
    tx.send(LoaderMessage::Done).unwrap();
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: 2,
                ..
            }
        ),
        "{event:?}"
    );
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
}

#[test]
fn a_rename_made_on_a_card_keeps_its_title_and_the_loaded_text() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    let path = PathBuf::from("/p/-tmp-beta/b.jsonl");
    let card = app
        .conversations()
        .iter()
        .find(|conversation| conversation.path == path)
        .expect("the card")
        .clone();
    assert!(card.full_text.is_empty(), "cards carry no full text");

    let (tx, rx) = mpsc::channel();
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    loader.start_with_receiver_for_test(rx, false);
    tx.send(LoaderMessage::Batch(corpus())).unwrap();
    assert!(loader.poll(&mut app).event.is_none());

    // What `rename_session` returns: the session re-read from its file.
    let mut renamed = conversation("/p/-tmp-beta/b.jsonl", 2 * HOUR_MS);
    renamed.custom_title = Some("Renamed".to_string());
    renamed.full_text = "renamed full text".into();
    renamed.search_text_lower = "renamed full text".into();
    app.apply_renamed(&path, renamed).unwrap();
    assert_eq!(listed_paths(&app).len(), 3);

    tx.send(LoaderMessage::Done).unwrap();
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: 3,
                ..
            }
        ),
        "{event:?}"
    );
    assert!(!app.shows_snapshot());
    let row = app
        .conversations()
        .iter()
        .find(|conversation| conversation.path == path)
        .expect("the renamed row");
    assert_eq!(row.custom_title.as_deref(), Some("Renamed"));
    assert_eq!(&*row.full_text, "renamed full text");
    assert_eq!(&*row.search_text_lower, "renamed full text");
    assert!(
        app.conversations()
            .iter()
            .enumerate()
            .all(|(index, conversation)| conversation.index == index),
        "the loaded conversations, indexed"
    );
    let untouched = app
        .conversations()
        .iter()
        .find(|conversation| conversation.path == Path::new("/p/-tmp-alpha/a.jsonl"))
        .unwrap();
    assert_eq!(&*untouched.full_text, "full text");
    assert_eq!(untouched.custom_title, None);
}

#[test]
fn a_complete_initial_load_writes_the_snapshot_once() {
    let sandbox = Sandbox::new();
    let mut loader = snapshot_loader(&sandbox);
    assert!(loader.open_snapshot().is_none(), "no generation yet");

    let mut app = loading_app(&sandbox, None, vec![]);
    start_injected(
        &mut loader,
        vec![LoaderMessage::Batch(corpus()), LoaderMessage::Done],
    );
    poll_until_event(&mut loader, &mut app);
    assert_eq!(
        loader.finish_snapshot_write(),
        Some(Publish::Published("g000001".into()))
    );
    let root = crate::store::default_root(&sandbox.env).unwrap();
    assert!(root.starts_with(sandbox.path()), "{root:?}");

    // The next start draws its first frame from it.
    let mut next = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    app.show_snapshot(next.open_snapshot().expect("the written generation"));
    assert_eq!(listed_paths(&app).len(), 3);
    // The same corpus writes nothing new.
    start_injected(
        &mut next,
        vec![LoaderMessage::Batch(corpus()), LoaderMessage::Done],
    );
    poll_until_event(&mut next, &mut app);
    assert_eq!(next.finish_snapshot_write(), Some(Publish::Unchanged));
    // A refresh never writes, even of a changed corpus.
    let mut refreshed = corpus();
    refreshed.push(conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS));
    let (tx, rx) = mpsc::channel();
    tx.send(LoaderMessage::Batch(refreshed)).unwrap();
    tx.send(LoaderMessage::Done).unwrap();
    next.start_with_receiver_for_test(rx, true);
    let event = poll_until_event(&mut next, &mut app);
    assert!(
        matches!(event, LoadEvent::Refreshed { new: 4, .. }),
        "{event:?}"
    );
    assert_eq!(next.finish_snapshot_write(), None);
    assert_eq!(
        crate::store::Snapshot::open(&root, true, TimeFilter::default())
            .unwrap()
            .generation_name(),
        "g000001"
    );
}

#[test]
fn partial_initial_loads_write_no_snapshot() {
    let sandbox = Sandbox::new();
    let root = crate::store::default_root(&sandbox.env).unwrap();
    let cases: Vec<(&str, LoaderOptions, Vec<LoaderMessage>, bool)> = vec![
        (
            "a project error",
            LoaderOptions::default().with_snapshot(true),
            vec![
                LoaderMessage::Batch(corpus()),
                LoaderMessage::ProjectError,
                LoaderMessage::Done,
            ],
            true,
        ),
        (
            "a time filter",
            LoaderOptions::default()
                .with_snapshot(true)
                .with_time_filter(TimeFilter {
                    after: Some(chrono::Local.timestamp_millis_opt(0).unwrap()),
                    before: None,
                }),
            vec![LoaderMessage::Batch(corpus()), LoaderMessage::Done],
            true,
        ),
        (
            "a dropped loader",
            LoaderOptions::default().with_snapshot(true),
            vec![LoaderMessage::Batch(corpus())],
            true,
        ),
        (
            "the option off",
            LoaderOptions::default(),
            vec![LoaderMessage::Batch(corpus()), LoaderMessage::Done],
            true,
        ),
        (
            "an empty corpus",
            LoaderOptions::default().with_snapshot(true),
            vec![LoaderMessage::Done],
            false,
        ),
    ];
    for (case, options, messages, loaded) in cases {
        let mut loader = HistoryLoader::new(sandbox.env.clone(), options);
        let mut app = loading_app(&sandbox, None, vec![]);
        start_injected(&mut loader, messages);
        let event = poll_until_event(&mut loader, &mut app);
        assert_eq!(
            matches!(event, LoadEvent::Loaded { .. }),
            loaded,
            "{case}: {event:?}"
        );
        assert_eq!(loader.finish_snapshot_write(), None, "{case}");
        assert!(!root.join("CURRENT").exists(), "{case}");
    }
}

#[test]
fn reconfiguring_keeps_a_running_write_and_dropping_joins_it() {
    let sandbox = Sandbox::new();
    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    start_injected(
        &mut loader,
        vec![LoaderMessage::Batch(corpus()), LoaderMessage::Done],
    );
    poll_until_event(&mut loader, &mut app);
    loader.reconfigure(
        sandbox.env.clone(),
        LoaderOptions::new(false).with_snapshot(true),
    );
    assert!(!loader.options().show_last);
    drop(loader);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    assert!(
        crate::store::Snapshot::open(&root, true, TimeFilter::default()).is_some(),
        "the write finished before the loader was gone"
    );
}
