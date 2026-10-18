//! G tests of the Keyboard shortcuts dialog (DESIGN §5.17, §8.7): L-069,
//! V-087, C-053.

use claude_history_core::config::{KeyBindings, parse_key_binding};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, DummyKeyboardMapper, KeyBinding, ScrollDelta, TestAppContext,
    point, px,
};

use crate::actions::{CommandPalette, OpenSettings, Rename, SelectPrev, ShowShortcuts};
use crate::help::shortcuts::{self, ShortcutRow};
use crate::keymap;
use crate::model::HelpSection;
use crate::test_support::{TestWorld, open_main, open_single_file};

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn has_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

/// How a keystroke is displayed (`J`, `↓`, `Ctrl+P`): the same formatter the
/// dialog uses, applied to the default keystroke of the table.
fn label(keystroke: &str) -> String {
    KeyBinding::load(
        keystroke,
        Box::new(SelectPrev),
        None,
        false,
        None,
        &DummyKeyboardMapper,
    )
    .unwrap()
    .keystrokes()
    .iter()
    .map(|keystroke| Kbd::format(keystroke.inner()))
    .collect::<Vec<_>>()
    .join(" ")
}

fn row<'a>(rows: &'a [ShortcutRow], section: HelpSection, text: &str) -> &'a ShortcutRow {
    rows.iter()
        .find(|row| row.section == section && row.label.as_ref() == text)
        .unwrap_or_else(|| panic!("row `{text}` in {section:?}: {rows:#?}"))
}

/// L-069: F1 opens the Shortcuts dialog with the Conversation list section
/// first; Esc closes it.
#[gpui_kit::test]
fn f1_opens_the_dialog_and_esc_closes_it(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    assert!(!has_dialog(cx, handle));
    cx.simulate_keystrokes(handle, "f1");
    cx.run_until_parked();
    render(cx, handle);
    assert!(has_dialog(cx, handle), "F1 opens the dialog");
    let dialog = cx.read(shortcuts::current).expect("the dialog");
    let rows = cx.read(|cx| dialog.read(cx).rows().to_vec());
    assert_eq!(rows[0].section, HelpSection::List, "list section first");
    assert_eq!(rows[0].label.as_ref(), "Move selection");
    assert_eq!(
        rows[0].keys.as_ref(),
        format!("{} / {}", label("up"), label("down"))
    );
    let sections = shortcuts::visible_sections(&rows);
    assert_eq!(sections, HelpSection::ALL.to_vec());
    assert!(!dialog.read_with(cx, |dialog, _| dialog.is_single_file()));
    cx.simulate_keystrokes(handle, "escape");
    cx.run_until_parked();
    assert!(!has_dialog(cx, handle), "Esc closes it");
}

/// L-069: the dialog scrolls natively and clamped: scrolling far past the end
/// stops at the last row (the TUI's unbounded scroll value is gone).
#[gpui_kit::test]
fn scrolling_past_the_end_keeps_the_last_row_visible(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.dispatch_action(handle, ShowShortcuts);
    render(cx, handle);
    render(cx, handle);
    let dialog = cx.read(shortcuts::current).expect("the dialog");
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            "shortcuts-list",
            ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
            cx,
        );
    })
    .unwrap();
    render(cx, handle);
    let (offset, max) = dialog.read_with(cx, |dialog, _| dialog.scroll_extent());
    assert!(max > 0.0, "the list overflows its box");
    assert!(
        (offset - max).abs() < 1.0,
        "scrolled to the end and no further: offset {offset}, max {max}"
    );
    // And back to the top.
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            "shortcuts-list",
            ScrollDelta::Pixels(point(px(0.), px(100_000.))),
            cx,
        );
    })
    .unwrap();
    render(cx, handle);
    let (offset, _) = dialog.read_with(cx, |dialog, _| dialog.scroll_extent());
    assert!(offset.abs() < 1.0, "back at the top: {offset}");
}

/// The dialog opens scrolled to the section of the region that had focus.
#[gpui_kit::test]
fn the_dialog_opens_at_the_focused_regions_section(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| {
        crate::help::open_shortcuts(Some(HelpSection::Global), window, cx)
    })
    .unwrap();
    render(cx, handle);
    render(cx, handle);
    let dialog = cx.read(shortcuts::current).expect("the dialog");
    let (offset, max) = dialog.read_with(cx, |dialog, _| dialog.scroll_extent());
    assert!(
        offset > 0.0 && offset <= max,
        "scrolled towards the Global section: {offset} of {max}"
    );
}

/// V-087: the Viewer section lists the TUI labels with live keys.
#[gpui_kit::test]
fn the_viewer_section_lists_the_tui_labels_with_live_keys(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let rows = cx.read(|cx| shortcuts::shortcut_rows(false, cx));
    let viewer = HelpSection::Viewer;
    let pair = |a: &str, b: &str| format!("{} / {}", label(a), label(b));
    for (text, keys) in [
        ("Scroll down", pair("j", "down")),
        ("Scroll up", pair("k", "up")),
        ("Next message", pair("shift-j", "]")),
        ("Previous message", pair("shift-k", "[")),
        ("Half page down", pair("d", "ctrl-d")),
        ("Half page up", pair("u", "ctrl-u")),
        ("Jump to top", pair("g", "home")),
        ("Jump to bottom", pair("shift-g", "end")),
        ("Next / prev match", pair("n", "shift-n")),
        ("Back to list", pair("q", "escape")),
    ] {
        assert_eq!(row(&rows, viewer, text).keys.as_ref(), keys, "{text}");
    }
    assert_eq!(row(&rows, viewer, "Search").keys.as_ref(), label("/"));
    assert_eq!(
        row(&rows, viewer, "Export to file").keys.as_ref(),
        label("e")
    );
    assert_eq!(row(&rows, viewer, "Resume").keys.as_ref(), label("ctrl-r"));
    assert_eq!(row(&rows, viewer, "Delete").keys.as_ref(), label("ctrl-x"));
}

/// C-053: the rows carry live keys: after rebinding Rename to F3 the Rename
/// row shows `F3`; in a single-file window the Back row reads `Close` and the
/// session rows are absent.
#[gpui_kit::test]
fn the_rows_reflect_rebinding(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let rows = cx.read(|cx| shortcuts::shortcut_rows(false, cx));
    assert_eq!(row(&rows, HelpSection::List, "Rename").keys.as_ref(), "F2");
    cx.update(|cx| {
        keymap::install_user_bindings(
            cx,
            &KeyBindings {
                rename: parse_key_binding("f3").unwrap(),
                ..KeyBindings::default()
            },
        )
        .unwrap()
    });
    cx.run_until_parked();
    let rows = cx.read(|cx| shortcuts::shortcut_rows(false, cx));
    assert_eq!(row(&rows, HelpSection::List, "Rename").keys.as_ref(), "F3");
    assert_eq!(
        cx.read(|cx| keymap::binding_labels(&Rename, "Workspace ConversationList", cx))
            .iter()
            .map(|l| l.as_ref())
            .collect::<Vec<_>>(),
        ["F3"]
    );
}

#[gpui_kit::test]
fn a_single_file_window_says_close(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = world.add_claude_session(
        "-synthetic-proj",
        &crate::test_support::synth::uuid(7),
        &[
            r#"{"type":"user","sessionId":"x","timestamp":"2026-09-01T10:00:00Z","message":{"role":"user","content":"hello"}}"#,
        ],
    );
    let handle = open_single_file(cx, &world, &path);
    let rows = cx.read(|cx| shortcuts::shortcut_rows(true, cx));
    let viewer = HelpSection::Viewer;
    assert!(
        rows.iter()
            .any(|r| r.section == viewer && r.label.as_ref() == "Close")
    );
    assert!(
        !rows
            .iter()
            .any(|r| r.section == viewer && r.label.as_ref() == "Back to list")
    );
    for text in ["Resume", "Fork resume", "Delete"] {
        assert!(
            !rows
                .iter()
                .any(|r| r.section == viewer && r.label.as_ref() == text),
            "{text} has no handler in a single-file window"
        );
    }
    assert!(
        !rows
            .iter()
            .any(|r| r.section == HelpSection::List && r.label.as_ref() == "Rename"),
        "no session rows"
    );
    // F1 in that window opens the single-file version.
    cx.simulate_keystrokes(handle, "f1");
    cx.run_until_parked();
    let dialog = cx.read(shortcuts::current).expect("the dialog");
    assert!(dialog.read_with(cx, |dialog, _| dialog.is_single_file()));
}

/// L-069/X-025: the dialog scrolls and closes from the keyboard like the TUI
/// overlay (j/k, PgDn/PgUp, d/u, g, q).
#[gpui_kit::test]
fn the_dialog_scrolls_and_closes_from_the_keyboard(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.dispatch_action(handle, ShowShortcuts);
    render(cx, handle);
    render(cx, handle);
    let dialog = cx.read(shortcuts::current).expect("the dialog");
    let offset = |cx: &mut TestAppContext| dialog.read_with(cx, |d, _| d.scroll_extent().0);
    cx.simulate_keystrokes(handle, "j j");
    render(cx, handle);
    let after_j = offset(cx);
    assert!(after_j > 0.0, "j scrolls down: {after_j}");
    cx.simulate_keystrokes(handle, "pagedown");
    render(cx, handle);
    let after_page = offset(cx);
    assert!(after_page > after_j, "PgDn scrolls further");
    cx.simulate_keystrokes(handle, "g");
    render(cx, handle);
    assert!(offset(cx).abs() < 1.0, "g jumps to the top");
    cx.simulate_keystrokes(handle, "q");
    render(cx, handle);
    assert!(!has_dialog(cx, handle), "q closes the dialog");
}

/// QA: `help::dialog_id` names the dialog a window shows.
#[gpui_kit::test]
fn dialog_id_names_the_open_dialog(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let id = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| crate::help::dialog_id(window, cx))
            .unwrap()
    };
    assert_eq!(id(cx), "none");
    cx.dispatch_action(handle, ShowShortcuts);
    render(cx, handle);
    assert_eq!(id(cx), "shortcuts");
    cx.simulate_keystrokes(handle, "escape");
    render(cx, handle);
    assert_eq!(id(cx), "none");
    cx.dispatch_action(handle, CommandPalette);
    render(cx, handle);
    assert_eq!(id(cx), "palette");
    cx.simulate_keystrokes(handle, "escape");
    render(cx, handle);
    cx.dispatch_action(handle, OpenSettings);
    render(cx, handle);
    assert_eq!(id(cx), "settings");
}

/// X-026b: the Refresh rows say "conversations" (the in-app word for the
/// list object, §0/§12), never the TUI's "sessions", and no row label of the
/// dialog uses the word for list objects.
#[gpui_kit::test]
fn the_refresh_rows_say_conversations_not_sessions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let rows = cx.read(|cx| shortcuts::shortcut_rows(false, cx));
    for section in [HelpSection::List, HelpSection::Viewer] {
        let refresh = row(&rows, section, "Refresh conversations");
        assert!(!refresh.keys.is_empty());
    }
    assert!(
        rows.iter()
            .all(|r| !r.label.to_lowercase().contains("refresh sessions")),
        "no TUI wording"
    );
}
