//! Corpus storage, preparation and incremental edits (K1, CCR-1, CCR-2).
//!
//! The corpus is an `Arc<Vec<Arc<Conversation>>>` ([`CorpusSnapshot`])
//! shared with the lexical and semantic workers, and its search data an
//! `Arc<Vec<Arc<SearchableConversation>>>` shared with the lexical worker.
//! Changing one conversation copies at most the vector of pointers
//! (`Arc::make_mut` when a worker still holds the previous version).
//! Positions by path and session id, the source mix and the rows in scope
//! ([`super::index::CorpusIndex`]) are kept in step, so finding the row of
//! a path or a session id hashes one key instead of scanning the corpus.
//!
//! The O(corpus) part of a load or refresh (sort, dedupe by canonical path,
//! search data, the lookup data, the rows in scope and, for a refresh, where
//! each current row moves) is [`Browser::prepare_corpus`], an associated
//! function that runs on any thread; [`Browser::install_prepared`] swaps the
//! result in with pointer-sized work only.
//!
//! Rows deleted or renamed through the browser while a
//! [`super::HistoryLoader`] runs are replayed on the corpus it installs: the
//! browser stamps every such edit with [`corpus_edit_mark`]'s clock, the
//! loader takes a mark when it starts reading, and the install replays the
//! edits stamped from that mark on (made while the loader read the files,
//! buffered them or prepared them).

use super::index::{CorpusIndex, ScopeSettings, remove_positions_in_place, without_positions};
use super::{Browser, ListSearchMode, LoadingState, UuidLookup};
use crate::history::Conversation;
use crate::search::query::ParsedQuery;
use crate::search::{self, SearchableConversation};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The browser's corpus, shared with its search workers: every conversation
/// exists once. Cloning a snapshot is cheap and it never changes; the
/// browser replaces its own snapshot when the corpus changes.
pub type CorpusSnapshot = Arc<Vec<Arc<Conversation>>>;

/// The lexical search data, parallel to the corpus.
pub(super) type SearchSnapshot = Arc<Vec<Arc<SearchableConversation>>>;

/// A corpus sorted newest first, deduplicated when asked, reindexed, with
/// its search data and lookup data built: the result of
/// [`Browser::prepare_corpus`], to be passed to
/// [`Browser::install_prepared`]. Opaque and `Send`, so it can be built on a
/// worker thread.
pub struct PreparedCorpus {
    conversations: Vec<Arc<Conversation>>,
    searchable: Vec<Arc<SearchableConversation>>,
    pub(super) index: CorpusIndex,
    /// Every conversation's `index` is its position (none was shared with
    /// the loading rows when it was prepared, and no edit moved it since).
    indexed: bool,
    /// Where each row of the corpus being replaced moved (a refresh
    /// prepared by a [`super::HistoryLoader`]).
    pub(super) remap: Option<Remap>,
}

/// The positions in a prepared corpus of the conversations of the corpus it
/// replaces, by path.
pub(super) struct Remap {
    /// The corpus being replaced, as it was when the preparation started.
    previous: CorpusSnapshot,
    /// For each position in `previous`, the position of the same path in
    /// the prepared corpus.
    positions: Vec<Option<usize>>,
}

/// What a preparation for a particular browser knows besides the
/// conversations: the scope to compute and, for a refresh, the corpus
/// whose rows must be mapped to the new one.
pub(super) struct PrepareContext {
    scope: ScopeSettings,
    previous: Option<CorpusSnapshot>,
}

impl PreparedCorpus {
    /// Number of conversations.
    pub fn len(&self) -> usize {
        self.conversations.len()
    }

    /// Whether the prepared corpus is empty.
    pub fn is_empty(&self) -> bool {
        self.conversations.is_empty()
    }

    /// Replace the conversation at `position` (same path), with its search
    /// data and its `index`.
    fn replace(&mut self, position: usize, mut conversation: Arc<Conversation>) {
        if conversation.index != position {
            Arc::make_mut(&mut conversation).index = position;
        }
        let previous = std::mem::replace(&mut self.conversations[position], conversation);
        self.searchable[position] = Arc::new(SearchableConversation::new(
            &self.conversations[position],
            position,
        ));
        self.index.replace(&self.conversations, position, &previous);
    }

    /// Append a conversation (a carried UUID result); `previous_position`
    /// is its position in the corpus being replaced.
    fn push(&mut self, mut conversation: Arc<Conversation>, previous_position: usize) {
        let position = self.conversations.len();
        if conversation.index != position {
            Arc::make_mut(&mut conversation).index = position;
        }
        self.searchable.push(Arc::new(SearchableConversation::new(
            &conversation,
            position,
        )));
        self.conversations.push(conversation);
        self.index.extend(&self.conversations, position);
        if let Some(slot) = self
            .remap
            .as_mut()
            .and_then(|remap| remap.positions.get_mut(previous_position))
        {
            *slot = Some(position);
        }
    }

    /// Remove the conversations at `removed` (ascending positions).
    fn remove_positions(&mut self, removed: &[usize]) {
        if removed.is_empty() {
            return;
        }
        let old_len = self.conversations.len();
        let gone = removed
            .iter()
            .map(|&position| self.conversations[position].clone())
            .collect::<Vec<_>>();
        remove_positions_in_place(&mut self.conversations, removed);
        remove_positions_in_place(&mut self.searchable, removed);
        self.index.remove(removed, &gone, old_len);
        if let Some(remap) = &mut self.remap {
            for slot in &mut remap.positions {
                if let Some(position) = *slot {
                    *slot = removed
                        .binary_search(&position)
                        .is_err()
                        .then(|| position - removed.partition_point(|&gone| gone < position));
                }
            }
        }
        // The later conversations moved: their `index` is set by the install.
        self.indexed = false;
    }
}

impl std::fmt::Debug for PreparedCorpus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedCorpus")
            .field("conversations", &self.conversations.len())
            .finish()
    }
}

/// A row edit to replay on a corpus that was being loaded meanwhile.
#[derive(Debug)]
pub(super) enum CorpusEdit {
    /// The conversation at this path left the corpus (deleted).
    Removed(PathBuf),
    /// The conversation at this path was replaced (renamed); the install
    /// takes the browser's current version.
    Replaced(PathBuf),
}

/// Clock of the corpus edits of every browser (a counter, not a time).
static CORPUS_EDIT_CLOCK: AtomicU64 = AtomicU64::new(0);

/// The current corpus edit clock: edits made from now on are stamped with a
/// value at least this. A loader takes it before it starts reading.
pub(super) fn corpus_edit_mark() -> u64 {
    CORPUS_EDIT_CLOCK.load(Ordering::SeqCst)
}

/// Edits kept while no load is tracked: enough for the edits between a
/// load's start and its first [`super::HistoryLoader::poll`].
const IDLE_CORPUS_EDITS: usize = 256;

/// The browser's recent row edits ([`CorpusEdit`]), oldest first.
#[derive(Debug, Default)]
pub(super) struct CorpusEdits {
    pub(super) entries: VecDeque<(u64, CorpusEdit)>,
    /// The mark of the load being tracked: every edit from it on is kept
    /// until that load installs its corpus. `None`: no load tracked yet,
    /// only the newest [`IDLE_CORPUS_EDITS`] are kept.
    pub(super) since: Option<u64>,
}

impl Browser {
    /// Prepare a corpus for [`Browser::install_prepared`] (any thread; this
    /// is the O(corpus) work of finishing a load or a refresh): keep the
    /// first conversation per canonical path when `dedupe` (upstream:
    /// refresh only, addendum C2), sort newest first (stable), set each
    /// conversation's `index`, and build the search data and the lookup
    /// data (positions by path and session id).
    pub fn prepare_corpus(conversations: Vec<Conversation>, dedupe: bool) -> PreparedCorpus {
        Self::prepare_shared(
            Arc::new(conversations.into_iter().map(Arc::new).collect()),
            dedupe,
            None,
        )
    }

    /// What a preparation for this browser needs to know: the scope
    /// settings now and, for a refresh, the current corpus (so the install
    /// maps the visible rows to the new corpus without hashing their paths).
    pub(super) fn prepare_context(&self, refresh: bool) -> PrepareContext {
        PrepareContext {
            scope: self.scope_settings(),
            previous: refresh.then(|| self.corpus_snapshot()),
        }
    }

    /// [`Browser::prepare_corpus`] for conversations that may be shared
    /// (the loading rows of a browser). An `index` that cannot be set
    /// because the conversation is still shared is set by the install.
    /// With a `context`, the rows in scope and the row mapping of a refresh
    /// are prepared too.
    pub(super) fn prepare_shared(
        snapshot: CorpusSnapshot,
        dedupe: bool,
        context: Option<PrepareContext>,
    ) -> PreparedCorpus {
        let mut conversations = Arc::unwrap_or_clone(snapshot);
        if dedupe {
            let mut paths = HashSet::new();
            conversations.retain(|conversation| {
                let path = conversation
                    .path
                    .canonicalize()
                    .unwrap_or_else(|_| conversation.path.clone());
                paths.insert(path)
            });
        }
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        let indexed = reindex(&mut conversations, false);
        let searchable = search::precompute_search_text(&conversations)
            .into_iter()
            .map(Arc::new)
            .collect();
        let (settings, previous) = match context {
            Some(context) => (Some(context.scope), context.previous),
            None => (None, None),
        };
        let index = CorpusIndex::build(&conversations, settings);
        let remap = previous.map(|previous| {
            let positions = previous
                .iter()
                .map(|conversation| index.first_path(&conversations, &conversation.path))
                .collect();
            Remap {
                previous,
                positions,
            }
        });
        PreparedCorpus {
            conversations,
            searchable,
            index,
            indexed,
            remap,
        }
    }

    /// Install a corpus from [`Browser::prepare_corpus`]:
    ///
    /// * while loading, it finishes the load like
    ///   [`Browser::finish_loading`] (the loading rows are replaced);
    /// * otherwise it replaces the corpus like
    ///   [`Browser::replace_conversations`] (selection kept by path; with a
    ///   non-empty query the previous rows stay until the new results
    ///   arrive, quirk 16).
    ///
    /// Only pointer-sized work per conversation runs here; a query that
    /// must be re-applied is sent to the search worker.
    pub fn install_prepared(&mut self, prepared: PreparedCorpus) {
        if self.is_loading() {
            self.finish_with(prepared);
        } else {
            self.replace_with(prepared);
        }
    }

    /// Append a batch of conversations during loading, unsorted, in arrival
    /// order. The batch is filtered by exclusions and scope (not by the
    /// query) and the first row is selected if nothing was. The search
    /// index is built when loading finishes.
    pub fn append_conversations(&mut self, new_convs: Vec<Conversation>) {
        let start_idx = self.conversations.len();
        self.corpus_mut()
            .extend(new_convs.into_iter().map(Arc::new));
        let end_idx = self.conversations.len();
        self.index.extend(&self.conversations, start_idx);
        self.ensure_scope();

        let new_filtered = self.filter_indices(start_idx..end_idx);
        self.filtered.extend(new_filtered);

        // Select first item if nothing selected yet
        if self.selected.is_none() && !self.filtered.is_empty() {
            self.selected = Some(0);
        }

        // Update loading count
        self.loading_state = LoadingState::Loading {
            loaded: self.conversations.len(),
        };
    }

    /// Mark loading as complete: sort newest first, reindex, build the
    /// search index, apply the query typed while loading (selection resets
    /// to the first row), then dispatch the semantic search or prewarm.
    ///
    /// Synchronous convenience: it runs [`Browser::prepare_corpus`] on the
    /// caller's thread. [`HistoryLoader`](super::HistoryLoader) prepares on
    /// a worker thread instead.
    pub fn finish_loading(&mut self) {
        let context = self.prepare_context(false);
        let prepared = Self::prepare_shared(self.conversations.clone(), false, Some(context));
        self.finish_with(prepared);
    }

    /// Replace the corpus with a refreshed one (upstream
    /// `replace_conversations`): dedupe by canonical path, sort newest
    /// first, reindex, keep the selection by path, rebuild the search data,
    /// then re-filter (empty query, plus prewarm) or re-dispatch.
    ///
    /// Quirk 16 fix: with a non-empty query the previous rows stay visible,
    /// re-mapped to the new corpus by path, until the new results arrive.
    ///
    /// On a browser that is still loading (upstream never refreshed one;
    /// for example a refresh that retries a failed initial load), the new
    /// corpus finishes the load instead, like [`Browser::finish_loading`].
    ///
    /// Synchronous convenience for [`Browser::prepare_corpus`] (with dedupe)
    /// plus [`Browser::install_prepared`].
    pub fn replace_conversations(&mut self, conversations: Vec<Conversation>) {
        let context = self.prepare_context(!self.is_loading());
        let prepared = Self::prepare_shared(
            Arc::new(conversations.into_iter().map(Arc::new).collect()),
            true,
            Some(context),
        );
        self.install_prepared(prepared);
    }

    /// Finish a load with a prepared corpus.
    fn finish_with(&mut self, prepared: PreparedCorpus) {
        let prepared = self.replay_corpus_edits(prepared);
        self.set_corpus(prepared);

        // The loading rows index the unsorted corpus. Rebuild them against
        // the sorted one: a query (below) leaves them visible until its
        // results arrive, so stale indices would show (and select) the
        // wrong, possibly excluded, conversations. Upstream kept them.
        self.filtered = self.scope_rows();
        self.selected = (!self.filtered.is_empty()).then_some(0);
        self.selection_anchor = None;
        self.loading_state = LoadingState::Ready;
        self.corpus_changed();

        // Apply the query typed while loading (exclusions and scope are
        // already applied).
        self.update_filter();
        if self.list_search_mode == ListSearchMode::Semantic && !self.query.trim().is_empty() {
            self.dispatch_search();
        } else {
            self.prewarm_semantic_cache();
        }
    }

    /// Replace a ready corpus with a prepared one.
    ///
    /// The result of a UUID jump survives (K2): a session it shows that the
    /// new corpus lacks (one loaded from disk past the filters, or one a
    /// narrower reload dropped) is appended to the new corpus again, as
    /// [`Browser::resolve_uuid_lookup`] appended it, and stays the only
    /// row; a [`UuidLookup::NotFound`] stays not found. Upstream re-read the
    /// file synchronously here; without that, the lookup would fall back to
    /// [`UuidLookup::Pending`] with no GUI request to resolve it.
    fn replace_with(&mut self, prepared: PreparedCorpus) {
        let mut prepared = self.replay_corpus_edits(prepared);
        let uuid_result = self.uuid_lookup.clone();
        let carried = match &uuid_result {
            Some(UuidLookup::Found {
                conversation_index, ..
            }) => self
                .conversations
                .get(*conversation_index)
                .map(|session| (*conversation_index, session.clone())),
            _ => None,
        }
        .filter(|(_, session)| {
            prepared
                .index
                .first_session(&prepared.conversations, &session.session_id)
                .is_none()
        });
        if let Some((previous_position, session)) = &carried {
            prepared.push(session.clone(), *previous_position);
        }
        let anchor = self.selected_path();
        // Quirk 16: with a query, the visible rows stay (in the new corpus)
        // until the new results arrive.
        let kept_rows = (!self.query.trim().is_empty()).then(|| self.rows_in_prepared(&prepared));
        self.selection_anchor = anchor.clone();
        self.set_corpus(prepared);
        self.filtered.clear();
        self.selected = None;
        self.corpus_changed();

        match kept_rows {
            None => {
                self.update_filter();
                self.prewarm_semantic_cache();
            }
            Some(rows) => {
                self.keep_rows(rows, anchor.as_deref());
                self.dispatch_search();
                self.keep_uuid_result(uuid_result, carried.is_some());
            }
        }
    }

    /// The current rows as positions in `prepared`: through the remap the
    /// preparation computed when the corpus is still the one it mapped, else
    /// by path.
    fn rows_in_prepared(&self, prepared: &PreparedCorpus) -> Vec<usize> {
        match &prepared.remap {
            Some(remap) if Arc::ptr_eq(&remap.previous, &self.conversations) => self
                .filtered
                .iter()
                .filter_map(|&index| remap.positions.get(index).copied().flatten())
                .collect(),
            _ => self
                .filtered
                .iter()
                .filter_map(|&index| {
                    prepared
                        .index
                        .first_path(&prepared.conversations, &self.conversations[index].path)
                })
                .collect(),
        }
    }

    /// After [`Browser::replace_with`] re-ran a UUID query: keep how the
    /// previous result was reached (`loaded_from_disk` of a carried
    /// session), and keep [`UuidLookup::NotFound`] instead of a new
    /// [`UuidLookup::Pending`] for the same UUID.
    fn keep_uuid_result(&mut self, previous: Option<UuidLookup>, carried: bool) {
        match (previous, &mut self.uuid_lookup) {
            (
                Some(UuidLookup::Found {
                    loaded_from_disk, ..
                }),
                Some(UuidLookup::Found {
                    conversation_index,
                    loaded_from_disk: now,
                }),
            ) if carried && *conversation_index + 1 == self.conversations.len() => {
                *now = loaded_from_disk;
            }
            (Some(UuidLookup::NotFound { uuid }), Some(UuidLookup::Pending { uuid: pending }))
                if *pending == uuid =>
            {
                self.uuid_lookup = Some(UuidLookup::NotFound { uuid });
            }
            _ => {}
        }
    }

    /// Swap the prepared corpus in and give each conversation its index.
    fn set_corpus(&mut self, prepared: PreparedCorpus) {
        let PreparedCorpus {
            mut conversations,
            searchable,
            index,
            indexed,
            remap,
        } = prepared;
        // The remap holds the previous corpus: release it first.
        drop(remap);
        let mut previous = Some(std::mem::replace(
            &mut self.conversations,
            Arc::new(Vec::new()),
        ));
        if !indexed {
            if self.is_loading() {
                // The loading rows are the prepared conversations: dropping
                // them only releases pointers, and lets the reindex below
                // write in place.
                previous = None;
            }
            // A refresh shares only the few replayed or carried
            // conversations with the previous corpus; those are copied.
            reindex(&mut conversations, true);
        }
        self.conversations = Arc::new(conversations);
        let previous_search = std::mem::replace(&mut self.searchable, Arc::new(searchable));
        let previous_index = std::mem::replace(&mut self.index, index);
        release_replaced(previous, previous_search, previous_index);
        self.ensure_scope();
        self.uuid_lookup = None;
    }

    /// Show `rows` (positions that still pass the filters are kept),
    /// selecting the row of `anchor` if present, else the first row. The
    /// selection anchor is kept for the next result set.
    fn keep_rows(&mut self, rows: Vec<usize>, anchor: Option<&Path>) {
        self.filtered = self.filter_indices(rows);
        self.selected = anchor
            .and_then(|anchor| self.row_of_path(&self.filtered, anchor))
            .or_else(|| (!self.filtered.is_empty()).then_some(0));
    }

    /// The corpus for in-place edits (copies the vector of pointers when a
    /// worker still holds the current snapshot).
    pub(super) fn corpus_mut(&mut self) -> &mut Vec<Arc<Conversation>> {
        Arc::make_mut(&mut self.conversations)
    }

    /// Whether the search data is built (parallel to the corpus): not while
    /// loading (it is built when loading finishes) nor in single-file mode.
    fn search_data_built(&self) -> bool {
        !self.is_loading() && !self.single_file_mode
    }

    /// The search data for in-place edits, or `None` while it is not built.
    fn search_data_mut(&mut self) -> Option<&mut Vec<Arc<SearchableConversation>>> {
        self.search_data_built()
            .then(|| Arc::make_mut(&mut self.searchable))
    }

    /// The corpus or its search data changed: make the semantic worker
    /// reload the corpus on its next dispatch and drop results computed
    /// against the old corpus. (The lexical worker gets the snapshots with
    /// each query, [`super::SearchCommand`].)
    pub(super) fn corpus_changed(&mut self) {
        self.semantic_corpus_version += 1;
        self.semantic_sent_scope_signature = None;
        self.invalidate_search_generation();
    }

    /// Replace the conversation at `index` (same path) and its search data.
    pub(super) fn replace_conversation_at(
        &mut self,
        index: usize,
        conversation: Arc<Conversation>,
    ) {
        self.record_corpus_edit(CorpusEdit::Replaced(conversation.path.clone()));
        if let Some(searchable) = self.search_data_mut() {
            searchable[index] = Arc::new(SearchableConversation::new(&conversation, index));
        }
        let previous = std::mem::replace(&mut self.corpus_mut()[index], conversation);
        self.index.replace(&self.conversations, index, &previous);
    }

    /// Append one conversation (a session loaded from disk by a UUID jump)
    /// with its search data. Returns its index.
    pub(super) fn push_conversation(&mut self, mut conversation: Conversation) -> usize {
        let index = self.conversations.len();
        conversation.index = index;
        if let Some(searchable) = self.search_data_mut() {
            searchable.push(Arc::new(SearchableConversation::new(&conversation, index)));
        }
        self.corpus_mut().push(Arc::new(conversation));
        self.index.extend(&self.conversations, index);
        index
    }

    /// Remove every loaded conversation whose path is in `paths` from the
    /// corpus, the search data and the rows (upstream
    /// `remove_selected_from_list`, generalized; for example after a
    /// delete, so every deleted copy leaves the list). The selection keeps
    /// its position (clamped) when its row was removed, and keeps the same
    /// conversation otherwise. Semantic metadata and lexical evidence are
    /// cleared; no re-search. A [`UuidLookup::Found`] result follows the
    /// shifted indices, and is cleared when its own session is removed.
    /// Returns the number of conversations removed.
    ///
    /// The paths are looked up in the path index (CCR-2): the work on the
    /// caller's thread is one hash per path plus moving the corpus's
    /// pointers and row numbers.
    pub fn remove_paths(&mut self, paths: &[PathBuf]) -> usize {
        let mut removed = paths
            .iter()
            .flat_map(|path| self.index.path_positions(&self.conversations, path))
            .collect::<Vec<_>>();
        removed.sort_unstable();
        removed.dedup();
        self.remove_indices(&removed);
        removed.len()
    }

    /// Remove the conversations at `removed` (ascending indices into the
    /// corpus). See [`Browser::remove_paths`].
    pub(super) fn remove_indices(&mut self, removed: &[usize]) {
        if removed.is_empty() {
            return;
        }
        let search_was_in_flight = self.search_in_flight;
        let old_len = self.conversations.len();
        let gone = removed
            .iter()
            .map(|&index| self.conversations[index].clone())
            .collect::<Vec<_>>();
        for conversation in &gone {
            self.record_corpus_edit(CorpusEdit::Removed(conversation.path.clone()));
        }
        let is_removed = |index: usize| removed.binary_search(&index).is_ok();
        // New index of a kept conversation.
        let shifted = |index: usize| index - removed.partition_point(|&gone| gone < index);

        let selected_row = self.selected;
        let removed_rows_before_selection = selected_row.map_or(0, |selected| {
            self.filtered[..selected.min(self.filtered.len())]
                .iter()
                .filter(|&&index| is_removed(index))
                .count()
        });

        remove_shared_positions(&mut self.conversations, removed);
        if self.search_data_built() {
            remove_shared_positions(&mut self.searchable, removed);
        }
        self.index.remove(removed, &gone, old_len);

        self.filtered = self
            .filtered
            .iter()
            .filter(|&&index| !is_removed(index))
            .map(|&index| shifted(index))
            .collect();

        // A UUID jump result indexes the corpus too: follow the shift, or
        // drop it when its session is the one removed.
        if let Some(UuidLookup::Found {
            conversation_index, ..
        }) = &mut self.uuid_lookup
        {
            if is_removed(*conversation_index) {
                self.uuid_lookup = None;
            } else {
                *conversation_index = shifted(*conversation_index);
            }
        }

        if let Some(selected) = selected_row {
            self.selected = Some(selected - removed_rows_before_selection);
        }
        if self.filtered.is_empty() {
            self.selected = None;
        } else if let Some(selected) = self.selected
            && selected >= self.filtered.len()
        {
            self.selected = Some(self.filtered.len() - 1);
        }

        self.corpus_changed();
        // `corpus_changed` dropped the pending answer; a lexical search that
        // was running goes out again so the rows settle on its result.
        if search_was_in_flight && self.list_search_mode == ListSearchMode::Lexical {
            let query = self.query.trim().to_string();
            if !ParsedQuery::parse(&query).is_effectively_empty() && !search::is_uuid(&query) {
                self.dispatch_lexical_search(query);
            }
        }
    }

    /// Stamp and keep a row edit for a load that may be running.
    fn record_corpus_edit(&mut self, edit: CorpusEdit) {
        let stamp = CORPUS_EDIT_CLOCK.fetch_add(1, Ordering::SeqCst);
        let edits = &mut self.corpus_edits;
        edits.entries.push_back((stamp, edit));
        if edits.since.is_none() && edits.entries.len() > IDLE_CORPUS_EDITS {
            edits.entries.pop_front();
        }
    }

    /// Track the edits of a load that started at `mark` (from
    /// [`corpus_edit_mark`]): every edit stamped from it on is replayed when
    /// the load installs its corpus. Called at each poll of the load.
    pub(super) fn track_corpus_edits_since(&mut self, mark: u64) {
        let edits = &mut self.corpus_edits;
        if edits.since != Some(mark) {
            edits.entries.retain(|(stamp, _)| *stamp >= mark);
            edits.since = Some(mark);
        }
    }

    /// Stop tracking (the load ended without installing a corpus).
    pub(super) fn stop_tracking_corpus_edits(&mut self) {
        self.corpus_edits = CorpusEdits::default();
    }

    /// Apply the tracked edits (rows deleted or renamed since the load
    /// started) to the prepared corpus. A deleted row is dropped; a renamed
    /// row is the browser's current version of it (keeping the project
    /// name and path the load found). Each edit is one indexed lookup.
    fn replay_corpus_edits(&mut self, mut prepared: PreparedCorpus) -> PreparedCorpus {
        if self.corpus_edits.since.is_none() {
            // Not a tracked load (a synchronous replace): nothing to replay.
            return prepared;
        }
        let edits = std::mem::take(&mut self.corpus_edits);
        if edits.entries.is_empty() {
            return prepared;
        }
        let mut removed = HashSet::new();
        let mut replaced = HashSet::new();
        for (_, edit) in edits.entries {
            match edit {
                CorpusEdit::Removed(path) => {
                    replaced.remove(&path);
                    removed.insert(path);
                }
                CorpusEdit::Replaced(path) => {
                    // The last edit of a path wins (a session deleted, then
                    // found again by a UUID jump and renamed).
                    removed.remove(&path);
                    replaced.insert(path);
                }
            }
        }

        for path in &replaced {
            let Some(current) = self
                .position_of_path(path)
                .map(|position| self.conversations[position].clone())
            else {
                continue;
            };
            let positions = prepared
                .index
                .path_positions(&prepared.conversations, path)
                .collect::<Vec<_>>();
            for position in positions {
                let loaded = &prepared.conversations[position];
                let renamed = if current.project_name == loaded.project_name
                    && current.project_path == loaded.project_path
                {
                    current.clone()
                } else {
                    let mut updated = Conversation::clone(&current);
                    updated.project_name = loaded.project_name.clone();
                    updated.project_path = loaded.project_path.clone();
                    Arc::new(updated)
                };
                prepared.replace(position, renamed);
            }
        }

        let mut removed_positions = removed
            .iter()
            .flat_map(|path| prepared.index.path_positions(&prepared.conversations, path))
            .collect::<Vec<_>>();
        removed_positions.sort_unstable();
        removed_positions.dedup();
        prepared.remove_positions(&removed_positions);
        prepared
    }
}

/// Drop what an install replaced. After a refresh the browser can hold the
/// last reference to every previous conversation and its search data, and
/// freeing 20k of them took about 35 ms: that happens on a short-lived
/// `corpus-release` thread instead of the caller's (CCR-1). Nothing to free
/// (the loading rows, an empty corpus) is dropped right here.
fn release_replaced(
    corpus: Option<CorpusSnapshot>,
    searchable: SearchSnapshot,
    index: CorpusIndex,
) {
    if corpus.as_ref().is_none_or(|corpus| corpus.is_empty()) && searchable.is_empty() {
        return;
    }
    let replaced = (corpus, searchable, index);
    // Should no thread start, the closure (and the corpus) is dropped here.
    let _ = std::thread::Builder::new()
        .name("corpus-release".into())
        .spawn(move || drop(replaced));
}

/// Remove `removed` (ascending positions) from a shared vector: in place
/// when no worker holds it, else a copy of the kept pointers.
fn remove_shared_positions<T: Clone>(items: &mut Arc<Vec<T>>, removed: &[usize]) {
    match Arc::get_mut(items) {
        Some(items) => remove_positions_in_place(items, removed),
        None => *items = Arc::new(without_positions(items, removed)),
    }
}

/// Set each conversation's `index` to its position. A conversation that is
/// still shared is copied when `clone_shared`, else left for later. Returns
/// whether every `index` is now set.
fn reindex(conversations: &mut [Arc<Conversation>], clone_shared: bool) -> bool {
    let mut all_set = true;
    for (index, conversation) in conversations.iter_mut().enumerate() {
        if conversation.index == index {
            continue;
        }
        if let Some(unique) = Arc::get_mut(conversation) {
            unique.index = index;
        } else if clone_shared {
            Arc::make_mut(conversation).index = index;
        } else {
            all_set = false;
        }
    }
    all_set
}
