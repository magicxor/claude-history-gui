//! Query dispatch, the lexical worker, and semantic orchestration (upstream
//! `tui/app/search_state.rs`).
//!
//! The lexical worker searches either the loaded corpus in memory or, when
//! the browser has one, a store generation's index ([`IndexScope`]). An
//! index search answers twice: the top [`TOP_PAGE`] rows with their evidence
//! first, then every result, unless a newer query arrives while it completes
//! the ranking (upstream ac7f591).
//!
//! # Differences from upstream
//!
//! * **Per-query data.** Every command is a query that carries what it
//!   searches: the browser's shared corpus and search data
//!   ([`SearchCommand::conversations`], [`SearchCommand::searchable`]) or the
//!   index with its documents in scope ([`SearchCommand::index`]). Upstream's
//!   worker kept them as state, set by `UpdateData` and `UseIndex` commands,
//!   and queued those commands while it completed an index search. Here the
//!   worker releases the snapshots before it answers (CCR-2), every command
//!   received while completing replaces that search, and a single `queued`
//!   slot holds it (upstream: a `VecDeque`).
//! * **Search data stays in the browser.** Upstream's `UpdateData` rebuilt
//!   every conversation's search data on the worker's pool; the browser
//!   keeps one shared `Arc` per conversation, built when the corpus is
//!   prepared off the UI thread, so the worker never rebuilds it. Upstream
//!   503fe42 (no `search_text_lower` on conversations) reuses a body by the
//!   `full_text` pointer in a map on the worker; the fork has no such map. An
//!   edited entry (rename, UUID-loaded session) takes the body normalized
//!   where the session was prepared ([`super::PreparedSession`], a
//!   background task in the GUI), reused by the replay of an edit at an
//!   install and by a carried UUID result: only its title, summary and
//!   project are normalized on the UI thread.
//! * **Waker.** The worker calls the waker after each answer (upstream's TUI
//!   polled).
//! * **Semantic corpus in generation mode.** As upstream ecc2f2f, semantic
//!   search ranks the generation's cards with their semantic data
//!   ([`store::SemanticCards`], read from the `sem-*.zst` segments on a
//!   `semantic-corpus` thread), and a swap reads the new generation's cards
//!   while the older ones serve. The scope is the cards whose card in the
//!   generation shown is in scope (upstream re-filtered the corpus by
//!   project, workspace, time and deletion), so the older cards serve only
//!   once mapped to the docs of the generation shown: by `generation-open`
//!   for a swap (see `cards`), else on a `semantic-corpus` thread (upstream
//!   mapped them on the TUI thread at the first result); a semantic query
//!   waits meanwhile (`sem loading conversations`, shown also after a scope
//!   change). Generations are told apart by their directory (store roots
//!   repeat names). A read of a generation no longer shown is given up, a
//!   read that fails is not retried for that generation until semantic
//!   mode is entered again (upstream read it again at every dispatch), and
//!   the corpus is sent to the semantic worker again once the index of its
//!   generation opens, so quoted-only queries read their text from it. The
//!   browser waits for a read it gave up when it is dropped (upstream
//!   detached it).
//! * **User-only view in memory.** As upstream ae7ae39, the worker builds
//!   [`search::user_only_view`] of the corpus and its search data on the
//!   first user-only query and keeps it. Since the corpus comes with each
//!   query, the view is kept for the corpus version it was built from
//!   ([`SearchCommand::corpus_version`]) rather than until upstream's
//!   `UpdateData`; it holds copies, never the browser's snapshots.
//! * **Background semantic indexing.** As upstream c6d0421, the semantic
//!   worker answers from the passages embedded so far and embeds the rest
//!   in the background ([`Browser::semantic_indexing`], `sem indexing`).
//!   The worker wakes the poller with each report (upstream's TUI polled
//!   every 250 ms while indexing). [`Browser::suspend_semantic`] closes the
//!   worker's channel before it cancels, and forgets the indexing progress
//!   with the worker (as does a replaced worker); a dropped browser cancels
//!   its worker's running slice (upstream detached it).

use super::corpus::{CorpusSnapshot, SearchSnapshot};
use super::{
    Browser, ListSearchMode, PreparedSession, SemanticProgress, SemanticResultMetadata, UuidLookup,
};
use crate::env::HistoryEnv;
use crate::history::Conversation;
use crate::search;
use crate::search::query::ParsedQuery;
use crate::semantic::types::SemanticCancellationToken;
use crate::semantic_worker::{
    SemanticIndexing, SemanticSearchMessage, SemanticWorkerCommand, SemanticWorkerConfig, Waker,
    spawn_semantic_worker_thread,
};
use crate::store;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

#[derive(Default)]
pub(super) struct SemanticSearchState {
    pub(super) available: bool,
    pub(super) pending_generation: Option<u64>,
    pub(super) pending_status: Option<SemanticProgress>,
    pub(super) prewarm_generation: Option<u64>,
    pub(super) prewarm_status: Option<SemanticProgress>,
    /// Progress of embedding the conversations the worker has no
    /// embeddings for, while it does.
    pub(super) indexing: Option<SemanticIndexing>,
    pub(super) last_status: SemanticProgress,
    pub(super) error: Option<String>,
    pub(super) results: HashMap<usize, SemanticResultMetadata>,
    pub(super) worker_tx: Option<mpsc::Sender<SemanticWorkerCommand>>,
    pub(super) worker_rx: Option<mpsc::Receiver<SemanticSearchMessage>>,
    pub(super) cancellation: Option<SemanticCancellationToken>,
    /// The worker thread, joined by [`Browser::suspend_semantic`]
    pub(super) worker_thread: Option<std::thread::JoinHandle<()>>,
    /// Suspended by [`Browser::suspend_semantic`]: no worker is started
    pub(super) suspended: bool,
}

/// What [`Browser::semantic_activity_status_text`] shows.
enum SemanticActivity {
    Loading,
    Preparing,
    Embedding { completed: usize, total: usize },
    Indexing { completed: usize, total: usize },
}

/// Conversations with semantic data that semantic search ranks in
/// generation mode, where rows are cards without it (upstream ecc2f2f).
/// Generations are named by [`store::Snapshot::generation_id`].
#[derive(Default)]
pub(super) struct SemanticCorpus {
    /// Cards of the newest generation read so far.
    pub(super) loaded: Option<Arc<store::SemanticCards>>,
    /// The generation being read and its `semantic-corpus` thread, while
    /// one is.
    pub(super) loading: Option<(String, SemanticJob)>,
    /// Generation doc of each loaded conversation, for the generation
    /// named. Only docs of the generation shown are used.
    pub(super) docs: Option<(String, Arc<Vec<Option<u32>>>)>,
    /// Maps `loaded` to the docs of the generation shown, while `docs` are
    /// another one's.
    mapping: Option<SemanticJob>,
    /// A generation whose semantic data could not be read: it is not read
    /// again until semantic mode is entered again.
    pub(super) failed: Option<String>,
    /// Whether the corpus last sent to the semantic worker went with the
    /// index of its generation.
    sent_texts: bool,
}

/// What a `semantic-corpus` thread sends: the cards it read (`None` when
/// the generation's semantic data could not be read) or mapped, and their
/// docs in a generation.
pub(super) struct LoadedSemantic {
    pub(super) cards: Option<Arc<store::SemanticCards>>,
    pub(super) docs: Option<(String, Arc<Vec<Option<u32>>>)>,
}

/// A `semantic-corpus` thread: it reads the cards of the generation shown,
/// or maps cards read before to its docs. Its handle goes to the browser's
/// helper threads once it answered or was given up, so the browser joins
/// it when dropped.
pub(super) struct SemanticJob {
    result: mpsc::Receiver<LoadedSemantic>,
    thread: std::thread::JoinHandle<()>,
    /// Set to give the job up: a read stops before its next card.
    cancel: Arc<AtomicBool>,
}

impl SemanticJob {
    /// Runs `work` on a `semantic-corpus` thread (8 MiB stack); it sends
    /// the result before it calls `waker`. `work` gets the job's cancel
    /// flag.
    pub(super) fn spawn(
        waker: Option<Waker>,
        work: impl FnOnce(&AtomicBool) -> LoadedSemantic + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("semantic-corpus".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let loaded = work(&cancelled);
                if tx.send(loaded).is_ok()
                    && let Some(waker) = waker
                {
                    waker();
                }
            })?;
        Ok(Self {
            result,
            thread,
            cancel,
        })
    }

    /// The answer once the thread sent it: `Some(None)` when the thread
    /// died without one.
    fn answer(&self) -> Option<Option<LoadedSemantic>> {
        match self.result.try_recv() {
            Ok(loaded) => Some(Some(loaded)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(None),
        }
    }

    /// Gives the job up (a read stops before its next card) and hands its
    /// thread to `helpers`, joined when the browser is dropped.
    fn retire(self, helpers: &mut super::HelperThreads) {
        self.cancel.store(true, Ordering::Relaxed);
        helpers.push(self.thread);
    }
}

/// A query for the lexical worker, with the browser's corpus and search data
/// at the time of the query (shared, not copied). The worker holds them only
/// while it searches and drops them before it answers: an idle worker pins
/// no snapshot, so a rename or a delete edits the browser's vectors in place
/// instead of copying their pointers (CCR-2).
pub(super) struct SearchCommand {
    pub(super) query: String,
    pub(super) generation: u64,
    pub(super) mode: ListSearchMode,
    /// Empty when `index` is set.
    pub(super) conversations: CorpusSnapshot,
    /// Empty when `index` is set.
    pub(super) searchable: SearchSnapshot,
    /// Search this generation index instead of `conversations`.
    pub(super) index: Option<IndexScope>,
    /// Search only the text the person typed (`Conversation::own_text`).
    pub(super) user_only: bool,
    /// Version of `conversations` (the browser's corpus version, changed
    /// with every edit of the corpus): the user-only view built for one
    /// version serves its later queries.
    pub(super) corpus_version: u64,
}

/// A generation index and the documents a query may return.
#[derive(Clone)]
pub(super) struct IndexScope {
    pub(super) index: Arc<store::SearchIndex>,
    /// Whether each document is in scope, by doc id.
    pub(super) docs: Arc<Vec<bool>>,
}

pub(super) struct SearchResponse {
    /// Conversation indices in rank order, or doc ids of `index` when it is
    /// set.
    pub(super) filtered: Vec<usize>,
    /// The index the doc ids belong to.
    pub(super) index: Option<Arc<store::SearchIndex>>,
    /// Whether `filtered` holds every result rather than the first page.
    pub(super) complete: bool,
    pub(super) generation: u64,
    pub(super) mode: ListSearchMode,
    pub(super) user_only: bool,
    /// Evidence keyed like `filtered`.
    pub(super) evidence: HashMap<usize, search::LexicalEvidence>,
}

/// Results ranked before the first response of an index search: more than
/// a screen holds.
pub(super) const TOP_PAGE: usize = 50;
/// Results ranked between checks for a newer command while completing.
const COMPLETE_STEP: usize = 256;
const MAX_EVIDENCE_ROWS: usize = 200;

/// With `user_only`, evidence comes from the text the person typed, shown
/// even where the preview already holds the match, since the preview mixes
/// in other messages.
fn build_search_evidence(
    conversations: &[Arc<Conversation>],
    filtered: &[usize],
    parsed: &ParsedQuery,
    user_only: bool,
) -> HashMap<usize, search::LexicalEvidence> {
    filtered
        .iter()
        .take(MAX_EVIDENCE_ROWS)
        .filter_map(|&index| {
            let conversation = &conversations[index];
            if user_only {
                search::build_lexical_evidence(&conversation.own_text, "", parsed)
            } else {
                search::build_lexical_evidence(
                    &conversation.full_text,
                    &conversation.preview,
                    parsed,
                )
            }
            .map(|evidence| (index, evidence))
        })
        .collect()
}

/// Evidence for card `doc` of `index` with its row preview `preview`, as
/// `build_search_evidence` builds it.
pub(super) fn index_evidence(
    index: &store::SearchIndex,
    doc: u32,
    preview: &str,
    parsed: &ParsedQuery,
    user_only: bool,
) -> Option<search::LexicalEvidence> {
    if user_only {
        search::build_lexical_evidence(&index.own_text(doc), "", parsed)
    } else {
        search::build_lexical_evidence(&index.text(doc), preview, parsed)
    }
}

/// Evidence for `docs` that `evidence` does not hold yet, read from their
/// text frames in parallel.
fn add_index_evidence(
    index: &store::SearchIndex,
    docs: &[u32],
    parsed: &ParsedQuery,
    user_only: bool,
    evidence: &mut HashMap<usize, search::LexicalEvidence>,
) {
    let missing: Vec<u32> = docs
        .iter()
        .copied()
        .filter(|&doc| !evidence.contains_key(&(doc as usize)))
        .collect();
    let found: Vec<(usize, search::LexicalEvidence)> = missing
        .par_iter()
        .filter_map(|&doc| {
            index_evidence(index, doc, index.cards().preview(doc), parsed, user_only)
                .map(|evidence| (doc as usize, evidence))
        })
        .collect();
    evidence.extend(found);
}

/// Coalescing (S-005): `first` is superseded by every command already queued,
/// only the latest query is searched.
pub(super) fn latest_command(
    first: SearchCommand,
    queue: &mpsc::Receiver<SearchCommand>,
) -> SearchCommand {
    let mut command = first;
    while let Ok(pending) = queue.try_recv() {
        command = pending;
    }
    command
}

struct SearchWorker {
    commands: mpsc::Receiver<SearchCommand>,
    responses: mpsc::Sender<SearchResponse>,
    waker: Option<Waker>,
    /// A command received while an index search was being completed; it
    /// replaces that search and is handled next.
    queued: Option<SearchCommand>,
    /// Ranks loaded conversations one query at a time, leaving the other
    /// cores to the UI and loader.
    memory_pool: rayon::ThreadPool,
    /// Ranks index queries and reads their evidence.
    index_pool: rayon::ThreadPool,
    /// `search::user_only_view` of a corpus and its search data, built on
    /// the first user-only search of that corpus version.
    own: Option<OwnView>,
}

/// The corpus reduced to what the person typed, for in-memory user-only
/// searches.
struct OwnView {
    /// [`SearchCommand::corpus_version`] of the corpus it was built from.
    corpus_version: u64,
    conversations: Vec<Conversation>,
    searchable: Vec<search::SearchableConversation>,
}

impl SearchWorker {
    fn run(mut self) {
        loop {
            let first = match self.queued.take() {
                Some(command) => command,
                None => match self.commands.recv() {
                    Ok(command) => command,
                    Err(_) => return,
                },
            };
            let mut command = latest_command(first, &self.commands);
            match command.index.take() {
                Some(scope) => {
                    let SearchCommand {
                        query,
                        generation,
                        mode,
                        conversations,
                        searchable,
                        user_only,
                        ..
                    } = command;
                    drop((conversations, searchable));
                    self.search_index(&scope, &query, generation, mode, user_only);
                }
                None => self.search_memory(command),
            }
        }
    }

    fn send(&self, response: SearchResponse) {
        if self.responses.send(response).is_ok()
            && let Some(waker) = &self.waker
        {
            waker();
        }
    }

    fn search_memory(&mut self, command: SearchCommand) {
        let SearchCommand {
            query,
            generation,
            mode,
            conversations,
            searchable,
            user_only,
            corpus_version,
            ..
        } = command;
        let current = |own: &OwnView| {
            own.corpus_version == corpus_version && own.conversations.len() == conversations.len()
        };
        if user_only && !self.own.as_ref().is_some_and(current) {
            // Free the view of an older corpus first.
            self.own = None;
            self.own = Some(self.index_pool.install(|| {
                let view = search::user_only_view(&conversations);
                let searchable = search::precompute_search_text(&view);
                OwnView {
                    corpus_version,
                    conversations: view,
                    searchable,
                }
            }));
        }
        let own = self.own.as_ref().filter(|_| user_only);
        let (filtered, evidence) = self.memory_pool.install(|| {
            let now = chrono::Local::now();
            let filtered = match own {
                Some(own) => search::search(&own.conversations, &own.searchable, &query, now),
                None => search::search(&conversations, &searchable, &query, now),
            };
            let parsed = ParsedQuery::parse(&query);
            let evidence = build_search_evidence(&conversations, &filtered, &parsed, user_only);
            (filtered, evidence)
        });
        // Release the snapshots before answering: once the browser sees the
        // answer, an edit finds its vectors unshared (when no other query is
        // queued).
        drop(searchable);
        drop(conversations);
        self.send(SearchResponse {
            filtered,
            index: None,
            complete: true,
            generation,
            mode,
            user_only,
            evidence,
        });
    }

    /// Sends the first page as soon as it is final, then every result,
    /// unless a newer search arrives first.
    fn search_index(
        &mut self,
        scope: &IndexScope,
        query: &str,
        generation: u64,
        mode: ListSearchMode,
        user_only: bool,
    ) {
        let index = &scope.index;
        let in_scope = |doc: u32| scope.docs.get(doc as usize).copied().unwrap_or(false);
        let parsed = ParsedQuery::parse(query);
        let mut evidence = HashMap::new();
        let mut ranking = self
            .index_pool
            .install(|| index.search(query, chrono::Local::now(), &in_scope, user_only));

        let top: Vec<u32> = self
            .index_pool
            .install(|| ranking.top(TOP_PAGE).iter().map(|hit| hit.doc).collect());
        self.index_pool
            .install(|| add_index_evidence(index, &top, &parsed, user_only, &mut evidence));
        let reply = IndexReply {
            scope,
            generation,
            mode,
            user_only,
        };
        let complete = top.len() < TOP_PAGE;
        self.send_index(&reply, &top, complete, &evidence);
        if complete {
            return;
        }

        let mut ranked = TOP_PAGE;
        loop {
            if let Ok(command) = self.commands.try_recv() {
                // Every command is a newer query: abandon this one.
                self.queued = Some(command);
                return;
            }
            ranked += COMPLETE_STEP;
            if self.index_pool.install(|| ranking.top(ranked).len()) < ranked {
                break;
            }
        }
        let docs: Vec<u32> = self
            .index_pool
            .install(|| ranking.complete().iter().map(|hit| hit.doc).collect());
        let shown = &docs[..docs.len().min(MAX_EVIDENCE_ROWS)];
        self.index_pool
            .install(|| add_index_evidence(index, shown, &parsed, user_only, &mut evidence));
        self.send_index(&reply, &docs, true, &evidence);
    }

    /// Answers with `docs` of the query's index.
    fn send_index(
        &self,
        reply: &IndexReply,
        docs: &[u32],
        complete: bool,
        evidence: &HashMap<usize, search::LexicalEvidence>,
    ) {
        self.send(SearchResponse {
            filtered: docs.iter().map(|&doc| doc as usize).collect(),
            index: Some(reply.scope.index.clone()),
            complete,
            generation: reply.generation,
            mode: reply.mode,
            user_only: reply.user_only,
            evidence: evidence.clone(),
        });
    }
}

/// What every response of one index search carries.
struct IndexReply<'a> {
    scope: &'a IndexScope,
    generation: u64,
    mode: ListSearchMode,
    user_only: bool,
}

/// The lexical search worker ("search-worker"): it drains every queued
/// command and runs only the latest query (coalescing), against the
/// snapshots or the index that query carries; the snapshots are released
/// before it answers. Loaded conversations are ranked on a one-thread pool,
/// index queries on a pool of up to 8 threads. `waker` is called after each
/// response.
pub(super) fn spawn_search_worker(
    waker: Option<Waker>,
) -> (mpsc::Sender<SearchCommand>, mpsc::Receiver<SearchResponse>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<SearchCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SearchResponse>();

    std::thread::Builder::new()
        .name("search-worker".into())
        .spawn(move || {
            let memory_pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|_| "lexical-rank-worker".to_string())
                .build()
                .expect("failed to create lexical ranking thread pool");
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
            let index_pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("index-search-{i}"))
                .build()
                .expect("failed to create index search thread pool");
            SearchWorker {
                commands: cmd_rx,
                responses: res_tx,
                waker,
                queued: None,
                memory_pool,
                index_pool,
                own: None,
            }
            .run();
        })
        .expect("failed to spawn search worker thread");

    (cmd_tx, res_rx)
}

impl Browser {
    pub(super) fn invalidate_search_generation(&mut self) {
        self.search_generation += 1;
        self.search_in_flight = false;
        self.search_page_shown = false;
        self.lexical_evidence.clear();
        self.clear_evidence_checked();
        self.semantic_search.pending_generation = None;
        self.semantic_search.pending_status = None;
        self.semantic_search.prewarm_generation = None;
        self.semantic_search.prewarm_status = None;
        self.semantic_search.last_status = if self.semantic_search.suspended {
            SemanticProgress::Suspended
        } else {
            SemanticProgress::Idle
        };
        self.semantic_search.error = None;
        self.semantic_search.results.clear();
    }

    fn semantic_worker_config(&self) -> SemanticWorkerConfig {
        SemanticWorkerConfig {
            embedder_factory: self.embedder_factory.clone(),
            waker: self.waker.clone(),
            ..SemanticWorkerConfig::from_env(&self.env)
        }
    }

    fn ensure_semantic_worker(&mut self) {
        if self.semantic_search.worker_tx.is_none() || self.semantic_search.worker_rx.is_none() {
            self.reset_semantic_worker();
        }
    }

    /// The rows in scope for the semantic worker: the cached scope (shared,
    /// no copy) when current. In generation mode, the semantic corpus's
    /// conversations whose card is in scope (time filter included, deleted
    /// ones out).
    pub(super) fn semantic_scope_indices(&self) -> Arc<Vec<usize>> {
        if self.snapshot.is_some() {
            // Docs of another generation would index this one's scope.
            let Some(docs) = self.semantic_docs_shown() else {
                return Arc::default();
            };
            return Arc::new(
                docs.iter()
                    .enumerate()
                    .filter(|(_, doc)| {
                        doc.is_some_and(|doc| {
                            self.cards.scope.get(doc as usize).copied().unwrap_or(false)
                        })
                    })
                    .map(|(index, _)| index)
                    .collect(),
            );
        }
        if self.scope_is_current() {
            return self.index.scope.rows().clone();
        }
        Arc::new(self.filter_indices(0..self.conversations.len()))
    }

    /// The conversations the semantic worker ranks: the loaded corpus, or
    /// the cards with their semantic data in generation mode (empty until
    /// they are read).
    fn semantic_conversations(&self) -> CorpusSnapshot {
        if self.snapshot.is_none() {
            return self.conversations.clone();
        }
        self.semantic_conversations_loaded().unwrap_or_default()
    }

    /// The cards semantic search ranks in generation mode, once read.
    pub(super) fn semantic_conversations_loaded(&self) -> Option<CorpusSnapshot> {
        self.semantic_corpus
            .loaded
            .as_ref()
            .map(|cards| cards.conversations().clone())
    }

    /// The generation index whose docs are the semantic corpus's cards:
    /// quoted-only semantic queries read their matches and text from it
    /// (upstream ecc2f2f).
    fn semantic_texts(&self) -> Option<Arc<store::SearchIndex>> {
        let cards = self.semantic_corpus.loaded.as_ref()?;
        self.search_index
            .clone()
            .filter(|index| index.cards().generation_id() == cards.generation_id())
    }

    /// The docs of the semantic corpus in the generation shown, once mapped
    /// to it.
    fn semantic_docs_shown(&self) -> Option<&Arc<Vec<Option<u32>>>> {
        let shown = self.generation_id()?;
        self.semantic_corpus
            .loaded
            .as_ref()
            .and(self.semantic_corpus.docs.as_ref())
            .filter(|(id, _)| *id == shown)
            .map(|(_, docs)| docs)
    }

    /// Takes the docs of the semantic corpus in the generation just
    /// installed (mapped off the UI thread), when they belong to the corpus
    /// loaded.
    pub(super) fn adopt_semantic_docs(
        &mut self,
        corpus: &CorpusSnapshot,
        mapped: Arc<Vec<Option<u32>>>,
    ) {
        let name = self.generation_id();
        if let (Some(cards), Some(name)) = (&self.semantic_corpus.loaded, name)
            && Arc::ptr_eq(cards.conversations(), corpus)
        {
            self.semantic_corpus.docs = Some((name, mapped));
            self.semantic_sent_scope_signature = None;
        }
    }

    /// Whether no semantic corpus serves the generation shown, nor will:
    /// its semantic data could not be read, and no cards read before are
    /// being mapped to it.
    fn semantic_corpus_failed(&self) -> bool {
        self.semantic_docs_shown().is_none()
            && self.semantic_corpus.mapping.is_none()
            && self.semantic_corpus.failed.is_some()
            && self.semantic_corpus.failed == self.generation_id()
    }

    /// Makes the semantic corpus of the generation shown ready (upstream
    /// ecc2f2f): starts reading the semantic data of its cards on a
    /// `semantic-corpus` thread unless they are read or being read (cards
    /// unchanged since the cards read before keep their data), and maps
    /// cards of another generation, which serve meanwhile, to the docs of
    /// the one shown, also off this thread. Returns whether a corpus serves
    /// the generation shown (always outside generation mode); until then
    /// semantic mode shows [`SemanticProgress::LoadingSessions`], and the
    /// poll that receives the corpus dispatches the query.
    pub(super) fn ensure_semantic_corpus(&mut self) -> bool {
        let Some(snapshot) = self.snapshot.clone() else {
            return true;
        };
        if !self.semantic_search.available {
            return false;
        }
        let shown = snapshot.generation_id();
        let corpus = &mut self.semantic_corpus;
        let current = corpus
            .loaded
            .as_ref()
            .is_some_and(|cards| cards.generation_id() == shown)
            || corpus
                .loading
                .as_ref()
                .is_some_and(|(loading, _)| *loading == shown)
            || corpus.failed.as_ref() == Some(&shown);
        if !current {
            // A read of a generation no longer shown is given up.
            if let Some((_, job)) = corpus.loading.take() {
                job.retire(&mut self.helpers);
            }
            let previous = corpus.loaded.clone();
            let cards = snapshot.clone();
            let read = SemanticJob::spawn(self.waker.clone(), move |cancel| {
                let cancelled = || cancel.load(Ordering::Relaxed);
                let cards =
                    store::SemanticCards::load_cancellable(&cards, previous.as_deref(), &cancelled);
                let docs = cards.as_ref().map(|cards| {
                    let docs = (0..cards.conversations().len() as u32).map(Some);
                    (cards.generation_id().to_owned(), Arc::new(docs.collect()))
                });
                LoadedSemantic {
                    cards: cards.map(Arc::new),
                    docs,
                }
            });
            match read {
                Ok(job) => self.semantic_corpus.loading = Some((shown, job)),
                Err(_) => self.semantic_corpus.failed = Some(shown),
            }
        }
        if self.semantic_docs_shown().is_some() {
            return true;
        }
        if self.semantic_corpus.mapping.is_none()
            && let Some(cards) = self.semantic_corpus.loaded.clone()
        {
            // Cards of another generation: mapped to this one, once.
            let mapping = SemanticJob::spawn(self.waker.clone(), move |_| LoadedSemantic {
                docs: Some((
                    snapshot.generation_id(),
                    Arc::new(super::cards::semantic_docs(
                        &snapshot,
                        cards.conversations(),
                    )),
                )),
                cards: Some(cards),
            });
            match mapping {
                Ok(job) => self.semantic_corpus.mapping = Some(job),
                Err(_) => {
                    self.semantic_search.error = Some("semantic corpus unavailable".to_string());
                    return false;
                }
            }
        }
        if self.list_search_mode == ListSearchMode::Semantic && !self.semantic_corpus_failed() {
            self.semantic_search.pending_status = Some(SemanticProgress::LoadingSessions);
        }
        false
    }

    /// Takes the cards read for a generation once they arrive, or their
    /// docs once mapped to the generation shown; in semantic mode a query
    /// waiting for them is dispatched (an empty one prewarms). Returns
    /// whether anything arrived.
    fn receive_semantic_corpus(&mut self) -> bool {
        let mut arrived = false;
        // Whether a query may wait for what arrived: not for a read of the
        // generation shown that failed while the cards read before serve it
        // (their query ran when they were mapped to it, and would run again
        // for nothing).
        let mut awaited = false;
        if let Some((_, job)) = &self.semantic_corpus.loading
            && let Some(answer) = job.answer()
        {
            let (id, job) = self.semantic_corpus.loading.take().expect("matched above");
            self.helpers.push(job.thread);
            arrived = true;
            match answer.and_then(|read| Some((read.cards?, read.docs))) {
                Some((cards, docs)) => {
                    self.semantic_corpus.loaded = Some(cards);
                    self.semantic_corpus.docs = docs;
                    // Mapping the cards read before is no use any more.
                    if let Some(job) = self.semantic_corpus.mapping.take() {
                        job.retire(&mut self.helpers);
                    }
                    self.readiness.semantic = true;
                    self.semantic_corpus_version += 1;
                    self.semantic_sent_scope_signature = None;
                    awaited = true;
                }
                // Cards read before keep serving (mapped to this one).
                None => {
                    awaited = self.generation_id().as_ref() != Some(&id)
                        || self.semantic_docs_shown().is_none();
                    self.semantic_corpus.failed = Some(id);
                }
            }
        }
        if let Some(job) = &self.semantic_corpus.mapping
            && let Some(answer) = job.answer()
        {
            if let Some(job) = self.semantic_corpus.mapping.take() {
                self.helpers.push(job.thread);
            }
            let Some(mapped) = answer else {
                // The mapping died: the next semantic query tries again.
                self.semantic_search.error = Some("semantic corpus unavailable".to_string());
                return true;
            };
            let shown = self.generation_id();
            let corpus = &mut self.semantic_corpus;
            let mapped_meanwhile = corpus.docs.as_ref().map(|(id, _)| id) == shown.as_ref();
            if !mapped_meanwhile {
                if let (Some(loaded), Some(cards)) = (&corpus.loaded, &mapped.cards)
                    && Arc::ptr_eq(loaded, cards)
                {
                    corpus.docs = mapped.docs;
                }
                self.semantic_sent_scope_signature = None;
                arrived = true;
                awaited = true;
            }
        }
        if !awaited || self.list_search_mode != ListSearchMode::Semantic {
            return arrived;
        }
        // Read or mapped for another generation than the one shown now
        // (swapped in meanwhile): read or mapped again, and the query waits
        // for that.
        if !self.ensure_semantic_corpus() {
            if self.semantic_corpus_failed() {
                self.semantic_search.pending_status = None;
                self.semantic_search.error = Some("semantic corpus unavailable".to_string());
                self.semantic_search.last_status = SemanticProgress::Failed;
            }
            return true;
        }
        if matches!(
            self.semantic_search.pending_status,
            Some(SemanticProgress::LoadingSessions)
        ) {
            self.semantic_search.pending_status = None;
        }
        if self.query.trim().is_empty() {
            self.prewarm_semantic_cache();
        } else {
            self.dispatch_semantic_search(self.query.trim().to_string(), false);
        }
        true
    }

    /// Uses `env` from now on (the loader moved to another set of session
    /// roots). The semantic corpus follows the generation shown, not the
    /// env.
    pub fn set_env(&mut self, env: Arc<HistoryEnv>) {
        if !Arc::ptr_eq(&self.env, &env) {
            self.env = env;
        }
    }

    /// Gives up the semantic corpus (generation mode ends, or the browser
    /// is dropped): a read or a mapping still running stops, and its thread
    /// is joined when the browser is dropped.
    pub(super) fn retire_semantic_corpus(&mut self) {
        let corpus = std::mem::take(&mut self.semantic_corpus);
        if let Some((_, job)) = corpus.loading {
            job.retire(&mut self.helpers);
        }
        if let Some(job) = corpus.mapping {
            job.retire(&mut self.helpers);
        }
    }

    /// Rows of semantic corpus conversations `indices`, in order, with
    /// `metadata` re-keyed by row. In generation mode conversations map to
    /// cards by doc when the corpus is the generation shown's, by path
    /// otherwise; others, and deleted cards, are dropped.
    pub(super) fn semantic_rows(
        &mut self,
        indices: Vec<usize>,
        metadata: HashMap<usize, SemanticResultMetadata>,
    ) -> (Vec<usize>, HashMap<usize, SemanticResultMetadata>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return (self.filter_indices(indices), metadata);
        };
        let SemanticCorpus {
            loaded: Some(cards),
            docs,
            ..
        } = &mut self.semantic_corpus
        else {
            return (Vec::new(), HashMap::new());
        };
        let shown = snapshot.generation_id();
        if docs.as_ref().is_none_or(|(id, _)| *id != shown) {
            // Mapped off this thread (a read, a generation open, a mapping
            // job) before a query is sent; an answer computed for another
            // generation is mapped here (upstream mapped at every first
            // answer), and the scope sent goes with it.
            let mapped = if cards.generation_id() == shown {
                (0..cards.conversations().len() as u32).map(Some).collect()
            } else {
                super::cards::semantic_docs(&snapshot, cards.conversations())
            };
            *docs = Some((shown, Arc::new(mapped)));
            self.semantic_sent_scope_signature = None;
        }
        let doc_of = docs
            .as_ref()
            .map(|(_, docs)| docs.clone())
            .unwrap_or_default();
        let mut rows = Vec::with_capacity(indices.len());
        let mut by_row = HashMap::new();
        let mut metadata = metadata;
        for index in indices {
            let Some(doc) = doc_of.get(index).copied().flatten() else {
                continue;
            };
            if !self.cards.scope.get(doc as usize).copied().unwrap_or(false) {
                continue;
            }
            let row = self.card_row(doc);
            rows.push(row);
            if let Some(meta) = metadata.remove(&index) {
                by_row.insert(row, meta);
            }
        }
        (rows, by_row)
    }

    fn reset_semantic_worker(&mut self) {
        let worker = spawn_semantic_worker_thread(self.semantic_worker_config());
        self.semantic_search.worker_tx = Some(worker.tx);
        self.semantic_search.worker_rx = Some(worker.rx);
        self.semantic_search.cancellation = Some(worker.cancellation);
        // A replaced worker stops on its own once its channel is closed.
        self.semantic_search.worker_thread = Some(worker.thread);
        self.semantic_sent_corpus_version = 0;
        self.semantic_sent_scope_signature = None;
        // The replaced worker's indexing reports no longer arrive.
        self.semantic_search.indexing = None;
    }

    /// Stop the semantic worker for an exclusive semantic-cache operation
    /// (generate or clear the cache, K6): cancel its search, close its
    /// channel and **join** its thread, which drops its embedder and its
    /// copy of the embedding cache. Blocks until the worker's current step
    /// returns (embedding checks the cancellation between batches). Its
    /// background indexing stops with it; what it embedded since its last
    /// cache write (at most 30 s of work) is embedded again later.
    ///
    /// While suspended no worker is started: a semantic query still gets
    /// the lexical fallback rows, [`Browser::semantic_progress`] is
    /// [`SemanticProgress::Suspended`] and [`Browser::semantic_status_text`]
    /// is `"sem paused"`. No-op when already suspended.
    pub fn suspend_semantic(&mut self) {
        if self.semantic_search.suspended {
            return;
        }
        self.semantic_search.suspended = true;
        // The channel is closed before the cancel: an indexing slice the
        // worker starts in between is cancelled (it takes its token before
        // it looks at the channel).
        self.semantic_search.worker_tx = None;
        self.semantic_search.worker_rx = None;
        if let Some(cancellation) = self.semantic_search.cancellation.take() {
            cancellation.cancel();
        }
        if let Some(thread) = self.semantic_search.worker_thread.take() {
            let _ = thread.join();
        }
        self.semantic_sent_corpus_version = 0;
        self.semantic_sent_scope_signature = None;
        self.semantic_search.indexing = None;
        // No semantic answer will come; a lexical fallback that is still
        // running is applied when it arrives.
        if !self.search_in_flight {
            self.semantic_search.pending_generation = None;
        }
        self.semantic_search.pending_status = None;
        self.semantic_search.prewarm_generation = None;
        self.semantic_search.prewarm_status = None;
        self.semantic_search.last_status = SemanticProgress::Suspended;
    }

    /// End [`Browser::suspend_semantic`]. In semantic mode the current query
    /// is dispatched again (an empty query prewarms), which starts a new
    /// worker; it loads the embedding cache file as it is now (for example
    /// one generated meanwhile). No-op when not suspended.
    pub fn resume_semantic(&mut self) {
        if !self.semantic_search.suspended {
            return;
        }
        self.semantic_search.suspended = false;
        self.semantic_search.last_status = SemanticProgress::Idle;
        if self.list_search_mode != ListSearchMode::Semantic || self.is_loading() {
            return;
        }
        if self.query.trim().is_empty() {
            self.prewarm_semantic_cache();
        } else {
            self.dispatch_search();
        }
    }

    /// Whether the semantic worker is suspended.
    pub fn is_semantic_suspended(&self) -> bool {
        self.semantic_search.suspended
    }

    fn send_semantic_command(&mut self, command: SemanticWorkerCommand) -> bool {
        self.ensure_semantic_worker();
        self.semantic_search
            .worker_tx
            .as_ref()
            .is_some_and(|tx| tx.send(command).is_ok())
    }

    fn send_semantic_state(&mut self) -> Option<(u64, u64)> {
        let texts = self.semantic_texts();
        // Fork: the cards are sent again once their generation's index is
        // open (they may be read before it).
        if self.semantic_sent_corpus_version != self.semantic_corpus_version
            || (texts.is_some() && !self.semantic_corpus.sent_texts)
        {
            let sent_texts = texts.is_some();
            let conversations = self.semantic_conversations();
            if !self.send_semantic_command(SemanticWorkerCommand::UpdateCorpus {
                corpus_version: self.semantic_corpus_version,
                conversations,
                texts,
            }) {
                return None;
            }
            self.semantic_corpus.sent_texts = sent_texts;
            self.semantic_sent_corpus_version = self.semantic_corpus_version;
            self.semantic_sent_scope_signature = None;
        } else if self
            .semantic_sent_scope_signature
            .as_ref()
            .is_some_and(|(corpus_version, _)| *corpus_version == self.semantic_corpus_version)
        {
            return Some((self.semantic_corpus_version, self.semantic_scope_version));
        }

        self.ensure_scope();
        let scope = self.semantic_scope_indices();
        let current_signature = (self.semantic_corpus_version, scope.clone());
        let scope_changed =
            self.semantic_sent_scope_signature
                .as_ref()
                .is_none_or(|(corpus_version, previous)| {
                    *corpus_version != self.semantic_corpus_version
                        || (!Arc::ptr_eq(previous, &scope) && previous.as_ref() != scope.as_ref())
                });
        if scope_changed {
            self.semantic_scope_version += 1;
            if !self.send_semantic_command(SemanticWorkerCommand::UpdateScope {
                corpus_version: self.semantic_corpus_version,
                scope_version: self.semantic_scope_version,
                indices: scope,
            }) {
                return None;
            }
            self.semantic_sent_scope_signature = Some(current_signature);
        }

        Some((self.semantic_corpus_version, self.semantic_scope_version))
    }

    /// Run the current query (upstream `dispatch_search`):
    ///
    /// 1. an effectively empty query shows the browse list synchronously
    ///    (in semantic mode an in-flight prewarm is kept);
    /// 2. a session UUID jumps to that session synchronously;
    /// 3. semantic mode dispatches a semantic search plus a lexical fallback;
    /// 4. otherwise a lexical search is sent to the worker.
    pub(super) fn dispatch_search(&mut self) {
        let query = self.query.trim().to_string();
        self.uuid_lookup = None;

        if ParsedQuery::parse(&query).is_effectively_empty() {
            let prewarm_generation = self.semantic_search.prewarm_generation;
            let prewarm_status = self.semantic_search.prewarm_status.clone();
            self.invalidate_search_generation();
            if self.list_search_mode == ListSearchMode::Semantic {
                self.semantic_search.prewarm_generation = prewarm_generation;
                self.semantic_search.prewarm_status = prewarm_status;
            }
            self.semantic_search.error = None;
            self.semantic_search.results.clear();
            self.update_filter();
            return;
        }

        if search::is_uuid(&query) {
            self.invalidate_search_generation();
            self.semantic_search.error = None;
            self.apply_uuid_filter(&query);
            return;
        }

        if self.list_search_mode == ListSearchMode::Semantic {
            self.dispatch_semantic_search(query, false);
            return;
        }

        if self.snapshot.is_some() && !self.readiness.lexical {
            // The query runs once the generation's index opens.
            self.invalidate_search_generation();
            self.update_filter();
            return;
        }

        self.semantic_search.results.clear();
        self.semantic_search.error = None;
        self.dispatch_lexical_search(query);
    }

    /// Send a lexical search for `query` to the worker (a new generation).
    pub(super) fn dispatch_lexical_search(&mut self, query: String) {
        self.search_generation += 1;
        self.search_in_flight = true;
        self.search_page_shown = false;
        self.send_lexical_search(query, self.search_generation, self.list_search_mode);
    }

    /// Hand the lexical worker a query with the index in use and the docs in
    /// scope, else with the current corpus and search data (pointer copies
    /// of the snapshots).
    fn send_lexical_search(&self, query: String, generation: u64, mode: ListSearchMode) {
        // The lexical fallback of a semantic query searches everything.
        let user_only = self.searches_user_only();
        let corpus_version = self.semantic_corpus_version;
        let command = match &self.index_scope {
            Some(scope) => SearchCommand {
                query,
                generation,
                mode,
                conversations: Arc::default(),
                searchable: Arc::default(),
                index: Some(scope.clone()),
                user_only,
                corpus_version,
            },
            None => SearchCommand {
                query,
                generation,
                mode,
                conversations: self.conversations.clone(),
                searchable: self.searchable.clone(),
                index: None,
                user_only,
                corpus_version,
            },
        };
        let _ = self.search_tx.send(command);
    }

    fn dispatch_semantic_search(&mut self, query: String, prewarm: bool) {
        if !self.semantic_search.available {
            // Never reached through the public API (the browser cannot be in
            // semantic mode while semantic search is unavailable); never
            // spawn the worker (and load the model) in that case.
            self.invalidate_search_generation();
            self.semantic_search.error = Some("semantic search is unavailable".to_string());
            self.semantic_search.last_status = SemanticProgress::Failed;
            return;
        }
        if self.semantic_search.suspended {
            self.dispatch_while_suspended(query, prewarm);
            return;
        }
        if !self.ensure_semantic_corpus() {
            // Dispatched again once the corpus arrives.
            self.invalidate_search_generation();
            if self.semantic_corpus_failed() {
                self.semantic_search.error = Some("semantic corpus unavailable".to_string());
                self.semantic_search.last_status = SemanticProgress::Failed;
            } else {
                self.semantic_search.pending_status = Some(SemanticProgress::LoadingSessions);
            }
            return;
        }
        self.search_generation += 1;
        self.search_page_shown = false;
        self.search_in_flight = !prewarm;
        self.semantic_search.pending_generation = Some(self.search_generation);
        // Embedding activity remains visible while a replacement query waits for the worker.
        if !matches!(
            self.semantic_search.pending_status,
            Some(SemanticProgress::InitializingModel | SemanticProgress::Embedding { .. })
        ) {
            self.semantic_search.pending_status = None;
        }
        if prewarm {
            self.semantic_search.prewarm_generation = Some(self.search_generation);
            self.semantic_search.prewarm_status = None;
        }
        self.semantic_search.last_status = SemanticProgress::Idle;
        self.semantic_search.error = None;
        if prewarm {
            self.semantic_search.results.clear();
        }
        if let Some(cancellation) = &self.semantic_search.cancellation {
            cancellation.cancel();
        }
        let (corpus_version, scope_version) = match self.send_semantic_state() {
            Some(versions) => versions,
            None => {
                self.reset_semantic_worker();
                let Some(versions) = self.send_semantic_state() else {
                    self.semantic_search.error = Some("semantic worker unavailable".to_string());
                    self.semantic_search.pending_status = Some(SemanticProgress::Failed);
                    return;
                };
                versions
            }
        };
        let generation = self.search_generation;
        if !prewarm {
            self.send_lexical_search(query.clone(), generation, ListSearchMode::Semantic);
        }
        if !self.send_semantic_command(SemanticWorkerCommand::Search {
            generation,
            query: ParsedQuery::parse(&query),
            corpus_version,
            scope_version,
            prewarm,
        }) {
            self.reset_semantic_worker();
            let Some((corpus_version, scope_version)) = self.send_semantic_state() else {
                self.semantic_search.error = Some("semantic worker unavailable".to_string());
                self.semantic_search.pending_status = Some(SemanticProgress::Failed);
                return;
            };
            if !self.send_semantic_command(SemanticWorkerCommand::Search {
                generation,
                query: ParsedQuery::parse(&query),
                corpus_version,
                scope_version,
                prewarm,
            }) {
                self.semantic_search.error = Some("semantic worker unavailable".to_string());
                self.semantic_search.pending_status = Some(SemanticProgress::Failed);
            }
        }
    }

    /// A semantic dispatch while the worker is suspended: no worker; a
    /// query gets the lexical fallback (applied even when empty, since no
    /// semantic answer follows), a prewarm does nothing.
    fn dispatch_while_suspended(&mut self, query: String, prewarm: bool) {
        self.search_generation += 1;
        self.search_page_shown = false;
        self.semantic_search.error = None;
        self.semantic_search.pending_status = None;
        self.semantic_search.prewarm_generation = None;
        self.semantic_search.prewarm_status = None;
        self.semantic_search.last_status = SemanticProgress::Suspended;
        if prewarm {
            self.search_in_flight = false;
            self.semantic_search.pending_generation = None;
            self.semantic_search.results.clear();
            return;
        }
        self.search_in_flight = true;
        self.semantic_search.pending_generation = Some(self.search_generation);
        self.send_lexical_search(query, self.search_generation, ListSearchMode::Semantic);
    }

    /// In semantic mode with an empty query (and the semantic worker able
    /// to rank the loaded corpus), ask the semantic worker to load the model
    /// and embed the corpus in the background.
    pub(super) fn prewarm_semantic_cache(&mut self) {
        if self.semantic_search.available
            && self.list_search_mode == ListSearchMode::Semantic
            && self.query.trim().is_empty()
            && self.readiness.semantic
        {
            self.dispatch_semantic_search(String::new(), true);
        }
    }

    /// Apply finished worker results (upstream `receive_search_results`).
    /// Returns whether anything visible changed. Call it whenever the
    /// [`super::BrowserOptions::waker`] fires (or periodically while
    /// [`Browser::has_search_work_in_flight`]).
    pub fn poll(&mut self) -> bool {
        let mut applied = self.receive_semantic_corpus();
        applied |= self.receive_row_evidence();
        self.ensure_scope();
        while let Ok(response) = self.search_rx.try_recv() {
            let semantic_fallback_pending = response.mode == ListSearchMode::Semantic
                && self.semantic_search.pending_generation == Some(response.generation);
            // An answer from an index no longer in use (dropped or swapped)
            // does not apply.
            let index_current = match (&response.index, &self.search_index) {
                (None, _) => true,
                (Some(answered), Some(current)) => Arc::ptr_eq(answered, current),
                (Some(_), None) => false,
            };
            if response.generation == self.search_generation
                && response.mode == self.list_search_mode
                && response.user_only == self.searches_user_only()
                && (response.mode == ListSearchMode::Lexical || semantic_fallback_pending)
                && index_current
            {
                let (filtered, evidence) = if response.index.is_some() {
                    let filtered = self.rows_of_docs(&response.filtered);
                    (filtered, self.evidence_of_docs(response.evidence))
                } else {
                    (self.filter_indices(response.filtered), response.evidence)
                };
                let suspended = self.semantic_search.suspended;
                if response.mode == ListSearchMode::Semantic && filtered.is_empty() && !suspended {
                    self.search_in_flight = !response.complete;
                    continue;
                }
                self.lexical_evidence = evidence;
                if response.mode == ListSearchMode::Semantic {
                    self.semantic_search.results.clear();
                    if suspended && response.complete {
                        // No semantic answer follows while suspended.
                        self.semantic_search.pending_generation = None;
                    }
                }
                if self.search_in_flight
                    && !self.filtered.is_empty()
                    && self.selection_anchor.is_none()
                    && self.search_page_shown
                {
                    // The complete ranking extends the page already shown;
                    // keep what the user selected on it.
                    self.selection_anchor = self.selected_path();
                }
                self.apply_filtered(filtered);
                self.search_page_shown = !response.complete;
                self.search_in_flight = !response.complete;
                applied = true;
            }
        }
        if let Some(rx) = self.semantic_search.worker_rx.take() {
            let active_generation = self.search_generation;
            while let Ok(message) = rx.try_recv() {
                match message {
                    SemanticSearchMessage::Progress {
                        generation,
                        progress,
                    } => {
                        if self.list_search_mode == ListSearchMode::Semantic {
                            if Some(generation) == self.semantic_search.prewarm_generation {
                                self.semantic_search.prewarm_status = Some(progress.clone());
                                self.semantic_search.pending_status = Some(progress);
                                applied = true;
                            } else if generation == active_generation {
                                self.semantic_search.prewarm_generation = None;
                                self.semantic_search.prewarm_status = None;
                                self.semantic_search.pending_status = Some(progress);
                                applied = true;
                            }
                        }
                    }
                    SemanticSearchMessage::Indexing(indexing) => {
                        // A search waiting for indexing shows its progress.
                        if let (Some(SemanticProgress::Embedding { .. }), Some(indexing)) =
                            (&self.semantic_search.pending_status, indexing)
                        {
                            self.semantic_search.pending_status =
                                Some(SemanticProgress::Embedding {
                                    completed: indexing.completed,
                                    total: indexing.total,
                                });
                        }
                        self.semantic_search.indexing = indexing;
                        applied = true;
                    }
                    SemanticSearchMessage::Complete(response) => {
                        if self.list_search_mode == ListSearchMode::Semantic {
                            // CCR-3: the worker answers requests in order and
                            // drops a prewarm that a later request replaced
                            // before it ran, so an answer to a later
                            // generation ends the prewarm. Without a corpus
                            // to embed no progress message clears it first,
                            // and the prewarm stayed in flight forever.
                            if let Some(prewarm) = self.semantic_search.prewarm_generation
                                && response.generation > prewarm
                            {
                                self.semantic_search.prewarm_generation = None;
                                self.semantic_search.prewarm_status = None;
                                applied = true;
                            }
                            if response.prewarm
                                && Some(response.generation)
                                    == self.semantic_search.prewarm_generation
                            {
                                self.semantic_search.prewarm_generation = None;
                                self.semantic_search.prewarm_status = None;
                                if response.generation == active_generation {
                                    self.semantic_search.pending_generation = None;
                                    self.semantic_search.pending_status = None;
                                    self.semantic_search.last_status = response.progress;
                                    self.semantic_search.error = response.error;
                                    let (_, metadata) =
                                        self.semantic_rows(Vec::new(), response.metadata);
                                    self.semantic_search.results = metadata;
                                    if !self.query.trim().is_empty() {
                                        self.dispatch_semantic_search(
                                            self.query.trim().to_string(),
                                            false,
                                        );
                                    }
                                }
                                applied = true;
                            } else if response.generation == active_generation {
                                // Results refreshed after indexing replace ones
                                // already shown; keep a row the user moved to.
                                if self.semantic_search.pending_generation.is_none()
                                    && self.selection_anchor.is_none()
                                    && self.selected.is_some_and(|selected| selected > 0)
                                {
                                    self.selection_anchor = self.selected_path();
                                }
                                self.search_in_flight = false;
                                self.semantic_search.pending_generation = None;
                                self.semantic_search.pending_status = None;
                                self.semantic_search.last_status = response.progress;
                                self.semantic_search.error = response.error;
                                let (filtered, metadata) =
                                    self.semantic_rows(response.filtered, response.metadata);
                                self.semantic_search.results = metadata;
                                self.apply_filtered(filtered);
                                applied = true;
                            }
                        }
                    }
                }
            }
            self.semantic_search.worker_rx = Some(rx);
        }
        applied
    }

    /// Progress of the semantic worker's background indexing, while it runs
    /// (upstream c6d0421). It continues after the search or prewarm that
    /// started it was answered, and is not search work in flight.
    pub fn semantic_indexing(&self) -> Option<SemanticIndexing> {
        self.semantic_search.indexing
    }

    /// Whether a lexical search, a semantic search or a prewarm is running,
    /// or the semantic corpus is being read (also while an older one
    /// serves, as upstream) or mapped to a generation.
    pub fn has_search_work_in_flight(&self) -> bool {
        self.semantic_corpus.loading.is_some()
            || self.semantic_corpus.mapping.is_some()
            || self.search_in_flight
            || self.semantic_search.pending_generation.is_some()
            || self.semantic_search.prewarm_generation.is_some()
    }

    /// Whether semantic queries wait for the semantic corpus: it is being
    /// read or mapped, and none serves the generation shown yet.
    fn semantic_corpus_pending(&self) -> bool {
        (self.semantic_corpus.loading.is_some() || self.semantic_corpus.mapping.is_some())
            && self.semantic_docs_shown().is_none()
    }

    /// Semantic scores and evidence of a conversation in the current results.
    pub fn semantic_result_metadata(
        &self,
        conversation_index: usize,
    ) -> Option<&SemanticResultMetadata> {
        self.semantic_search.results.get(&conversation_index)
    }

    /// Lexical evidence (hidden-match context ranges) of a conversation in
    /// the current lexical results (computed for the first 200 hits).
    pub fn lexical_evidence(&self, conversation_index: usize) -> Option<&search::LexicalEvidence> {
        self.lexical_evidence.get(&conversation_index)
    }

    /// Semantic metadata of the selected row (semantic mode only; upstream's
    /// Ctrl+S details popup).
    pub fn semantic_result_metadata_for_selection(&self) -> Option<&SemanticResultMetadata> {
        if self.list_search_mode != ListSearchMode::Semantic {
            return None;
        }
        let selected = self.selected?;
        let conversation_index = *self.filtered.get(selected)?;
        self.semantic_result_metadata(conversation_index)
    }

    /// Search-bar suffix in semantic mode: `"sem failed"`, `"sem no text"`,
    /// or `"sem paused"` while the worker is suspended.
    pub fn semantic_status_text(&self) -> Option<String> {
        if self.list_search_mode != ListSearchMode::Semantic {
            return None;
        }
        if self.semantic_search.suspended {
            return Some("sem paused".to_string());
        }
        if self.semantic_search.error.is_some() {
            return Some("sem failed".to_string());
        }
        let status = self
            .semantic_search
            .pending_status
            .as_ref()
            .unwrap_or(&self.semantic_search.last_status);
        match status {
            SemanticProgress::EmptyCorpus => Some("sem no text".to_string()),
            SemanticProgress::Failed => Some("sem failed".to_string()),
            _ => None,
        }
    }

    /// Activity text while the conversations load, the model loads, a
    /// search waits for embeddings or the worker embeds the corpus in the
    /// background: `"sem loading conversations"`, `"sem preparing
    /// embeddings"`, `"sem embedding <p>%  <d>/<t> chunks"` or `"sem indexing
    /// <p>%  <d>/<t> chunks"` (upstream c6d0421).
    pub fn semantic_activity_status_text(&self) -> Option<String> {
        let percent = |completed: usize, total: usize| {
            completed
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(100)
        };
        Some(match self.semantic_activity()? {
            SemanticActivity::Loading => "sem loading conversations".to_string(),
            SemanticActivity::Preparing => "sem preparing embeddings".to_string(),
            SemanticActivity::Embedding { completed, total } => format!(
                "sem embedding {}%  {completed}/{total} chunks",
                percent(completed, total)
            ),
            SemanticActivity::Indexing { completed, total } => format!(
                "sem indexing {}%  {completed}/{total} chunks",
                percent(completed, total)
            ),
        })
    }

    /// The `(done, total)` chunk counts that
    /// [`Browser::semantic_activity_status_text`] shows, when it shows any
    /// (`sem embedding` or `sem indexing`), for a progress bar.
    pub fn semantic_activity_counts(&self) -> Option<(usize, usize)> {
        match self.semantic_activity()? {
            SemanticActivity::Embedding { completed, total }
            | SemanticActivity::Indexing { completed, total } => Some((completed, total)),
            SemanticActivity::Loading | SemanticActivity::Preparing => None,
        }
    }

    /// What the semantic activity shows: the prewarm's or the pending
    /// search's status, else the semantic corpus load, else the background
    /// indexing (upstream c6d0421; done counts capped at the total).
    fn semantic_activity(&self) -> Option<SemanticActivity> {
        if self.list_search_mode != ListSearchMode::Semantic || self.semantic_search.error.is_some()
        {
            return None;
        }
        // A query or scope change clears the pending status while the
        // corpus loads: the load still shows.
        let loading = SemanticProgress::LoadingSessions;
        let status = self
            .semantic_search
            .prewarm_status
            .as_ref()
            .or(self.semantic_search.pending_status.as_ref())
            .or(self.semantic_corpus_pending().then_some(&loading));
        let Some(status) = status else {
            let SemanticIndexing { completed, total } = self.semantic_search.indexing?;
            return Some(SemanticActivity::Indexing {
                completed: completed.min(total),
                total,
            });
        };
        match status {
            SemanticProgress::LoadingSessions => Some(SemanticActivity::Loading),
            SemanticProgress::InitializingModel => Some(SemanticActivity::Preparing),
            SemanticProgress::Embedding { completed, total } => Some(SemanticActivity::Embedding {
                completed: *completed.min(total),
                total: *total,
            }),
            _ => None,
        }
    }

    /// The semantic error message, if the last semantic search failed
    /// (upstream only showed "sem failed"; quirk 8).
    pub fn semantic_error(&self) -> Option<&str> {
        if self.list_search_mode != ListSearchMode::Semantic {
            return None;
        }
        self.semantic_search.error.as_deref()
    }

    /// The most recent semantic progress (pending, else last completed).
    pub fn semantic_progress(&self) -> &SemanticProgress {
        self.semantic_search
            .prewarm_status
            .as_ref()
            .or(self.semantic_search.pending_status.as_ref())
            .unwrap_or(&self.semantic_search.last_status)
    }

    /// Replace the query. Like any query edit in the TUI: clears the
    /// selection anchor and searches immediately, except before there are
    /// rows (the query is applied when loading finishes or the first
    /// generation is shown). In generation mode a lexical query waits for
    /// the generation's index ([`super::Readiness::lexical`]); the cards
    /// stay listed meanwhile. Unchanged text is a no-op.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        if query == self.query {
            return;
        }
        self.query = query;
        self.clear_selection_anchor();
        if !self.is_loading()
            || (self.readiness.lexical && self.list_search_mode == ListSearchMode::Lexical)
        {
            self.dispatch_search();
        }
    }

    /// Esc in the list: clear a non-empty query (and show the browse list),
    /// or report [`super::EscapeOutcome::Quit`] when it is already empty.
    pub fn escape(&mut self) -> super::EscapeOutcome {
        if self.query.is_empty() {
            return super::EscapeOutcome::Quit;
        }
        self.query.clear();
        self.dispatch_search();
        super::EscapeOutcome::Cleared
    }

    /// Toggle the workspace ("project") scope. No-op without a workspace.
    pub fn toggle_workspace_filter(&mut self) {
        if self.workspace.is_some() {
            self.workspace_filter = !self.workspace_filter;
            self.scope_changed();
        }
    }

    /// Turn the workspace scope on or off (no-op without a workspace or
    /// when unchanged).
    pub fn set_workspace_filter(&mut self, enabled: bool) {
        if self.workspace.is_some() && self.workspace_filter != enabled {
            self.workspace_filter = enabled;
            self.scope_changed();
        }
    }

    /// Change the workspace (the GUI's replacement for the launch cwd).
    /// Without a workspace the scope is turned off. The rows are re-filtered
    /// only when the scope is on.
    pub fn set_workspace(&mut self, workspace: Option<crate::env::WorkspaceScope>) {
        if self.single_file_mode || self.workspace == workspace {
            return;
        }
        let scope_was_active = self.workspace_filter;
        if workspace.is_none() {
            self.workspace_filter = false;
        }
        self.workspace = workspace;
        if scope_was_active {
            self.scope_changed();
        }
    }

    /// Change the excluded project names (`[tui].exclude_projects`).
    pub fn set_excluded_projects(&mut self, projects: Vec<String>) {
        let projects = projects.into_iter().collect();
        if self.single_file_mode || self.excluded_projects == projects {
            return;
        }
        self.excluded_projects = projects;
        self.scope_changed();
    }

    /// Change the excluded working-directory prefixes (`[tui].exclude_paths`,
    /// upstream 982aea9; fork: changeable at runtime, like
    /// [`Browser::set_excluded_projects`]). No-op in single-file mode or when
    /// unchanged.
    pub fn set_excluded_paths(&mut self, prefixes: Vec<String>) {
        if self.single_file_mode || self.excluded_paths == prefixes {
            return;
        }
        self.excluded_paths = prefixes;
        self.scope_changed();
    }

    /// List or hide automated (Codex exec) and child (subagent)
    /// conversations (upstream `toggle_all_kinds`, Ctrl+G), keeping the query
    /// and the selected session. They are lexical only: indexing gives them
    /// no semantic turns, so semantic search does not find them. No-op in
    /// single-file mode.
    pub fn toggle_all_kinds(&mut self) {
        self.set_all_kinds(!self.all_kinds);
    }

    /// Turn [`Browser::toggle_all_kinds`] on or off (no-op when unchanged).
    pub fn set_all_kinds(&mut self, all_kinds: bool) {
        if self.single_file_mode || self.all_kinds == all_kinds {
            return;
        }
        self.all_kinds = all_kinds;
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.selected_path();
        }
        self.scope_changed();
    }

    /// Search only the text the person typed, or everything again (upstream
    /// `toggle_user_only`, Ctrl+V), keeping the query and the selected
    /// session. Lexical only ([`Browser::searches_user_only`]). No-op in
    /// single-file mode.
    pub fn toggle_user_only(&mut self) {
        self.set_user_only(!self.user_only);
    }

    /// Turn [`Browser::toggle_user_only`] on or off (no-op when unchanged).
    pub fn set_user_only(&mut self, user_only: bool) {
        if self.single_file_mode || self.user_only == user_only {
            return;
        }
        self.user_only = user_only;
        if self.list_search_mode == ListSearchMode::Semantic {
            // Semantic search ranks whole conversations; see
            // `searches_user_only`.
            return;
        }
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.selected_path();
        }
        self.invalidate_search_generation();
        if self.query.trim().is_empty() || !self.readiness.lexical {
            self.update_filter();
        } else {
            self.dispatch_search();
        }
    }

    /// Whether user-only search is on ([`Browser::toggle_user_only`]),
    /// whatever the search mode.
    pub fn user_only(&self) -> bool {
        self.user_only
    }

    /// Whether queries match only the text the person typed: user-only is
    /// on and the mode is lexical. Semantic search embeds chunks that mix
    /// both sides of the conversation, so it ignores the toggle.
    pub fn searches_user_only(&self) -> bool {
        self.user_only && self.list_search_mode == ListSearchMode::Lexical
    }

    /// Re-run the current query after the scope or exclusions changed
    /// (upstream `toggle_workspace_filter`).
    fn scope_changed(&mut self) {
        self.semantic_sent_scope_signature = None;
        self.invalidate_search_generation();
        if self.snapshot.is_some() {
            // The cards' scope is decided again; the query runs again.
            self.rescope_cards();
            return;
        }
        // The one O(n) pass of a scope change: the rows in scope.
        self.ensure_scope();
        if self.is_loading() {
            // Quirk 14 fix: the search index does not exist yet.
            self.apply_scope_only_filter();
            return;
        }
        // The docs an index query may return follow the scope.
        self.send_index_scope();
        if self.list_search_mode == ListSearchMode::Semantic && !self.query.trim().is_empty() {
            self.dispatch_search();
        } else {
            self.update_filter();
        }
    }

    /// While loading: every loaded row that passes the exclusions and the
    /// scope, in arrival order.
    fn apply_scope_only_filter(&mut self) {
        let filtered = self.scope_rows();
        self.apply_filtered(filtered);
    }

    /// Switch between lexical and semantic search. No-op when semantic
    /// search is unavailable.
    pub fn toggle_list_search_mode(&mut self) {
        if !self.semantic_search.available {
            return;
        }
        self.list_search_mode = self.list_search_mode.toggled();
        if self.list_search_mode == ListSearchMode::Semantic {
            // Entering semantic mode retries semantic data that could not
            // be read.
            self.semantic_corpus.failed = None;
        }
        self.invalidate_search_generation();
        if self.is_loading() {
            // Quirk 14 fix: the mode is applied when loading finishes.
            self.apply_scope_only_filter();
            return;
        }
        if self.list_search_mode == ListSearchMode::Semantic && self.query.trim().is_empty() {
            self.ensure_semantic_corpus();
            self.prewarm_semantic_cache();
        } else {
            self.dispatch_search();
        }
    }

    /// Set the search mode (no-op when unchanged or unavailable).
    pub fn set_list_search_mode(&mut self, mode: ListSearchMode) {
        if self.list_search_mode != mode {
            self.toggle_list_search_mode();
        }
    }

    /// The UUID jump: a loaded session with this id (ignoring ASCII case)
    /// becomes the only row ([`UuidLookup::Found`]). Otherwise the lookup is
    /// [`UuidLookup::Pending`] and the rows stay: this never touches the
    /// disk. The GUI then loads the session in the background
    /// ([`crate::history::load_session_by_uuid`]) and hands the result to
    /// [`Browser::resolve_uuid_lookup_session`]. Returns whether it was found.
    pub(super) fn apply_uuid_filter(&mut self, query: &str) -> bool {
        if let Some(index) = self.find_loaded_uuid(query) {
            self.show_uuid_row(index, false);
            true
        } else {
            self.uuid_lookup = Some(UuidLookup::Pending {
                uuid: query.to_string(),
            });
            false
        }
    }

    fn show_uuid_row(&mut self, conversation_index: usize, loaded_from_disk: bool) {
        self.filtered = vec![conversation_index];
        self.selected = Some(0);
        self.uuid_lookup = Some(UuidLookup::Found {
            conversation_index,
            loaded_from_disk,
        });
    }

    /// Index of the first loaded session with this id (ignoring ASCII
    /// case): an indexed lookup, no scan of the corpus (X-038 UUID paste).
    /// While only the first cards of a generation are shown, a card beyond
    /// them is found in the generation (a scan of its rows) and
    /// materialized.
    fn find_loaded_uuid(&mut self, uuid: &str) -> Option<usize> {
        if let Some(index) = self.index.first_session(&self.conversations, uuid) {
            return Some(index);
        }
        if self.generation_open() {
            return None;
        }
        let doc = self.snapshot.as_ref()?.find_session(uuid)?;
        Some(self.card_row(doc))
    }

    /// Finish a [`UuidLookup::Pending`] jump with the result of
    /// [`crate::history::load_session_by_uuid`] (ADD C1 semantics: the
    /// loaded session bypasses exclusions, the scope and the time filter,
    /// and is appended at the end of the corpus).
    ///
    /// Applies only while the lookup is still pending for `uuid` (the query
    /// is still that UUID); a stale result is ignored. `Some` appends the
    /// session (one element, with its search data) and shows it as the only
    /// row ([`UuidLookup::Found`] with `loaded_from_disk`); `None` reports
    /// [`UuidLookup::NotFound`] and keeps the rows (quirk 6). A session that
    /// was loaded meanwhile (a refresh) is shown without appending a copy.
    ///
    /// Synchronous convenience: it normalizes the session's text on the
    /// caller's thread ([`PreparedSession::new`]); a GUI prepares it in the
    /// background and calls [`Browser::resolve_uuid_lookup_session`].
    pub fn resolve_uuid_lookup(&mut self, uuid: &str, loaded: Option<Conversation>) {
        self.resolve_uuid_lookup_session(uuid, loaded.map(PreparedSession::new));
    }

    /// [`Browser::resolve_uuid_lookup`] for a session prepared off this
    /// thread: no text is normalized here.
    pub fn resolve_uuid_lookup_session(&mut self, uuid: &str, loaded: Option<PreparedSession>) {
        let pending = matches!(
            &self.uuid_lookup,
            Some(UuidLookup::Pending { uuid: pending }) if pending == uuid
        );
        if !pending || self.query.trim() != uuid {
            return;
        }
        if let Some(index) = self.find_loaded_uuid(uuid) {
            self.show_uuid_row(index, false);
            return;
        }
        match loaded {
            Some(conversation) => {
                let index = self.push_conversation(conversation);
                self.corpus_changed();
                self.show_uuid_row(index, true);
            }
            None => {
                self.uuid_lookup = Some(UuidLookup::NotFound {
                    uuid: uuid.to_string(),
                });
            }
        }
    }

    /// Re-apply the query to the current corpus (upstream `update_filter`).
    ///
    /// An empty query (browse) and the UUID jump apply synchronously (both
    /// are cheap). A lexical query is sent to the search worker; until it
    /// answers, the current rows stay, re-filtered by the exclusions and the
    /// scope (upstream searched synchronously on the UI thread). In semantic
    /// mode a non-empty query is left to the semantic worker.
    pub(super) fn update_filter(&mut self) {
        let query = self.query.trim().to_string();
        self.uuid_lookup = None;

        if ParsedQuery::parse(&query).is_effectively_empty() || !self.readiness.lexical {
            self.semantic_search.error = None;
            self.apply_browse_filter();
            return;
        }

        if search::is_uuid(&query) {
            self.semantic_search.error = None;
            self.apply_uuid_filter(&query);
            return;
        }

        if self.list_search_mode == ListSearchMode::Semantic {
            return;
        }

        self.semantic_search.error = None;
        self.semantic_search.results.clear();
        self.dispatch_lexical_search(query);
        self.keep_current_rows();
    }

    /// The browse list: every conversation that passes the exclusions and
    /// the scope, in corpus order (newest first once loaded); before a
    /// generation is open, its newest cards.
    fn apply_browse_filter(&mut self) {
        if self.snapshot.is_some() && !self.generation_open() {
            self.show_snapshot_cards();
            return;
        }
        let filtered = self.scope_rows();
        self.apply_filtered(filtered);
    }

    /// While a lexical search runs: the current rows that still pass the
    /// exclusions and the scope; the selection anchor's row (kept for the
    /// results) or the first row is selected.
    fn keep_current_rows(&mut self) {
        let rows = std::mem::take(&mut self.filtered);
        self.ensure_scope();
        self.filtered = self.filter_indices(rows);
        self.selected = self
            .selection_anchor
            .as_deref()
            .and_then(|anchor| self.row_of_path(&self.filtered, anchor))
            .or_else(|| (!self.filtered.is_empty()).then_some(0));
    }
}
