//! The list's sort order (fork): relevance or date, in lexical mode only.

use super::test_helpers::*;
use super::*;
use chrono::{Local, TimeZone};

/// Conversations dated 2024-01-`day` whose text is `text`.
fn dated(name: &str, text: &str, day: u32) -> Conversation {
    let mut conversation = plain_conversation(PathBuf::from(format!("/p/-tmp-proj/{name}.jsonl")));
    conversation.timestamp = Local.with_ymd_and_hms(2024, 1, day, 12, 0, 0).unwrap();
    conversation.preview = text.to_string().into();
    conversation.full_text = text.to_string().into();
    conversation.project_name = Some("proj".to_string());
    conversation
}

/// Newest first (the corpus order of a loaded list): `d` (no match), `b`,
/// `c`, `a`; `a` matches the query most often, `b` least.
fn corpus() -> Vec<Conversation> {
    vec![
        dated("d", "unrelated words", 4),
        dated("b", "needle", 3),
        dated("c", "needle and needle again", 2),
        dated("a", "needle needle needle needle needle", 1),
    ]
}

fn names(app: &Browser) -> Vec<String> {
    app.filtered()
        .iter()
        .map(|&index| {
            let path = &app.conversations()[index].path;
            path.file_stem().unwrap().to_string_lossy().into_owned()
        })
        .collect()
}

fn search(app: &mut Browser, query: &str) {
    app.set_query(query);
    wait_for_search(app);
}

#[test]
fn the_default_order_is_relevance() {
    let sandbox = Sandbox::new();
    let app = Browser::new(corpus(), sandbox.options());
    assert_eq!(app.sort_order(), ListSortOrder::Relevance);
    assert_eq!(app.effective_sort_order(), ListSortOrder::Relevance);
    assert!(app.sort_order_applies());
    assert_eq!(names(&app), ["d", "b", "c", "a"]);
}

#[test]
fn the_browse_list_follows_the_date_order() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(corpus(), sandbox.options());
    app.set_sort_order(ListSortOrder::Oldest);
    assert_eq!(names(&app), ["a", "c", "b", "d"]);
    app.set_sort_order(ListSortOrder::Newest);
    assert_eq!(names(&app), ["d", "b", "c", "a"]);
    app.set_sort_order(ListSortOrder::Oldest);
    app.set_sort_order(ListSortOrder::Relevance);
    assert_eq!(
        names(&app),
        ["d", "b", "c", "a"],
        "the browse list is newest first"
    );
}

#[test]
fn lexical_results_are_ranked_or_dated_and_the_rank_comes_back() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(corpus(), sandbox.options());
    search(&mut app, "needle");
    let ranked = names(&app);
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0], "a", "the most matches rank first: {ranked:?}");

    app.set_sort_order(ListSortOrder::Newest);
    wait_for_search(&mut app);
    assert_eq!(names(&app), ["b", "c", "a"]);
    app.set_sort_order(ListSortOrder::Oldest);
    wait_for_search(&mut app);
    assert_eq!(names(&app), ["a", "c", "b"]);

    // A new query answers in the chosen order.
    search(&mut app, "needle again");
    assert_eq!(names(&app), ["c"]);
    search(&mut app, "needle");
    assert_eq!(names(&app), ["a", "c", "b"]);

    app.set_sort_order(ListSortOrder::Relevance);
    wait_for_search(&mut app);
    assert_eq!(names(&app), ranked);
}

#[test]
fn the_selected_session_stays_selected() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(corpus(), sandbox.options());
    search(&mut app, "needle");
    let row = names(&app).iter().position(|name| name == "c").unwrap();
    assert!(app.select(row));
    let chosen = app.selected_path();

    app.set_sort_order(ListSortOrder::Oldest);
    wait_for_search(&mut app);
    assert_eq!(app.selected_path(), chosen);
    app.set_sort_order(ListSortOrder::Relevance);
    wait_for_search(&mut app);
    assert_eq!(app.selected_path(), chosen);
    assert_eq!(app.query(), "needle", "the query is kept");
}

#[test]
fn semantic_results_stay_ranked() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(
        corpus(),
        sandbox
            .options()
            .with_sort_order(ListSortOrder::Oldest)
            .with_default_mode(ListSearchMode::Semantic),
    );
    assert_eq!(app.sort_order(), ListSortOrder::Oldest);
    assert_eq!(app.effective_sort_order(), ListSortOrder::Relevance);
    assert!(!app.sort_order_applies());
    let (_request_tx, request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    drop(request_rx);
    app.search_generation = 7;
    app.search_in_flight = true;
    app.semantic_search.pending_generation = Some(7);

    // Ranked c, a, b (indices 2, 3, 1): the order is kept.
    send_semantic_complete_response(
        &response_tx,
        7,
        vec![2, 3, 1],
        HashMap::new(),
        SemanticProgress::Complete,
    );
    assert!(app.poll());
    assert_eq!(names(&app), ["c", "a", "b"]);

    // Changing the order in semantic mode keeps the rows; it is stored.
    app.set_sort_order(ListSortOrder::Newest);
    assert_eq!(names(&app), ["c", "a", "b"]);
    assert_eq!(app.sort_order(), ListSortOrder::Newest);
}

#[test]
fn the_order_given_at_construction_applies() {
    let sandbox = Sandbox::new();
    let app = Browser::new(
        corpus(),
        sandbox.options().with_sort_order(ListSortOrder::Oldest),
    );
    assert_eq!(names(&app), ["a", "c", "b", "d"]);
}

#[test]
fn a_uuid_jump_shows_its_one_row_whatever_the_order() {
    let sandbox = Sandbox::new();
    let mut conversations = corpus();
    let uuid = "aaaaaaaa-1111-4111-8111-111111111111";
    conversations[1].session_id = uuid.to_string();
    let mut app = Browser::new(conversations, sandbox.options());
    app.set_query(uuid);
    assert_eq!(names(&app), ["b"]);
    app.set_sort_order(ListSortOrder::Oldest);
    assert_eq!(names(&app), ["b"]);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Found {
            loaded_from_disk: false,
            ..
        })
    ));
}

#[test]
fn semantic_mode_shows_the_browse_list_ranked() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(corpus(), sandbox.options());
    app.set_sort_order(ListSortOrder::Oldest);
    assert_eq!(names(&app), ["a", "c", "b", "d"]);
    assert!(app.select(1));
    let chosen = app.selected_path();

    app.set_list_search_mode(ListSearchMode::Semantic);
    assert_eq!(app.effective_sort_order(), ListSortOrder::Relevance);
    assert_eq!(names(&app), ["d", "b", "c", "a"], "as the control says");
    assert_eq!(app.selected_path(), chosen);
}

#[test]
fn rows_kept_by_a_pending_or_unknown_uuid_follow_the_date_order() {
    let sandbox = Sandbox::new();
    let mut app = Browser::new(corpus(), sandbox.options());
    let uuid = "cccccccc-3333-4333-8333-333333333333";
    app.set_query(uuid);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Pending { .. })
    ));
    assert_eq!(names(&app), ["d", "b", "c", "a"], "the rows of before stay");

    app.set_sort_order(ListSortOrder::Oldest);
    assert_eq!(names(&app), ["a", "c", "b", "d"]);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::Pending { .. })
    ));

    app.resolve_uuid_lookup(uuid, None);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::NotFound { .. })
    ));
    app.set_sort_order(ListSortOrder::Newest);
    assert_eq!(names(&app), ["d", "b", "c", "a"]);
    assert!(matches!(
        app.uuid_lookup(),
        Some(UuidLookup::NotFound { .. })
    ));
}
