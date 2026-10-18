//! Rename and delete (upstream `tui/app/dialog_state.rs::submit_rename` and
//! the delete branch of `tui/runtime.rs`).

use super::{Browser, DeleteError, DeletedSession, ListSearchMode, RenameError};
use crate::history::{self, Conversation, Source};
use std::path::Path;
use std::sync::Arc;

impl Browser {
    /// Initial text of a rename dialog for the selected session: its custom
    /// title (not the generated summary), or empty.
    pub fn rename_prefill(&self) -> Option<String> {
        self.selected_conversation()
            .map(|conversation| conversation.custom_title.clone().unwrap_or_default())
    }

    /// Rename the selected session. See [`Browser::rename_conversation`].
    pub fn rename_selected(&mut self, input: &str) -> Result<(), RenameError> {
        let path = self.selected_path().ok_or(RenameError::NoTarget)?;
        self.rename_conversation(&path, input)
    }

    /// Rename the loaded session at `path` (capture the path when the
    /// rename dialog opens; addendum A1).
    ///
    /// Synchronous convenience: [`history::rename_session`] (writes the
    /// rename in the session's own format and re-parses the file; the
    /// preview honors `show_last`) on the caller's thread, then
    /// [`Browser::apply_renamed`]. A GUI runs the first half in the
    /// background. Show [`super::RENAMED_STATUS`] on success, or the
    /// error's `Display` text.
    pub fn rename_conversation(&mut self, path: &Path, input: &str) -> Result<(), RenameError> {
        let (_, conversation) = self
            .conversation_by_path(path)
            .ok_or(RenameError::NoTarget)?;
        let source = conversation.source;
        let path = conversation.path.clone();
        let renamed = history::rename_session(&self.env, &path, source, input, self.show_last)?;
        self.apply_renamed(&path, renamed)
    }

    /// Put a session re-parsed by [`history::rename_session`] in place of
    /// the loaded one at `path` (K3).
    ///
    /// The conversation keeps its index, project name and project path and
    /// is not moved (addendum B3); only its corpus element and its search
    /// data change. Semantic metadata and lexical evidence are cleared.
    /// The query is re-applied (a lexical query in the background, keeping
    /// the rows) and the renamed session stays selected when it is shown.
    /// [`RenameError::NoTarget`] when `path` is no longer loaded (deleted
    /// or refreshed away meanwhile).
    ///
    /// The path is found through the path index and the browse rows come
    /// from the cached scope (CCR-2): the caller's thread does no work per
    /// conversation besides copying pointers and row numbers.
    pub fn apply_renamed(&mut self, path: &Path, renamed: Conversation) -> Result<(), RenameError> {
        let index = self.position_of_path(path).ok_or(RenameError::NoTarget)?;
        let previous = &self.conversations[index];
        let mut renamed = renamed;
        renamed.index = index;
        renamed.project_name = previous.project_name.clone();
        renamed.project_path = previous.project_path.clone();
        self.replace_conversation_at(index, Arc::new(renamed));
        if self.is_loading() {
            // No search data or query results yet: the row just shows the
            // new title.
            return Ok(());
        }
        self.corpus_changed();

        let path = path.to_path_buf();
        self.selection_anchor = Some(path.clone());
        self.update_filter();
        let lexical_pending =
            self.search_in_flight && self.list_search_mode == ListSearchMode::Lexical;
        if !lexical_pending {
            self.selection_anchor = None;
        }
        if let Some(row) = self.row_of_path(&self.filtered, &path) {
            self.selected = Some(row);
        }
        Ok(())
    }

    /// Delete the selected session. See [`Browser::delete_conversation`].
    pub fn delete_selected(&mut self) -> Result<DeletedSession, DeleteError> {
        if self.single_file_mode {
            return Err(DeleteError::SingleFileMode);
        }
        let path = self.selected_path().ok_or(DeleteError::NoTarget)?;
        self.delete_conversation(&path)
    }

    /// Delete the loaded session at `path` from disk and from the list
    /// (capture the path when the confirmation opens; addendum A1).
    ///
    /// The implementation follows that conversation's own source (A2):
    ///
    /// * Claude: every `<uuid>.jsonl` with the file's stem in **all** Claude
    ///   project directories (cross-project fork copies) and each
    ///   `<project>/<uuid>/` session directory;
    /// * Pi: the file, if it parses as a Pi session;
    /// * OMP: the file, if it parses as an OMP session, and its artifact
    ///   directory.
    ///
    /// On success the row is removed (other copies of the same session
    /// already in the list stay until a refresh, as upstream). On failure
    /// nothing changes, the upstream debug-log line is written, and the
    /// error is returned so the GUI can show it.
    ///
    /// Synchronous convenience; a GUI uses
    /// [`crate::maintenance::plan_session_delete`] and
    /// [`crate::maintenance::execute_session_delete`] in the background and
    /// then [`Browser::remove_paths`].
    pub fn delete_conversation(&mut self, path: &Path) -> Result<DeletedSession, DeleteError> {
        if self.single_file_mode {
            return Err(DeleteError::SingleFileMode);
        }
        let (idx, source) = self
            .position_of_path(path)
            .map(|idx| (idx, self.conversations[idx].source))
            .ok_or(DeleteError::NoTarget)?;
        let result = match source {
            Source::Claude => {
                let uuid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                history::delete_session_by_uuid(&self.env, uuid)
            }
            Source::Pi => history::pi_loader::delete_session(path).map(|()| 1),
            Source::Omp => history::omp_loader::delete_session(path).map(|()| 1),
        };
        match result {
            Ok(files_removed) => {
                self.remove_conversation(idx);
                Ok(DeletedSession {
                    path: path.to_path_buf(),
                    source,
                    files_removed,
                })
            }
            Err(error) => {
                let error = DeleteError::Failed {
                    path: path.to_path_buf(),
                    error,
                };
                if let Some(log_path) = self.env.debug_log_file() {
                    let _ = crate::debug_log::log_debug(&log_path, &error.to_string());
                }
                Err(error)
            }
        }
    }
}
