//! Tests of the frozen cross-WP contracts: preview flush before list
//! actions, open requests with an agent tab, session routing from the agent
//! panel, menu enabled state per window, region keys vs toolbar buttons,
//! startup key validation, help rows, region cycling, the clipboard guard.

use claude_history_core::config::{KeyBindings, parse_key_binding};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    Action, AnyWindowHandle, AppContext as _, DummyKeyboardMapper, Focusable as _, KeyBinding,
    ParentElement as _, TestAppContext,
};

use crate::actions::*;
use crate::deep_search::worker::AgentWorkerHandle;
use crate::globals::debug_buffer;
use crate::keymap;
use crate::list::ListPaneEvent;
use crate::model::{AgentTab, ConversationTarget, LeftTab, PaneFocus, RowMenuItem};
use crate::services::Services;
use crate::test_support::{TestWorld, open_main, open_single_file, recording};
use crate::workspace::routing::{self, SessionRoute};

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn target(path: &std::path::Path) -> ConversationTarget {
    ConversationTarget::from_parsed_file(path, None)
}

/// L-085 (frozen hook): a list action within the debounce shows the pending
/// preview first, so the viewer shows what the action targets.
#[gpui_kit::test]
fn a_list_action_flushes_the_pending_preview_first(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let first = super::claude_session(&world);
    let second = super::second_session(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    // The viewer shows the first conversation; the selection moves on.
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: target(&first),
            focus_viewer: false,
            agent_tab: None,
        })
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        list.update(cx, |list, cx| list.focus_search(window, cx));
    })
    .unwrap();
    render(cx, handle);
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::SelectionChanged {
            target: Some(target(&second)),
            user: true,
        })
    });
    cx.run_until_parked();
    assert!(cx.read(|cx| ws.read(cx).has_pending_preview()));
    let shown = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            ws.read(cx)
                .viewer()
                .read(cx)
                .target()
                .map(|target| target.path.clone())
        })
    };
    assert_eq!(shown(cx), Some(first.clone()), "debounced, not shown yet");
    // Ctrl+R within the debounce (no clock advance): the capture hook shows
    // the new row before the list handles Resume.
    cx.simulate_keystrokes(handle, "ctrl-r");
    assert_eq!(shown(cx), Some(second.clone()));
    assert!(!cx.read(|cx| ws.read(cx).has_pending_preview()));
}

/// D-029 route: an open request with an agent tab opens the conversation
/// and the panel on that tab, focused.
#[gpui_kit::test]
fn an_open_request_with_an_agent_tab_opens_the_panel(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = super::claude_session(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: target(&path),
            focus_viewer: true,
            agent_tab: Some(AgentTab::Within),
        })
    });
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        let viewer = ws.read(cx).viewer().clone();
        assert_eq!(
            viewer.read(cx).target().map(|t| t.path.clone()),
            Some(path.clone())
        );
        let panel = viewer.read(cx).agent_panel().clone();
        assert!(panel.read(cx).is_visible());
        assert_eq!(panel.read(cx).tab(), AgentTab::Within);
        assert!(panel.read(cx).contains_focus(window, cx));
    })
    .unwrap();
}

/// The routing table: what a pane does with each kind of session action.
#[gpui_kit::test]
fn session_routes_cover_the_viewer_needing_actions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = super::claude_session(&world);
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| {
        let options = routing::default_export_options(cx);
        let route = |action: &dyn Action, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
            routing::run_session_action(action, target(&path), options, window, cx)
        };
        assert_eq!(
            route(&OpenConversation, window, cx),
            SessionRoute::Open { agent_tab: None }
        );
        assert_eq!(
            route(&SearchWithinConversation, window, cx),
            SessionRoute::Open {
                agent_tab: Some(AgentTab::Within)
            }
        );
        assert_eq!(
            route(&ToggleOutline, window, cx),
            SessionRoute::Open {
                agent_tab: Some(AgentTab::Outline)
            }
        );
        assert_eq!(
            route(&ReadRange, window, cx),
            SessionRoute::Open {
                agent_tab: Some(AgentTab::Read)
            }
        );
        assert_eq!(route(&CopyFocusedMessage, window, cx), SessionRoute::Viewer);
        assert_eq!(route(&CopyPath, window, cx), SessionRoute::Done);
        assert_eq!(route(&Refresh, window, cx), SessionRoute::NotSession);
    })
    .unwrap();
    for (item, name) in [
        (RowMenuItem::Open, "history::OpenConversation"),
        (
            RowMenuItem::SearchWithin,
            "history::SearchWithinConversation",
        ),
        (RowMenuItem::Outline, "history::ToggleOutline"),
        (RowMenuItem::Reveal, "history::RevealInFileManager"),
        (RowMenuItem::Delete, "history::Delete"),
    ] {
        assert_eq!(routing::row_menu_action(item).name(), name);
    }
}

/// §7.8/PD15: with the agent panel focused, session actions act on the
/// panel's result-set target (not the viewer's); Esc returns to the
/// transcript and keeps the panel open (§8.3).
#[gpui_kit::test]
fn agent_panel_session_actions_use_the_panel_target(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let shown = super::claude_session(&world);
    let other = super::second_session(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: target(&shown),
            focus_viewer: true,
            agent_tab: Some(AgentTab::Outline),
        })
    });
    cx.run_until_parked();
    let viewer = cx.read(|cx| ws.read(cx).viewer().clone());
    let panel = cx.read(|cx| viewer.read(cx).agent_panel().clone());
    // The panel's result set belongs to another conversation.
    panel.update(cx, |panel, cx| {
        panel.set_viewer_target(Some(target(&other)), cx)
    });
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert!(panel.read(cx).contains_focus(window, cx));
    })
    .unwrap();
    assert_eq!(
        cx.read(|cx| crate::globals::ActivePane::of_window(cx, handle)),
        PaneFocus::AgentPanel
    );
    cx.dispatch_action(handle, SearchWithinConversation);
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            viewer.read(cx).target().map(|t| t.path.clone()),
            Some(other.clone()),
            "the panel's target, not the viewer's"
        );
        assert_eq!(panel.read(cx).tab(), AgentTab::Within);
        assert!(panel.read(cx).contains_focus(window, cx));
    })
    .unwrap();
    cx.simulate_keystrokes(handle, "escape");
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert!(panel.read(cx).is_visible(), "Esc keeps the panel open");
        assert!(!panel.read(cx).contains_focus(window, cx));
        assert!(viewer.read(cx).contains_focus(window, cx));
    })
    .unwrap();
}

/// §8.10: menu items are enabled exactly where a listener exists — Quit
/// everywhere, Close window only in single-file windows, Resume/Fork/
/// Rename/Delete/Select and exit never in single-file windows (PD14).
#[gpui_kit::test]
fn menu_availability_follows_the_window(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = super::claude_session(&world);
    let (main, _ws) = open_main(cx, &world, &[]);
    render(cx, main);
    cx.update_window(main, |_, window, cx| {
        assert!(window.is_action_available(&Quit, cx));
        assert!(window.is_action_available(&OpenJsonlFile, cx));
        assert!(window.is_action_available(&Resume, cx));
        assert!(window.is_action_available(&CopyPath, cx));
        assert!(
            !window.is_action_available(&CloseWindow, cx),
            "File → Close window is for single-file windows"
        );
        assert!(!window.is_action_available(&ShowConversations, cx));
    })
    .unwrap();
    let single = open_single_file(cx, &world, &path);
    render(cx, single);
    cx.update_window(single, |_, window, cx| {
        let unavailable: [Box<dyn Action>; 6] = [
            Box::new(Resume),
            Box::new(Fork),
            Box::new(Rename),
            Box::new(Delete),
            Box::new(SelectAndExit),
            Box::new(Refresh),
        ];
        for action in unavailable {
            assert!(
                !window.is_action_available(action.as_ref(), cx),
                "{} is disabled in a single-file window",
                action.name()
            );
        }
        let available: [Box<dyn Action>; 8] = [
            Box::new(Quit),
            Box::new(CloseWindow),
            Box::new(ShowConversations),
            Box::new(OpenJsonlFile),
            Box::new(CopyPath),
            Box::new(ExportConversation),
            Box::new(ToggleOutline),
            Box::new(CommandPalette),
        ];
        for action in available {
            assert!(
                window.is_action_available(action.as_ref(), cx),
                "{} is enabled in a single-file window",
                action.name()
            );
        }
    })
    .unwrap();
}

/// PD5: picker mode opens no extra windows, so Open JSONL file is disabled.
#[gpui_kit::test]
fn picker_mode_disables_open_jsonl_file(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &["--show-path"]);
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert!(!window.is_action_available(&OpenJsonlFile, cx));
        assert!(window.is_action_available(&SelectAndExit, cx));
    })
    .unwrap();
}

/// The list's keys never reach a focused toolbar button: Tab moves focus
/// on instead of toggling the scope, Esc does not clear the query.
#[gpui_kit::test]
fn list_keys_leave_toolbar_buttons_alone(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let home = world.home().display().to_string();
    let (handle, ws) = open_main(cx, &world, &["--workspace", &home]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    assert!(cx.read(|cx| list.read(cx).has_workspace()));
    cx.simulate_input(handle, "abc");
    cx.simulate_keystrokes(handle, "tab");
    assert!(
        cx.read(|cx| list.read(cx).scope_is_workspace()),
        "Tab in the search toggles the scope"
    );
    cx.update_window(handle, |_, window, cx| {
        list.update(cx, |list, cx| list.focus_toolbar(window, cx))
    })
    .unwrap();
    render(cx, handle);
    cx.simulate_keystrokes(handle, "escape");
    assert_eq!(cx.read(|cx| list.read(cx).query(cx)).as_ref(), "abc");
    cx.simulate_keystrokes(handle, "tab");
    assert!(
        cx.read(|cx| list.read(cx).scope_is_workspace()),
        "Tab in the toolbar does not toggle the scope"
    );
    // Back in the search input the list keys work again.
    cx.update_window(handle, |_, window, cx| {
        list.update(cx, |list, cx| list.focus_search(window, cx))
    })
    .unwrap();
    cx.simulate_keystrokes(handle, "escape");
    assert_eq!(cx.read(|cx| list.read(cx).query(cx)).as_ref(), "");
}

/// §8.5 at startup: a reserved key from config.toml is not bound (the
/// default stays), the debug log says so, and Ctrl+C in the search copies.
#[gpui_kit::test]
fn startup_rejects_reserved_user_keys(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.write_config(Some("[keys]\nresume = \"ctrl+c\"\n"), None);
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        let installed = cx
            .try_global::<keymap::UserBindingSet>()
            .and_then(|set| set.keys);
        assert_eq!(installed, Some(KeyBindings::default()));
        let lines = debug_buffer(cx).lines();
        assert!(
            lines.iter().any(|line| line.text.contains("resume")
                && line.text.contains("reserved for clipboard and editing")),
            "{lines:?}"
        );
    });
    cx.simulate_input(handle, "abc");
    cx.simulate_keystrokes(handle, "secondary-a secondary-c");
    assert_eq!(recording(cx).clipboard().as_deref(), Some("abc"));
    // Settings → Keys refuses the same key live.
    let result = cx.update(|cx| {
        keymap::install_user_bindings(
            cx,
            &KeyBindings {
                resume: parse_key_binding("ctrl+v").unwrap(),
                ..KeyBindings::default()
            },
        )
    });
    assert!(result.unwrap_err().contains("reserved"));
}

fn key_label(keystroke: &str) -> String {
    let binding = KeyBinding::load(
        keystroke,
        Box::new(SelectPrev),
        None,
        false,
        None,
        &DummyKeyboardMapper,
    )
    .unwrap();
    binding
        .keystrokes()
        .iter()
        .map(|keystroke| Kbd::format(keystroke.inner()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// §8.7: help rows keep the TUI key pairs and their window-specific labels.
#[gpui_kit::test]
fn help_rows_show_key_pairs(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let row = |label: &str, first: &str| {
        keymap::help_rows()
            .iter()
            .find(|row| row.label == label && row.keys[0].keystroke == Some(first))
            .unwrap_or_else(|| panic!("{label}"))
    };
    cx.update(|cx| {
        let pairs = [
            ("Move selection", "up", "down"),
            ("Move selection", "ctrl-p", "ctrl-n"),
            ("Jump by page", "pageup", "pagedown"),
            ("Jump to first/last", "home", "end"),
            ("Next / prev match", "n", "shift-n"),
            ("Back to list", "q", "escape"),
            ("Word left/right", "alt-b", "alt-f"),
        ];
        for (label, a, b) in pairs {
            assert_eq!(
                keymap::help_keys(row(label, a), cx).as_ref(),
                format!("{} / {}", key_label(a), key_label(b)),
                "{label}"
            );
        }
        let back = row("Back to list", "q");
        assert_eq!(back.label_for(true), "Close");
        assert_eq!(back.label_for(false), "Back to list");
        let toggle = row("Toggle semantic search", "ctrl-t");
        assert!(keymap::help_row_visible(toggle, false, cx));
        assert!(!keymap::help_row_visible(toggle, true, cx));
        let resume = keymap::help_rows()
            .iter()
            .find(|row| row.label == "Resume" && row.section == crate::model::HelpSection::Viewer)
            .unwrap();
        assert!(!keymap::help_row_visible(resume, true, cx));
        assert_eq!(keymap::help_keys(resume, cx).as_ref(), key_label("ctrl-r"));
        keymap::install_user_bindings(
            cx,
            &KeyBindings {
                refresh: parse_key_binding("ctrl+t").unwrap(),
                ..KeyBindings::default()
            },
        )
        .unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let toggle = row("Toggle semantic search", "ctrl-t");
        assert!(
            !keymap::help_row_visible(toggle, false, cx),
            "Ctrl+T is listed only while it toggles the mode"
        );
        let details = row("Semantic details", "ctrl-s");
        assert!(!keymap::help_row_visible(details, false, cx));
    });
}

/// AD16 / C-058 (frozen regions): search → toolbar → viewer → agent panel
/// (when open) → search; Shift+F6 reverses.
#[gpui_kit::test]
fn f6_cycles_through_the_list_toolbar(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = super::claude_session(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let region = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            ws.read(cx).focused_region(window, cx)
        })
        .unwrap()
    };
    assert_eq!(region(cx), PaneFocus::List);
    cx.simulate_keystrokes(handle, "f6");
    assert_eq!(region(cx), PaneFocus::ListToolbar);
    cx.simulate_keystrokes(handle, "f6");
    assert_eq!(region(cx), PaneFocus::Viewer);
    cx.simulate_keystrokes(handle, "f6");
    assert_eq!(region(cx), PaneFocus::List, "no agent panel yet");
    // With the agent panel open it joins the cycle.
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: target(&path),
            focus_viewer: false,
            agent_tab: Some(AgentTab::Outline),
        })
    });
    cx.run_until_parked();
    assert_eq!(region(cx), PaneFocus::AgentPanel);
    cx.simulate_keystrokes(handle, "f6");
    assert_eq!(region(cx), PaneFocus::List);
    cx.simulate_keystrokes(handle, "shift-f6");
    assert_eq!(region(cx), PaneFocus::AgentPanel);
    cx.simulate_keystrokes(handle, "shift-f6");
    assert_eq!(region(cx), PaneFocus::Viewer);
}

/// L-087 / §7.2: leaving the viewer goes back to the conversation list's
/// search input, also when the Deep search tab was active.
#[gpui_kit::test]
fn leaving_the_viewer_returns_to_the_list_search(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = super::claude_session(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: target(&path),
            focus_viewer: false,
            agent_tab: None,
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes(handle, "secondary-2");
    cx.simulate_keystrokes(handle, "secondary-3");
    assert_eq!(cx.read(|cx| ws.read(cx).left_tab()), LeftTab::DeepSearch);
    cx.simulate_keystrokes(handle, "q");
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(ws.read(cx).left_tab(), LeftTab::Conversations);
        assert!(list.focus_handle(cx).is_focused(window));
    })
    .unwrap();
}

/// PD10/§15.3: with recording services, gpui-kit's own Copy/Cut/Paste in
/// any input (a dialog's included) use the in-memory clipboard; the
/// platform clipboard is never touched.
#[gpui_kit::test]
fn the_clipboard_guard_routes_input_clipboard_actions(cx: &mut TestAppContext) {
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
        input.focus_handle(cx).focus(window, cx)
    })
    .unwrap();
    render(cx, handle);
    cx.simulate_input(handle, "hello");
    cx.simulate_keystrokes(handle, "secondary-a secondary-c");
    let services = recording(cx);
    assert_eq!(services.clipboard().as_deref(), Some("hello"));
    services.set_clipboard(Some("pasted\ntext".to_string()));
    cx.simulate_keystrokes(handle, "secondary-a secondary-v");
    assert_eq!(cx.read(|cx| input.read(cx).value()).as_ref(), "pastedtext");
    cx.simulate_keystrokes(handle, "secondary-a secondary-x");
    assert_eq!(services.clipboard().as_deref(), Some("pastedtext"));
    assert_eq!(cx.read(|cx| input.read(cx).value()).as_ref(), "");
    assert!(
        cx.read_from_clipboard().is_none(),
        "the platform clipboard was never written"
    );
}

/// §15.3: a QA run records every side effect; any other run is real.
#[test]
fn services_follow_the_launch() {
    let launch = crate::model::LaunchContext::default();
    assert!(!Services::for_launch(&launch).is_recording());
    #[cfg(feature = "qa")]
    {
        let launch = crate::model::LaunchContext {
            qa: Some(crate::model::QaArgs::default()),
            ..crate::model::LaunchContext::default()
        };
        assert!(Services::for_launch(&launch).is_recording());
    }
}

/// Every consumer of the agent worker gets its own reply channel.
#[gpui_kit::test]
fn agent_clients_have_their_own_reply_channels(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let worker = AgentWorkerHandle::global(cx);
        let a = worker.client();
        let b = worker.client();
        assert_ne!(a.id(), b.id());
        assert!(a.replies().is_empty() && b.replies().is_empty());
        assert!(!a.reply_sender().same_channel(&b.reply_sender()));
    });
}

/// `ensure_app` refuses to reopen an app with other arguments instead of
/// silently ignoring them.
#[gpui_kit::test]
#[should_panic(expected = "cannot reopen it")]
fn the_harness_refuses_conflicting_arguments(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = super::claude_session(&world);
    let _single = open_single_file(cx, &world, &path);
    let _main = open_main(cx, &world, &["--lexical"]);
}

/// §7.8: with Deep search focused, Session → Outline acts on the selected
/// hit's conversation (none in the stub), never on the viewer's.
#[gpui_kit::test]
fn deep_search_session_actions_use_the_selected_hit(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = super::claude_session(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: target(&path),
            focus_viewer: false,
            agent_tab: None,
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes(handle, "secondary-2");
    render(cx, handle);
    assert_eq!(
        cx.read(|cx| crate::globals::ActivePane::of_window(cx, handle)),
        PaneFocus::DeepSearch
    );
    cx.dispatch_action(handle, ToggleOutline);
    render(cx, handle);
    let panel_visible = cx.read(|cx| {
        ws.read(cx)
            .viewer()
            .read(cx)
            .agent_panel()
            .read(cx)
            .is_visible()
    });
    assert!(!panel_visible, "the viewer's outline was not toggled");
    assert_eq!(
        cx.read(crate::globals::last_toast).as_ref(),
        "No conversation selected"
    );
}
