//! WP0 skeleton tests (DESIGN §17.1 item 15) and the WP0 FEATURES rows
//! L-086, L-087, L-088, A-053, C-056, C-063, C-064, X-036, plus the frozen
//! cross-WP contracts (`contracts`), the integrator's Phase-B wiring
//! (`phase_b`) and the source safety lint (`safety_lint`).

mod contracts;
mod phase_b;
mod safety_lint;

use std::time::Duration;

use claude_history_core::config::{KeyBindings, parse_key_binding};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{GlobalState, WindowExt as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Focusable as _, ParentElement as _, TestAppContext, px, size};

use crate::actions::{Quit, UNIT_ACTION_NAMES};
use crate::exit;
use crate::keymap;
use crate::list::ListPaneEvent;
use crate::menus;
use crate::model::{ConversationTarget, LeftTab};
use crate::test_support::{TestWorld, kbd, open_main, redirect_root};

/// The viewer of a single-file window.
fn single_file_viewer(
    cx: &mut TestAppContext,
    handle: gpui_kit::AnyWindowHandle,
) -> gpui_kit::Entity<crate::viewer::ViewerPane> {
    cx.update_window(handle, |root, _, cx| {
        let root = root
            .downcast::<gpui_kit::base::Root>()
            .expect("a gpui-kit root");
        let view = root.read(cx).view().clone();
        view.downcast::<crate::single_file::SingleFileWindow>()
            .expect("a single-file window")
            .read(cx)
            .viewer()
            .clone()
    })
    .unwrap()
}

/// A second conversation (another project) for tests that need two.
fn second_session(world: &TestWorld) -> std::path::PathBuf {
    let uuid = "8a95d451-1c2d-4e5f-8a9b-0c1d2e3f4a5b";
    let lines = crate::test_support::synth::claude_conversation(
        uuid,
        &crate::test_support::synth::cwd_base().join("api"),
        "Deploy pipeline",
        2,
        1,
    );
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.add_claude_session("C--synthetic-api", uuid, &refs)
}

fn claude_session(world: &TestWorld) -> std::path::PathBuf {
    let lines = crate::test_support::synth::claude_conversation(
        "3f2a19c0-1c2d-4e5f-8a9b-0c1d2e3f4a5b",
        &crate::test_support::synth::cwd_base().join("proj"),
        "Fix login bug",
        1,
        1,
    );
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.add_claude_session(
        "C--synthetic-proj",
        "3f2a19c0-1c2d-4e5f-8a9b-0c1d2e3f4a5b",
        &refs,
    )
}

#[gpui_kit::test]
fn skeleton_opens_and_focuses_search(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let list = ws.read(cx).list().clone();
        assert!(
            list.focus_handle(cx).is_focused(window),
            "the search input has focus"
        );
        assert_eq!(ws.read(cx).left_tab(), LeftTab::Conversations);
        assert_eq!(ws.read(cx).title(), "Claude History — All projects");
        assert_eq!(
            GlobalState::global(cx).app_menus().len(),
            menus::MENU_NAMES.len()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn every_action_builds_by_name(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        for name in UNIT_ACTION_NAMES {
            assert!(cx.build_action(name, None).is_ok(), "{name} builds by name");
        }
        let registered = cx
            .all_action_names()
            .iter()
            .filter(|name| name.starts_with("history::"))
            .count();
        // Every unit action plus the RowMenu data action.
        assert_eq!(registered, UNIT_ACTION_NAMES.len() + 1);
        assert!(cx.all_action_names().contains(&"history::RowMenu"));
    });
}

#[gpui_kit::test]
fn static_keymap_installs_without_panic(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        let count = cx.key_bindings().borrow().bindings().count();
        assert!(count > keymap::static_bindings().len());
        // Rebinding Resume keeps the static F5 → Refresh row (C-034).
        let keys = KeyBindings {
            resume: parse_key_binding("alt+r").unwrap(),
            ..KeyBindings::default()
        };
        keymap::install_user_bindings(cx, &keys).unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let refresh = keymap::binding_labels(&crate::actions::Refresh, keymap::CTX_WORKSPACE, cx);
        assert!(
            refresh.iter().any(|label| label.as_ref() == "F5"),
            "{refresh:?}"
        );
        let resume = keymap::binding_labels(&crate::actions::Resume, keymap::CTX_LIST, cx);
        assert_eq!(resume.len(), 1, "{resume:?}");
        assert_eq!(resume[0].as_ref(), kbd("alt-r"));
    });
}

#[gpui_kit::test]
fn menu_tree_builds(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        let tree = menus::menu_tree(cx);
        let names: Vec<String> = tree.iter().map(|menu| menu.name.to_string()).collect();
        assert_eq!(names, menus::MENU_NAMES);
        let groups = crate::palette::palette_groups(cx);
        let entries: usize = groups.iter().map(|group| group.entries.len()).sum();
        assert!(
            entries > 50,
            "every menu leaf is a palette command ({entries})"
        );
    });
}

/// A-053, C-063: `secondary-q` quits even while a dialog input has focus,
/// and File → Quit quits with focus cleared.
#[gpui_kit::test]
fn quit_action_quits_even_with_dialog_focus(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let input = cx
        .update_window(handle, |_, window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            let dialog_input = input.clone();
            window.open_dialog(cx, move |dialog, _, _| {
                dialog.title("Test dialog").child(Input::new(&dialog_input))
            });
            input
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        input.focus_handle(cx).focus(window, cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(input.focus_handle(cx).is_focused(window));
    })
    .unwrap();
    assert_eq!(cx.read(exit::quit_requested), None);
    cx.simulate_keystrokes(handle, "secondary-q");
    assert_eq!(cx.read(exit::quit_requested), Some(exit::EXIT_OK));

    // With focus cleared, the menu's Quit (dispatched with nothing focused) quits.
    let world = TestWorld::new();
    let mut cx2 = cx.new_app();
    let (handle2, _ws2) = open_main(&mut cx2, &world, &[]);
    cx2.update_window(handle2, |_, window, cx| window.blur(cx))
        .unwrap();
    cx2.dispatch_action(handle2, Quit);
    assert_eq!(cx2.read(exit::quit_requested), Some(exit::EXIT_OK));
}

/// C-056: `secondary-w` never closes the main window; `alt-f4` on the main
/// window quits (Windows/Linux).
#[gpui_kit::test]
fn close_keys_follow_the_desktop_rules(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.simulate_keystrokes(handle, "secondary-w");
    assert_eq!(cx.read(exit::quit_requested), None);
    assert!(
        cx.update_window(handle, |_, _, _| ()).is_ok(),
        "the main window stays open"
    );
    if !cfg!(target_os = "macos") {
        cx.simulate_keystrokes(handle, "alt-f4");
        assert_eq!(cx.read(exit::quit_requested), Some(exit::EXIT_OK));
    }
}

/// C-064: Ctrl+W deletes the previous word in the deep-search query and in a
/// dialog input; the window stays open.
#[cfg(not(target_os = "macos"))]
#[gpui_kit::test]
fn ctrl_w_in_input_deletes_word(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, ws) = open_main(cx, &world, &[]);
    cx.simulate_keystrokes(handle, "secondary-2");
    cx.simulate_input(handle, "hello world");
    cx.simulate_keystrokes(handle, "ctrl-w");
    let query = cx.read(|cx| ws.read(cx).deep().read(cx).qa_probe("deep.query", cx));
    assert_eq!(query.as_deref(), Some("hello "));
    assert_eq!(cx.read(exit::quit_requested), None);

    let input = cx
        .update_window(handle, |_, window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            let dialog_input = input.clone();
            window.open_dialog(cx, move |dialog, _, _| {
                dialog.title("Test dialog").child(Input::new(&dialog_input))
            });
            input
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        input.focus_handle(cx).focus(window, cx)
    })
    .unwrap();
    cx.simulate_input(handle, "foo bar");
    cx.simulate_keystrokes(handle, "ctrl-w");
    assert_eq!(cx.read(|cx| input.read(cx).value()).as_ref(), "foo ");
    assert!(cx.update_window(handle, |_, _, _| ()).is_ok());
    assert_eq!(cx.read(exit::quit_requested), None);
}

/// Enter on an empty list does nothing; an open request (what Enter emits
/// for the selected row) shows the conversation and moves focus to the viewer.
#[gpui_kit::test]
fn enter_moves_focus_to_viewer_stub(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, ws) = open_main(cx, &world, &[]);
    cx.simulate_keystrokes(handle, "enter");
    cx.update_window(handle, |_, window, cx| {
        let list = ws.read(cx).list().clone();
        assert!(list.focus_handle(cx).is_focused(window));
    })
    .unwrap();

    // Written after the first Enter: the real list loads asynchronously, so a
    // session on disk at startup could already be selected (WP1).
    let path = claude_session(&world);
    let target = ConversationTarget::from_parsed_file(&path, None);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: target.clone(),
            focus_viewer: true,
            agent_tab: None,
        })
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let viewer = ws.read(cx).viewer().clone();
        assert!(
            viewer.read(cx).contains_focus(window, cx),
            "the viewer has focus"
        );
        assert_eq!(
            viewer.read(cx).target().map(|t| t.path.clone()),
            Some(path.clone())
        );
    })
    .unwrap();
}

/// L-086: `secondary-2` activates Deep search and focuses its query;
/// `secondary-1` returns and focuses the search input.
#[gpui_kit::test]
fn left_tabs_switch_with_secondary_digits(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, ws) = open_main(cx, &world, &[]);
    cx.simulate_keystrokes(handle, "secondary-2");
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(ws.read(cx).left_tab(), LeftTab::DeepSearch);
        let deep = ws.read(cx).deep().clone();
        assert!(deep.focus_handle(cx).is_focused(window));
    })
    .unwrap();
    cx.simulate_keystrokes(handle, "secondary-1");
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(ws.read(cx).left_tab(), LeftTab::Conversations);
        let list = ws.read(cx).list().clone();
        assert!(list.focus_handle(cx).is_focused(window));
    })
    .unwrap();
}

/// L-087: `secondary-shift-b` hides the left pane and moves focus to the
/// viewer; leaving the viewer re-shows the list and focuses the search input.
#[gpui_kit::test]
fn focus_viewer_mode_hides_and_restores_the_list(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: ConversationTarget::from_parsed_file(&path, None),
            focus_viewer: false,
            agent_tab: None,
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes(handle, "secondary-shift-b");
    cx.update_window(handle, |_, window, cx| {
        assert!(ws.read(cx).left_collapsed());
        let viewer = ws.read(cx).viewer().clone();
        assert!(viewer.read(cx).contains_focus(window, cx));
    })
    .unwrap();
    // Esc in the viewer with nothing to unwind → LeaveRequested.
    cx.simulate_keystrokes(handle, "escape");
    cx.update_window(handle, |_, window, cx| {
        assert!(!ws.read(cx).left_collapsed());
        let list = ws.read(cx).list().clone();
        assert!(list.focus_handle(cx).is_focused(window));
    })
    .unwrap();
}

/// L-088: widths clamp to the tab range (50rem → 40rem) and an 880 px window
/// keeps the viewer at least 32rem wide.
#[gpui_kit::test]
fn split_widths_clamp_to_range_and_window(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, ws) = open_main(cx, &world, &[]);
    cx.simulate_window_resize(handle, size(px(1600.), px(1000.)));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        ws.update(cx, |ws, cx| ws.set_left_width_rem(50.0, window, cx));
        assert_eq!(ws.read(cx).left_width_rem(), 40.0);
    })
    .unwrap();
    cx.simulate_window_resize(handle, size(px(880.), px(560.)));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let rem = f32::from(window.rem_size());
        let viewer = window.find("viewer-area").bounds();
        assert!(
            f32::from(viewer.size.width) >= 32.0 * rem - 0.5,
            "viewer {} px at rem {rem}",
            f32::from(viewer.size.width)
        );
        assert!(ws.read(cx).effective_left_width_rem(window) <= 55.0 - 32.0);
    })
    .unwrap();
}

/// X-036: the process environment of GUI tests points into a temp root.
#[test]
fn env_redirect_points_into_temp() {
    let root = redirect_root();
    assert!(root.starts_with(std::env::temp_dir()), "{}", root.display());
    for (name, _) in crate::test_support::REDIRECTED_VARS {
        let value = std::env::var_os(name).unwrap_or_else(|| panic!("{name} is set"));
        assert!(
            std::path::Path::new(&value).starts_with(root),
            "{name} = {}",
            value.to_string_lossy()
        );
    }
    for name in crate::test_support::REMOVED_VARS {
        assert!(std::env::var_os(name).is_none(), "{name} is removed");
    }
    let home = home::home_dir().expect("a home directory");
    assert!(
        home.starts_with(root),
        "home::home_dir() = {}",
        home.display()
    );
}

/// X-036: core's own guards are active in GUI test builds (core feature
/// `hermetic`, K13, enabled by `crates/gui/Cargo.toml` `[dev-dependencies]`):
/// the process environment is never read, the update check never reaches the
/// network, terminals are never spawned and the real embedding model never
/// loads.
#[test]
fn hermetic_guards_active() {
    use claude_history_core::HistoryEnv;
    use claude_history_core::resume::terminal::{
        BuildOptions, LaunchSpec, Platform, TerminalLauncher, build_terminal_command,
        spawn_terminal, spawn_terminal_checked,
    };

    fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_default()
    }

    let from_env = std::panic::catch_unwind(HistoryEnv::from_process_env)
        .expect_err("HistoryEnv::from_process_env panics in GUI tests");
    assert!(
        panic_text(from_env).contains("tests must use HistoryEnv::sandbox"),
        "from_process_env refuses with core's guard message"
    );

    let update =
        std::panic::catch_unwind(|| claude_history_core::update::check_latest_release("0.0.0"))
            .expect_err("update::check_latest_release panics in GUI tests");
    assert!(
        panic_text(update).contains("tests must not use the network"),
        "the update check refuses with core's guard message"
    );

    let root = tempfile::tempdir().unwrap();
    let spec = LaunchSpec {
        program: claude_history_core::program::ResolvedProgram {
            path: root.path().join("claude"),
            kind: claude_history_core::program::ProgramKind::Executable,
        },
        args: Vec::new(),
        cwd: Some(root.path().to_path_buf()),
    };
    let launcher = if cfg!(windows) {
        TerminalLauncher::WindowsConsole
    } else {
        TerminalLauncher::Template("true {args}".to_string())
    };
    let options = BuildOptions {
        platform: Platform::current(),
        keep_open: false,
        script_path: None,
    };
    let command = build_terminal_command(&launcher, &spec, &options).unwrap();
    assert_eq!(
        spawn_terminal(&command).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(
        spawn_terminal_checked(&command, Duration::from_millis(10))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );

    let env = HistoryEnv::sandbox(root.path());
    let model_dir = env.semantic_model_dir().unwrap();
    let mut factory = claude_history_core::semantic_ops::fastembed_factory(&env);
    assert!(factory().is_err(), "the default embedder factory refuses");
    let loader = claude_history_core::semantic::fastembed::FastembedEmbedder::new_in(
        model_dir.clone(),
        false,
    );
    assert!(loader.is_err(), "the shared embedder's loader refuses");
    assert!(!model_dir.exists(), "no model directory is created");
}

/// The quit state starts clear in a fresh app (sanity check of the probe).
#[gpui_kit::test]
fn quit_state_starts_clear(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.executor().advance_clock(Duration::from_millis(10));
    assert_eq!(cx.read(exit::quit_requested), None);
}

// ---------------------------------------------------------------------------
// Frozen infrastructure used by the other work packages
// ---------------------------------------------------------------------------

mod infrastructure {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;

    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

    use crate::actions::CommandPalette;
    use crate::deep_search::worker::AgentWorkerHandle;
    use crate::globals::{CorpusBus, CorpusNotice, OpenTargets, SemanticGate, last_toast};
    use crate::list::ListPaneEvent;
    use crate::model::{ConversationTarget, OpenPathsOptions, SemanticOp};
    use crate::services::{PlatformServices as _, ServiceCall};
    use crate::test_support::{TestWorld, open_main, open_single_file, recording};
    use crate::widgets::{ConfirmOptions, ConfirmOutcome, ReadOnlyText, open_destructive_confirm};

    fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    fn has_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
        cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap()
    }

    #[gpui_kit::test]
    fn destructive_confirm_ignores_enter_and_confirms_with_y(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        let confirmed = Rc::new(Cell::new(0));
        let closed = Rc::new(Cell::new(0));
        let control = {
            let confirmed = confirmed.clone();
            let closed = closed.clone();
            cx.update_window(handle, move |_, window, cx| {
                let options = ConfirmOptions::destructive("Delete \"Fix login bug\"?", "Delete")
                    .description("These will be removed from disk:")
                    .on_closed(move |_| closed.set(closed.get() + 1));
                open_destructive_confirm(options, window, cx, move |_, _, _| {
                    confirmed.set(confirmed.get() + 1);
                    ConfirmOutcome::Close
                })
            })
            .unwrap()
        };
        cx.run_until_parked();
        render(cx, handle);
        cx.simulate_keystrokes(handle, "enter");
        assert_eq!(
            confirmed.get(),
            0,
            "Enter never confirms a destructive action"
        );
        assert!(cx.read(|cx| control.is_open(cx)));
        assert!(has_dialog(cx, handle));
        cx.simulate_keystrokes(handle, "y");
        cx.run_until_parked();
        assert_eq!(confirmed.get(), 1);
        assert_eq!(closed.get(), 1);
        assert!(!cx.read(|cx| control.is_open(cx)));
        assert!(!has_dialog(cx, handle));
    }

    #[gpui_kit::test]
    fn destructive_confirm_cancels_with_n_and_shows_busy_state(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        let confirmed = Rc::new(Cell::new(0));
        let control = {
            let confirmed = confirmed.clone();
            cx.update_window(handle, move |_, window, cx| {
                open_destructive_confirm(
                    ConfirmOptions::destructive("Clear semantic cache?", "Clear"),
                    window,
                    cx,
                    move |_, _, _| {
                        confirmed.set(confirmed.get() + 1);
                        ConfirmOutcome::Close
                    },
                )
            })
            .unwrap()
        };
        cx.run_until_parked();
        render(cx, handle);
        cx.simulate_keystrokes(handle, "n");
        cx.run_until_parked();
        assert_eq!(confirmed.get(), 0);
        assert!(!cx.read(|cx| control.is_open(cx)));
        assert!(!has_dialog(cx, handle));

        // A handler that starts work keeps the dialog open and busy until closed.
        let busy = cx
            .update_window(handle, |_, window, cx| {
                open_destructive_confirm(
                    ConfirmOptions::destructive("Delete 2 empty transcripts?", "Delete 2"),
                    window,
                    cx,
                    |_, _, _| ConfirmOutcome::Busy,
                )
            })
            .unwrap();
        cx.run_until_parked();
        render(cx, handle);
        cx.simulate_keystrokes(handle, "shift-y");
        cx.run_until_parked();
        assert!(cx.read(|cx| busy.is_open(cx) && busy.is_busy(cx)));
        cx.update(|cx| busy.close(cx));
        cx.run_until_parked();
        assert!(!cx.read(|cx| busy.is_open(cx)));
        assert!(!has_dialog(cx, handle));
    }

    #[gpui_kit::test]
    fn non_destructive_confirm_accepts_enter(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        let confirmed = Rc::new(Cell::new(0));
        {
            let confirmed = confirmed.clone();
            cx.update_window(handle, move |_, window, cx| {
                open_destructive_confirm(
                    ConfirmOptions::non_destructive(
                        "Copy \"Fix login bug\" to C:\\git\\new?",
                        "Copy and fork",
                    ),
                    window,
                    cx,
                    move |_, _, _| {
                        confirmed.set(confirmed.get() + 1);
                        ConfirmOutcome::Close
                    },
                )
            })
            .unwrap();
        }
        cx.run_until_parked();
        render(cx, handle);
        cx.simulate_keystrokes(handle, "enter");
        cx.run_until_parked();
        assert_eq!(confirmed.get(), 1);
        assert!(!has_dialog(cx, handle));
    }

    /// The palette dispatches to the element focused before it opened.
    #[gpui_kit::test]
    fn palette_dispatches_to_the_stored_focus(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        cx.dispatch_action(handle, CommandPalette);
        render(cx, handle);
        assert!(has_dialog(cx, handle));
        cx.simulate_input(handle, "keyboard shortcuts");
        render(cx, handle);
        cx.simulate_keystrokes(handle, "enter");
        cx.run_until_parked();
        // WP6: the confirmed item ran: the Keyboard shortcuts dialog replaced
        // the palette (it was a "not available yet" toast in the skeleton).
        assert!(has_dialog(cx, handle));
        assert!(cx.read(crate::help::shortcuts::current).is_some());
        assert_ne!(
            cx.read(last_toast).as_ref(),
            "Keyboard shortcuts is not available yet"
        );
    }

    /// A deletion broadcast clears the viewer and closes dialogs on the path.
    #[gpui_kit::test]
    fn corpus_deletion_clears_viewer_and_closes_dialogs(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let path = super::claude_session(&world);
        let (handle, ws) = open_main(cx, &world, &[]);
        let list = cx.read(|cx| ws.read(cx).list().clone());
        list.update(cx, |_, cx| {
            cx.emit(ListPaneEvent::OpenRequested {
                target: ConversationTarget::from_parsed_file(&path, None),
                focus_viewer: true,
                agent_tab: None,
            })
        });
        cx.run_until_parked();
        // A dialog of the main window holding the path, closed the natural
        // way (`window.close_dialog`) by its close function.
        cx.update_window(handle, |_, window, cx| {
            window.open_dialog(cx, |dialog, _, _| dialog.title("Rename conversation"));
        })
        .unwrap();
        cx.run_until_parked();
        assert!(has_dialog(cx, handle));
        {
            let path = path.clone();
            cx.update(move |cx| {
                OpenTargets::register(
                    cx,
                    handle,
                    path,
                    "Fix login bug".into(),
                    Box::new(|window, cx| window.close_dialog(cx)),
                );
            });
        }
        // Broadcast from inside the main window's update, like a delete
        // dialog's completion does.
        cx.update_window(handle, |_, _, cx| {
            CorpusBus::broadcast(CorpusNotice::Deleted(vec![path.clone()]), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!has_dialog(cx, handle), "the dialog really closed");
        assert!(cx.read(|cx| ws.read(cx).viewer().read(cx).target().is_none()));
        assert_eq!(cx.read(OpenTargets::len), 0);
        assert_eq!(
            cx.read(last_toast).as_ref(),
            "Closed: \"Fix login bug\" was deleted"
        );
    }

    /// Without a main window (a FILE launch) a deletion still closes the
    /// dialogs holding the path and clears the single-file viewer.
    #[gpui_kit::test]
    fn corpus_deletion_reaches_single_file_windows(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let path = super::claude_session(&world);
        let handle = open_single_file(cx, &world, &path);
        cx.update_window(handle, |_, window, cx| {
            window.open_dialog(cx, |dialog, _, _| dialog.title("Debug log"));
        })
        .unwrap();
        cx.run_until_parked();
        {
            let path = path.clone();
            cx.update(move |cx| {
                OpenTargets::register(
                    cx,
                    handle,
                    path,
                    "Fix login bug".into(),
                    Box::new(|window, cx| window.close_dialog(cx)),
                );
            });
        }
        cx.update(|cx| CorpusBus::broadcast(CorpusNotice::Deleted(vec![path.clone()]), cx));
        cx.run_until_parked();
        assert!(!has_dialog(cx, handle));
        let viewer = super::single_file_viewer(cx, handle);
        assert!(cx.read(|cx| viewer.read(cx).target().is_none()));
        assert_eq!(
            cx.read(last_toast).as_ref(),
            "Closed: \"Fix login bug\" was deleted"
        );
    }

    /// `close_then` runs its follow-up after the confirmation is gone, so a
    /// follow-up dialog is never the one that gets closed.
    #[gpui_kit::test]
    fn confirm_close_then_opens_the_follow_up_after_closing(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        let control = cx
            .update_window(handle, |_, window, cx| {
                open_destructive_confirm(
                    ConfirmOptions::destructive("Replace \"x.md\"?", "Replace"),
                    window,
                    cx,
                    |control, _, cx| {
                        control.close_then(cx, |window, cx| {
                            window.open_dialog(cx, |dialog, _, _| dialog.title("Follow-up"));
                        });
                        ConfirmOutcome::KeepOpen
                    },
                )
            })
            .unwrap();
        cx.run_until_parked();
        render(cx, handle);
        cx.simulate_keystrokes(handle, "y");
        cx.run_until_parked();
        assert!(!cx.read(|cx| control.is_open(cx)));
        assert!(has_dialog(cx, handle), "the follow-up dialog stays open");
        cx.update_window(handle, |_, window, cx| window.close_dialog(cx))
            .unwrap();
        cx.run_until_parked();
        assert!(!has_dialog(cx, handle), "only the follow-up was left");
    }

    /// The gate pauses the agent worker itself: it holds in a FILE-launched
    /// session (no main window, no Deep search pane) too.
    #[gpui_kit::test]
    fn semantic_gate_pauses_the_worker_without_a_main_window(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let path = super::claude_session(&world);
        let _handle = open_single_file(cx, &world, &path);
        cx.update(|cx| {
            assert!(crate::globals::main_panes(cx).deep().is_none());
            assert!(SemanticGate::enter_exclusive(SemanticOp::Clear, cx));
            assert!(AgentWorkerHandle::global(cx).is_semantic_paused());
            SemanticGate::leave(cx);
            assert!(!AgentWorkerHandle::global(cx).is_semantic_paused());
        });
    }

    #[gpui_kit::test]
    fn semantic_gate_is_exclusive(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (_handle, _ws) = open_main(cx, &world, &[]);
        cx.update(|cx| {
            assert!(SemanticGate::is_open(cx));
            assert!(SemanticGate::enter_exclusive(SemanticOp::Generate, cx));
            assert!(!SemanticGate::enter_exclusive(SemanticOp::Clear, cx));
            assert!(AgentWorkerHandle::global(cx).is_semantic_paused());
            SemanticGate::leave(cx);
            assert!(SemanticGate::is_open(cx));
            assert!(!AgentWorkerHandle::global(cx).is_semantic_paused());
        });
    }

    #[gpui_kit::test]
    fn recording_services_answer_prompts_and_record_calls(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        let services = recording(cx);
        let answer = PathBuf::from("C:/sbx/out/x.md");
        services.answer_save(Ok(Some(answer.clone())));
        type Answers = Rc<RefCell<Vec<Result<Option<PathBuf>, String>>>>;
        let got: Answers = Rc::new(RefCell::new(Vec::new()));
        {
            let services = services.clone();
            let got = got.clone();
            cx.update_window(handle, move |_, window, cx| {
                for _ in 0..2 {
                    let got = got.clone();
                    services.prompt_save_path(
                        std::path::Path::new("C:/sbx/out"),
                        "x.md",
                        window,
                        cx,
                        Box::new(move |answer, _, _| got.borrow_mut().push(answer)),
                    );
                }
                services.prompt_open_paths(
                    OpenPathsOptions {
                        files: true,
                        directories: false,
                        multiple: true,
                        prompt: "Open".into(),
                    },
                    window,
                    cx,
                    Box::new(|answer, _, _| assert_eq!(answer, Ok(None))),
                );
                services.clipboard_write("3f2a19c0".to_string(), cx);
            })
            .unwrap();
        }
        cx.run_until_parked();
        assert_eq!(*got.borrow(), vec![Ok(Some(answer)), Ok(None)]);
        assert_eq!(services.clipboard().as_deref(), Some("3f2a19c0"));
        let calls = services.calls();
        assert!(matches!(calls[0], ServiceCall::PromptSave { .. }));
        assert!(matches!(calls[2], ServiceCall::PromptOpen(_)));
        assert_eq!(
            calls[3],
            ServiceCall::ClipboardWrite("3f2a19c0".to_string())
        );
        assert!(services.write_stdout("x").is_err(), "no stdout by default");
        assert_eq!(
            services.calls().last(),
            Some(&ServiceCall::StdoutFailed("x".to_string())),
            "the failed attempt is recorded"
        );
        assert_eq!(services.stdout(), "");
        services.set_stdout_kind(crate::model::StdoutKind::Redirected);
        assert!(services.write_stdout("y\n").is_ok());
        assert_eq!(services.stdout(), "y\n");
    }

    #[gpui_kit::test]
    fn read_only_text_holds_text(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        let text = cx
            .update_window(handle, |_, window, cx| {
                let view = ReadOnlyText::new_in(window, cx);
                view.set_text("line 1\nline 2", window, cx);
                view
            })
            .unwrap();
        assert_eq!(cx.read(|cx| text.text(cx)).as_ref(), "line 1\nline 2");
    }
}

/// Integrator wiring of the Phase-A merge (WP7 + WP6 + WP1 + WP2): requested
/// changes applied across package boundaries (DESIGN §17.11).
mod phase_a {
    use std::time::{Duration, Instant};

    use gpui_kit::{BorrowAppContext as _, TestAppContext, WindowBounds};

    use crate::globals::{AppState, app_state, update_app_state};
    use crate::persist::{self, PlacementBounds, WindowPlacement, WindowPlacementKind};
    use crate::test_support::{TestWorld, open_main, open_single_file, synth};
    use crate::workspace::{Workspace, main_window_options};

    fn wait_idle(cx: &mut TestAppContext, ws: &gpui_kit::Entity<Workspace>) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            cx.run_until_parked();
            let busy = cx.read(|cx| {
                let list = ws.read(cx).list().read(cx);
                list.is_loading() || list.is_refreshing()
            });
            if !busy {
                return;
            }
            assert!(Instant::now() < deadline, "the list did not settle in 30 s");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// C-037 (WP6 request): the main window opens with the remembered
    /// placement when `restore_session` is on, else with the default size;
    /// the minimum size stays either way.
    #[gpui_kit::test]
    fn main_window_options_restore_the_remembered_placement(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (_handle, _ws) = open_main(cx, &world, &[]);
        let default = cx.read(main_window_options);
        assert!(matches!(
            default.window_bounds,
            Some(WindowBounds::Windowed(_))
        ));
        assert!(default.window_min_size.is_some());
        cx.update(|cx| {
            persist::update_ui(cx, |ui| {
                ui.main_window = Some(WindowPlacement {
                    kind: WindowPlacementKind::Maximized,
                    bounds: PlacementBounds {
                        x: 10.0,
                        y: 10.0,
                        w: 1000.0,
                        h: 700.0,
                    },
                });
            })
        });
        let restored = cx.read(main_window_options);
        assert!(
            matches!(restored.window_bounds, Some(WindowBounds::Maximized(_))),
            "the remembered maximized placement is re-applied"
        );
        assert_eq!(restored.window_min_size, default.window_min_size);
        cx.update(|cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.config.gui.restore_session = Some(false)
            })
        });
        let off = cx.read(main_window_options);
        assert!(matches!(off.window_bounds, Some(WindowBounds::Windowed(_))));
    }

    /// L-082, PD3 rule 3 (c) (WP7 request): a launch directory recorded as
    /// the cwd of a loaded Pi session becomes the workspace after the first
    /// load when no other rule chose one, and the list is re-scoped.
    #[gpui_kit::test]
    fn a_recorded_pi_cwd_becomes_the_workspace_after_the_first_load(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let project = world.home().join("pi-only-project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd_json = serde_json::to_string(&project.display().to_string()).unwrap();
        let session = synth::PI_SESSION.replace(r#""/tmp/pi-project""#, &cwd_json);
        let pi = world.add_fixture(
            ".pi/agent/sessions/--pi-only-project--/2024-03-01T00-00-00-000Z_01912345-6789-7abc-8def-0123456789ab.jsonl",
            session.as_bytes(),
        );
        // Initialise the app without a main window, then pretend it was
        // launched from `project` (the harness launches from the home).
        let _single = open_single_file(cx, &world, &pi);
        cx.update(|cx| {
            update_app_state(cx, |state, _| {
                state.launch.launch_cwd = Some(project.clone());
                state.set_workspace(None);
            })
        });
        let (_handle, ws) = open_main(cx, &world, &[]);
        wait_idle(cx, &ws);
        let canonical = std::fs::canonicalize(&project).unwrap();
        let chosen = cx.read(|cx| {
            app_state(cx)
                .workspace
                .as_ref()
                .map(|scope| std::fs::canonicalize(scope.dir()).unwrap())
        });
        assert_eq!(chosen, Some(canonical));
        assert!(cx.read(|cx| ws.read(cx).list().read(cx).has_workspace()));
    }

    /// DESIGN §15.3 `dialog` probe (WP6 + WP7 requests): WP6's dialog ids,
    /// then the id an owner registered, else `other`.
    #[cfg(feature = "qa")]
    #[gpui_kit::test]
    fn the_dialog_probe_names_open_dialogs(cx: &mut TestAppContext) {
        use gpui_kit::AppContext as _;
        use gpui_kit::component::WindowExt as _;

        let world = TestWorld::new();
        let (handle, _ws) = open_main(cx, &world, &[]);
        let probe = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                crate::qa::probe::read("dialog", window, cx)
            })
            .unwrap()
        };
        assert_eq!(probe(cx).as_deref(), Some("none"));
        cx.update_window(handle, |_, window, cx| {
            crate::help::open_shortcuts(None, window, cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(probe(cx).as_deref(), Some("shortcuts"));
        cx.update_window(handle, |_, window, cx| window.close_dialog(cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(probe(cx).as_deref(), Some("none"));
        cx.update_window(handle, |_, window, cx| {
            window.open_alert_dialog(cx, |alert, _, _| alert.title("Rename"));
            crate::workspace::startup::register_dialog(cx, "rename");
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(probe(cx).as_deref(), Some("rename"));
        cx.update_window(handle, |_, window, cx| {
            window.close_dialog(cx);
            cx.set_global(crate::workspace::startup::LastDialog::default());
            window.open_alert_dialog(cx, |alert, _, _| alert.title("Something"));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(probe(cx).as_deref(), Some("other"));
    }

    /// §15.3 probes reach a single-file window's root (WP2's
    /// `SingleFileWindow::qa_probe`) and name the window kind.
    #[cfg(feature = "qa")]
    #[gpui_kit::test]
    fn probes_reach_single_file_windows(cx: &mut TestAppContext) {
        use gpui_kit::AppContext as _;

        let world = TestWorld::new();
        let lines = synth::claude_conversation(
            &synth::uuid(7),
            &synth::cwd_base().join("proj"),
            "Probe me",
            1,
            1,
        );
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = world.add_claude_session("C--synthetic-proj", &synth::uuid(7), &refs);
        let single = open_single_file(cx, &world, &path);
        let (main, _ws) = open_main(cx, &world, &[]);
        cx.run_until_parked();
        let read = |cx: &mut TestAppContext, handle, key: &str| {
            cx.update_window(handle, |_, window, cx| {
                crate::qa::probe::read(key, window, cx)
            })
            .unwrap()
        };
        assert_eq!(
            read(cx, single, "window.kind").as_deref(),
            Some("single-file")
        );
        assert_eq!(read(cx, main, "window.kind").as_deref(), Some("main"));
        let shown = read(cx, single, "viewer.path").unwrap_or_default();
        assert!(shown.contains(&synth::uuid(7)), "viewer.path = {shown:?}");
    }
}
