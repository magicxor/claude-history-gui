//! Query dispatch, the lexical worker, and semantic orchestration (upstream
//! `tui/app/search_state.rs`).

use super::corpus::{CorpusSnapshot, SearchSnapshot};
use super::{Browser, ListSearchMode, SemanticProgress, SemanticResultMetadata, UuidLookup};
use crate::history::Conversation;
use crate::search;
use crate::search::query::ParsedQuery;
use crate::semantic::types::SemanticCancellationToken;
use crate::semantic_worker::{
    SemanticSearchMessage, SemanticWorkerCommand, SemanticWorkerConfig, Waker,
    spawn_semantic_worker_thread,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;

#[derive(Default)]
pub(super) struct SemanticSearchState {
    pub(super) available: bool,
    pub(super) pending_generation: Option<u64>,
    pub(super) pending_status: Option<SemanticProgress>,
    pub(super) prewarm_generation: Option<u64>,
    pub(super) prewarm_status: Option<SemanticProgress>,
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

/// A query for the lexical worker, with the browser's corpus and search data
/// at the time of the query (shared, not copied). The worker holds them only
/// while it searches and drops them before it answers: an idle worker pins
/// no snapshot, so a rename or a delete edits the browser's vectors in place
/// instead of copying their pointers (CCR-2).
pub(super) struct SearchCommand {
    pub(super) query: String,
    pub(super) generation: u64,
    pub(super) mode: ListSearchMode,
    pub(super) conversations: CorpusSnapshot,
    pub(super) searchable: SearchSnapshot,
}

pub(super) struct SearchResponse {
    pub(super) filtered: Vec<usize>,
    pub(super) generation: u64,
    pub(super) mode: ListSearchMode,
    pub(super) evidence: HashMap<usize, search::LexicalEvidence>,
}

fn build_search_evidence(
    conversations: &[Arc<Conversation>],
    filtered: &[usize],
    parsed: &ParsedQuery,
) -> HashMap<usize, search::LexicalEvidence> {
    const MAX_EVIDENCE_ROWS: usize = 200;

    filtered
        .iter()
        .take(MAX_EVIDENCE_ROWS)
        .filter_map(|&index| {
            search::build_lexical_evidence(&conversations[index], parsed)
                .map(|evidence| (index, evidence))
        })
        .collect()
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

/// The lexical search worker: one thread with a one-thread rayon pool. It
/// drains every queued command and runs only the latest query (coalescing),
/// against the snapshots that query carries, which it releases before it
/// answers. `waker` is called after each response.
pub(super) fn spawn_search_worker(
    waker: Option<Waker>,
) -> (mpsc::Sender<SearchCommand>, mpsc::Receiver<SearchResponse>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<SearchCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SearchResponse>();

    std::thread::Builder::new()
        .name("search-worker".into())
        .spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|_| "lexical-rank-worker".to_string())
                .build()
                .expect("failed to create lexical ranking thread pool");
            pool.install(move || {
                while let Ok(first) = cmd_rx.recv() {
                    let command = latest_command(first, &cmd_rx);
                    let SearchCommand {
                        query,
                        generation,
                        mode,
                        conversations,
                        searchable,
                    } = command;

                    let now = chrono::Local::now();
                    let filtered = search::search(&conversations, &searchable, &query, now);
                    let parsed = ParsedQuery::parse(&query);
                    let evidence = build_search_evidence(&conversations, &filtered, &parsed);
                    // Release the snapshots before answering: once the
                    // browser sees the answer, an edit finds its vectors
                    // unshared (when no other query is queued).
                    drop(searchable);
                    drop(conversations);

                    let sent = res_tx
                        .send(SearchResponse {
                            filtered,
                            generation,
                            mode,
                            evidence,
                        })
                        .is_ok();
                    if sent && let Some(waker) = &waker {
                        waker();
                    }
                }
            });
        })
        .expect("failed to spawn search worker thread");

    (cmd_tx, res_rx)
}

impl Browser {
    pub(super) fn invalidate_search_generation(&mut self) {
        self.search_generation += 1;
        self.search_in_flight = false;
        self.lexical_evidence.clear();
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
    /// no copy) when current.
    pub(super) fn semantic_scope_indices(&self) -> Arc<Vec<usize>> {
        if self.scope_is_current() {
            return self.index.scope.rows().clone();
        }
        Arc::new(self.filter_indices(0..self.conversations.len()))
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
    }

    /// Stop the semantic worker for an exclusive semantic-cache operation
    /// (generate or clear the cache, K6): cancel its search, close its
    /// channel and **join** its thread, which drops its embedder and its
    /// copy of the embedding cache. Blocks until the worker's current step
    /// returns (embedding checks the cancellation between batches).
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
        if let Some(cancellation) = self.semantic_search.cancellation.take() {
            cancellation.cancel();
        }
        self.semantic_search.worker_tx = None;
        self.semantic_search.worker_rx = None;
        if let Some(thread) = self.semantic_search.worker_thread.take() {
            let _ = thread.join();
        }
        self.semantic_sent_corpus_version = 0;
        self.semantic_sent_scope_signature = None;
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
        if self.semantic_sent_corpus_version != self.semantic_corpus_version {
            if !self.send_semantic_command(SemanticWorkerCommand::UpdateCorpus {
                corpus_version: self.semantic_corpus_version,
                conversations: self.conversations.clone(),
            }) {
                return None;
            }
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

        self.semantic_search.results.clear();
        self.semantic_search.error = None;
        self.dispatch_lexical_search(query);
    }

    /// Send a lexical search for `query` to the worker (a new generation).
    pub(super) fn dispatch_lexical_search(&mut self, query: String) {
        self.search_generation += 1;
        self.search_in_flight = true;
        self.send_lexical_search(query, self.search_generation, self.list_search_mode);
    }

    /// Hand the lexical worker a query with the current corpus and search
    /// data (pointer copies of the snapshots).
    fn send_lexical_search(&self, query: String, generation: u64, mode: ListSearchMode) {
        let _ = self.search_tx.send(SearchCommand {
            query,
            generation,
            mode,
            conversations: self.conversations.clone(),
            searchable: self.searchable.clone(),
        });
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
        self.search_generation += 1;
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

    /// In semantic mode with an empty query (and not loading), ask the
    /// semantic worker to load the model and embed the corpus in the
    /// background.
    pub(super) fn prewarm_semantic_cache(&mut self) {
        if self.semantic_search.available
            && self.list_search_mode == ListSearchMode::Semantic
            && self.query.trim().is_empty()
            && !self.is_loading()
        {
            self.dispatch_semantic_search(String::new(), true);
        }
    }

    /// Apply finished worker results (upstream `receive_search_results`).
    /// Returns whether anything visible changed. Call it whenever the
    /// [`super::BrowserOptions::waker`] fires (or periodically while
    /// [`Browser::has_search_work_in_flight`]).
    pub fn poll(&mut self) -> bool {
        let mut applied = false;
        self.ensure_scope();
        while let Ok(response) = self.search_rx.try_recv() {
            let semantic_fallback_pending = response.mode == ListSearchMode::Semantic
                && self.semantic_search.pending_generation == Some(response.generation);
            if response.generation == self.search_generation
                && response.mode == self.list_search_mode
                && (response.mode == ListSearchMode::Lexical || semantic_fallback_pending)
            {
                let filtered = self.filter_indices(response.filtered);
                let suspended = self.semantic_search.suspended;
                if response.mode == ListSearchMode::Semantic && filtered.is_empty() && !suspended {
                    self.search_in_flight = false;
                    continue;
                }
                self.lexical_evidence = response.evidence;
                if response.mode == ListSearchMode::Semantic {
                    self.semantic_search.results.clear();
                    if suspended {
                        // No semantic answer follows while suspended.
                        self.semantic_search.pending_generation = None;
                    }
                }
                self.apply_filtered(filtered);
                self.search_in_flight = false;
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
                                    self.semantic_search.results = response.metadata;
                                    if !self.query.trim().is_empty() {
                                        self.dispatch_semantic_search(
                                            self.query.trim().to_string(),
                                            false,
                                        );
                                    }
                                }
                                applied = true;
                            } else if response.generation == active_generation {
                                self.search_in_flight = false;
                                self.semantic_search.pending_generation = None;
                                self.semantic_search.pending_status = None;
                                self.semantic_search.last_status = response.progress;
                                self.semantic_search.error = response.error;
                                self.semantic_search.results = response.metadata;
                                let filtered = self.filter_indices(response.filtered);
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

    /// Whether a lexical search, a semantic search or a prewarm is running.
    pub fn has_search_work_in_flight(&self) -> bool {
        self.search_in_flight
            || self.semantic_search.pending_generation.is_some()
            || self.semantic_search.prewarm_generation.is_some()
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

    /// Activity text while the model loads or the corpus is embedded:
    /// `"sem preparing embeddings"` or `"sem embedding <p>%  <d>/<t> chunks"`.
    pub fn semantic_activity_status_text(&self) -> Option<String> {
        if self.list_search_mode != ListSearchMode::Semantic || self.semantic_search.error.is_some()
        {
            return None;
        }
        let status = self
            .semantic_search
            .prewarm_status
            .as_ref()
            .or(self.semantic_search.pending_status.as_ref())?;
        match status {
            SemanticProgress::InitializingModel => Some("sem preparing embeddings".to_string()),
            SemanticProgress::Embedding { completed, total } => {
                let percent = if *total == 0 {
                    100
                } else {
                    completed.min(total).saturating_mul(100) / total
                };
                Some(format!(
                    "sem embedding {percent}%  {}/{total} chunks",
                    completed.min(total)
                ))
            }
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
    /// selection anchor and searches immediately, except while loading (the
    /// query is applied when loading finishes). Unchanged text is a no-op.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        if query == self.query {
            return;
        }
        self.query = query;
        self.clear_selection_anchor();
        if !self.is_loading() {
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

    /// Re-run the current query after the scope or exclusions changed
    /// (upstream `toggle_workspace_filter`).
    fn scope_changed(&mut self) {
        self.semantic_sent_scope_signature = None;
        self.invalidate_search_generation();
        // The one O(n) pass of a scope change: the rows in scope.
        self.ensure_scope();
        if self.is_loading() {
            // Quirk 14 fix: the search index does not exist yet.
            self.apply_scope_only_filter();
            return;
        }
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
        self.invalidate_search_generation();
        if self.is_loading() {
            // Quirk 14 fix: the mode is applied when loading finishes.
            self.apply_scope_only_filter();
            return;
        }
        if self.list_search_mode == ListSearchMode::Semantic && self.query.trim().is_empty() {
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
    /// [`Browser::resolve_uuid_lookup`]. Returns whether it was found.
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
    fn find_loaded_uuid(&self, uuid: &str) -> Option<usize> {
        self.index.first_session(&self.conversations, uuid)
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
    pub fn resolve_uuid_lookup(&mut self, uuid: &str, loaded: Option<Conversation>) {
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

        if ParsedQuery::parse(&query).is_effectively_empty() {
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
    /// the scope, in corpus order (newest first once loaded).
    fn apply_browse_filter(&mut self) {
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
