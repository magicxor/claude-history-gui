//! The list's search hit in the previewed conversation (upstream 411e227
//! list preview; L-109, DESIGN §9.8a): the debounced preview of a search
//! result opens at its hit, a refined query re-anchors it without a new
//! build, a cleared query or a `/` search drops the hit's highlights.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px};

use super::{probe, render, viewer_of};
use crate::test_support::TestWorld;
use crate::test_support::fixtures_list::{ListFixture, SessionSpec};
use crate::test_support::fixtures_viewer::wait_viewer_ready;
use crate::viewer::ViewerPane;
use crate::viewer::nav;

/// `n` alternating filler messages of three paragraphs each.
fn filler(n: usize) -> Vec<(&'static str, String)> {
    (0..n)
        .map(|ix| {
            let role = if ix % 2 == 0 { "user" } else { "assistant" };
            let text = format!(
                "Filler message {ix} body.\n\nSecond paragraph of filler {ix}.\n\nThird paragraph."
            );
            (role, text)
        })
        .collect()
}

/// Write a session of `messages`, its last message `hours_ago` hours ago.
fn session(
    world: &TestWorld,
    number: u32,
    messages: &[(&'static str, String)],
    hours_ago: i64,
) -> PathBuf {
    let refs: Vec<(&str, &str)> = messages
        .iter()
        .map(|(role, text)| (*role, text.as_str()))
        .collect();
    let start = chrono::Utc::now()
        - chrono::Duration::hours(hours_ago)
        - chrono::Duration::minutes(messages.len() as i64);
    SessionSpec::new("proj", number)
        .messages(&refs)
        .start(start)
        .write(world)
}

/// The dialogue of upstream's `list_preview_anchors_on_the_first_match`
/// between `fillers` filler messages before and 20 after, newest; and an
/// older session without it.
fn dialogue_world(fillers: usize) -> (TestWorld, PathBuf) {
    let world = TestWorld::new();
    let mut messages = filler(fillers);
    for (role, text) in [
        ("user", "set up the tunnel"),
        ("assistant", "the tunnel is up"),
        ("user", "now rotate the token"),
        ("assistant", "rotated the token and restarted"),
    ] {
        messages.push((role, text.to_string()));
    }
    messages.extend(filler(20));
    let path = session(&world, 1, &messages, 1);
    session(&world, 2, &filler(4), 48);
    (world, path)
}

/// Draw frames until the hit is in place.
fn place(cx: &mut TestAppContext, handle: AnyWindowHandle, viewer: &Entity<ViewerPane>) {
    for _ in 0..6 {
        render(cx, handle);
    }
    assert!(
        !cx.read(|cx| viewer.read(cx).hit_placing()),
        "the hit was placed"
    );
}

/// Let the debounced preview (or hit) land, then place it.
fn preview(cx: &mut TestAppContext, f: &ListFixture) -> Entity<ViewerPane> {
    f.wait_idle(cx);
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    let viewer = viewer_of(cx, &f.ws);
    wait_viewer_ready(cx, f.handle, &viewer);
    place(cx, f.handle, &viewer);
    viewer
}

/// The text of the line the viewer's list hit is at.
fn hit_line(cx: &TestAppContext, viewer: &Entity<ViewerPane>) -> Option<String> {
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        let anchor = viewer.hit_anchor()?;
        let items = viewer.items()?;
        let text = &items[anchor.item].segments[anchor.segment].text;
        Some(text[anchor.line.clone()].to_string())
    })
}

/// L-109 (upstream `list_preview_anchors_on_the_first_match`): the
/// previewed result opens at the first line matching the query, its
/// message on top and focused; the list keeps the focus.
#[gpui_kit::test]
fn l109_the_previewed_result_opens_at_its_hit(cx: &mut TestAppContext) {
    let (world, path) = dialogue_world(30);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "token");
    let viewer = preview(cx, &f);

    assert_eq!(
        cx.read(|cx| viewer.read(cx).target().map(|t| t.path.clone())),
        Some(path)
    );
    assert_eq!(
        cx.read(|cx| viewer.read(cx).list_hit().map(|hit| hit.query.clone())),
        Some("token".to_string())
    );
    assert_eq!(
        hit_line(cx, &viewer).as_deref(),
        Some("now rotate the token")
    );
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        let anchor = viewer.hit_anchor().unwrap();
        assert_eq!(
            viewer.nav().highlighted(),
            anchor.position,
            "its message is focused"
        );
        let info = viewer.build_info().unwrap();
        let first = nav::first_item_of(&info, anchor.position.unwrap()).unwrap();
        let list = viewer.list_state();
        let viewport = list.viewport_bounds();
        let top = list
            .bounds_for_item(first)
            .expect("the message is drawn")
            .top();
        assert!(
            (top - viewport.top()).abs() < px(1.),
            "the message starts at the top: {top:?} vs {viewport:?}"
        );
        assert!(list.logical_scroll_top().item_ix > 0, "not at the top");
        let matches = viewer.hit_matches().expect("highlighted");
        let current: Vec<bool> = matches.values().flatten().map(|m| m.2).collect();
        assert_eq!(current.len(), 2, "both matches are highlighted");
        assert_eq!(current.iter().filter(|c| **c).count(), 1, "one is current");
    });
    assert_eq!(
        probe(cx, f.handle, &f.ws, "focus"),
        "list",
        "the list keeps focus"
    );
    let hit = cx
        .read(|cx| viewer.read(cx).qa_probe("viewer.hit_line", cx))
        .unwrap();
    assert_eq!(hit, "now rotate the token");
}

/// L-109: a hit deep inside a long message sits a third of the way down
/// the viewport (upstream `ListPreview::scroll_offset`).
#[gpui_kit::test]
fn l109_a_hit_deep_in_its_message_sits_a_third_of_the_way_down(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let mut paragraphs: Vec<String> = (0..200)
        .map(|ix| format!("Filler paragraph {ix} of the long message."))
        .collect();
    paragraphs.push("now rotate the token".to_string());
    paragraphs.extend((0..60).map(|ix| format!("Closing paragraph {ix}.")));
    let messages = vec![
        ("user", "a long question".to_string()),
        ("assistant", paragraphs.join("\n\n")),
    ];
    session(&world, 1, &messages, 1);
    session(&world, 2, &filler(4), 48);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "token");
    let viewer = preview(cx, &f);
    assert_eq!(
        hit_line(cx, &viewer).as_deref(),
        Some("now rotate the token")
    );

    let (item, segment, byte) = cx.read(|cx| {
        let anchor = viewer.read(cx).hit_anchor().unwrap().clone();
        (anchor.item, anchor.segment, anchor.byte)
    });
    viewer.update(cx, |viewer, cx| {
        viewer.request_text_probe(item, segment, byte, cx)
    });
    render(cx, f.handle);
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        let y = viewer.text_probe().expect("the hit is drawn").y;
        let viewport = viewer.list_state().viewport_bounds();
        let third = viewport.size.height / 3.;
        let below_top = y - viewport.top();
        assert!(
            (below_top - third).abs() < px(30.),
            "the hit sits a third of the way down: {below_top:?} vs {third:?}"
        );
        let anchor = viewer.hit_anchor().unwrap();
        assert_eq!(viewer.nav().highlighted(), anchor.position);
    });
}

/// L-109 (upstream `list_preview_needs_a_query`): with a blank query the
/// selected row previews at the top, with nothing highlighted.
#[gpui_kit::test]
fn l109_a_blank_query_previews_at_the_top(cx: &mut TestAppContext) {
    let (world, path) = dialogue_world(30);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, " ");
    let viewer = preview(cx, &f);
    assert_eq!(
        cx.read(|cx| viewer.read(cx).target().map(|t| t.path.clone())),
        Some(path)
    );
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        assert!(viewer.list_hit().is_none());
        assert!(viewer.hit_anchor().is_none());
        assert!(viewer.hit_matches().is_none());
        assert!(!viewer.nav().is_active());
        assert_eq!(viewer.list_state().logical_scroll_top().item_ix, 0);
    });
}

/// L-109 (upstream 917758a `list_preview_keeps_showing_the_conversation_while_the_
/// query_changes`): typing more of the query re-anchors the conversation
/// shown without building it again.
#[gpui_kit::test]
fn l109_typing_more_re_anchors_without_a_rebuild(cx: &mut TestAppContext) {
    let (world, path) = dialogue_world(30);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "rotate");
    let viewer = preview(cx, &f);
    assert_eq!(
        hit_line(cx, &viewer).as_deref(),
        Some("now rotate the token")
    );
    let builds = cx.read(|cx| viewer.read(cx).completed_builds());

    f.type_text(cx, "d");
    f.wait_idle(cx);
    assert!(
        cx.read(|cx| f.ws.read(cx).has_pending_hit()),
        "debounced like a preview"
    );
    assert_eq!(
        cx.read(|cx| viewer.read(cx).list_hit().map(|hit| hit.query.clone())),
        Some("rotate".to_string())
    );
    let viewer = preview(cx, &f);
    assert_eq!(
        cx.read(|cx| viewer.read(cx).target().map(|t| t.path.clone())),
        Some(path)
    );
    assert_eq!(
        cx.read(|cx| viewer.read(cx).list_hit().map(|hit| hit.query.clone())),
        Some("rotated".to_string())
    );
    assert_eq!(
        hit_line(cx, &viewer).as_deref(),
        Some("rotated the token and restarted")
    );
    assert_eq!(
        cx.read(|cx| viewer.read(cx).completed_builds()),
        builds,
        "re-anchored, not rebuilt"
    );
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        assert_eq!(
            viewer.nav().highlighted(),
            viewer.hit_anchor().unwrap().position
        );
    });
}

/// L-109: a `/` search replaces the hit's highlights; clearing the list's
/// query drops them and leaves the conversation where it is.
#[gpui_kit::test]
fn l109_a_find_or_a_cleared_query_drops_the_hit(cx: &mut TestAppContext) {
    let (world, path) = dialogue_world(30);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "token");
    let viewer = preview(cx, &f);
    assert!(cx.read(|cx| viewer.read(cx).hit_matches().is_some()));

    // The list's query cleared (Esc in the search input).
    f.press(cx, "escape");
    let viewer = preview(cx, &f);
    assert_eq!(
        cx.read(|cx| viewer.read(cx).target().map(|t| t.path.clone())),
        Some(path.clone())
    );
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        assert!(viewer.list_hit().is_none());
        assert!(viewer.hit_matches().is_none());
        assert!(
            viewer.list_state().logical_scroll_top().item_ix > 0,
            "the viewport stays"
        );
    });

    // Again at the hit, then `/` in the viewer.
    f.type_text(cx, "token");
    let viewer = preview(cx, &f);
    assert!(cx.read(|cx| viewer.read(cx).hit_matches().is_some()));
    cx.update_window(f.handle, |_, window, cx| {
        viewer.update(cx, |viewer, cx| viewer.focus(window, cx))
    })
    .unwrap();
    render(cx, f.handle);
    f.press(cx, "/");
    cx.simulate_input(f.handle, "tunnel");
    render(cx, f.handle);
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        assert!(viewer.list_hit().is_none(), "the find replaced the hit");
        assert!(viewer.hit_matches().is_none());
        assert_eq!(viewer.view_search().count(), 2);
    });
}

/// L-109: a double-click on the row of the conversation shown, within the
/// debounce of a cleared query, leaves no stale hit (the open is not a
/// captured action, so nothing flushes the pending hit first).
#[gpui_kit::test]
fn l109_an_open_within_the_debounce_of_a_cleared_query_drops_the_hit(cx: &mut TestAppContext) {
    let (world, path) = dialogue_world(30);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "token");
    let viewer = preview(cx, &f);
    assert!(cx.read(|cx| viewer.read(cx).list_hit().is_some()));

    f.press(cx, "escape");
    f.wait_idle(cx);
    assert!(
        cx.read(|cx| f.ws.read(cx).has_pending_hit()),
        "the cleared query waits for the debounce"
    );
    let target = cx.read(|cx| viewer.read(cx).target().cloned()).unwrap();
    assert_eq!(target.path, path);
    f.list.update(cx, |_, cx| {
        cx.emit(crate::list::ListPaneEvent::OpenRequested {
            target,
            focus_viewer: true,
            agent_tab: None,
        })
    });
    f.wait_idle(cx);
    cx.read(|cx| {
        let viewer = viewer.read(cx);
        assert!(viewer.list_hit().is_none());
        assert!(viewer.hit_matches().is_none());
    });
}
