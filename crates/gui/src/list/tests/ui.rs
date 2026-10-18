//! G tests of the toolbar, footer, row menu, workspace picker, time filter,
//! picker banner and the row layout in a window (L-011…L-013, L-040,
//! L-041, L-048, L-049, L-068, L-081, L-084, L-089, L-090).

use std::sync::atomic::Ordering;

use chrono::{Duration, Utc};
use claude_history_core::config::{KeyBindings, parse_key_binding};
use claude_history_core::export::ExportFormat;
use gpui_kit::AppContext as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Entity, TestAppContext, px};

use crate::keymap;
use crate::list::ListPaneEvent;
use crate::list::context_menu::MenuEntry;
use crate::list::footer::{ChipKind, FooterModel};
use crate::list::toolbar::{NO_WORKSPACE_TOOLTIP, WORKSPACE_TOOLTIP};
use crate::list::workspace_picker::IS_DIR_CALLS;
use crate::model::RowMenuItem;
use crate::test_support::fixtures_list::{ListFixture, SessionSpec, listed_corpus};
use crate::test_support::{TestWorld, recording, synth};
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
        chip(&model, ChipKind::Resume).as_deref(),
        Some("Ctrl+R resume")
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
        chip(&model, ChipKind::Resume).as_deref(),
        Some("Alt+R resume")
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
    assert_eq!(model.shortcut("Open"), Some("Enter"));
    assert_eq!(model.shortcut("Resume"), Some("Ctrl+R"));
    assert_eq!(model.shortcut("Fork"), Some("Ctrl+F"));
    assert_eq!(model.shortcut("Rename…"), Some("F2"));
    assert_eq!(model.shortcut("Delete…"), Some("Ctrl+X"));
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
    let model = cx.read(|cx| crate::list::context_menu::build(1, &target, true, cx));
    for label in ["Resume", "Fork", "Rename…", "Delete…"] {
        assert!(model.is_disabled(label), "{label}");
    }
    assert!(!model.is_disabled("Open"));
    // An unknown source disables them too.
    let mut unknown = target.clone();
    unknown.source = None;
    let model = cx.read(|cx| crate::list::context_menu::build(2, &unknown, false, cx));
    assert!(model.is_disabled("Resume") && model.is_disabled("Delete…"));
    // The header session ID differs only for Pi/OMP-style ids.
    assert!(
        !model.labels().iter().any(|l| l == "Copy header session ID")
            || unknown.header_id_differs()
    );
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
    let calls = IS_DIR_CALLS.load(Ordering::SeqCst);
    let model = cx.read(|cx| f.list.read(cx).workspace_picker_model(cx));
    assert_eq!(
        IS_DIR_CALLS.load(Ordering::SeqCst),
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
