//! List/search/semantic controller (upstream `tui/app.rs`,
//! `tui/app/{list_state,search_state,types}.rs`, the rename logic of
//! `tui/app/dialog_state.rs`, and the loader/refresh/delete semantics of
//! `tui/runtime.rs`), without any terminal dependency.
//!
//! A [`Browser`] owns the loaded conversations, the query, the filtered and
//! ranked row list, the selection, and two background workers:
//!
//! * a lexical search worker thread ("search-worker") that coalesces queued
//!   queries and answers with generation-tagged results;
//! * a lazily spawned semantic worker ([`crate::semantic_worker`]), with
//!   corpus/scope versioning, prewarming and a lexical fallback while the
//!   semantic answer is pending.
//!
//! Workers post their answers to channels; the GUI calls [`Browser::poll`]
//! to apply them. [`BrowserOptions::waker`] is called from the worker
//! threads after each answer, so a GUI can schedule that poll instead of
//! using a timer. [`HistoryLoader`] drives the streaming history loader into
//! a browser (initial load and buffered refresh).
//!
//! # Corpus storage
//!
//! Every conversation exists once, as an `Arc<Conversation>` in a
//! [`CorpusSnapshot`] shared by the browser, the lexical worker and the
//! semantic worker (its search data likewise, one `Arc` per conversation).
//! The lexical worker gets the snapshots with each query and releases them
//! before it answers, so while it is idle a rename, a remove or an append
//! edits the browser's vectors in place; otherwise an edit copies at most
//! the vectors of pointers (O(n) pointer copies). Positions by path and by
//! session id, the source mix and the rows in scope (exclusions and
//! workspace) are indexed and kept in step with the corpus, so a rename, a
//! delete, a UUID jump or a path lookup never scans
//! the corpus. The expensive part of a load or refresh (sort, dedupe by
//! canonical path, search data, that index and, for a refresh, where each
//! current row moves) runs in [`Browser::prepare_corpus`], off the UI thread
//! ([`HistoryLoader`] uses a `corpus-prepare` thread), and
//! [`Browser::install_prepared`] only swaps the prepared corpus in. Queries
//! never run synchronously on the caller's thread: a lexical query that has
//! to be re-applied (end of a load, a rename, a scope change) is sent to the
//! lexical worker while the current rows stay visible.
//!
//! # Differences from upstream
//!
//! Behavior is kept identical to the TUI except for these documented fixes
//! (`inventory-list-mode.md` section 12 and the addendum):
//!
//! * **Workspace (D2).** The workspace scope comes from
//!   [`BrowserOptions::workspace`] (a [`WorkspaceScope`]), never from the
//!   process working directory. Pi/OMP rows are compared with the workspace
//!   directory after canonicalization and Windows path normalization (W1).
//! * **Quirk 6.** A UUID query that matches nothing reports
//!   [`UuidLookup::NotFound`] through [`Browser::uuid_lookup`]. The previous
//!   rows stay visible, as upstream.
//! * **Quirk 9 / addendum C1.** Sessions re-parsed after a rename and
//!   sessions loaded from disk by a UUID jump honor
//!   [`BrowserOptions::show_last`] for their preview (upstream always used
//!   the first messages).
//! * **Quirk 14.** While loading, toggling the workspace scope or the search
//!   mode (and changing the workspace or exclusions) only re-applies the
//!   scope and exclusions to the loaded rows. The query is applied when
//!   loading finishes. Upstream filtered against the not-yet-built search
//!   index and showed an empty list until loading finished.
//! * **Quirk 16.** A refresh with a non-empty query keeps the previous rows
//!   (re-mapped to the refreshed corpus by path) until the new results
//!   arrive. Upstream cleared the list until the worker answered.
//! * **Quirk 7.** Delete failures are returned as [`DeleteError`] so the
//!   GUI can show them (upstream only wrote the debug-log line, which is
//!   still written).
//! * **Quirk 8.** The semantic error text is available from
//!   [`Browser::semantic_error`].
//! * **Addendum A1/A2.** Rename and delete can target a captured path
//!   ([`Browser::rename_conversation`], [`Browser::delete_conversation`]);
//!   the delete implementation is chosen from that conversation's own
//!   source, never from the live selection.
//! * **Addendum D1.** A single opened file that cannot be read, or that the
//!   viewer would render empty, keeps the reason in
//!   [`Browser::single_file_error`]. A readable file that is only left out
//!   of the list (for example a Warmup-only session) has no row but no
//!   error: it still opens in the viewer, as upstream.
//! * **Semantic availability.** With [`BrowserOptions::semantic_available`]
//!   off the browser is always lexical and never starts the semantic worker
//!   (upstream turned semantic search off only in single-file mode, which
//!   also forced lexical mode).
//! * **Failed initial load.** [`LoadEvent::LoadFailed`] leaves the browser
//!   ready, so [`HistoryLoader::start_refresh`] can retry (upstream exited).
//!
//! The other section-12 quirks are presentation or key handling and belong
//! to the GUI (1-5, 10-12, 15); 13 (Windows worktree names) is fixed in
//! [`crate::history::path`]; 17 (every new result set selects the first row)
//! is intended and kept.
//!
//! Single-file mode (upstream: a `FILE` argument) has no list, no semantic
//! search, and no delete.

use crate::display_format::{HighlightQuery, RowHeaderInput, RowPreviewInput};
use crate::env::{HistoryEnv, WorkspaceScope};
use crate::history::{Conversation, format_short_name_from_path, process_conversation_file};
use crate::search::{self, LexicalEvidence, SearchableConversation};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

mod corpus;
mod index;
mod list_state;
mod loader;
mod mutations;
mod search_state;
mod types;

pub use crate::semantic_worker::{SemanticEmbedderFactory, Waker};
pub use corpus::{CorpusSnapshot, PreparedCorpus};
pub use loader::{
    HistoryLoader, LoadEvent, LoaderOptions, LoaderPoll, REFRESHING_STATUS, RefreshPolicy,
};
pub use types::{
    DeleteError, DeletedSession, EscapeOutcome, ListSearchMode, LoadingState, RENAMED_STATUS,
    RenameError, SemanticProgress, SemanticResultMetadata, UuidLookup, query_has_literals,
};

use corpus::{CorpusEdits, SearchSnapshot};
use index::{CorpusIndex, ScopeSettings};
use search_state::{SearchCommand, SearchResponse, SemanticSearchState, spawn_search_worker};

/// Construction options of a [`Browser`].
#[derive(Clone)]
pub struct BrowserOptions {
    /// Environment: Claude projects root (UUID lookup, delete), semantic
    /// cache and model directories, debug log.
    pub env: Arc<HistoryEnv>,
    /// Row preview from the last messages (`true`, upstream default) or the
    /// first ones. Must match the loader's setting; used when core itself
    /// re-parses a session (rename, UUID jump).
    pub show_last: bool,
    /// Initial search mode (`[search].mode` / `[tui].semantic_search`).
    pub default_mode: ListSearchMode,
    /// Project names hidden from the list (`[tui].exclude_projects`).
    pub exclude_projects: Vec<String>,
    /// The workspace for the "project" scope. [`BrowserOptions::new`] takes
    /// it from [`HistoryEnv::workspace`] (a workspace that is not an
    /// absolute path is left out there; validate it with
    /// [`WorkspaceScope::new`] to report the error).
    pub workspace: Option<WorkspaceScope>,
    /// Start with the workspace scope on (upstream `--local`).
    pub workspace_filter: bool,
    /// Whether semantic search can be used (forced off in single-file mode).
    /// When false the browser starts in lexical mode whatever
    /// [`BrowserOptions::default_mode`] says, and never starts the semantic
    /// worker (so fastembed is never loaded or downloaded).
    pub semantic_available: bool,
    /// Called from worker threads after they post a result.
    pub waker: Option<Waker>,
    /// Embedder for the semantic worker; `None` loads fastembed from
    /// [`HistoryEnv::semantic_model_dir`].
    pub embedder_factory: Option<SemanticEmbedderFactory>,
}

impl BrowserOptions {
    /// Defaults: last-message previews, lexical mode, no exclusions, the
    /// env's workspace (scope off), semantic search available.
    pub fn new(env: Arc<HistoryEnv>) -> Self {
        let workspace = env.workspace().ok().flatten();
        Self {
            env,
            show_last: true,
            default_mode: ListSearchMode::Lexical,
            exclude_projects: Vec::new(),
            workspace,
            workspace_filter: false,
            semantic_available: true,
            waker: None,
            embedder_factory: None,
        }
    }

    pub fn with_show_last(mut self, show_last: bool) -> Self {
        self.show_last = show_last;
        self
    }

    pub fn with_default_mode(mut self, mode: ListSearchMode) -> Self {
        self.default_mode = mode;
        self
    }

    pub fn with_exclude_projects(mut self, projects: Vec<String>) -> Self {
        self.exclude_projects = projects;
        self
    }

    pub fn with_workspace(mut self, workspace: Option<WorkspaceScope>) -> Self {
        self.workspace = workspace;
        self
    }

    pub fn with_workspace_filter(mut self, enabled: bool) -> Self {
        self.workspace_filter = enabled;
        self
    }

    pub fn with_semantic_available(mut self, available: bool) -> Self {
        self.semantic_available = available;
        self
    }

    pub fn with_waker(mut self, waker: Waker) -> Self {
        self.waker = Some(waker);
        self
    }

    pub fn with_embedder_factory(mut self, factory: SemanticEmbedderFactory) -> Self {
        self.embedder_factory = Some(factory);
        self
    }
}

impl std::fmt::Debug for BrowserOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserOptions")
            .field("env", &self.env)
            .field("show_last", &self.show_last)
            .field("default_mode", &self.default_mode)
            .field("exclude_projects", &self.exclude_projects)
            .field("workspace", &self.workspace)
            .field("workspace_filter", &self.workspace_filter)
            .field("semantic_available", &self.semantic_available)
            .field("waker", &self.waker.is_some())
            .field("embedder_factory", &self.embedder_factory.is_some())
            .finish()
    }
}

/// The conversation list controller. See the module documentation.
pub struct Browser {
    env: Arc<HistoryEnv>,
    show_last: bool,
    waker: Option<Waker>,
    embedder_factory: Option<SemanticEmbedderFactory>,
    /// All loaded conversations, shared with the lexical and semantic workers
    conversations: CorpusSnapshot,
    /// Precomputed search data, parallel to `conversations` once built
    /// (empty while loading and in single-file mode); shared with the
    /// lexical worker
    searchable: SearchSnapshot,
    /// Positions by path and session id, source counts and the rows in
    /// scope, kept in step with `conversations` (CCR-1, CCR-2)
    index: CorpusIndex,
    /// Version of the corpus as seen by the semantic worker
    semantic_corpus_version: u64,
    /// Version of the semantic scope snapshot
    semantic_scope_version: u64,
    /// Last corpus version sent to the semantic worker
    semantic_sent_corpus_version: u64,
    /// Last scope signature sent to the semantic worker
    semantic_sent_scope_signature: Option<(u64, Arc<Vec<usize>>)>,
    /// Indices into conversations, sorted by current score
    filtered: Vec<usize>,
    /// Currently selected index into filtered (None if no results)
    selected: Option<usize>,
    /// Session path to restore after replacing and re-filtering the corpus
    selection_anchor: Option<PathBuf>,
    /// Current search query
    query: String,
    /// Loading state
    loading_state: LoadingState,
    /// Whether the browser shows a single opened file (no list)
    single_file_mode: bool,
    /// Why the single opened file could not be parsed, if it could not
    single_file_error: Option<String>,
    /// Whether the workspace scope is active
    workspace_filter: bool,
    /// The workspace for the scope filter (upstream: the process cwd)
    workspace: Option<WorkspaceScope>,
    /// Exact project names hidden from list-mode display
    excluded_projects: HashSet<String>,
    /// Channel to send commands to the background search worker
    search_tx: mpsc::Sender<SearchCommand>,
    /// Channel to receive results from the background search worker
    search_rx: mpsc::Receiver<SearchResponse>,
    /// Monotonic generation counter for search requests
    search_generation: u64,
    /// Whether a search is currently in-flight on the worker thread
    search_in_flight: bool,
    /// Current list search mode
    list_search_mode: ListSearchMode,
    /// Semantic search state
    semantic_search: SemanticSearchState,
    /// Cached lexical evidence produced outside the render path
    lexical_evidence: HashMap<usize, LexicalEvidence>,
    /// Outcome of the last UUID query
    uuid_lookup: Option<UuidLookup>,
    /// Rows deleted or renamed while a [`HistoryLoader`] loads; replayed on
    /// the corpus it installs
    corpus_edits: CorpusEdits,
}

struct BrowserParts {
    conversations: Vec<Conversation>,
    searchable: Vec<SearchableConversation>,
    filtered: Vec<usize>,
    selected: Option<usize>,
    loading_state: LoadingState,
    single_file_mode: bool,
    single_file_error: Option<String>,
    search_tx: mpsc::Sender<SearchCommand>,
    search_rx: mpsc::Receiver<SearchResponse>,
}

impl Browser {
    fn from_parts(opts: BrowserOptions, parts: BrowserParts) -> Self {
        let semantic_available = opts.semantic_available && !parts.single_file_mode;
        // Upstream only disabled semantic search in single-file mode, which
        // also forced lexical mode: never start in a mode the user could not
        // leave (toggling is a no-op while semantic search is unavailable).
        let list_search_mode = if semantic_available {
            opts.default_mode
        } else {
            ListSearchMode::Lexical
        };
        let (workspace, workspace_filter, excluded_projects) = if parts.single_file_mode {
            (None, false, HashSet::new())
        } else {
            (
                opts.workspace,
                opts.workspace_filter,
                opts.exclude_projects.into_iter().collect(),
            )
        };
        let conversations: CorpusSnapshot =
            Arc::new(parts.conversations.into_iter().map(Arc::new).collect());
        let index = CorpusIndex::build(
            &conversations,
            Some(ScopeSettings {
                excluded: excluded_projects.clone(),
                workspace: workspace.clone().filter(|_| workspace_filter),
            }),
        );
        let searchable: SearchSnapshot =
            Arc::new(parts.searchable.into_iter().map(Arc::new).collect());
        Self {
            env: opts.env,
            show_last: opts.show_last,
            waker: opts.waker,
            embedder_factory: opts.embedder_factory,
            semantic_corpus_version: 1,
            semantic_scope_version: 0,
            semantic_sent_corpus_version: 0,
            semantic_sent_scope_signature: None,
            conversations,
            searchable,
            index,
            filtered: parts.filtered,
            selected: parts.selected,
            selection_anchor: None,
            query: String::new(),
            loading_state: parts.loading_state,
            single_file_mode: parts.single_file_mode,
            single_file_error: parts.single_file_error,
            workspace_filter,
            workspace,
            excluded_projects,
            search_tx: parts.search_tx,
            search_rx: parts.search_rx,
            search_generation: 0,
            search_in_flight: false,
            list_search_mode,
            semantic_search: SemanticSearchState {
                available: semantic_available,
                ..Default::default()
            },
            lexical_evidence: HashMap::new(),
            uuid_lookup: None,
            corpus_edits: CorpusEdits::default(),
        }
    }

    /// A browser over already loaded conversations (upstream
    /// `App::new_with_options`). The order of `conversations` is kept.
    pub fn new(conversations: Vec<Conversation>, opts: BrowserOptions) -> Self {
        let searchable = search::precompute_search_text(&conversations);
        let (search_tx, search_rx) = spawn_search_worker(opts.waker.clone());
        let mut browser = Self::from_parts(
            opts,
            BrowserParts {
                conversations,
                searchable,
                filtered: Vec::new(),
                selected: None,
                loading_state: LoadingState::Ready,
                single_file_mode: false,
                single_file_error: None,
                search_tx,
                search_rx,
            },
        );
        browser.filtered = browser.filter_indices(0..browser.conversations.len());
        browser.selected = if browser.filtered.is_empty() {
            None
        } else {
            Some(0)
        };
        browser
    }

    /// An empty browser waiting for [`Browser::append_conversations`] and
    /// [`Browser::finish_loading`] (upstream `App::new_loading_with_options`).
    pub fn new_loading(opts: BrowserOptions) -> Self {
        let (search_tx, search_rx) = spawn_search_worker(opts.waker.clone());
        Self::from_parts(
            opts,
            BrowserParts {
                conversations: Vec::new(),
                searchable: Vec::new(),
                filtered: Vec::new(),
                selected: None,
                loading_state: LoadingState::Loading { loaded: 0 },
                single_file_mode: false,
                single_file_error: None,
                search_tx,
                search_rx,
            },
        )
    }

    /// A browser for one opened JSONL file (upstream `App::new_single_file`).
    ///
    /// The file is parsed like a listed session and its project name is
    /// derived from its `cwd` (or the file path). Semantic search,
    /// exclusions, the workspace scope and delete are unavailable.
    ///
    /// A file the list would leave out (a clear-only session, or one with
    /// no listable preview such as a Warmup prompt and its reply) yields an
    /// empty browser. That is an error ([`Browser::single_file_error`]) only
    /// when the file cannot be read or the viewer would render no message
    /// in any display mode; otherwise the GUI opens the viewer on the path
    /// as upstream did, with no list row or metadata (the header falls back
    /// to the file stem). Upstream showed an empty viewer for a bad file.
    pub fn new_single_file(path: PathBuf, opts: BrowserOptions) -> Self {
        let (search_tx, search_rx) = spawn_search_worker(opts.waker.clone());
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let show_last = opts.show_last;

        let mut conversations = Vec::new();
        let mut filtered = Vec::new();
        let mut selected = None;
        let mut single_file_error = None;

        match process_conversation_file(path.clone(), modified, None) {
            Ok(Some(mut conv)) => {
                // Set project_name the same way as the loader does
                let project_path = conv.cwd.clone().unwrap_or_else(|| path.clone());
                conv.project_name = Some(format_short_name_from_path(&project_path));
                apply_preview_mode(&mut conv, show_last);
                conversations.push(conv);
                filtered.push(0);
                selected = Some(0);
            }
            Ok(None) => match single_file_renders_nothing(&path) {
                Ok(false) => {}
                Ok(true) => {
                    single_file_error = Some(format!(
                        "{} contains no conversation messages",
                        path.display()
                    ));
                }
                Err(error) => single_file_error = Some(error.to_string()),
            },
            Err(error) => single_file_error = Some(error.to_string()),
        }

        Self::from_parts(
            opts,
            BrowserParts {
                conversations,
                searchable: Vec::new(),
                filtered,
                selected,
                loading_state: LoadingState::Ready,
                single_file_mode: true,
                single_file_error,
                search_tx,
                search_rx,
            },
        )
    }

    /// The environment this browser was created with.
    pub fn env(&self) -> &Arc<HistoryEnv> {
        &self.env
    }

    /// Whether previews show the last messages (see [`BrowserOptions::show_last`]).
    pub fn show_last(&self) -> bool {
        self.show_last
    }

    /// Change the preview mode (applies to later renames; the rows are
    /// replaced by the next load).
    pub fn set_show_last(&mut self, show_last: bool) {
        self.show_last = show_last;
    }

    /// Consume the browser and return its conversations (cloned where a
    /// worker still shares them).
    pub fn into_conversations(self) -> Vec<Conversation> {
        Arc::unwrap_or_clone(self.conversations)
            .into_iter()
            .map(Arc::unwrap_or_clone)
            .collect()
    }

    pub fn loading_state(&self) -> &LoadingState {
        &self.loading_state
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.loading_state, LoadingState::Loading { .. })
    }

    /// Row list: indices into [`Browser::conversations`], in display order.
    pub fn filtered(&self) -> &[usize] {
        &self.filtered
    }

    /// Every loaded conversation (including excluded and out-of-scope ones).
    /// The elements are shared with the search workers.
    pub fn conversations(&self) -> &[Arc<Conversation>] {
        &self.conversations
    }

    /// The current corpus, shared (no copy): the same `Arc` the workers
    /// search. It stays valid (and unchanged) after the browser moves on.
    pub fn corpus_snapshot(&self) -> CorpusSnapshot {
        self.conversations.clone()
    }

    /// The conversation shown at row `filtered_pos`.
    pub fn row(&self, filtered_pos: usize) -> Option<&Conversation> {
        self.filtered
            .get(filtered_pos)
            .map(|&index| self.conversations[index].as_ref())
    }

    /// Index and conversation of the loaded session at `path`.
    pub fn conversation_by_path(&self, path: &Path) -> Option<(usize, &Conversation)> {
        self.position_of_path(path)
            .map(|index| (index, self.conversations[index].as_ref()))
    }

    /// Index (into [`Browser::conversations`]) of the first loaded session
    /// at `path` (an indexed lookup, CCR-2).
    pub(super) fn position_of_path(&self, path: &Path) -> Option<usize> {
        self.index.first_path(&self.conversations, path)
    }

    /// Selected row (position in [`Browser::filtered`]).
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_single_file_mode(&self) -> bool {
        self.single_file_mode
    }

    /// Why the single opened file cannot be shown (single-file mode): an
    /// I/O or parse error, or a file the viewer would render empty. `None`
    /// with no conversations means the file is readable but not listable;
    /// it still opens in the viewer.
    pub fn single_file_error(&self) -> Option<&str> {
        self.single_file_error.as_deref()
    }

    pub fn workspace_filter(&self) -> bool {
        self.workspace_filter
    }

    /// The workspace used by the project scope.
    pub fn workspace(&self) -> Option<&WorkspaceScope> {
        self.workspace.as_ref()
    }

    /// Whether a workspace is set (the scope toggle is a no-op without one).
    pub fn has_project_context(&self) -> bool {
        self.workspace.is_some()
    }

    pub fn list_search_mode(&self) -> ListSearchMode {
        self.list_search_mode
    }

    /// Whether semantic search can be used (always false in single-file mode).
    pub fn semantic_available(&self) -> bool {
        self.semantic_search.available
    }

    /// Project names hidden from the list.
    pub fn excluded_projects(&self) -> &HashSet<String> {
        &self.excluded_projects
    }

    /// Outcome of the current query if it is a session UUID. A
    /// [`UuidLookup::Found`] index always points into
    /// [`Browser::conversations`]; the lookup is cleared when that session
    /// is deleted.
    pub fn uuid_lookup(&self) -> Option<&UuidLookup> {
        self.uuid_lookup.as_ref()
    }

    /// Search-bar count text: `"Loading... <n>"`, `"<sel>/<rows>"` or
    /// `"0/<rows>"` (upstream `render_search_bar`).
    pub fn count_text(&self) -> String {
        match &self.loading_state {
            LoadingState::Loading { loaded } => format!("Loading... {}", loaded),
            LoadingState::Ready => match self.selected {
                Some(selected) => format!("{}/{}", selected + 1, self.filtered.len()),
                None => format!("0/{}", self.filtered.len()),
            },
        }
    }

    /// Inputs of [`crate::display_format::build_row_preview`] for row
    /// `filtered_pos`, wired to this browser's mode, semantic metadata and
    /// lexical evidence. `query` is [`HighlightQuery::parse`] of
    /// [`Browser::query`], parsed once per frame.
    pub fn row_preview_input<'a>(
        &'a self,
        filtered_pos: usize,
        query: &'a HighlightQuery,
        max_width: usize,
    ) -> Option<RowPreviewInput<'a>> {
        let conversation_index = *self.filtered.get(filtered_pos)?;
        Some(RowPreviewInput {
            conversation: &self.conversations[conversation_index],
            query,
            semantic_mode: self.list_search_mode == ListSearchMode::Semantic,
            semantic_metadata: self.semantic_result_metadata(conversation_index),
            lexical_evidence: self.lexical_evidence(conversation_index),
            max_width,
        })
    }

    /// Inputs of [`crate::display_format::build_row_header`] for row
    /// `filtered_pos` (`width` in cells, `now` computed once per frame).
    pub fn row_header_input<'a>(
        &'a self,
        filtered_pos: usize,
        query: &'a HighlightQuery,
        width: usize,
        now: chrono::DateTime<chrono::Local>,
    ) -> Option<RowHeaderInput<'a>> {
        let conversation_index = *self.filtered.get(filtered_pos)?;
        Some(RowHeaderInput {
            conversation: &self.conversations[conversation_index],
            query,
            width,
            show_source_label: self.has_multiple_sources(),
            semantic_mode: self.list_search_mode == ListSearchMode::Semantic,
            semantic_metadata: self.semantic_result_metadata(conversation_index),
            now,
        })
    }

    /// The whole right-aligned search-bar status: the count, prefixed with
    /// `"sem "` in semantic mode and followed by
    /// [`Browser::semantic_status_text`] when there is one.
    pub fn search_status_text(&self) -> String {
        let count_text = self.count_text();
        if self.list_search_mode == ListSearchMode::Semantic {
            self.semantic_status_text()
                .map(|status| {
                    format!(
                        "{} {} {}",
                        self.list_search_mode.label(),
                        count_text,
                        status
                    )
                })
                .unwrap_or_else(|| format!("{} {}", self.list_search_mode.label(), count_text))
        } else {
            count_text
        }
    }
}

use crate::history::apply_preview_mode;

/// Whether the viewer would show no message for `path` even with every
/// display toggle on (thinking, full tool output). Used for a single file
/// the list leaves out: only then is it reported as empty.
fn single_file_renders_nothing(path: &Path) -> std::io::Result<bool> {
    let entries = crate::view::parse_conversation_file(path)?;
    let prefs = crate::view::ViewPrefs {
        tool_display: crate::view::ToolDisplayMode::Full,
        show_thinking: true,
        show_timing: false,
    };
    Ok(
        crate::view::build_document(&entries, &prefs, &Default::default())
            .messages
            .is_empty(),
    )
}

#[cfg(test)]
mod test_helpers;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod interaction_tests;

#[cfg(test)]
mod gap_tests;

#[cfg(test)]
mod ccr_tests;

#[cfg(test)]
mod perf_tests;
