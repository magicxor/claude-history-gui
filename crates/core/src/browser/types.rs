//! Value types of the list controller (upstream `tui/app/types.rs`, minus the
//! terminal-only `Action`, `DialogMode`, `AppMode` and `ViewState`, which a
//! GUI models itself).

use crate::error::AppError;
use crate::history::Source;
use crate::search::mode::TuiSearchMode;
use crate::search::query::ParsedQuery;
use crate::semantic::types::{SemanticExplanation, SemanticScoreBreakdown};
use std::fmt;
use std::path::PathBuf;

/// Search mode of the conversation list.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ListSearchMode {
    #[default]
    Lexical,
    Semantic,
}

impl ListSearchMode {
    /// Short label shown in the search bar: `"lex"` / `"sem"`.
    pub fn label(self) -> &'static str {
        match self {
            ListSearchMode::Lexical => "lex",
            ListSearchMode::Semantic => "sem",
        }
    }

    /// The other mode.
    pub fn toggled(self) -> Self {
        match self {
            ListSearchMode::Lexical => ListSearchMode::Semantic,
            ListSearchMode::Semantic => ListSearchMode::Lexical,
        }
    }
}

impl From<TuiSearchMode> for ListSearchMode {
    fn from(mode: TuiSearchMode) -> Self {
        match mode {
            TuiSearchMode::Lexical => ListSearchMode::Lexical,
            TuiSearchMode::Semantic => ListSearchMode::Semantic,
        }
    }
}

impl From<ListSearchMode> for TuiSearchMode {
    fn from(mode: ListSearchMode) -> Self {
        match mode {
            ListSearchMode::Lexical => TuiSearchMode::Lexical,
            ListSearchMode::Semantic => TuiSearchMode::Semantic,
        }
    }
}

/// Whether `query` contains at least one quoted literal.
///
/// Replaces upstream's `list_lines_per_item` (3 or 4 terminal lines per
/// row): rows reserve the extra "literal context" line exactly when this is
/// true.
pub fn query_has_literals(query: &str) -> bool {
    !ParsedQuery::parse(query).literals().is_empty()
}

/// Progress of the semantic worker, as reported to the list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SemanticProgress {
    #[default]
    Idle,
    InitializingModel,
    CacheReady,
    Embedding {
        completed: usize,
        total: usize,
    },
    Ranking,
    Complete,
    EmptyCorpus,
    Failed,
    /// The semantic worker is suspended ([`super::Browser::suspend_semantic`])
    /// while the semantic cache is generated or cleared.
    Suspended,
}

/// Scores and explanation of one semantic hit.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticResultMetadata {
    pub score_breakdown: SemanticScoreBreakdown,
    pub explanation: SemanticExplanation,
}

/// Loading state of the list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadingState {
    /// Still loading conversations; `loaded` counts every conversation
    /// received so far (including excluded ones).
    Loading { loaded: usize },
    /// All conversations loaded and ready.
    Ready,
}

/// Result of [`super::Browser::escape`] (upstream: Esc in the list).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EscapeOutcome {
    /// The query was non-empty and has been cleared.
    Cleared,
    /// The query was already empty: upstream quits the app.
    Quit,
}

/// Outcome of the last query that was a session UUID (36 characters,
/// 8-4-4-4-12 hex groups).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UuidLookup {
    /// The session is the only row of the list.
    Found {
        conversation_index: usize,
        /// The session was not loaded yet and has been read from disk.
        loaded_from_disk: bool,
    },
    /// No loaded session has this id. The previous rows stay until the GUI
    /// has run [`crate::history::load_session_by_uuid`] in the background
    /// and passed the result to [`super::Browser::resolve_uuid_lookup`].
    Pending { uuid: String },
    /// No loaded session and no `<uuid>.jsonl` in any Claude project.
    /// Upstream silently kept the previous rows (quirk 6); core reports it so
    /// the GUI can say so. The previous rows are still kept.
    NotFound { uuid: String },
}

impl UuidLookup {
    /// `"Session not found: <uuid>"` for [`UuidLookup::NotFound`].
    pub fn status_message(&self) -> Option<String> {
        match self {
            UuidLookup::Found { .. } | UuidLookup::Pending { .. } => None,
            UuidLookup::NotFound { uuid } => Some(format!("Session not found: {uuid}")),
        }
    }
}

/// Status shown after a successful rename (upstream wording).
pub const RENAMED_STATUS: &str = "Session renamed";

/// Why a rename did not happen.
#[derive(Debug)]
pub enum RenameError {
    /// No row is selected, or the target is no longer in the list.
    NoTarget,
    /// The session re-parsed to nothing after the rename was written.
    BecameEmpty,
    /// Writing or re-parsing the session failed.
    Failed(AppError),
}

impl fmt::Display for RenameError {
    /// The exact upstream status messages.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenameError::NoTarget => f.write_str("Failed to rename: session not found"),
            RenameError::BecameEmpty => f.write_str("Failed to rename: conversation became empty"),
            RenameError::Failed(error) => write!(f, "Failed to rename: {error}"),
        }
    }
}

impl std::error::Error for RenameError {}

/// A session removed by [`super::Browser::delete_conversation`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletedSession {
    pub path: PathBuf,
    pub source: Source,
    /// Transcript files removed: every copy of a Claude session across
    /// projects (cross-project forks), else 1.
    pub files_removed: usize,
}

/// Why a delete did not happen.
#[derive(Debug)]
pub enum DeleteError {
    /// No row is selected, or the target is no longer in the list.
    NoTarget,
    /// Single-file mode has no list; upstream disabled delete there.
    SingleFileMode,
    /// The files could not be removed (upstream only logged this).
    Failed { path: PathBuf, error: AppError },
}

impl fmt::Display for DeleteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeleteError::NoTarget => f.write_str("Failed to delete session: session not found"),
            DeleteError::SingleFileMode => {
                f.write_str("Deleting is not available for a single opened file")
            }
            // Upstream's debug-log line.
            DeleteError::Failed { path, error } => {
                write!(f, "Failed to delete session {}: {}", path.display(), error)
            }
        }
    }
}

impl std::error::Error for DeleteError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_search_mode_labels_and_conversions() {
        assert_eq!(ListSearchMode::Lexical.label(), "lex");
        assert_eq!(ListSearchMode::Semantic.label(), "sem");
        assert_eq!(ListSearchMode::Lexical.toggled(), ListSearchMode::Semantic);
        assert_eq!(ListSearchMode::Semantic.toggled(), ListSearchMode::Lexical);
        assert_eq!(
            ListSearchMode::from(TuiSearchMode::Semantic),
            ListSearchMode::Semantic
        );
        assert_eq!(
            TuiSearchMode::from(ListSearchMode::Lexical),
            TuiSearchMode::Lexical
        );
    }

    #[test]
    fn literal_queries_reserve_the_context_line() {
        assert!(!query_has_literals(""));
        assert!(!query_has_literals("plain words"));
        assert!(!query_has_literals("snake_case identifier"));
        assert!(query_has_literals("\"exact\""));
        assert!(query_has_literals("alpha \"beta"));
        assert!(!query_has_literals("\"  \""));
    }

    #[test]
    fn status_messages_keep_upstream_wording() {
        assert_eq!(
            RenameError::BecameEmpty.to_string(),
            "Failed to rename: conversation became empty"
        );
        assert_eq!(
            RenameError::Failed(AppError::SessionNotFound("x".to_string())).to_string(),
            "Failed to rename: Session not found: x"
        );
        assert_eq!(
            DeleteError::Failed {
                path: PathBuf::from("a.jsonl"),
                error: AppError::SessionNotFound("a".to_string()),
            }
            .to_string(),
            "Failed to delete session a.jsonl: Session not found: a"
        );
        assert_eq!(
            UuidLookup::NotFound {
                uuid: "u".to_string()
            }
            .status_message()
            .as_deref(),
            Some("Session not found: u")
        );
        assert_eq!(
            UuidLookup::Found {
                conversation_index: 0,
                loaded_from_disk: false
            }
            .status_message(),
            None
        );
    }
}
