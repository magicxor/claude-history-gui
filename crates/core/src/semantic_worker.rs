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
//!   `FastembedEmbedder::new_quiet()`). The default factory loads fastembed
//!   from [`SemanticWorkerConfig::model_dir`] without download progress
//!   output. Under `cfg(test)` the default factory refuses to run, so a test
//!   can never download the model by accident.
//! * After posting every message the worker calls the optional
//!   [`SemanticWorkerConfig::waker`], so a GUI can wake its poller instead of
//!   polling on a timer.
//!
//! Everything else is unchanged: the command protocol, corpus/scope version
//! gating, coalescing of queued commands, the quoted-only exact fallback
//! (no embeddings), evidence windows (±80 characters with `…`), and the
//! progress mapping.

use crate::browser::{SemanticProgress, SemanticResultMetadata};
use crate::env::HistoryEnv;
use crate::error::Result;
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
    EmbeddingCache, SemanticCancellationToken, SemanticChunkIdentity, SemanticExplanation,
    SemanticQuality, SemanticRationaleKind, SemanticScoreBreakdown,
};
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

    fn create_embedder(&self) -> Result<Box<dyn SemanticEmbedder>> {
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
    crate::semantic::fastembed::quiet_embedder_in(model_dir.to_path_buf())
}

#[derive(Clone)]
pub enum SemanticWorkerCommand {
    UpdateCorpus {
        corpus_version: u64,
        conversations: Arc<Vec<Arc<Conversation>>>,
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
/// returns; cancel it first to make that quick).
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

    while let Ok(command) = cmd_rx.recv() {
        let mut request = worker.apply_command(command, &cancellation_source);
        while let Ok(pending) = cmd_rx.try_recv() {
            if let Some(search) = worker.apply_command(pending, &cancellation_source) {
                request = Some(search);
            }
        }

        let Some(request) = request else {
            continue;
        };
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
            let response = exact_literal_semantic_response(
                request.generation,
                request.prewarm,
                worker.corpus.as_ref(),
                worker.scope.as_ref(),
                &request.query,
            );
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
                let embedder = embedder.as_deref_mut().expect("embedder initialized above");
                rank_or_prewarm_semantic_request(
                    request.generation,
                    &index_request,
                    &request.query,
                    &mut state,
                    embedder,
                    &cancellation,
                    &outbox,
                    &mut save_cache,
                )
                .unwrap_or_else(|error| {
                    failed_semantic_response(request.generation, request.prewarm, error.to_string())
                })
            }
            Ok(false) => match state.clear_empty(&index_request, &cancellation) {
                Ok(()) => empty_semantic_response(request.generation, request.prewarm),
                Err(error) => {
                    failed_semantic_response(request.generation, request.prewarm, error.to_string())
                }
            },
            Err(error) => {
                failed_semantic_response(request.generation, request.prewarm, error.to_string())
            }
        };

        outbox.post(SemanticSearchMessage::Complete(response));
    }
}

#[derive(Default)]
struct SemanticWorkerState {
    corpus_version: u64,
    scope_version: u64,
    scope_corpus_version: u64,
    corpus: Arc<Vec<Arc<Conversation>>>,
    scope: Arc<Vec<usize>>,
    pending_search: Option<SemanticSearchRequest>,
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
            } => {
                if corpus_version >= self.corpus_version {
                    self.corpus_version = corpus_version;
                    self.corpus = conversations;
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

#[allow(clippy::too_many_arguments)]
fn rank_or_prewarm_semantic_request(
    generation: u64,
    request: &SemanticIndexRequest<'_>,
    parsed: &ParsedQuery,
    state: &mut SemanticIndexState,
    embedder: &mut dyn SemanticEmbedder,
    cancellation: &SemanticCancellationToken,
    outbox: &Outbox,
    save_cache: &mut impl FnMut(&EmbeddingCache),
) -> Result<SemanticSearchResponse> {
    let response = state.refresh_or_prewarm(
        request,
        embedder,
        cancellation,
        |progress| {
            outbox.post(SemanticSearchMessage::Progress {
                generation,
                progress: semantic_progress(progress),
            });
        },
        save_cache,
    )?;
    Ok(semantic_search_response(
        generation,
        response,
        request.full_corpus,
        parsed,
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
            conversations
                .get(*index)
                .map(|conversation| (*index, exact_literal_metadata(conversation, parsed)))
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
                .match_ranges(&conversation.full_text)
                .first()
                .map(|range| evidence_window(&conversation.full_text, *range))
                .or_else(|| {
                    conversation.project_name.as_deref().and_then(|project| {
                        literal
                            .match_ranges(project)
                            .first()
                            .map(|range| evidence_window(project, *range))
                    })
                })
        })
        .unwrap_or_else(|| conversation.preview.clone())
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
            evidence_preview: literal_evidence_preview(conversation, parsed, ""),
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
        SemanticConversationFixture::new(path, semantic_turns)
            .with_normalized_search_text_lower()
            .build()
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
        }
    }

    fn recv_complete(rx: &mpsc::Receiver<SemanticSearchMessage>) -> SemanticSearchResponse {
        loop {
            match rx
                .recv_timeout(WORKER_RESPONSE_TIMEOUT)
                .expect("worker response")
            {
                SemanticSearchMessage::Progress { .. } => continue,
                SemanticSearchMessage::Complete(response) => return response,
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
    fn empty_prewarm_search_builds_cache_without_idle_short_circuit() {
        let root = tempfile::tempdir().unwrap();
        let config = sandbox_config(root.path());
        let cache_path = config.cache_path.clone().unwrap();
        let (tx, rx, _cancellation) = spawn_semantic_worker(config);
        send_worker_setup(&tx, vec!["visible dialogue"], "\"\"", true);

        let response = recv_complete(&rx);
        assert!(response.prewarm);
        assert!(response.filtered.is_empty());
        assert!(response.metadata.is_empty());
        assert_eq!(response.error, None);
        assert_ne!(response.progress, SemanticProgress::Idle);
        // The embedding cache is written inside the sandbox only.
        assert!(cache_path.starts_with(root.path()));
        assert!(cache_path.exists(), "{cache_path:?}");
        drop(tx);
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
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
        }
    }

    #[test]
    fn s022_exact_fallback_metadata_and_evidence_window() {
        // Quoted-only semantic queries: `exact` metadata, evidence ±80
        // characters around the first hidden literal match, `…` marks.
        let before = "b".repeat(120);
        let after = "a".repeat(120);
        let mut target = conversation("/projects/p/target.jsonl", vec![]);
        target.full_text = format!("{before} Needle Phrase {after}");
        target.preview = "visible preview".to_string();
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
        assert_eq!(candidates[0].conversation.semantic_turns, vec!["visible"]);
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

        let mut progress = Vec::new();
        let response = loop {
            match rx.recv_timeout(WORKER_RESPONSE_TIMEOUT).unwrap() {
                SemanticSearchMessage::Progress {
                    generation,
                    progress: step,
                } => {
                    assert_eq!(generation, 3);
                    progress.push(step);
                }
                SemanticSearchMessage::Complete(response) => break response,
            }
        };
        assert_eq!(progress.first(), Some(&SemanticProgress::InitializingModel));
        assert!(
            progress.contains(&SemanticProgress::Ranking),
            "{progress:?}"
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
                SemanticSearchMessage::Progress { .. } => continue,
                SemanticSearchMessage::Complete(response) => break response,
            }
        };
        assert_eq!(response.error, None);
        assert_eq!(response.filtered.first(), Some(&0));
    }
}
