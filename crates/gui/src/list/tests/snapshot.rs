//! G tests of the startup snapshot (L-099, L-062, L-068, L-040, A-001;
//! upstream 3708bb6): the first rows are the store's snapshot cards, newest
//! first, before any batch arrives; Resume, Fork and Ctrl+O work on them,
//! Rename and Delete wait for the load; the loaded corpus replaces them
//! keeping the selection, and the initial load writes the snapshot.
//!
//! Snapshots are off in tests (`app::init_for_test`); each test here turns
//! them on before opening the window and joins the write before its world
//! goes.

use std::path::PathBuf;
use std::time::SystemTime;

use claude_history_core::maintenance::load_sorted_conversations;
use claude_history_core::store::{self, Publish};
use claude_history_core::time_filter::TimeFilter;
use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;

use crate::actions::OpenRowContextMenu;
use crate::list::ListPaneEvent;
use crate::list::ListSnapshots;
use crate::list::footer::ChipKind;
use crate::test_support::fixtures_list::{ListFixture, SessionSpec, listed_corpus};
use crate::test_support::fixtures_ops::{project_dir, write_gui_config};
use crate::test_support::fixtures_viewer::{draw, wait_viewer_ready};
use crate::test_support::{TestWorld, recording};

/// Publish the store snapshot of the world's corpus with every card titled
/// `Card <n>` (newest first), so card rows are told apart from loaded rows.
/// Returns the card paths, newest first.
fn publish_cards(world: &TestWorld) -> Vec<PathBuf> {
    let mut conversations =
        load_sorted_conversations(&world.env, true, None, &TimeFilter::default())
            .expect("load the world's corpus");
    for (ix, conversation) in conversations.iter_mut().enumerate() {
        conversation.custom_title = Some(format!("Card {ix}"));
    }
    let root = store::default_root(&world.env).expect("a store root in the sandbox");
    assert!(root.starts_with(world.home()), "{root:?}");
    store::write_snapshot(&root, &conversations, SystemTime::now()).expect("write the snapshot");
    conversations.into_iter().map(|c| c.path).collect()
}

/// Open the main window with snapshots on and the poll loop paused (nothing
/// is loaded until the test steps or resumes it).
fn open_with_snapshots(cx: &mut TestAppContext, world: &TestWorld) -> ListFixture {
    ListFixture::init_app(cx, world, &[]);
    cx.update(|cx| cx.set_global(ListSnapshots(true)));
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

/// L-099, L-040, L-068: before any batch arrives the rows are the snapshot
/// cards, newest first, the first one selected; the suffix reads
/// `Loading… 0`; the open, resume and fork chips are lit while rename and
/// delete stay dimmed; the row menu enables Resume and disables Rename.
#[gpui_kit::test]
fn snapshot_cards_are_listed_newest_first_before_the_load(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let cards = publish_cards(&world);
    let f = open_with_snapshots(cx, &world);

    assert!(f.read(cx, |list, _| list.is_loading()));
    assert!(f.read(cx, |list, _| list.list_ready()));
    assert_eq!(f.paths(cx), cards, "newest first, before any batch");
    assert_eq!(
        f.titles(cx),
        (0..cards.len())
            .map(|ix| format!("Card {ix}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(f.selected_position(cx), Some(0));
    assert_eq!(
        f.read(cx, |list, _| list.search_status().text),
        "Loading… 0"
    );
    assert_eq!(
        f.read(cx, |list, cx| list.qa_probe("list.ready", cx)),
        Some("true".to_string())
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
        assert!(!dimmed(kind), "{kind:?} is lit on snapshot cards");
    }
    for kind in [ChipKind::Rename, ChipKind::Delete] {
        assert!(dimmed(kind), "{kind:?} waits for the load");
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
    assert!(
        matches!(
            f.list.update(cx, |list, _| list.finish_snapshot_write()),
            Some(Publish::Published(_))
        ),
        "the loaded corpus (real titles) replaces the cards' generation"
    );
}

/// L-062, A-001: on snapshot cards Ctrl+R resumes and Ctrl+F forks the
/// selected card's session (one recorded terminal command each) and Ctrl+O
/// asks before printing; F2 and Ctrl+X open no dialog until the load ends.
#[gpui_kit::test]
fn snapshot_cards_resume_and_fork_but_wait_to_edit(cx: &mut TestAppContext) {
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
    let f = open_with_snapshots(cx, &world);
    assert!(f.read(cx, |list, _| list.is_loading()));
    let stem = newest.file_stem().unwrap().to_string_lossy().into_owned();

    f.press(cx, "f2");
    f.press(cx, "ctrl-x");
    cx.run_until_parked();
    assert!(!has_dialog(&f, cx), "rename and delete wait for the load");

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

/// AD6, upstream 3708bb6: a card opened in the viewer while the load runs
/// does not make Rename and Delete available there either: F2 and Ctrl+X
/// (viewer focused) open no dialog until the load ends, then they work.
#[gpui_kit::test]
fn the_viewer_of_a_card_waits_to_rename_and_delete(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let cards = publish_cards(&world);
    let f = open_with_snapshots(cx, &world);
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
    assert!(!has_dialog(&f, cx), "delete waits for the load");
    f.press(cx, "f2");
    settle(cx);
    assert!(!has_dialog(&f, cx), "rename waits for the load");
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
    assert!(opened, "delete works once the load ended");
}

/// L-099, L-003: the loaded corpus replaces the cards in one step and the
/// selected card's session stays selected (no new `SelectionChanged`, so the
/// viewer keeps its conversation).
#[gpui_kit::test]
fn the_swap_keeps_the_selected_session(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let cards = publish_cards(&world);
    let f = open_with_snapshots(cx, &world);
    f.with_window(cx, |list, _, cx| list.select_by(3, cx));
    assert_eq!(f.selected_path(cx), Some(cards[3].clone()));
    let events = f.record_events(cx);

    f.resume(cx);
    f.wait_loaded(cx);
    assert!(!f.read(cx, |list, _| list.browser().shows_snapshot()));
    assert_eq!(f.selected_path(cx), Some(cards[3].clone()));
    assert_eq!(f.selected_position(cx), Some(3));
    assert!(
        f.titles(cx).iter().all(|title| !title.starts_with("Card ")),
        "the loaded rows replaced the cards: {:?}",
        f.titles(cx)
    );
    assert!(events.selections().is_empty(), "{:?}", events.all());
    assert!(events.count_matching(|e| *e == ListPaneEvent::CorpusChanged) >= 1);
    f.list.update(cx, |list, _| list.finish_snapshot_write());
}

/// L-099, L-001: without a snapshot the rows stream in as before; the
/// complete initial load then writes the first generation, and a second
/// window over the same corpus writes nothing new.
#[gpui_kit::test]
fn the_initial_load_writes_the_snapshot(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = open_with_snapshots(cx, &world);
    assert!(
        !f.read(cx, |list, _| list.list_ready()),
        "nothing to list yet"
    );
    f.step_until(cx, "the first batch", |list| {
        !list.loading_snapshots().is_empty()
    });
    assert!(!f.read(cx, |list, _| list.browser().shows_snapshot()));
    f.resume(cx);
    f.wait_loaded(cx);
    assert_eq!(
        f.list.update(cx, |list, _| list.finish_snapshot_write()),
        Some(Publish::Published("g000001".to_string()))
    );
    let root = store::default_root(&world.env).unwrap();
    let snapshot = store::Snapshot::open(&root, true, TimeFilter::default()).expect("CURRENT");
    assert_eq!(snapshot.conversations(100, |_| true).len(), 8);

    // A second window over the same corpus loads the same rows and writes
    // nothing new.
    let again = ListFixture::open(cx, &world, &[]);
    again.wait_loaded(cx);
    assert_eq!(
        again
            .list
            .update(cx, |list, _| list.finish_snapshot_write()),
        Some(Publish::Unchanged)
    );
}
