//! The integrator's Phase-B wiring (DESIGN §17.11 "Phase B as built"):
//! `$TERMINAL` from `main` to the launcher detection, toast buttons that
//! tests can press, the list's indexed lookups.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{AppContext as _, TestAppContext};

use crate::notify::{Toast, last_toast_actions, trigger_last_toast_action};
use crate::ops::resume::TerminalVar;
use crate::services::RecordingServices;
use crate::test_support::{TestWorld, launch_for_args, open_main, synth};

fn wait_idle(cx: &mut TestAppContext, ws: &gpui_kit::Entity<crate::workspace::Workspace>) {
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

/// WP3 request (PD4, §11.10 step 6, §14.1): the `$TERMINAL` value `main`
/// read reaches `ops::resume` through the launch context and `app::init`.
#[gpui_kit::test]
fn the_terminal_variable_main_read_reaches_the_launcher_detection(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let mut launch = launch_for_args(&world, &[]);
    launch.terminal_var = Some(OsString::from("kitty"));
    cx.update(|cx| {
        crate::app::init_for_test(cx, &world, launch, Arc::new(RecordingServices::new()))
    });
    let value = cx.read(|cx| cx.try_global::<TerminalVar>().and_then(|var| var.0.clone()));
    assert_eq!(value, Some(OsString::from("kitty")));
}

/// WP3 request (A-010, A-061): a toast records its buttons (the action
/// button first) and a test presses one by its label.
#[gpui_kit::test]
fn toast_buttons_are_recorded_and_can_be_pressed(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let pressed = std::rc::Rc::new(std::cell::Cell::new(0));
    let (a, b) = (pressed.clone(), pressed.clone());
    cx.update_window(handle, move |_, window, cx| {
        Toast::info("Projects directory: x")
            .action("Retry", move |_, _| a.set(a.get() + 1))
            .button("Copy", move |_, _| b.set(b.get() + 10))
            .show(window, cx);
    })
    .unwrap();
    let labels: Vec<String> = cx
        .read(last_toast_actions)
        .iter()
        .map(|label| label.to_string())
        .collect();
    assert_eq!(labels, ["Retry", "Copy"]);
    let copied = cx
        .update_window(handle, |_, window, cx| {
            trigger_last_toast_action("Copy", window, cx)
        })
        .unwrap();
    assert!(copied);
    assert_eq!(pressed.get(), 10);
    let missing = cx
        .update_window(handle, |_, window, cx| {
            trigger_last_toast_action("Reveal", window, cx)
        })
        .unwrap();
    assert!(!missing);
    // A toast without buttons clears them.
    cx.update_window(handle, |_, window, cx| {
        crate::notify::info(window, cx, "Done")
    })
    .unwrap();
    assert!(cx.read(last_toast_actions).is_empty());
}

/// WP-C2 request (X-038): the list finds a conversation and its row through
/// core's path index, also for the same file spelled differently on Windows.
#[gpui_kit::test]
fn the_list_finds_conversations_by_path(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 30);
    let (_handle, ws) = open_main(cx, &world, &[]);
    wait_idle(cx, &ws);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    let paths: Vec<std::path::PathBuf> = cx.read(|cx| {
        list.read(cx)
            .browser()
            .conversations()
            .iter()
            .map(|conversation| conversation.path.clone())
            .collect()
    });
    assert_eq!(paths.len(), 30);
    let last = paths.last().unwrap().clone();
    assert!(cx.read(|cx| list.read(cx).conversation(&last).is_some()));
    assert!(cx.update(|cx| list.update(cx, |list, cx| list.select_path(&last, cx))));
    assert_eq!(
        cx.read(|cx| list.read(cx).selected_target().map(|target| target.path)),
        Some(last.clone())
    );
    if cfg!(windows) {
        let upper = std::path::PathBuf::from(last.display().to_string().to_uppercase());
        assert!(cx.read(|cx| list.read(cx).conversation(&upper).is_some()));
    }
    let missing = world.home().join("missing.jsonl");
    assert!(cx.read(|cx| list.read(cx).conversation(&missing).is_none()));
    assert!(!cx.update(|cx| list.update(cx, |list, cx| list.select_path(&missing, cx))));
}

/// viewer-basics.qa (V-097) on a file outside the history roots (D-035):
/// `o` shows the agent panel with its message and keeps the focus in it, `o`
/// again hides it and the transcript has the focus (integrator: the real
/// panel replaced the tab body under the focus).
#[gpui_kit::test]
fn the_outline_key_toggles_the_panel_on_a_file_outside_the_roots(cx: &mut TestAppContext) {
    use crate::deep_search::panel::KeyState;
    use crate::test_support::fixtures_agent::{agent_corpus, copy_outside_roots};
    use gpui_kit::test::TestWindowExt as _;

    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let outside = copy_outside_roots(&world, &corpus.retry);
    let handle = crate::test_support::open_single_file(cx, &world, &outside);
    let viewer = super::single_file_viewer(cx, handle);
    let panel = cx.read(|cx| viewer.read(cx).agent_panel().clone());
    let render = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    };
    // Wait for the viewer to show the file.
    let deadline = Instant::now() + Duration::from_secs(30);
    while cx.read(|cx| viewer.read(cx).target().is_none()) {
        cx.executor().allow_parking();
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "the file did not open");
        std::thread::sleep(Duration::from_millis(3));
    }
    render(cx);
    cx.update_window(handle, |_, window, cx| {
        viewer.update(cx, |viewer, cx| viewer.focus(window, cx))
    })
    .unwrap();
    render(cx);

    cx.simulate_keystrokes(handle, "o");
    let deadline = Instant::now() + Duration::from_secs(30);
    while cx.read(|cx| matches!(panel.read(cx).key_state(), KeyState::Pending(_))) {
        cx.executor().allow_parking();
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "the key did not resolve");
        std::thread::sleep(Duration::from_millis(3));
    }
    render(cx);
    assert!(matches!(
        cx.read(|cx| panel.read(cx).key_state().clone()),
        KeyState::Unavailable(_)
    ));
    assert!(cx.read(|cx| panel.read(cx).is_visible()));
    let panel_focused = cx
        .update_window(handle, |_, window, cx| {
            panel.read(cx).contains_focus(window, cx)
        })
        .unwrap();
    assert!(panel_focused, "the panel keeps the focus");

    cx.simulate_keystrokes(handle, "o");
    render(cx);
    assert!(!cx.read(|cx| panel.read(cx).is_visible()));
    let viewer_focused = cx
        .update_window(handle, |_, window, cx| {
            viewer.read(cx).contains_focus(window, cx)
        })
        .unwrap();
    assert!(viewer_focused, "the transcript has the focus again");
}

/// The same with the key reply arriving before the panel's first frame (the
/// QA run's order): the panel still ends up with the focus.
#[gpui_kit::test]
fn the_panel_keeps_the_focus_when_the_key_reply_precedes_its_first_frame(cx: &mut TestAppContext) {
    use crate::deep_search::panel::KeyState;
    use crate::model::AgentTab;
    use crate::test_support::fixtures_agent::{agent_corpus, copy_outside_roots};
    use gpui_kit::test::TestWindowExt as _;

    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let outside = copy_outside_roots(&world, &corpus.retry);
    let handle = crate::test_support::open_single_file(cx, &world, &outside);
    let viewer = super::single_file_viewer(cx, handle);
    let panel = cx.read(|cx| viewer.read(cx).agent_panel().clone());
    let deadline = Instant::now() + Duration::from_secs(30);
    while cx.read(|cx| viewer.read(cx).target().is_none()) {
        cx.executor().allow_parking();
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "the file did not open");
        std::thread::sleep(Duration::from_millis(3));
    }
    cx.update_window(handle, |_, window, cx| {
        viewer.update(cx, |viewer, cx| viewer.focus(window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    // Open the panel and process the key reply without drawing a frame.
    cx.update_window(handle, |_, window, cx| {
        viewer.update(cx, |viewer, cx| {
            viewer.open_agent_panel(AgentTab::Outline, window, cx)
        })
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while cx.read(|cx| matches!(panel.read(cx).key_state(), KeyState::Pending(_))) {
        cx.executor().allow_parking();
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "the key did not resolve");
        std::thread::sleep(Duration::from_millis(3));
    }
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    let panel_focused = cx
        .update_window(handle, |_, window, cx| {
            panel.read(cx).contains_focus(window, cx)
        })
        .unwrap();
    assert!(panel_focused, "the panel keeps the focus");
}
