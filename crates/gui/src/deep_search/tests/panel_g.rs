//! G tests of the agent panel (D-001, D-009, D-014, D-018…D-024, D-029,
//! D-031, D-032, D-034…D-036, X-044).

use std::collections::BTreeMap;
use std::path::Path;

use gpui_kit::AppContext as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Entity, TestAppContext};

use super::{PanelFixture, render};
use crate::actions::{ReadRange, SearchWithinConversation, ToggleOutline};
use crate::deep_search::form::{Flag, ModeChoice};
use crate::deep_search::outline::FormTab;
use crate::deep_search::panel::{AgentPanel, AgentPanelEvent, KeyState, OUT_OF_ROOT_TEXT};
use crate::deep_search::results::Body;
use crate::model::AgentTab;
use crate::test_support::fixtures_agent::{
    agent_corpus, copy_outside_roots, duplicate_uuid_sessions, install_agent_embedder,
};
use crate::test_support::fixtures_list::Gate;
use crate::test_support::{TestWorld, open_single_file};

fn set_text(
    f: &PanelFixture,
    cx: &mut TestAppContext,
    pick: impl FnOnce(&AgentPanel) -> Entity<gpui_kit::component::input::InputState>,
    text: &str,
) {
    let text = text.to_string();
    f.with(cx, |panel, window, cx| {
        let input = pick(panel);
        input.update(cx, |input, cx| input.set_value(text, window, cx));
    });
}

fn body(f: &PanelFixture, cx: &TestAppContext, tab: AgentTab) -> Body {
    cx.read(|cx| f.panel.read(cx).results_of(tab).read(cx).body().clone())
}

/// The viewer's ref (resolved by the worker).
fn viewer_ref(f: &PanelFixture, cx: &TestAppContext) -> String {
    f.probe(cx, "agent.ref")
}

/// D-001: opening the panel offers the three tabs; the key resolves and the
/// outline runs for the viewer's conversation.
#[gpui_kit::test]
fn the_panel_outlines_the_viewers_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.outline12, AgentTab::Outline);
    assert_eq!(f.probe(cx, "agent.visible"), "true");
    assert_eq!(f.probe(cx, "agent.tab"), "outline");
    assert_eq!(f.probe(cx, "agent.key"), "known");
    assert!(viewer_ref(&f, cx).starts_with("ch_"));
    // D-022: a 12-message conversation: one row per message.
    assert_eq!(f.probe(cx, "agent.rows"), "12");
    assert_eq!(f.probe(cx, "agent.summary"), "12 visible messages");
    for tab in [AgentTab::Within, AgentTab::Read, AgentTab::Outline] {
        f.with(cx, |panel, window, cx| panel.show_tab(tab, window, cx));
        f.wait_idle(cx);
        assert_eq!(f.probe(cx, "agent.tab"), format!("{tab:?}").to_lowercase());
    }
    f.render(cx);
}

/// D-022: 25 messages are three segments; they expand and reveal.
#[gpui_kit::test]
fn a_long_outline_is_three_segments(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.outline25, AgentTab::Outline);
    assert_eq!(f.probe(cx, "agent.rows"), "3");
    assert_eq!(f.probe(cx, "agent.summary"), "25 visible messages");
    f.render(cx);
}

/// D-034: Outline computed for A re-runs for B when the viewer moves on;
/// Within results for A show the stale banner and still reveal in A.
#[gpui_kit::test]
fn results_belong_to_their_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Outline);
    assert_eq!(f.probe(cx, "agent.set"), "Token refresh retry");
    let events = f.record(cx);

    // Within for A.
    f.with(cx, |panel, window, cx| {
        panel.show_tab(AgentTab::Within, window, cx)
    });
    set_text(&f, cx, |panel| panel.within.query.clone(), "retry");
    f.with(cx, |panel, _, cx| panel.run_within(cx));
    f.wait_idle(cx);
    assert_eq!(f.probe(cx, "agent.set"), "Token refresh retry");
    assert_eq!(f.probe(cx, "agent.stale"), "false");
    assert!(f.probe(cx, "agent.rows").parse::<usize>().unwrap() >= 2);

    // The viewer moves to B: Outline (the visible tab of an earlier visit)
    // re-runs for B, Within is stale.
    f.with(cx, |panel, window, cx| {
        panel.show_tab(AgentTab::Outline, window, cx)
    });
    f.open_conversation(cx, &corpus.login, None);
    f.with(cx, |panel, window, cx| {
        panel.show_tab(AgentTab::Outline, window, cx)
    });
    f.wait_idle(cx);
    assert_eq!(f.probe(cx, "agent.target"), "Fix login bug");
    assert_eq!(
        f.probe(cx, "agent.set"),
        "Fix login bug",
        "the outline re-ran for B"
    );
    assert_eq!(f.probe(cx, "agent.rows"), "4");

    f.with(cx, |panel, _, cx| panel.select_tab(AgentTab::Within, cx));
    assert_eq!(f.probe(cx, "agent.set"), "Token refresh retry");
    assert_eq!(f.probe(cx, "agent.stale"), "true");
    // Clicking a row reveals in A, never in the viewer's conversation.
    let results = f.results(cx, AgentTab::Within);
    results.update(cx, |results, cx| results.open_index(0, false, None, cx));
    let Some(AgentPanelEvent::Reveal { target, reveal }) = events.borrow().last().cloned() else {
        panic!("a Reveal event");
    };
    assert_eq!(target.path, corpus.retry);
    assert_eq!(reveal.path, corpus.retry);
    // The workspace opened A (the viewer follows the reveal).
    f.render(cx);
    assert_eq!(
        cx.read(|cx| f
            .ws
            .read(cx)
            .viewer()
            .read(cx)
            .target()
            .map(|t| t.path.clone())),
        Some(corpus.retry.clone())
    );
}

/// D-019: Within has no conversation field; its target line and `No hits`
/// appear even for 0 hits; the request names the viewer's ref; a small
/// budget cuts the text.
#[gpui_kit::test]
fn within_searches_the_viewers_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Within);
    set_text(&f, cx, |panel| panel.within.query.clone(), "zzz");
    f.with(cx, |panel, _, cx| panel.run_within(cx));
    f.wait_idle(cx);
    let Body::Search(model) = body(&f, cx, AgentTab::Within) else {
        panic!("a search body");
    };
    assert!(model.within && model.rows.is_empty());
    let line = model.target_line.clone().expect("a target line");
    let full_ref = viewer_ref(&f, cx);
    let canonical = line
        .split("ref=")
        .nth(1)
        .expect("a ref in the target line")
        .to_string();
    assert!(
        full_ref.starts_with(&canonical),
        "{full_ref} vs {canonical}"
    );
    assert!(
        line.starts_with("conversation project=pr_") || line.starts_with("conversation project=")
    );
    assert_eq!(f.probe(cx, "agent.error"), "");

    // Budget 200 cuts the output of a query that hits.
    set_text(&f, cx, |panel| panel.within.query.clone(), "retry");
    set_text(&f, cx, |panel| panel.within.budget.clone(), "200");
    f.with(cx, |panel, _, cx| panel.run_within(cx));
    f.wait_idle(cx);
    let results = f.results(cx, AgentTab::Within);
    let text = cx.read(|cx| results.read(cx).response().unwrap().text().to_string());
    assert!(text.contains("cut="), "{text}");
    assert!(text.contains(&format!("ref={canonical}")) || text.contains("agent-within"));
    // The set remembers the viewer's ref (what the request carried).
    assert_eq!(
        cx.read(|cx| f
            .panel
            .read(cx)
            .set_of(AgentTab::Within)
            .map(|s| s.full_ref.clone())),
        Some(full_ref)
    );
    // D-009: the agent text toggle equals the response text.
    cx.update_window(f.handle, |_, window, cx| {
        results.update(cx, |results, cx| results.set_text_shown(true, window, cx))
    })
    .unwrap();
    f.render(cx);
    assert_eq!(f.probe(cx, "agent.text"), text);
    assert_eq!(f.probe(cx, "agent.text_shown"), "true");
}

/// D-018: the status line shows the progress of a semantic Within run.
#[gpui_kit::test]
fn within_shows_the_embedding_progress(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Within);
    let control = install_agent_embedder(cx);
    let gate = Gate::closed();
    *control.gate.lock().unwrap() = Some(gate.clone());
    f.with(cx, |panel, _, cx| {
        panel.set_within_mode(ModeChoice::Semantic, cx)
    });
    set_text(&f, cx, |panel| panel.within.query.clone(), "retry");
    f.with(cx, |panel, _, cx| panel.run_within(cx));
    let deadline = std::time::Instant::now() + super::WAIT;
    loop {
        cx.executor().allow_parking();
        cx.run_until_parked();
        if f.probe(cx, "agent.status")
            .starts_with("Searching… Embedding ")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "status: {}",
            f.probe(cx, "agent.status")
        );
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    let status = f.probe(cx, "agent.status");
    let progress = status.strip_prefix("Searching… Embedding ").unwrap();
    let (done, total) = progress.split_once('/').expect("n/m");
    assert!(
        done.parse::<usize>().is_ok() && total.parse::<usize>().unwrap() > 0,
        "{status}"
    );
    gate.open();
    f.wait_idle(cx);
    assert_eq!(f.probe(cx, "agent.status"), "");
}

/// D-021: reading ranges gives the message cards; Match marks lines; two
/// conversations give two headers.
#[gpui_kit::test]
fn read_shows_cards_grouped_by_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Read);
    let retry = viewer_ref(&f, cx);
    f.with(cx, |panel, window, cx| {
        panel.set_refs(&format!("{retry}:m2..m3"), window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
    let Body::Read(model) = body(&f, cx, AgentTab::Read) else {
        panic!("a read body");
    };
    assert_eq!(model.groups.len(), 1);
    let cards = &model.groups[0].cards;
    assert_eq!(cards.len(), 2);
    assert!(cards[0].header().starts_with("m2 · assistant · line "));
    assert!(cards[1].header().starts_with("m3 · user · line "));
    assert_eq!(
        model.groups[0].title.as_deref(),
        Some("Token refresh retry")
    );
    assert!(cards[0].body.contains("retry loop"));

    // Match: the matching lines of one message carry `>` markers.
    f.with(cx, |panel, window, cx| {
        panel.set_refs(&format!("{retry}:m2..m2"), window, cx)
    });
    set_text(&f, cx, |panel| panel.read.match_query.clone(), "backoff");
    set_text(&f, cx, |panel| panel.read.context.clone(), "1");
    f.with(cx, |panel, _, cx| panel.run_read(cx));
    f.wait_idle(cx);
    let Body::Read(model) = body(&f, cx, AgentTab::Read) else {
        panic!("a read body");
    };
    let card = &model.groups[0].cards[0];
    assert!(
        card.body
            .lines()
            .any(|line| line.trim_start().starts_with('>') && line.contains("backoff")),
        "{}",
        card.body
    );
    assert!(
        card.slice
            .as_deref()
            .is_some_and(|slice| slice.contains("match=")),
        "{:?}",
        card.slice
    );

    // Two refs of two conversations: two headers.
    set_text(&f, cx, |panel| panel.read.match_query.clone(), "");
    set_text(&f, cx, |panel| panel.read.context.clone(), "");
    f.open_conversation(cx, &corpus.login, Some(AgentTab::Read));
    let login = viewer_ref(&f, cx);
    f.with(cx, |panel, window, cx| {
        panel.set_refs(&format!("{retry}:m1..m1, {login}:m1..m1"), window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
    let Body::Read(model) = body(&f, cx, AgentTab::Read) else {
        panic!("a read body");
    };
    assert_eq!(model.groups.len(), 2, "one header per conversation");
    // Multi-ref reads have no paging.
    assert!(cx.read(|cx| f.panel.read(cx).read.paging.is_none()));
    f.render(cx);
}

/// D-014, D-023: refs errors are typed alerts; an ambiguous ref lists
/// candidates and clicking one fills the Refs field; a full UUID resolves;
/// refs separated by spaces are several refs.
#[gpui_kit::test]
fn read_reports_typed_errors_and_resolves_uuids(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let (dup_a, _dup_b) = duplicate_uuid_sessions(&world, 77);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Read);
    let run = |f: &PanelFixture, cx: &mut TestAppContext, refs: &str| {
        let refs = refs.to_string();
        f.with(cx, |panel, window, cx| {
            panel.set_refs(&refs, window, cx);
            panel.run_read(cx);
        });
        f.wait_idle(cx);
    };

    run(&f, cx, "ch_zz");
    assert!(
        f.probe(cx, "agent.error").starts_with("invalid-ref: "),
        "{}",
        f.probe(cx, "agent.error")
    );
    run(&f, cx, "ch_12");
    assert!(
        f.probe(cx, "agent.error").starts_with("invalid-ref: "),
        "too short: {}",
        f.probe(cx, "agent.error")
    );

    // Ambiguous: the same UUID in two projects.
    let uuid = dup_a.file_stem().unwrap().to_string_lossy().to_string();
    run(&f, cx, &uuid);
    let error = cx.read(|cx| {
        f.panel
            .read(cx)
            .results_of(AgentTab::Read)
            .read(cx)
            .error()
            .cloned()
            .expect("an error")
    });
    assert!(error.text.starts_with("ambiguous-ref: "), "{}", error.text);
    assert_eq!(error.candidates.len(), 2);
    let candidate = error.candidates[0].clone();
    // A real click on the first candidate button (the id the view gives it).
    f.with(cx, |panel, window, cx| {
        panel.show_tab(AgentTab::Read, window, cx)
    });
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("agent-read-candidate-0", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| f.panel.read(cx).read.refs.read(cx).value().to_string()),
        candidate,
        "clicking a candidate fills the Refs field"
    );
    f.with(cx, |panel, _, cx| panel.run_read(cx));
    f.wait_idle(cx);
    assert_eq!(f.probe(cx, "agent.error"), "");

    // A full UUID resolves; `a b` sends two refs.
    let retry_uuid = corpus
        .retry
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    run(&f, cx, &retry_uuid);
    assert_eq!(f.probe(cx, "agent.error"), "");
    let login_uuid = corpus
        .login
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    run(&f, cx, &format!("{retry_uuid} {login_uuid}"));
    let Body::Read(model) = body(&f, cx, AgentTab::Read) else {
        panic!("a read body");
    };
    assert_eq!(model.groups.len(), 2, "two refs were sent");
}

/// D-020: Lines and Match conflict inline; an anchor needs one ref without a
/// range.
#[gpui_kit::test]
fn the_read_form_validates_like_the_cli(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Read);
    set_text(&f, cx, |panel| panel.read.lines.clone(), "1..2");
    set_text(&f, cx, |panel| panel.read.match_query.clone(), "retry");
    f.with(cx, |panel, _, cx| panel.run_read(cx));
    assert_eq!(
        f.probe(cx, "agent.error"),
        "the argument '--lines' cannot be used with '--match'"
    );
    set_text(&f, cx, |panel| panel.read.lines.clone(), "");
    set_text(&f, cx, |panel| panel.read.match_query.clone(), "");
    let retry = viewer_ref(&f, cx);
    set_text(
        &f,
        cx,
        |panel| panel.read.anchor.clone(),
        "ma_0123456789abcdef",
    );
    f.with(cx, |panel, window, cx| {
        panel.set_refs(&format!("{retry}:m1..m2"), window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
    assert!(
        f.probe(cx, "agent.error")
            .contains("--anchor requires exactly one conversation ref without an mN range"),
        "{}",
        f.probe(cx, "agent.error")
    );
}

/// D-031: Load next / Load previous extend a single-ref range by 5.
#[gpui_kit::test]
fn paging_extends_the_range(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.outline25, AgentTab::Read);
    let key = viewer_ref(&f, cx);
    f.with(cx, |panel, window, cx| {
        panel.set_refs(&format!("{key}:m2..m3"), window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
    f.with(cx, |panel, window, cx| panel.load_more(false, window, cx));
    f.wait_idle(cx);
    assert_eq!(
        cx.read(|cx| f.panel.read(cx).read.refs.read(cx).value().to_string()),
        format!("{key}:m2..m8")
    );
    let Body::Read(model) = body(&f, cx, AgentTab::Read) else {
        panic!("a read body");
    };
    assert_eq!(model.groups[0].cards.len(), 7, "m2..m8");

    f.with(cx, |panel, window, cx| {
        panel.set_refs(&format!("{key}:m10..m12"), window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
    f.with(cx, |panel, window, cx| panel.load_more(true, window, cx));
    f.wait_idle(cx);
    assert_eq!(
        cx.read(|cx| f.panel.read(cx).read.refs.read(cx).value().to_string()),
        format!("{key}:m5..m12")
    );
    // Loading next beyond the transcript clamps to its end (m25).
    f.with(cx, |panel, window, cx| {
        panel.set_refs(&format!("{key}:m20..m23"), window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
    f.with(cx, |panel, window, cx| panel.load_more(false, window, cx));
    f.wait_idle(cx);
    f.wait_idle(cx);
    assert_eq!(f.probe(cx, "agent.error"), "");
    assert_eq!(
        cx.read(|cx| f.panel.read(cx).read.refs.read(cx).value().to_string()),
        format!("{key}:m20..m25")
    );
}

/// D-036: `[agent]` flags render on and disabled with the config tooltip.
#[gpui_kit::test]
fn config_forced_flags_are_on_and_disabled(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    world.write_config(Some("[agent]\ntools = true\n"), None);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Outline);
    cx.read(|cx| {
        let panel = f.panel.read(cx);
        assert_eq!(
            panel.flag_state(FormTab::Outline, Flag::Tools, cx),
            (true, true, "Enabled by [agent] tools in config".to_string())
        );
        let (checked, disabled, tooltip) = panel.flag_state(FormTab::Read, Flag::Thinking, cx);
        assert!(!checked && !disabled);
        assert_eq!(tooltip, "Include thinking blocks");
    });
    // The outline ran with the policy applied (tools are visible).
    let outline = cx.read(|cx| {
        match f
            .panel
            .read(cx)
            .results_of(AgentTab::Outline)
            .read(cx)
            .body()
        {
            Body::Outline(model) => model.hint.tools,
            other => panic!("{other:?}"),
        }
    });
    assert!(outline);
    f.render(cx);
}

/// D-035: a file outside the history roots has no key: the panel says so; an
/// in-root file works.
#[gpui_kit::test]
fn a_file_outside_the_roots_disables_the_panel(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let outside = copy_outside_roots(&world, &corpus.retry);
    let handle = open_single_file(cx, &world, &outside);
    let panel = single_panel(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        panel.update(cx, |panel, cx| {
            panel.set_viewer_target(
                Some(crate::model::ConversationTarget::from_parsed_file(
                    &outside, None,
                )),
                cx,
            );
            panel.show_tab(AgentTab::Outline, window, cx);
        })
    })
    .unwrap();
    wait_key(cx, &panel);
    cx.read(|cx| {
        assert!(matches!(
            panel.read(cx).key_state(),
            KeyState::Unavailable(_)
        ));
        assert_eq!(
            panel.read(cx).qa_probe("agent.rows", cx).as_deref(),
            Some("0")
        );
    });
    render(cx, handle);
    assert_eq!(
        OUT_OF_ROOT_TEXT,
        "Outline, Within and Read work only for conversations inside the Claude, Pi or OMP history folders."
    );

    // The original file inside the roots works.
    let in_root = open_single_file_again(cx, &world, &corpus.retry);
    let panel = single_panel(cx, in_root);
    cx.update_window(in_root, |_, window, cx| {
        panel.update(cx, |panel, cx| {
            panel.set_viewer_target(
                Some(crate::model::ConversationTarget::from_parsed_file(
                    &corpus.retry,
                    None,
                )),
                cx,
            );
            panel.show_tab(AgentTab::Outline, window, cx);
        })
    })
    .unwrap();
    wait_key(cx, &panel);
    let deadline = std::time::Instant::now() + super::WAIT;
    loop {
        cx.executor().allow_parking();
        cx.run_until_parked();
        let rows = cx
            .read(|cx| panel.read(cx).qa_probe("agent.rows", cx))
            .unwrap();
        if rows != "0" {
            assert_eq!(rows, "7", "the tool result joins the call's message");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the outline did not arrive"
        );
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

fn open_single_file_again(
    cx: &mut TestAppContext,
    world: &TestWorld,
    path: &Path,
) -> gpui_kit::AnyWindowHandle {
    open_single_file(cx, world, path)
}

fn single_panel(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle) -> Entity<AgentPanel> {
    cx.update_window(handle, |root, _, cx| {
        let root = root
            .downcast::<gpui_kit::base::Root>()
            .expect("a gpui-kit root");
        let view = root.read(cx).view().clone();
        let single = view
            .downcast::<crate::single_file::SingleFileWindow>()
            .expect("a single-file window");
        single.read(cx).viewer().read(cx).agent_panel().clone()
    })
    .unwrap()
}

fn wait_key(cx: &mut TestAppContext, panel: &Entity<AgentPanel>) {
    let deadline = std::time::Instant::now() + super::WAIT;
    loop {
        cx.executor().allow_parking();
        cx.run_until_parked();
        if !cx.read(|cx| matches!(panel.read(cx).key_state(), KeyState::Pending(_))) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the key did not resolve"
        );
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

/// D-029: with the viewer focused, the session actions open the matching tab
/// for the viewer's conversation.
#[gpui_kit::test]
fn session_actions_open_the_panel_tabs(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Outline);
    f.with(cx, |panel, _, cx| panel.hide(cx));
    f.open_conversation(cx, &corpus.retry, None);
    f.render(cx);
    cx.dispatch_action(f.handle, SearchWithinConversation);
    f.wait_idle(cx);
    f.render(cx);
    assert_eq!(
        (f.probe(cx, "agent.visible"), f.probe(cx, "agent.tab")),
        ("true".into(), "within".into())
    );
    cx.dispatch_action(f.handle, ReadRange);
    f.wait_idle(cx);
    assert_eq!(f.probe(cx, "agent.tab"), "read");
    cx.dispatch_action(f.handle, ToggleOutline);
    f.wait_idle(cx);
    f.render(cx);
    // `o` with the panel open on another tab shows the outline tab; on the
    // outline tab it closes the panel.
    assert_eq!(
        (f.probe(cx, "agent.visible"), f.probe(cx, "agent.tab")),
        ("true".into(), "outline".into())
    );
    cx.dispatch_action(f.handle, ToggleOutline);
    f.render(cx);
    assert_eq!(f.probe(cx, "agent.visible"), "false");
}

/// X-044: with the panel focused, `j` moves the outline selection; `t` and
/// `q` do not reach the transcript.
#[gpui_kit::test]
fn the_panel_has_its_own_key_context(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Outline);
    f.render(cx);
    let results = f.results(cx, AgentTab::Outline);
    cx.update_window(f.handle, |_, window, cx| {
        results.update(cx, |results, cx| results.focus_rows(window, cx))
    })
    .unwrap();
    f.render(cx);
    let tool_display = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            f.ws.read(cx)
                .viewer()
                .read(cx)
                .qa_probe("viewer.tool_display", cx)
        })
    };
    let before = tool_display(cx);
    assert_eq!(f.probe(cx, "agent.selected"), "0");
    cx.simulate_keystrokes(f.handle, "j");
    assert_eq!(f.probe(cx, "agent.selected"), "1");
    cx.simulate_keystrokes(f.handle, "k");
    assert_eq!(f.probe(cx, "agent.selected"), "0");
    cx.simulate_keystrokes(f.handle, "down down");
    assert_eq!(f.probe(cx, "agent.selected"), "2");
    cx.simulate_keystrokes(f.handle, "t");
    cx.simulate_keystrokes(f.handle, "q");
    assert_eq!(tool_display(cx), before, "t did not reach the transcript");
    let focus = cx
        .update_window(f.handle, |_, window, cx| {
            f.ws.update(cx, |ws, cx| ws.qa_probe("focus", window, cx))
        })
        .unwrap();
    assert_eq!(focus.as_deref(), Some("agent"), "q did not leave the panel");
    // Enter reveals the selected message (and focuses the viewer).
    let events = f.record(cx);
    cx.simulate_keystrokes(f.handle, "enter");
    let Some(AgentPanelEvent::Reveal { target, reveal }) = events.borrow().last().cloned() else {
        panic!("a Reveal event");
    };
    assert_eq!(target.path, corpus.retry);
    assert!(matches!(
        reveal.kind,
        crate::model::RevealKind::AgentOrdinal { ordinal: 3, .. }
    ));
}

/// §5.4: Esc clears the query, cancels a running job, then falls through to
/// the viewer.
#[gpui_kit::test]
fn escape_in_the_panel(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Within);
    let control = install_agent_embedder(cx);
    f.render(cx);
    set_text(&f, cx, |panel| panel.within.query.clone(), "retry");
    f.with(cx, |panel, window, cx| {
        use gpui_kit::Focusable as _;
        panel.within.query.focus_handle(cx).focus(window, cx)
    });
    f.render(cx);
    cx.simulate_keystrokes(f.handle, "escape");
    assert_eq!(
        cx.read(|cx| f.panel.read(cx).within.query.read(cx).value().to_string()),
        ""
    );

    // A running semantic job is cancelled first.
    let gate = Gate::closed();
    *control.gate.lock().unwrap() = Some(gate.clone());
    f.with(cx, |panel, _, cx| {
        panel.set_within_mode(ModeChoice::Semantic, cx)
    });
    set_text(&f, cx, |panel| panel.within.query.clone(), "retry");
    f.with(cx, |panel, _, cx| panel.run_within(cx));
    assert!(f.probe(cx, "agent.status").starts_with("Searching"));
    cx.simulate_keystrokes(f.handle, "escape");
    assert_eq!(f.probe(cx, "agent.status"), "Search cancelled");
    gate.open();
    f.wait_idle(cx);
    // With nothing to clear or cancel Esc focuses the transcript.
    set_text(&f, cx, |panel| panel.within.query.clone(), "");
    cx.simulate_keystrokes(f.handle, "escape");
    f.render(cx);
    let focus = cx
        .update_window(f.handle, |_, window, cx| {
            f.ws.update(cx, |ws, cx| ws.qa_probe("focus", window, cx))
        })
        .unwrap();
    assert_eq!(focus.as_deref(), Some("viewer"));
    assert_eq!(f.probe(cx, "agent.visible"), "true", "the panel stays open");
}

/// D-032: running Search, Within, Read and Outline leaves the Claude, Pi and
/// OMP trees unchanged.
#[gpui_kit::test]
fn the_agent_never_writes_the_history_trees(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    // One Pi and one OMP session, so those code paths really run (and the tree
    // map has Pi/OMP files whose modification it could catch).
    let pi_id = "01912345-6789-7abc-8def-0123456789ab";
    let omp_id = "01a082ad-c8d8-7149-8d3e-dd8f74bb7ed4";
    for (root, text, id) in [
        (".pi", crate::test_support::synth::PI_SESSION, pi_id),
        (".omp", crate::test_support::synth::OMP_SESSION, omp_id),
    ] {
        world.add_fixture(
            &format!("{root}/agent/sessions/--only--/2024-03-02T00-00-00-000Z_{id}.jsonl"),
            text.replace("omp_session_custom_id", omp_id).as_bytes(),
        );
    }
    let before = tree_hash(world.home());
    assert!(
        before.keys().any(|k| k.contains(".pi")) && before.keys().any(|k| k.contains(".omp")),
        "the map covers Pi and OMP transcripts"
    );
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Outline);
    set_text(&f, cx, |panel| panel.within.query.clone(), "retry");
    f.with(cx, |panel, window, cx| {
        panel.show_tab(AgentTab::Within, window, cx);
        panel.run_within(cx);
    });
    f.wait_idle(cx);
    f.with(cx, |panel, window, cx| {
        panel.show_tab(AgentTab::Read, window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
    for id in [pi_id, omp_id] {
        f.with(cx, |panel, window, cx| {
            panel.set_refs(id, window, cx);
            panel.run_read(cx);
        });
        f.wait_idle(cx);
        assert_eq!(f.probe(cx, "agent.error"), "", "reading {id}");
        assert!(
            matches!(body(&f, cx, AgentTab::Read), Body::Read(_)),
            "{id} was really read"
        );
    }
    let deep = cx.read(|cx| f.ws.read(cx).deep().clone());
    deep.update(cx, |_, _| {});
    cx.update_window(f.handle, |_, window, cx| {
        deep.update(cx, |pane, cx| {
            pane.set_query("alpha", window, cx);
            pane.run(cx);
        })
    })
    .unwrap();
    let deadline = std::time::Instant::now() + super::WAIT;
    while cx.read(|cx| deep.read(cx).is_running(cx)) {
        cx.executor().allow_parking();
        cx.run_until_parked();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert_eq!(
        cx.read(|cx| deep.read(cx).qa_probe("deep.error", cx))
            .as_deref(),
        Some("")
    );
    assert_eq!(
        tree_hash(world.home()),
        before,
        "the history trees are untouched"
    );
}

/// A digest of every file under the Claude, Pi and OMP trees of `home`.
fn tree_hash(home: &Path) -> BTreeMap<String, (u64, std::time::SystemTime)> {
    let mut hashes = BTreeMap::new();
    for root in [".claude", ".pi", ".omp"] {
        let mut stack = vec![home.join(root)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let meta = entry.metadata().expect("metadata");
                if meta.is_dir() {
                    stack.push(path);
                } else {
                    hashes.insert(
                        path.display().to_string(),
                        (meta.len(), meta.modified().expect("mtime")),
                    );
                }
            }
        }
    }
    hashes
}

/// A deleted conversation's rows no longer open anything.
#[gpui_kit::test]
fn deleted_conversations_disable_their_rows(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Outline);
    let events = f.record(cx);
    let results = f.results(cx, AgentTab::Outline);
    f.with(cx, |panel, _, cx| {
        panel.mark_deleted(std::slice::from_ref(&corpus.retry), cx)
    });
    results.update(cx, |results, cx| results.open_index(0, false, None, cx));
    assert!(
        events.borrow().is_empty(),
        "a deleted conversation does not open"
    );
}
