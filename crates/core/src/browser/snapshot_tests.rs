//! Generation mode (upstream `tui/app/snapshot_tests.rs`, 358becb),
//! re-expressed through the browser API, plus the fork's loader side: store
//! mode's reconcile passes, the `generation-open` hand-off and the events
//! the GUI keeps.
//!
//! Differences from upstream: the workspace is a [`WorkspaceScope`] in the
//! sandbox (not the process cwd); upstream's key and click gating tests are
//! GUI tests (`crates/gui/src/list/tests/snapshot.rs`); a rename goes
//! through [`Browser::apply_renamed`] (upstream typed it into its dialog).
//! Upstream's runtime (the detached `--reconcile` child and the `CURRENT`
//! polling of its event loop) is the fork's [`HistoryLoader`] in store mode,
//! tested with session files in the sandbox.

use super::test_helpers::{Sandbox, connect_semantic_search_channels, type_char, wait_for_search};
use super::*;
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

fn republish(root: &tempfile::TempDir, conversations: &[Conversation]) -> crate::store::Snapshot {
    crate::store::generation::publish(root.path(), conversations).unwrap();
    crate::store::Snapshot::open(root.path(), true, TimeFilter::default()).unwrap()
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

fn type_query(app: &mut Browser, query: &str) {
    for c in query.chars() {
        type_char(app, c);
    }
}

#[test]
fn snapshot_rows_show_first_and_index_lists_the_rest_keeping_selection() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    assert_eq!(app.count_text(), "Loading...");
    show(&mut app, &root, TimeFilter::default());

    assert!(app.readiness().list);
    assert!(!app.is_loading());
    assert!(app.shows_snapshot());
    assert!(!app.generation_open());
    assert_eq!(
        listed_paths(&app),
        vec![
            "/p/-tmp-alpha/c.jsonl",
            "/p/-tmp-beta/b.jsonl",
            "/p/-tmp-alpha/a.jsonl"
        ]
    );
    assert_eq!(&*app.conversations()[0].preview, "last");
    assert_eq!(&*app.conversations()[0].full_text, "");
    assert_eq!(app.count_text(), "1/3");
    app.select_next();

    assert!(app.open_search_index());
    assert!(app.generation_open());
    assert!(app.readiness().lexical);
    assert_eq!(app.generation_len(), Some(3));
    assert_eq!(listed_paths(&app).len(), 3);
    assert_eq!(
        app.selected_path(),
        Some(PathBuf::from("/p/-tmp-beta/b.jsonl"))
    );
    // Rows are docs once the generation is open.
    assert_eq!(app.row_doc(1), Some(1));
}

#[test]
fn query_typed_before_the_index_opens_runs_on_it() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[1].full_text = "needle".into();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());

    type_query(&mut app, "needle");
    assert_eq!(app.query(), "needle");
    assert!(!app.has_search_work_in_flight());
    assert_eq!(app.filtered().len(), 3);

    assert!(app.open_search_index());
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

    app.set_excluded_projects(Vec::new());
    assert_eq!(listed_paths(&app)[0], "/p/-tmp-gamma/e.jsonl");

    // Once open, the browse list follows the same scope.
    assert!(app.open_search_index());
    assert_eq!(listed_paths(&app).len(), 3);
    // Fork: the time filter changes at runtime.
    app.set_time_filter(TimeFilter::default());
    assert_eq!(app.time_filter(), TimeFilter::default());
    assert_eq!(listed_paths(&app).len(), 4);
    app.set_time_filter(time);
    assert_eq!(listed_paths(&app).len(), 3);
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
    assert_eq!(card.source, crate::history::model::SourceKind::Claude);
    assert_eq!(card.session_id, "c");
    assert_eq!(card.project_path, Some(PathBuf::from("/tmp/alpha")));
    assert_eq!(card.project_name.as_deref(), Some("alpha"));
    assert_eq!(
        app.conversation_by_path(Path::new("/p/-tmp-beta/b.jsonl"))
            .map(|(index, _)| index),
        Some(1)
    );
    assert!(app.open_search_index());
    assert_eq!(
        app.conversation_at(Path::new("/p/-tmp-alpha/c.jsonl"))
            .map(|c| c.session_id),
        Some("c".to_string())
    );
}

#[test]
fn without_a_generation_rows_wait() {
    let sandbox = Sandbox::new();
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(!app.readiness().list);
    assert!(app.is_loading());
    app.append_conversations(corpus());
    assert!(!app.shows_snapshot());
    assert_eq!(app.count_text(), "Loading... 3");
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
    here.source = crate::history::model::SourceKind::Pi;
    here.project_path = Some(current.clone());
    let mut elsewhere = conversation("/pi/sessions/elsewhere.jsonl", 2 * HOUR_MS);
    elsewhere.source = crate::history::model::SourceKind::Pi;
    elsewhere.project_path = Some(sandbox.path().join("elsewhere-project"));
    let root = publish(&[here, elsewhere]);

    let mut app = loading_app(&sandbox, Some(&current), vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert_eq!(listed_paths(&app), vec!["/pi/sessions/here.jsonl"]);
    assert!(!app.has_multiple_sources());
}

#[test]
fn the_source_label_follows_the_generation() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[0].source = crate::history::model::SourceKind::Pi;
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.has_multiple_sources());
    assert!(app.open_search_index());
    assert!(app.has_multiple_sources());
}

/// `corpus()` with a hidden match far into the oldest conversation's text
/// and one in the newest.
fn needle_corpus() -> Vec<Conversation> {
    let mut corpus = corpus();
    corpus[0].full_text = format!("{} hidden needle here", "padding ".repeat(400)).into();
    corpus[2].full_text = "needle in the newest".into();
    corpus
}

#[test]
fn index_answers_queries_with_evidence_from_frames() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(!app.readiness().lexical);
    assert!(app.open_search_index());
    assert!(app.readiness().lexical);

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

    // Rows the worker left without evidence get it from their frames when
    // shown, off the caller's thread.
    app.lexical_evidence.clear();
    app.fill_visible_evidence(0..10);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.lexical_evidence(hidden).is_none() {
        assert!(Instant::now() < deadline, "no evidence from the frame");
        app.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    let evidence = app.lexical_evidence(hidden).unwrap();
    let (start, end) = evidence.context_ranges[0];
    assert_eq!(&evidence.text[start..end], "needle");
}

#[test]
fn automated_and_child_threads_are_hidden_in_generation_mode() {
    use crate::history::sources::Registry;
    const EXEC: &str = "01a00000-0000-7000-8000-000000000006";
    const SUB: &str = "01a00000-0000-7000-8000-000000000007";
    fn settle(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                settle(&path);
            } else {
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(SystemTime::now() - Duration::from_secs(3600))
                    .unwrap();
            }
        }
    }

    let sandbox = Sandbox::new();
    let work = tempfile::tempdir().unwrap();
    let home = crate::history::sources::codex::tests::home_copy(work.path());
    settle(&home);
    let root = work.path().join("store");
    let registry = Registry::with_roots(None, None, None).with_codex(Some(home));
    crate::store::reconcile::reconcile(&root, &registry).unwrap();

    let mut app = loading_app(&sandbox, None, vec![]);
    let snapshot = crate::store::Snapshot::open(&root, true, TimeFilter::default()).unwrap();
    app.show_snapshot(snapshot);
    assert!(app.open_search_index());

    let ids = |app: &Browser| -> Vec<String> {
        app.filtered()
            .iter()
            .map(|&index| app.conversations()[index].session_id.to_string())
            .collect()
    };
    let listed = ids(&app);
    assert!(!listed.is_empty());
    assert!(!listed.iter().any(|id| id == EXEC || id == SUB));

    for (query, hidden) in [("iguana", EXEC), ("armadillo", SUB)] {
        app.set_query(query);
        wait_for_search(&mut app);
        assert!(!ids(&app).iter().any(|id| id == hidden), "{query}");
    }
    app.set_query("walrus");
    wait_for_search(&mut app);
    assert!(!ids(&app).is_empty());

    // Upstream parity: the session id still finds a hidden thread.
    for hidden in [EXEC, SUB] {
        app.set_query(hidden);
        wait_for_search(&mut app);
        assert!(ids(&app).iter().any(|id| id == hidden), "{hidden}");
    }
}

#[test]
fn index_results_match_in_memory_search() {
    let sandbox = Sandbox::new();
    let corpus = needle_corpus();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());

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

#[test]
fn newer_generation_swaps_in_and_reruns_the_query_keeping_selection() {
    let sandbox = Sandbox::new();
    let corpus = needle_corpus();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
    app.select_next();

    let same = crate::store::Snapshot::open(root.path(), true, TimeFilter::default()).unwrap();
    assert!(!app.swap_generation(same), "same generation");

    let mut newer = corpus.clone();
    newer.push(conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS));
    newer[3].full_text = "late needle".into();
    let newer = republish(&root, &newer);
    assert!(app.swap_generation(newer));
    assert_eq!(app.generation_len(), Some(4));
    assert_eq!(app.search_index_generation(), Some("g000002"));
    // Fork (quirk 16, upstream 7ec719a): the rows stay until the query
    // answers on the new index.
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
    assert_eq!(
        app.selected_path(),
        Some(PathBuf::from("/p/-tmp-alpha/a.jsonl"))
    );
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
fn snapshot_without_index_files_browses_until_a_generation_has_them() {
    let sandbox = Sandbox::new();
    let root = publish(&needle_corpus());
    let generation = std::fs::read_to_string(root.path().join("CURRENT")).unwrap();
    std::fs::remove_file(root.path().join(generation.trim()).join("base.fst")).unwrap();
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(!app.open_search_index());
    assert!(app.generation_open(), "the cards are listed anyway");
    type_query(&mut app, "needle");
    assert!(!app.has_search_work_in_flight());
    assert_eq!(app.filtered().len(), 3);

    let mut newer = needle_corpus();
    newer[1].custom_title = Some("retitled".to_string());
    let newer = republish(&root, &newer);
    assert!(app.swap_generation(newer));
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
}

#[test]
fn deleted_rows_stay_hidden_until_a_generation_drops_them() {
    let sandbox = Sandbox::new();
    let corpus = needle_corpus();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    assert!(!app.take_reconcile_request());

    assert_eq!(
        app.remove_paths(&[PathBuf::from("/p/-tmp-alpha/c.jsonl")]),
        1
    );
    assert!(app.take_reconcile_request());
    assert!(app.card_removed(0));
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-beta/b.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
    assert_eq!(app.selected(), Some(0));
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-alpha/a.jsonl"]);

    // A generation published before the delete still holds the session.
    let mut stale = corpus.clone();
    stale[1].custom_title = Some("stale".to_string());
    let stale = republish(&root, &stale);
    assert!(app.swap_generation(stale));
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), vec!["/p/-tmp-alpha/a.jsonl"]);
    app.set_query("");
    assert_eq!(listed_paths(&app).len(), 2, "out of the browse list too");

    let pruned = republish(&root, &corpus[..2]);
    assert!(app.swap_generation(pruned));
    assert!(app.overlays().0.is_empty());
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-beta/b.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
}

#[test]
fn renamed_rows_show_the_new_title_until_the_generation_has_it() {
    let sandbox = Sandbox::new();
    let corpus = corpus();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let path = PathBuf::from("/p/-tmp-alpha/c.jsonl");
    assert_eq!(app.selected_path(), Some(path.clone()));

    // What `rename_session` returns: the session re-read from its file.
    let mut renamed = corpus[2].clone();
    renamed.custom_title = Some("fresh".to_string());
    app.apply_renamed(&path, renamed).unwrap();
    assert!(app.take_reconcile_request());
    let row = app.filtered()[0];
    assert_eq!(
        app.conversations()[row].custom_title.as_deref(),
        Some("fresh")
    );
    assert_eq!(app.selected_path(), Some(path.clone()));
    assert!(app.search_index_generation().is_some(), "the index stays");

    // A generation published before the rename keeps the overlay.
    let mut stale = corpus.clone();
    stale[0].custom_title = Some("stale".to_string());
    let stale = republish(&root, &stale);
    assert!(app.swap_generation(stale));
    let row = app.filtered()[0];
    assert_eq!(
        app.conversations()[row].custom_title.as_deref(),
        Some("fresh")
    );

    let mut retitled = corpus.clone();
    retitled[2].custom_title = Some("fresh".to_string());
    let retitled = republish(&root, &retitled);
    assert!(app.swap_generation(retitled));
    assert!(app.overlays().1.is_empty());
    let row = app.filtered()[0];
    assert_eq!(
        app.conversations()[row].custom_title.as_deref(),
        Some("fresh")
    );
}

#[test]
fn deleting_a_card_beyond_the_first_rows_survives_the_full_install() {
    let sandbox = Sandbox::new();
    let corpus: Vec<Conversation> = (0..(SNAPSHOT_ROWS as i64 + 5))
        .map(|i| conversation(&format!("/p/-tmp-alpha/{i:04}.jsonl"), (i + 10) * HOUR_MS))
        .collect();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert_eq!(app.conversations().len(), SNAPSHOT_ROWS);

    // The oldest card is not a row yet: no row matches, the delete counts.
    let gone = PathBuf::from("/p/-tmp-alpha/0000.jsonl");
    assert_eq!(app.remove_paths(std::slice::from_ref(&gone)), 0);
    assert!(app.take_reconcile_request());
    assert!(app.open_search_index());
    app.set_query("");
    let listed = listed_paths(&app);
    assert_eq!(listed.len(), SNAPSHOT_ROWS + 4);
    assert!(!listed.contains(&gone.display().to_string()));
}

#[test]
fn deleting_before_the_first_generation_asks_for_a_reconcile_and_is_remembered() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    let gone = PathBuf::from("/p/-tmp-alpha/c.jsonl");
    assert_eq!(app.remove_paths(std::slice::from_ref(&gone)), 0);
    assert!(app.take_reconcile_request());
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    assert_eq!(
        listed_paths(&app),
        vec!["/p/-tmp-beta/b.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );
}

#[test]
fn uuid_query_finds_cards_beyond_the_first_rows() {
    let sandbox = Sandbox::new();
    let uuid = "abcdef01-2345-4678-9abc-def012345678";
    let mut corpus: Vec<Conversation> = (0..(SNAPSHOT_ROWS as i64 + 5))
        .map(|i| conversation(&format!("/p/-tmp-alpha/{i:04}.jsonl"), (i + 10) * HOUR_MS))
        .collect();
    corpus.push(conversation(&format!("/p/-tmp-beta/{uuid}.jsonl"), HOUR_MS));
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert_eq!(app.conversations().len(), SNAPSHOT_ROWS);

    app.set_query(uuid.to_uppercase());
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Found {
            loaded_from_disk: false,
            ..
        })
    ));
    assert_eq!(
        listed_paths(&app),
        vec![format!("/p/-tmp-beta/{uuid}.jsonl")]
    );
}

#[test]
fn semantic_results_map_to_cards_by_path() {
    let sandbox = Sandbox::new();
    let alpha_dir = sandbox.path().join("alpha");
    std::fs::create_dir_all(&alpha_dir).unwrap();
    let alpha = crate::env::workspace_project_dir_name(&alpha_dir);
    let published = vec![
        conversation(&format!("/p/{alpha}/a.jsonl"), HOUR_MS),
        conversation("/p/-tmp-beta/b.jsonl", 2 * HOUR_MS),
        conversation(&format!("/p/{alpha}/c.jsonl"), 3 * HOUR_MS),
    ];
    let root = publish(&published);
    let mut app = loading_app(&sandbox, Some(&alpha_dir), vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let mut loaded = published.clone();
    loaded.push(conversation(
        &format!("/p/{alpha}/unpublished.jsonl"),
        9 * HOUR_MS,
    ));
    let cards = crate::store::SemanticCards::from_conversations("loaded", loaded);
    let docs = cards::semantic_docs(app.snapshot.as_ref().unwrap(), cards.conversations());
    assert_eq!(docs, vec![Some(2), Some(1), Some(0), None]);
    app.semantic_corpus.loaded = Some(Arc::new(cards));
    app.semantic_corpus.docs = Some((app.generation_id().unwrap(), Arc::new(docs)));

    // In scope: the alpha conversations that have a card.
    assert_eq!(*app.semantic_scope_indices(), vec![0, 2]);
    let (rows, _) = app.semantic_rows(vec![2, 3, 0], HashMap::new());
    let paths: Vec<_> = rows
        .iter()
        .map(|&row| app.conversations()[row].path.display().to_string())
        .collect();
    assert_eq!(
        paths,
        vec![format!("/p/{alpha}/c.jsonl"), format!("/p/{alpha}/a.jsonl")]
    );

    // A deleted card leaves the semantic scope too.
    app.remove_paths(&[PathBuf::from(format!("/p/{alpha}/c.jsonl"))]);
    assert_eq!(*app.semantic_scope_indices(), vec![0]);
}

#[test]
fn semantic_corpus_is_read_from_the_generation_and_follows_swaps() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[0].semantic_turns = vec!["alpha turn".to_string()].into();
    corpus[1].semantic_turns = vec!["beta turn".to_string()].into();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    app.remove_paths(&[PathBuf::from("/p/-tmp-beta/b.jsonl")]);

    assert!(!app.ensure_semantic_corpus());
    wait_for_search(&mut app);
    let first = app.semantic_corpus.loaded.clone().unwrap();
    let turns = |cards: &crate::store::SemanticCards, name: &str| {
        cards
            .conversations()
            .iter()
            .find(|card| card.path.ends_with(name))
            .map(|card| card.semantic_turns.clone())
            .unwrap()
    };
    assert_eq!(&*turns(&first, "a.jsonl"), ["alpha turn".to_string()]);
    assert!(Arc::ptr_eq(
        &app.semantic_conversations_loaded().unwrap(),
        first.conversations()
    ));
    let scope = app.semantic_scope_indices();
    let scoped: Vec<_> = scope
        .iter()
        .map(|&index| first.conversations()[index].path.display().to_string())
        .collect();
    assert_eq!(
        scoped,
        vec!["/p/-tmp-alpha/c.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );

    corpus[2].semantic_turns = vec!["gamma turn".to_string()].into();
    assert!(app.swap_generation(republish(&root, &corpus)));
    assert!(
        app.ensure_semantic_corpus(),
        "the older corpus serves meanwhile"
    );
    wait_for_search(&mut app);
    let second = app.semantic_corpus.loaded.clone().unwrap();
    assert_ne!(second.generation_name(), first.generation_name());
    assert_eq!(&*turns(&second, "c.jsonl"), ["gamma turn".to_string()]);
    assert!(Arc::ptr_eq(
        &turns(&second, "a.jsonl"),
        &turns(&first, "a.jsonl")
    ));
}

/// Fork: the `semantic-corpus` thread wakes the poll once the cards are
/// read, so a GUI that polls only when woken receives them.
#[test]
fn the_semantic_corpus_read_wakes_the_poll() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let waker: Waker = {
        let woken = woken.clone();
        Arc::new(move || {
            woken.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let mut app = Browser::new_loading(sandbox.options().with_waker(waker));
    show(&mut app, &root, TimeFilter::default());
    let before = woken.load(std::sync::atomic::Ordering::SeqCst);

    assert!(!app.ensure_semantic_corpus());
    let deadline = Instant::now() + Duration::from_secs(10);
    while woken.load(std::sync::atomic::Ordering::SeqCst) == before {
        assert!(Instant::now() < deadline, "the read never woke the poll");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(app.poll(), "the woken poll receives the cards");
    assert!(app.semantic_corpus.loaded.is_some());
    assert!(!app.has_search_work_in_flight());
}

/// Fork: cards read before the generation's index opened are sent to the
/// semantic worker again with that index, which quoted-only queries read.
#[test]
fn the_semantic_corpus_is_sent_again_with_its_generation_index() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(!app.ensure_semantic_corpus());
    wait_for_search(&mut app);
    let (_request_tx, request_rx, _response_tx) = connect_semantic_search_channels(&mut app);
    let sent_texts = |rx: &mpsc::Receiver<crate::semantic_worker::SemanticWorkerCommand>| {
        rx.try_iter()
            .filter_map(|command| match command {
                crate::semantic_worker::SemanticWorkerCommand::UpdateCorpus { texts, .. } => {
                    Some(texts.map(|index| index.cards().generation_id()))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    app.set_list_search_mode(ListSearchMode::Semantic);
    assert_eq!(sent_texts(&request_rx), vec![None], "no index yet");
    assert!(app.open_search_index());
    assert_eq!(
        sent_texts(&request_rx),
        vec![Some(app.generation_id().unwrap())]
    );
}

/// A read of the semantic data that plays the `semantic-corpus` thread: it
/// waits for `gate`, then answers with the cards of `snapshot`.
fn gated_semantic_read(
    snapshot: crate::store::Snapshot,
    gate: mpsc::Receiver<()>,
) -> (String, search_state::SemanticJob) {
    let id = snapshot.generation_id();
    let job = search_state::SemanticJob::spawn(None, move |_| {
        let _ = gate.recv();
        let cards = crate::store::SemanticCards::load(&snapshot, None).map(Arc::new);
        search_state::LoadedSemantic {
            docs: cards.as_ref().map(|cards| {
                let docs = (0..cards.conversations().len() as u32).map(Some).collect();
                (cards.generation_id().to_owned(), Arc::new(docs))
            }),
            cards,
        }
    })
    .unwrap();
    (id, job)
}

/// A generation swapped in while the semantic data of the one shown before
/// is read gets its own read, which ranks a session new since the first
/// read started; the first is given up.
#[test]
fn a_semantic_read_of_an_older_generation_gives_way_to_the_one_shown() {
    let sandbox = Sandbox::new();
    let published = corpus();
    let root = publish(&published);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let (_request_tx, request_rx, _response_tx) = connect_semantic_search_channels(&mut app);
    let (release, gate) = mpsc::channel();
    app.semantic_corpus.loading = Some(gated_semantic_read(app.snapshot.clone().unwrap(), gate));
    app.set_list_search_mode(ListSearchMode::Semantic);
    type_query(&mut app, "needle");

    let mut newer = published.clone();
    newer.push(conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS));
    assert!(app.swap_generation(republish(&root, &newer)));
    assert_eq!(app.generation_name(), Some("g000002"));
    assert_eq!(
        app.semantic_corpus
            .loading
            .as_ref()
            .map(|(id, _)| id.clone()),
        app.generation_id(),
        "the generation shown is read"
    );
    assert_eq!(
        app.semantic_activity_status_text().as_deref(),
        Some("sem loading conversations")
    );
    release.send(()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let scope = 'sent: loop {
        assert!(Instant::now() < deadline, "no scope was sent");
        app.poll();
        while let Ok(command) = request_rx.try_recv() {
            if let crate::semantic_worker::SemanticWorkerCommand::UpdateScope { indices, .. } =
                command
            {
                break 'sent indices;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // d is doc 0 of g000002.
    assert_eq!(*scope, vec![0, 1, 2, 3], "the new session is ranked");
    assert_eq!(
        app.semantic_corpus
            .loaded
            .as_ref()
            .map(|cards| cards.generation_name()),
        Some("g000002")
    );
    assert_eq!(app.semantic_activity_status_text(), None);
}

/// While the semantic data is read, a scope change (which clears the
/// pending status) still shows the read; dropping the browser gives the
/// read up and waits for its thread.
#[test]
fn a_semantic_read_shows_until_it_arrives_and_stops_with_the_browser() {
    let sandbox = Sandbox::new();
    let root = publish(&corpus());
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let job = {
        let ended = ended.clone();
        search_state::SemanticJob::spawn(None, move |cancel| {
            while !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            std::thread::sleep(Duration::from_millis(50));
            ended.store(true, std::sync::atomic::Ordering::Relaxed);
            search_state::LoadedSemantic {
                cards: None,
                docs: None,
            }
        })
        .unwrap()
    };
    app.semantic_corpus.loading = Some((app.generation_id().unwrap(), job));
    app.set_list_search_mode(ListSearchMode::Semantic);
    assert_eq!(
        app.semantic_activity_status_text().as_deref(),
        Some("sem loading conversations")
    );
    app.set_time_filter(TimeFilter {
        after: Some(chrono::Local::now() - chrono::Duration::days(1)),
        before: None,
    });
    assert_eq!(
        app.semantic_activity_status_text().as_deref(),
        Some("sem loading conversations"),
        "a re-scope keeps the read shown"
    );
    assert!(app.has_search_work_in_flight());
    drop(app);
    assert!(
        ended.load(std::sync::atomic::Ordering::Relaxed),
        "the browser waited for the given-up read"
    );
}

/// Removes the semantic segments of the generation `id` names (none is
/// mapped before a read opens them).
fn remove_semantic_segments(id: &str) {
    let mut removed = 0;
    for entry in std::fs::read_dir(id).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("sem-"))
        {
            std::fs::remove_file(path).unwrap();
            removed += 1;
        }
    }
    assert!(removed > 0, "the generation has semantic segments");
}

/// Fork: a browser that read the cards of g000001 (a has semantic turns),
/// then opened g000002, which drops b, adds d and has no semantic segments
/// (the open came before the cards: no docs are mapped to it yet); not in
/// semantic mode. Returns it with the cards read, the snapshot of g000002
/// and the store root.
fn older_cards_and_unreadable_semantic_data(
    sandbox: &Sandbox,
) -> (
    Browser,
    Arc<crate::store::SemanticCards>,
    crate::store::Snapshot,
    tempfile::TempDir,
) {
    let mut published = corpus();
    published[0].semantic_turns = vec!["alpha turn".to_string()].into();
    let root = publish(&published);
    let mut app = loading_app(sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    assert!(!app.ensure_semantic_corpus());
    wait_for_search(&mut app);
    let first = app.semantic_corpus.loaded.clone().unwrap();

    // g000002 drops b and adds d: docs d, c, a (g000001: c, b, a).
    let newer = vec![
        published[0].clone(),
        published[2].clone(),
        conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS),
    ];
    let snapshot = republish(&root, &newer);
    remove_semantic_segments(&snapshot.generation_id());
    let open = cards::GenerationOpen::build(snapshot.clone(), Some(app.corpus_snapshot()), None);
    app.install_generation(open);
    (app, first, snapshot, root)
}

/// The scopes sent to the semantic worker since the last call, and how
/// many queries.
fn sent_scopes_and_queries(
    rx: &mpsc::Receiver<crate::semantic_worker::SemanticWorkerCommand>,
) -> (Vec<Vec<usize>>, usize) {
    let mut scopes = Vec::new();
    let mut queries = 0;
    for command in rx.try_iter() {
        match command {
            crate::semantic_worker::SemanticWorkerCommand::UpdateScope { indices, .. } => {
                scopes.push(indices.to_vec());
            }
            crate::semantic_worker::SemanticWorkerCommand::Search { .. } => queries += 1,
            crate::semantic_worker::SemanticWorkerCommand::UpdateCorpus { .. } => {}
        }
    }
    (scopes, queries)
}

/// Fork: cards read before keep serving a generation whose semantic data
/// cannot be read, mapped to its docs on a `semantic-corpus` thread. The
/// read of that data fails on another one, before or after the mapping
/// sends the scope: the test waits for both, and the query runs once
/// either way.
#[test]
fn older_cards_serve_a_generation_whose_semantic_data_cannot_be_read() {
    let sandbox = Sandbox::new();
    let (mut app, first, _, _root) = older_cards_and_unreadable_semantic_data(&sandbox);
    let (_request_tx, request_rx, _response_tx) = connect_semantic_search_channels(&mut app);
    app.set_list_search_mode(ListSearchMode::Semantic);
    type_query(&mut app, "needle");

    let (mut scopes, mut queries) = (Vec::new(), 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while scopes.is_empty() || app.semantic_corpus.loading.is_some() {
        assert!(
            Instant::now() < deadline,
            "no scope was sent, or the read never answered"
        );
        app.poll();
        let (sent, ran) = sent_scopes_and_queries(&request_rx);
        scopes.extend(sent);
        queries += ran;
        std::thread::sleep(Duration::from_millis(5));
    }
    // c and a of the cards read before; b has no card any more.
    assert_eq!(scopes, vec![vec![0, 2]]);
    assert_eq!(queries, 1, "the query ran once");
    assert!(Arc::ptr_eq(
        app.semantic_corpus.loaded.as_ref().unwrap(),
        &first
    ));
    assert_eq!(app.semantic_corpus.failed, app.generation_id());
    assert_eq!(app.semantic_error(), None);
}

/// Fork: a read that fails once the cards read before serve the generation
/// shown changes nothing for the query they serve: it is not run again
/// (it was, when the mapping answered before the read), and the failure is
/// kept.
#[test]
fn a_read_failing_after_older_cards_serve_does_not_run_the_query_again() {
    let sandbox = Sandbox::new();
    let (mut app, first, snapshot, _root) = older_cards_and_unreadable_semantic_data(&sandbox);
    let (_request_tx, request_rx, _response_tx) = connect_semantic_search_channels(&mut app);
    let (release, gate) = mpsc::channel();
    app.semantic_corpus.loading = Some(gated_semantic_read(snapshot, gate));
    app.set_list_search_mode(ListSearchMode::Semantic);
    type_query(&mut app, "needle");

    let deadline = Instant::now() + Duration::from_secs(10);
    let sent = loop {
        assert!(Instant::now() < deadline, "no scope was sent");
        app.poll();
        let sent = sent_scopes_and_queries(&request_rx);
        if !sent.0.is_empty() {
            break sent;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(sent, (vec![vec![0, 2]], 1), "the older cards serve");
    assert_eq!(app.semantic_corpus.failed, None, "the read is held");

    release.send(()).unwrap();
    while app.semantic_corpus.loading.is_some() {
        assert!(Instant::now() < deadline, "the read never answered");
        app.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.semantic_corpus.failed, app.generation_id());
    assert_eq!(
        sent_scopes_and_queries(&request_rx),
        (Vec::new(), 0),
        "the query is not run again"
    );
    assert!(Arc::ptr_eq(
        app.semantic_corpus.loaded.as_ref().unwrap(),
        &first
    ));
    assert_eq!(app.semantic_error(), None);
}

/// Fork: a generation whose semantic data cannot be read fails semantic
/// search once (no read loop); entering semantic mode again retries.
#[test]
fn semantic_data_that_cannot_be_read_fails_until_semantic_mode_is_entered_again() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[0].semantic_turns = vec!["alpha turn".to_string()].into();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    remove_semantic_segments(&app.generation_id().unwrap());

    app.set_list_search_mode(ListSearchMode::Semantic);
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(app.semantic_error(), Some("semantic corpus unavailable"));
    assert_eq!(app.semantic_status_text().as_deref(), Some("sem failed"));
    assert!(!app.poll(), "nothing is read again");
    assert!(!app.has_search_work_in_flight());

    app.set_list_search_mode(ListSearchMode::Lexical);
    app.set_list_search_mode(ListSearchMode::Semantic);
    assert!(app.has_search_work_in_flight(), "read again");
    wait_for_search(&mut app);
    assert_eq!(app.semantic_error(), Some("semantic corpus unavailable"));
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
        }),
        user_only: false,
        corpus_version: 1,
    })
    .unwrap();
    let page = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!page.complete);
    assert_eq!(page.filtered.len(), search_state::TOP_PAGE);
    assert!(Arc::ptr_eq(page.index.as_ref().unwrap(), &index));
    let complete = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(complete.complete);
    assert_eq!(complete.generation, 3);
    assert_eq!(complete.filtered.len(), 120);
    assert_eq!(complete.filtered[..50], page.filtered[..]);
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
        user_only: false,
        evidence: HashMap::new(),
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

    // An answer from an index no longer in use (a newer generation was
    // swapped in) is dropped.
    let newer = republish(&root, &many_needles(121));
    assert!(app.swap_generation(newer));
    let generation = app.search_generation;
    tx.send(SearchResponse {
        generation,
        ..response(ranking[..3].to_vec(), true)
    })
    .unwrap();
    assert!(!app.poll());
    assert_eq!(app.filtered().len(), 120, "the rows kept for the query");
}

// --- the loader in store mode (fork) ---

/// Drive `loader` until it reports a load event.
fn poll_until_event(loader: &mut HistoryLoader, browser: &mut Browser) -> LoadEvent {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(event) = loader.poll(browser).event {
            return event;
        }
        assert!(Instant::now() < deadline, "loader did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Drive `loader` until `done` holds.
fn poll_until(
    loader: &mut HistoryLoader,
    browser: &mut Browser,
    what: &str,
    done: impl Fn(&Browser) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        loader.poll(browser);
        browser.poll();
        if done(browser) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
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

/// A loader and a browser after the initial load of `sandbox`'s sessions.
fn loaded_store(sandbox: &Sandbox) -> (HistoryLoader, Browser) {
    let mut loader = snapshot_loader(sandbox);
    let mut app = loading_app(sandbox, None, vec![]);
    if let Some(snapshot) = loader.open_snapshot() {
        app.show_snapshot(snapshot);
    }
    assert!(loader.start_initial_load());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(matches!(event, LoadEvent::Loaded { .. }), "{event:?}");
    (loader, app)
}

#[test]
fn the_first_pass_publishes_the_generation_the_list_shows() {
    let sandbox = Sandbox::new();
    let paths = write_sessions(&sandbox);
    let mut loader = snapshot_loader(&sandbox);
    assert!(loader.open_snapshot().is_none(), "no generation yet");
    assert!(loader.uses_store());

    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    assert!(loader.is_running());
    assert_eq!(app.count_text(), "Loading...");
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: 3,
                project_errors: 0
            }
        ),
        "{event:?}"
    );
    assert!(!loader.is_running());
    assert_eq!(listed_paths(&app), display(&paths));
    assert!(app.generation_open());
    assert_eq!(app.search_index_generation(), Some("g000001"));
    assert_eq!(
        loader.finish_snapshot_write(),
        Some(Publish::Published("g000001".into()))
    );
    let root = crate::store::default_root(&sandbox.env).unwrap();
    assert!(root.starts_with(sandbox.path()), "{root:?}");
    assert!(
        debug_log(&sandbox).contains("Reconcile: Outcome { publish: Published(\"g000001\")"),
        "upstream's reconcile line: {}",
        debug_log(&sandbox)
    );

    // Cards carry no text: queries run on the index.
    assert_eq!(&*app.conversations()[0].full_text, "");
    type_query(&mut app, "second");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), display(&paths[1..2]));
}

#[test]
fn a_start_shows_the_generation_then_swaps_in_what_the_pass_publishes() {
    let sandbox = Sandbox::new();
    let paths = write_sessions(&sandbox);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    let sources = crate::history::sources::Registry::from_env(&sandbox.env);
    crate::store::reconcile::reconcile(&root, &sources).unwrap();
    let d = write_session(&sandbox, "-tmp-beta", "d.jsonl", 4, "fourth prompt");

    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    app.show_snapshot(loader.open_snapshot().expect("the published generation"));
    assert_eq!(listed_paths(&app), display(&paths), "the first frame");
    app.select_next();
    assert!(loader.start_initial_load());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: 3,
                ..
            }
        ),
        "the generation shown at start: {event:?}"
    );

    // A delta generation, possibly merged into another one.
    let Some(Publish::Published(published)) = loader.finish_snapshot_write() else {
        panic!("the pass publishes the new session");
    };
    poll_until(&mut loader, &mut app, "the swap", |app| {
        app.generation_name() == Some(published.as_str())
    });
    assert_eq!(app.search_index_generation(), Some(published.as_str()));
    let mut expected = vec![d];
    expected.extend(paths.iter().cloned());
    assert_eq!(listed_paths(&app), display(&expected));
    assert_eq!(app.selected_path(), Some(paths[1].clone()), "kept");
    assert!(!loader.is_running(), "a background swap is no load");
}

#[test]
fn a_refresh_runs_a_pass_and_reports_the_generation_sizes() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let (mut loader, mut app) = loaded_store(&sandbox);
    loader.finish_snapshot_write();
    poll_until(&mut loader, &mut app, "the pass to settle", |app| {
        app.readiness().reconcile == ReconcileState::Done
    });

    write_session(&sandbox, "-tmp-beta", "d.jsonl", 4, "fourth prompt");
    assert!(loader.start_refresh());
    assert!(loader.is_refreshing());
    assert!(!loader.start_refresh(), "one refresh at a time");
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(
            event,
            LoadEvent::Refreshed {
                old: 3,
                new: 4,
                project_errors: 0
            }
        ),
        "{event:?}"
    );
    assert_eq!(
        event.status_message().as_deref(),
        Some("Refreshed sessions: 3 -> 4")
    );
    assert!(!loader.is_refreshing());
    assert_eq!(listed_paths(&app).len(), 4);

    // Unchanged session files write nothing.
    assert!(loader.start_refresh());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(event, LoadEvent::Refreshed { old: 4, new: 4, .. }),
        "{event:?}"
    );
    assert_eq!(loader.finish_snapshot_write(), Some(Publish::Unchanged));
}

#[test]
fn the_time_filter_rescopes_the_cards_shown() {
    let sandbox = Sandbox::new();
    let paths = write_sessions(&sandbox);
    let (mut loader, mut app) = loaded_store(&sandbox);
    // Half an hour before the newest session (c): b and a are older.
    let newest = app.conversation_at(&paths[0]).unwrap().timestamp;
    let after = newest - chrono::Duration::minutes(30);
    loader.set_time_filter(TimeFilter {
        after: Some(after),
        before: None,
    });
    assert!(loader.start_refresh());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(event, LoadEvent::Refreshed { old: 3, new: 3, .. }),
        "{event:?}"
    );
    assert_eq!(listed_paths(&app), display(&paths[..1]));
    assert_eq!(app.generation_len(), Some(3), "the generation is unchanged");
}

#[test]
fn edits_ask_for_a_pass_that_drops_their_overlays() {
    let sandbox = Sandbox::new();
    let paths = write_sessions(&sandbox);
    let (mut loader, mut app) = loaded_store(&sandbox);
    loader.finish_snapshot_write();
    poll_until(&mut loader, &mut app, "the pass to settle", |app| {
        app.readiness().reconcile == ReconcileState::Done
    });

    // Delete b (the file and the row).
    std::fs::remove_file(&paths[1]).unwrap();
    assert_eq!(app.remove_paths(&paths[1..2]), 1);
    assert_eq!(
        listed_paths(&app),
        display(&[paths[0].clone(), paths[2].clone()])
    );
    // Rename c through the browser (writes its file).
    app.rename_conversation(&paths[0], "fresh").unwrap();
    assert_eq!(
        app.selected_conversation().unwrap().custom_title.as_deref(),
        Some("fresh")
    );
    assert!(!app.overlays().0.is_empty() && !app.overlays().1.is_empty());

    poll_until(
        &mut loader,
        &mut app,
        "a generation with the edits",
        |app| app.overlays().0.is_empty() && app.overlays().1.is_empty(),
    );
    assert_eq!(app.generation_len(), Some(2));
    assert_eq!(
        listed_paths(&app),
        display(&[paths[0].clone(), paths[2].clone()])
    );
    assert_eq!(
        app.selected_conversation().unwrap().custom_title.as_deref(),
        Some("fresh"),
        "the card holds the title"
    );
    assert!(!loader.is_running(), "an edit's pass is no load");
}

/// Fork: a workspace whose Pi session root differs reads and writes another
/// store root (`roots-<hash>`); after `reconfigure` the refresh's pass
/// publishes there and its generation is shown.
#[test]
fn a_new_store_root_is_reconciled_and_shown_after_reconfigure() {
    let sandbox = Sandbox::new();
    let paths = write_sessions(&sandbox);
    let (mut loader, mut app) = loaded_store(&sandbox);
    loader.finish_snapshot_write();
    let first_root = crate::store::default_root(&sandbox.env).unwrap();

    let elsewhere = sandbox.path().join("pi-elsewhere");
    let env = Arc::new(HistoryEnv::sandbox(sandbox.path()).with_pi_session_dir(elsewhere.clone()));
    let second_root = crate::store::default_root(&env).unwrap();
    assert_ne!(first_root, second_root);
    loader.reconfigure(env, LoaderOptions::default().with_snapshot(true));
    assert!(loader.start_refresh());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(event, LoadEvent::Refreshed { new: 3, .. }),
        "{event:?}"
    );
    assert_eq!(listed_paths(&app), display(&paths));
    let shown = app.generation_name().map(str::to_owned);
    assert_eq!(
        crate::store::Generation::current_name(&second_root),
        shown,
        "the generation of the new store is shown"
    );
    // Settled: no further open is started.
    for _ in 0..5 {
        loader.poll(&mut app);
        assert!(!loader.is_preparing());
    }
}

#[test]
fn a_first_pass_over_no_session_reports_no_history() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.projects_root()).unwrap();
    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.open_snapshot().is_none());
    assert!(loader.start_initial_load());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(&event, LoadEvent::NoHistory(error) if error.to_string().contains("selected scope")),
        "{event:?}"
    );
    assert!(app.readiness().list, "an empty, ready list");
    assert!(app.filtered().is_empty());
}

#[test]
fn without_the_store_option_the_history_loads_in_memory() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    assert!(loader.open_snapshot().is_none());
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    assert!(!loader.uses_store());
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
    assert!(app.conversations()[0].full_text.contains("third prompt"));
    assert_eq!(loader.finish_snapshot_write(), None);
    assert!(!root.join("CURRENT").exists());
}

#[test]
fn reconfiguring_keeps_a_running_pass() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let mut loader = snapshot_loader(&sandbox);
    let (gate, _) = loader.gate_next_snapshot_write_for_test();
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.start_initial_load());
    loader.reconfigure(
        sandbox.env.clone(),
        LoaderOptions::new(false).with_snapshot(true),
    );
    assert!(!loader.options().show_last);
    drop(gate);
    assert_eq!(
        loader.finish_snapshot_write(),
        Some(Publish::Published("g000001".into()))
    );
    let event = poll_until_event(&mut loader, &mut app);
    assert!(matches!(event, LoadEvent::Loaded { .. }), "{event:?}");
    assert_eq!(&*app.conversations()[0].preview, "third prompt");
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
    loader.poll(&mut app);
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
    wait_for_log(&sandbox, "publish: Busy", 1);
    assert!(loader.poll(&mut app).event.is_none(), "still loading");
    // Long enough for a few retries.
    std::thread::sleep(loader::BUSY_RETRY * 2);
    drop(held);
    assert_eq!(
        loader.finish_snapshot_write(),
        Some(Publish::Published("g000001".into()))
    );
    let event = poll_until_event(&mut loader, &mut app);
    assert!(matches!(event, LoadEvent::Loaded { .. }), "{event:?}");
    assert_eq!(app.search_index_generation(), Some("g000001"));
    assert_eq!(
        debug_log(&sandbox).matches("publish: Busy").count(),
        1,
        "{}",
        debug_log(&sandbox)
    );

    let held = PublishLock::try_acquire(&root).unwrap().unwrap();
    write_session(&sandbox, "-tmp-beta", "d.jsonl", 4, "fourth prompt");
    let (mut loader, mut app) = {
        let mut loader = snapshot_loader(&sandbox);
        let mut app = loading_app(&sandbox, None, vec![]);
        app.show_snapshot(loader.open_snapshot().unwrap());
        assert!(loader.start_initial_load());
        (loader, app)
    };
    let event = poll_until_event(&mut loader, &mut app);
    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: 3,
                ..
            }
        ),
        "the generation shown does not wait for the lock: {event:?}"
    );
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

/// Upstream 04c714b: while a pass may run, the progress another instance's
/// pass reports in `PROGRESS` (upstream: another process) reaches the
/// browser through the `store-watch` thread; once no pass runs, the shown
/// indexing leaves a completion notice.
#[test]
fn the_progress_of_a_pass_indexing_the_store_shows_until_no_pass_runs() {
    use crate::store::progress::{Indexing, MIN_FILES, Reporter};
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    // Another instance holds the store and indexes it: the loader's pass
    // waits for the lock meanwhile.
    let held = PublishLock::try_acquire(&root).unwrap().unwrap();
    let total = MIN_FILES + 10;
    let reporter = Reporter::start(&root, total).unwrap();
    reporter
        .counter()
        .fetch_add(3, std::sync::atomic::Ordering::Relaxed);
    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    assert_eq!(app.index_progress(), None);
    assert!(loader.start_initial_load());
    poll_until(&mut loader, &mut app, "the progress", |app| {
        app.index_progress() == Some(Indexing { done: 3, total })
    });
    assert_eq!(
        app.indexing_status_text().as_deref(),
        Some("Indexing sessions\u{2026} 3 / 60")
    );
    assert_eq!(app.take_index_notice(), None);

    // That pass ends (its file is removed), then the loader's own runs.
    drop(reporter);
    drop(held);
    let event = poll_until_event(&mut loader, &mut app);
    assert!(matches!(event, LoadEvent::Loaded { .. }), "{event:?}");
    loader.finish_snapshot_write();
    poll_until(&mut loader, &mut app, "the pass to end", |app| {
        app.readiness().reconcile == ReconcileState::Done
    });
    assert_eq!(app.indexing_status_text(), None);
    let notice = app.take_index_notice();
    assert_eq!(notice, Some(IndexNotice::Indexed { total }));
    assert!(!root.join("PROGRESS").exists());
}

/// Upstream 04c714b: a failed pass that no load event reports (here the
/// initial load's pass over a store whose generation is shown) leaves a
/// failure notice and its message in `PROGRESS`; a refresh's failed pass is
/// reported by `RefreshFailed` instead, without a notice.
#[test]
fn a_failed_pass_leaves_a_failure_notice_unless_an_event_reports_it() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    let sources = crate::history::sources::Registry::from_env(&sandbox.env);
    crate::store::reconcile::reconcile(&root, &sources).unwrap();
    // The next pass cannot open the store's lock file: it fails.
    std::fs::remove_file(root.join("LOCK")).unwrap();
    std::fs::create_dir(root.join("LOCK")).unwrap();

    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    app.show_snapshot(loader.open_snapshot().unwrap());
    assert!(loader.start_initial_load());
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
    let mut notice = app.take_index_notice();
    if notice.is_none() {
        loader.finish_snapshot_write();
        poll_until(&mut loader, &mut app, "the failed pass", |app| {
            app.readiness().reconcile == ReconcileState::Done
        });
        notice = app.take_index_notice();
    }
    let Some(IndexNotice::Failed(message)) = &notice else {
        panic!("a failure notice: {notice:?}");
    };
    assert!(
        !message.is_empty() && !message.contains('\n'),
        "{message:?}"
    );
    assert_eq!(
        notice.as_ref().unwrap().status_message(),
        format!("Indexing failed: {message}")
    );
    assert_eq!(
        crate::store::progress::read(&root),
        Some(crate::store::progress::Status::Failed(message.clone())),
        "upstream's record for the instance that started the pass"
    );

    assert!(loader.start_refresh());
    let event = poll_until_event(&mut loader, &mut app);
    assert!(matches!(event, LoadEvent::RefreshFailed(_)), "{event:?}");
    assert_eq!(app.take_index_notice(), None, "the event reports it");
}

/// Upstream 04c714b: when the first pass fails and no card is listed, the
/// initial load fails with `indexing sessions failed: …` (upstream exited
/// with it) instead of reporting no history.
#[test]
fn a_first_pass_that_fails_with_no_card_fails_the_initial_load() {
    let sandbox = Sandbox::new();
    write_sessions(&sandbox);
    let root = crate::store::default_root(&sandbox.env).unwrap();
    std::fs::create_dir_all(root.join("LOCK")).unwrap();
    let mut loader = snapshot_loader(&sandbox);
    let mut app = loading_app(&sandbox, None, vec![]);
    assert!(loader.open_snapshot().is_none());
    assert!(loader.start_initial_load());
    let event = poll_until_event(&mut loader, &mut app);
    let LoadEvent::LoadFailed(error) = &event else {
        panic!("{event:?}");
    };
    assert!(
        error
            .to_string()
            .starts_with("I/O error: indexing sessions failed: "),
        "{error}"
    );
    assert!(app.readiness().list, "an empty, ready list");
    assert_eq!(app.take_index_notice(), None, "the event reports it");

    // A retry that fails again shows the same text.
    assert!(loader.start_refresh());
    let event = poll_until_event(&mut loader, &mut app);
    let LoadEvent::RefreshFailed(error) = &event else {
        panic!("{event:?}");
    };
    assert!(
        error
            .to_string()
            .starts_with("I/O error: indexing sessions failed: "),
        "{error}"
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

#[test]
fn user_only_searches_what_the_person_typed_and_keeps_the_query() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[0].full_text = "the walrus assigns, the walrus returns".into();
    corpus[1].full_text = "how does the walrus work".into();
    corpus[1].own_text = "how does the walrus work".into();
    corpus[2].own_text = "nothing relevant".into();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    type_query(&mut app, "walrus");
    wait_for_search(&mut app);
    let both = ["/p/-tmp-alpha/a.jsonl", "/p/-tmp-beta/b.jsonl"];
    assert_eq!(listed_paths(&app), both);
    // Upstream pressed Down; the GUI's toggle is `toggle_user_only`.
    app.select_next();

    app.toggle_user_only();
    wait_for_search(&mut app);
    assert!(app.user_only());
    assert_eq!(app.query(), "walrus");
    assert_eq!(listed_paths(&app), ["/p/-tmp-beta/b.jsonl"]);
    let evidence = app.lexical_evidence(app.filtered()[0]).unwrap();
    assert_eq!(evidence.text, "how does the walrus work");

    app.toggle_user_only();
    wait_for_search(&mut app);
    assert!(!app.user_only());
    assert_eq!(listed_paths(&app), both);
    assert_eq!(app.selected(), Some(1));
}

/// Fork: rows the worker left without evidence get it from the own text in
/// user-only mode ([`Browser::fill_visible_evidence`]).
#[test]
fn visible_rows_get_evidence_from_own_text_in_user_only_mode() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[1].full_text = "assistant said walrus first, then the person asked".into();
    corpus[1].own_text = "a question about the walrus".into();
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    app.toggle_user_only();
    type_query(&mut app, "walrus");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), ["/p/-tmp-beta/b.jsonl"]);
    let row = app.filtered()[0];
    app.lexical_evidence.clear();
    app.fill_visible_evidence(0..1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.lexical_evidence(row).is_none() {
        assert!(Instant::now() < deadline, "no evidence arrived");
        if !app.poll() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(
        app.lexical_evidence(row).unwrap().text,
        "a question about the walrus"
    );
}

#[test]
fn hidden_kinds_toggle_in_and_out_of_the_list_and_its_search() {
    let sandbox = Sandbox::new();
    let mut corpus = corpus();
    corpus[0].full_text = "needle".into();
    let mut exec = conversation("/p/-tmp-alpha/exec.jsonl", 4 * HOUR_MS);
    exec.provenance.automated = true;
    exec.full_text = "needle".into();
    let mut child = conversation("/p/-tmp-beta/child.jsonl", 5 * HOUR_MS);
    child.provenance.child = true;
    corpus.extend([exec, child]);
    let root = publish(&corpus);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    let shown = [
        "/p/-tmp-alpha/c.jsonl",
        "/p/-tmp-beta/b.jsonl",
        "/p/-tmp-alpha/a.jsonl",
    ];
    let all = [
        "/p/-tmp-beta/child.jsonl",
        "/p/-tmp-alpha/exec.jsonl",
        "/p/-tmp-alpha/c.jsonl",
        "/p/-tmp-beta/b.jsonl",
        "/p/-tmp-alpha/a.jsonl",
    ];
    // Fork: the first cards follow the toggle too, before the index opens.
    assert_eq!(listed_paths(&app), shown);
    app.toggle_all_kinds();
    assert_eq!(listed_paths(&app), all);
    app.toggle_all_kinds();
    assert_eq!(listed_paths(&app), shown);

    assert!(app.open_search_index());
    assert_eq!(listed_paths(&app), shown);
    app.select_next();

    app.toggle_all_kinds();
    assert!(app.all_kinds());
    assert_eq!(listed_paths(&app), all);
    // Fork: the selected session stays selected (upstream's anchor).
    assert_eq!(
        app.selected_path(),
        Some(PathBuf::from("/p/-tmp-beta/b.jsonl"))
    );

    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        ["/p/-tmp-alpha/exec.jsonl", "/p/-tmp-alpha/a.jsonl"]
    );

    app.toggle_all_kinds();
    wait_for_search(&mut app);
    assert!(!app.all_kinds());
    assert_eq!(listed_paths(&app), ["/p/-tmp-alpha/a.jsonl"]);

    app.escape();
    assert_eq!(listed_paths(&app), shown);
}

/// Upstream 982aea9: `[tui] exclude_paths` hides cards by their recorded
/// cwd from the first cards, the browse list, lexical queries and the
/// semantic scope, for every source; a full session id still opens one.
/// Fork: the prefixes change at runtime ([`Browser::set_excluded_paths`]).
#[test]
fn exclude_paths_hide_cards_by_cwd_in_list_search_and_semantic_scope() {
    use crate::history::model::SourceKind;
    let sandbox = Sandbox::new();
    let hidden_uuid = "6dd62dc0-8b64-4f5e-a651-5bd95216478e";
    let card = |path: &str, at: i64, source: SourceKind, cwd: Option<&str>| {
        let mut card = conversation(path, at * HOUR_MS);
        card.source = source;
        card.cwd = cwd.map(PathBuf::from);
        card.project_path = cwd.map(PathBuf::from);
        card.full_text = "needle".into();
        card.semantic_turns = vec!["needle turn".to_string()].into();
        card
    };
    let mut corpus = vec![
        card(
            &format!("/p/-tmp-eval/{hidden_uuid}.jsonl"),
            1,
            SourceKind::Claude,
            Some("/tmp/eval-1"),
        ),
        card(
            "/s/pi.jsonl",
            2,
            SourceKind::Pi,
            Some("/private/tmp/eval-2"),
        ),
        card("/s/omp.jsonl", 3, SourceKind::Omp, Some("/tmp/eval-3")),
        card("/s/codex.jsonl", 4, SourceKind::Codex, Some("/tmp/eval-4")),
        card("/s/tmpfoo.jsonl", 5, SourceKind::Codex, Some("/tmpfoo/x")),
        card("/s/bare.jsonl", 6, SourceKind::Pi, Some("/tmp")),
        card("/p/-tmp-nocwd/n.jsonl", 7, SourceKind::Claude, None),
        card(
            "/p/-work-gone/g.jsonl",
            8,
            SourceKind::Claude,
            Some("/work"),
        ),
    ];
    // Without a recorded cwd the decoded project path is not consulted.
    corpus[6].project_path = Some(PathBuf::from("/tmp/nocwd"));
    let root = publish(&corpus);
    let mut app = Browser::new_loading(
        sandbox
            .options()
            .with_exclude_projects(vec!["gone".to_string()])
            .with_exclude_paths(vec!["/tmp/".to_string(), "/private/tmp/".to_string()]),
    );
    show(&mut app, &root, TimeFilter::default());
    let visible = ["/p/-tmp-nocwd/n.jsonl", "/s/bare.jsonl", "/s/tmpfoo.jsonl"];
    assert_eq!(listed_paths(&app), visible);
    assert!(
        !app.generation_is_empty(),
        "exclusions are not an empty history"
    );

    assert!(app.open_search_index());
    assert_eq!(listed_paths(&app), visible);
    type_query(&mut app, "needle");
    wait_for_search(&mut app);
    assert_eq!(listed_paths(&app), visible);

    assert!(!app.ensure_semantic_corpus());
    wait_for_search(&mut app);
    let loaded = app.semantic_corpus.loaded.clone().unwrap();
    let mut scoped: Vec<_> = app
        .semantic_scope_indices()
        .iter()
        .map(|&index| loaded.conversations()[index].path.display().to_string())
        .collect();
    scoped.sort();
    assert_eq!(scoped, visible);

    // A full session id still opens an excluded session.
    app.set_query(hidden_uuid);
    wait_for_search(&mut app);
    assert_eq!(
        listed_paths(&app),
        [format!("/p/-tmp-eval/{hidden_uuid}.jsonl")]
    );
    app.escape();
    assert_eq!(listed_paths(&app), visible);

    // Fork: changed at runtime, the prefixes re-scope the cards.
    app.set_excluded_paths(vec!["/tmp/eval-".to_string()]);
    assert_eq!(app.excluded_paths(), ["/tmp/eval-"]);
    assert_eq!(
        listed_paths(&app),
        [
            "/p/-tmp-nocwd/n.jsonl",
            "/s/bare.jsonl",
            "/s/tmpfoo.jsonl",
            "/s/pi.jsonl"
        ]
    );
    app.set_excluded_paths(Vec::new());
    assert_eq!(listed_paths(&app).len(), 7);
}
