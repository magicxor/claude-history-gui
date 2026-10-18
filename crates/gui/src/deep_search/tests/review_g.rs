//! Regression tests of the WP4 review findings: explicit out-of-range
//! reads, result-set ownership of reads, session-action targets, deleted
//! hits, the virtualized Read list, the semantic gate on a running job and
//! the pure helpers (match lines, continue-read commands, full-ref
//! candidates).

use std::sync::Arc;
use std::time::Instant;

use claude_history_core::agent::AgentRequest;
use claude_history_core::agent::args::{AgentSearchArgs, AgentSearchModeArgs};
use claude_history_core::config::ConfigFile;
use claude_history_core::search::mode::SearchMode;
use gpui_kit::TestAppContext;

use super::{DeepFixture, PanelFixture};
use crate::deep_search::results::{Body, continue_commands, is_match_line};
use crate::deep_search::worker::{
    AgentJob, AgentJobKind, AgentReply, AgentWorkerHandle, SEMANTIC_PAUSED_TEXT, expand_handles,
};
use crate::globals::{CorpusBus, CorpusNotice};
use crate::model::AgentTab;
use crate::test_support::TestWorld;
use crate::test_support::fixtures_agent::{
    AgentEmbedderControl, agent_corpus, agent_embedder_factory, duplicate_uuid_sessions,
    outline_session, warm_semantic_cache,
};
use crate::test_support::fixtures_list::Gate;

fn run_read(f: &PanelFixture, cx: &mut TestAppContext, refs: &str) {
    let refs = refs.to_string();
    f.with(cx, |panel, window, cx| {
        panel.set_refs(&refs, window, cx);
        panel.run_read(cx);
    });
    f.wait_idle(cx);
}

/// D-014: an explicit range past the end of the transcript keeps the typed
/// `out-of-range` error and the user's text; only Load next clamps.
#[gpui_kit::test]
fn an_explicit_out_of_range_read_is_an_error(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.outline25, AgentTab::Read);
    let key = f.probe(cx, "agent.ref");
    let typed = format!("{key}:m20..m30");
    run_read(&f, cx, &typed);
    assert_eq!(
        f.probe(cx, "agent.error"),
        "out-of-range: message range m20..m30 exceeds transcript length m25"
    );
    assert_eq!(
        cx.read(|cx| f.panel.read(cx).read.refs.read(cx).value().to_string()),
        typed,
        "the Refs field is not rewritten"
    );
    assert_eq!(f.probe(cx, "agent.rows"), "0");
}

/// D-034/D-021: a Read of another conversation belongs to that
/// conversation: the set's target and ref are its own, the stale banner is
/// about the viewer moving away from it.
#[gpui_kit::test]
fn a_read_set_belongs_to_the_conversation_it_read(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.login, AgentTab::Read);
    let login = f.probe(cx, "agent.ref");
    f.open_conversation(cx, &corpus.retry, Some(AgentTab::Read));
    assert_ne!(f.probe(cx, "agent.ref"), login);
    run_read(&f, cx, &format!("{login}:m1..m1"));
    let (path, full_ref) = cx.read(|cx| {
        let panel = f.panel.read(cx);
        let set = panel.set_of(AgentTab::Read).expect("a read set");
        (set.target.path.clone(), set.full_ref.clone())
    });
    assert_eq!(path, corpus.login);
    assert_eq!(full_ref, login);
    assert_eq!(f.probe(cx, "agent.stale"), "true", "the viewer shows retry");

    // The viewer moves to the conversation that was read: not stale, and
    // the set takes the viewer's richer target (title).
    f.open_conversation(cx, &corpus.login, Some(AgentTab::Read));
    assert_eq!(f.probe(cx, "agent.stale"), "false");
    assert_eq!(f.probe(cx, "agent.set"), "Fix login bug");
}

/// §7.8: session actions with the panel focused use the result set's
/// target (title, project, custom title of the viewer), not the row's
/// file-stem target.
#[gpui_kit::test]
fn the_panel_target_is_the_result_sets(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.outline12, AgentTab::Outline);
    let viewer = cx.read(|cx| f.ws.read(cx).viewer().read(cx).target().cloned().unwrap());
    let target = cx
        .read(|cx| f.panel.read(cx).selected_target())
        .expect("a target");
    assert_eq!(target.path, viewer.path);
    assert_eq!(target.title, viewer.title, "not the file stem");
    assert_eq!(target.project_name, viewer.project_name);
    assert_eq!(target.custom_title, viewer.custom_title);
}

/// D-021: only `>N:` lines of a `match=` slice are match lines; markdown
/// quotes are not.
#[test]
fn only_numbered_marker_lines_are_matches() {
    assert!(is_match_line(">12: retry"));
    assert!(!is_match_line("> a quote"));
    assert!(!is_match_line("  >3: indented"));
    assert!(!is_match_line(" 12: context"));
}

/// D-021: the `continue read` lines of the agent text become commands.
#[test]
fn continue_read_lines_become_commands() {
    let text = "message m1 role=user\nbody\ncontinue read refs=ch_aaaaaaaaaaaa:m3..m5%3Bch_bbbbbbbbbbbb:m1..m2\ncontinue read ref=ch_cccccccccccc:m7..m9\n";
    assert_eq!(
        continue_commands(text),
        vec![
            "claude-history agent read ch_aaaaaaaaaaaa:m3..m5 ch_bbbbbbbbbbbb:m1..m2",
            "claude-history agent read ch_cccccccccccc:m7..m9"
        ]
    );
    assert!(continue_commands("nothing to continue").is_empty());
}

/// D-014: candidates carry the full ref, computed from the directory and
/// file of the candidate line.
#[test]
fn candidate_handles_expand_to_full_refs() {
    use claude_history_core::agent::refs::AgentConversationRef;
    let full = AgentConversationRef::from_parts("-proj", "a.jsonl").full_ref();
    let detail = "ambiguous conversation ref; retry with one of these ch_ handles; candidates: ch_aaaaaaaaaaaa project=pr_1 source=claude directory=-proj file=a.jsonl\n  ch_bbbbbbbbbbbb project=pr_2 source=pi directory=x file=a.jsonl";
    let expanded = expand_handles(detail, &[]);
    assert!(
        expanded.contains(&format!("candidates: {full} project=pr_1")),
        "{expanded}"
    );
    assert!(
        expanded.contains("\n  ch_bbbbbbbbbbbb project=pr_2"),
        "non-Claude candidates without a discovered key keep their handle: {expanded}"
    );
    // With the discovered keys a Pi candidate becomes a durable full ref too.
    let mut pi = claude_history_core::agent::refs::AgentConversationKey::new(
        "x",
        "a.jsonl",
        std::path::PathBuf::from("x/a.jsonl"),
    );
    pi.source = claude_history_core::history::Source::Pi;
    pi.session_id = "pi-session".to_string();
    let with_keys = expand_handles(detail, std::slice::from_ref(&pi));
    let pi_full = pi.conversation_ref().full_ref();
    assert!(
        with_keys.contains(&format!("\n  {pi_full} project=pr_2")),
        "{with_keys}"
    );
    assert_eq!(
        crate::deep_search::results::ambiguous_candidates(&with_keys),
        vec![full.clone(), pi_full]
    );
    assert_eq!(
        crate::deep_search::results::ambiguous_candidates(&expanded)[0],
        full
    );
}

/// D-014: the real ambiguity of one UUID in two projects offers full refs.
#[gpui_kit::test]
fn ambiguous_candidates_are_full_refs(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let (dup_a, _) = duplicate_uuid_sessions(&world, 77);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Read);
    let uuid = dup_a.file_stem().unwrap().to_string_lossy().to_string();
    run_read(&f, cx, &uuid);
    let candidates = cx.read(|cx| {
        f.panel
            .read(cx)
            .results_of(AgentTab::Read)
            .read(cx)
            .error()
            .map(|error| error.candidates.clone())
            .unwrap()
    });
    assert_eq!(candidates.len(), 2);
    for candidate in &candidates {
        assert_eq!(candidate.len(), 35, "ch_ + 32 hex: {candidate}");
    }
    // A full ref resolves.
    run_read(&f, cx, &candidates[0]);
    assert_eq!(f.probe(cx, "agent.error"), "");
}

/// D-014 / CCR: one Pi (or OMP) session id in two session folders is
/// ambiguous; the candidates of the real `AgentService` error become durable
/// full refs through the lazy key discovery, and each one resolves.
#[gpui_kit::test]
fn ambiguous_pi_and_omp_candidates_are_full_refs_end_to_end(cx: &mut TestAppContext) {
    use crate::test_support::synth;
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let pi_id = "01912345-6789-7abc-8def-0123456789ab";
    let omp_id = "01a082ad-c8d8-7149-8d3e-dd8f74bb7ed4";
    for (root, text, folder_id, cwd_from) in [
        (".pi", synth::PI_SESSION, pi_id, "/tmp/pi-project"),
        (".omp", synth::OMP_SESSION, omp_id, "/tmp/omp-project"),
    ] {
        for (dir, cwd) in [("--a--", "/tmp/dup-a"), ("--b--", "/tmp/dup-b")] {
            world.add_fixture(
                &format!("{root}/agent/sessions/{dir}/2024-03-02T00-00-00-000Z_{folder_id}.jsonl"),
                text.replace(cwd_from, cwd)
                    .replace("omp_session_custom_id", omp_id)
                    .as_bytes(),
            );
        }
    }
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Read);
    for (id, source) in [(pi_id, "pi"), (omp_id, "omp")] {
        run_read(&f, cx, id);
        let (candidates, detail) = cx.read(|cx| {
            let results = f.panel.read(cx).results_of(AgentTab::Read);
            let error = results
                .read(cx)
                .error()
                .cloned()
                .expect("an ambiguity error");
            (error.candidates.clone(), error.text.clone())
        });
        assert!(detail.contains(&format!(" source={source} ")), "{detail}");
        assert_eq!(candidates.len(), 2, "{detail}");
        for candidate in &candidates {
            assert_eq!(
                candidate.len(),
                35,
                "ch_ + 32 hex, not a handle: {candidate}"
            );
        }
        run_read(&f, cx, &candidates[0]);
        assert_eq!(f.probe(cx, "agent.error"), "", "{source}");
    }
}

/// PD15: a deleted conversation's hits stay visible but no longer open or
/// act.
#[gpui_kit::test]
fn deleted_hits_are_disabled(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let f = DeepFixture::open(cx, &world, &[]);
    f.search(cx, "alpha");
    let events = f.record(cx);
    let target = f.with(cx, |pane, _, cx| pane.selected_hit_target(cx));
    let path = target.expect("a selected hit").path;
    cx.update(|cx| CorpusBus::broadcast(CorpusNotice::Deleted(vec![path]), cx));
    cx.run_until_parked();
    assert!(
        f.with(cx, |pane, _, cx| pane.selected_hit_target(cx))
            .is_none()
    );
    f.results
        .update(cx, |results, cx| results.open_selected(false, cx));
    assert!(
        events.borrow().is_empty(),
        "no Open event: {:?}",
        events.borrow()
    );
}

/// §7.6: closing the gate cancels a semantic job that is already running
/// and answers it with the paused text.
#[test]
fn closing_the_gate_stops_a_running_semantic_job() {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    warm_semantic_cache(&world, &corpus.retry);
    let control = Arc::new(AgentEmbedderControl::default());
    let gate = Gate::closed();
    *control.query_gate.lock().unwrap() = Some(gate.clone());
    let worker = AgentWorkerHandle::start(agent_embedder_factory(control.clone()));
    let client = worker.client();
    client.submit(AgentJob {
        kind: AgentJobKind::Search,
        request: AgentRequest::Search(AgentSearchArgs {
            search_mode: AgentSearchModeArgs::explicit(SearchMode::Semantic),
            ..AgentSearchArgs::new("retry")
        }),
        env: world.env.clone(),
        config: ConfigFile::default(),
    });
    let deadline = Instant::now() + super::WAIT;
    while control.entered.load(std::sync::atomic::Ordering::SeqCst) < 1 {
        assert!(Instant::now() < deadline, "the embedder was not reached");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    worker.pause_semantic(true);
    gate.open();
    let replies = client.replies();
    let response = loop {
        if let Ok(AgentReply::Done { response, .. }) = replies.try_recv() {
            break response;
        }
        assert!(Instant::now() < deadline, "no answer");
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    assert!(
        response.text().contains(SEMANTIC_PAUSED_TEXT),
        "{}",
        response.text()
    );
}

/// Rule 18: a long read renders only the visible cards: the frame of a
/// 1,500-message read stays far below the cost of 1,500 cards.
#[gpui_kit::test]
fn a_long_read_is_virtualized(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    let big = outline_session(&world, 90, "Big conversation", 1500);
    let f = PanelFixture::open(cx, &world, &[], &big, AgentTab::Read);
    run_read(&f, cx, "");
    let Body::Read(model) = cx.read(|cx| {
        f.panel
            .read(cx)
            .results_of(AgentTab::Read)
            .read(cx)
            .body()
            .clone()
    }) else {
        panic!("a read body");
    };
    assert_eq!(model.groups[0].cards.len(), 1500);
    f.render(cx);
    let started = Instant::now();
    for _ in 0..3 {
        f.render(cx);
    }
    let per_frame = started.elapsed() / 3;
    assert!(
        per_frame < std::time::Duration::from_millis(200),
        "a frame took {per_frame:?}"
    );
    // Selecting a far row scrolls the list without rendering everything.
    let results = f.results(cx, AgentTab::Read);
    results.update(cx, |results, cx| results.select(1200, cx));
    let started = Instant::now();
    f.render(cx);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(1500),
        "selecting row 1200 took {:?}",
        started.elapsed()
    );
}

/// `[agent]` changes update the `Default (N)` placeholders of the panel.
#[gpui_kit::test]
fn the_panel_placeholders_follow_the_agent_config(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let f = PanelFixture::open(cx, &world, &[], &corpus.retry, AgentTab::Within);
    assert_eq!(f.probe(cx, "agent.within_top_placeholder"), "Default (20)");
    cx.update(|cx| {
        crate::globals::update_app_state(cx, |state, _| {
            let agent = state
                .config
                .effective
                .agent
                .get_or_insert_with(Default::default);
            agent.within_top = Some(3);
        })
    });
    cx.run_until_parked();
    f.render(cx);
    assert_eq!(f.probe(cx, "agent.within_top_placeholder"), "Default (3)");
}
