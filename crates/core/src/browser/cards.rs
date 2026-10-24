//! Generation mode: the rows are the cards of a store generation (upstream
//! `tui/app.rs` `show_snapshot` to `conversation_at`, 3708bb6, ac7f591 and
//! 358becb).
//!
//! With a store (the GUI's main list, [`super::HistoryLoader`] in store
//! mode) the browser never holds the loaded history: its rows are list-only
//! conversations built from the cards of the generation shown (no
//! `full_text`, no semantic data, no parse errors), lexical queries run on
//! the generation's index, and each newer generation a reconcile pass
//! publishes is swapped in keeping the query, the scope and the selected
//! session. Edits not yet in a generation are overlays: a deleted session
//! is hidden (by `CardRow::key_hash`) from the list and the index's scope, a
//! renamed one shows its new title (by path), and both ask for a reconcile
//! pass ([`Browser::take_reconcile_request`]); a swap drops the overlays the
//! new generation reflects.
//!
//! Two steps show a generation:
//!
//! 1. [`Browser::show_snapshot`] lists the newest [`SNAPSHOT_ROWS`] cards in
//!    scope for the first frame (bounded work, on the caller's thread);
//!    queries wait.
//! 2. [`Browser::install_generation`] takes a [`GenerationOpen`]: every card
//!    materialized, its lookup data and the generation's index, built off
//!    the UI thread. From then on row `r` below [`Browser::generation_len`]
//!    is doc `r`, the browse list holds every card in scope, and lexical
//!    queries run on the index ([`super::Readiness::lexical`]). A swap is
//!    the same install for a newer generation.
//!
//! [`Browser::open_search_index`] and [`Browser::swap_generation`] are the
//! synchronous conveniences (upstream's names) that build the
//! [`GenerationOpen`] on the caller's thread.
//!
//! # Differences from upstream
//!
//! * **Off the UI thread.** Upstream materialized the cards of the browse
//!   list (every card in scope) and opened the index on the TUI thread. Here
//!   [`GenerationOpen::build`] does both, plus the path and session lookups
//!   and where each shown row moves, on the loader's `generation-open`
//!   thread; the install swaps pointers. The browse list holds every card,
//!   so a query result never materializes a card (upstream built them on
//!   the TUI thread as results arrived).
//! * **Rows kept across a swap** (quirk 16; upstream 7ec719a, later): a
//!   query's rows stay shown, moved to the new generation's rows by path,
//!   until the query answers on the new index.
//! * **A UUID result read from disk** that the new generation lacks is kept
//!   as a row after the cards (upstream read the file again on the TUI
//!   thread).
//! * **Time filter at runtime.** [`Browser::set_time_filter`] re-scopes the
//!   cards (upstream fixed the filter at launch).
//! * **Visible evidence.** [`Browser::fill_visible_evidence`] decompresses
//!   the frames of the visible rows the worker left without evidence on a
//!   `row-evidence` thread (upstream: on the TUI thread, every frame), which
//!   the browser joins when it is dropped.

use super::index::{CardScope, CorpusIndex, ScopeIndex};
use super::{Browser, CorpusSnapshot, ListSearchMode, SNAPSHOT_ROWS, UuidLookup};
use crate::history::Conversation;
use crate::search::LexicalEvidence;
use crate::search::query::ParsedQuery;
use crate::store::{SearchIndex, Snapshot};
use crate::time_filter::TimeFilter;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

/// The doc of a row that is not a card (a session a UUID jump read from
/// disk).
pub(super) const NO_DOC: u32 = u32::MAX;

/// Evidence computed for visible rows: the search generation it was
/// computed for and the evidence by row.
type RowEvidence = (u64, Vec<(usize, LexicalEvidence)>);

/// What the browser holds in generation mode besides the rows.
#[derive(Default)]
pub(super) struct CardRows {
    /// While only the first cards are shown: the doc of each row
    /// ([`NO_DOC`] for a session read from disk). Empty once the generation
    /// is open.
    row_docs: Vec<u32>,
    /// Once the generation is open: its number of cards, which are rows
    /// `0..n` (doc = row).
    open: Option<usize>,
    /// Per doc: inside the time filter, its project in scope (exclusions,
    /// workspace) and not deleted here.
    pub(super) scope: Vec<bool>,
    /// Sessions deleted here, by `CardRow::key_hash` (the path's key, also
    /// for a session read from disk), hidden until a generation without
    /// them is shown.
    removed: HashSet<u64>,
    /// Titles set here, by path, shown until a generation holds them.
    renamed: HashMap<PathBuf, Option<String>>,
    /// The session files changed: the store should reconcile.
    reconcile_requested: bool,
    /// Rows whose evidence was asked for by [`Browser::fill_visible_evidence`].
    evidence_checked: HashSet<usize>,
    /// Answers of the `row-evidence` threads.
    evidence: Option<(mpsc::Sender<RowEvidence>, mpsc::Receiver<RowEvidence>)>,
    /// Whether the generation's cards come from more than one source.
    mixed_sources: bool,
}

impl CardRows {
    /// The overlays and the reconcile request survive a new generation;
    /// everything else starts over.
    fn carry_over(&mut self) -> Self {
        Self {
            removed: std::mem::take(&mut self.removed),
            renamed: std::mem::take(&mut self.renamed),
            reconcile_requested: self.reconcile_requested,
            evidence: self.evidence.take(),
            ..Self::default()
        }
    }
}

/// A generation opened off the UI thread for [`Browser::install_generation`]:
/// every card materialized, their lookup data and the generation's index.
pub(super) struct GenerationOpen {
    snapshot: Snapshot,
    index: Option<Arc<SearchIndex>>,
    cards: Vec<Arc<Conversation>>,
    lookup: CorpusIndex,
    /// For each row of the corpus shown when the open started, its card in
    /// this generation (by path).
    remap: Option<(CorpusSnapshot, Vec<Option<usize>>)>,
    /// The semantic corpus's conversations mapped to this generation's docs.
    semantic_docs: Option<(CorpusSnapshot, Arc<Vec<Option<u32>>>)>,
}

impl GenerationOpen {
    /// Materializes every card of `snapshot` and opens its index (O(cards);
    /// any thread). `shown` is the browser's corpus, whose rows a query
    /// keeps until it answers on the new index; `semantic` the semantic
    /// corpus, mapped to the new docs.
    pub(super) fn build(
        snapshot: Snapshot,
        shown: Option<CorpusSnapshot>,
        semantic: Option<CorpusSnapshot>,
    ) -> Self {
        let index = SearchIndex::open(snapshot.clone()).map(Arc::new);
        // Built here so lookups by key never build it on the UI thread.
        snapshot.docs_by_key();
        let cards: Vec<Arc<Conversation>> = (0..snapshot.len() as u32)
            .map(|doc| Arc::new(snapshot.card(doc)))
            .collect();
        let lookup = CorpusIndex::build(&cards, None);
        let remap = shown.map(|shown| {
            let positions = shown
                .iter()
                .map(|conversation| lookup.first_path(&cards, &conversation.path))
                .collect();
            (shown, positions)
        });
        let semantic_docs = semantic.map(|corpus| {
            let docs = Arc::new(semantic_docs(&snapshot, &corpus));
            (corpus, docs)
        });
        Self {
            snapshot,
            index,
            cards,
            lookup,
            remap,
            semantic_docs,
        }
    }

    /// Name of the generation opened.
    pub(super) fn generation_name(&self) -> &str {
        self.snapshot.generation_name()
    }

    /// The preview mode its cards were built with.
    pub(super) fn show_last(&self) -> bool {
        self.snapshot.show_last()
    }
}

/// The doc of each conversation of `corpus` in `snapshot`, by path.
pub(super) fn semantic_docs(snapshot: &Snapshot, corpus: &[Arc<Conversation>]) -> Vec<Option<u32>> {
    let by_key = snapshot.docs_by_key();
    corpus
        .iter()
        .map(|conversation| {
            let key = crate::store::generation::key_hash(&conversation.path.to_string_lossy());
            by_key.get(&key).copied()
        })
        .collect()
}

impl Browser {
    /// Shows the cards of `snapshot`: rows come from its cards from now on
    /// (generation mode). Only the newest [`SNAPSHOT_ROWS`] cards in scope
    /// (exclusions, workspace, the snapshot's time filter) are materialized
    /// for the first frame, the first one selected; queries wait until
    /// [`Browser::install_generation`] (or [`Browser::open_search_index`])
    /// lists the rest and opens the index. No-op in single-file mode.
    pub fn show_snapshot(&mut self, snapshot: Snapshot) {
        if self.single_file_mode {
            return;
        }
        self.reset_rows(snapshot);
        self.show_snapshot_cards();
        self.readiness.list = true;
        if self.list_search_mode == ListSearchMode::Semantic {
            self.ensure_semantic_corpus();
        }
    }

    /// Makes `snapshot` the source of rows, dropping the rows shown.
    fn reset_rows(&mut self, snapshot: Snapshot) {
        self.leave_memory_mode();
        let docs = snapshot.len();
        self.cards = self.cards.carry_over();
        self.cards.mixed_sources = snapshot.has_multiple_sources();
        self.snapshot = Some(snapshot);
        self.cards.scope = self.card_scope();
        let mut index = CorpusIndex::default();
        index.scope = ScopeIndex::all(0, self.scope_settings());
        self.conversations = Arc::new(Vec::new());
        self.index = index;
        self.doc_rows = vec![None; docs];
        self.search_index = None;
        self.index_scope = None;
        self.readiness.lexical = false;
        self.lexical_evidence.clear();
        self.uuid_lookup = None;
    }

    /// Leaves the in-memory mode: its search data is not used by cards.
    fn leave_memory_mode(&mut self) {
        if self.snapshot.is_none() {
            self.searchable = Arc::new(Vec::new());
            self.streaming = false;
        }
    }

    /// Every card's scope under the current settings and time filter, with
    /// the deleted cards out (O(cards), memory only; each project is
    /// decided once).
    fn card_scope(&self) -> Vec<bool> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let scope = CardScope::new(&self.excluded_projects, self.effective_workspace());
        let mut docs = snapshot.scope(|project| scope.project_visible(project));
        if !self.cards.removed.is_empty() {
            let by_key = snapshot.docs_by_key();
            for key in &self.cards.removed {
                if let Some(&doc) = by_key.get(key) {
                    docs[doc as usize] = false;
                }
            }
        }
        docs
    }

    /// Whether the generation shown mixes sources (while only its first
    /// cards are rows).
    pub(super) fn cards_mixed_sources(&self) -> bool {
        self.cards.mixed_sources
    }

    /// Whether the rows are cards of a generation (generation mode).
    pub fn shows_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    /// Whether every card of the generation shown is a row and its index
    /// (if it has one) is in use.
    pub fn generation_open(&self) -> bool {
        self.cards.open.is_some()
    }

    /// Generation name of the index lexical queries run on, if any.
    pub fn search_index_generation(&self) -> Option<&str> {
        self.search_index
            .as_ref()
            .map(|index| index.cards().generation_name())
    }

    /// Number of cards in the generation shown, in generation mode.
    pub fn generation_len(&self) -> Option<usize> {
        self.snapshot.as_ref().map(Snapshot::len)
    }

    /// Identity of the generation shown ([`Snapshot::generation_id`]).
    pub(super) fn generation_id(&self) -> Option<String> {
        self.snapshot.as_ref().map(Snapshot::generation_id)
    }

    /// Name of the generation shown, in generation mode.
    pub fn generation_name(&self) -> Option<&str> {
        self.snapshot.as_ref().map(Snapshot::generation_name)
    }

    /// Whether no card of the generation shown is inside the time filter.
    pub fn generation_is_empty(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.head(1, |_| true).is_empty())
    }

    /// The time filter the cards are listed under (inactive outside
    /// generation mode).
    pub fn time_filter(&self) -> TimeFilter {
        self.snapshot
            .as_ref()
            .map(|snapshot| *snapshot.time_filter())
            .unwrap_or_default()
    }

    /// Lists the cards under `time` (fork: the GUI changes the time filter
    /// at runtime; upstream fixed it at launch). Re-scopes the cards and
    /// applies the query again. No-op outside generation mode or when
    /// unchanged.
    pub fn set_time_filter(&mut self, time: TimeFilter) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if *snapshot.time_filter() == time {
            return;
        }
        self.snapshot = Some(snapshot.with_time_filter(time));
        self.semantic_sent_scope_signature = None;
        self.invalidate_search_generation();
        self.rescope_cards();
    }

    /// The doc of row `row`, if it is a card.
    pub(super) fn row_doc(&self, row: usize) -> Option<u32> {
        match self.cards.open {
            Some(docs) => (row < docs).then_some(row as u32),
            None => self
                .cards
                .row_docs
                .get(row)
                .copied()
                .filter(|&doc| doc != NO_DOC),
        }
    }

    /// The membership of every row in scope: a card's scope, never a
    /// session read from disk (it is only shown as a UUID result).
    fn generation_members(&self) -> Vec<bool> {
        (0..self.conversations.len())
            .map(|row| {
                self.row_doc(row)
                    .is_some_and(|doc| self.cards.scope[doc as usize])
            })
            .collect()
    }

    /// The rows' scope from the cards' scope (generation mode's
    /// [`Browser::ensure_scope`]).
    pub(super) fn set_generation_scope(&mut self) {
        let member = self.generation_members();
        self.index.scope = ScopeIndex::from_members(member, self.scope_settings());
    }

    /// The browse list over the first cards: the newest [`SNAPSHOT_ROWS`]
    /// in scope.
    pub(super) fn show_snapshot_cards(&mut self) {
        if self.snapshot.is_none() {
            return;
        }
        let docs: Vec<u32> = self
            .cards
            .scope
            .iter()
            .enumerate()
            .filter(|(_, in_scope)| **in_scope)
            .take(SNAPSHOT_ROWS)
            .map(|(doc, _)| doc as u32)
            .collect();
        let filtered = docs.into_iter().map(|doc| self.card_row(doc)).collect();
        self.uuid_lookup = None;
        self.lexical_evidence.clear();
        self.apply_filtered(filtered);
    }

    /// The scope, the exclusions or the time filter changed: every card's
    /// scope is decided again (from its generation's projects), and the
    /// browse list or the query is applied again.
    pub(super) fn rescope_cards(&mut self) {
        self.cards.scope = self.card_scope();
        self.set_generation_scope();
        self.send_index_scope();
        if self.cards.open.is_none() {
            self.show_snapshot_cards();
        } else if self.list_search_mode == ListSearchMode::Semantic && !self.query.trim().is_empty()
        {
            self.dispatch_search();
        } else {
            self.update_filter();
        }
    }

    /// Row of card `doc`, materializing it on first use while only the
    /// first cards are shown.
    pub(super) fn card_row(&mut self, doc: u32) -> usize {
        if self.cards.open.is_some() {
            return doc as usize;
        }
        if let Some(row) = self.doc_rows[doc as usize] {
            return row;
        }
        let snapshot = self.snapshot.as_ref().expect("cards come from a snapshot");
        let mut card = snapshot.card(doc);
        if let Some(title) = self.cards.renamed.get(&card.path) {
            card.custom_title = title.clone();
        }
        let row = self.conversations.len();
        card.index = row;
        self.corpus_mut().push(Arc::new(card));
        self.index.push_in_scope(&self.conversations, row);
        self.doc_rows[doc as usize] = Some(row);
        self.cards.row_docs.push(doc);
        row
    }

    /// A session read from disk by a UUID jump was appended in generation
    /// mode: it is no card and stays out of the browse list.
    pub(super) fn pushed_non_card(&mut self) {
        if self.snapshot.is_none() {
            return;
        }
        if self.cards.open.is_none() {
            self.cards.row_docs.push(NO_DOC);
        }
        self.set_generation_scope();
    }

    /// Rows of index docs `docs`, in order, without the cards out of scope
    /// (deleted ones included).
    pub(super) fn rows_of_docs(&mut self, docs: &[usize]) -> Vec<usize> {
        let docs: Vec<u32> = docs
            .iter()
            .filter(|&&doc| self.cards.scope.get(doc).copied().unwrap_or(false))
            .map(|&doc| doc as u32)
            .collect();
        docs.into_iter().map(|doc| self.card_row(doc)).collect()
    }

    /// Evidence keyed by index doc, keyed by row instead.
    pub(super) fn evidence_of_docs(
        &self,
        evidence: HashMap<usize, LexicalEvidence>,
    ) -> HashMap<usize, LexicalEvidence> {
        match self.cards.open {
            Some(_) => evidence,
            None => evidence
                .into_iter()
                .filter_map(|(doc, evidence)| {
                    Some((self.doc_rows.get(doc).copied().flatten()?, evidence))
                })
                .collect(),
        }
    }

    /// Opens the index of the snapshot shown and lists every card in scope
    /// (upstream `open_search_index`; synchronous: O(cards) on the caller's
    /// thread, which a GUI leaves to [`super::HistoryLoader`]). Returns
    /// whether an index was opened.
    pub fn open_search_index(&mut self) -> bool {
        let Some(snapshot) = self.snapshot.clone() else {
            return false;
        };
        let open = GenerationOpen::build(snapshot, Some(self.corpus_snapshot()), None);
        let opened = open.index.is_some();
        self.install_generation(open);
        opened
    }

    /// Shows `snapshot` if it is a newer generation than the one shown,
    /// keeping the query, scope and selected session (upstream
    /// `swap_generation`; synchronous, see [`Browser::open_search_index`]).
    /// Returns whether it did.
    pub fn swap_generation(&mut self, snapshot: Snapshot) -> bool {
        if self.single_file_mode
            || self.snapshot.as_ref().is_some_and(|shown| {
                shown.generation_name() == snapshot.generation_name()
                    && shown.show_last() == snapshot.show_last()
            })
        {
            return false;
        }
        let semantic = self.semantic_conversations_loaded();
        let open = GenerationOpen::build(snapshot, Some(self.corpus_snapshot()), semantic);
        self.install_generation(open);
        true
    }

    /// Shows a generation opened off the UI thread: every card becomes a
    /// row, its index answers lexical queries, overlays the generation
    /// reflects are dropped and the others applied, and the query, the
    /// scope, the time filter shown and the selected session are kept (a
    /// query's rows stay until it answers). Pointer-sized work per card.
    pub(super) fn install_generation(&mut self, open: GenerationOpen) {
        if self.single_file_mode {
            return;
        }
        let GenerationOpen {
            snapshot,
            index,
            mut cards,
            mut lookup,
            remap,
            semantic_docs,
        } = open;
        // The time filter shown wins over the one the open started with.
        let snapshot = match &self.snapshot {
            Some(shown) if shown.time_filter() != snapshot.time_filter() => {
                snapshot.with_time_filter(*shown.time_filter())
            }
            _ => snapshot,
        };
        let anchor = self
            .selection_anchor
            .take()
            .or_else(|| self.selected_path());
        let query = self.query.trim().to_string();
        let parsed = ParsedQuery::parse(&query);
        let keep_rows = !parsed.is_effectively_empty() && !crate::search::is_uuid(&query);
        let kept_rows: Vec<usize> = if keep_rows {
            match &remap {
                Some((shown, positions)) if Arc::ptr_eq(shown, &self.conversations) => self
                    .filtered
                    .iter()
                    .filter_map(|&row| positions.get(row).copied().flatten())
                    .collect(),
                _ => self
                    .filtered
                    .iter()
                    .filter_map(|&row| lookup.first_path(&cards, &self.conversations[row].path))
                    .collect(),
            }
        } else {
            Vec::new()
        };
        drop(remap);
        // A session a UUID jump read from disk that the generation lacks.
        let carried = match &self.uuid_lookup {
            Some(UuidLookup::Found {
                conversation_index,
                loaded_from_disk: true,
            }) => self
                .conversations
                .get(*conversation_index)
                .filter(|session| lookup.first_session(&cards, &session.session_id).is_none())
                .cloned(),
            _ => None,
        };

        // The new generation holds whatever was changed here, unless it was
        // published before the change.
        self.cards.removed.retain(|key| snapshot.contains_key(*key));
        self.cards.renamed.retain(|path, title| {
            lookup
                .first_path(&cards, path)
                .is_none_or(|doc| cards[doc].custom_title != *title)
        });
        for (path, title) in &self.cards.renamed {
            if let Some(doc) = lookup.first_path(&cards, path) {
                Arc::make_mut(&mut cards[doc]).custom_title = title.clone();
            }
        }
        let docs = cards.len();
        if let Some(mut session) = carried {
            if session.index != docs {
                Arc::make_mut(&mut session).index = docs;
            }
            cards.push(session);
            lookup.extend(&cards, docs);
        }

        let previous = std::mem::replace(&mut self.conversations, Arc::new(cards));
        self.leave_memory_mode();
        self.snapshot = Some(snapshot);
        self.cards = self.cards.carry_over();
        self.cards.open = Some(docs);
        self.cards.scope = self.card_scope();
        self.index = lookup;
        self.set_generation_scope();
        self.doc_rows = Vec::new();
        self.readiness.list = true;
        self.readiness.lexical = index.is_some();
        self.search_index = index;
        if let Some((corpus, docs)) = semantic_docs {
            self.adopt_semantic_docs(&corpus, docs);
        }
        self.uuid_lookup = None;
        self.semantic_sent_scope_signature = None;
        self.invalidate_search_generation();
        self.send_index_scope();
        // Freed off this thread when it held the last references.
        super::corpus::release_corpus(previous);

        if keep_rows {
            self.filtered = self.filter_indices(kept_rows);
            self.selected = anchor
                .as_deref()
                .and_then(|anchor| self.row_of_path(&self.filtered, anchor))
                .or_else(|| (!self.filtered.is_empty()).then_some(0));
            self.selection_anchor = anchor;
            self.dispatch_search();
            if !self.search_in_flight {
                self.selection_anchor = None;
            }
        } else {
            self.selection_anchor = anchor;
            self.dispatch_search();
            self.selection_anchor = None;
            if self.list_search_mode == ListSearchMode::Semantic && self.ensure_semantic_corpus() {
                self.prewarm_semantic_cache();
            }
        }
    }

    /// Asks for a reconcile pass ([`Browser::take_reconcile_request`]), in
    /// generation mode, and calls the waker so the loader's next poll takes
    /// the request.
    ///
    /// The flag is set in memory mode too: the store loader takes it before
    /// its first generation exists, the in-memory loader never reads it.
    pub(super) fn request_reconcile(&mut self) {
        self.cards.reconcile_requested = true;
        if let Some(waker) = &self.waker {
            waker();
        }
    }

    /// Whether the session files changed here (a rename or a delete) since
    /// the last call, so the store should reconcile.
    pub fn take_reconcile_request(&mut self) -> bool {
        std::mem::take(&mut self.cards.reconcile_requested)
    }

    /// Records whether a reconcile pass may still publish a newer
    /// generation ([`super::Readiness::reconcile`]).
    pub fn set_reconciling(&mut self, running: bool) {
        self.readiness.reconcile = if running {
            super::ReconcileState::Running
        } else {
            super::ReconcileState::Done
        };
    }

    /// Hides the rows of `paths` in generation mode (upstream
    /// `remove_selected_from_list` with a snapshot): rows stay put so row
    /// indices remain valid; a card stays hidden until a generation without
    /// it is shown, and a reconcile pass is asked for. The selection keeps
    /// its position (clamped). Returns the number of rows hidden.
    pub(super) fn hide_paths(&mut self, paths: &[PathBuf]) -> usize {
        let mut rows: Vec<usize> = paths
            .iter()
            .flat_map(|path| self.index.path_positions(&self.conversations, path))
            .collect();
        rows.sort_unstable();
        rows.dedup();
        // Every path is recorded, with or without a row: a card past the
        // rows shown, or a session a running pass is about to publish,
        // must not come back when a generation is installed.
        self.cards
            .removed
            .extend(paths.iter().map(|path| path_key(path)));
        if rows.is_empty() {
            if !paths.is_empty() {
                self.cards.scope = self.card_scope();
                self.set_generation_scope();
                self.send_index_scope();
                self.semantic_sent_scope_signature = None;
                self.request_reconcile();
            }
            return 0;
        }
        for &row in &rows {
            self.cards
                .removed
                .insert(path_key(&self.conversations[row].path));
            if let Some(doc) = self.row_doc(row) {
                self.cards.scope[doc as usize] = false;
            }
        }
        self.set_generation_scope();
        let hidden = |row: &usize| rows.binary_search(row).is_ok();
        let selected = self.selected;
        let hidden_before = selected.map_or(0, |selected| {
            self.filtered[..selected.min(self.filtered.len())]
                .iter()
                .filter(|row| hidden(row))
                .count()
        });
        self.filtered.retain(|row| !hidden(row));
        self.selected = selected.map(|selected| selected - hidden_before);
        if self.filtered.is_empty() {
            self.selected = None;
        } else if let Some(selected) = self.selected
            && selected >= self.filtered.len()
        {
            self.selected = Some(self.filtered.len() - 1);
        }
        if let Some(UuidLookup::Found {
            conversation_index, ..
        }) = &self.uuid_lookup
            && hidden(conversation_index)
        {
            self.uuid_lookup = None;
        }
        self.send_index_scope();
        self.semantic_sent_scope_signature = None;
        self.request_reconcile();
        rows.len()
    }

    /// In memory mode, remembers `paths` as deleted for the cards of a
    /// generation that is shown later (store mode, first run: no generation
    /// yet), and asks the store to reconcile.
    pub(super) fn note_removed_before_generation(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() || self.snapshot.is_some() {
            return;
        }
        self.cards
            .removed
            .extend(paths.iter().map(|path| path_key(path)));
        self.request_reconcile();
    }

    /// Whether row `index` was hidden by a delete made here (generation
    /// mode keeps the rows of deleted sessions in place).
    pub fn is_hidden(&self, index: usize) -> bool {
        self.snapshot.is_some()
            && !self.cards.removed.is_empty()
            && self.conversations.get(index).is_some_and(|conversation| {
                self.cards.removed.contains(&path_key(&conversation.path))
            })
    }

    /// Whether no conversation is listed whatever the scope: no card inside
    /// the time filter in generation mode, else no loaded conversation.
    pub fn history_is_empty(&self) -> bool {
        if self.snapshot.is_some() {
            self.generation_is_empty()
        } else {
            self.conversations.is_empty()
        }
    }

    /// Shows the title of a session renamed here on its row until a
    /// generation holds it, and asks for a reconcile pass (upstream
    /// `submit_rename` with a snapshot). The index keeps the old title until
    /// then.
    pub(super) fn retitle_row(&mut self, row: usize, path: &Path, title: Option<String>) {
        let conversation = &mut self.corpus_mut()[row];
        if conversation.custom_title != title {
            Arc::make_mut(conversation).custom_title = title.clone();
        }
        self.cards.renamed.insert(path.to_path_buf(), title);
        self.request_reconcile();
    }

    /// Sends lexical queries the index and the docs in scope: the cards in
    /// scope, deleted ones out.
    pub(super) fn send_index_scope(&mut self) {
        self.index_scope =
            self.search_index
                .as_ref()
                .map(|index| super::search_state::IndexScope {
                    index: index.clone(),
                    docs: Arc::new(self.cards.scope.clone()),
                });
    }

    /// Stops generation mode: the in-memory corpus installed next replaces
    /// the cards (upstream `show_conversations` drops the index).
    pub(super) fn leave_generation(&mut self) {
        if self.snapshot.take().is_none() {
            return;
        }
        self.cards = CardRows::default();
        self.doc_rows = Vec::new();
        self.search_index = None;
        self.index_scope = None;
        self.retire_semantic_corpus();
        if self.search_in_flight && self.list_search_mode == ListSearchMode::Lexical {
            self.search_generation += 1;
            self.search_in_flight = false;
        }
        self.search_page_shown = false;
    }

    /// Computes lexical evidence for the rows at `rows` (positions in
    /// [`Browser::filtered`], the visible ones) that the search worker left
    /// without any, from their text frames, on a `row-evidence` thread
    /// (upstream: on the TUI thread every frame). A later
    /// [`Browser::poll`] applies it. Only in generation mode, for a lexical
    /// query whose answer is complete.
    pub fn fill_visible_evidence(&mut self, rows: std::ops::Range<usize>) {
        let Some(index) = self.search_index.clone() else {
            return;
        };
        if self.list_search_mode != ListSearchMode::Lexical || self.search_in_flight {
            return;
        }
        let parsed = ParsedQuery::parse(self.query.trim());
        if parsed.is_effectively_empty() {
            return;
        }
        let end = rows.end.min(self.filtered.len());
        let wanted: Vec<(usize, u32, Arc<str>)> = self.filtered[rows.start.min(end)..end]
            .iter()
            .copied()
            .filter(|row| {
                !self.lexical_evidence.contains_key(row)
                    && !self.cards.evidence_checked.contains(row)
            })
            .filter_map(|row| {
                let doc = self.row_doc(row)?;
                Some((row, doc, self.conversations[row].preview.clone()))
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.cards
            .evidence_checked
            .extend(wanted.iter().map(|(row, _, _)| *row));
        let tx = self
            .cards
            .evidence
            .get_or_insert_with(mpsc::channel)
            .0
            .clone();
        let generation = self.search_generation;
        let waker = self.waker.clone();
        // Joined when the browser is dropped (it reads the mapped text).
        let spawned = std::thread::Builder::new()
            .name("row-evidence".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let found = wanted
                    .into_iter()
                    .filter_map(|(row, doc, preview)| {
                        let text = index.text(doc);
                        crate::search::build_lexical_evidence(&text, &preview, &parsed)
                            .map(|evidence| (row, evidence))
                    })
                    .collect();
                if tx.send((generation, found)).is_ok()
                    && let Some(waker) = waker
                {
                    waker();
                }
            });
        if let Ok(thread) = spawned {
            self.helpers.push(thread);
        }
    }

    /// Applies the evidence of [`Browser::fill_visible_evidence`] computed
    /// for the current query. Returns whether any arrived.
    pub(super) fn receive_row_evidence(&mut self) -> bool {
        let Some((_, rx)) = &self.cards.evidence else {
            return false;
        };
        let mut applied = false;
        while let Ok((generation, found)) = rx.try_recv() {
            if generation == self.search_generation {
                applied |= !found.is_empty();
                self.lexical_evidence.extend(found);
            }
        }
        applied
    }

    /// Forget which rows asked for evidence (the results changed).
    pub(super) fn clear_evidence_checked(&mut self) {
        self.cards.evidence_checked.clear();
    }

    /// The conversation of the row stored at `path` (upstream
    /// `conversation_at`, for acting on it after the TUI exits).
    pub fn conversation_at(&self, path: &Path) -> Option<Conversation> {
        self.conversation_by_path(path)
            .map(|(_, conversation)| conversation.clone())
    }

    /// Whether `doc` of the generation shown is a card hidden by a delete
    /// made here (tests).
    #[cfg(test)]
    pub(super) fn card_removed(&self, doc: u32) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| self.cards.removed.contains(&snapshot.key_hash(doc)))
    }

    /// The overlays (tests): deleted keys and renamed paths.
    #[cfg(test)]
    pub(super) fn overlays(&self) -> (&HashSet<u64>, &HashMap<PathBuf, Option<String>>) {
        (&self.cards.removed, &self.cards.renamed)
    }
}

/// `CardRow::key_hash` of the session stored at `path`.
fn path_key(path: &Path) -> u64 {
    crate::store::generation::key_hash(&path.to_string_lossy())
}
