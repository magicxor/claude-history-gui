//! Filtering and selection (upstream `tui/app/list_state.rs`).

use super::Browser;
use super::index::{ScopeFilter, ScopeIndex, ScopeSettings};
use crate::env::WorkspaceScope;
use crate::history::{Conversation, Source};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

impl Browser {
    /// The rows of `indices` that pass the exclusions and the scope, in the
    /// given order. Uses the cached scope (no per-row work besides a flag)
    /// when it matches the current corpus and settings.
    pub(super) fn filter_indices<I>(&self, indices: I) -> Vec<usize>
    where
        I: IntoIterator<Item = usize>,
    {
        if self.scope_is_current() {
            let scope = &self.index.scope;
            return indices
                .into_iter()
                .filter(|&index| scope.contains(index))
                .collect();
        }
        filter_conversation_indices(
            indices,
            &self.conversations,
            &self.excluded_projects,
            self.workspace_filter,
            self.workspace.as_ref(),
        )
    }

    /// The workspace the scope filters by: the workspace while the scope
    /// is on.
    pub(super) fn effective_workspace(&self) -> Option<&WorkspaceScope> {
        self.workspace.as_ref().filter(|_| self.workspace_filter)
    }

    /// The current scope settings (for a corpus prepared elsewhere).
    pub(super) fn scope_settings(&self) -> ScopeSettings {
        ScopeSettings {
            excluded: self.excluded_projects.clone(),
            workspace: self.effective_workspace().cloned(),
        }
    }

    /// Whether the cached scope belongs to the current corpus and settings.
    pub(super) fn scope_is_current(&self) -> bool {
        self.index.scope.is_current(
            &self.excluded_projects,
            self.effective_workspace(),
            self.conversations.len(),
        )
    }

    /// Recompute the cached scope when the corpus or the settings changed
    /// in a way an incremental update could not follow (O(n)).
    pub(super) fn ensure_scope(&mut self) {
        if !self.scope_is_current() {
            self.index.scope = ScopeIndex::compute(&self.conversations, self.scope_settings());
        }
    }

    /// Every conversation in scope, in corpus order.
    pub(super) fn scope_rows(&mut self) -> Vec<usize> {
        self.ensure_scope();
        self.index.scope.rows().as_ref().clone()
    }

    /// Position in `rows` of the first row showing `path`.
    pub(super) fn row_of_path(&self, rows: &[usize], path: &Path) -> Option<usize> {
        let mut positions = self.index.path_positions(&self.conversations, path);
        match (positions.next(), positions.next()) {
            (None, _) => None,
            (Some(only), None) => rows.iter().position(|&index| index == only),
            (Some(first), Some(second)) => {
                let all = [first, second]
                    .into_iter()
                    .chain(positions)
                    .collect::<Vec<_>>();
                rows.iter().position(|index| all.contains(index))
            }
        }
    }

    pub(super) fn apply_filtered(&mut self, filtered: Vec<usize>) {
        self.filtered = filtered;
        self.selected = self
            .selection_anchor
            .take()
            .and_then(|path| self.row_of_path(&self.filtered, &path));
        if self.selected.is_none() && !self.filtered.is_empty() {
            self.selected = Some(0);
        }
    }

    pub(super) fn clear_selection_anchor(&mut self) {
        self.selection_anchor = None;
    }

    /// Select row `filtered_pos` (e.g. a clicked row). Returns false when
    /// there is no such row.
    pub fn select(&mut self, filtered_pos: usize) -> bool {
        if filtered_pos < self.filtered.len() {
            self.clear_selection_anchor();
            self.selected = Some(filtered_pos);
            true
        } else {
            false
        }
    }

    /// Move the selection up one row (stops at the first row).
    pub fn select_prev(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected
            && selected > 0
        {
            self.selected = Some(selected - 1);
        }
    }

    /// Move the selection down one row (stops at the last row).
    pub fn select_next(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected
            && selected + 1 < self.filtered.len()
        {
            self.selected = Some(selected + 1);
        }
    }

    pub fn select_first(&mut self) {
        self.clear_selection_anchor();
        if !self.filtered.is_empty() {
            self.selected = Some(0);
        }
    }

    pub fn select_last(&mut self) {
        self.clear_selection_anchor();
        if !self.filtered.is_empty() {
            self.selected = Some(self.filtered.len() - 1);
        }
    }

    /// Move the selection up by 10 rows (upstream PageUp).
    pub fn select_page_up(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected {
            self.selected = Some(selected.saturating_sub(10));
        }
    }

    /// Move the selection down by 10 rows (upstream PageDown).
    pub fn select_page_down(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected {
            let new_selected = (selected + 10).min(self.filtered.len().saturating_sub(1));
            self.selected = Some(new_selected);
        }
    }

    /// Move the selection down by half of `viewport_rows` (upstream Ctrl+D).
    pub fn select_half_page_down(&mut self, viewport_rows: usize) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected {
            let half_page = viewport_rows / 2;
            let new_selected = (selected + half_page).min(self.filtered.len().saturating_sub(1));
            self.selected = Some(new_selected);
        }
    }

    /// Move the selection by `delta` rows, clamped (upstream mouse wheel
    /// passes the sign only: ±1 per wheel event).
    pub fn scroll_list(&mut self, delta: isize) {
        self.clear_selection_anchor();
        let Some(selected) = self.selected else {
            return;
        };

        let max = self.filtered.len().saturating_sub(1);
        let new_selected = if delta >= 0 {
            selected.saturating_add(delta as usize).min(max)
        } else {
            selected.saturating_sub(delta.unsigned_abs())
        };
        self.selected = Some(new_selected);
    }

    /// Path of the selected conversation.
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected
            .and_then(|sel| self.filtered.get(sel))
            .map(|&idx| self.conversations[idx].path.clone())
    }

    /// Source of the selected conversation.
    pub fn selected_source(&self) -> Option<Source> {
        self.selected_conversation_index()
            .map(|index| self.conversations[index].source)
    }

    /// Index (into [`Browser::conversations`]) of the selected conversation.
    pub fn selected_conversation_index(&self) -> Option<usize> {
        self.selected
            .and_then(|sel| self.filtered.get(sel))
            .copied()
    }

    /// The selected conversation.
    pub fn selected_conversation(&self) -> Option<&Conversation> {
        self.selected_conversation_index()
            .map(|index| self.conversations[index].as_ref())
    }

    /// Whether the loaded corpus mixes Claude, Pi and OMP sessions (rows
    /// then show a `CC`/`Pi`/`OMP` source label).
    pub fn has_multiple_sources(&self) -> bool {
        self.index.mixed_sources()
    }

    /// Remove the conversation at `conv_idx` from the corpus, the search
    /// data and the rows (upstream `remove_selected_from_list`, generalized
    /// to any row). See [`Browser::remove_paths`].
    pub(super) fn remove_conversation(&mut self, conv_idx: usize) {
        if conv_idx < self.conversations.len() {
            self.remove_indices(&[conv_idx]);
        }
    }
}

/// Exclusions first, then the workspace scope (upstream
/// `filter_conversation_indices`); see [`ScopeFilter`] for the rules.
pub(super) fn filter_conversation_indices<I>(
    indices: I,
    conversations: &[Arc<Conversation>],
    excluded_projects: &HashSet<String>,
    workspace_filter: bool,
    workspace: Option<&WorkspaceScope>,
) -> Vec<usize>
where
    I: IntoIterator<Item = usize>,
{
    let mut filter = ScopeFilter::new(excluded_projects, workspace.filter(|_| workspace_filter));
    indices
        .into_iter()
        .filter(|&idx| filter.passes(&conversations[idx]))
        .collect()
}
