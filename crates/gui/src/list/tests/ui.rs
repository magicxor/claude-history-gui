//! G tests of the toolbar, footer, row menu, workspace picker, time filter,
//! picker banner and the row layout in a window (L-011…L-013, L-040,
//! L-041, L-048, L-049, L-068, L-081, L-084, L-089, L-090).

use chrono::{Duration, Utc};
use claude_history_core::config::{KeyBindings, parse_key_binding};
use claude_history_core::export::ExportFormat;
use gpui_kit::AppContext as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Entity, TestAppContext, px};

use crate::keymap;
use crate::list::ListPaneEvent;
use crate::list::context_menu::MenuEntry;
use crate::list::footer::{ChipKind, FooterModel, chip_width_rem};
use crate::list::toolbar::{
    NO_WORKSPACE_TOOLTIP, USER_ONLY_SEMANTIC_TOOLTIP, USER_ONLY_TOOLTIP, WORKSPACE_TOOLTIP,
};
use crate::list::workspace_picker::IS_DIR_CALLS;
use crate::model::RowMenuItem;
use crate::test_support::fixtures_list::{ListFixture, SessionSpec, listed_corpus};
use crate::test_support::{TestWorld, kbd, recording, synth};
use crate::widgets::time_range::{LastTimeRangeForm, TimeRangeChoice, open_custom_range_dialog};

fn footer(f: &ListFixture, cx: &mut TestAppContext) -> FooterModel {
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        f.list.read(cx).footer_model(window, cx)
    })
    .unwrap()
}

fn row_id(path: &std::path::Path) -> String {
    format!("row:{}", path.display())
}

fn chip(model: &FooterModel, kind: ChipKind) -> Option<String> {
    model
        .chips
        .iter()
        .find(|chip| chip.kind == kind)
        .map(|chip| chip.text())
}

/// L-040, L-041: chips come from the live keymap, are dimmed while loading,
/// trimmed to the width with `F1 help` kept, absent without focus, and never
/// advertise Refresh.
#[gpui_kit::test]
fn footer_hints_follow_the_keymap_and_the_focus(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    // Store off: the in-memory loader streams batches.
    crate::test_support::use_memory_list(cx, &world, &[]);
    let f = ListFixture::open_paused(cx, &world, &[]);
    f.step_until(cx, "the first batch", |list| {
        !list.loading_snapshots().is_empty()
    });
    let loading = cx
        .update_window(f.handle, |_, window, cx| {
            f.list.read(cx).footer_model_for_width(60.0, window, cx)
        })
        .unwrap();
    for kind in [
        ChipKind::Open,
        ChipKind::Resume,
        ChipKind::Fork,
        ChipKind::Rename,
        ChipKind::Delete,
    ] {
        let chip = loading
            .chips
            .iter()
            .find(|chip| chip.kind == kind)
            .unwrap_or_else(|| panic!("{kind:?} chip missing in {loading:?}"));
        assert!(chip.dimmed, "{kind:?} is dimmed while loading");
    }
    f.resume(cx);
    f.wait_loaded(cx);

    let wide = |f: &ListFixture, cx: &mut TestAppContext| {
        cx.update_window(f.handle, |_, window, cx| {
            f.list.read(cx).footer_model_for_width(60.0, window, cx)
        })
        .unwrap()
    };
    let model = wide(&f, cx);
    assert_eq!(
        chip(&model, ChipKind::Resume),
        Some(format!("{} resume", kbd("ctrl-r")))
    );
    assert!(model.chips.iter().all(|chip| !chip.dimmed));
    assert!(
        model
            .chips
            .iter()
            .all(|chip| !chip.label.to_lowercase().contains("refresh")),
        "L-041: Refresh is not advertised"
    );
    assert!(chip(&model, ChipKind::Help).is_some());

    // Rebinding Resume updates the chip live.
    cx.update(|cx| {
        let keys = KeyBindings {
            resume: parse_key_binding("alt+r").unwrap(),
            ..KeyBindings::default()
        };
        keymap::install_user_bindings(cx, &keys).unwrap();
    });
    cx.run_until_parked();
    let model = wide(&f, cx);
    assert_eq!(
        chip(&model, ChipKind::Resume),
        Some(format!("{} resume", kbd("alt-r")))
    );

    // 18rem: only the highest priority chips plus help remain.
    let narrow = cx
        .update_window(f.handle, |_, window, cx| {
            f.list.read(cx).footer_model_for_width(18.0, window, cx)
        })
        .unwrap();
    assert!(narrow.chips.len() < model.chips.len(), "{narrow:?}");
    assert!(chip(&narrow, ChipKind::Open).is_some());
    assert!(chip(&narrow, ChipKind::Help).is_some());
    assert!(chip(&narrow, ChipKind::Delete).is_none());

    // With the viewer focused the list shows no chips.
    f.press(cx, "secondary-3");
    assert!(footer(&f, cx).chips.is_empty());
}

/// L-084: without a workspace the Workspace segment is disabled with its tooltip.
#[gpui_kit::test]
fn the_workspace_segment_needs_a_workspace(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let model = f.read(cx, |list, _| list.toolbar_model());
    assert!(!model.workspace_enabled);
    assert_eq!(model.workspace_tooltip, NO_WORKSPACE_TOOLTIP);
    assert_eq!(model.workspace_label, "No workspace");
}

/// L-058, S-020: Ctrl+T toggles the mode and the toolbar follows; the mode is
/// remembered in the UI state.
#[gpui_kit::test]
fn ctrl_t_toggles_the_mode(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    crate::test_support::fixtures_list::ListFixture::init_app(cx, &world, &[]);
    let _control = crate::test_support::fixtures_list::install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let rows = f.paths(cx);
    use claude_history_core::browser::ListSearchMode::{Lexical, Semantic};
    assert_eq!(f.read(cx, |list, _| list.toolbar_model().mode), Lexical);
    f.press(cx, "ctrl-t");
    f.wait_idle(cx);
    assert_eq!(f.read(cx, |list, _| list.toolbar_model().mode), Semantic);
    assert_eq!(
        cx.read(|cx| crate::globals::app_state(cx).ui.search_mode),
        Some(crate::persist::RememberedSearchMode::Semantic)
    );
    f.press(cx, "ctrl-t");
    f.wait_idle(cx);
    assert_eq!(f.read(cx, |list, _| list.toolbar_model().mode), Lexical);
    assert_eq!(
        cx.read(|cx| crate::globals::app_state(cx).ui.search_mode),
        Some(crate::persist::RememberedSearchMode::Lexical)
    );
    assert_eq!(f.paths(cx), rows, "toggling twice returns to the same rows");
}

/// L-012 (G), L-011, L-013: rows are 4.25rem high, 5.5rem in literal mode;
/// the selected row's model is selected.
#[gpui_kit::test]
fn row_height_follows_the_literal_mode(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let first = f.paths(cx)[0].clone();
    let height = |cx: &mut TestAppContext| {
        cx.update_window(f.handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.find(row_id(&first)).bounds().size.height
        })
        .unwrap()
    };
    assert_eq!(height(cx), px(68.0), "4.25rem at 16 px");
    f.type_text(cx, "\"question\"");
    f.wait_idle(cx);
    assert!(f.read(cx, |list, _| list.literal));
    let first = f.paths(cx)[0].clone();
    let literal_height = cx
        .update_window(f.handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.find(row_id(&first)).bounds().size.height
        })
        .unwrap();
    assert_eq!(literal_height, px(88.0), "5.5rem at 16 px");

    let rows = f.read(cx, |list, _| list.row_models(0..1, 480.0));
    assert!(rows[0].selected);
}

/// L-068: the row menu lists the §5.2 items with live shortcuts, keeps the
/// target captured at build time, and Shift+F10 builds it for the selected row.
#[gpui_kit::test]
fn the_row_menu_keeps_its_captured_target(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let paths = f.paths(cx);
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.right_click(row_id(&paths[3]), cx);
    })
    .unwrap();
    assert_eq!(f.selected_position(cx), Some(3), "right click selects");
    let model = f
        .read(cx, |list, _| list.last_row_menu().cloned())
        .expect("a menu");
    let labels = model.labels();
    assert_eq!(
        &labels[..6],
        &[
            "Open",
            "Resume",
            "Fork",
            "Rename…",
            "Export…",
            "Copy to clipboard"
        ]
    );
    for label in [
        "Copy path",
        "Copy session ID",
        "Reveal in file manager",
        "Search within conversation…",
        "Outline",
        "Delete…",
    ] {
        assert!(labels.iter().any(|l| l == label), "{label} in {labels:?}");
    }
    assert_eq!(model.shortcut("Open"), Some(kbd("enter").as_str()));
    assert_eq!(model.shortcut("Resume"), Some(kbd("ctrl-r").as_str()));
    assert_eq!(model.shortcut("Fork"), Some(kbd("ctrl-f").as_str()));
    assert_eq!(model.shortcut("Rename…"), Some("F2"));
    assert_eq!(model.shortcut("Delete…"), Some(kbd("ctrl-x").as_str()));
    let MenuEntry::Submenu { entries, .. } = model.item("Copy to clipboard").unwrap() else {
        panic!("Copy to clipboard is a submenu");
    };
    assert_eq!(entries.len(), 4);
    assert!(!model.is_disabled("Resume"));

    // The selection moves; the token still names row 3.
    f.press(cx, "home");
    assert_eq!(f.selected_position(cx), Some(0));
    let target = f
        .read(cx, |list, _| list.row_menu_target(model.token))
        .unwrap();
    assert_eq!(target.path, paths[3]);

    // Open from the menu acts on the captured row.
    let events = f.record_events(cx);
    cx.update_window(f.handle, |_, window, cx| {
        window.dispatch_action(
            Box::new(crate::actions::RowMenu {
                item: RowMenuItem::Open,
                token: model.token,
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        events.all().iter().any(|event| matches!(
            event,
            ListPaneEvent::OpenRequested { target, .. } if target.path == paths[3]
        )),
        "{:?}",
        events.all()
    );

    // Shift+F10 builds the menu for the selected row (Open moved the focus
    // into the viewer: back to the list first).
    f.with_window(cx, |list, window, cx| list.focus_search(window, cx));
    f.press(cx, "shift-f10");
    let keyboard = f.read(cx, |list, _| list.last_row_menu().cloned()).unwrap();
    assert_ne!(keyboard.token, model.token);
    assert_eq!(keyboard.path, paths[0]);
    // Copy as… items exist for every export format.
    let formats: Vec<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            MenuEntry::Item {
                item: RowMenuItem::CopyAs(format),
                ..
            } => Some(*format),
            _ => None,
        })
        .collect();
    assert_eq!(
        formats,
        vec![
            ExportFormat::Ledger,
            ExportFormat::Plain,
            ExportFormat::Markdown,
            ExportFormat::Jsonl
        ]
    );
}

/// L-068: while loading the modifying items are disabled.
#[gpui_kit::test]
fn the_row_menu_disables_modifying_items_while_loading(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let target = f.read(cx, |list, _| list.selected_target()).unwrap();
    let model = cx.read(|cx| crate::list::context_menu::build(1, &target, true, false, cx));
    for label in ["Resume", "Fork", "Rename…", "Delete…"] {
        assert!(model.is_disabled(label), "{label}");
    }
    assert!(!model.is_disabled("Open"));
    // An unknown source disables them too.
    let mut unknown = target.clone();
    unknown.source = None;
    let model = cx.read(|cx| crate::list::context_menu::build(2, &unknown, false, true, cx));
    assert!(model.is_disabled("Resume") && model.is_disabled("Delete…"));
    // The header session ID differs only for Pi/OMP-style ids.
    assert!(
        !model.labels().iter().any(|l| l == "Copy header session ID")
            || unknown.header_id_differs()
    );
}

/// PD3 rule 3 (c): a directory recorded only by automated or child Codex
/// threads (hidden from the list) is no recorded cwd; a listed thread's is.
#[gpui_kit::test]
fn recorded_agent_cwds_skip_hidden_codex_threads(cx: &mut TestAppContext) {
    let hidden = TestWorld::new();
    for (name, bytes) in [
        (
            "rollout-2026-09-02T11-00-00-01a00000-0000-7000-8000-000000000006.jsonl",
            &include_bytes!(
                "../../../../core/tests/fixtures/codex/sessions/2026/09/02/rollout-2026-09-02T11-00-00-01a00000-0000-7000-8000-000000000006.jsonl"
            )[..],
        ),
        (
            "rollout-2026-09-02T12-00-00-01a00000-0000-7000-8000-000000000007.jsonl",
            &include_bytes!(
                "../../../../core/tests/fixtures/codex/sessions/2026/09/02/rollout-2026-09-02T12-00-00-01a00000-0000-7000-8000-000000000007.jsonl"
            )[..],
        ),
    ] {
        hidden.add_fixture(&format!(".codex/sessions/2026/09/02/{name}"), bytes);
    }
    let f = ListFixture::open_loaded(cx, &hidden, &[]);
    let (total, cwds) = cx.read(|cx| {
        let list = f.list.read(cx);
        (
            list.browser().conversations().len(),
            list.recorded_agent_cwds(),
        )
    });
    assert_eq!(total, 2, "both hidden threads are in the corpus");
    assert!(cwds.is_empty(), "{cwds:?}");
}

/// The listed Codex thread 002 and the hidden exec (006) and subagent (007)
/// threads of core's Codex fixtures.
fn codex_kinds_world() -> TestWorld {
    let world = TestWorld::new();
    for (day, name, bytes) in [
        (
            "01",
            "rollout-2026-09-01T10-00-00-01a00000-0000-7000-8000-000000000002.jsonl",
            &include_bytes!(
                "../../../../core/tests/fixtures/codex/sessions/2026/09/01/rollout-2026-09-01T10-00-00-01a00000-0000-7000-8000-000000000002.jsonl"
            )[..],
        ),
        (
            "02",
            "rollout-2026-09-02T11-00-00-01a00000-0000-7000-8000-000000000006.jsonl",
            &include_bytes!(
                "../../../../core/tests/fixtures/codex/sessions/2026/09/02/rollout-2026-09-02T11-00-00-01a00000-0000-7000-8000-000000000006.jsonl"
            )[..],
        ),
        (
            "02",
            "rollout-2026-09-02T12-00-00-01a00000-0000-7000-8000-000000000007.jsonl",
            &include_bytes!(
                "../../../../core/tests/fixtures/codex/sessions/2026/09/02/rollout-2026-09-02T12-00-00-01a00000-0000-7000-8000-000000000007.jsonl"
            )[..],
        ),
    ] {
        world.add_fixture(&format!(".codex/sessions/2026/09/{day}/{name}"), bytes);
    }
    world
}

/// The thread number (last UUID digits) of each listed row and its kind tag.
fn codex_rows(f: &ListFixture, cx: &TestAppContext) -> Vec<(String, Option<&'static str>)> {
    f.read(cx, |list, _| {
        let rows = list.filtered_paths().len();
        list.row_models(0..rows, 480.0)
            .into_iter()
            .map(|row| {
                let stem = row.stem.clone();
                (stem[stem.len() - 3..].to_string(), row.kind_label)
            })
            .collect()
    })
}

/// L-104 (upstream cb990e6): Ctrl+G lists the automated (exec) and subagent
/// Codex threads, tagged `auto` / `sub` on line 2, keeping the query and the
/// selected conversation; again Ctrl+G hides them. The View menu item and the
/// footer's `all kinds` chip follow the toggle.
#[gpui_kit::test]
fn ctrl_g_lists_automated_and_subagent_threads(cx: &mut TestAppContext) {
    let world = codex_kinds_world();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(codex_rows(&f, cx), [("002".to_string(), None)]);
    let probe = |cx: &TestAppContext| f.read(cx, |list, cx| list.qa_probe("list.all_kinds", cx));
    assert_eq!(probe(cx).as_deref(), Some("false"));
    assert!(!cx.read(|cx| crate::menus::current_checks(cx).all_kinds));
    let wide_footer = |cx: &mut TestAppContext| {
        cx.update_window(f.handle, |_, window, cx| {
            window.render_frame(cx);
            f.list.read(cx).footer_model_for_width(120.0, window, cx)
        })
        .unwrap()
    };
    assert_eq!(chip(&wide_footer(cx), ChipKind::AllKinds), None);
    let selected = f.selected_path(cx);
    assert!(selected.is_some());

    f.press(cx, "ctrl-g");
    f.wait_idle(cx);
    assert_eq!(probe(cx).as_deref(), Some("true"));
    // Newest first; the listed thread moves down and stays selected.
    assert_eq!(
        codex_rows(&f, cx),
        [
            ("007".to_string(), Some("sub")),
            ("006".to_string(), Some("auto")),
            ("002".to_string(), None),
        ]
    );
    assert_eq!(f.selected_path(cx), selected, "the selection is kept");
    assert!(cx.read(|cx| crate::menus::current_checks(cx).all_kinds));
    assert_eq!(
        chip(&wide_footer(cx), ChipKind::AllKinds),
        Some(format!("{} all kinds", kbd("ctrl-g")))
    );
    let tagged = f.read(cx, |list, _| {
        let rows = list.filtered_paths().len();
        list.row_models(0..rows, 480.0)
            .into_iter()
            .find(|row| row.kind_label.is_some())
            .map(|row| (row.meta_text(), row.tooltip.clone()))
            .unwrap()
    });
    assert!(tagged.0.starts_with("sub · "), "{}", tagged.0);
    assert!(tagged.1.starts_with("sub · "), "{}", tagged.1);

    // "iguana" is only in the exec thread: the query stays across toggles.
    f.type_text(cx, "iguana");
    f.wait_idle(cx);
    assert_eq!(codex_rows(&f, cx), [("006".to_string(), Some("auto"))]);
    f.press(cx, "ctrl-g");
    f.wait_idle(cx);
    assert_eq!(probe(cx).as_deref(), Some("false"));
    assert!(codex_rows(&f, cx).is_empty());
    assert_eq!(f.read(cx, |list, cx| list.query(cx).to_string()), "iguana");
    f.press(cx, "ctrl-g");
    f.wait_idle(cx);
    assert_eq!(codex_rows(&f, cx), [("006".to_string(), Some("auto"))]);

    f.press(cx, "escape");
    f.press(cx, "ctrl-g");
    f.wait_idle(cx);
    assert_eq!(codex_rows(&f, cx), [("002".to_string(), None)]);
    assert!(!cx.read(|cx| crate::menus::current_checks(cx).all_kinds));
}

/// C-068: `--all-kinds` starts with the hidden kinds listed; `[keys]
/// all_kinds` rebinds the toggle.
#[gpui_kit::test]
fn the_all_kinds_flag_and_key_binding(cx: &mut TestAppContext) {
    let world = codex_kinds_world();
    world.write_config(Some("[keys]\nall_kinds = \"alt+k\"\n"), None);
    let f = ListFixture::open_loaded(cx, &world, &["--all-kinds"]);
    assert_eq!(codex_rows(&f, cx).len(), 3);
    f.press(cx, "ctrl-g");
    f.wait_idle(cx);
    assert_eq!(codex_rows(&f, cx).len(), 3, "ctrl-g is no longer bound");
    f.press(cx, "alt-k");
    f.wait_idle(cx);
    assert_eq!(codex_rows(&f, cx), [("002".to_string(), None)]);
}

/// Two conversations that mention `walrus`: `asked` in what the user typed,
/// `answered` only in the assistant's reply (twice, so it ranks first).
fn walrus_world() -> (TestWorld, std::path::PathBuf, std::path::PathBuf) {
    let world = TestWorld::new();
    let start = Utc::now() - Duration::hours(3);
    let asked = SessionSpec::new("alpha", 1)
        .messages(&[
            ("user", "how does the walrus work"),
            ("assistant", "it assigns a value"),
        ])
        .start(start)
        .write(&world);
    let answered = SessionSpec::new("beta", 2)
        .messages(&[
            ("user", "explain this operator"),
            ("assistant", "the walrus assigns, the walrus returns"),
        ])
        .start(start + Duration::hours(1))
        .write(&world);
    (world, asked, answered)
}

/// S-039 (upstream ae7ae39): Alt+U searches only the user's own messages,
/// keeping the query and the selected conversation; Alt+U again searches
/// everything. The toolbar's `You only` toggle, the View menu item and the
/// footer's `you only` chip follow; in semantic mode, which ignores it, the
/// chip and the tooltip say so.
#[gpui_kit::test]
fn alt_u_searches_only_your_own_messages(cx: &mut TestAppContext) {
    let (world, asked, answered) = walrus_world();
    ListFixture::init_app(cx, &world, &[]);
    let _control = crate::test_support::fixtures_list::install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let probe = |cx: &TestAppContext| f.read(cx, |list, cx| list.qa_probe("list.user_only", cx));
    let wide_footer = |cx: &mut TestAppContext| {
        cx.update_window(f.handle, |_, window, cx| {
            window.render_frame(cx);
            f.list.read(cx).footer_model_for_width(120.0, window, cx)
        })
        .unwrap()
    };
    assert_eq!(probe(cx).as_deref(), Some("false"));
    assert_eq!(chip(&wide_footer(cx), ChipKind::UserOnly), None);

    f.type_text(cx, "walrus");
    f.wait_idle(cx);
    assert_eq!(f.paths(cx), [answered.clone(), asked.clone()]);

    f.press(cx, "alt-u");
    f.wait_idle(cx);
    assert_eq!(probe(cx).as_deref(), Some("true"));
    assert_eq!(f.paths(cx), std::slice::from_ref(&asked));
    assert_eq!(f.read(cx, |list, cx| list.query(cx).to_string()), "walrus");
    assert!(cx.read(|cx| crate::menus::current_checks(cx).user_only));
    let toolbar = f.read(cx, |list, _| list.toolbar_model());
    assert!(toolbar.user_only);
    assert_eq!(toolbar.user_only_tooltip, USER_ONLY_TOOLTIP);
    let chip_text = chip(&wide_footer(cx), ChipKind::UserOnly);
    assert_eq!(chip_text, Some(format!("{} you only", kbd("alt-u"))));
    assert_eq!(f.selected_path(cx), Some(asked.clone()));

    // Everything again: the query stays and so does the selection.
    f.press(cx, "alt-u");
    f.wait_idle(cx);
    assert_eq!(probe(cx).as_deref(), Some("false"));
    assert_eq!(f.paths(cx), [answered.clone(), asked.clone()]);
    assert_eq!(
        f.selected_path(cx),
        Some(asked.clone()),
        "the selection is kept"
    );
    assert!(!cx.read(|cx| crate::menus::current_checks(cx).user_only));
    assert_eq!(chip(&wide_footer(cx), ChipKind::UserOnly), None);

    // Semantic search ignores the toggle; the chip and the tooltip say so.
    f.press(cx, "alt-u");
    f.press(cx, "ctrl-t");
    f.wait_idle(cx);
    let toolbar = f.read(cx, |list, _| list.toolbar_model());
    assert!(toolbar.user_only);
    assert_eq!(toolbar.user_only_tooltip, USER_ONLY_SEMANTIC_TOOLTIP);
    assert_eq!(
        chip(&wide_footer(cx), ChipKind::UserOnly),
        Some(format!("{} you only: not in semantic", kbd("alt-u")))
    );
    // 18rem with all kinds on too: the pinned chips and help stay and fit.
    f.press(cx, "ctrl-g");
    f.wait_idle(cx);
    let narrow = cx
        .update_window(f.handle, |_, window, cx| {
            window.render_frame(cx);
            f.list.read(cx).footer_model_for_width(18.0, window, cx)
        })
        .unwrap();
    for kind in [ChipKind::AllKinds, ChipKind::UserOnly, ChipKind::Help] {
        assert!(chip(&narrow, kind).is_some(), "{kind:?}: {narrow:?}");
    }
    let width: f32 = narrow
        .chips
        .iter()
        .map(|chip| {
            let key = if chip.keyless {
                0
            } else {
                chip.key.chars().count()
            };
            chip_width_rem(key, chip.label.chars().count())
        })
        .sum::<f32>()
        + (narrow.chips.len() - 1) as f32 * 0.75;
    let count = narrow.count.chars().count() as f32 * 0.35 + 1.0;
    assert!(width + count + 1.5 <= 18.0, "{width} {narrow:?}");
    f.press(cx, "ctrl-g");
    f.wait_idle(cx);
    f.press(cx, "ctrl-t");
    f.wait_idle(cx);
    assert_eq!(f.paths(cx), [asked], "lexical again: only yours");
}

/// C-070: upstream's default `[keys] user_only = "ctrl+v"` is paste in the
/// GUI: it is rejected and Alt+U stays.
#[gpui_kit::test]
fn the_user_only_key_binding(cx: &mut TestAppContext) {
    let (world, asked, _) = walrus_world();
    world.write_config(Some("[keys]\nuser_only = \"ctrl+v\"\n"), None);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "walrus");
    f.press(cx, "alt-u");
    f.wait_idle(cx);
    assert_eq!(f.paths(cx), [asked]);
    f.press(cx, "alt-u");
    f.wait_idle(cx);
    assert_eq!(f.paths(cx).len(), 2);
}

/// C-070: `gui.toml` `[keys] user_only` rebinds the toggle.
#[gpui_kit::test]
fn gui_toml_rebinds_the_user_only_key(cx: &mut TestAppContext) {
    let (world, asked, _) = walrus_world();
    world.write_config(None, Some("[keys]\nuser_only = \"alt+y\"\n"));
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "walrus");
    f.press(cx, "alt-y");
    f.wait_idle(cx);
    assert_eq!(f.paths(cx), [asked]);
}

#[gpui_kit::test]
fn recorded_agent_cwds_keep_listed_codex_threads(cx: &mut TestAppContext) {
    let listed = TestWorld::new();
    listed.add_fixture(
        ".codex/sessions/2026/09/01/rollout-2026-09-01T10-00-00-01a00000-0000-7000-8000-000000000002.jsonl",
        include_bytes!(
            "../../../../core/tests/fixtures/codex/sessions/2026/09/01/rollout-2026-09-01T10-00-00-01a00000-0000-7000-8000-000000000002.jsonl"
        ),
    );
    let f = ListFixture::open_loaded(cx, &listed, &[]);
    let cwds = cx.read(|cx| f.list.read(cx).recorded_agent_cwds());
    assert_eq!(cwds, vec![std::path::PathBuf::from("/work/demo")]);
}

/// L-081: the workspace picker shows Recent and Projects from cached data,
/// makes no file-system call when built, and Choose folder… switches.
#[gpui_kit::test]
fn the_workspace_picker_uses_cached_paths(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let work = world.home().join("work");
    let alpha = work.join("alpha");
    let beta = work.join("beta");
    let gone = work.join("gone");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    SessionSpec::new("x", 1)
        .cwd(alpha.clone())
        .summary("A")
        .write(&world);
    SessionSpec::new("x", 2)
        .cwd(beta.clone())
        .summary("B")
        .start(Utc::now() - Duration::hours(2))
        .write(&world);
    // state.json: two recents, one of which no longer exists.
    let state = crate::persist::state_file(&world.env).unwrap();
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(
        &state,
        serde_json::json!({ "recent_workspaces": [alpha, gone] }).to_string(),
    )
    .unwrap();

    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.wait_until(cx, "the cached paths", |list, _| {
        list.project_paths().len() == 2 && !list.recent_existing.is_empty()
    });
    let calls = IS_DIR_CALLS.with(std::cell::Cell::get);
    let model = cx.read(|cx| f.list.read(cx).workspace_picker_model(cx));
    assert_eq!(
        IS_DIR_CALLS.with(std::cell::Cell::get),
        calls,
        "building the menu makes no file-system call"
    );
    assert_eq!(model.recent.len(), 1, "the missing recent is hidden");
    assert_eq!(model.recent[0].path, alpha);
    assert_eq!(model.recent[0].name, "alpha");
    let projects: Vec<_> = model
        .projects
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert_eq!(projects, vec![alpha.clone(), beta.clone()], "newest first");
    assert!(!model.can_clear);

    // Choose folder… with a scripted answer.
    recording(cx).answer_open(Ok(Some(vec![beta.clone()])));
    f.with_window(cx, |list, window, cx| {
        list.choose_workspace_folder(window, cx)
    });
    cx.run_until_parked();
    f.wait_idle(cx);
    let title = cx.read(|cx| f.ws.read(cx).title().to_string());
    assert_eq!(title, "Claude History — beta");
    let toolbar = f.read(cx, |list, _| list.toolbar_model());
    assert!(toolbar.workspace_enabled);
    assert_eq!(toolbar.workspace_tooltip, WORKSPACE_TOOLTIP);
    assert_eq!(toolbar.workspace_label, "beta");
    let recent = cx.read(|cx| crate::globals::app_state(cx).ui.recent_workspaces.clone());
    assert_eq!(recent.first(), Some(&beta));

    // Clear workspace.
    f.with_window(cx, |list, _, cx| list.clear_workspace(cx));
    f.wait_idle(cx);
    assert_eq!(
        cx.read(|cx| f.ws.read(cx).title().to_string()),
        "Claude History — All projects"
    );
    assert!(!f.read(cx, |list, _| list.has_workspace()));
}

/// L-048: the CLI filter is applied, shown as a chip and kept by a refresh;
/// a failing reload reverts the filter with an error toast (§11.21).
#[gpui_kit::test]
fn the_time_filter_is_applied_kept_and_reverted(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1).summary("Recent").write(&world);
    SessionSpec::new("proj", 2)
        .summary("Old")
        .start(Utc::now() - Duration::days(30))
        .write(&world);
    // Store off: an in-memory reload reads the files again and can fail (in store mode the cards are re-scoped).
    crate::test_support::use_memory_list(cx, &world, &["--since", "1d"]);
    let f = ListFixture::open_loaded(cx, &world, &["--since", "1d"]);
    assert_eq!(f.titles(cx), vec!["Recent".to_string()]);
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().time_label)
            .as_ref(),
        "Since 1d"
    );
    f.with_window(cx, |list, _, cx| list.refresh(cx));
    f.wait_loaded(cx);
    assert_eq!(
        f.titles(cx),
        vec!["Recent".to_string()],
        "a refresh keeps the filter"
    );

    // A preset reloads: everything within a year.
    let choice = crate::widgets::time_range::preset_choice("1y").unwrap();
    f.with_window(cx, |list, _, cx| {
        list.set_time_filter(choice.filter, choice.label.map(|l| l.to_string()), cx)
    });
    f.wait_loaded(cx);
    assert_eq!(f.titles(cx).len(), 2);
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().time_label)
            .as_ref(),
        "Since 1y"
    );

    // A reload that fails reverts the chip and the loader filter.
    std::fs::remove_dir_all(world.projects_root()).unwrap();
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
    assert_eq!(f.titles(cx).len(), 2, "the rows are kept");
}

/// §11.20: a failing workspace reload reverts the workspace.
#[gpui_kit::test]
fn a_failing_workspace_reload_reverts(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1).summary("One").write(&world);
    // Store off: an in-memory reload reads the files again and can fail.
    crate::test_support::use_memory_list(cx, &world, &[]);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let dir = world.home().join("some-workspace");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(world.projects_root()).unwrap();
    f.with_window(cx, |list, window, cx| {
        list.apply_workspace_choice(dir.clone(), window, cx)
    });
    f.wait_loaded(cx);
    assert!(
        !f.read(cx, |list, _| list.has_workspace()),
        "the workspace reverted"
    );
    assert_eq!(
        cx.read(|cx| f.ws.read(cx).title().to_string()),
        "Claude History — All projects"
    );
    let last = f.read(cx, |list, _| list.toasts().last().cloned()).unwrap();
    assert!(
        last.text.starts_with("Couldn't switch the workspace: "),
        "{}",
        last.text
    );
    assert_eq!(f.paths(cx).len(), 1);
}

/// L-049: the custom range dialog validates inline and Apply stays disabled.
#[gpui_kit::test]
fn the_custom_range_dialog_validates(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let applied: std::rc::Rc<std::cell::RefCell<Vec<TimeRangeChoice>>> = Default::default();
    let sink = applied.clone();
    cx.update_window(f.handle, |_, window, cx| {
        open_custom_range_dialog(
            TimeRangeChoice::default(),
            move |choice, _, _| sink.borrow_mut().push(choice),
            window,
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    let form: Entity<crate::widgets::time_range::TimeRangeForm> = cx
        .read(|cx| cx.global::<LastTimeRangeForm>().0.upgrade())
        .expect("the form");
    assert!(
        cx.read(|cx| form.read(cx).can_apply()),
        "both empty = Any time"
    );
    f.type_text(cx, "2x");
    assert_eq!(
        cx.read(|cx| form.read(cx).error().map(str::to_string))
            .as_deref(),
        Some("'2x': unknown unit 'x' (s, m (minutes), h, d, w, mo (months), y)")
    );
    assert!(!cx.read(|cx| form.read(cx).can_apply()));
    // Fix it: `2d` applies.
    f.press(cx, "backspace");
    f.type_text(cx, "d");
    assert!(cx.read(|cx| form.read(cx).can_apply()));
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("time-range-apply", cx);
    })
    .unwrap();
    let applied = applied.borrow();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].label.as_deref(), Some("Since 2d"));
}

/// L-089: picker mode shows the banner; Select dispatches `SelectAndExit`.
#[gpui_kit::test]
fn the_picker_banner_has_a_select_button(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &["--show-id"]);
    assert_eq!(
        crate::list::picker_banner::banner_text(
            claude_history_core::settings::PickerAction::ShowId
        ),
        "Picker mode: Ctrl+O or Select prints the session ID and exits"
    );
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("picker-banner").is_some());
        window.click("picker-select", cx);
    })
    .unwrap();
    cx.run_until_parked();
    // `SelectAndExit` reached `ops::select` (WP3): the test services have no
    // stdout, so the picker answers with the "nothing to print to" toast.
    assert_eq!(
        cx.read(crate::globals::last_toast).as_ref(),
        crate::ops::select::NOTHING_TO_PRINT
    );
}

/// L-090: the spinner shows while a search runs and is gone after the results.
#[gpui_kit::test]
fn the_search_spinner_shows_while_a_worker_runs(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 30);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert!(!f.read(cx, |list, _| list.search_status().spinner));
    cx.dispatch_keystroke(f.handle, gpui_kit::Keystroke::parse("s").unwrap());
    assert!(
        f.read(cx, |list, _| list.search_status().spinner),
        "right after typing"
    );
    f.wait_idle(cx);
    assert!(!f.read(cx, |list, _| list.search_status().spinner));
}

/// L-048: a time filter queued behind a failing reload keeps the user's last
/// choice for its own attempt, and its failure reverts to the committed
/// filter (not to the failed intermediate one).
#[gpui_kit::test]
fn a_queued_time_filter_reverts_to_the_committed_one(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1).summary("Recent").write(&world);
    SessionSpec::new("proj", 2)
        .summary("Very old")
        .start(Utc::now() - Duration::days(800))
        .write(&world);
    // Store off: an in-memory reload reads the files again and can fail (in store mode the cards are re-scoped).
    crate::test_support::use_memory_list(cx, &world, &[]);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(f.titles(cx).len(), 2);
    std::fs::remove_dir_all(world.projects_root()).unwrap();
    let year = crate::widgets::time_range::preset_choice("1y").unwrap();
    let week = crate::widgets::time_range::preset_choice("1w").unwrap();
    f.with_window(cx, |list, _, cx| {
        list.set_time_filter(year.filter, year.label.map(|l| l.to_string()), cx);
        list.set_time_filter(week.filter, week.label.map(|l| l.to_string()), cx);
    });
    f.wait_idle(cx);
    f.wait_until(cx, "both reloads to end", |list, _| {
        !list.is_refreshing() && !list.loader().is_running()
    });
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().time_label)
            .as_ref(),
        "Any time",
        "the committed filter is back"
    );
    assert_eq!(f.titles(cx).len(), 2, "the rows are kept");
}
