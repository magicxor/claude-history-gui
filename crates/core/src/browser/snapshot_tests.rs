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
    assert_eq!(card.source, crate::history::Source::Claude);
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
    here.source = crate::history::Source::Pi;
    here.project_path = Some(current.clone());
    let mut elsewhere = conversation("/pi/sessions/elsewhere.jsonl", 2 * HOUR_MS);
    elsewhere.source = crate::history::Source::Pi;
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
    corpus[0].source = crate::history::Source::Pi;
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
    let loaded: CorpusSnapshot = Arc::new(loaded.into_iter().map(Arc::new).collect());
    let docs = cards::semantic_docs(app.snapshot.as_ref().unwrap(), &loaded);
    assert_eq!(docs, vec![Some(2), Some(1), Some(0), None]);
    app.semantic_corpus = search_state::SemanticCorpus::Loaded {
        conversations: loaded,
        docs: Some((app.generation_id().unwrap(), Arc::new(docs))),
        mapping: None,
    };

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

/// A semantic corpus load that plays the `semantic-corpus` thread: it
/// waits for `gate`, then answers with `corpus` mapped to the docs of
/// `shown` (the generation shown when it started).
fn gated_semantic_load(
    shown: crate::store::Snapshot,
    corpus: CorpusSnapshot,
    gate: mpsc::Receiver<()>,
) -> search_state::SemanticCorpus {
    let job = search_state::SemanticJob::spawn(None, move |_| {
        let _ = gate.recv();
        search_state::LoadedSemantic {
            docs: Some((
                shown.generation_name().to_owned(),
                Arc::new(cards::semantic_docs(&shown, &corpus)),
            )),
            conversations: corpus,
        }
    })
    .unwrap();
    search_state::SemanticCorpus::Loading(job)
}

/// Review fix: a generation swapped in while the semantic corpus loads
/// (its open could not map a corpus not loaded yet) gets the corpus mapped
/// to its docs before the semantic worker is sent a scope, so a session new
/// since the load started is ranked.
#[test]
fn a_semantic_corpus_loaded_under_an_older_generation_ranks_the_one_shown() {
    let sandbox = Sandbox::new();
    let published = corpus();
    let root = publish(&published);
    let mut app = loading_app(&sandbox, None, vec![]);
    show(&mut app, &root, TimeFilter::default());
    assert!(app.open_search_index());
    let (_request_tx, request_rx, _response_tx) = connect_semantic_search_channels(&mut app);
    let added = conversation("/p/-tmp-beta/d.jsonl", 4 * HOUR_MS);
    let mut on_disk = published.clone();
    on_disk.push(added.clone());
    let on_disk: CorpusSnapshot = Arc::new(on_disk.into_iter().map(Arc::new).collect());
    let (release, gate) = mpsc::channel();
    app.semantic_corpus = gated_semantic_load(app.snapshot.clone().unwrap(), on_disk, gate);
    app.set_list_search_mode(ListSearchMode::Semantic);
    type_query(&mut app, "needle");

    let mut newer = published.clone();
    newer.push(added);
    assert!(app.swap_generation(republish(&root, &newer)));
    assert_eq!(app.generation_name(), Some("g000002"));
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
    // d is doc 0 of g000002; with docs of g000001 it had none.
    assert_eq!(*scope, vec![0, 1, 2, 3], "the new session is ranked");
    assert_eq!(*app.semantic_scope_indices(), vec![0, 1, 2, 3]);
    assert_eq!(app.semantic_activity_status_text(), None);
}

/// Review fix: while the semantic corpus loads, a scope change (which
/// clears the pending status) still shows the load; dropping the browser
/// gives the load up and waits for its thread.
#[test]
fn a_semantic_corpus_load_shows_until_it_arrives_and_stops_with_the_browser() {
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
                conversations: Arc::default(),
                docs: None,
            }
        })
        .unwrap()
    };
    app.semantic_corpus = search_state::SemanticCorpus::Loading(job);
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
        "a re-scope keeps the load shown"
    );
    assert!(app.has_search_work_in_flight());
    drop(app);
    assert!(
        ended.load(std::sync::atomic::Ordering::Relaxed),
        "the browser waited for the given-up load"
    );
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
    let sources = crate::store::reconcile::Sources::from_env(&sandbox.env);
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
