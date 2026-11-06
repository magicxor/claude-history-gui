//! Background semantic search worker (upstream `tui/semantic_worker.rs`).
//!
//! Moved almost verbatim. Differences from upstream (plan section 5.2):
//!
//! * [`spawn_semantic_worker`] takes a [`SemanticWorkerConfig`] instead of
//!   reading the real home directory: the embedding cache is loaded from
//!   [`SemanticWorkerConfig::cache_path`] (upstream:
//!   `SemanticIndexState::new()`), written back through
//!   [`crate::semantic::cache::embedding_cache_writer`], and the embedder is
//!   created by an injectable [`SemanticEmbedderFactory`] (upstream:
//!   `FastembedEmbedder::new_quiet(EmbedPriority::Background)`). The default
//!   factory loads fastembed from [`SemanticWorkerConfig::model_dir`]
//!   without download progress output, on half the cores (upstream
//!   8322afe). Under `cfg(test)` the default factory refuses to run, so a
//!   test can never download the model by accident.
//! * The worker lowers its own thread's scheduling priority before it makes
//!   an embedder, whatever the factory (upstream: `new_quiet(Background)`
//!   lowered the thread that called it, so only with the default factory);
//!   on Windows too, and not in tests (see
//!   [`crate::semantic::fastembed::lower_background_thread_priority`]).
//! * After posting every message the worker calls the optional
//!   [`SemanticWorkerConfig::waker`], so a GUI can wake its poller instead of
//!   polling on a timer (upstream c6d0421's TUI polled every 250 ms while
//!   indexing).
//!
//! Searches are ranked over the passages embedded so far and the rest of the
//! corpus is embedded in the background, in slices between commands, as
//! upstream c6d0421 ("rank semantic queries without waiting for new
//! embeddings"), with these differences:
//!
//! * The embedder is created by the factory as before, and again after a
//!   failed creation (upstream c6d0421 keeps the first failure for the life
//!   of the worker).
//! * Cache writes, every 30 s while indexing and when it ends, go through
//!   the same cache writer as every other write (the configured file, under
//!   the K6 `embeddings.lock`).
//! * The cancellation token of a slice is taken before the worker looks for
//!   a command, so closing the channel followed by a cancel
//!   ([`crate::browser::Browser::suspend_semantic`], `Browser::drop`) always
//!   stops a running slice at its next batch.
//! * A prewarm answered before anything is embedded reports
//!   [`SemanticProgress::CacheReady`] rather than `EmptyCorpus`: the corpus
//!   has text, which indexing is embedding (upstream showed `sem no text`
//!   until the next query).
//! * When embedding fails while indexing, a search still waiting for it is
//!   answered with the error (upstream left it waiting, showing `sem
//!   embedding` for good); a search an earlier slice already answered keeps
//!   its results. An embedder whose model loads at its first call
//!   (the GUI's shared model) fails there.
//!
//! Everything else is unchanged: the command protocol, corpus/scope version
//! gating, coalescing of queued commands, the quoted-only exact fallback
//! (no embeddings), evidence windows (±80 characters with `…`), and the
//! progress mapping.

use crate::browser::{SemanticProgress, SemanticResultMetadata};
use crate::env::HistoryEnv;
use crate::error::{AppError, Result};
use crate::history::Conversation;
use crate::search::literal::{build_literal_corpus, exact_fallback};
use crate::search::query::ParsedQuery;
use crate::semantic::cache::embedding_cache_writer;
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::index::{
    SemanticIndexCandidate, SemanticIndexProgress, SemanticIndexRequest, SemanticIndexResponse,
    SemanticIndexState,
};
use crate::semantic::types::{
    DEFAULT_EMBEDDING_BATCH_SIZE, EmbeddingCache, SemanticCancellationToken, SemanticChunkIdentity,
    SemanticExplanation, SemanticQuality, SemanticRationaleKind, SemanticScoreBreakdown,
};
use crate::store::SearchIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

/// Callback invoked from a worker thread after it posted a message. A GUI
/// uses it to wake the thread that polls the receiving channel.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Creates the embedder used by the semantic worker. It is called on the
/// worker thread, lazily, the first time a search needs embeddings, and
/// again after a failed initialization.
pub type SemanticEmbedderFactory = Arc<dyn Fn() -> Result<Box<dyn SemanticEmbedder>> + Send + Sync>;

/// Inputs of a semantic worker (replaces upstream's implicit home-directory
/// lookups).
#[derive(Clone, Default)]
pub struct SemanticWorkerConfig {
    /// Embedding cache file to load at start and to rewrite while embedding.
    /// `None` keeps the cache in memory only.
    pub cache_path: Option<PathBuf>,
    /// fastembed model directory used by the default embedder factory.
    pub model_dir: Option<PathBuf>,
    /// Embedder factory; `None` uses fastembed in [`Self::model_dir`].
    pub embedder_factory: Option<SemanticEmbedderFactory>,
    /// Called after every message the worker posts.
    pub waker: Option<Waker>,
}

impl SemanticWorkerConfig {
    /// Cache file and model directory from `env`
    /// ([`HistoryEnv::embedding_cache_file`], [`HistoryEnv::semantic_model_dir`]).
    pub fn from_env(env: &HistoryEnv) -> Self {
        Self {
            cache_path: env.embedding_cache_file(),
            model_dir: env.semantic_model_dir(),
            embedder_factory: None,
            waker: None,
        }
    }

    pub fn with_embedder_factory(mut self, factory: SemanticEmbedderFactory) -> Self {
        self.embedder_factory = Some(factory);
        self
    }

    pub fn with_waker(mut self, waker: Waker) -> Self {
        self.waker = Some(waker);
        self
    }

    /// Makes the embedder, on the worker thread, after lowering that
    /// thread's scheduling priority (upstream 8322afe:
    /// `FastembedEmbedder::new_quiet(EmbedPriority::Background)` lowered it;
    /// fork: the worker does, for an injected factory too).
    fn create_embedder(&self) -> Result<Box<dyn SemanticEmbedder>> {
        crate::semantic::fastembed::lower_background_thread_priority();
        match &self.embedder_factory {
            Some(factory) => factory(),
            None => default_embedder(self.model_dir.as_deref()),
        }
    }
}

impl std::fmt::Debug for SemanticWorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemanticWorkerConfig")
            .field("cache_path", &self.cache_path)
            .field("model_dir", &self.model_dir)
            .field("embedder_factory", &self.embedder_factory.is_some())
            .field("waker", &self.waker.is_some())
            .finish()
    }
}

/// fastembed in `model_dir`, quiet (no hf-hub progress bar on stderr).
/// Under `cfg(test)` it refuses: tests must inject an embedder factory (no
/// model download; see [`crate::semantic::fastembed::quiet_embedder_in`]).
fn default_embedder(model_dir: Option<&Path>) -> Result<Box<dyn SemanticEmbedder>> {
    let model_dir = model_dir.ok_or_else(|| {
        crate::error::AppError::ConfigError(
            "semantic model directory is unavailable (no home directory)".to_string(),
        )
    })?;
    crate::semantic::fastembed::quiet_embedder_in(
        model_dir.to_path_buf(),
        crate::semantic::fastembed::EmbedPriority::Background,
    )
}

#[derive(Clone)]
pub enum SemanticWorkerCommand {
    UpdateCorpus {
        corpus_version: u64,
        conversations: Arc<Vec<Arc<Conversation>>>,
        /// Lexical index whose doc ids are the indices of `conversations`,
        /// whose cards carry no text, for quoted-only queries.
        texts: Option<Arc<SearchIndex>>,
    },
    UpdateScope {
        corpus_version: u64,
        scope_version: u64,
        indices: Arc<Vec<usize>>,
    },
    Search {
        generation: u64,
        query: ParsedQuery,
        corpus_version: u64,
        scope_version: u64,
        prewarm: bool,
    },
}

#[derive(Clone)]
struct SemanticSearchRequest {
    generation: u64,
    query: ParsedQuery,
    corpus_version: u64,
    scope_version: u64,
    prewarm: bool,
}

pub enum SemanticSearchMessage {
    Progress {
        generation: u64,
        progress: SemanticProgress,
    },
    Complete(SemanticSearchResponse),
    /// Progress of embedding the passages the corpus has no embedding for,
    /// which runs behind searches.
    Indexing(SemanticIndexing),
    /// The worker has nothing left to do for the searches up to
    /// `generation`: each is answered and indexing has finished.
    Idle {
        generation: u64,
    },
}

/// Passages embedded since indexing started, of those it has found to embed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticIndexing {
    pub completed: usize,
    pub total: usize,
}

pub struct SemanticSearchResponse {
    pub generation: u64,
    pub filtered: Vec<usize>,
    pub metadata: HashMap<usize, SemanticResultMetadata>,
    pub error: Option<String>,
    pub progress: SemanticProgress,
    pub prewarm: bool,
}

/// Sends worker messages and wakes the receiver.
struct Outbox {
    tx: mpsc::Sender<SemanticSearchMessage>,
    waker: Option<Waker>,
}

impl Outbox {
    fn post(&self, message: SemanticSearchMessage) {
        let _ = self.tx.send(message);
        if let Some(waker) = &self.waker {
            waker();
        }
    }
}

/// Spawn the worker thread ("semantic-search-worker", with a one-thread
/// rayon pool). Dropping the returned sender stops the worker.
pub fn spawn_semantic_worker(
    config: SemanticWorkerConfig,
) -> (
    mpsc::Sender<SemanticWorkerCommand>,
    mpsc::Receiver<SemanticSearchMessage>,
    SemanticCancellationToken,
) {
    let worker = spawn_semantic_worker_thread(config);
    (worker.tx, worker.rx, worker.cancellation)
}

/// A spawned semantic worker, with its thread so that the owner can join
/// it (the worker exits once `tx` is dropped and its current request
/// returns; drop `tx` first, then cancel, to make that quick).
pub(crate) struct SemanticWorkerHandle {
    pub(crate) tx: mpsc::Sender<SemanticWorkerCommand>,
    pub(crate) rx: mpsc::Receiver<SemanticSearchMessage>,
    pub(crate) cancellation: SemanticCancellationToken,
    pub(crate) thread: std::thread::JoinHandle<()>,
}

pub(crate) fn spawn_semantic_worker_thread(config: SemanticWorkerConfig) -> SemanticWorkerHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel::<SemanticWorkerCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SemanticSearchMessage>();
    let cancellation = SemanticCancellationToken::new();
    let worker_cancellation = cancellation.clone();

    let thread = std::thread::Builder::new()
        .name("semantic-search-worker".into())
        .spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|_| "semantic-rank-worker".to_string())
                .stack_size(8 * 1024 * 1024)
                .build()
                .expect("failed to create semantic ranking thread pool");
            pool.install(|| run_semantic_worker(cmd_rx, res_tx, worker_cancellation, config));
        })
        .expect("failed to spawn semantic search worker thread");

    SemanticWorkerHandle {
        tx: cmd_tx,
        rx: res_rx,
        cancellation,
        thread,
    }
}

/// Embedding of the passages a search found without embeddings, done in
/// slices between commands so that searches never wait for it.
struct Indexing {
    /// The search whose results are refreshed once indexing finishes.
    request: SemanticSearchRequest,
    /// Fork: whether `request` has been answered. One still waiting for
    /// indexing is answered with the error when embedding fails (upstream
    /// left it waiting; its 960281f adds this field for its own use).
    answered: bool,
    completed: usize,
    /// Passages to embed per slice, sized so a slice takes about
    /// `SLICE_TARGET`.
    budget: usize,
    last_save: std::time::Instant,
}

impl Indexing {
    /// How long a slice should take: short enough that a command waits
    /// little for the running slice, long enough that planning each slice
    /// costs little next to embedding.
    const SLICE_TARGET: std::time::Duration = std::time::Duration::from_secs(2);
    /// Interval between writes of the embedding cache while indexing; the
    /// cache is also written when indexing finishes.
    const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

    fn new(request: SemanticSearchRequest) -> Self {
        Self {
            request,
            answered: true,
            completed: 0,
            budget: DEFAULT_EMBEDDING_BATCH_SIZE,
            last_save: std::time::Instant::now(),
        }
    }

    fn progress(&self, missing: usize) -> SemanticIndexing {
        SemanticIndexing {
            completed: self.completed,
            total: self.completed + missing,
        }
    }

    fn resize(&mut self, embedded: usize, elapsed: std::time::Duration) {
        if embedded == 0 || elapsed.is_zero() {
            return;
        }
        let per_second = embedded as f64 / elapsed.as_secs_f64();
        let budget = (per_second * Self::SLICE_TARGET.as_secs_f64()) as usize;
        self.budget = budget.clamp(DEFAULT_EMBEDDING_BATCH_SIZE / 2, 4096);
    }

    /// Writes `cache` through `save_cache` (fork: the configured cache
    /// writer; upstream `write_embedding_cache`) unless it was written less
    /// than `SAVE_INTERVAL` ago.
    fn save(&mut self, cache: &EmbeddingCache, save_cache: &mut dyn FnMut(&EmbeddingCache)) {
        if self.last_save.elapsed() >= Self::SAVE_INTERVAL {
            save_cache(cache);
            self.last_save = std::time::Instant::now();
        }
    }
}

fn run_semantic_worker(
    cmd_rx: mpsc::Receiver<SemanticWorkerCommand>,
    res_tx: mpsc::Sender<SemanticSearchMessage>,
    cancellation_source: SemanticCancellationToken,
    config: SemanticWorkerConfig,
) {
    let outbox = Outbox {
        tx: res_tx,
        waker: config.waker.clone(),
    };
    let mut worker = SemanticWorkerState::default();
    let mut state = SemanticIndexState::load(config.cache_path.as_deref());
    let mut save_cache = embedding_cache_writer(config.cache_path.clone());
    let mut embedder: Option<Box<dyn SemanticEmbedder>> = None;
    let mut indexing: Option<Indexing> = None;
    let mut idle_reported = true;

    loop {
        if indexing.is_none() && !idle_reported {
            idle_reported = true;
            outbox.post(SemanticSearchMessage::Idle {
                generation: worker.last_search_generation,
            });
        }
        // Fork: the token of the next slice is taken before the channel is
        // looked at. Closing the channel and then cancelling (K6 suspend)
        // either closes it before the look or cancels the slice.
        let slice_cancellation = cancellation_source.child();
        let command = if indexing.is_some() {
            match cmd_rx.try_recv() {
                Ok(command) => Some(command),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        } else {
            match cmd_rx.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };
        let Some(command) = command else {
            if let Some(current) = indexing.as_mut() {
                let done = index_slice(
                    &worker,
                    &mut state,
                    &mut embedder,
                    current,
                    &slice_cancellation,
                    &outbox,
                    &mut save_cache,
                );
                if done {
                    indexing = None;
                }
            }
            continue;
        };

        let mut request = worker.apply_command(command, &cancellation_source);
        while let Ok(pending) = cmd_rx.try_recv() {
            if let Some(search) = worker.apply_command(pending, &cancellation_source) {
                request = Some(search);
            }
        }

        let Some(request) = request else {
            continue;
        };
        idle_reported = false;
        let Some(version_state) = worker.version_state_for(&request) else {
            continue;
        };
        match version_state {
            VersionState::Future => {
                worker.pending_search = Some(request);
                continue;
            }
            VersionState::Stale => continue,
            VersionState::Current => {}
        }

        let cancellation = cancellation_source.child();
        if request.query.is_effectively_empty() && !request.prewarm {
            outbox.post(SemanticSearchMessage::Complete(SemanticSearchResponse {
                generation: request.generation,
                filtered: Vec::new(),
                metadata: HashMap::new(),
                error: None,
                progress: SemanticProgress::Idle,
                prewarm: request.prewarm,
            }));
            continue;
        }
        if request.query.is_quoted_only() {
            let response = match &worker.texts {
                Some(texts) => indexed_literal_semantic_response(
                    request.generation,
                    request.prewarm,
                    texts,
                    worker.corpus.as_ref(),
                    worker.scope.as_ref(),
                    &request.query,
                ),
                None => exact_literal_semantic_response(
                    request.generation,
                    request.prewarm,
                    worker.corpus.as_ref(),
                    worker.scope.as_ref(),
                    &request.query,
                ),
            };
            outbox.post(SemanticSearchMessage::Complete(response));
            continue;
        }
        let full_corpus = worker.full_corpus_candidates();
        let scope = worker.semantic_candidates();
        let index_request = SemanticIndexRequest {
            query: request.query.semantic_text(),
            literal_filters: request.query.literals(),
            full_corpus: &full_corpus,
            scope: &scope,
            corpus_version: request.corpus_version,
            prewarm: request.prewarm,
            include_chunk_hits: false,
        };
        let response = match state.has_chunks(&index_request, &cancellation) {
            Ok(true) => {
                if embedder.is_none() {
                    outbox.post(SemanticSearchMessage::Progress {
                        generation: request.generation,
                        progress: SemanticProgress::InitializingModel,
                    });
                    // Fork: created again after a failure (upstream kept it).
                    embedder = match config.create_embedder() {
                        Ok(embedder) => Some(embedder),
                        Err(error) => {
                            outbox.post(SemanticSearchMessage::Complete(failed_semantic_response(
                                request.generation,
                                request.prewarm,
                                error.to_string(),
                            )));
                            continue;
                        }
                    };
                }
                let model = embedder.as_deref_mut().expect("embedder initialized above");
                rank_embedded(
                    request.generation,
                    &index_request,
                    &request.query,
                    &mut state,
                    model,
                    &cancellation,
                )
                .map(|(mut response, missing)| {
                    if missing == 0 {
                        return Some(response);
                    }
                    let current = indexing.get_or_insert_with(|| Indexing::new(request.clone()));
                    let unanswered = response.filtered.is_empty() && !request.prewarm;
                    if !request.prewarm || current.request.prewarm {
                        current.request = request.clone();
                        current.answered = !unanswered;
                    }
                    let progress = current.progress(missing);
                    outbox.post(SemanticSearchMessage::Indexing(progress));
                    if unanswered {
                        // Nothing embedded so far matches; the search is
                        // answered once something does.
                        outbox.post(SemanticSearchMessage::Progress {
                            generation: request.generation,
                            progress: SemanticProgress::Embedding {
                                completed: progress.completed,
                                total: progress.total,
                            },
                        });
                        return None;
                    }
                    if response.progress == SemanticProgress::EmptyCorpus {
                        // Fork: nothing is embedded yet, but the corpus has
                        // text (upstream answered `sem no text`).
                        response.progress = SemanticProgress::CacheReady;
                    }
                    Some(response)
                })
                .unwrap_or_else(|error| {
                    Some(failed_semantic_response(
                        request.generation,
                        request.prewarm,
                        error.to_string(),
                    ))
                })
            }
            Ok(false) => Some(match state.clear_empty(&index_request, &cancellation) {
                Ok(()) => empty_semantic_response(request.generation, request.prewarm),
                Err(error) => {
                    failed_semantic_response(request.generation, request.prewarm, error.to_string())
                }
            }),
            Err(error) => Some(failed_semantic_response(
                request.generation,
                request.prewarm,
                error.to_string(),
            )),
        };

        if let Some(response) = response {
            outbox.post(SemanticSearchMessage::Complete(response));
        }
    }
}

/// Embeds one slice of the passages still without embeddings. Returns
/// whether indexing is over: every passage is embedded, and the search it
/// serves has been answered again with all of them, or embedding failed.
fn index_slice(
    worker: &SemanticWorkerState,
    state: &mut SemanticIndexState,
    embedder: &mut Option<Box<dyn SemanticEmbedder>>,
    indexing: &mut Indexing,
    cancellation: &SemanticCancellationToken,
    outbox: &Outbox,
    save_cache: &mut dyn FnMut(&EmbeddingCache),
) -> bool {
    let Some(model) = embedder.as_deref_mut() else {
        return true;
    };
    let full_corpus = worker.full_corpus_candidates();
    let index_request = SemanticIndexRequest {
        query: "",
        literal_filters: &[],
        full_corpus: &full_corpus,
        scope: &full_corpus,
        corpus_version: worker.corpus_version,
        prewarm: true,
        include_chunk_hits: false,
    };
    let started = std::time::Instant::now();
    let budget = indexing.budget;
    let slice = state.index_slice(&index_request, model, cancellation, budget, |cache| {
        indexing.save(cache, save_cache)
    });
    let response = match slice {
        Ok(response) => response,
        // A command is waiting; the next slice continues after it.
        Err(AppError::SemanticSearchCancelled) => return false,
        Err(error) => {
            save_cache(&state.cache);
            // Fork: a search still waiting for indexing gets the error
            // (upstream left it waiting). A model that loads at its first
            // embedding, like the GUI's shared one, can fail here.
            let request = &indexing.request;
            if !indexing.answered
                && worker
                    .version_state_for(request)
                    .is_some_and(|version| matches!(version, VersionState::Current))
            {
                outbox.post(SemanticSearchMessage::Complete(failed_semantic_response(
                    request.generation,
                    request.prewarm,
                    error.to_string(),
                )));
            }
            return true;
        }
    };
    indexing.completed += response.embedded_chunk_count;
    indexing.resize(response.embedded_chunk_count, started.elapsed());
    if response.missing_chunk_count > 0 {
        outbox.post(SemanticSearchMessage::Indexing(
            indexing.progress(response.missing_chunk_count),
        ));
        if !indexing.answered
            && let Some(response) =
                rank_current(worker, state, model, &indexing.request, cancellation)
            && !response.filtered.is_empty()
        {
            indexing.answered = true;
            outbox.post(SemanticSearchMessage::Complete(response));
        }
        return false;
    }

    save_cache(&state.cache);
    if let Some(response) = rank_current(worker, state, model, &indexing.request, cancellation) {
        outbox.post(SemanticSearchMessage::Complete(response));
    }
    true
}

/// Ranks `request` over the passages embedded so far, unless it is a
/// prewarm or the corpus or scope has moved on from it.
fn rank_current(
    worker: &SemanticWorkerState,
    state: &mut SemanticIndexState,
    model: &mut dyn SemanticEmbedder,
    request: &SemanticSearchRequest,
    cancellation: &SemanticCancellationToken,
) -> Option<SemanticSearchResponse> {
    if request.prewarm
        || !matches!(
            worker.version_state_for(request),
            Some(VersionState::Current)
        )
    {
        return None;
    }
    let full_corpus = worker.full_corpus_candidates();
    let scope = worker.semantic_candidates();
    let index_request = SemanticIndexRequest {
        query: request.query.semantic_text(),
        literal_filters: request.query.literals(),
        full_corpus: &full_corpus,
        scope: &scope,
        corpus_version: request.corpus_version,
        prewarm: false,
        include_chunk_hits: false,
    };
    rank_embedded(
        request.generation,
        &index_request,
        &request.query,
        state,
        model,
        cancellation,
    )
    .ok()
    .map(|(response, _)| response)
}

#[derive(Default)]
struct SemanticWorkerState {
    corpus_version: u64,
    scope_version: u64,
    scope_corpus_version: u64,
    corpus: Arc<Vec<Arc<Conversation>>>,
    texts: Option<Arc<SearchIndex>>,
    scope: Arc<Vec<usize>>,
    pending_search: Option<SemanticSearchRequest>,
    /// Generation of the newest search received.
    last_search_generation: u64,
}

enum VersionState {
    Current,
    Future,
    Stale,
}

impl SemanticWorkerState {
    fn apply_command(
        &mut self,
        command: SemanticWorkerCommand,
        cancellation: &SemanticCancellationToken,
    ) -> Option<SemanticSearchRequest> {
        match command {
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version,
                conversations,
                texts,
            } => {
                if corpus_version >= self.corpus_version {
                    self.corpus_version = corpus_version;
                    self.corpus = conversations;
                    self.texts = texts;
                    cancellation.cancel();
                }
                self.take_ready_pending()
            }
            SemanticWorkerCommand::UpdateScope {
                corpus_version,
                scope_version,
                indices,
            } => {
                if scope_version >= self.scope_version {
                    self.scope_corpus_version = corpus_version;
                    self.scope_version = scope_version;
                    self.scope = indices;
                    cancellation.cancel();
                }
                self.take_ready_pending()
            }
            SemanticWorkerCommand::Search {
                generation,
                query,
                corpus_version,
                scope_version,
                prewarm,
            } => {
                cancellation.cancel();
                self.last_search_generation = self.last_search_generation.max(generation);
                Some(SemanticSearchRequest {
                    generation,
                    query,
                    corpus_version,
                    scope_version,
                    prewarm,
                })
            }
        }
    }

    fn take_ready_pending(&mut self) -> Option<SemanticSearchRequest> {
        let request = self.pending_search.take()?;
        match self.version_state_for(&request) {
            Some(VersionState::Current) | Some(VersionState::Stale) => Some(request),
            Some(VersionState::Future) | None => {
                self.pending_search = Some(request);
                None
            }
        }
    }

    fn version_state_for(&self, request: &SemanticSearchRequest) -> Option<VersionState> {
        if self.scope_corpus_version != self.corpus_version {
            return Some(VersionState::Future);
        }
        if self.corpus_version == request.corpus_version
            && self.scope_version == request.scope_version
        {
            Some(VersionState::Current)
        } else if self.corpus_version < request.corpus_version
            || self.scope_version < request.scope_version
        {
            Some(VersionState::Future)
        } else {
            Some(VersionState::Stale)
        }
    }

    fn full_corpus_candidates(&self) -> Vec<SemanticIndexCandidate> {
        self.corpus
            .iter()
            .enumerate()
            .map(|(index, conversation)| SemanticIndexCandidate {
                index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                conversation: conversation.clone(),
            })
            .collect()
    }

    fn semantic_candidates(&self) -> Vec<SemanticIndexCandidate> {
        self.scope
            .iter()
            .filter_map(|&index| {
                self.corpus
                    .get(index)
                    .map(|conversation| SemanticIndexCandidate {
                        index,
                        source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                        conversation: conversation.clone(),
                    })
            })
            .collect()
    }
}

/// Answers `request` from the passages embedded so far, embedding none.
/// Also returns how many passages of the corpus have no embedding yet.
fn rank_embedded(
    generation: u64,
    request: &SemanticIndexRequest<'_>,
    parsed: &ParsedQuery,
    state: &mut SemanticIndexState,
    embedder: &mut dyn SemanticEmbedder,
    cancellation: &SemanticCancellationToken,
) -> Result<(SemanticSearchResponse, usize)> {
    let response = state.refresh_or_prewarm_with_budget(
        request,
        embedder,
        cancellation,
        Some(0),
        |_| {},
        |_| {},
    )?;
    let missing = response.missing_chunk_count;
    Ok((
        semantic_search_response(generation, response, request.full_corpus, parsed),
        missing,
    ))
}

fn semantic_search_response(
    generation: u64,
    response: SemanticIndexResponse,
    _conversations: &[SemanticIndexCandidate],
    _parsed: &ParsedQuery,
) -> SemanticSearchResponse {
    let filtered = response
        .hits
        .iter()
        .map(|hit| hit.conversation_index)
        .collect::<Vec<_>>();
    let metadata = response
        .hits
        .into_iter()
        .map(|hit| {
            let explanation = hit.explanation;
            (
                hit.conversation_index,
                SemanticResultMetadata {
                    score_breakdown: hit.score_breakdown,
                    explanation,
                },
            )
        })
        .collect();

    SemanticSearchResponse {
        generation,
        filtered,
        metadata,
        error: None,
        progress: semantic_progress(response.progress),
        prewarm: response.prewarm,
    }
}

fn empty_semantic_response(generation: u64, prewarm: bool) -> SemanticSearchResponse {
    SemanticSearchResponse {
        generation,
        filtered: Vec::new(),
        metadata: HashMap::new(),
        error: None,
        progress: SemanticProgress::EmptyCorpus,
        prewarm,
    }
}

fn exact_literal_semantic_response(
    generation: u64,
    prewarm: bool,
    conversations: &[Arc<Conversation>],
    scope: &[usize],
    parsed: &ParsedQuery,
) -> SemanticSearchResponse {
    // The literal search reads the shared corpus directly (no copy).
    let corpus = build_literal_corpus(conversations);
    let scope = scope
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let filtered = exact_fallback(conversations, &corpus, parsed.literals(), |index| {
        scope.contains(&index)
    });

    let metadata = filtered
        .iter()
        .filter_map(|index| {
            conversations.get(*index).map(|conversation| {
                (
                    *index,
                    exact_literal_metadata(conversation, &conversation.full_text, parsed),
                )
            })
        })
        .collect();

    SemanticSearchResponse {
        generation,
        filtered,
        metadata,
        error: None,
        progress: SemanticProgress::Complete,
        prewarm,
    }
}

/// Like `exact_literal_semantic_response`, for cards whose text is in
/// `texts`: the index finds the matches, newest first, and the text of the
/// first `EVIDENCE_LIMIT` is read for their evidence.
fn indexed_literal_semantic_response(
    generation: u64,
    prewarm: bool,
    texts: &SearchIndex,
    conversations: &[Arc<Conversation>],
    scope: &[usize],
    parsed: &ParsedQuery,
) -> SemanticSearchResponse {
    const EVIDENCE_LIMIT: usize = 200;
    let mut in_scope = vec![false; conversations.len()];
    for &index in scope {
        if let Some(slot) = in_scope.get_mut(index) {
            *slot = true;
        }
    }
    let scope = |doc: u32| in_scope.get(doc as usize).copied().unwrap_or(false);
    let mut ranking = texts.search(parsed.raw(), chrono::Local::now(), &scope, false);
    let filtered: Vec<usize> = ranking
        .complete()
        .iter()
        .map(|hit| hit.doc as usize)
        .filter(|&doc| doc < conversations.len())
        .collect();
    let metadata = filtered
        .iter()
        .enumerate()
        .map(|(rank, &index)| {
            let conversation = &conversations[index];
            let text = if rank < EVIDENCE_LIMIT {
                texts.text(index as u32)
            } else {
                String::new()
            };
            (index, exact_literal_metadata(conversation, &text, parsed))
        })
        .collect();
    SemanticSearchResponse {
        generation,
        filtered,
        metadata,
        error: None,
        progress: SemanticProgress::Complete,
        prewarm,
    }
}

fn literal_evidence_preview(
    conversation: &Conversation,
    full_text: &str,
    parsed: &ParsedQuery,
    visible_preview: &str,
) -> String {
    parsed
        .literals()
        .iter()
        .filter(|literal| !literal.matches(visible_preview))
        .chain(parsed.literals().iter())
        .find_map(|literal| {
            literal
                .match_ranges(full_text)
                .first()
                .map(|range| evidence_window(full_text, *range))
                .or_else(|| {
                    conversation.project_name.as_deref().and_then(|project| {
                        literal
                            .match_ranges(project)
                            .first()
                            .map(|range| evidence_window(project, *range))
                    })
                })
        })
        .unwrap_or_else(|| conversation.preview.to_string())
}

fn evidence_window(text: &str, range: (usize, usize)) -> String {
    const CONTEXT_CHARS: usize = 80;
    let (start, end) = range;
    let prefix_start = text[..start]
        .char_indices()
        .rev()
        .nth(CONTEXT_CHARS)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let suffix_end = text[end..]
        .char_indices()
        .nth(CONTEXT_CHARS)
        .map(|(index, _)| end + index)
        .unwrap_or(text.len());
    let mut preview = String::new();
    if prefix_start > 0 {
        preview.push('…');
    }
    preview.push_str(text[prefix_start..suffix_end].trim());
    if suffix_end < text.len() {
        preview.push('…');
    }
    preview
}

fn exact_literal_metadata(
    conversation: &Conversation,
    full_text: &str,
    parsed: &ParsedQuery,
) -> SemanticResultMetadata {
    SemanticResultMetadata {
        score_breakdown: SemanticScoreBreakdown {
            hybrid: 0.0,
            semantic: 0.0,
            lexical: 0.0,
        },
        explanation: SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "exact",
            matched_terms: parsed
                .literals()
                .iter()
                .map(|literal| literal.text().to_string())
                .collect(),
            evidence_preview: literal_evidence_preview(conversation, full_text, parsed, ""),
            rationale_kind: SemanticRationaleKind::LexicalBoosted,
            chunk: SemanticChunkIdentity {
                conversation_index: conversation.index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: conversation
                    .path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("?")
                    .to_string(),
                chunk_index: 0,
                message_range: crate::agent::refs::MessageRange::single(1),
            },
        },
    }
}

fn failed_semantic_response(
    generation: u64,
    prewarm: bool,
    error: String,
) -> SemanticSearchResponse {
    SemanticSearchResponse {
        generation,
        filtered: Vec::new(),
        metadata: HashMap::new(),
        error: Some(error),
        progress: SemanticProgress::Failed,
        prewarm,
    }
}

fn semantic_progress(progress: SemanticIndexProgress) -> SemanticProgress {
    match progress {
        SemanticIndexProgress::Embedding { completed, total } => {
            SemanticProgress::Embedding { completed, total }
        }
        SemanticIndexProgress::CacheReady => SemanticProgress::CacheReady,
        SemanticIndexProgress::Ranking => SemanticProgress::Ranking,
        SemanticIndexProgress::Complete => SemanticProgress::Complete,
        SemanticIndexProgress::EmptyCorpus => SemanticProgress::EmptyCorpus,
    }
}

/// Embedders used by tests (no model, no network).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Deterministic two-dimensional embeddings: text mentioning "beta"
    /// points one way, everything else the other.
    pub(crate) struct FakeEmbedder;

    impl SemanticEmbedder for FakeEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(passages.iter().map(|passage| embedding(passage)).collect())
        }

        fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(embedding(query)))
        }
    }

    fn embedding(text: &str) -> Vec<f32> {
        if text.contains("beta") {
            vec![0.0, 1.0]
        } else {
            vec![1.0, 0.0]
        }
    }

    /// A factory producing [`FakeEmbedder`]s that counts its calls.
    pub(crate) fn fake_embedder_factory() -> (SemanticEmbedderFactory, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let factory: SemanticEmbedderFactory = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeEmbedder) as Box<dyn SemanticEmbedder>)
        });
        (factory, calls)
    }

    /// A worker config rooted in a sandbox, with the fake embedder.
    pub(crate) fn sandbox_config(root: &Path) -> SemanticWorkerConfig {
        let env = HistoryEnv::sandbox(root);
        SemanticWorkerConfig::from_env(&env).with_embedder_factory(fake_embedder_factory().0)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{fake_embedder_factory, sandbox_config};
    use super::*;
    use crate::history::Conversation;
    use crate::semantic::test_fixtures::{SemanticConversationFixture, beta_hit_metadata};
    use chrono::{Duration as ChronoDuration, Local};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const WORKER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

    fn conversation(path: &str, semantic_turns: Vec<&str>) -> Conversation {
        SemanticConversationFixture::new(path, semantic_turns).build()
    }

    fn corpus(conversations: Vec<Conversation>) -> Arc<Vec<Arc<Conversation>>> {
        Arc::new(conversations.into_iter().map(Arc::new).collect())
    }

    fn send_worker_setup(
        tx: &mpsc::Sender<SemanticWorkerCommand>,
        semantic_turns: Vec<&str>,
        query: &str,
        prewarm: bool,
    ) {
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![conversation(
                "/projects/project-a/session-a.jsonl",
                semantic_turns,
            )]),
            texts: None,
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse(query),
            corpus_version: 1,
            scope_version: 1,
            prewarm,
        })
        .expect("send semantic request");
    }

    fn recv_empty_complete(rx: &mpsc::Receiver<SemanticSearchMessage>) -> SemanticSearchResponse {
        let message = rx
            .recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("empty response");
        match message {
            SemanticSearchMessage::Complete(response) => {
                assert!(response.filtered.is_empty());
                assert!(response.metadata.is_empty());
                assert_eq!(response.error, None);
                response
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before empty response: {progress:?}");
            }
            SemanticSearchMessage::Indexing(indexing) => {
                panic!("unexpected indexing before empty response: {indexing:?}");
            }
            SemanticSearchMessage::Idle { generation } => {
                panic!("unexpected idle before empty response: {generation}");
            }
        }
    }

    fn recv_complete(rx: &mpsc::Receiver<SemanticSearchMessage>) -> SemanticSearchResponse {
        loop {
            match rx
                .recv_timeout(WORKER_RESPONSE_TIMEOUT)
                .expect("worker response")
            {
                SemanticSearchMessage::Progress { .. }
                | SemanticSearchMessage::Indexing(_)
                | SemanticSearchMessage::Idle { .. } => {
                    continue;
                }
                SemanticSearchMessage::Complete(response) => return response,
            }
        }
    }

    /// Embeds passages and queries by whether they mention alpha or beta,
    /// and holds passage embedding while `hold` is set.
    struct FakeEmbedder {
        hold: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeEmbedder {
        fn vector(text: &str) -> Vec<f32> {
            vec![
                f32::from(u8::from(text.contains("alpha"))),
                f32::from(u8::from(text.contains("beta"))),
                0.1,
            ]
        }
    }

    impl SemanticEmbedder for FakeEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            while self.hold.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(passages
                .iter()
                .map(|passage| Self::vector(passage))
                .collect())
        }

        fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(Self::vector(query)))
        }
    }

    /// A worker with [`FakeEmbedder`], its cache in a sandbox under `root`,
    /// and the switch that holds its passage embedding (upstream:
    /// `spawn_semantic_worker_with`).
    fn spawn_fake_worker(
        root: &Path,
    ) -> (
        mpsc::Sender<SemanticWorkerCommand>,
        mpsc::Receiver<SemanticSearchMessage>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let embedder_hold = hold.clone();
        let factory: SemanticEmbedderFactory = Arc::new(move || {
            Ok(Box::new(FakeEmbedder {
                hold: embedder_hold.clone(),
            }) as Box<dyn SemanticEmbedder>)
        });
        let config = sandbox_config(root).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        (tx, rx, hold)
    }

    fn send_corpus(
        tx: &mpsc::Sender<SemanticWorkerCommand>,
        version: u64,
        conversations: Vec<Conversation>,
    ) {
        let indices = (0..conversations.len()).collect();
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: version,
            conversations: corpus(conversations),
            texts: None,
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: version,
            scope_version: version,
            indices: Arc::new(indices),
        })
        .expect("send scope");
    }

    fn send_search(
        tx: &mpsc::Sender<SemanticWorkerCommand>,
        generation: u64,
        version: u64,
        query: &str,
        prewarm: bool,
    ) {
        tx.send(SemanticWorkerCommand::Search {
            generation,
            query: ParsedQuery::parse(query),
            corpus_version: version,
            scope_version: version,
            prewarm,
        })
        .expect("send semantic request");
    }

    fn recv(rx: &mpsc::Receiver<SemanticSearchMessage>) -> SemanticSearchMessage {
        rx.recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("worker message")
    }

    /// Receives until indexing finishes, returning the searches answered on
    /// the way.
    fn recv_until_indexed(
        rx: &mpsc::Receiver<SemanticSearchMessage>,
    ) -> Vec<SemanticSearchResponse> {
        let mut responses = Vec::new();
        loop {
            match recv(rx) {
                SemanticSearchMessage::Complete(response) => responses.push(response),
                SemanticSearchMessage::Idle { .. } => return responses,
                SemanticSearchMessage::Indexing(_) | SemanticSearchMessage::Progress { .. } => {}
            }
        }
    }

    #[test]
    fn maps_domain_hits_to_original_indices_and_metadata() {
        let (expected_score_breakdown, expected_explanation) = beta_hit_metadata(42, "session-b");
        let response = semantic_search_response(
            7,
            SemanticIndexResponse {
                hits: vec![crate::semantic::types::SemanticHit::new(
                    expected_score_breakdown,
                    expected_explanation.clone(),
                )],
                chunk_hits: Vec::new(),
                indexed_chunk_count: 1,
                missing_chunk_count: 0,
                embedded_chunk_count: 0,
                query_embedding_returned: true,
                progress: SemanticIndexProgress::Complete,
                prewarm: false,
            },
            &[],
            &ParsedQuery::parse("beta"),
        );

        assert_eq!(response.generation, 7);
        assert_eq!(response.filtered, vec![42]);
        assert_eq!(response.progress, SemanticProgress::Complete);
        let metadata = &response.metadata[&42];
        assert_eq!(metadata.score_breakdown, expected_score_breakdown);
        assert_eq!(metadata.explanation, expected_explanation);
    }

    #[test]
    fn empty_visible_dialogue_returns_before_embedder_initialization() {
        let root = tempfile::tempdir().unwrap();
        let (factory, calls) = fake_embedder_factory();
        let config = sandbox_config(root.path()).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        send_worker_setup(&tx, vec![], "alpha", false);
        let response = recv_empty_complete(&rx);
        assert_eq!(response.progress, SemanticProgress::EmptyCorpus);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn empty_quoted_search_returns_idle_without_embedding() {
        let root = tempfile::tempdir().unwrap();
        let (factory, calls) = fake_embedder_factory();
        let config = sandbox_config(root.path()).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        send_worker_setup(&tx, vec!["visible dialogue"], "\"\"", false);
        let response = recv_empty_complete(&rx);
        assert_eq!(response.progress, SemanticProgress::Idle);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn prewarm_answers_at_once_and_indexes_in_the_background() {
        let root = tempfile::tempdir().unwrap();
        let cache_path = sandbox_config(root.path()).cache_path.unwrap();
        let (tx, rx, _hold) = spawn_fake_worker(root.path());
        send_worker_setup(&tx, vec!["visible dialogue"], "\"\"", true);

        let SemanticSearchMessage::Progress { progress, .. } = recv(&rx) else {
            panic!("expected model initialization first");
        };
        assert_eq!(progress, SemanticProgress::InitializingModel);
        let SemanticSearchMessage::Indexing(indexing) = recv(&rx) else {
            panic!("expected indexing to start");
        };
        assert_eq!(indexing.completed, 0);
        assert!(indexing.total > 0);
        let SemanticSearchMessage::Complete(response) = recv(&rx) else {
            panic!("expected the prewarm to be answered");
        };
        assert!(response.prewarm);
        assert!(response.filtered.is_empty());
        assert_eq!(response.error, None);
        assert_ne!(response.progress, SemanticProgress::Idle);
        // Fork: nothing embedded yet is not `sem no text`.
        assert_eq!(response.progress, SemanticProgress::CacheReady);
        assert!(recv_until_indexed(&rx).is_empty());
        // The embedding cache is written when indexing ends, inside the
        // sandbox only.
        assert!(cache_path.starts_with(root.path()));
        assert!(cache_path.exists(), "{cache_path:?}");
        drop(tx);
    }

    #[test]
    fn search_waiting_for_indexing_is_answered_when_it_finishes() {
        let root = tempfile::tempdir().unwrap();
        let (tx, rx, _hold) = spawn_fake_worker(root.path());
        send_corpus(
            &tx,
            1,
            vec![conversation(
                "/projects/a/session-a.jsonl",
                vec!["alpha notes"],
            )],
        );
        send_search(&tx, 1, 1, "alpha", false);

        let responses = recv_until_indexed(&rx);
        assert_eq!(responses.len(), 1, "answered once, after indexing");
        assert_eq!(responses[0].generation, 1);
        assert_eq!(responses[0].filtered, vec![0]);
    }

    /// Fork: when indexing cannot embed, a search waiting for it gets the
    /// error instead of waiting for good; one already answered keeps its
    /// results.
    #[test]
    fn search_waiting_for_indexing_gets_the_embedding_error() {
        /// Embeds passages until `fail` is set.
        struct Failing {
            fail: Arc<std::sync::atomic::AtomicBool>,
        }
        impl SemanticEmbedder for Failing {
            fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
                if self.fail.load(Ordering::SeqCst) {
                    return Err(AppError::ConfigError("no model".to_string()));
                }
                Ok(passages
                    .iter()
                    .map(|passage| FakeEmbedder::vector(passage))
                    .collect())
            }

            fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
                Ok(Some(FakeEmbedder::vector(query)))
            }
        }
        let root = tempfile::tempdir().unwrap();
        let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let embedder_fail = fail.clone();
        let factory: SemanticEmbedderFactory = Arc::new(move || {
            Ok(Box::new(Failing {
                fail: embedder_fail.clone(),
            }) as Box<dyn SemanticEmbedder>)
        });
        let config = sandbox_config(root.path()).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        let alpha = || conversation("/projects/a/session-a.jsonl", vec!["alpha notes"]);
        send_corpus(&tx, 1, vec![alpha()]);
        send_search(&tx, 1, 1, "alpha", false);

        let responses = recv_until_indexed(&rx);
        assert_eq!(responses.len(), 1, "answered once, with the error");
        assert_eq!(responses[0].generation, 1);
        assert!(responses[0].filtered.is_empty());
        assert_eq!(responses[0].progress, SemanticProgress::Failed);
        assert_eq!(
            responses[0].error.as_deref(),
            Some("Configuration error: no model")
        );

        // Embedded once embedding works; then a new conversation fails to
        // embed behind a search that the embedded one answers.
        fail.store(false, Ordering::SeqCst);
        send_search(&tx, 2, 1, "", true);
        recv_until_indexed(&rx);
        fail.store(true, Ordering::SeqCst);
        send_corpus(
            &tx,
            2,
            vec![
                alpha(),
                conversation("/projects/b/session-b.jsonl", vec!["beta notes"]),
            ],
        );
        send_search(&tx, 3, 2, "alpha", false);
        let responses = recv_until_indexed(&rx);
        assert_eq!(responses.len(), 1, "answered once, before indexing");
        assert_eq!(responses[0].generation, 3);
        assert_eq!(responses[0].filtered, vec![0]);
        assert_eq!(responses[0].error, None);
    }

    #[test]
    fn search_is_answered_once_indexed_conversations_match() {
        let root = tempfile::tempdir().unwrap();
        let (tx, rx, _hold) = spawn_fake_worker(root.path());
        let mut conversations = vec![conversation(
            "/projects/a/session-a.jsonl",
            vec!["alpha notes"],
        )];
        conversations.extend((1..=DEFAULT_EMBEDDING_BATCH_SIZE * 2).map(|index| {
            let text = format!("beta {index}");
            conversation(&format!("/projects/b/session-{index}.jsonl"), vec![&text])
        }));
        send_corpus(&tx, 1, conversations);
        send_search(&tx, 1, 1, "alpha", false);

        let mut indexing = None;
        let mut first_answer_during = None;
        let mut responses = Vec::new();
        loop {
            match recv(&rx) {
                SemanticSearchMessage::Complete(response) => {
                    first_answer_during = first_answer_during.or(indexing);
                    responses.push(response);
                }
                SemanticSearchMessage::Indexing(progress) => indexing = Some(progress),
                SemanticSearchMessage::Idle { .. } => break,
                SemanticSearchMessage::Progress { .. } => {}
            }
        }
        let first_answer_during: SemanticIndexing =
            first_answer_during.expect("answered while indexing");
        assert!(first_answer_during.completed > 0);
        assert!(first_answer_during.completed < first_answer_during.total);
        assert_eq!(responses.len(), 2, "answered, then refreshed");
        assert!(
            responses
                .iter()
                .all(|response| response.generation == 1 && response.filtered[0] == 0)
        );
    }

    #[test]
    fn search_ranks_embedded_conversations_while_indexing_new_ones() {
        let root = tempfile::tempdir().unwrap();
        let (tx, rx, hold) = spawn_fake_worker(root.path());
        let alpha = || conversation("/projects/a/session-a.jsonl", vec!["alpha notes"]);
        send_corpus(&tx, 1, vec![alpha()]);
        send_search(&tx, 1, 1, "", true);
        recv_until_indexed(&rx);

        hold.store(true, std::sync::atomic::Ordering::SeqCst);
        send_corpus(
            &tx,
            2,
            vec![
                alpha(),
                conversation("/projects/b/session-b.jsonl", vec!["beta notes"]),
            ],
        );
        send_search(&tx, 2, 2, "alpha", false);
        let first = loop {
            match recv(&rx) {
                SemanticSearchMessage::Complete(response) => break response,
                SemanticSearchMessage::Idle { .. } => panic!("indexing ran while held"),
                _ => {}
            }
        };
        assert_eq!(first.generation, 2);
        assert_eq!(first.filtered, vec![0]);

        hold.store(false, std::sync::atomic::Ordering::SeqCst);
        let refreshed = recv_until_indexed(&rx);
        assert_eq!(refreshed.len(), 1, "refreshed once indexing finishes");
        assert_eq!(refreshed[0].generation, 2);
        assert_eq!(refreshed[0].filtered[0], 0);
    }

    /// Fork (K6): closing the channel and cancelling, as
    /// `Browser::suspend_semantic` does, stops a slice that is running at
    /// its next batch, so the worker ends without finishing its indexing.
    #[test]
    fn closing_the_channel_stops_a_running_slice() {
        const BATCH: usize = DEFAULT_EMBEDDING_BATCH_SIZE;
        /// The first call takes a millisecond (the first slice measures
        /// it and grows the next one past a batch); the second waits while
        /// `hold` is set.
        struct Counting {
            hold: Arc<std::sync::atomic::AtomicBool>,
            passages: Arc<AtomicUsize>,
        }
        impl SemanticEmbedder for Counting {
            fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                let before = self.passages.fetch_add(texts.len(), Ordering::SeqCst);
                if before == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
                while before > 0 && self.hold.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(texts
                    .iter()
                    .map(|text| FakeEmbedder::vector(text))
                    .collect())
            }

            fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
                Ok(Some(FakeEmbedder::vector(query)))
            }
        }
        let root = tempfile::tempdir().unwrap();
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let passages = Arc::new(AtomicUsize::new(0));
        let (embedder_hold, embedder_passages) = (hold.clone(), passages.clone());
        let factory: SemanticEmbedderFactory = Arc::new(move || {
            Ok(Box::new(Counting {
                hold: embedder_hold.clone(),
                passages: embedder_passages.clone(),
            }) as Box<dyn SemanticEmbedder>)
        });
        let worker = spawn_semantic_worker_thread(
            sandbox_config(root.path()).with_embedder_factory(factory),
        );
        // Four batches: the first slice embeds one, the second the rest.
        let conversations = (0..BATCH * 4)
            .map(|index| {
                let text = format!("beta {index}");
                conversation(&format!("/projects/b/session-{index}.jsonl"), vec![&text])
            })
            .collect();
        send_corpus(&worker.tx, 1, conversations);
        send_search(&worker.tx, 1, 1, "", true);
        // The second slice's first batch is being embedded.
        let deadline = std::time::Instant::now() + WORKER_RESPONSE_TIMEOUT;
        while passages.load(Ordering::SeqCst) <= BATCH {
            assert!(std::time::Instant::now() < deadline, "no second slice");
            std::thread::sleep(Duration::from_millis(1));
        }
        let embedding = passages.load(Ordering::SeqCst);
        drop(worker.tx);
        worker.cancellation.cancel();
        hold.store(false, Ordering::SeqCst);
        worker.thread.join().unwrap();
        assert_eq!(
            passages.load(Ordering::SeqCst),
            embedding,
            "the slice stopped after its running batch"
        );
        assert!(embedding < BATCH * 4);
        let messages: Vec<_> = worker.rx.try_iter().collect();
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message, SemanticSearchMessage::Idle { .. })),
            "indexing did not finish"
        );
    }

    #[test]
    fn quoted_only_search_uses_literal_fallback_without_embedding() {
        let root = tempfile::tempdir().unwrap();
        let (factory, calls) = fake_embedder_factory();
        let config = sandbox_config(root.path()).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        send_worker_setup(&tx, vec![], "\"title sentinel\"", false);
        let message = rx
            .recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("literal response");

        match message {
            SemanticSearchMessage::Complete(response) => {
                assert_eq!(response.filtered, vec![0]);
                assert!(
                    response.metadata[&0]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
                assert_eq!(response.error, None);
                assert_eq!(response.progress, SemanticProgress::Complete);
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before literal response: {progress:?}");
            }
            SemanticSearchMessage::Indexing(indexing) => {
                panic!("unexpected indexing before literal response: {indexing:?}");
            }
            SemanticSearchMessage::Idle { generation } => {
                panic!("unexpected idle before literal response: {generation}");
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn quoted_only_search_of_cards_reads_matches_from_the_index() {
        let mut old = conversation("/projects/project-a/session-a.jsonl", vec![]);
        old.timestamp = Local::now() - ChronoDuration::days(1);
        let mut newest = conversation("/projects/project-a/session-b.jsonl", vec![]);
        newest.timestamp = Local::now();
        let mut hidden = conversation("/projects/project-a/session-c.jsonl", vec![]);
        hidden.timestamp = Local::now() + ChronoDuration::days(1);
        let mut unrelated = conversation("/projects/project-a/session-d.jsonl", vec![]);
        unrelated.full_text = "nothing to see".into();
        let root = tempfile::tempdir().unwrap();
        crate::store::generation::publish(root.path(), &[old, newest, hidden, unrelated]).unwrap();
        let snapshot = crate::store::Snapshot::open(
            root.path(),
            false,
            crate::time_filter::TimeFilter::default(),
        )
        .unwrap();
        let cards = crate::store::SemanticCards::load(&snapshot, None).unwrap();
        let index = SearchIndex::open(snapshot).unwrap();
        let paths: Vec<_> = cards
            .conversations()
            .iter()
            .map(|card| card.path.display().to_string())
            .collect();
        let doc = |name: &str| paths.iter().position(|path| path.ends_with(name)).unwrap();
        assert!(
            cards
                .conversations()
                .iter()
                .all(|card| card.full_text.is_empty())
        );

        let response = indexed_literal_semantic_response(
            1,
            false,
            &index,
            cards.conversations(),
            &[
                doc("session-a.jsonl"),
                doc("session-b.jsonl"),
                doc("session-d.jsonl"),
            ],
            &ParsedQuery::parse("\"title sentinel\""),
        );

        assert_eq!(
            response.filtered,
            vec![doc("session-b.jsonl"), doc("session-a.jsonl")]
        );
        for index in &response.filtered {
            assert!(
                response.metadata[index]
                    .explanation
                    .evidence_preview
                    .contains("title sentinel")
            );
        }
        assert_eq!(response.progress, SemanticProgress::Complete);
    }

    #[test]
    fn quoted_only_search_respects_scope_and_orders_newest_first() {
        let mut old = conversation("/projects/project-a/session-a.jsonl", vec![]);
        old.timestamp = Local::now() - ChronoDuration::days(1);
        let mut newest = conversation("/projects/project-a/session-b.jsonl", vec![]);
        newest.timestamp = Local::now();
        let mut hidden = conversation("/projects/project-a/session-c.jsonl", vec![]);
        hidden.timestamp = Local::now() + ChronoDuration::days(1);
        let root = tempfile::tempdir().unwrap();
        let (tx, rx, _cancellation) = spawn_semantic_worker(sandbox_config(root.path()));
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![old, newest, hidden]),
            texts: None,
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0, 1]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("\"title sentinel\""),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .expect("send semantic request");

        match rx
            .recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("literal response")
        {
            SemanticSearchMessage::Complete(response) => {
                assert_eq!(response.filtered, vec![1, 0]);
                assert!(
                    response.metadata[&1]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
                assert!(
                    response.metadata[&0]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before literal response: {progress:?}");
            }
            SemanticSearchMessage::Indexing(indexing) => {
                panic!("unexpected indexing before literal response: {indexing:?}");
            }
            SemanticSearchMessage::Idle { generation } => {
                panic!("unexpected idle before literal response: {generation}");
            }
        }
    }

    #[test]
    fn s022_exact_fallback_metadata_and_evidence_window() {
        // Quoted-only semantic queries: `exact` metadata, evidence ±80
        // characters around the first hidden literal match, `…` marks.
        let before = "b".repeat(120);
        let after = "a".repeat(120);
        let mut target = conversation("/projects/p/target.jsonl", vec![]);
        target.full_text = format!("{before} Needle Phrase {after}").into();
        target.preview = "visible preview".to_string().into();
        let corpus = corpus(vec![target]);
        let response = exact_literal_semantic_response(
            9,
            false,
            &corpus,
            &[0],
            &ParsedQuery::parse("\"Needle Phrase\""),
        );
        assert_eq!(response.filtered, vec![0]);
        assert_eq!(response.progress, SemanticProgress::Complete);
        let metadata = &response.metadata[&0];
        assert_eq!(metadata.explanation.quality_label, "exact");
        assert_eq!(metadata.explanation.quality, SemanticQuality::Strong);
        assert_eq!(
            metadata.explanation.rationale_kind,
            SemanticRationaleKind::LexicalBoosted
        );
        assert_eq!(metadata.explanation.matched_terms, vec!["Needle Phrase"]);
        assert_eq!(
            metadata.explanation.evidence_preview,
            // Upstream's ±80-character window (`evidence_window`).
            format!("…{} Needle Phrase {}…", &before[..80], &after[..79])
        );
        assert_eq!(metadata.score_breakdown.hybrid, 0.0);
        // Smart case: a capitalized literal does not match lower case.
        let response = exact_literal_semantic_response(
            9,
            false,
            &corpus,
            &[0],
            &ParsedQuery::parse("\"needle phrase\""),
        );
        assert_eq!(response.filtered, vec![0]);
        let response = exact_literal_semantic_response(
            9,
            false,
            &corpus,
            &[0],
            &ParsedQuery::parse("\"NEEDLE PHRASE\""),
        );
        assert!(response.filtered.is_empty());
    }

    #[test]
    fn worker_builds_distinct_full_corpus_and_scoped_candidates() {
        let mut worker = SemanticWorkerState::default();
        let cancellation = SemanticCancellationToken::new();
        worker.apply_command(
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version: 1,
                conversations: corpus(vec![
                    conversation("/projects/project-a/session-a.jsonl", vec!["hidden"]),
                    conversation("/projects/project-a/session-b.jsonl", vec!["visible"]),
                ]),
                texts: None,
            },
            &cancellation,
        );
        worker.apply_command(
            SemanticWorkerCommand::UpdateScope {
                corpus_version: 1,
                scope_version: 1,
                indices: Arc::new(vec![1]),
            },
            &cancellation,
        );

        let full_corpus = worker.full_corpus_candidates();
        let candidates = worker.semantic_candidates();

        assert_eq!(full_corpus.len(), 2);
        assert_eq!(full_corpus[0].index, 0);
        assert_eq!(full_corpus[1].index, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].index, 1);
        assert_eq!(
            candidates[0].conversation.semantic_turns.to_vec(),
            vec!["visible"]
        );
    }

    #[test]
    fn stale_search_is_not_current_after_scope_advances() {
        let mut worker = SemanticWorkerState::default();
        let cancellation = SemanticCancellationToken::new();
        worker.apply_command(
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version: 1,
                conversations: corpus(vec![conversation(
                    "/projects/project-a/session-a.jsonl",
                    vec!["visible"],
                )]),
                texts: None,
            },
            &cancellation,
        );
        worker.apply_command(
            SemanticWorkerCommand::UpdateScope {
                corpus_version: 1,
                scope_version: 2,
                indices: Arc::new(vec![0]),
            },
            &cancellation,
        );
        let request = SemanticSearchRequest {
            generation: 1,
            query: ParsedQuery::parse("visible"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        };

        assert!(matches!(
            worker.version_state_for(&request),
            Some(VersionState::Stale)
        ));
    }

    #[test]
    fn semantic_search_ranks_with_injected_embedder_and_reports_progress() {
        let root = tempfile::tempdir().unwrap();
        let (factory, calls) = fake_embedder_factory();
        let config = sandbox_config(root.path()).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![
                conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
                conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
            ]),
            texts: None,
        })
        .unwrap();
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0, 1]),
        })
        .unwrap();
        tx.send(SemanticWorkerCommand::Search {
            generation: 3,
            query: ParsedQuery::parse("beta"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .unwrap();

        // Nothing is embedded yet: the search waits for indexing, which
        // embeds both passages, and is answered when it finishes.
        let mut progress = Vec::new();
        let mut indexing = Vec::new();
        let response = loop {
            match rx.recv_timeout(WORKER_RESPONSE_TIMEOUT).unwrap() {
                SemanticSearchMessage::Progress {
                    generation,
                    progress: step,
                } => {
                    assert_eq!(generation, 3);
                    progress.push(step);
                }
                SemanticSearchMessage::Indexing(step) => indexing.push(step),
                SemanticSearchMessage::Idle { .. } => panic!("idle before the answer"),
                SemanticSearchMessage::Complete(response) => break response,
            }
        };
        assert_eq!(
            progress,
            vec![
                SemanticProgress::InitializingModel,
                SemanticProgress::Embedding {
                    completed: 0,
                    total: 2
                }
            ]
        );
        assert_eq!(
            indexing.first(),
            Some(&SemanticIndexing {
                completed: 0,
                total: 2
            })
        );
        assert_eq!(
            rx.recv_timeout(WORKER_RESPONSE_TIMEOUT)
                .ok()
                .and_then(|message| match message {
                    SemanticSearchMessage::Idle { generation } => Some(generation),
                    _ => None,
                }),
            Some(3),
            "idle follows the answer"
        );
        assert_eq!(response.error, None);
        assert_eq!(response.generation, 3);
        assert_eq!(response.filtered.first(), Some(&1));
        assert!(response.metadata.contains_key(&1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // The embedder is created once per worker and reused.
        tx.send(SemanticWorkerCommand::Search {
            generation: 4,
            query: ParsedQuery::parse("alpha"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .unwrap();
        let response = recv_complete(&rx);
        assert_eq!(response.generation, 4);
        assert_eq!(response.filtered.first(), Some(&0));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn embedder_initialization_failure_is_reported_and_retried() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let factory: SemanticEmbedderFactory = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(crate::error::AppError::ConfigError("no model".to_string()))
        });
        let config = sandbox_config(root.path()).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        send_worker_setup(&tx, vec!["visible dialogue"], "dialogue", false);
        let response = recv_complete(&rx);
        assert_eq!(response.progress, SemanticProgress::Failed);
        assert_eq!(
            response.error.as_deref(),
            Some("Configuration error: no model")
        );

        tx.send(SemanticWorkerCommand::Search {
            generation: 2,
            query: ParsedQuery::parse("dialogue"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .unwrap();
        let response = recv_complete(&rx);
        assert_eq!(response.generation, 2);
        assert_eq!(response.progress, SemanticProgress::Failed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn default_factory_refuses_to_load_the_model_in_tests() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let config = SemanticWorkerConfig::from_env(&env);
        assert!(config.embedder_factory.is_none());
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        send_worker_setup(&tx, vec!["visible dialogue"], "dialogue", false);
        let response = recv_complete(&rx);
        assert_eq!(response.progress, SemanticProgress::Failed);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("tests must inject")),
            "{:?}",
            response.error
        );
        // Nothing was downloaded into the sandbox model directory.
        assert!(!env.semantic_model_dir().unwrap().exists());
    }

    #[test]
    fn worker_calls_the_waker_after_posting() {
        let root = tempfile::tempdir().unwrap();
        // The worker wakes right after it posts, so the wake may land just
        // after the answer is received: wait for it instead of reading a
        // counter immediately.
        let (wake_tx, wake_rx) = mpsc::channel();
        let config = sandbox_config(root.path()).with_waker(Arc::new(move || {
            let _ = wake_tx.send(());
        }));
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        send_worker_setup(&tx, vec![], "\"title sentinel\"", false);
        let _ = recv_complete(&rx);
        wake_rx
            .recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("the worker calls the waker after posting");
    }

    #[test]
    fn config_from_env_points_into_the_semantic_cache_dir() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let config = SemanticWorkerConfig::from_env(&env);
        assert_eq!(config.cache_path, env.embedding_cache_file());
        assert_eq!(config.model_dir, env.semantic_model_dir());
        assert!(config.cache_path.unwrap().starts_with(root.path()));
        assert!(format!("{:?}", SemanticWorkerConfig::default()).contains("embedder_factory"));
    }

    /// Real fastembed model (downloads ~130 MB into a temporary directory).
    /// Run with `CLAUDE_HISTORY_NETWORK_TESTS=1 cargo test -- --ignored`.
    #[test]
    #[ignore = "downloads the embedding model; needs network"]
    fn real_model_ranks_semantically_related_dialogue() {
        if std::env::var_os("CLAUDE_HISTORY_NETWORK_TESTS").is_none() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let model_dir = env.semantic_model_dir().unwrap();
        let factory: SemanticEmbedderFactory = Arc::new(move || {
            Ok(
                Box::new(crate::semantic::fastembed::real_model_for_network_test(
                    model_dir.clone(),
                )?) as Box<dyn SemanticEmbedder>,
            )
        });
        let config = SemanticWorkerConfig::from_env(&env).with_embedder_factory(factory);
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![
                conversation(
                    "/projects/a/cooking.jsonl",
                    vec!["how long should I bake sourdough bread"],
                ),
                conversation(
                    "/projects/a/rust.jsonl",
                    vec!["fix the borrow checker error in my rust code"],
                ),
            ]),
            texts: None,
        })
        .unwrap();
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0, 1]),
        })
        .unwrap();
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("baking recipes"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .unwrap();
        let response = loop {
            match rx.recv_timeout(Duration::from_secs(600)).unwrap() {
                SemanticSearchMessage::Progress { .. }
                | SemanticSearchMessage::Indexing(_)
                | SemanticSearchMessage::Idle { .. } => {
                    continue;
                }
                SemanticSearchMessage::Complete(response) => break response,
            }
        };
        assert_eq!(response.error, None);
        assert_eq!(response.filtered.first(), Some(&0));
    }
}
