//! Snapshot cards and the generation's search index (upstream `tui/app.rs`
//! `show_snapshot` to `drop_search_index`, 3708bb6 and ac7f591).
//!
//! While loading, the rows are cards of the store's current generation,
//! materialized on first use: the newest [`SNAPSHOT_ROWS`] in scope for the
//! browse list, and the cards a query on the generation's index returns.
//! With that index ([`Browser::use_search_index`]) lexical queries run
//! before the load finishes ([`super::Readiness::lexical`]). The loaded
//! corpus keeps using it through each card's position in the corpus until a
//! rename or a refresh drops it, or the generation written from the loaded
//! corpus replaces it ([`super::HistoryLoader`]); without an index, queries
//! run over the loaded corpus in memory.
//!
//! # Differences from upstream
//!
//! * **Off the UI thread.** Opening an index (it normalizes every card's
//!   title and summary) and mapping its cards to the loaded conversations
//!   are O(corpus): [`super::HistoryLoader`] opens the index on an
//!   `index-open` thread, the corpus preparation of the load locates the
//!   cards in the loaded corpus, and the `snapshot-writer` thread opens and
//!   locates the generation it wrote. Upstream did all three on the TUI
//!   thread. The cards a query returns are built by the lexical worker
//!   ([`super::search_state::IndexScope::cards`]); the browser only appends
//!   them.
//! * **Stale index after the load.** Upstream keeps the startup index
//!   until its detached builder's generation is swapped in. Here the
//!   builder dies with the loader, so a quit before it ends would repeat
//!   the stale index every session: when the cards do not cover the loaded
//!   corpus (a conversation without a card, or a different timestamp), the
//!   preparation drops the index and queries run in memory.
//! * **Edits on cards.** A rename or delete made on a card while loading
//!   (core allows both and replays them on the loaded corpus, K1; the GUI
//!   offers neither while loading, like upstream's TUI) stops the index
//!   until the load ends, and a deleted card stays out of the list.
//! * **The swap.** The generation written from the loaded corpus is swapped
//!   in only while the corpus is still the one it was written from (its
//!   cards were located in it); otherwise the index in use, which then
//!   misses conversations, is dropped and queries run in memory.
//! * **A delete after loading** shifts the documents' rows in place
//!   ([`Browser::remove_doc_rows`], O(cards), no path lookup), so the index
//!   keeps answering with the right conversations. Upstream located every
//!   card in the corpus again on the TUI thread (`locate_docs` from
//!   `send_search_data`).

use super::index::{CardScope, ScopeFilter, ScopeIndex, remove_positions_in_place};
use super::search_state::IndexScope;
use super::{Browser, CorpusIndex, CorpusSnapshot, ListSearchMode, SNAPSHOT_ROWS};
use crate::history::Conversation;
use crate::search::LexicalEvidence;
use crate::search::query::ParsedQuery;
use crate::store::{SearchIndex, Snapshot};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The doc of a row that is not a card (a session a UUID jump loaded while
/// the cards were shown).
pub(super) const NO_DOC: u32 = u32::MAX;

/// The materialized cards while snapshot cards are the rows.
#[derive(Default)]
pub(super) struct CardRows {
    /// The doc of each row of the browser's conversations.
    row_docs: Vec<u32>,
    /// Per doc: inside the snapshot's time filter, its project in scope
    /// (exclusions, workspace) and not deleted.
    scope: Vec<bool>,
    /// Docs whose card was deleted while loading.
    removed: Vec<u32>,
    /// Per doc, whether it is materialized: the hint the lexical worker
    /// reads to build only the cards the browser lacks.
    materialized: Option<Arc<[AtomicBool]>>,
    /// A card was renamed or deleted: no index is used until the load ends.
    edited: bool,
}

/// A generation written from the loaded corpus, opened and located off the
/// UI thread, for [`Browser::swap_search_index`].
pub(super) struct IndexSwap {
    pub(super) index: Arc<SearchIndex>,
    /// For each card, its position in `corpus`.
    pub(super) located: Vec<Option<usize>>,
    /// The corpus the generation was written from.
    pub(super) corpus: CorpusSnapshot,
}

impl Browser {
    /// Shows the newest cards of `snapshot` until loading finishes (call it
    /// on a loading browser before the load starts). Loaded batches are held
    /// back meanwhile and swapped in when the load is installed, keeping the
    /// selected session. No-op on a browser that is not loading or shows a
    /// single file.
    ///
    /// At most [`SNAPSHOT_ROWS`] cards are materialized, in the current
    /// scope (exclusions, workspace) and the snapshot's time filter; the
    /// first row is selected and [`super::Readiness::list`] is set. Lexical
    /// queries wait for the load unless the snapshot's index is opened
    /// ([`Browser::use_search_index`]).
    pub fn show_snapshot(&mut self, snapshot: Snapshot) {
        if !self.is_loading() || self.single_file_mode {
            return;
        }
        if self.pending.is_none() {
            // Rows that arrived before the cards (normally none) wait with
            // the rest of the load.
            self.pending = Some(std::mem::take(&mut self.conversations));
        }
        self.drop_search_index();
        self.snapshot = Some(snapshot);
        self.reset_cards();
        self.show_snapshot_cards();
    }

    /// Start over with no card materialized.
    fn reset_cards(&mut self) {
        let docs = self.snapshot.as_ref().map_or(0, Snapshot::len);
        self.conversations = Arc::new(Vec::new());
        let mut index = CorpusIndex::default();
        index.scope = ScopeIndex::all(0, self.scope_settings());
        self.index = index;
        self.doc_rows = vec![None; docs];
        self.cards = CardRows {
            materialized: Some((0..docs).map(|_| AtomicBool::new(false)).collect()),
            ..CardRows::default()
        };
        self.cards.scope = self.card_scope();
        self.lexical_evidence.clear();
        self.uuid_lookup = None;
    }

    /// Every card's scope under the current settings (O(cards), memory
    /// only; each project is decided once).
    fn card_scope(&self) -> Vec<bool> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let scope = CardScope::new(&self.excluded_projects, self.effective_workspace());
        let mut docs = snapshot.scope(|project| scope.project_visible(project));
        for &doc in &self.cards.removed {
            docs[doc as usize] = false;
        }
        docs
    }

    /// The browse list over the cards: the newest [`SNAPSHOT_ROWS`] in scope.
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
        let filtered = docs
            .into_iter()
            .map(|doc| self.card_row(doc, None))
            .collect();
        self.uuid_lookup = None;
        self.lexical_evidence.clear();
        self.apply_filtered(filtered);
        self.readiness.list = true;
    }

    /// The scope or the exclusions changed while cards are shown: every
    /// card's scope is decided again (from its generation's projects, no
    /// per-card work beyond a flag), the materialized cards keep their rows,
    /// and the browse list or the lexical query is applied again.
    pub(super) fn rescope_cards(&mut self) {
        self.cards.scope = self.card_scope();
        let settings = self.scope_settings();
        let mut filter = ScopeFilter::new(&settings.excluded, settings.workspace.as_ref());
        let member = self
            .cards
            .row_docs
            .iter()
            .zip(self.conversations.iter())
            .map(|(&doc, conversation)| match doc {
                NO_DOC => filter.passes(conversation),
                doc => self.cards.scope[doc as usize],
            })
            .collect();
        self.index.scope = ScopeIndex::from_members(member, settings);
        self.send_index_scope();
        if self.readiness.lexical && self.list_search_mode == ListSearchMode::Lexical {
            self.update_filter();
        } else {
            self.show_snapshot_cards();
        }
    }

    /// Row of snapshot card `doc`, materializing it on first use from
    /// `card` (built by the lexical worker), else here.
    fn card_row(&mut self, doc: u32, card: Option<Arc<Conversation>>) -> usize {
        if let Some(row) = self.doc_rows[doc as usize] {
            return row;
        }
        let mut card = match card {
            Some(card) => card,
            None => Arc::new(
                self.snapshot
                    .as_ref()
                    .expect("cards come from a snapshot")
                    .card(doc),
            ),
        };
        let row = self.conversations.len();
        if card.index != row {
            Arc::make_mut(&mut card).index = row;
        }
        self.corpus_mut().push(card);
        self.index.push_in_scope(&self.conversations, row);
        self.doc_rows[doc as usize] = Some(row);
        self.cards.row_docs.push(doc);
        if let Some(flag) = self
            .cards
            .materialized
            .as_ref()
            .and_then(|flags| flags.get(doc as usize))
        {
            flag.store(true, Ordering::Relaxed);
        }
        row
    }

    /// A conversation that is not a card was appended while cards are shown
    /// (a session a UUID jump loaded).
    pub(super) fn pushed_non_card(&mut self) {
        if self.pending.is_some() {
            self.cards.row_docs.push(NO_DOC);
        }
    }

    /// Conversation rows of index docs `docs`, in order. While cards are
    /// shown every doc becomes a card row (`cards` holds those the worker
    /// built); afterwards docs without a loaded conversation are dropped.
    pub(super) fn rows_of_docs(
        &mut self,
        docs: &[usize],
        cards: Vec<(u32, Arc<Conversation>)>,
    ) -> Vec<usize> {
        if self.pending.is_some() {
            let mut cards: HashMap<u32, Arc<Conversation>> = cards.into_iter().collect();
            let docs_len = self.doc_rows.len();
            docs.iter()
                .filter(|&&doc| doc < docs_len)
                .map(|&doc| {
                    let doc = doc as u32;
                    let card = cards.remove(&doc);
                    self.card_row(doc, card)
                })
                .collect()
        } else {
            let rows = docs
                .iter()
                .filter_map(|&doc| self.doc_rows.get(doc).copied().flatten())
                .collect::<Vec<_>>();
            self.filter_indices(rows)
        }
    }

    /// Evidence keyed by index doc, keyed by row instead.
    pub(super) fn evidence_of_docs(
        &self,
        evidence: HashMap<usize, LexicalEvidence>,
    ) -> HashMap<usize, LexicalEvidence> {
        evidence
            .into_iter()
            .filter_map(|(doc, evidence)| {
                Some((self.doc_rows.get(doc).copied().flatten()?, evidence))
            })
            .collect()
    }

    /// Search `index`, opened off the UI thread (see
    /// [`super::HistoryLoader::open_snapshot`]), while the cards of its
    /// generation are shown, so lexical queries run before loading finishes
    /// (upstream `open_search_index`); a lexical query typed meanwhile runs
    /// now. Returns whether it was taken: not once loading finished, for
    /// another generation, while an index is in use, or after a card was
    /// renamed or deleted.
    pub fn use_search_index(&mut self, index: Arc<SearchIndex>) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        if self.pending.is_none()
            || self.cards.edited
            || self.search_index.is_some()
            || index.cards().generation_name() != snapshot.generation_name()
            || index.cards().len() != snapshot.len()
        {
            return false;
        }
        self.search_index = Some(index);
        self.send_index_scope();
        self.readiness.lexical = true;
        if self.list_search_mode == ListSearchMode::Lexical && !self.query.trim().is_empty() {
            self.dispatch_search();
        }
        true
    }

    /// [`Browser::use_search_index`] with the index of the shown snapshot,
    /// opened here (upstream `open_search_index`; O(cards): a GUI uses
    /// [`super::HistoryLoader`], which opens it off the UI thread). Returns
    /// whether one was opened and taken.
    pub fn open_search_index(&mut self) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        let Some(index) = SearchIndex::open(snapshot.clone()) else {
            return false;
        };
        self.use_search_index(Arc::new(index))
    }

    /// Generation name of the index lexical queries run on, if any.
    pub fn search_index_generation(&self) -> Option<&str> {
        self.search_index
            .as_ref()
            .map(|index| index.cards().generation_name())
    }

    /// Switches to the index of a newer generation written from the loaded
    /// corpus, and reruns a lexical query on it keeping the selection
    /// (upstream `swap_search_index`). Only applies once loading has
    /// finished and while the corpus is the one the generation was written
    /// from; otherwise an index of another generation is dropped (it lacks
    /// what that generation added) and a lexical query runs in memory.
    /// Returns whether the swap applied.
    pub(super) fn swap_search_index(&mut self, swap: IndexSwap) -> bool {
        if self.is_loading()
            || self.single_file_mode
            || self.search_index_generation() == Some(swap.index.cards().generation_name())
        {
            return false;
        }
        let applies = Arc::ptr_eq(&self.conversations, &swap.corpus)
            && swap.located.len() == swap.index.cards().len();
        drop(swap.corpus);
        if !applies {
            if self.search_index.is_some() {
                self.drop_search_index();
                self.rerun_lexical_query();
            }
            return false;
        }
        self.search_index = Some(swap.index);
        self.doc_rows = swap.located;
        self.send_index_scope();
        self.rerun_lexical_query();
        true
    }

    /// Search a lexical query again (on the index now in use, or in
    /// memory), keeping the selected session.
    fn rerun_lexical_query(&mut self) {
        let query = self.query.trim().to_string();
        if self.list_search_mode == ListSearchMode::Lexical
            && !ParsedQuery::parse(&query).is_effectively_empty()
            && !crate::search::is_uuid(&query)
        {
            self.selection_anchor = self.selected_path();
            self.dispatch_lexical_search(query);
        }
    }

    /// Builds the index and the docs in scope that lexical queries carry:
    /// the cards' scope while cards are shown, else the cards whose project
    /// is in scope and that have a loaded conversation.
    pub(super) fn send_index_scope(&mut self) {
        let Some(index) = &self.search_index else {
            self.index_scope = None;
            return;
        };
        let (docs, cards) = if self.pending.is_some() {
            (self.cards.scope.clone(), self.cards.materialized.clone())
        } else {
            let scope = CardScope::new(&self.excluded_projects, self.effective_workspace());
            let mut docs = index
                .cards()
                .scope(|project| scope.project_visible(project));
            for (doc, row) in docs.iter_mut().zip(&self.doc_rows) {
                *doc &= row.is_some();
            }
            (docs, None)
        };
        self.index_scope = Some(IndexScope {
            index: index.clone(),
            docs: Arc::new(docs),
            cards,
        });
    }

    /// Stops searching the index (upstream `drop_search_index`). Afterwards
    /// lexical queries run over the loaded conversations; while loading they
    /// wait for the load again. An index search in flight is abandoned.
    pub(super) fn drop_search_index(&mut self) {
        if self.search_index.take().is_none() {
            return;
        }
        self.index_scope = None;
        if self.pending.is_some() {
            self.readiness.lexical = false;
        } else {
            self.doc_rows.clear();
        }
        if self.search_in_flight && self.list_search_mode == ListSearchMode::Lexical {
            self.search_generation += 1;
            self.search_in_flight = false;
        }
        self.search_page_shown = false;
    }

    /// A card was renamed or deleted while loading: the index is not used
    /// until the load ends.
    pub(super) fn cards_edited(&mut self) {
        if self.pending.is_some() {
            self.cards.edited = true;
            self.drop_search_index();
        }
    }

    /// The rows at `removed` (ascending positions) left the corpus: the
    /// cards' rows follow. A deleted card stays out of the list.
    pub(super) fn remove_doc_rows(&mut self, removed: &[usize]) {
        if self.doc_rows.is_empty() {
            return;
        }
        let shifted = |row: usize| row - removed.partition_point(|&gone| gone < row);
        let cards = self.pending.is_some();
        for (doc, slot) in self.doc_rows.iter_mut().enumerate() {
            let Some(row) = *slot else {
                continue;
            };
            if removed.binary_search(&row).is_ok() {
                *slot = None;
                if cards {
                    self.cards.removed.push(doc as u32);
                    if let Some(in_scope) = self.cards.scope.get_mut(doc) {
                        *in_scope = false;
                    }
                }
            } else {
                *slot = Some(shifted(row));
            }
        }
        if cards {
            remove_positions_in_place(&mut self.cards.row_docs, removed);
            self.cards_edited();
        } else {
            self.send_index_scope();
        }
    }

    /// The docs of the rows shown while an index answers a lexical query
    /// over the cards, to keep them when the loaded corpus replaces the
    /// cards; `None` otherwise (the browse list is rebuilt).
    pub(super) fn shown_docs(&self) -> Option<Vec<u32>> {
        let query = self.query.trim();
        (self.pending.is_some()
            && self.search_index.is_some()
            && self.list_search_mode == ListSearchMode::Lexical
            && !ParsedQuery::parse(query).is_effectively_empty()
            && !crate::search::is_uuid(query))
        .then(|| {
            self.filtered
                .iter()
                .filter_map(|&row| self.cards.row_docs.get(row).copied())
                .filter(|&doc| doc != NO_DOC)
                .collect()
        })
    }

    /// The evidence shown on card rows, by doc.
    pub(super) fn card_evidence(&mut self) -> Vec<(u32, LexicalEvidence)> {
        std::mem::take(&mut self.lexical_evidence)
            .into_iter()
            .filter_map(|(row, evidence)| {
                let doc = *self.cards.row_docs.get(row)?;
                (doc != NO_DOC).then_some((doc, evidence))
            })
            .collect()
    }

    /// The cards are gone (the loaded corpus replaced them).
    pub(super) fn clear_cards(&mut self) {
        self.cards = CardRows::default();
    }
}
