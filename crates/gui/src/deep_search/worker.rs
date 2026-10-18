//! The `agent-worker` thread (DESIGN §7.5, §11.14). WP4.
//!
//! One worker thread per process ([`AgentWorkerHandle`], a global). Every
//! consumer — the Deep search pane and each agent panel (the main window's
//! and every single-file window's) — takes its own [`AgentClient`]: the
//! client's replies arrive on its own async channel, and the worker keeps one
//! pending slot per (client, request kind) — submitting a job of a kind
//! cancels that client's running job of the kind (K7) and replaces its queued
//! one, without touching other clients' jobs. While the semantic gate is
//! exclusive, semantic/hybrid jobs fail fast with
//! [`SEMANTIC_PAUSED_TEXT`].
//!
//! The thread builds an `AgentService` per job (it is not `Send`): the shared
//! model comes from the `Send + Sync` factory of `embedder::browser_factory`
//! captured when the handle is created, so Clear/Generate and the fake models
//! of tests reach it through the one `SharedEmbedder`. Notices become
//! [`AgentReply::Progress`] while the job runs; the outcome is one
//! [`AgentReply::Done`] (a cancelled job answers with the core's cancelled
//! response; consumers drop replies of stale generations).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use claude_history_core::HistoryEnv;
use claude_history_core::agent::refs::conversation_key_for_path;
use claude_history_core::agent::search::effective_agent_mode;
use claude_history_core::agent::{AgentNotice, AgentRequest, AgentResponse, AgentService};
use claude_history_core::browser::SemanticEmbedderFactory;
use claude_history_core::config::ConfigFile;
use claude_history_core::search::mode::SearchMode;
use claude_history_core::semantic::types::SemanticCancellationToken;
use gpui_kit::{App, Global};

use crate::embedder;

/// What a semantic/hybrid job reports while the semantic gate is exclusive.
pub const SEMANTIC_PAUSED_TEXT: &str = "Semantic search is paused while the cache is generated";

/// The kind of an agent job (one pending slot per client and kind).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AgentJobKind {
    Search,
    Within,
    Read,
    Outline,
    /// Resolve the agent conversation key of a viewer file (K8).
    KeyForPath(PathBuf),
}

impl AgentJobKind {
    /// The slot a job of this kind occupies (all `KeyForPath` jobs of a
    /// client share one: only the latest path matters).
    fn slot(&self) -> u8 {
        match self {
            AgentJobKind::Search => 0,
            AgentJobKind::Within => 1,
            AgentJobKind::Read => 2,
            AgentJobKind::Outline => 3,
            AgentJobKind::KeyForPath(_) => 4,
        }
    }
}

/// A request for the worker.
pub struct AgentJob {
    pub kind: AgentJobKind,
    pub request: AgentRequest,
    /// The environment with the current workspace applied.
    pub env: HistoryEnv,
    /// The effective configuration.
    pub config: ConfigFile,
}

/// A worker reply, tagged with the job's generation (stale replies are dropped).
// A response is much larger than the other replies; replies are moved through
// a channel once each, so the size difference costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
pub enum AgentReply {
    Progress {
        kind: AgentJobKind,
        generation: u64,
        notice: AgentNotice,
    },
    Done {
        kind: AgentJobKind,
        generation: u64,
        response: AgentResponse,
    },
    Key {
        generation: u64,
        path: PathBuf,
        full_ref: Option<String>,
        /// The lookup itself failed (not "outside the roots"): its message.
        error: Option<String>,
    },
}

/// A queued job.
struct Pending {
    client: u64,
    generation: u64,
    job: AgentJob,
    sender: async_channel::Sender<AgentReply>,
    token: SemanticCancellationToken,
}

/// The job the worker thread runs.
struct Running {
    client: u64,
    slot: u8,
    /// A semantic or hybrid search: the gate cancels it when it closes.
    semantic: bool,
    token: SemanticCancellationToken,
}

#[derive(Default)]
struct State {
    pending: VecDeque<Pending>,
    running: Option<Running>,
    closed: bool,
}

struct Inner {
    state: Mutex<State>,
    wake: Condvar,
    generation: AtomicU64,
    next_client: AtomicU64,
    semantic_paused: AtomicBool,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Closes the worker's queue when the last handle is gone.
struct Shutdown(Arc<Inner>);

impl Drop for Shutdown {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.closed = true;
        for pending in &state.pending {
            pending.token.cancel();
        }
        if let Some(running) = &state.running {
            running.token.cancel();
        }
        state.pending.clear();
        drop(state);
        self.0.wake.notify_all();
    }
}

/// The process's agent worker (a global, created lazily).
#[derive(Clone)]
pub struct AgentWorkerHandle {
    inner: Arc<Inner>,
    _shutdown: Arc<Shutdown>,
}

impl Global for AgentWorkerHandle {}

impl AgentWorkerHandle {
    /// The worker of this process (created on first use).
    pub fn global(cx: &mut App) -> AgentWorkerHandle {
        if cx.try_global::<AgentWorkerHandle>().is_none() {
            // The shared model's `Send + Sync` factory: the worker thread
            // builds the (non-`Send`) core factory from it per job.
            embedder::shared(cx);
            let factory = embedder::browser_factory(cx);
            let handle = Self::start(factory);
            cx.set_global(handle);
        }
        cx.global::<AgentWorkerHandle>().clone()
    }

    /// A worker with its own thread and embedder factory (the global is
    /// created by [`AgentWorkerHandle::global`]; tests start their own).
    pub(crate) fn start(factory: SemanticEmbedderFactory) -> AgentWorkerHandle {
        let inner = Arc::new(Inner {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            generation: AtomicU64::new(0),
            next_client: AtomicU64::new(0),
            semantic_paused: AtomicBool::new(false),
        });
        let thread_inner = inner.clone();
        let spawned = std::thread::Builder::new()
            .name("agent-worker".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || worker_loop(thread_inner, factory));
        if let Err(error) = spawned {
            // Jobs then never answer; the panes keep showing "Searching…".
            // There is no better recovery for a process that cannot spawn a
            // thread.
            claude_history_core::debug::error(
                Some(claude_history_core::debug::DebugLevel::Error),
                &format!("could not start the agent worker: {error}"),
            );
        }
        AgentWorkerHandle {
            _shutdown: Arc::new(Shutdown(inner.clone())),
            inner,
        }
    }

    /// A new consumer of the worker with its own reply channel and slots.
    /// Keep it on the consuming entity (dropping it cancels its jobs).
    pub fn client(&self) -> AgentClient {
        let (sender, receiver) = async_channel::unbounded();
        AgentClient {
            id: self.inner.next_client.fetch_add(1, Ordering::SeqCst) + 1,
            worker: self.clone(),
            sender,
            receiver,
        }
    }

    /// Semantic gate (§7.6): refuse semantic/hybrid jobs while paused.
    pub fn pause_semantic(&self, paused: bool) {
        self.inner.semantic_paused.store(paused, Ordering::SeqCst);
        if paused {
            // A semantic or hybrid job already running must stop using the
            // embedder and the cache (§7.6): cancel it; `run_job` answers it
            // with `SEMANTIC_PAUSED_TEXT`.
            let state = self.inner.lock();
            if let Some(running) = &state.running
                && running.semantic
            {
                running.token.cancel();
            }
        }
    }

    pub fn is_semantic_paused(&self) -> bool {
        self.inner.semantic_paused.load(Ordering::SeqCst)
    }

    /// Jobs queued or running (tests).
    pub fn busy(&self) -> bool {
        let state = self.inner.lock();
        state.running.is_some() || !state.pending.is_empty()
    }
}

/// One consumer of the agent worker (DESIGN §7.5): the Deep search pane or
/// one agent panel.
pub struct AgentClient {
    id: u64,
    worker: AgentWorkerHandle,
    /// The worker's end of this client's reply channel.
    sender: async_channel::Sender<AgentReply>,
    receiver: async_channel::Receiver<AgentReply>,
}

impl AgentClient {
    /// The client's id (distinguishes the pending slots of several clients).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Queue `job`; returns its generation (replies carry it). A running or
    /// queued job of the same kind of this client is cancelled.
    pub fn submit(&self, job: AgentJob) -> u64 {
        let inner = &self.worker.inner;
        let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let slot = job.kind.slot();
        let mut state = inner.lock();
        cancel_slot(&mut state, self.id, slot);
        state.pending.push_back(Pending {
            client: self.id,
            generation,
            job,
            sender: self.sender.clone(),
            token: SemanticCancellationToken::new(),
        });
        drop(state);
        inner.wake.notify_all();
        generation
    }

    /// Cancel this client's running and queued job of `kind`.
    pub fn cancel(&self, kind: AgentJobKind) {
        let mut state = self.worker.inner.lock();
        cancel_slot(&mut state, self.id, kind.slot());
    }

    /// This client's replies (`Progress`, `Done`, `Key`), in order. Poll it
    /// from a task stored on the consuming entity.
    pub fn replies(&self) -> async_channel::Receiver<AgentReply> {
        self.receiver.clone()
    }

    /// The sending end the worker answers on.
    pub fn reply_sender(&self) -> async_channel::Sender<AgentReply> {
        self.sender.clone()
    }

    /// Whether the semantic gate currently refuses semantic/hybrid jobs.
    pub fn is_semantic_paused(&self) -> bool {
        self.worker.is_semantic_paused()
    }
}

impl Drop for AgentClient {
    fn drop(&mut self) {
        let mut state = self.worker.inner.lock();
        for slot in 0..=4 {
            cancel_slot(&mut state, self.id, slot);
        }
    }
}

/// Cancel the running job and drop the queued one of (`client`, `slot`).
fn cancel_slot(state: &mut State, client: u64, slot: u8) {
    if let Some(running) = &state.running
        && running.client == client
        && running.slot == slot
    {
        running.token.cancel();
    }
    state.pending.retain(|pending| {
        let same = pending.client == client && pending.job.kind.slot() == slot;
        if same {
            pending.token.cancel();
        }
        !same
    });
}

fn worker_loop(inner: Arc<Inner>, factory: SemanticEmbedderFactory) {
    loop {
        let pending = {
            let mut state = inner.lock();
            loop {
                if state.closed {
                    return;
                }
                if let Some(pending) = state.pending.pop_front() {
                    state.running = Some(Running {
                        client: pending.client,
                        slot: pending.job.kind.slot(),
                        semantic: is_semantic(&pending.job.request, &pending.job.config),
                        token: pending.token.clone(),
                    });
                    break pending;
                }
                state = inner.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        run_job(&inner, &factory, pending);
        inner.lock().running = None;
    }
}

/// Whether `request` runs a semantic or hybrid search under `config`.
fn is_semantic(request: &AgentRequest, config: &ConfigFile) -> bool {
    let (query, cli_mode) = match request {
        AgentRequest::Search(args) => (args.query.as_str(), args.mode_override()),
        AgentRequest::Within(args) => (args.query.as_str(), args.mode_override()),
        AgentRequest::Read(_) | AgentRequest::Outline(_) => return false,
    };
    let config_mode = config
        .agent
        .as_ref()
        .and_then(|agent| agent.mode)
        .or(config.search.as_ref().and_then(|search| search.mode));
    matches!(
        effective_agent_mode(query, cli_mode, config_mode, None),
        SearchMode::Semantic | SearchMode::Hybrid
    )
}

/// D-014: an ambiguous-ref error lists the short handles of its candidates,
/// which are unique only for the current key set. Rewrite them in the
/// detail to full refs, so clicking a candidate fills a durable ref.
fn full_ref_candidates(response: &mut AgentResponse, env: &HistoryEnv, cancelled: bool) {
    let Some(error) = response.error.as_mut() else {
        return;
    };
    if error.kind == claude_history_core::agent::diagnostic::AgentErrorKind::AmbiguousRef {
        // Pi/OMP refs hash the session id that only core's key discovery
        // knows (CCR): discover lazily, an error leaves them as handles.
        // The discovery is blocking and not cancellable: skip it for a job
        // that was cancelled or replaced (the handles stay).
        let keys = if !cancelled
            && (error.detail.contains(" source=pi ") || error.detail.contains(" source=omp "))
        {
            claude_history_core::agent::service::discover_agent_keys(env, None)
                .map(|(keys, _)| keys)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        error.detail = expand_handles(&error.detail, &keys);
    }
}

/// Replace the short handle at the start of every candidate (`ch_… project=…
/// source=claude directory=D file=F`) with the full ref computed from D and
/// F; Pi/OMP candidates are looked up in `keys` (core's discovery) and keep
/// their handle when no key matches.
pub(super) fn expand_handles(
    detail: &str,
    keys: &[claude_history_core::agent::refs::AgentConversationKey],
) -> String {
    let Some((head, list)) = detail.split_once("candidates:") else {
        return detail.to_string();
    };
    let lines: Vec<String> = list
        .split("\n  ")
        .map(|entry| {
            let lead = &entry[..entry.len() - entry.trim_start().len()];
            let entry = entry.trim_start();
            let Some((handle, rest)) = entry.split_once(' ') else {
                return format!("{lead}{entry}");
            };
            let source = rest
                .split(" source=")
                .nth(1)
                .and_then(|s| s.split(' ').next());
            let directory = rest
                .split_once(" directory=")
                .and_then(|(_, tail)| tail.rsplit_once(" file="));
            match (source, directory) {
                (Some("claude"), Some((directory, file))) if handle.starts_with("ch_") => {
                    let full = claude_history_core::agent::refs::AgentConversationRef::from_parts(
                        directory, file,
                    )
                    .full_ref();
                    format!("{lead}{full} {rest}")
                }
                (Some(source), Some((directory, file))) if handle.starts_with("ch_") => {
                    match claude_history_core::agent::refs::full_ref_for_candidate(
                        keys, source, directory, file,
                    ) {
                        Some(full) => format!("{lead}{full} {rest}"),
                        None => format!("{lead}{entry}"),
                    }
                }
                _ => format!("{lead}{entry}"),
            }
        })
        .collect();
    format!("{head}candidates:{}", lines.join("\n  "))
}

/// The response of a job that was refused before it ran.
fn refused(text: &str) -> AgentResponse {
    AgentResponse {
        stdout: String::new(),
        stderr: format!("Error: {text}\n"),
        exit_code: 1,
        result: None,
        warnings: Vec::new(),
        error: None,
        notices: Vec::new(),
    }
}

/// The core's response of a cancelled request.
fn cancelled() -> AgentResponse {
    AgentResponse {
        stderr: AgentResponse::CANCELLED_TEXT.to_string(),
        ..refused("")
    }
}

fn run_job(inner: &Inner, factory: &SemanticEmbedderFactory, pending: Pending) {
    let Pending {
        generation,
        job,
        sender,
        token,
        ..
    } = pending;
    let AgentJob {
        kind,
        request,
        env,
        config,
    } = job;
    if let AgentJobKind::KeyForPath(path) = &kind {
        let (full_ref, error) = if token.is_cancelled() {
            (None, None)
        } else {
            match conversation_key_for_path(&env, &config, path) {
                Ok(key) => (key.map(|key| key.conversation_ref().full_ref()), None),
                Err(error) => (None, Some(error.to_string())),
            }
        };
        let _ = sender.try_send(AgentReply::Key {
            generation,
            path: path.clone(),
            full_ref,
            error,
        });
        return;
    }
    let semantic = is_semantic(&request, &config);
    let gate_closed = || semantic && inner.semantic_paused.load(Ordering::SeqCst);
    let response = if gate_closed() {
        refused(SEMANTIC_PAUSED_TEXT)
    } else if token.is_cancelled() {
        cancelled()
    } else {
        let factory = factory.clone();
        let progress_sender = sender.clone();
        let progress_kind = kind.clone();
        let env_for_refs = env.clone();
        let mut service = AgentService::new(env, config)
            .with_embedder_factory(Box::new(move || factory()))
            .with_cancellation(token.clone())
            .with_notice_sink(Box::new(move |notice| {
                let _ = progress_sender.try_send(AgentReply::Progress {
                    kind: progress_kind.clone(),
                    generation,
                    notice: notice.clone(),
                });
            }));
        let mut response = service.respond(request);
        if token.is_cancelled() && gate_closed() {
            // The gate closed while the job ran and cancelled it.
            response = refused(SEMANTIC_PAUSED_TEXT);
        }
        full_ref_candidates(&mut response, &env_for_refs, token.is_cancelled());
        response
    };
    let _ = sender.try_send(AgentReply::Done {
        kind,
        generation,
        response,
    });
}
