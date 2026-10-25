//! G tests of the list in store mode (L-099…L-101, L-062, L-068, L-040,
//! A-001, L-004, L-048, S-015; upstream 3708bb6, ac7f591, e905169, 358becb):
//! the first rows are the newest cards of the store's generation; Resume,
//! Fork and Ctrl+O work on them, Rename and Delete wait until the generation
//! is shown in full; a query typed before that runs on the generation's
//! index; the initial load is a reconcile pass whose newer generation is
//! swapped in keeping the selection; a time filter re-scopes the cards and a
//! UUID finds a card outside it; a delete hides the row until a generation
//! drops it.
//!
//! The store is on in tests (`app::init_for_test`); tests that end while a
//! pass may still publish join it (`ListPane::finish_snapshot_write`).

use std::path::PathBuf;

use chrono::{Duration, Utc};
use claude_history_core::store;
use claude_history_core::store::card::Fingerprint;
use claude_history_core::store::generation::{self, Doc, Publish, PublishLock, Update};
use claude_history_core::store::load_in_memory;
use claude_history_core::time_filter::TimeFilter;
use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;

use crate::actions::OpenRowContextMenu;
use crate::list::ListPaneEvent;
use crate::list::footer::ChipKind;
use crate::test_support::fixtures_list::{ListFixture, SessionSpec, listed_corpus};
use crate::test_support::fixtures_ops::{project_dir, write_gui_config};
use crate::test_support::fixtures_viewer::{draw, wait_viewer_ready};
use crate::test_support::{TestWorld, recording, synth};

/// Publish the store generation of the world's corpus with every card titled
/// `Card <n>` (newest first), so card rows are told apart from parsed rows.
/// The fingerprints are unknown, so the reconcile pass of the initial load
/// parses every session again and publishes the real titles. Returns the
/// card paths, newest first. The corpus is parsed from the session files
/// (`maintenance::load_sorted_conversations` would publish a generation of
/// its own first).
fn publish_cards(world: &TestWorld) -> Vec<PathBuf> {
    let mut conversations = load_in_memory(&world.env, true).expect("load the world's corpus");
    conversations.sort_by_key(|c| std::cmp::Reverse(c.timestamp));
    for (ix, conversation) in conversations.iter_mut().enumerate() {
        conversation.custom_title = Some(format!("Card {ix}"));
    }
    let root = store::default_root(&world.env).expect("a store root in the sandbox");
    assert!(root.starts_with(world.home()), "{root:?}");
    let lock = PublishLock::try_acquire(&root)
        .expect("open the lock")
        .expect("take the lock");
    let paths = conversations.iter().map(|c| c.path.clone()).collect();
    let docs = conversations
        .into_iter()
        .map(|conversation| Doc::New {
            conversation: Box::new(conversation),
            fingerprint: Fingerprint::default(),
        })
        .collect();
    let (publish, _) = generation::publish_update(
        &lock,
        Update {
            base: None,
            docs,
            ignored: Vec::new(),
        },
    )
    .expect("write the generation");
    assert!(matches!(publish, Publish::Published(_)), "{publish:?}");
    paths
}

/// Hold the store's lock as another instance would: the initial pass waits
/// (retrying every second) until the lock is dropped, so the generation
/// published by [`publish_cards`] stays shown meanwhile.
fn hold_store(world: &TestWorld) -> PublishLock {
    let root = store::default_root(&world.env).expect("a store root in the sandbox");
    PublishLock::try_acquire(&root)
        .expect("open the lock")
        .expect("take the lock")
}

/// Open the main window with the poll loop paused (nothing is installed
/// until the test steps or resumes it; the first frame's cards are shown).
fn open_paused(cx: &mut TestAppContext, world: &TestWorld) -> ListFixture {
    let f = ListFixture::open_paused(cx, world, &[]);
    f.with_window(cx, |list, window, cx| list.focus_search(window, cx));
    cx.run_until_parked();
    f
}

fn has_dialog(f: &ListFixture, cx: &mut TestAppContext) -> bool {
    cx.update_window(f.handle, |_, window, cx| {
        use gpui_kit::component::WindowExt as _;
        window.has_active_dialog(cx)
    })
    .unwrap()
}

/// Let the resume's background planning and the deferred launch finish.
fn settle(cx: &mut TestAppContext) {
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    cx.run_until_parked();
}

/// Release the store (`held`), then wait until the reconcile pass has
/// published and its generation is shown (the titles are parsed ones, not
/// `Card <n>`).
fn wait_for_the_reconciled_titles(f: &ListFixture, cx: &mut TestAppContext, held: PublishLock) {
    drop(held);
    let published = f.list.update(cx, |list, _| list.finish_snapshot_write());
    assert!(
        matches!(published, Some(Publish::Published(_))),
        "the pass publishes the parsed titles: {published:?}"
    );
    f.wait_until(cx, "the swap to the reconciled generation", |list, _| {
        list.browser()
            .conversations()
            .iter()
            .all(|c| !c.custom_title.as_deref().unwrap_or("").starts_with("Card "))
    });
    f.wait_idle(cx);
}

/// L-099, L-040, L-068: the first frame lists the newest cards, the first
/// one selected; open, resume and fork are lit while rename and delete stay
/// dimmed until the generation is shown in full; then the pass's generation
/// (the parsed titles) is swapped in.
#[gpui_kit::test]
fn the_first_frame_lists_the_cards_of_the_generation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let cards = publish_cards(&world);
    let held = hold_store(&world);
    let f = open_paused(cx, &world);

    assert!(f.read(cx, |list, _| list.is_loading()));
    assert!(f.read(cx, |list, _| list.list_ready()));
    assert_eq!(f.paths(cx), cards, "newest first, before the install");
    assert_eq!(
        f.titles(cx),
        (0..cards.len())
            .map(|ix| format!("Card {ix}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(f.selected_position(cx), Some(0));
    assert_eq!(
        f.read(cx, |list, _| list.search_status().text),
        format!("1/{}", cards.len())
    );
    assert_eq!(
        f.read(cx, |list, cx| list.qa_probe("list.ready", cx)),
        Some("true".to_string())
    );
    assert_eq!(
        f.read(cx, |list, cx| list.qa_probe("list.generation", cx)),
        Some("g000001".to_string())
    );

    let footer = cx
        .update_window(f.handle, |_, window, cx| {
            f.list.read(cx).footer_model_for_width(60.0, window, cx)
        })
        .unwrap();
    let dimmed = |kind: ChipKind| {
        footer
            .chips
            .iter()
            .find(|chip| chip.kind == kind)
            .unwrap_or_else(|| panic!("{kind:?} chip missing in {footer:?}"))
            .dimmed
    };
    for kind in [ChipKind::Open, ChipKind::Resume, ChipKind::Fork] {
        assert!(!dimmed(kind), "{kind:?} is lit on the first cards");
    }
    for kind in [ChipKind::Rename, ChipKind::Delete] {
        assert!(dimmed(kind), "{kind:?} waits for the install");
    }

    f.with_window(cx, |list, window, cx| {
        list.on_open_row_menu(&OpenRowContextMenu, window, cx)
    });
    let menu = f
        .read(cx, |list, _| list.last_row_menu().cloned())
        .expect("a row menu");
    assert_eq!(menu.path, cards[0]);
    for label in ["Open", "Resume", "Fork"] {
        assert!(!menu.is_disabled(label), "{label}");
    }
    for label in ["Rename…", "Delete…"] {
        assert!(menu.is_disabled(label), "{label}");
    }

    f.resume(cx);
    f.wait_loaded(cx);
    assert_eq!(f.paths(cx), cards, "every card, still the published titles");
    assert!(
        f.read(cx, |list, _| list.browser().generation_open()),
        "the generation is shown in full"
    );
    assert_eq!(
        f.read(cx, |list, cx| list.qa_probe("list.index", cx)),
        Some("g000001".to_string())
    );
    assert_eq!(
        f.read(cx, |list, cx| list.qa_probe("list.reconciling", cx)),
        Some("true".to_string()),
        "the pass waits for the store"
    );
    wait_for_the_reconciled_titles(&f, cx, held);
    assert_eq!(f.paths(cx).len(), cards.len());
    assert_eq!(
        f.read(cx, |list, cx| list.qa_probe("list.reconciling", cx)),
        Some("false".to_string()),
        "the pass's generation is shown"
    );
}

/// L-062, A-001: on the first cards Ctrl+R resumes and Ctrl+F forks the
/// selected card's session (one recorded terminal command each) and Ctrl+O
/// asks before printing; F2 and Ctrl+X open no dialog until the install.
#[gpui_kit::test]
fn the_first_cards_resume_and_fork_but_wait_to_edit(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let cwd = project_dir(&world, "proj");
    SessionSpec::new("ops", 1)
        .cwd(cwd.clone())
        .summary("Older session")
        .start(chrono::Utc::now() - chrono::Duration::hours(2))
        .write(&world);
    let newest = SessionSpec::new("ops", 2)
        .cwd(cwd)
        .summary("Newest session")
        .write(&world);
    let cards = publish_cards(&world);
    assert_eq!(cards[0], newest);
    let f = open_paused(cx, &world);
    assert!(f.read(cx, |list, _| list.is_loading()));
    let stem = newest.file_stem().unwrap().to_string_lossy().into_owned();

    f.press(cx, "f2");
    f.press(cx, "ctrl-x");
    cx.run_until_parked();
    assert!(
        !has_dialog(&f, cx),
        "rename and delete wait for the install"
    );

    f.press(cx, "ctrl-r");
    settle(cx);
    let commands = recording(cx).terminal_commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].display().contains(&format!("--resume {stem}")),
        "{}",
        commands[0].display()
    );

    f.press(cx, "ctrl-f");
    settle(cx);
    let commands = recording(cx).terminal_commands();
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(
        commands[1]
            .display()
            .contains(&format!("--resume {stem} --fork-session")),
        "{}",
        commands[1].display()
    );

    recording(cx).set_stdout_kind(crate::model::StdoutKind::Terminal);
    f.press(cx, "ctrl-o");
    cx.run_until_parked();
    assert!(
        has_dialog(&f, cx),
        "Ctrl+O asks before printing a card's path"
    );
    assert!(f.read(cx, |list, _| list.is_loading()), "still loading");

    f.resume(cx);
    f.wait_loaded(cx);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
}

/// AD6, upstream 3708bb6: a card opened in the viewer before the install
/// does not make Rename and Delete available there either: F2 and Ctrl+X
/// (viewer focused) open no dialog until then, then they work.
#[gpui_kit::test]
fn the_viewer_of_a_card_waits_to_rename_and_delete(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let cards = publish_cards(&world);
    let f = open_paused(cx, &world);
    assert!(f.read(cx, |list, _| list.is_loading()));
    // Enter on the selected card: the target carries the source the card knows.
    f.press(cx, "enter");
    let viewer = cx.read(|cx| f.ws.read(cx).viewer().clone());
    wait_viewer_ready(cx, f.handle, &viewer);
    assert_eq!(
        cx.read(|cx| viewer.read(cx).target().map(|t| t.path.clone())),
        Some(cards[0].clone())
    );
    assert!(f.read(cx, |list, _| list.is_loading()), "still loading");

    f.press(cx, "ctrl-x");
    settle(cx);
    assert!(!has_dialog(&f, cx), "delete waits for the install");
    f.press(cx, "f2");
    settle(cx);
    assert!(!has_dialog(&f, cx), "rename waits for the install");
    assert!(cx.read(|cx| viewer.read(cx).target().is_some()));

    f.resume(cx);
    f.wait_loaded(cx);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
    f.press(cx, "ctrl-x");
    let mut opened = false;
    for _ in 0..200 {
        settle(cx);
        draw(cx, f.handle);
        if has_dialog(&f, cx) {
            opened = true;
            break;
        }
    }
    assert!(opened, "delete works once the generation is shown in full");
}

/// L-004, upstream ac7f591, 358becb: a query typed over the first cards is
/// kept (no index yet, the cards stay listed) and runs on the generation's
/// index once it is installed; the rows are cards.
#[gpui_kit::test]
fn a_query_typed_over_the_first_cards_runs_on_the_index(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let written = listed_corpus(&world);
    let cards = publish_cards(&world);
    let held = hold_store(&world);
    let f = open_paused(cx, &world);

    f.type_text(cx, "deploy");
    assert!(
        f.read(cx, |list, _| !list.browser().readiness().lexical
            && !list.browser().has_search_work_in_flight()),
        "the query waits for the index"
    );
    assert_eq!(f.paths(cx), cards, "the cards stay listed");

    f.resume(cx);
    f.wait_loaded(cx);
    let found = f.paths(cx);
    assert!(found.contains(&written[1]), "{found:?}");
    assert!(found.len() < cards.len(), "{found:?}");
    assert!(
        f.titles(cx).iter().all(|title| title.starts_with("Card ")),
        "the rows are cards: {:?}",
        f.titles(cx)
    );
    assert_eq!(
        f.read(cx, |list, _| list
            .browser()
            .search_index_generation()
            .map(str::to_string)),
        Some("g000001".to_string())
    );
    // The pass publishes the parsed titles; the query runs on the new
    // generation's index and keeps the rows.
    wait_for_the_reconciled_titles(&f, cx, held);
    assert_eq!(f.paths(cx), found);
}

/// L-099, L-003: the install of every card and the swap to the reconciled
/// generation keep the selected session (no new `SelectionChanged`, so the
/// viewer keeps its conversation).
#[gpui_kit::test]
fn the_install_and_the_swap_keep_the_selected_session(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let cards = publish_cards(&world);
    let held = hold_store(&world);
    let f = open_paused(cx, &world);
    f.with_window(cx, |list, _, cx| list.select_by(3, cx));
    assert_eq!(f.selected_path(cx), Some(cards[3].clone()));
    let events = f.record_events(cx);

    f.resume(cx);
    f.wait_loaded(cx);
    assert_eq!(f.selected_path(cx), Some(cards[3].clone()));
    wait_for_the_reconciled_titles(&f, cx, held);
    assert_eq!(f.selected_path(cx), Some(cards[3].clone()));
    assert_eq!(f.selected_position(cx), Some(3));
    assert!(events.selections().is_empty(), "{:?}", events.all());
    assert!(events.count_matching(|e| *e == ListPaneEvent::CorpusChanged) >= 1);
}

/// L-099, L-101: without a generation the list waits (`Loading…`) for the
/// first one the initial pass publishes; a second window over the same
/// session files lists the same rows and its pass writes nothing.
#[gpui_kit::test]
fn the_initial_pass_publishes_the_first_generation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let written = listed_corpus(&world);
    let f = open_paused(cx, &world);
    assert!(
        !f.read(cx, |list, _| list.list_ready()),
        "nothing to list yet"
    );
    assert_eq!(f.read(cx, |list, _| list.search_status().text), "Loading…");
    f.resume(cx);
    f.wait_loaded(cx);
    assert_eq!(f.paths(cx).len(), 8, "{written:?}");
    assert!(f.read(cx, |list, _| list.browser().shows_snapshot()));
    let published = f.list.update(cx, |list, _| list.finish_snapshot_write());
    assert!(
        matches!(
            published,
            Some(Publish::Published(_)) | Some(Publish::Unchanged)
        ),
        "{published:?}"
    );
    let root = store::default_root(&world.env).unwrap();
    let snapshot = store::Snapshot::open(&root, true, TimeFilter::default()).expect("CURRENT");
    assert_eq!(snapshot.len(), 8);
    assert!(
        store::SearchIndex::open(snapshot).is_some(),
        "the generation carries its text and index"
    );

    // A second window over the same session files lists the same rows and
    // writes nothing new.
    let again = ListFixture::open(cx, &world, &[]);
    again.wait_loaded(cx);
    assert_eq!(again.paths(cx), f.paths(cx));
    assert_eq!(
        again
            .list
            .update(cx, |list, _| list.finish_snapshot_write()),
        Some(Publish::Unchanged)
    );
}

/// L-048 (store): the CLI time filter hides the older cards; another filter
/// re-scopes the cards of the same generation.
#[gpui_kit::test]
fn a_time_filter_rescopes_the_cards(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1).summary("Recent").write(&world);
    SessionSpec::new("proj", 2)
        .summary("Old")
        .start(Utc::now() - Duration::days(30))
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--since", "1d"]);
    assert_eq!(f.titles(cx), vec!["Recent".to_string()]);
    assert_eq!(
        f.read(cx, |list, _| list.browser().generation_len()),
        Some(2),
        "the generation holds both"
    );
    f.list.update(cx, |list, _| list.finish_snapshot_write());

    let year = crate::widgets::time_range::preset_choice("1y").unwrap();
    f.with_window(cx, |list, _, cx| {
        list.set_time_filter(year.filter, year.label.map(|l| l.to_string()), cx)
    });
    f.wait_loaded(cx);
    assert_eq!(f.titles(cx).len(), 2);
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().time_label)
            .as_ref(),
        "Since 1y"
    );
    f.list.update(cx, |list, _| list.finish_snapshot_write());
}

/// L-048, §11.21 (store, review fix): a reload whose reconcile pass fails
/// reverts the time filter, and the cards follow the reverted filter at
/// once (the revert schedules the poll that re-scopes them).
#[gpui_kit::test]
fn a_failing_pass_reverts_the_time_filter_and_its_cards(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1).summary("Recent").write(&world);
    SessionSpec::new("proj", 2)
        .summary("Old")
        .start(Utc::now() - Duration::days(30))
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--since", "1d"]);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
    f.wait_idle(cx);
    let year = crate::widgets::time_range::preset_choice("1y").unwrap();
    f.with_window(cx, |list, _, cx| {
        list.set_time_filter(year.filter, year.label.map(|l| l.to_string()), cx)
    });
    f.wait_loaded(cx);
    assert_eq!(f.titles(cx).len(), 2);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
    f.wait_idle(cx);

    // The next pass cannot open the store's lock file: it fails.
    let root = store::default_root(&world.env).unwrap();
    std::fs::remove_file(root.join("LOCK")).unwrap();
    std::fs::create_dir(root.join("LOCK")).unwrap();
    let day = crate::widgets::time_range::preset_choice("1d").unwrap();
    f.with_window(cx, |list, _, cx| {
        list.set_time_filter(day.filter, day.label.map(|l| l.to_string()), cx)
    });
    f.wait_loaded(cx);
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().time_label)
            .as_ref(),
        "Since 1y",
        "the chip reverted"
    );
    let last = f.read(cx, |list, _| list.toasts().last().cloned()).unwrap();
    assert!(
        last.text.starts_with("Couldn't apply the time filter: "),
        "{}",
        last.text
    );
    assert_eq!(f.titles(cx).len(), 2, "the cards of the reverted filter");
}

/// S-015 (store): a session outside the time filter is a card of the
/// generation, so its UUID finds it at once (no disk lookup).
#[gpui_kit::test]
fn a_uuid_outside_the_time_filter_is_found_among_the_cards(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let old = SessionSpec::new("proj", 70)
        .summary("Old session")
        .start(Utc::now() - Duration::days(400))
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--since", "1d"]);
    assert!(!f.paths(cx).contains(&old));
    f.type_text(cx, &synth::uuid(70));
    assert!(
        matches!(
            f.read(cx, |list, _| list.uuid_lookup().cloned()),
            Some(claude_history_core::browser::UuidLookup::Found {
                loaded_from_disk: false,
                ..
            })
        ),
        "found at once"
    );
    assert_eq!(f.paths(cx), vec![old]);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
}

/// Upstream 358becb: a deleted conversation's row is hidden at once and its
/// card is gone from the generation the pass the delete asked for publishes.
#[gpui_kit::test]
fn a_delete_hides_the_row_until_a_generation_drops_it(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let written = listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
    f.wait_idle(cx);
    let cards = f
        .read(cx, |list, _| list.browser().generation_len())
        .expect("a generation");
    let gone = written[2].clone();
    std::fs::remove_file(&gone).unwrap();
    f.list.update(cx, |list, cx| {
        list.remove_paths(std::slice::from_ref(&gone), cx)
    });
    assert!(!f.paths(cx).contains(&gone));
    assert!(
        f.read(cx, |list, _| list.conversation(&gone)).is_none(),
        "a hidden row is not in the list"
    );
    f.wait_until(cx, "a generation without the session", |list, _| {
        list.browser().generation_len() == Some(cards - 1)
    });
    assert_eq!(f.paths(cx).len(), cards - 1);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
}
