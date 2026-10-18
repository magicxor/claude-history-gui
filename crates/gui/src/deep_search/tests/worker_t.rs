//! The agent worker: pending slots per client and kind, cancellation (K7),
//! the semantic gate and the key lookup (K8). Plain threads, no window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use claude_history_core::agent::args::{AgentOutlineArgs, AgentSearchArgs, AgentSearchModeArgs};
use claude_history_core::agent::{AgentRequest, execute};
use claude_history_core::config::ConfigFile;
use claude_history_core::search::mode::SearchMode;

use crate::deep_search::worker::{
    AgentClient, AgentJob, AgentJobKind, AgentReply, AgentWorkerHandle, SEMANTIC_PAUSED_TEXT,
};
use crate::test_support::TestWorld;
use crate::test_support::fixtures_agent::{
    AgentEmbedderControl, agent_corpus, agent_embedder_factory, copy_outside_roots,
    warm_semantic_cache,
};
use crate::test_support::fixtures_list::Gate;

fn wait_reply(client: &AgentClient) -> AgentReply {
    let replies = client.replies();
    let deadline = Instant::now() + super::WAIT;
    loop {
        if let Ok(reply) = replies.try_recv() {
            return reply;
        }
        assert!(Instant::now() < deadline, "no reply from the worker");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The next `Done` (progress notices are skipped).
fn wait_done(
    client: &AgentClient,
) -> (AgentJobKind, u64, claude_history_core::agent::AgentResponse) {
    loop {
        if let AgentReply::Done {
            kind,
            generation,
            response,
        } = wait_reply(client)
        {
            return (kind, generation, response);
        }
    }
}

/// Wait until a gated embedder call blocks the worker.
fn wait_entered(control: &AgentEmbedderControl, calls: usize) {
    let deadline = Instant::now() + super::WAIT;
    while control.entered.load(std::sync::atomic::Ordering::SeqCst) < calls {
        assert!(Instant::now() < deadline, "the embedder was not reached");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn job(world: &TestWorld, kind: AgentJobKind, request: AgentRequest) -> AgentJob {
    AgentJob {
        kind,
        request,
        env: world.env.clone(),
        config: ConfigFile::default(),
    }
}

fn search_job(world: &TestWorld, query: &str, mode: Option<SearchMode>) -> AgentJob {
    job(
        world,
        AgentJobKind::Search,
        AgentRequest::Search(AgentSearchArgs {
            search_mode: mode
                .map_or_else(AgentSearchModeArgs::default, AgentSearchModeArgs::explicit),
            ..AgentSearchArgs::new(query)
        }),
    )
}

fn worker(control: &Arc<AgentEmbedderControl>) -> AgentWorkerHandle {
    AgentWorkerHandle::start(agent_embedder_factory(control.clone()))
}

/// The response is the core's, verbatim.
#[test]
fn a_job_answers_with_the_core_response() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let worker = worker(&Arc::default());
    let client = worker.client();
    let generation = client.submit(search_job(&world, "alpha", None));
    let (kind, answered, response) = wait_done(&client);
    assert_eq!((kind, answered), (AgentJobKind::Search, generation));
    let expected = execute(
        &world.env,
        &ConfigFile::default(),
        AgentRequest::Search(AgentSearchArgs::new("alpha")),
    );
    assert_eq!(response.text(), expected.text());
    assert_eq!(response.stderr, expected.stderr);
    assert!(response.is_success());
}

/// K7: a newer job of the same kind cancels the running one and replaces the
/// queued one; another client's job is untouched.
#[test]
fn a_new_job_cancels_its_own_and_spares_other_clients() {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    warm_semantic_cache(&world, &corpus.retry);
    let control = Arc::new(AgentEmbedderControl::default());
    let gate = Gate::closed();
    *control.query_gate.lock().unwrap() = Some(gate.clone());
    let worker = worker(&control);
    let a = worker.client();
    let b = worker.client();

    // A's semantic search blocks inside the embedder.
    let first = a.submit(search_job(&world, "retry", Some(SearchMode::Semantic)));
    wait_entered(&control, 1);
    // B's outline queues behind it; A's second search cancels the first.
    let outline = b.submit(job(
        &world,
        AgentJobKind::Outline,
        AgentRequest::Outline(AgentOutlineArgs::new("ch_nothing")),
    ));
    let second = a.submit(search_job(&world, "alpha", None));
    assert!(first < outline && outline < second);
    gate.open();

    let (_, answered, response) = wait_done(&a);
    assert_eq!(answered, first);
    assert!(
        response.is_cancelled(),
        "the running job was cancelled: {response:?}"
    );
    let (kind, answered, _) = wait_done(&b);
    assert_eq!((kind, answered), (AgentJobKind::Outline, outline));
    let (_, answered, response) = wait_done(&a);
    assert_eq!(answered, second);
    assert!(response.is_success());
}

/// `cancel(kind)` drops the queued job: only later jobs answer.
#[test]
fn cancel_drops_the_queued_job() {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    warm_semantic_cache(&world, &corpus.retry);
    let control = Arc::new(AgentEmbedderControl::default());
    let gate = Gate::closed();
    *control.query_gate.lock().unwrap() = Some(gate.clone());
    let worker = worker(&control);
    let a = worker.client();
    let b = worker.client();
    let blocker = a.submit(search_job(&world, "retry", Some(SearchMode::Semantic)));
    wait_entered(&control, 1);
    let queued = b.submit(search_job(&world, "alpha", None));
    b.cancel(AgentJobKind::Search);
    let later = b.submit(search_job(&world, "login", None));
    gate.open();
    assert_eq!(wait_done(&a).1, blocker);
    let (_, answered, response) = wait_done(&b);
    assert_eq!(answered, later, "the cancelled job {queued} never answers");
    assert!(response.is_success());
}

/// §7.6: while paused, semantic and hybrid jobs fail fast (the shared
/// worker refuses them); lexical, read and outline jobs run.
#[test]
fn the_semantic_gate_refuses_semantic_jobs() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let worker = worker(&Arc::default());
    let client = worker.client();
    worker.pause_semantic(true);
    assert!(client.is_semantic_paused());
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        client.submit(search_job(&world, "retry", Some(mode)));
        let (_, _, response) = wait_done(&client);
        assert!(!response.is_success());
        assert!(
            response.text().contains(SEMANTIC_PAUSED_TEXT),
            "{}",
            response.text()
        );
        assert!(response.error.is_none() && !response.is_cancelled());
    }
    client.submit(search_job(&world, "alpha", Some(SearchMode::Lexical)));
    assert!(wait_done(&client).2.is_success());
    // A quoted-only query forces exact, which is not semantic.
    client.submit(search_job(
        &world,
        "\"retry loop\"",
        Some(SearchMode::Semantic),
    ));
    assert!(wait_done(&client).2.is_success());
    worker.pause_semantic(false);
    client.submit(search_job(&world, "retry", Some(SearchMode::Semantic)));
    assert!(wait_done(&client).2.is_success());
}

/// K8: the key of a viewer file; a file outside the history roots has none.
#[test]
fn the_key_lookup_resolves_files_inside_the_roots() {
    let world = TestWorld::new();
    let corpus = agent_corpus(&world);
    let outside = copy_outside_roots(&world, &corpus.retry);
    let worker = worker(&Arc::default());
    let client = worker.client();
    for (path, has_key) in [(&corpus.retry, true), (&outside, false)] {
        let generation = client.submit(job(
            &world,
            AgentJobKind::KeyForPath(path.clone()),
            AgentRequest::Outline(AgentOutlineArgs::default()),
        ));
        let AgentReply::Key {
            generation: answered,
            path: answered_path,
            full_ref,
            ..
        } = wait_reply(&client)
        else {
            panic!("a key reply");
        };
        assert_eq!((answered, &answered_path), (generation, path));
        assert_eq!(full_ref.is_some(), has_key, "{path:?}");
        if let Some(full_ref) = full_ref {
            assert!(full_ref.starts_with("ch_") && full_ref.len() > 15);
        }
    }
}

/// Notices are delivered while the job runs (progress), then the response.
#[test]
fn progress_notices_arrive_before_the_response() {
    let world = TestWorld::new();
    agent_corpus(&world);
    let worker = worker(&Arc::default());
    let client = worker.client();
    client.submit(search_job(&world, "retry", Some(SearchMode::Semantic)));
    let mut progress = 0;
    loop {
        match wait_reply(&client) {
            AgentReply::Progress { kind, .. } => {
                assert_eq!(kind, AgentJobKind::Search);
                progress += 1;
            }
            AgentReply::Done { response, .. } => {
                assert_eq!(
                    progress,
                    response.notices.len(),
                    "every notice was forwarded"
                );
                assert!(progress > 0, "a semantic search reports its progress");
                break;
            }
            AgentReply::Key { .. } => panic!("no key job was submitted"),
        }
    }
}
