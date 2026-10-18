//! G tests of the Deep search tab (D-001…D-016, D-018, D-024, D-026, D-030,
//! D-033, C-031) in the main window over the agent corpus.

use claude_history_core::agent::args::AgentSearchArgs;
use claude_history_core::agent::{AgentRequest, AgentResult, execute};
use claude_history_core::config::ConfigFile;
use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;

use super::DeepFixture;
use crate::deep_search::form::{ModeChoice, ScopeChoice, TimeTexts};
use crate::deep_search::results::{Body, HitAction, SearchRow};
use crate::deep_search::worker::SEMANTIC_PAUSED_TEXT;
use crate::deep_search::{DeepSearchEvent, Field};
use crate::globals::{app_state, last_toast};
use crate::model::{AgentTab, RevealKind};
use crate::test_support::fixtures_agent::{
    AgentEmbedderControl, agent_corpus, install_agent_embedder, warm_semantic_cache,
    workspace_needle_corpus,
};
use crate::test_support::fixtures_list::{Gate, install_failing_embedder};
use crate::test_support::{TestWorld, recording};

fn expected(world: &TestWorld, args: AgentSearchArgs) -> claude_history_core::agent::AgentResponse {
    execute(
        &world.env,
        &ConfigFile::default(),
        AgentRequest::Search(args),
    )
}

fn search_model(f: &DeepFixture, cx: &TestAppContext) -> crate::deep_search::results::SearchModel {
    cx.read(|cx| match f.results.read(cx).body() {
        Body::Search(model) => model.clone(),
        other => panic!("not a search body: {other:?}"),
    })
}

/// D-001, D-010, D-026: a lexical search renders groups and hits that equal
/// the core data; the footer has the counts.
#[gpui_kit::test]
fn a_lexical_search_renders_groups_hits_and_the_footer(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.render(cx);
    f.search(cx, "alpha");
    let want = expected(&world, AgentSearchArgs::new("alpha"));
    let Some(AgentResult::Search(result)) = &want.result else {
        panic!("a search result");
    };
    let model = search_model(&f, cx);
    assert!(model.groups >= 3);
    assert_eq!(f.probe(cx, "deep.groups"), model.groups.to_string());
    assert_eq!(f.probe(cx, "deep.hits"), result.hits().len().to_string());
    assert_eq!(
        f.probe(cx, "deep.stats"),
        format!(
            "{} groups · {} hits · shortlisted {} · loaded {}",
            result.output.groups.len(),
            result.hits().len(),
            result.output.stats.shortlisted,
            result.output.stats.transcripts_loaded
        )
    );
    let SearchRow::Group(first) = &model.rows[0] else {
        panic!("the first row is a group card");
    };
    let group = &result.output.groups[0];
    assert_eq!(
        (first.rank, first.title.as_str()),
        (1, group.title.as_str())
    );
    assert_eq!(first.score_text(), format!("{:.6}", group.score));
    assert_eq!(first.shown, group.hits.len());
    assert_eq!(first.total, group.total_hits);
    let info = result.conversation(&group.conversation_ref).unwrap();
    assert_eq!(first.full_ref, info.full_ref);
    // The text of the toggle is the CLI text.
    assert_eq!(f.response(cx).text(), want.text());
    f.render(cx);
}

/// D-008: Flat has hit rows without group cards and labels Top; one hit per
/// conversation; Show duplicate hits changes the count as the text does.
#[gpui_kit::test]
fn layout_and_hit_options_change_the_output(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.with(cx, |pane, _, cx| pane.set_flat(true, cx));
    assert_eq!(f.probe(cx, "deep.top_label"), "Top · message hits");
    f.search(cx, "alpha");
    let flat = search_model(&f, cx);
    assert!(flat.flat && flat.groups == 0);
    assert!(flat.rows.iter().all(|row| matches!(row, SearchRow::Hit(_))));
    assert_eq!(f.probe(cx, "deep.groups"), "0");

    f.with(cx, |pane, _, cx| pane.set_flat(false, cx));
    assert_eq!(f.probe(cx, "deep.top_label"), "Top");
    f.with(cx, |pane, window, cx| {
        pane.set_field(Field::HitsPerConv, "1", window, cx)
    });
    f.search(cx, "retry");
    let one = search_model(&f, cx);
    assert!(one.groups >= 1);
    assert_eq!(one.hits, one.groups, "one hit per conversation card");
    let all = expected(
        &world,
        AgentSearchArgs {
            hits_per_conv: Some(1),
            ..AgentSearchArgs::new("retry")
        },
    );
    assert_eq!(f.response(cx).text(), all.text());

    f.with(cx, |pane, window, cx| {
        pane.set_field(Field::HitsPerConv, "5", window, cx);
        pane.set_all_hits(true, cx);
    });
    f.search(cx, "retry");
    let dups = expected(
        &world,
        AgentSearchArgs {
            hits_per_conv: Some(5),
            all_hits: true,
            ..AgentSearchArgs::new("retry")
        },
    );
    let text = f.response(cx);
    assert_eq!(text.text(), dups.text());
    assert_eq!(
        search_model(&f, cx).hits,
        text.text()
            .lines()
            .filter(|l| l.starts_with("hit "))
            .count()
    );
}

/// D-004: a quoted-only query shows the hint and the exact mode badge.
#[gpui_kit::test]
fn a_quoted_only_query_shows_the_exact_hint(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.search(cx, "\"refresh token\"");
    assert_eq!(f.probe(cx, "deep.exact_forced"), "true");
    assert_eq!(f.probe(cx, "deep.mode_badge"), "exact");
    f.search(cx, "refresh token");
    assert_eq!(f.probe(cx, "deep.exact_forced"), "false");
    assert_eq!(f.probe(cx, "deep.mode_badge"), "lexical");
}

/// D-009: the agent text toggle shows `response.text()` verbatim, Copy puts
/// it on the clipboard, a small budget cuts the output.
#[gpui_kit::test]
fn the_agent_text_toggle_shows_the_cli_text(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.search(cx, "alpha");
    let response = f.response(cx);
    f.results.update(cx, |_, _| {});
    cx.update_window(f.handle, |_, window, cx| {
        f.results
            .update(cx, |results, cx| results.set_text_shown(true, window, cx))
    })
    .unwrap();
    f.render(cx);
    let shown = cx.read(|cx| f.results.read(cx).text_view().displayed(cx));
    assert_eq!(shown.as_deref(), Some(response.text()));
    assert_eq!(f.probe(cx, "deep.text_shown"), "true");
    assert_eq!(f.probe(cx, "deep.text"), response.text());
    cx.update_window(f.handle, |_, window, cx| {
        f.results
            .update(cx, |results, cx| results.copy_text(window, cx))
    })
    .unwrap();
    assert_eq!(recording(cx).clipboard().as_deref(), Some(response.text()));

    f.with(cx, |pane, window, cx| {
        pane.set_field(Field::Budget, "200", window, cx)
    });
    f.search(cx, "alpha");
    let cut = f.response(cx);
    assert!(cut.text().contains("cut=tail"), "{}", cut.text());
    let shown = cx.read(|cx| f.results.read(cx).text_view().displayed(cx));
    assert_eq!(
        shown.as_deref(),
        Some(cut.text()),
        "the view follows the new run"
    );
    f.with(cx, |pane, window, cx| {
        pane.set_field(Field::Budget, "0", window, cx);
        pane.run(cx);
    });
    assert!(
        f.probe(cx, "deep.error").contains("'--budget'"),
        "{}",
        f.probe(cx, "deep.error")
    );
}

/// D-006: the uncached banner and its button; the stderr part of the text
/// view holds the upstream line verbatim.
#[gpui_kit::test]
fn the_uncached_banner_and_the_stderr_part(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    install_agent_embedder(cx);
    f.with(cx, |pane, _, cx| pane.set_mode(ModeChoice::Semantic, cx));
    f.search(cx, "retry");
    let banner = f.probe(cx, "deep.uncached");
    let count = banner
        .strip_suffix(" passage(s) are not cached. Generate the semantic cache to include them.")
        .or_else(|| {
            banner
                .split_once(
                    " passage(s) are not cached. Generate the semantic cache to include them.",
                )
                .map(|(n, _)| n)
        })
        .unwrap_or_else(|| panic!("banner text: {banner}"));
    let count: usize = count.parse().expect("the count");
    assert!(count > 0);
    let response = f.response(cx);
    assert!(response.stderr.contains(&format!(
        "Semantic search: {count} passage(s) are not cached; run claude-history --generate-semantic-cache"
    )));
    cx.update_window(f.handle, |_, window, cx| {
        f.results
            .update(cx, |results, cx| results.set_text_shown(true, window, cx))
    })
    .unwrap();
    f.render(cx);
    let notices = cx.read(|cx| f.results.read(cx).text_view().displayed_notices(cx));
    assert!(
        notices
            .unwrap()
            .contains("passage(s) are not cached; run claude-history --generate-semantic-cache")
    );
    // A real click on the button (the Deep search pane must be shown).
    cx.simulate_keystrokes(f.handle, "secondary-2");
    f.render(cx);
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("deep-results-generate", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let toast = cx.read(|cx| last_toast(cx).to_string());
    let dialog = cx
        .update_window(f.handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap();
    assert!(
        toast.contains("Generate semantic cache") || dialog,
        "the click reached its handler: toast {toast:?}"
    );
}

/// D-007: hybrid with a failing embedder keeps the lexical results and lists
/// `semantic-unavailable`.
#[gpui_kit::test]
fn a_failing_embedder_falls_back_with_a_warning(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    // A warm cache makes the global search embed its query.
    warm_semantic_cache(&world, &corpus.retry);
    let f = DeepFixture::open(cx, &world, &[]);
    install_failing_embedder(cx, "the model is gone");
    f.with(cx, |pane, _, cx| pane.set_mode(ModeChoice::Hybrid, cx));
    f.search(cx, "alpha");
    assert!(search_model(&f, cx).hits > 0, "lexical results are present");
    assert!(
        f.probe(cx, "deep.warnings")
            .contains("semantic-unavailable"),
        "{}",
        f.probe(cx, "deep.warnings")
    );
    assert_eq!(f.probe(cx, "deep.error"), "");
}

/// D-013: the malformed fixture gives one warning; expanding lists its ref.
#[gpui_kit::test]
fn the_warnings_banner_expands_to_the_refs(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.search(cx, "alpha");
    assert_eq!(
        f.probe(cx, "deep.warnings"),
        "1 warning: malformed-transcript 1"
    );
    // Collapsed first; the toggle click expands the list.
    cx.simulate_keystrokes(f.handle, "secondary-2");
    f.render(cx);
    assert!(!cx.read(|cx| f.results.read(cx).warnings_expanded()));
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("deep-results-warnings-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        cx.read(|cx| f.results.read(cx).warnings_expanded()),
        "the toggle click expands the list"
    );
    let details = cx.read(|cx| f.results.read(cx).warning_details());
    assert_eq!(details.len(), 1);
    assert!(
        details[0].starts_with("malformed-transcript ch_"),
        "{details:?}"
    );
}

/// D-003, C-031: the form shows the resolved `[agent]` defaults and the
/// response follows them.
#[gpui_kit::test]
fn the_form_follows_the_agent_config(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    world.write_config(Some("[agent]\ntop = 1\nmode = \"exact\"\n"), None);
    let f = DeepFixture::open(cx, &world, &[]);
    assert_eq!(f.probe(cx, "deep.mode_label"), "Default (exact)");
    assert_eq!(f.probe(cx, "deep.top_placeholder"), "Default (1)");
    f.search(cx, "alpha");
    assert_eq!(f.probe(cx, "deep.mode_badge"), "exact");
    assert_eq!(
        f.probe(cx, "deep.groups"),
        "1",
        "[agent] top = 1 returns one group"
    );
}

/// D-015: the Workspace scope is disabled without a workspace.
#[gpui_kit::test]
fn the_workspace_scope_needs_a_workspace(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    workspace_needle_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    assert!(cx.read(|cx| app_state(cx).workspace.is_none()));
    f.with(cx, |pane, _, cx| pane.set_scope(ScopeChoice::Workspace, cx));
    assert_eq!(
        f.probe(cx, "deep.scope"),
        "default",
        "the Workspace scope is disabled"
    );
    f.search(cx, "needle");
    assert_eq!(f.probe(cx, "deep.groups"), "2");
}

/// D-015: a Default scope that resolves to the workspace needs one.
#[gpui_kit::test]
fn a_local_default_scope_without_a_workspace_is_an_error(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    workspace_needle_corpus(&world);
    world.write_config(Some("[agent]\nscope = \"local\"\n"), None);
    let f = DeepFixture::open(cx, &world, &[]);
    assert_eq!(f.probe(cx, "deep.scope_label"), "Default (Workspace)");
    f.search(cx, "needle");
    assert!(
        f.probe(cx, "deep.error")
            .contains("local agent search requires a current project"),
        "{}",
        f.probe(cx, "deep.error")
    );
}

/// D-002, D-003: with a workspace the scopes filter the corpus.
#[gpui_kit::test]
fn the_scope_chooses_the_conversations(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (proj, _api) = workspace_needle_corpus(&world);
    world.write_config(Some("[agent]\nscope = \"local\"\n"), None);
    let f = DeepFixture::open(cx, &world, &["--workspace", &proj.display().to_string()]);
    assert_eq!(f.probe(cx, "deep.scope_label"), "Default (Workspace)");
    f.search(cx, "needle");
    assert_eq!(f.probe(cx, "deep.groups"), "1", "[agent] scope = local");
    f.with(cx, |pane, _, cx| pane.set_scope(ScopeChoice::All, cx));
    f.search(cx, "needle");
    assert_eq!(f.probe(cx, "deep.groups"), "2");
    f.with(cx, |pane, _, cx| pane.set_scope(ScopeChoice::Workspace, cx));
    f.search(cx, "needle");
    assert_eq!(f.probe(cx, "deep.groups"), "1");
}

/// D-016: an inverted time range is core's out-of-range error naming the
/// command; a valid range filters.
#[gpui_kit::test]
fn an_inverted_time_range_is_out_of_range(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.with(cx, |pane, _, cx| {
        pane.set_time(
            TimeTexts {
                since: "1d".into(),
                before: "3d".into(),
            },
            cx,
        )
    });
    f.search(cx, "alpha");
    let response = f.response(cx);
    let error = response.error.expect("a typed error");
    assert_eq!(error.kind.as_str(), "out-of-range");
    assert_eq!(error.reference.as_deref(), Some("command"));
    assert!(f.probe(cx, "deep.error").starts_with("out-of-range: "));
    f.with(cx, |pane, _, cx| pane.set_time(TimeTexts::default(), cx));
    f.search(cx, "alpha");
    assert_eq!(f.probe(cx, "deep.error"), "");
}

/// Wait until a gated embedder call is really blocking the worker.
fn wait_entered(cx: &mut TestAppContext, control: &AgentEmbedderControl, calls: usize) {
    cx.executor().allow_parking();
    let deadline = std::time::Instant::now() + super::WAIT;
    while control.entered.load(std::sync::atomic::Ordering::SeqCst) < calls {
        cx.run_until_parked();
        assert!(
            std::time::Instant::now() < deadline,
            "the embedder was not reached"
        );
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

/// D-033: Cancel shows `Search cancelled` at once; a new query cancels the
/// running one and only the second result renders.
#[gpui_kit::test]
fn cancel_and_single_flight(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    warm_semantic_cache(&world, &corpus.retry);
    let f = DeepFixture::open(cx, &world, &[]);
    let control = install_agent_embedder(cx);
    let gate = Gate::closed();
    *control.query_gate.lock().unwrap() = Some(gate.clone());
    f.with(cx, |pane, _, cx| pane.set_mode(ModeChoice::Semantic, cx));

    f.start(cx, "retry");
    assert_eq!(f.probe(cx, "deep.running"), "true");
    assert_eq!(f.probe(cx, "deep.status"), "Searching…");
    wait_entered(cx, &control, 1);
    let started = std::time::Instant::now();
    f.with(cx, |pane, _, cx| pane.cancel(cx));
    assert_eq!(f.probe(cx, "deep.status"), "Search cancelled");
    assert_eq!(f.probe(cx, "deep.running"), "false");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));

    // A new query while one runs: the first is cancelled, the second renders.
    f.start(cx, "retry");
    f.with(cx, |pane, _, cx| pane.set_mode(ModeChoice::Lexical, cx));
    f.start(cx, "login");
    assert_eq!(f.probe(cx, "deep.running"), "true");
    gate.open();
    f.wait_idle(cx);
    assert!(
        f.response(cx).text().contains("query text=login"),
        "{}",
        f.response(cx).text()
    );
    // The stale answers of the cancelled runs never replace it.
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(f.response(cx).text().contains("query text=login"));
    assert_eq!(f.probe(cx, "deep.status"), "");
}

/// D-033: while the semantic gate is exclusive a semantic search reports the
/// pause; a lexical one runs.
#[gpui_kit::test]
fn a_paused_gate_refuses_semantic_searches(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    install_agent_embedder(cx);
    f.with(cx, |pane, _, cx| {
        pane.pause_semantic(true, cx);
        pane.set_mode(ModeChoice::Semantic, cx);
    });
    f.search(cx, "retry");
    assert!(
        f.probe(cx, "deep.error").contains(SEMANTIC_PAUSED_TEXT),
        "{}",
        f.probe(cx, "deep.error")
    );
    f.with(cx, |pane, _, cx| pane.set_mode(ModeChoice::Lexical, cx));
    f.search(cx, "alpha");
    assert_eq!(f.probe(cx, "deep.error"), "");
    f.with(cx, |pane, _, cx| {
        pane.pause_semantic(false, cx);
        pane.set_mode(ModeChoice::Semantic, cx);
    });
    f.search(cx, "retry");
    assert_eq!(f.probe(cx, "deep.error"), "");
}

/// D-005: opening a hit emits OpenHit with the focus ordinal and the target
/// of the hit's conversation; Enter and Ctrl+Enter open the selected hit.
#[gpui_kit::test]
fn opening_a_hit_emits_the_target_and_the_ordinal(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    let events = f.record(cx);
    f.search(cx, "\"retry loop\"");
    assert_eq!(
        cx.read(|cx| f.deep.read(cx).selected_hit_target(cx))
            .map(|t| t.path),
        Some(corpus.retry.clone()),
        "the first hit is selected"
    );
    cx.dispatch_action(f.handle, crate::actions::FocusDeepSearch);
    f.render(cx);
    // The results list takes the keys.
    cx.update_window(f.handle, |_, window, cx| {
        f.results
            .update(cx, |results, cx| results.focus_rows(window, cx))
    })
    .unwrap();
    cx.simulate_keystrokes(f.handle, "enter");
    cx.run_until_parked();
    let first = events.borrow().last().cloned().expect("an OpenHit event");
    let DeepSearchEvent::OpenHit {
        target,
        reveal,
        focus_viewer,
        agent_tab,
    } = first;
    assert_eq!(target.path, corpus.retry);
    assert_eq!(target.title.as_ref(), "Token refresh retry");
    assert!(!focus_viewer && agent_tab.is_none());
    let RevealKind::AgentOrdinal { ordinal, .. } = reveal.kind else {
        panic!("an agent ordinal");
    };
    assert_eq!(ordinal, 2);
    assert_eq!(reveal.path, corpus.retry);
    assert!(
        reveal
            .full_ref
            .as_deref()
            .is_some_and(|r| r.starts_with("ch_"))
    );
    // The workspace opened it in the viewer.
    f.render(cx);
    let shown = cx.read(|cx| {
        f.ws.read(cx)
            .viewer()
            .read(cx)
            .target()
            .map(|t| t.path.clone())
    });
    assert_eq!(shown, Some(corpus.retry.clone()));

    // Ctrl+Enter focuses the viewer.
    f.with(cx, |_, _, _| {});
    f.results
        .update(cx, |results, cx| results.open_selected(true, cx));
    let last = events.borrow().last().cloned().unwrap();
    let DeepSearchEvent::OpenHit { focus_viewer, .. } = last;
    assert!(focus_viewer);
}

/// §5.4: the hit menu's actions: copy ref, read command, anchors, agent
/// records; the panel entries open the viewer's tabs (D-024, D-029, D-030).
#[gpui_kit::test]
fn the_hit_menu_actions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    let events = f.record(cx);
    f.search(cx, "\"retry loop\"");
    let response = f.response(cx);
    let Some(AgentResult::Search(result)) = &response.result else {
        panic!("a search result");
    };
    let hit = result.hits()[0].clone();
    let info = result.conversation(&hit.conversation_ref).unwrap().clone();
    let act = |cx: &mut TestAppContext, action: HitAction| {
        cx.update_window(f.handle, |_, window, cx| {
            f.results
                .update(cx, |results, cx| results.hit_action(0, action, window, cx))
        })
        .unwrap();
    };
    act(cx, HitAction::CopyRef);
    assert_eq!(
        recording(cx).clipboard(),
        Some(info.full_ref.clone()),
        "D-030"
    );
    act(cx, HitAction::CopyAnchors);
    let anchors = recording(cx).clipboard().unwrap();
    assert!(anchors.starts_with("ma_"), "{anchors}");
    assert_eq!(anchors, hit.anchors.join(","));
    act(cx, HitAction::CopyReadCommand);
    let mut command = format!(
        "claude-history agent read {}:m{}..m{} --focus m{}..m{}",
        info.full_ref,
        hit.read_range.start,
        hit.read_range.end,
        hit.focus_range.start,
        hit.focus_range.end
    );
    for (on, flag) in [
        (hit.render_options.tools, " --tools"),
        (hit.render_options.tool_results, " --tool-results"),
        (hit.render_options.thinking, " --thinking"),
        (hit.render_options.subagents, " --subagents"),
    ] {
        if on {
            command.push_str(flag);
        }
    }
    assert_eq!(recording(cx).clipboard(), Some(command));
    act(cx, HitAction::CopyAgentRecords);
    let records = recording(cx).clipboard().unwrap();
    assert!(response.text().contains(&records), "{records}");
    assert!(records.starts_with("hit ") && records.contains("\nread ref="));

    for (action, tab) in [
        (HitAction::SearchWithin, AgentTab::Within),
        (HitAction::Outline, AgentTab::Outline),
        (HitAction::ReadRange, AgentTab::Read),
    ] {
        act(cx, action);
        let last = events.borrow().last().cloned().unwrap();
        let DeepSearchEvent::OpenHit {
            agent_tab, target, ..
        } = last;
        assert_eq!(agent_tab, Some(tab));
        assert_eq!(target.path, info.path);
    }
    act(cx, HitAction::Open);
    let DeepSearchEvent::OpenHit { agent_tab, .. } = events.borrow().last().cloned().unwrap();
    assert_eq!(agent_tab, None);
}

/// Esc cancels a running search, then clears the query, then returns to the
/// Conversations tab.
#[gpui_kit::test]
fn escape_cancels_clears_then_leaves(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    warm_semantic_cache(&world, &corpus.retry);
    let f = DeepFixture::open(cx, &world, &[]);
    let control = install_agent_embedder(cx);
    let gate = Gate::closed();
    *control.query_gate.lock().unwrap() = Some(gate.clone());
    cx.dispatch_action(f.handle, crate::actions::FocusDeepSearch);
    f.render(cx);
    f.with(cx, |pane, _, cx| pane.set_mode(ModeChoice::Semantic, cx));
    f.start(cx, "retry");
    wait_entered(cx, &control, 1);
    f.with(cx, |pane, window, cx| pane.focus_query(window, cx));
    f.render(cx);
    cx.simulate_keystrokes(f.handle, "escape");
    assert_eq!(f.probe(cx, "deep.status"), "Search cancelled");
    assert_eq!(f.probe(cx, "deep.query"), "retry");
    cx.simulate_keystrokes(f.handle, "escape");
    assert_eq!(f.probe(cx, "deep.query"), "");
    cx.simulate_keystrokes(f.handle, "escape");
    cx.run_until_parked();
    let focus = cx
        .update_window(f.handle, |_, window, cx| {
            f.ws.update(cx, |ws, cx| ws.qa_probe("focus", window, cx))
        })
        .unwrap();
    assert_eq!(focus.as_deref(), Some("list"));
    gate.open();
    f.wait_idle(cx);
}

/// C-038: the Options disclosure is remembered.
#[gpui_kit::test]
fn the_options_disclosure_is_remembered(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let f = DeepFixture::open(cx, &world, &[]);
    assert_eq!(f.probe(cx, "deep.options"), "false");
    f.with(cx, |pane, _, cx| pane.set_options_open(true, cx));
    assert_eq!(f.probe(cx, "deep.options"), "true");
    assert!(cx.read(|cx| app_state(cx).ui.deep_options_open));
    f.render(cx);
}

/// D-010: group cards and hit rows name the project the way the conversation
/// list does (the cwd the transcript records), also for a path with `-` and
/// for Pi sessions whose folder is not in Claude's encoding.
#[gpui_kit::test]
fn project_labels_equal_the_lists_names(cx: &mut TestAppContext) {
    use crate::test_support::fixtures_agent::{SessionBuilder, project_cwd};
    let world = TestWorld::new();
    agent_corpus(&world);
    SessionBuilder::new(60, project_cwd("alpha-beta-gamma"))
        .summary("Dashed project")
        .user("zebrafinch question")
        .assistant("zebrafinch answer")
        .write(&world);
    let pi_text = crate::test_support::synth::PI_SESSION
        .replace("/tmp/pi-project", "/tmp/dup-a")
        .replace("caching", "zebrafinch");
    world.add_fixture(
        ".pi/agent/sessions/--a--/2024-03-02T00-00-00-000Z_01912345-6789-7abc-8def-0123456789ab.jsonl",
        pi_text.as_bytes(),
    );
    let f = DeepFixture::open(cx, &world, &[]);
    // The labels come from the loaded list: a search that answers before the
    // list's load ends falls back to the folder name.
    f.wait_list_loaded(cx);
    f.render(cx);
    f.search(cx, "zebrafinch");
    let model = search_model(&f, cx);
    let list = cx.read(|cx| {
        crate::globals::main_panes(cx)
            .list()
            .expect("the list pane")
    });
    let mut seen = Vec::new();
    for row in &model.rows {
        let (info, label) = match row {
            SearchRow::Group(group) => (group.info.clone(), group.project.clone()),
            SearchRow::Hit(hit) => (hit.info.clone(), hit.project.clone()),
        };
        let info = info.expect("a conversation");
        let want = cx.read(|cx| {
            list.read(cx)
                .conversation(&info.path)
                .and_then(|conversation| conversation.project_name.clone())
        });
        assert_eq!(Some(label.clone()), want, "{}", info.path.display());
        seen.push(label);
    }
    assert!(
        seen.iter().any(|label| label == "alpha-beta-gamma"),
        "{seen:?}"
    );
    assert!(seen.iter().any(|label| label == "dup-a"), "{seen:?}");
}

/// D-010, X-024b: a flat hit row shows the score (with the breakdown of a
/// semantic hit), a ref tooltip and an accessible name.
#[gpui_kit::test]
fn hit_rows_carry_score_ref_and_accessible_name(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.search(cx, "alpha");
    let model = search_model(&f, cx);
    let hit = model
        .rows
        .iter()
        .find_map(|row| match row {
            SearchRow::Hit(hit) => Some(hit.clone()),
            _ => None,
        })
        .expect("a hit row");
    assert!(hit.score_text().starts_with(&format!("{:.6}", hit.score)));
    assert!(hit.tooltip().starts_with(&hit.full_ref));
    assert!(hit.tooltip().contains(&hit.score_text()));
    let label = hit.accessibility_label();
    assert!(
        label.contains(&hit.title) && label.contains(&hit.project),
        "{label}"
    );
    assert!(label.contains(&hit.score_text()), "{label}");
    let semantic = crate::deep_search::results::HitModel {
        breakdown: Some((0.5, 0.25)),
        ..hit
    };
    assert!(semantic.score_text().ends_with("sem 0.500 lex 0.250"));
}
