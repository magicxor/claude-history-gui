//! Snapshot cards shown while loading (upstream `tui/app/snapshot_tests.rs`),
//! re-expressed through the browser API, plus the fork's loader side: the
//! held-back batches installed through the corpus preparation (K1), edits
//! made on cards, and the reconcile pass after the initial load.
//!
//! Differences from upstream: the workspace is a [`WorkspaceScope`] in the
//! sandbox (not the process cwd); upstream's key and click gating tests
//! (resume, fork and Ctrl+O on cards, no rename or delete dialog) are GUI
//! tests (`crates/gui/src/list/tests/snapshot.rs`). Upstream's index tests
//! (ac7f591) type through the browser API; the fork adds the loader's
//! `index-open` and `store-reconcile` hand-offs, edits with an index, the
//! worker's two answers and the cards it builds.

use super::test_helpers::{Sandbox, type_char, wait_for_search};
use super::*;
use crate::history::LoaderMessage;
use crate::store::generation::{Publish, PublishLock};
use crate::store::test_support::conversation;
use crate::time_filter::TimeFilter;
use chrono::TimeZone;
use std::time::{Duration, Instant, SystemTime};

const HOUR_MS: i64 = 3_600_000;

fn publish(conversations: &[Conversation]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    crate::store::generation::publish(dir.path(), conversations).unwrap();
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

/// Writes a Claude session file in project directory `project` of the
/// sandbox, with one prompt `text`. Its modification time (the
/// conversation's timestamp) is `24 - hour` hours back, so a later `hour` is
/// newer and the reconcile pass's fingerprint of it is known. Returns its
/// path.
fn write_session(sandbox: &Sandbox, project: &str, name: &str, hour: u32, text: &str) -> PathBuf {
    let dir = sandbox.projects_root().join(project);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let line = serde_json::json!({
        "type": "user",
        "timestamp": format!("2024-01-01T{hour:02}:00:00Z"),
        "cwd": "/tmp/work",
        "message": {"role": "user", "content": text}
    });
    std::fs::write(&path, format!("{line}\n")).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(3600 * (24 - u64::from(hour))))
        .unwrap();
    path
}

/// Three sessions on disk, the newest first in the returned paths.
fn write_sessions(sandbox: &Sandbox) -> Vec<PathBuf> {
    vec![
        write_session(sandbox, "-tmp-alpha", "c.jsonl", 3, "third prompt"),
        write_session(sandbox, "-tmp-beta", "b.jsonl", 2, "second prompt"),
        write_session(sandbox, "-tmp-alpha", "a.jsonl", 1, "first prompt"),
    ]
}

fn display(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

/// The debug log of the sandbox (empty before anything was logged).
fn debug_log(sandbox: &Sandbox) -> String {
    std::fs::read_to_string(sandbox.env.debug_log_file().unwrap()).unwrap_or_default()
}

/// Waits until the debug log holds `text` `count` times.
fn wait_for_log(sandbox: &Sandbox, text: &str, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while debug_log(sandbox).matches(text).count() < count {
        assert!(
            Instant::now() < deadline,
            "no {text:?}: {}",
            debug_log(sandbox)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
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
    let session = PreparedSession::new(renamed);
    let body = session.body_lower.clone();
    app.apply_renamed_session(&path, session).unwrap();
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
    // The search data built at the end of the load holds the renamed text
    // (the body normalized when the rename was prepared).
    let position = row.index;
    assert_eq!(&*app.searchable[position].body_lower, "renamed full text");
    assert!(Arc::ptr_eq(&app.searchable[position].body_lower, &body));
    assert!(
        search::search(
            &app.conversations,
            &app.searchable,
            "renamed",
            chrono::Local::now()
        )
        .contains(&position)
    );
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
fn a_complete_initial_load_reconciles_the_store_once() {
    let sandbox = Sandbox::new();
    let paths = write_sessions(&sandbox);
    let mut loader = snapshot_loader(&sandbox);
    assert!(loader.open_snapshot().is_none(), "no generation yet");

    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    poll_until_event(&mut loader, &mut app);
    assert_eq!(
        loader.finish_snapshot_write(),
        Some(Publish::Published("g000001".into()))
    );
    let root = crate::store::default_root(&sandbox.env).unwrap();
    assert!(root.starts_with(sandbox.path()), "{root:?}");
    loader.poll(&mut app);
    assert_eq!(app.search_index_generation(), Some("g000001"));
    assert!(
        debug_log(&sandbox).contains("Reconcile: Outcome { publish: Published(\"g000001\")"),
        "upstream's reconcile line: {}",
        debug_log(&sandbox)
    );

    // The next start draws its first frame from it.
    let mut next = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    app.show_snapshot(next.open_snapshot().expect("the published generation"));
    assert_eq!(listed_paths(&app), display(&paths));
    // Unchanged session files write nothing.
    assert!(next.start_initial_load());
    poll_until_event(&mut next, &mut app);
    assert_eq!(next.finish_snapshot_write(), Some(Publish::Unchanged));
    // A refresh starts no pass, even with a new session on disk.
    write_session(&sandbox, "-tmp-beta", "d.jsonl", 4, "fourth prompt");
    assert!(next.start_refresh());
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
fn partial_initial_loads_start_no_reconcile_pass() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
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
fn reconfiguring_keeps_a_running_pass() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    poll_until_event(&mut loader, &mut app);
    loader.reconfigure(
        sandbox.env.clone(),
        LoaderOptions::new(false).with_snapshot(true),
    );
    assert!(!loader.options().show_last);
    assert_eq!(
        loader.finish_snapshot_write(),
        Some(Publish::Published("g000001".into()))
    );
    drop(loader);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    assert_eq!(
        crate::store::Snapshot::open(&root, true, TimeFilter::default())
            .map(|snapshot| snapshot.len()),
        Some(3),
        "the pass finished"
    );
}

/// Quitting drops the loader: a pass still running is given up (no
/// generation, no build directory left) instead of holding up the exit.
#[test]
fn dropping_the_loader_gives_up_a_running_reconcile_pass() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let mut loader = snapshot_loader(&sandbox);
    let (gate, cancel) = loader.gate_next_snapshot_write_for_test();
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    poll_until_event(&mut loader, &mut app);
    // The pass has started and waits at the gate; let it go on only once
    // the drop has asked it to stop, so the pass sees the request.
    let release = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cancel.is_set() {
            assert!(Instant::now() < deadline, "the drop never cancelled");
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(gate);
    });
    drop(loader);
    release.join().unwrap();

    let root = crate::store::default_root(&sandbox.env).unwrap();
    assert!(
        crate::store::Snapshot::open(&root, true, TimeFilter::default()).is_none(),
        "the pass was given up"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name != "LOCK")
                .collect()
        })
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let log = debug_log(&sandbox);
    assert!(
        !log.contains("Reconcile") && !log.contains("list snapshot"),
        "a pass given up is not logged: {log}"
    );
}

/// A second GUI holds the store's lock: the pass tries again every
/// `BUSY_RETRY` (logging the busy pass once) and publishes once the lock is
/// free; dropping the loader while it waits ends the wait.
#[test]
fn a_locked_store_is_reconciled_once_the_lock_is_free() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    let held = PublishLock::try_acquire(&root).unwrap().unwrap();
    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    poll_until_event(&mut loader, &mut app);
    wait_for_log(&sandbox, "publish: Busy", 1);
    // Long enough for a few retries.
    std::thread::sleep(loader::BUSY_RETRY * 2);
    drop(held);
    assert_eq!(
        loader.finish_snapshot_write(),
        Some(Publish::Published("g000001".into()))
    );
    loader.poll(&mut app);
    assert_eq!(app.search_index_generation(), Some("g000001"));
    assert_eq!(
        debug_log(&sandbox).matches("publish: Busy").count(),
        1,
        "{}",
        debug_log(&sandbox)
    );

    let held = PublishLock::try_acquire(&root).unwrap().unwrap();
    write_session(&sandbox, "-tmp-beta", "d.jsonl", 4, "fourth prompt");
    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    poll_until_event(&mut loader, &mut app);
    wait_for_log(&sandbox, "publish: Busy", 2);
    drop(loader);
    drop(held);
    assert_eq!(
        crate::store::Snapshot::open(&root, true, TimeFilter::default())
            .unwrap()
            .generation_name(),
        "g000001",
        "nothing was published after the drop"
    );
}

#[test]
fn a_cancel_wakes_a_wait_for_the_lock() {
    let cancel = Arc::new(loader::Cancel::default());
    let waiting = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            (cancel.sleep(Duration::from_secs(60)), started.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(20));
    cancel.set();
    let (cancelled, waited) = waiting.join().unwrap();
    assert!(cancelled);
    assert!(waited < Duration::from_secs(60), "{waited:?}");
    assert!(cancel.sleep(Duration::from_secs(60)), "set stays set");
}

// --- the generation's search index (upstream ac7f591) ---

fn type_query(app: &mut Browser, query: &str) {
    for c in query.chars() {
        type_char(app, c);
    }
}

/// `corpus()` with a hidden match far into the oldest conversation's text
/// and one in the newest.
fn needle_corpus() -> Vec<Conversation> {
    let mut corpus = corpus();
    corpus[0].full_text = format!("{} hidden needle here", "padding ".repeat(400)).into();
    corpus[2].full_text = "needle in the newest".into();
    corpus
}

/// `needle_corpus()` plus a newer conversation with a match.
fn needle_corpus_with_late_needle() -> Vec<Conversation> {
    let mut loaded = needle_corpus();
    loaded.push(conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS));
    loaded[3].full_text = "late needle".into();
    loaded
}

#[test]
fn index_answers_queries_before_loading_with_evidence_from_frames() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(!app.readiness().lexical);
    assert!(app.open_search_index());
    assert!(app.readiness().lexical);
    assert!(app.is_loading());

    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
    let hidden = app.filtered()[1];
    let evidence = app
        .lexical_evidence(hidden)
        .expect("evidence from the frame");
    let (start, end) = evidence.context_ranges[0];
    assert_eq!(&evidence.text[start..end], "needle");
    assert!(evidence.text.len() < 4000);
    assert_eq!(app.count_text(), "Loading... 0");

    // Loading keeps the index and maps its results to loaded conversations.
    app.append_conversations(needle_corpus());
    app.finish_loading();
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"],
        "the rows the index answered stay until the query answers again"
    );
    wait_for_search(&mut app);
    assert_eq!(app.search_index_generation(), Some("g000001"));
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
    assert_eq!(
        &*app.conversations()[app.filtered()[0]].full_text,
        "needle in the newest"
    );
}

#[test]
fn index_results_match_in_memory_search_after_loading() {
    let sandbox = Sandbox::new();
    let corpus = needle_corpus();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    app.append_conversations(corpus.clone());
    app.finish_loading();
    assert!(app.search_index_generation().is_some());

    let mut in_memory = loading_app(&sandbox, None, vec![]);
    in_memory.append_conversations(corpus);
    in_memory.finish_loading();
    for query in [
        "needle",
        "full",
        "\"hidden needle\"",
        "padding needle",
        "zzz",
    ] {
        app.set_query(query);
        wait_for_search(&mut app);
        in_memory.set_query(query);
        wait_for_search(&mut in_memory);
        assert_eq!(listed_paths(&app), listed_paths(&in_memory), "{query}");
    }
}

#[test]
fn index_scope_follows_workspace_filter() {
    let sandbox = Sandbox::new();
    let beta_dir = sandbox.path().join("beta");
    std::fs::create_dir_all(&beta_dir).unwrap();
    let beta = crate::env::workspace_project_dir_name(&beta_dir);
    let mut corpus = needle_corpus();
    corpus[1].path = PathBuf::from(format!("/p/{beta}/b.jsonl"));
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, Some(&beta_dir), vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    type_query(&mut app, "full");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec![format!("/p/{beta}/b.jsonl")]);
    app.set_query("needle");
    wait_for_search(&mut app);
    assert!(
        listed_paths(&app).is_empty(),
        "the needles are out of scope"
    );

    app.toggle_workspace_filter();
    assert!(!app.workspace_filter());
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
    app.set_query("full text");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec![format!("/p/{beta}/b.jsonl")]);
    app.set_query("");
    assert_eq!(listed_paths(&app).len(), 3, "the browse list of the cards");
}

/// The current generation under `root`, opened and located in `corpus`
/// (what the reconcile thread hands the loader).
fn swap_of(root: &Path, corpus: CorpusSnapshot) -> cards::IndexSwap {
    let snapshot = crate::store::Snapshot::open(root, true, TimeFilter::default()).unwrap();
    let index = crate::store::SearchIndex::open(snapshot).unwrap();
    cards::IndexSwap {
        located: index.cards().locate(&corpus),
        index: Arc::new(index),
        corpus,
    }
}

#[test]
fn newer_generation_swaps_in_and_reruns_the_query_keeping_selection() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let loaded = needle_corpus_with_late_needle();
    app.append_conversations(loaded.clone());
    app.finish_loading();
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    // The startup index misses the new conversation: it is dropped and the
    // query runs in memory.
    assert_eq!(app.search_index_generation(), None);
    assert_eq!(
        listed_paths(&app),
        vec![
            "/p/-tmp-beta/d.jsonl",
            "/p/-tmp-alpha/c.jsonl",
            "/p/-tmp-alpha/a.jsonl"
        ]
    );
    app.select_next();
    app.select_next();

    crate::store::generation::publish(root.path(), &loaded).unwrap();
    assert!(app.swap_search_index(swap_of(root.path(), app.corpus_snapshot())));
    assert_eq!(app.search_index_generation(), Some("g000002"));
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec![
            "/p/-tmp-beta/d.jsonl",
            "/p/-tmp-alpha/c.jsonl",
            "/p/-tmp-alpha/a.jsonl"
        ]
    );
    assert_eq!(
        app.selected_path(),
        Some(PathBuf::from("/p/-tmp-alpha/a.jsonl"))
    );
}

#[test]
fn a_swap_for_a_corpus_that_changed_drops_the_stale_index() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let loaded = needle_corpus_with_late_needle();
    app.append_conversations(loaded.clone());
    app.finish_loading();
    crate::store::generation::publish(root.path(), &loaded).unwrap();
    let swap = swap_of(root.path(), app.corpus_snapshot());

    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app).len(),
        3,
        "in memory: the stale index is dropped"
    );
    // An edit after the publish: the new generation's positions are stale.
    assert_eq!(
        app.remove_paths(&[PathBuf::from("/p/-tmp-alpha/a.jsonl")]),
        1
    );
    assert!(!app.swap_search_index(swap));
    assert_eq!(app.search_index_generation(), None);
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-beta/d.jsonl", "/p/-tmp-alpha/c.jsonl"],
        "in memory: the late conversation is found"
    );
}

#[test]
fn snapshot_without_index_files_waits_for_loading() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let generation = std::fs::read_to_string(root.path().join("CURRENT")).unwrap();
    std::fs::remove_file(root.path().join(generation.trim()).join("base.fst")).unwrap();
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(!app.open_search_index());
    type_query(&mut app, "needle");
    assert!(!app.has_search_work_in_flight());

    app.append_conversations(needle_corpus());
    app.finish_loading();
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
}

#[test]
fn edits_on_cards_stop_the_index_until_the_load_ends() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app).len(), 2);
    app.set_query("needle ");
    assert!(app.has_search_work_in_flight());

    assert_eq!(
        app.remove_paths(&[PathBuf::from("/p/-tmp-alpha/c.jsonl")]),
        1
    );
    assert!(!app.readiness().lexical, "queries wait for the load again");
    assert!(
        !app.has_search_work_in_flight(),
        "the abandoned query does not run over the cards in memory"
    );
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-alpha/a.jsonl"]);
    assert_eq!(app.search_index_generation(), None);
    assert!(!app.open_search_index(), "not while the cards are edited");
    // A scope change shows the browse list of the cards again.
    app.set_excluded_projects(vec!["gamma".to_string()]);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-beta/b.jsonl", "/p/-tmp-alpha/a.jsonl"],
        "the deleted card stays out of the browse list"
    );
    type_query(&mut app, " again");
    assert!(!app.has_search_work_in_flight());
    app.set_query("needle");

    // The query runs in memory once loaded (a synchronous `finish_loading`
    // replays no edit; the loader's install does, see
    // `edits_made_on_cards_are_replayed_on_the_loaded_corpus`).
    app.append_conversations(needle_corpus());
    app.finish_loading();
    assert_eq!(app.search_index_generation(), None);
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
}

#[test]
fn after_loading_a_delete_keeps_the_index_in_step_and_a_rename_drops_it() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    app.append_conversations(needle_corpus());
    app.finish_loading();
    assert!(app.search_index_generation().is_some());

    assert_eq!(
        app.remove_paths(&[PathBuf::from("/p/-tmp-alpha/c.jsonl")]),
        1
    );
    assert!(app.search_index_generation().is_some());
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-alpha/a.jsonl"]);
    app.set_query("full");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-beta/b.jsonl"]);

    // What `rename_session` returns: the session re-read from its file.
    let mut renamed = conversation("/p/-tmp-beta/b.jsonl", 2 * HOUR_MS);
    renamed.custom_title = Some("Renamed".to_string());
    renamed.full_text = "renamed full text".into();
    app.apply_renamed(Path::new("/p/-tmp-beta/b.jsonl"), renamed)
        .unwrap();
    assert_eq!(
        app.search_index_generation(),
        None,
        "the index holds the old title"
    );
    app.set_query("renamed");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-beta/b.jsonl"]);
}

/// `count` conversations that all match `needle`, the newest last.
fn many_needles(count: usize) -> Vec<Conversation> {
    (0..count)
        .map(|i| {
            let mut conversation =
                conversation(&format!("/p/-tmp-alpha/{i:03}.jsonl"), i as i64 * HOUR_MS);
            conversation.full_text = format!("needle number {i}").into();
            conversation
        })
        .collect()
}

#[test]
fn the_index_answers_with_the_top_page_then_every_result() {
    let root = publish(&many_needles(120));
    let snapshot = crate::store::Snapshot::open(root.path(), true, TimeFilter::default()).unwrap();
    let index = Arc::new(crate::store::SearchIndex::open(snapshot).unwrap());
    let materialized: Arc<[std::sync::atomic::AtomicBool]> = (0..120)
        .map(|doc| std::sync::atomic::AtomicBool::new(doc < 10))
        .collect();
    let (tx, rx) = search_state::spawn_search_worker(None);
    tx.send(search_state::SearchCommand {
        query: "needle".to_string(),
        generation: 3,
        mode: ListSearchMode::Lexical,
        conversations: Arc::default(),
        searchable: Arc::default(),
        index: Some(search_state::IndexScope {
            index: index.clone(),
            docs: Arc::new(vec![true; 120]),
            cards: Some(materialized),
        }),
    })
    .unwrap();
    let page = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!page.complete);
    assert_eq!(page.filtered.len(), search_state::TOP_PAGE);
    assert!(Arc::ptr_eq(page.index.as_ref().unwrap(), &index));
    let carded: Vec<u32> = page.cards.iter().map(|(doc, _)| *doc).collect();
    assert!(
        page.filtered
            .iter()
            .all(|&doc| (doc < 10) != carded.contains(&(doc as u32))),
        "cards for the docs not materialized"
    );
    let complete = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(complete.complete);
    assert_eq!(complete.generation, 3);
    assert_eq!(complete.filtered.len(), 120);
    assert_eq!(complete.filtered[..50], page.filtered[..]);
    assert_eq!(
        complete.cards.len() + page.cards.len(),
        110,
        "each card is built once"
    );
}

#[test]
fn the_complete_ranking_keeps_a_row_selected_on_the_first_page() {
    let sandbox = Sandbox::new();
    let root = publish(&many_needles(120));
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let index = app.search_index.clone().unwrap();
    let (tx, rx) = mpsc::channel();
    app.search_rx = rx;
    type_query(&mut app, "needle");
    let generation = app.search_generation;
    let ranking: Vec<usize> = (0..120).map(|doc| (doc * 7) % 120).collect();
    let response = |filtered: Vec<usize>, complete: bool| SearchResponse {
        filtered,
        index: Some(index.clone()),
        complete,
        generation,
        mode: ListSearchMode::Lexical,
        evidence: HashMap::new(),
        cards: Vec::new(),
    };

    tx.send(response(ranking[..50].to_vec(), false)).unwrap();
    assert!(app.poll());
    assert_eq!(app.filtered().len(), 50);
    assert!(app.has_search_work_in_flight(), "the rest is coming");
    app.select_next();
    app.select_next();
    let chosen = app.selected_path();

    tx.send(response(ranking.clone(), true)).unwrap();
    assert!(app.poll());
    assert_eq!(app.filtered().len(), 120);
    assert!(!app.has_search_work_in_flight());
    assert_eq!(app.selected(), Some(2));
    assert_eq!(app.selected_path(), chosen);

    // An answer from an index no longer in use is dropped.
    app.drop_search_index();
    tx.send(response(ranking[..3].to_vec(), true)).unwrap();
    assert!(!app.poll());
    assert_eq!(app.filtered().len(), 120);
}

#[test]
fn the_loader_opens_the_index_off_the_ui_thread_and_swaps_in_the_reconciled_generation() {
    let sandbox = Sandbox::new();
    let root = crate::store::default_root(&sandbox.env).unwrap();
    let a = write_session(
        &sandbox,
        "-tmp-alpha",
        "a.jsonl",
        1,
        &format!("{} hidden needle here", "padding ".repeat(400)),
    );
    write_session(&sandbox, "-tmp-beta", "b.jsonl", 2, "full text");
    let c = write_session(&sandbox, "-tmp-alpha", "c.jsonl", 3, "needle in the newest");
    let sources = crate::store::reconcile::Sources::from_env(&sandbox.env);
    crate::store::reconcile::reconcile(&root, &sources).unwrap();
    let d = write_session(&sandbox, "-tmp-beta", "d.jsonl", 4, "late needle");

    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    app.show_snapshot(loader.open_snapshot().expect("the published generation"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !app.readiness().lexical {
        assert!(Instant::now() < deadline, "the index did not arrive");
        loader.poll(&mut app);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.search_index_generation(), Some("g000001"));
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), display(&[c.clone(), a.clone()]));

    assert!(loader.start_initial_load());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(matches!(event, LoadEvent::Loaded { .. }), "{event:?}");
    assert_eq!(
        app.search_index_generation(),
        None,
        "the cards miss a conversation"
    );
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app).len(), 3, "in memory");

    let Some(Publish::Published(published)) = loader.finish_snapshot_write() else {
        panic!("the pass publishes the new session");
    };
    loader.poll(&mut app);
    assert_eq!(app.search_index_generation(), Some(published.as_str()));
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), display(&[d, c, a]));
    let generation = crate::store::Generation::open(&root).unwrap();
    assert_eq!(generation.name(), published);
    assert!(
        generation.open_search_files().is_some(),
        "the published generation carries its text and index"
    );
}
