//! Semantic cache maintenance and reports (upstream `semantic_cli.rs`) as
//! data-returning operations.
//!
//! Upstream printed everything to stderr. Here every operation returns a
//! structured report that also carries upstream's exact stderr text
//! (`text`), and long-running work reports progress through a callback and
//! honors a [`SemanticCancellationToken`]:
//!
//! * [`generate_semantic_cache`] (`--generate-semantic-cache`)
//! * [`clear_semantic_cache`] (`--clear-semantic-cache`), only ever inside
//!   this crate's own cache namespace, and [`clear_embedding_cache`]
//! * [`semantic_search_report`] (`--semantic-search`)
//! * [`semantic_debug_report`] (`--debug-semantic-search`)
//! * [`semantic_status`]: cache file, model and corpus coverage statistics
//!
//! The embedder is created lazily through an [`EmbedderFactory`]
//! ([`fastembed_factory`] in production, which never draws hf-hub's stderr
//! progress bar; fakes in tests), so nothing downloads a model unless an
//! operation really needs embeddings.

use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history::{self, Conversation};
use crate::search::literal::{CaseMode, build_literal_corpus, exact_fallback};
use crate::search::query::ParsedQuery;
use crate::semantic::cache::{
    MAX_CACHE_ENTRIES, cache_contains_text, cache_miss_count, cached_chunks, embedding_cache_key,
    embedding_cache_writer, read_embedding_cache, write_embedding_cache,
};
use crate::semantic::chunk::{build_chunks, build_chunks_with_sources};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::filter::filter_embedded_chunks_by_literals;
use crate::semantic::index::{
    SemanticIndexCandidate, SemanticIndexProgress, SemanticIndexRequest, SemanticIndexResponse,
    SemanticIndexState,
};
use crate::semantic::output::{format_hit, truncate};
use crate::semantic::rank::rank_chunks;
use crate::semantic::types::{
    CACHE_SCHEMA_VERSION, ChunkConfig, EmbeddingCache, MODEL_NAME, SemanticCancellationToken,
    SemanticChunkSource, SemanticHit,
};
use crate::time_filter::TimeFilter;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Creates the embedder when an operation needs one.
pub type EmbedderFactory<'a> = &'a mut dyn FnMut() -> Result<Box<dyn SemanticEmbedder>>;

/// The production embedder: fastembed BGE-small, downloaded on first use
/// into [`HistoryEnv::semantic_model_dir`], without a progress bar. Under
/// `cfg(test)` the embedder refuses to load: tests inject their own.
pub fn fastembed_factory(
    env: &HistoryEnv,
) -> impl FnMut() -> Result<Box<dyn SemanticEmbedder>> + use<> {
    let model_dir = env.semantic_model_dir();
    move || {
        let model_dir = model_dir.clone().ok_or_else(missing_home)?;
        crate::semantic::fastembed::quiet_embedder_in(model_dir)
    }
}

fn missing_home() -> AppError {
    AppError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Could not determine home directory",
    ))
}

/// Upstream's message when no conversation is selected.
pub fn no_conversations_message(local: bool) -> &'static str {
    if local {
        "No conversations available for semantic search in the current workspace."
    } else {
        "No conversations available for semantic search."
    }
}

/// Indices of the conversations a semantic operation works on: all of them,
/// or (with a workspace, upstream `--local`) those whose Claude project
/// directory name is the workspace's (worktree-aware). Pi/OMP conversations
/// never match a workspace here, exactly as upstream (`semantic_cli.rs:446-472`).
pub fn select_conversation_indices(
    conversations: &[Conversation],
    workspace: Option<&WorkspaceScope>,
) -> Vec<usize> {
    conversations
        .iter()
        .enumerate()
        .filter(|(_, conversation)| {
            workspace.is_none_or(|workspace| {
                conversation
                    .path
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .is_some_and(|name| {
                        history::is_same_project(
                            &name.to_string_lossy(),
                            workspace.project_dir_name(),
                        )
                    })
            })
        })
        .map(|(index, _)| index)
        .collect()
}

fn selected_refs<'a>(
    conversations: &'a [Conversation],
    indices: &[usize],
) -> Vec<&'a Conversation> {
    indices.iter().map(|index| &conversations[*index]).collect()
}

fn semantic_index_candidates(selected: &[&Conversation]) -> Vec<SemanticIndexCandidate> {
    selected
        .iter()
        .enumerate()
        .map(|(index, conversation)| SemanticIndexCandidate {
            index,
            source: SemanticChunkSource::VisibleDialogue,
            conversation: Arc::new((*conversation).clone()),
        })
        .collect()
}

/// Visible dialogue plus one "agent route" passage per conversation: what
/// the cache generation embeds and protects.
fn semantic_cache_candidates(selected: &[&Conversation]) -> Vec<SemanticIndexCandidate> {
    let mut candidates = semantic_index_candidates(selected);
    for (index, conversation) in selected.iter().enumerate() {
        if !conversation.semantic_turns.is_empty()
            && let Some(route) =
                crate::agent::service::agent_route_semantic_conversation(conversation)
        {
            candidates.push(SemanticIndexCandidate {
                index,
                source: SemanticChunkSource::AgentRoute,
                conversation: Arc::new(route),
            });
        }
    }
    candidates
}

/// Upstream's `intent:` / `literals:` lines.
fn format_parsed_query(parsed: &ParsedQuery) -> String {
    let mut output = format!("intent: {:?}\n", parsed.unquoted());
    if parsed.literals().is_empty() {
        output.push_str("literals: none\n");
    } else {
        output.push_str("literals:\n");
        for literal in parsed.literals() {
            output.push_str(&format!(
                "  {:?} ({:?})\n",
                literal.text(),
                literal.case_mode()
            ));
        }
    }
    output
}

/// Collects upstream's stderr output.
#[derive(Default)]
struct Stderr(String);

impl Stderr {
    fn line(&mut self, text: impl AsRef<str>) {
        self.0.push_str(text.as_ref());
        self.0.push('\n');
    }

    fn raw(&mut self, text: impl AsRef<str>) {
        self.0.push_str(text.as_ref());
    }
}

// ---------------------------------------------------------------------------
// generate
// ---------------------------------------------------------------------------

/// Progress of [`generate_semantic_cache`]. Its `Display` text is
/// upstream's stderr line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerateProgress {
    /// About to check `chunk_count` passages of `conversation_count`
    /// conversations against the cache.
    Checking {
        chunk_count: usize,
        conversation_count: usize,
    },
    /// `completed` of `total` passages missing from the cache are embedded.
    Embedded { completed: usize, total: usize },
}

impl fmt::Display for GenerateProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenerateProgress::Checking {
                chunk_count,
                conversation_count,
            } => write!(
                f,
                "Semantic cache: checking {chunk_count} chunk(s) from {conversation_count} recent conversation(s)."
            ),
            GenerateProgress::Embedded { completed, total } => write!(
                f,
                "Semantic cache: embedded {completed}/{total} changed chunk(s)"
            ),
        }
    }
}

/// How [`generate_semantic_cache`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerateOutcome {
    /// Nothing selected (no conversations, or none in the workspace).
    NoConversations,
    /// The selected conversations have no dialogue or route text.
    NoText,
    /// The cache now holds `cached_chunks` of the corpus's passages.
    Cached { cached_chunks: usize },
}

/// Result of [`generate_semantic_cache`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerateReport {
    pub outcome: GenerateOutcome,
    /// Selected conversations.
    pub conversation_count: usize,
    /// Passages (dialogue chunks plus route passages).
    pub chunk_count: usize,
    /// Distinct passage texts.
    pub unique_chunk_count: usize,
    /// The embedding cache file written.
    pub cache_file: Option<PathBuf>,
    /// Upstream's exact stderr output.
    pub text: String,
}

/// Upstream's refusal when a time filter is active (a partial cache would be
/// silently wrong on later unfiltered searches).
pub const GENERATE_WITH_TIME_FILTER_ERROR: &str =
    "--generate-semantic-cache cannot be combined with a time filter";

/// Embed every passage of the selected conversations into the embedding
/// cache and mark them protected (upstream `generate_cache`).
///
/// `conversations` must be the whole corpus (newest first); with a
/// `workspace` only its Claude conversations are selected. Fails when
/// `time_filter` is active, when the corpus has more unique passages than
/// the cache holds (50 000), or with [`AppError::SemanticSearchCancelled`]
/// when `cancel` fires (passages embedded so far are saved every 256).
pub fn generate_semantic_cache(
    env: &HistoryEnv,
    conversations: &[Conversation],
    workspace: Option<&WorkspaceScope>,
    time_filter: &TimeFilter,
    embedder: EmbedderFactory<'_>,
    progress: &mut dyn FnMut(GenerateProgress),
    cancel: &SemanticCancellationToken,
) -> Result<GenerateReport> {
    if time_filter.is_active() {
        return Err(AppError::ConfigError(
            GENERATE_WITH_TIME_FILTER_ERROR.to_string(),
        ));
    }
    let mut stderr = Stderr::default();
    let cache_file = env.embedding_cache_file();
    let indices = select_conversation_indices(conversations, workspace);
    let selected = selected_refs(conversations, &indices);
    let mut report = GenerateReport {
        outcome: GenerateOutcome::NoConversations,
        conversation_count: selected.len(),
        chunk_count: 0,
        unique_chunk_count: 0,
        cache_file: cache_file.clone(),
        text: String::new(),
    };
    if selected.is_empty() {
        stderr.line(no_conversations_message(workspace.is_some()));
        report.text = stderr.0;
        return Ok(report);
    }

    let candidates = semantic_cache_candidates(&selected);
    let chunks = build_chunks_with_sources(
        candidates.iter().map(|candidate| {
            (
                candidate.index,
                candidate.source,
                candidate.conversation.as_ref(),
            )
        }),
        ChunkConfig::default(),
    );
    let chunk_count = chunks.len();
    let core_keys = chunks
        .iter()
        .map(|chunk| embedding_cache_key(&chunk.text))
        .collect::<HashSet<_>>();
    let unique_chunk_count = core_keys.len();
    report.chunk_count = chunk_count;
    report.unique_chunk_count = unique_chunk_count;
    if unique_chunk_count > MAX_CACHE_ENTRIES {
        return Err(AppError::ConfigError(format!(
            "semantic core has {unique_chunk_count} unique passages, exceeding the {MAX_CACHE_ENTRIES}-entry cache capacity"
        )));
    }
    if chunk_count == 0 {
        stderr.line("No semantic dialogue or route text available for cache generation.");
        report.outcome = GenerateOutcome::NoText;
        report.text = stderr.0;
        return Ok(report);
    }

    let request = SemanticIndexRequest {
        query: "",
        literal_filters: &[],
        full_corpus: &candidates,
        scope: &candidates,
        corpus_version: 1,
        prewarm: true,
        include_chunk_hits: false,
    };
    let mut state = SemanticIndexState::load(cache_file.as_deref());
    for (key, entry) in &mut state.cache.entries {
        entry.protected &= core_keys.contains(key);
    }
    let checking = GenerateProgress::Checking {
        chunk_count,
        conversation_count: selected.len(),
    };
    stderr.line(checking.to_string());
    progress(checking);

    let mut embedder = embedder()?;
    let response = state.refresh_or_prewarm(
        &request,
        embedder.as_mut(),
        cancel,
        |index_progress| {
            if let SemanticIndexProgress::Embedding { completed, total } = index_progress
                && total > 0
                && completed > 0
            {
                let event = GenerateProgress::Embedded { completed, total };
                stderr.line(event.to_string());
                progress(event);
            }
        },
        embedding_cache_writer(cache_file.clone()),
    )?;
    for key in &core_keys {
        if let Some(entry) = state.cache.entries.get_mut(key) {
            entry.protected = true;
        }
    }
    if let Some(path) = &cache_file {
        write_embedding_cache(path, &state.cache);
    }

    let cached_chunks = response.indexed_chunk_count.min(chunk_count);
    stderr.line(format!(
        "Semantic cache: cached {} chunk(s) from {} recent conversation(s).",
        cached_chunks,
        selected.len()
    ));
    report.outcome = GenerateOutcome::Cached { cached_chunks };
    report.text = stderr.0;
    Ok(report)
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

/// Result of clearing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClearReport {
    /// Whether anything was removed.
    pub removed: bool,
    /// What was (or would have been) removed.
    pub path: Option<PathBuf>,
    /// Upstream's message (`Semantic cache cleared.` / `Semantic cache is
    /// already empty.`).
    pub message: &'static str,
}

impl ClearReport {
    fn new(removed: bool, path: Option<PathBuf>) -> Self {
        Self {
            removed,
            path,
            message: if removed {
                "Semantic cache cleared."
            } else {
                "Semantic cache is already empty."
            },
        }
    }
}

/// The installed CLI's cache root (`<home>/.cache/claude-history`), which
/// this crate must never delete from.
fn cli_cache_root(env: &HistoryEnv) -> Option<PathBuf> {
    env.home_dir
        .as_ref()
        .map(|home| home.join(".cache").join("claude-history"))
}

fn ensure_own_namespace(env: &HistoryEnv, path: &Path) -> Result<()> {
    let Some(cli_root) = cli_cache_root(env) else {
        return Ok(());
    };
    let inside_cli = path.starts_with(&cli_root)
        || matches!(
            (path.canonicalize(), cli_root.canonicalize()),
            (Ok(path), Ok(cli_root)) if path.starts_with(&cli_root)
        );
    if inside_cli {
        return Err(AppError::ConfigError(format!(
            "refusing to clear {}: it belongs to the claude-history CLI's cache",
            path.display()
        )));
    }
    Ok(())
}

/// Empty the semantic cache directory (`<cache root>/semantic`): the
/// embedding cache AND the downloaded model, like upstream (which removed
/// the directory; its lock file now stays, see
/// [`crate::semantic::cache::clear_semantic_cache_files`], which also never
/// follows links). Refuses to touch the installed CLI's cache directory if
/// the cache root was pointed there.
///
/// Holds the semantic cache lock while deleting (K6), waiting at most
/// [`crate::semantic::cache::CACHE_LOCK_WAIT`] for another writer; then it
/// fails with an I/O error instead. [`generate_semantic_cache`] writes
/// through the same lock.
pub fn clear_semantic_cache(env: &HistoryEnv) -> Result<ClearReport> {
    let dir = env.semantic_cache_dir();
    if let Some(dir) = &dir {
        ensure_own_namespace(env, dir)?;
    }
    let removed = crate::semantic::cache::clear_semantic_cache_files(dir.as_deref())?;
    Ok(ClearReport::new(removed, dir))
}

/// Remove only the embedding cache file, keeping the downloaded model.
pub fn clear_embedding_cache(env: &HistoryEnv) -> Result<ClearReport> {
    let file = env.embedding_cache_file();
    let Some(path) = file.clone() else {
        return Ok(ClearReport::new(false, None));
    };
    ensure_own_namespace(env, &path)?;
    if !path.exists() {
        return Ok(ClearReport::new(false, file));
    }
    // Same single-writer lock as every cache write (K6).
    let _lock = match path.parent() {
        Some(dir) => Some(
            crate::semantic::cache::lock_semantic_cache_dir(
                dir,
                crate::semantic::cache::CACHE_LOCK_WAIT,
            )?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "the semantic cache is being written by another process; try again",
                )
            })?,
        ),
        None => None,
    };
    std::fs::remove_file(&path)?;
    Ok(ClearReport::new(true, file))
}

// ---------------------------------------------------------------------------
// search report
// ---------------------------------------------------------------------------

/// One result row (upstream `format_hit` / `format_exact_hit`).
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticSearchRow {
    /// 1-based rank.
    pub rank: usize,
    /// Index into the `conversations` passed in.
    pub conversation_index: usize,
    pub path: PathBuf,
    /// Project name or `(none)`.
    pub project: String,
    /// Custom title, else summary, else preview.
    pub title: String,
    /// The session (file stem), or the full path when the stem is ambiguous
    /// or unknown.
    pub session_or_path: String,
    /// The semantic hit (`None` for exact literal matches).
    pub hit: Option<SemanticHit>,
    /// Upstream's text for this row.
    pub text: String,
}

/// How [`semantic_search_report`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticSearchOutcome {
    NoConversations,
    EmptyQuery,
    /// A quoted-only query: exact literal matches, newest first.
    Exact,
    /// No visible dialogue to search.
    NoText,
    /// The model returned no query embedding.
    NoQueryEmbedding,
    /// Ranked semantic hits (possibly none).
    Ranked,
}

/// Result of [`semantic_search_report`].
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticSearchReport {
    pub outcome: SemanticSearchOutcome,
    pub conversation_count: usize,
    /// Cached passages searched.
    pub searched_chunks: usize,
    pub rows: Vec<SemanticSearchRow>,
    /// Upstream's exact stderr output.
    pub text: String,
}

fn conversation_title(conversation: &Conversation) -> String {
    conversation
        .custom_title
        .as_deref()
        .or(conversation.summary.as_deref())
        .unwrap_or(&conversation.preview)
        .to_string()
}

fn session_stem(conversation: &Conversation) -> String {
    conversation
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("?")
        .to_string()
}

fn format_exact_hit(rank: usize, conversation: &Conversation) -> String {
    let project = conversation.project_name.as_deref().unwrap_or("(none)");
    format!(
        "#{rank:2} exact | {project} | {}\n     {}\n",
        session_stem(conversation),
        conversation_title(conversation)
    )
}

fn exact_row(rank: usize, original_index: usize, conversation: &Conversation) -> SemanticSearchRow {
    SemanticSearchRow {
        rank,
        conversation_index: original_index,
        path: conversation.path.clone(),
        project: conversation
            .project_name
            .clone()
            .unwrap_or_else(|| "(none)".to_string()),
        title: conversation_title(conversation),
        session_or_path: session_stem(conversation),
        hit: None,
        text: format_exact_hit(rank, conversation),
    }
}

fn hit_row(
    rank: usize,
    hit: &SemanticHit,
    selected: &[&Conversation],
    indices: &[usize],
) -> SemanticSearchRow {
    let conversation = selected[hit.conversation_index];
    let text = format_hit(rank, hit, selected);
    // `format_hit` line 1: "#NN hybrid=… | project | session_or_path".
    let session_or_path = text
        .lines()
        .next()
        .and_then(|line| line.rsplit(" | ").next())
        .unwrap_or_default()
        .to_string();
    SemanticSearchRow {
        rank,
        conversation_index: indices[hit.conversation_index],
        path: conversation.path.clone(),
        project: conversation
            .project_name
            .clone()
            .unwrap_or_else(|| "(none)".to_string()),
        title: conversation_title(conversation),
        session_or_path,
        hit: Some(hit.clone()),
        text,
    }
}

fn exact_literal_indices(conversations: &[&Conversation], parsed: &ParsedQuery) -> Vec<usize> {
    let plain_conversations = conversations
        .iter()
        .map(|conversation| (*conversation).clone())
        .collect::<Vec<_>>();
    let corpus = build_literal_corpus(&plain_conversations);
    exact_fallback(&plain_conversations, &corpus, parsed.literals(), |_| true)
}

/// Upstream's interactive refresh: embed missing passages, save the cache
/// once, then rank.
fn refresh_and_rank_interactive(
    request: &SemanticIndexRequest<'_>,
    state: &mut SemanticIndexState,
    embedder: &mut dyn SemanticEmbedder,
    mut save_cache: impl FnMut(&EmbeddingCache),
) -> Result<(SemanticIndexResponse, SemanticIndexResponse)> {
    let cancellation = SemanticCancellationToken::new();
    let refresh = state.refresh_passages(request, embedder, &cancellation, |_| {}, |_| {})?;
    save_cache(&state.cache);
    let response = state.rank_refreshed(request, embedder, &cancellation, |_| {})?;
    Ok((refresh, response))
}

/// Upstream `--semantic-search` (`semantic_cli.rs:8-79`): rank the selected
/// conversations for `query` (embedding passages missing from the cache and
/// saving it), at most `top` rows.
pub fn semantic_search_report(
    env: &HistoryEnv,
    query: &str,
    conversations: &[Conversation],
    top: usize,
    workspace: Option<&WorkspaceScope>,
    embedder: EmbedderFactory<'_>,
) -> Result<SemanticSearchReport> {
    let mut stderr = Stderr::default();
    let indices = select_conversation_indices(conversations, workspace);
    let selected = selected_refs(conversations, &indices);
    let mut report = SemanticSearchReport {
        outcome: SemanticSearchOutcome::NoConversations,
        conversation_count: selected.len(),
        searched_chunks: 0,
        rows: Vec::new(),
        text: String::new(),
    };
    let finish = |mut report: SemanticSearchReport, stderr: Stderr, outcome| {
        report.outcome = outcome;
        report.text = stderr.0;
        Ok(report)
    };
    if selected.is_empty() {
        stderr.line(no_conversations_message(workspace.is_some()));
        return finish(report, stderr, SemanticSearchOutcome::NoConversations);
    }

    let parsed = ParsedQuery::parse(query);
    if parsed.is_effectively_empty() {
        stderr.line("Query is empty.");
        return finish(report, stderr, SemanticSearchOutcome::EmptyQuery);
    }
    if parsed.is_quoted_only() {
        let results = exact_literal_indices(&selected, &parsed);
        for (rank, index) in results.iter().take(top).enumerate() {
            let row = exact_row(rank + 1, indices[*index], selected[*index]);
            stderr.line(&row.text);
            report.rows.push(row);
        }
        if results.is_empty() {
            stderr.line("No semantic matches found.");
        }
        return finish(report, stderr, SemanticSearchOutcome::Exact);
    }

    let candidates = semantic_index_candidates(&selected);
    let request = SemanticIndexRequest {
        query: parsed.semantic_text(),
        literal_filters: parsed.literals(),
        full_corpus: &candidates,
        scope: &candidates,
        corpus_version: 1,
        prewarm: false,
        include_chunk_hits: false,
    };
    let cache_file = env.embedding_cache_file();
    let mut state = SemanticIndexState::load(cache_file.as_deref());
    let cancellation = SemanticCancellationToken::new();
    if !state.has_chunks(&request, &cancellation)? {
        state.clear_empty(&request, &cancellation)?;
        stderr.line("No visible dialogue text available for semantic search.");
        return finish(report, stderr, SemanticSearchOutcome::NoText);
    }

    let mut embedder = embedder()?;
    let (refresh, response) = refresh_and_rank_interactive(
        &request,
        &mut state,
        embedder.as_mut(),
        embedding_cache_writer(cache_file),
    )?;
    report.searched_chunks = refresh.indexed_chunk_count;
    stderr.line(format!(
        "Semantic search: searching {} cached chunk(s) from {} recent conversation(s) with fastembed {MODEL_NAME}",
        refresh.indexed_chunk_count,
        selected.len()
    ));
    if !response.query_embedding_returned {
        stderr.line("No query embedding returned.");
        return finish(report, stderr, SemanticSearchOutcome::NoQueryEmbedding);
    }
    for (rank, hit) in response.hits.iter().take(top).enumerate() {
        let row = hit_row(rank + 1, hit, &selected, &indices);
        stderr.line(&row.text);
        report.rows.push(row);
    }
    if response.hits.is_empty() {
        stderr.line("No semantic matches found.");
    }
    finish(report, stderr, SemanticSearchOutcome::Ranked)
}

// ---------------------------------------------------------------------------
// debug report
// ---------------------------------------------------------------------------

/// A quoted literal of the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryLiteral {
    pub text: String,
    pub case_mode: CaseMode,
}

/// Chunking statistics of the selected conversations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkStats {
    pub turns: usize,
    pub chunks: usize,
    pub target_chars: usize,
    pub overlap_chars: usize,
    pub context_turns: usize,
}

/// A conversation with many chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkHeavyConversation {
    pub rank: usize,
    /// Index into the `conversations` passed in.
    pub conversation_index: usize,
    pub chunk_count: usize,
    pub path: PathBuf,
    pub title: String,
}

/// A sampled chunk (`session:chunk_index` and its text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkSample {
    pub session: String,
    pub chunk_index: usize,
    pub text: String,
}

/// Cache coverage of the selected chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheCoverage {
    pub entries: usize,
    pub hits: usize,
    pub misses: usize,
}

/// Where [`semantic_debug_report`] stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticDebugOutcome {
    NoConversations,
    EmptyQuery,
    /// Quoted-only query: exact literal matches.
    Exact {
        matches: usize,
    },
    NoText,
    NoCachedChunks,
    NoQueryEmbedding,
    Ranked,
}

/// Upstream `--debug-semantic-search` as data.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticDebugReport {
    pub outcome: SemanticDebugOutcome,
    pub intent: String,
    pub literals: Vec<QueryLiteral>,
    pub selected: usize,
    pub model_dir: Option<PathBuf>,
    pub cache_file: Option<PathBuf>,
    pub chunk_stats: Option<ChunkStats>,
    /// The ten conversations with the most chunks.
    pub chunk_heavy: Vec<ChunkHeavyConversation>,
    /// Up to five chunks containing code fences or command/system markers.
    pub suspicious: Vec<ChunkSample>,
    pub cache: Option<CacheCoverage>,
    /// Up to five chunks missing from the cache.
    pub missing: Vec<ChunkSample>,
    /// Cached chunks ranked.
    pub ranked_chunks: usize,
    /// Exact matches or ranked hits (top 20).
    pub rows: Vec<SemanticSearchRow>,
    /// Upstream's exact stderr output.
    pub text: String,
}

/// Upstream `--debug-semantic-search` (`semantic_cli.rs:180-352`). Ranks
/// only cached passages (never embeds passages); the embedder is created
/// only to embed the query.
pub fn semantic_debug_report(
    env: &HistoryEnv,
    query: &str,
    conversations: &[Conversation],
    workspace: Option<&WorkspaceScope>,
    embedder: EmbedderFactory<'_>,
) -> Result<SemanticDebugReport> {
    let mut stderr = Stderr::default();
    let parsed = ParsedQuery::parse(query);
    let indices = select_conversation_indices(conversations, workspace);
    let selected = selected_refs(conversations, &indices);
    let model_dir = env.semantic_model_dir();
    let cache_file = env.embedding_cache_file();
    let mut report = SemanticDebugReport {
        outcome: SemanticDebugOutcome::NoConversations,
        intent: parsed.unquoted().to_string(),
        literals: parsed
            .literals()
            .iter()
            .map(|literal| QueryLiteral {
                text: literal.text().to_string(),
                case_mode: literal.case_mode(),
            })
            .collect(),
        selected: selected.len(),
        model_dir: model_dir.clone(),
        cache_file: cache_file.clone(),
        chunk_stats: None,
        chunk_heavy: Vec::new(),
        suspicious: Vec::new(),
        cache: None,
        missing: Vec::new(),
        ranked_chunks: 0,
        rows: Vec::new(),
        text: String::new(),
    };
    let finish = |mut report: SemanticDebugReport, stderr: Stderr, outcome| {
        report.outcome = outcome;
        report.text = stderr.0;
        Ok(report)
    };

    stderr.raw(format_parsed_query(&parsed));
    stderr.line("");
    stderr.line(format!(
        "Semantic debug: selected {} conversation(s).",
        selected.len()
    ));
    match &model_dir {
        Some(dir) => stderr.line(format!("Semantic debug: model cache: {}", dir.display())),
        None => stderr.line("Semantic debug: model cache: unavailable"),
    }
    match &cache_file {
        Some(path) => stderr.line(format!(
            "Semantic debug: embedding cache: {}",
            path.display()
        )),
        None => stderr.line("Semantic debug: embedding cache: unavailable"),
    }

    if selected.is_empty() {
        stderr.line(no_conversations_message(workspace.is_some()));
        return finish(report, stderr, SemanticDebugOutcome::NoConversations);
    }
    if parsed.is_effectively_empty() {
        stderr.line("Semantic debug: query is empty.");
        return finish(report, stderr, SemanticDebugOutcome::EmptyQuery);
    }

    let turn_count = selected
        .iter()
        .map(|conversation| conversation.semantic_turns.len())
        .sum::<usize>();
    let chunk_config = ChunkConfig::default();
    let chunks = build_chunks(&selected, chunk_config);
    let stats = ChunkStats {
        turns: turn_count,
        chunks: chunks.len(),
        target_chars: chunk_config.target_chars,
        overlap_chars: chunk_config.overlap_chars,
        context_turns: chunk_config.context_turns,
    };
    report.chunk_stats = Some(stats);
    stderr.line(format!(
        "Semantic debug: {} semantic turn(s), {} chunk(s), target={} overlap={} context_turns={}",
        stats.turns, stats.chunks, stats.target_chars, stats.overlap_chars, stats.context_turns
    ));

    let mut chunk_counts = HashMap::<usize, usize>::new();
    for chunk in &chunks {
        *chunk_counts.entry(chunk.conversation_index).or_default() += 1;
    }
    let mut chunk_counts = chunk_counts.into_iter().collect::<Vec<_>>();
    chunk_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (rank, (index, count)) in chunk_counts.iter().take(10).enumerate() {
        let conversation = selected[*index];
        let heavy = ChunkHeavyConversation {
            rank: rank + 1,
            conversation_index: indices[*index],
            chunk_count: *count,
            path: conversation.path.clone(),
            title: conversation_title(conversation),
        };
        stderr.line(format!(
            "Semantic debug: chunk-heavy #{:2}: {} chunk(s) | {} | {}",
            heavy.rank,
            heavy.chunk_count,
            heavy.path.display(),
            heavy.title
        ));
        report.chunk_heavy.push(heavy);
    }

    let suspicious = chunks
        .iter()
        .filter(|chunk| {
            chunk.text.contains("```")
                || chunk.text.contains("<system-reminder>")
                || chunk.text.contains("<local-command-stdout>")
                || chunk.text.contains("<command-message>")
        })
        .take(5)
        .collect::<Vec<_>>();
    if suspicious.is_empty() {
        stderr.line(
            "Semantic debug: no sampled chunks contain code fences or command/system markers.",
        );
    } else {
        for chunk in suspicious {
            stderr.line(format!(
                "Semantic debug: suspicious chunk {}:{}: {}",
                chunk.session,
                chunk.chunk_index,
                truncate(&chunk.text, 220)
            ));
            report.suspicious.push(ChunkSample {
                session: chunk.session.clone(),
                chunk_index: chunk.chunk_index,
                text: chunk.text.clone(),
            });
        }
    }

    if parsed.is_quoted_only() {
        let results = exact_literal_indices(&selected, &parsed);
        stderr.line(format!(
            "Semantic debug: exact literal matches={}.",
            results.len()
        ));
        for (rank, index) in results.iter().take(20).enumerate() {
            let row = exact_row(rank + 1, indices[*index], selected[*index]);
            stderr.line(&row.text);
            report.rows.push(row);
        }
        if results.is_empty() {
            stderr.line("Semantic debug: no semantic matches found.");
        }
        let matches = results.len();
        return finish(report, stderr, SemanticDebugOutcome::Exact { matches });
    }

    if chunks.is_empty() {
        stderr.line("Semantic debug: no visible dialogue text available.");
        return finish(report, stderr, SemanticDebugOutcome::NoText);
    }

    let cache = read_embedding_cache(cache_file.as_deref(), chunk_config);
    let missing = cache_miss_count(&chunks, &cache);
    let coverage = CacheCoverage {
        entries: cache.entries.len(),
        hits: chunks.len().saturating_sub(missing),
        misses: missing,
    };
    report.cache = Some(coverage);
    stderr.line(format!(
        "Semantic debug: cache entries={}, hits={}, misses={}",
        coverage.entries, coverage.hits, coverage.misses
    ));
    for chunk in chunks
        .iter()
        .filter(|chunk| !cache_contains_text(&cache, &chunk.text))
        .take(5)
    {
        stderr.line(format!(
            "Semantic debug: missing cache chunk {}:{}: {}",
            chunk.session,
            chunk.chunk_index,
            truncate(&chunk.text, 180)
        ));
        report.missing.push(ChunkSample {
            session: chunk.session.clone(),
            chunk_index: chunk.chunk_index,
            text: chunk.text.clone(),
        });
    }

    let (embedded_chunks, _) = cached_chunks(chunks, &cache, &SemanticCancellationToken::new())?;
    if embedded_chunks.is_empty() {
        stderr.line("Semantic debug: no cached chunks available for ranking.");
        return finish(report, stderr, SemanticDebugOutcome::NoCachedChunks);
    }

    let mut embedder = embedder()?;
    let Some(query_embedding) = embedder.embed_query(parsed.semantic_text())? else {
        stderr.line("Semantic debug: no query embedding returned.");
        return finish(report, stderr, SemanticDebugOutcome::NoQueryEmbedding);
    };
    let embedded_chunks = filter_embedded_chunks_by_literals(embedded_chunks, parsed.literals());
    let hits = rank_chunks(
        parsed.semantic_text(),
        &query_embedding,
        &embedded_chunks,
        &SemanticCancellationToken::new(),
    )?;
    report.ranked_chunks = embedded_chunks.len();
    stderr.line(format!(
        "Semantic debug: ranked {} cached chunk(s) with fastembed {MODEL_NAME}.",
        embedded_chunks.len()
    ));
    for (rank, hit) in hits.iter().take(20).enumerate() {
        let row = hit_row(rank + 1, hit, &selected, &indices);
        stderr.line(&row.text);
        report.rows.push(row);
    }
    if hits.is_empty() {
        stderr.line("Semantic debug: no semantic matches found.");
    }
    finish(report, stderr, SemanticDebugOutcome::Ranked)
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// State of the embedding cache file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheFileState {
    /// No cache location (no home directory) or no file.
    Missing,
    /// The file exists but cannot be read or decoded.
    Unreadable(String),
    /// Written for another schema, model or chunking: it is ignored and
    /// replaced on the next save.
    Incompatible { schema_version: u32, model: String },
    /// A usable cache.
    Ready {
        entries: usize,
        protected_entries: usize,
        /// Entries over the 50 000-entry capacity are pruned on load.
        over_capacity: bool,
    },
}

/// How much of a corpus is embedded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticCoverage {
    /// Selected conversations.
    pub conversations: usize,
    /// Passages the cache generation would embed.
    pub chunks: usize,
    /// Distinct passage texts.
    pub unique_chunks: usize,
    /// Distinct passages in the cache.
    pub cached: usize,
    /// Distinct passages missing from the cache.
    pub missing: usize,
}

/// Semantic search status for a settings or maintenance view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticStatus {
    /// `<cache root>/semantic` (emptied by [`clear_semantic_cache`]).
    pub semantic_dir: Option<PathBuf>,
    pub cache_file: Option<PathBuf>,
    pub cache_file_size: Option<u64>,
    pub cache: CacheFileState,
    pub model_name: &'static str,
    pub model_dir: Option<PathBuf>,
    /// Whether the model directory exists and is not empty.
    pub model_downloaded: bool,
    /// Total size of the model directory in bytes.
    pub model_dir_size: u64,
    /// Coverage of `conversations` (when given).
    pub coverage: Option<SemanticCoverage>,
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            Ok(_) => entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Read the cache without pruning or rewriting it.
fn inspect_cache_file(path: Option<&Path>) -> (CacheFileState, Option<EmbeddingCache>) {
    let Some(path) = path.filter(|path| path.exists()) else {
        return (CacheFileState::Missing, None);
    };
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) => return (CacheFileState::Unreadable(error.to_string()), None),
    };
    let cache = match bincode::deserialize::<EmbeddingCache>(&data) {
        Ok(cache) => cache,
        Err(error) => return (CacheFileState::Unreadable(error.to_string()), None),
    };
    let config = ChunkConfig::default();
    let compatible = cache.schema_version == CACHE_SCHEMA_VERSION
        && cache.model == MODEL_NAME
        && cache.chunk_target_chars == config.target_chars
        && cache.chunk_overlap_chars == config.overlap_chars
        && cache.chunk_context_turns == config.context_turns;
    if !compatible {
        return (
            CacheFileState::Incompatible {
                schema_version: cache.schema_version,
                model: cache.model.clone(),
            },
            None,
        );
    }
    (
        CacheFileState::Ready {
            entries: cache.entries.len(),
            protected_entries: cache
                .entries
                .values()
                .filter(|entry| entry.protected)
                .count(),
            over_capacity: cache.entries.len() > MAX_CACHE_ENTRIES,
        },
        Some(cache),
    )
}

/// Cache, model and (optionally) corpus coverage statistics. Read-only:
/// never writes the cache or downloads the model.
pub fn semantic_status(
    env: &HistoryEnv,
    conversations: Option<&[Conversation]>,
    workspace: Option<&WorkspaceScope>,
) -> SemanticStatus {
    let cache_file = env.embedding_cache_file();
    let cache_file_size = cache_file
        .as_deref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len());
    let (cache_state, cache) = inspect_cache_file(cache_file.as_deref());
    let model_dir = env.semantic_model_dir();
    let model_downloaded = model_dir
        .as_deref()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .is_some_and(|mut entries| entries.next().is_some());
    let model_dir_size = model_dir.as_deref().map(dir_size).unwrap_or(0);
    let coverage = conversations.map(|conversations| {
        let indices = select_conversation_indices(conversations, workspace);
        let selected = selected_refs(conversations, &indices);
        let candidates = semantic_cache_candidates(&selected);
        let chunks = build_chunks_with_sources(
            candidates.iter().map(|candidate| {
                (
                    candidate.index,
                    candidate.source,
                    candidate.conversation.as_ref(),
                )
            }),
            ChunkConfig::default(),
        );
        let unique_chunks = chunks
            .iter()
            .map(|chunk| embedding_cache_key(&chunk.text))
            .collect::<HashSet<_>>()
            .len();
        let missing = match &cache {
            Some(cache) => cache_miss_count(&chunks, cache),
            None => unique_chunks,
        };
        SemanticCoverage {
            conversations: selected.len(),
            chunks: chunks.len(),
            unique_chunks,
            cached: unique_chunks - missing,
            missing,
        }
    });
    SemanticStatus {
        semantic_dir: env.semantic_cache_dir(),
        cache_file,
        cache_file_size,
        cache: cache_state,
        model_name: MODEL_NAME,
        model_dir,
        model_downloaded,
        model_dir_size,
        coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::test_fixtures::SemanticConversationFixture;
    use chrono::Local;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Offline embedder: "beta" passages and queries point one way,
    /// everything else the other way.
    struct FakeEmbedder {
        passage_calls: Rc<Cell<usize>>,
        query_calls: Rc<Cell<usize>>,
        query_embedding: Option<Vec<f32>>,
        query_error: bool,
    }

    impl SemanticEmbedder for FakeEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            self.passage_calls.set(self.passage_calls.get() + 1);
            Ok(passages
                .iter()
                .map(|passage| {
                    if passage.contains("beta") {
                        vec![0.0, 1.0]
                    } else {
                        vec![1.0, 0.0]
                    }
                })
                .collect())
        }

        fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
            self.query_calls.set(self.query_calls.get() + 1);
            if self.query_error {
                return Err(AppError::ConfigError("query failed".to_string()));
            }
            Ok(if query.contains("beta") {
                Some(vec![0.0, 1.0])
            } else {
                self.query_embedding.clone()
            })
        }
    }

    struct Fake {
        created: Rc<Cell<usize>>,
        passage_calls: Rc<Cell<usize>>,
        query_calls: Rc<Cell<usize>>,
        query_embedding: Option<Vec<f32>>,
        query_error: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                created: Rc::default(),
                passage_calls: Rc::default(),
                query_calls: Rc::default(),
                query_embedding: Some(vec![1.0, 0.0]),
                query_error: false,
            }
        }

        fn factory(&self) -> impl FnMut() -> Result<Box<dyn SemanticEmbedder>> + use<> {
            let created = Rc::clone(&self.created);
            let passage_calls = Rc::clone(&self.passage_calls);
            let query_calls = Rc::clone(&self.query_calls);
            let query_embedding = self.query_embedding.clone();
            let query_error = self.query_error;
            move || {
                created.set(created.get() + 1);
                Ok(Box::new(FakeEmbedder {
                    passage_calls: Rc::clone(&passage_calls),
                    query_calls: Rc::clone(&query_calls),
                    query_embedding: query_embedding.clone(),
                    query_error,
                }))
            }
        }
    }

    fn test_conversation(path: &str, title: &str, semantic_turns: Vec<String>) -> Conversation {
        SemanticConversationFixture::new(path, semantic_turns)
            .with_title(title)
            .with_project("project-a", "/projects/project-a")
            .with_cwd(None)
            .with_summary(None)
            .build()
    }

    fn corpus() -> Vec<Conversation> {
        vec![
            test_conversation(
                "/projects/-proj-a/session-1.jsonl",
                "one",
                vec!["visible alpha".to_string()],
            ),
            test_conversation(
                "/projects/-proj-b/session-2.jsonl",
                "two",
                vec!["visible beta".to_string()],
            ),
        ]
    }

    fn sandbox() -> (tempfile::TempDir, HistoryEnv) {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        (root, env)
    }

    // --- upstream semantic_cli tests ---

    #[test]
    fn production_factory_refuses_to_load_the_real_model_in_tests() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let mut factory = fastembed_factory(&env);
        let Err(error) = factory() else {
            panic!("the real model must never load in tests");
        };
        assert!(error.to_string().contains("tests must inject"), "{error}");
        assert!(!env.semantic_model_dir().unwrap().exists());
    }

    #[test]
    fn interactive_refresh_writes_cache_once_before_query_ranking() {
        let conversations = corpus();
        let selected = vec![&conversations[0]];
        let candidates = semantic_index_candidates(&selected);
        let request = SemanticIndexRequest {
            query: "alpha",
            literal_filters: &[],
            full_corpus: &candidates,
            scope: &candidates,
            corpus_version: 1,
            prewarm: false,
            include_chunk_hits: false,
        };
        let mut state = SemanticIndexState::with_cache(
            ChunkConfig::default(),
            crate::semantic::cache::empty_embedding_cache(ChunkConfig::default()),
        );
        let fake = Fake {
            query_embedding: None,
            ..Fake::new()
        };
        let mut embedder = (fake.factory())().unwrap();
        let mut saved = Vec::new();
        let (refresh, response) = refresh_and_rank_interactive(
            &request,
            &mut state,
            embedder.as_mut(),
            |cache: &EmbeddingCache| saved.push(cache.entries.len()),
        )
        .unwrap();
        assert_eq!(refresh.indexed_chunk_count, 1);
        assert_eq!(saved, vec![1]);
        assert!(!response.query_embedding_returned);
        assert_eq!(fake.passage_calls.get(), 1);
        assert_eq!(fake.query_calls.get(), 1);
    }

    #[test]
    fn interactive_refresh_writes_cache_before_query_error() {
        let conversations = corpus();
        let selected = vec![&conversations[0]];
        let candidates = semantic_index_candidates(&selected);
        let request = SemanticIndexRequest {
            query: "alpha",
            literal_filters: &[],
            full_corpus: &candidates,
            scope: &candidates,
            corpus_version: 1,
            prewarm: false,
            include_chunk_hits: false,
        };
        let mut state = SemanticIndexState::with_cache(
            ChunkConfig::default(),
            crate::semantic::cache::empty_embedding_cache(ChunkConfig::default()),
        );
        let fake = Fake {
            query_error: true,
            ..Fake::new()
        };
        let mut embedder = (fake.factory())().unwrap();
        let mut saved = Vec::new();
        let Err(error) = refresh_and_rank_interactive(
            &request,
            &mut state,
            embedder.as_mut(),
            |cache: &EmbeddingCache| saved.push(cache.entries.len()),
        ) else {
            panic!("query error should propagate");
        };
        assert_eq!(saved, vec![1]);
        assert!(error.to_string().contains("query failed"));
    }

    #[test]
    fn selection_includes_all_conversations() {
        let conversations = corpus();
        assert_eq!(
            select_conversation_indices(&conversations, None),
            vec![0, 1]
        );
    }

    #[test]
    fn t014_local_selection_matches_claude_project_dirs_only() {
        // Upstream matched only the Claude project directory name: Pi/OMP
        // sessions (in their own session directories) are never local, even
        // when their working directory is the workspace.
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceScope::new(dir.path()).unwrap();
        let project = workspace.project_dir_name().to_string();
        let mut pi = test_conversation(
            "sessions/--work-project--/pi.jsonl",
            "pi",
            vec!["pi".to_string()],
        );
        pi.source = crate::history::Source::Pi;
        pi.project_path = Some(dir.path().to_path_buf());
        let mut omp = test_conversation("sessions/omp.jsonl", "omp", vec!["omp".to_string()]);
        omp.source = crate::history::Source::Omp;
        omp.cwd = Some(dir.path().to_path_buf());
        let claude = test_conversation(
            &format!("projects/{project}/claude.jsonl"),
            "claude",
            vec!["claude".to_string()],
        );
        let conversations = vec![pi, omp, claude];
        assert_eq!(
            select_conversation_indices(&conversations, Some(&workspace)),
            vec![2]
        );
        assert_eq!(
            select_conversation_indices(&conversations, None),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn selection_filters_to_the_workspace_without_changing_the_cwd() {
        // Upstream used std::env::set_current_dir here (W13).
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceScope::new(dir.path()).unwrap();
        let conversations = vec![
            test_conversation(
                &format!("projects/{}/session-1.jsonl", workspace.project_dir_name()),
                "local",
                vec!["local".to_string()],
            ),
            test_conversation(
                &format!(
                    "projects/{}--worktrees-feature/session-3.jsonl",
                    workspace.project_dir_name()
                ),
                "worktree",
                vec!["worktree".to_string()],
            ),
            test_conversation(
                "projects/other-project/session-2.jsonl",
                "other",
                vec!["other".to_string()],
            ),
        ];
        assert_eq!(
            select_conversation_indices(&conversations, Some(&workspace)),
            vec![0, 1]
        );
    }

    #[test]
    fn empty_conversation_message_mentions_local_scope() {
        assert_eq!(
            no_conversations_message(true),
            "No conversations available for semantic search in the current workspace."
        );
        assert_eq!(
            no_conversations_message(false),
            "No conversations available for semantic search."
        );
    }

    #[test]
    fn semantic_index_candidates_use_selected_slice_indices() {
        let conversations = corpus();
        let selected = vec![&conversations[1], &conversations[0]];
        let candidates = semantic_index_candidates(&selected);
        assert_eq!(candidates[0].index, 0);
        assert_eq!(
            candidates[0].conversation.custom_title.as_deref(),
            Some("two")
        );
        assert_eq!(candidates[1].index, 1);
        assert_eq!(
            candidates[1].conversation.custom_title.as_deref(),
            Some("one")
        );
    }

    #[test]
    fn empty_corpus_returns_before_model_initialization() {
        let (_root, env) = sandbox();
        let fake = Fake::new();
        let mut factory = fake.factory();
        let report = semantic_search_report(&env, "cache", &[], 1, None, &mut factory).unwrap();
        assert_eq!(report.outcome, SemanticSearchOutcome::NoConversations);
        assert_eq!(
            report.text,
            "No conversations available for semantic search.\n"
        );
        assert_eq!(fake.created.get(), 0);
    }

    #[test]
    fn quoted_only_exact_fallback_returns_literal_matches_newest_first() {
        let mut older = test_conversation("/projects/-p/session-1.jsonl", "literal needle", vec![]);
        older.timestamp = Local::now() - chrono::Duration::days(1);
        let newer = test_conversation("/projects/-p/session-2.jsonl", "literal needle", vec![]);
        let miss = test_conversation("/projects/-p/session-3.jsonl", "other", vec![]);
        let conversations = vec![older, newer, miss];
        let selected = conversations.iter().collect::<Vec<_>>();
        let parsed = ParsedQuery::parse("\"literal needle\"");
        assert_eq!(exact_literal_indices(&selected, &parsed), vec![1, 0]);

        let (_root, env) = sandbox();
        let fake = Fake::new();
        let mut factory = fake.factory();
        let report = semantic_search_report(
            &env,
            "\"literal needle\"",
            &conversations,
            1,
            None,
            &mut factory,
        )
        .unwrap();
        assert_eq!(report.outcome, SemanticSearchOutcome::Exact);
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].conversation_index, 1);
        assert_eq!(
            report.text,
            "# 1 exact | project-a | session-2\n     literal needle\n\n"
        );
        assert_eq!(fake.created.get(), 0);
    }

    #[test]
    fn quoted_uuid_exact_fallback_is_literal_search() {
        let uuid = "e7d318b1-4274-4ee2-a341-e94893b5df49";
        let conversations = vec![
            test_conversation("/projects/-p/session-1.jsonl", uuid, vec![]),
            test_conversation("/projects/-p/session-2.jsonl", "other", vec![]),
        ];
        let selected = conversations.iter().collect::<Vec<_>>();
        let parsed = ParsedQuery::parse(&format!("\"{uuid}\""));
        assert_eq!(parsed.semantic_text(), "");
        assert_eq!(exact_literal_indices(&selected, &parsed), vec![0]);
    }

    #[test]
    fn semantic_filters_use_smart_case_literals() {
        let conversations = [
            test_conversation(
                "/projects/-p/session-1.jsonl",
                "restaurant_signals lower phrase",
                vec!["visible alpha restaurant_signals".to_string()],
            ),
            test_conversation(
                "/projects/-p/session-2.jsonl",
                "RESTAURANT_SIGNALS lower phrase",
                vec!["visible alpha".to_string()],
            ),
        ];
        let chunks = conversations
            .iter()
            .enumerate()
            .map(
                |(conversation_index, conversation)| crate::semantic::types::EmbeddedChunk {
                    conversation_index,
                    source: SemanticChunkSource::VisibleDialogue,
                    session: format!("session-{}", conversation_index + 1),
                    chunk_index: 0,
                    text: conversation.semantic_turns[0].clone(),
                    message_range: crate::agent::refs::MessageRange::single(1),
                    embedding: vec![1.0, 0.0],
                },
            )
            .collect::<Vec<_>>();
        let insensitive = filter_embedded_chunks_by_literals(
            chunks.clone(),
            ParsedQuery::parse("alpha \"restaurant_signals\"").literals(),
        );
        let sensitive = filter_embedded_chunks_by_literals(
            chunks,
            ParsedQuery::parse("alpha \"RESTAURANT_SIGNALS\"").literals(),
        );
        assert_eq!(insensitive.len(), 1);
        assert_eq!(insensitive[0].conversation_index, 0);
        assert!(sensitive.is_empty());
    }

    #[test]
    fn parsed_query_debug_output_reports_intent_and_literals() {
        let output = format_parsed_query(&ParsedQuery::parse(
            "alpha \"RESTAURANT_SIGNALS\" \"lower phrase\"",
        ));
        assert!(output.contains("intent: \"alpha\""));
        assert!(output.contains("\"RESTAURANT_SIGNALS\" (Sensitive)"));
        assert!(output.contains("\"lower phrase\" (Insensitive)"));
    }

    // --- generate / search / debug / status / clear ---

    #[test]
    fn generate_embeds_protects_and_reports_progress() {
        let (_root, env) = sandbox();
        let conversations = corpus();
        let fake = Fake::new();
        let mut factory = fake.factory();
        let mut events = Vec::new();
        let report = generate_semantic_cache(
            &env,
            &conversations,
            None,
            &TimeFilter::default(),
            &mut factory,
            &mut |event| events.push(event),
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(report.conversation_count, 2);
        assert!(report.chunk_count >= 2);
        assert_eq!(
            report.outcome,
            GenerateOutcome::Cached {
                cached_chunks: report.chunk_count
            }
        );
        assert_eq!(fake.created.get(), 1);
        assert!(matches!(
            events.first(),
            Some(GenerateProgress::Checking {
                conversation_count: 2,
                ..
            })
        ));
        assert!(events
            .iter()
            .any(|event| matches!(event, GenerateProgress::Embedded { completed, total } if completed == total)));
        let lines: Vec<&str> = report.text.lines().collect();
        assert_eq!(lines[0], events[0].to_string());
        assert_eq!(
            *lines.last().unwrap(),
            format!(
                "Semantic cache: cached {} chunk(s) from 2 recent conversation(s).",
                report.chunk_count
            )
        );

        // Everything is cached and protected, inside the sandbox only.
        let cache_file = env.embedding_cache_file().unwrap();
        assert_eq!(report.cache_file.as_ref(), Some(&cache_file));
        let status = semantic_status(&env, Some(&conversations), None);
        let CacheFileState::Ready {
            entries,
            protected_entries,
            over_capacity,
        } = status.cache
        else {
            panic!("{:?}", status.cache);
        };
        assert_eq!(entries, protected_entries);
        assert!(!over_capacity);
        let coverage = status.coverage.unwrap();
        assert_eq!(coverage.missing, 0);
        assert_eq!(coverage.cached, coverage.unique_chunks);
        assert_eq!(coverage.conversations, 2);
        assert!(!status.model_downloaded);
        assert_eq!(status.model_name, MODEL_NAME);

        // A second run embeds nothing new.
        let before = fake.passage_calls.get();
        generate_semantic_cache(
            &env,
            &conversations,
            None,
            &TimeFilter::default(),
            &mut factory,
            &mut |_| {},
            &SemanticCancellationToken::new(),
        )
        .unwrap();
        assert_eq!(fake.passage_calls.get(), before);
    }

    #[test]
    fn generate_refuses_time_filters_and_handles_empty_selections() {
        let (root, env) = sandbox();
        let fake = Fake::new();
        let mut factory = fake.factory();
        let filter = crate::agent::args::TimeRangeArgs {
            since: Some("2d".parse().unwrap()),
            ..Default::default()
        }
        .resolve()
        .unwrap();
        let error = generate_semantic_cache(
            &env,
            &corpus(),
            None,
            &filter,
            &mut factory,
            &mut |_| {},
            &SemanticCancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("Configuration error: {GENERATE_WITH_TIME_FILTER_ERROR}")
        );

        let workspace =
            WorkspaceScope::new(root.path().join("nowhere").join("at").join("all")).unwrap();
        let report = generate_semantic_cache(
            &env,
            &corpus(),
            Some(&workspace),
            &TimeFilter::default(),
            &mut factory,
            &mut |_| {},
            &SemanticCancellationToken::new(),
        )
        .unwrap();
        assert_eq!(report.outcome, GenerateOutcome::NoConversations);
        assert_eq!(
            report.text,
            "No conversations available for semantic search in the current workspace.\n"
        );

        let silent = vec![test_conversation("/projects/-p/s.jsonl", "", vec![])];
        let mut silent = silent;
        silent[0].semantic_route_text.clear();
        silent[0].custom_title = None;
        silent[0].summary = None;
        let report = generate_semantic_cache(
            &env,
            &silent,
            None,
            &TimeFilter::default(),
            &mut factory,
            &mut |_| {},
            &SemanticCancellationToken::new(),
        )
        .unwrap();
        assert_eq!(report.outcome, GenerateOutcome::NoText);
        assert_eq!(fake.created.get(), 0);
    }

    #[test]
    fn generate_can_be_cancelled() {
        let (_root, env) = sandbox();
        let fake = Fake::new();
        let mut factory = fake.factory();
        let cancel = SemanticCancellationToken::new().child();
        cancel.cancel();
        let error = generate_semantic_cache(
            &env,
            &corpus(),
            None,
            &TimeFilter::default(),
            &mut factory,
            &mut |_| {},
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(error, AppError::SemanticSearchCancelled));
    }

    #[test]
    fn search_report_ranks_and_maps_back_to_the_corpus() {
        let (_root, env) = sandbox();
        let conversations = corpus();
        let fake = Fake::new();
        let mut factory = fake.factory();
        let report =
            semantic_search_report(&env, "beta", &conversations, 5, None, &mut factory).unwrap();
        assert_eq!(report.outcome, SemanticSearchOutcome::Ranked);
        assert_eq!(report.conversation_count, 2);
        assert!(!report.rows.is_empty());
        let best = &report.rows[0];
        assert_eq!(best.conversation_index, 1);
        assert_eq!(best.session_or_path, "session-2");
        assert_eq!(best.title, "two");
        assert!(best.hit.is_some());
        assert!(report.text.starts_with(&format!(
            "Semantic search: searching {} cached chunk(s) from 2 recent conversation(s) with fastembed {MODEL_NAME}\n",
            report.searched_chunks
        )));
        assert!(report.text.contains(&best.text));
        // The passages were cached in the sandbox.
        assert!(env.embedding_cache_file().unwrap().is_file());

        let empty =
            semantic_search_report(&env, "   ", &conversations, 5, None, &mut factory).unwrap();
        assert_eq!(empty.outcome, SemanticSearchOutcome::EmptyQuery);
        assert_eq!(empty.text, "Query is empty.\n");
    }

    #[test]
    fn debug_report_ranks_only_cached_chunks() {
        let (_root, env) = sandbox();
        let conversations = corpus();
        let fake = Fake::new();
        let mut factory = fake.factory();

        // Nothing cached yet: no ranking, no embedder.
        let report =
            semantic_debug_report(&env, "beta", &conversations, None, &mut factory).unwrap();
        assert_eq!(report.outcome, SemanticDebugOutcome::NoCachedChunks);
        assert_eq!(fake.created.get(), 0);
        let cache = report.cache.unwrap();
        assert_eq!(cache.hits, 0);
        assert_eq!(cache.misses, report.chunk_stats.unwrap().chunks);
        assert!(report.text.starts_with(
            "intent: \"beta\"\nliterals: none\n\nSemantic debug: selected 2 conversation(s).\n"
        ));
        assert!(report.text.contains(&format!(
            "Semantic debug: embedding cache: {}\n",
            env.embedding_cache_file().unwrap().display()
        )));
        assert_eq!(report.missing.len(), 2);
        assert_eq!(report.chunk_heavy.len(), 2);

        generate_semantic_cache(
            &env,
            &conversations,
            None,
            &TimeFilter::default(),
            &mut factory,
            &mut |_| {},
            &SemanticCancellationToken::new(),
        )
        .unwrap();
        let report =
            semantic_debug_report(&env, "beta", &conversations, None, &mut factory).unwrap();
        assert_eq!(report.outcome, SemanticDebugOutcome::Ranked);
        assert_eq!(report.rows[0].conversation_index, 1);
        assert_eq!(report.cache.unwrap().misses, 0);
        assert!(report.text.contains(&format!(
            "Semantic debug: ranked {} cached chunk(s) with fastembed {MODEL_NAME}.\n",
            report.ranked_chunks
        )));

        let exact =
            semantic_debug_report(&env, "\"two\"", &conversations, None, &mut factory).unwrap();
        assert_eq!(exact.outcome, SemanticDebugOutcome::Exact { matches: 1 });
        assert!(
            exact
                .text
                .contains("Semantic debug: exact literal matches=1.\n")
        );
    }

    #[test]
    fn status_reports_missing_and_incompatible_caches() {
        let (_root, env) = sandbox();
        let status = semantic_status(&env, Some(&corpus()), None);
        assert_eq!(status.cache, CacheFileState::Missing);
        assert_eq!(status.cache_file_size, None);
        let coverage = status.coverage.unwrap();
        assert_eq!(coverage.cached, 0);
        assert_eq!(coverage.missing, coverage.unique_chunks);

        let path = env.embedding_cache_file().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"garbage").unwrap();
        assert!(matches!(
            semantic_status(&env, None, None).cache,
            CacheFileState::Unreadable(_)
        ));
        let mut old = crate::semantic::cache::empty_embedding_cache(ChunkConfig::default());
        old.schema_version = CACHE_SCHEMA_VERSION - 1;
        std::fs::write(&path, bincode::serialize(&old).unwrap()).unwrap();
        assert_eq!(
            semantic_status(&env, None, None).cache,
            CacheFileState::Incompatible {
                schema_version: CACHE_SCHEMA_VERSION - 1,
                model: MODEL_NAME.to_string()
            }
        );
        // Status never rewrites the file.
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bincode::serialize(&old).unwrap()
        );

        let model = env.semantic_model_dir().unwrap();
        std::fs::create_dir_all(model.join("models--x")).unwrap();
        std::fs::write(model.join("models--x").join("w.onnx"), b"12345").unwrap();
        let status = semantic_status(&env, None, None);
        assert!(status.model_downloaded);
        assert_eq!(status.model_dir_size, 5);
    }

    #[test]
    fn clear_removes_only_this_crates_semantic_dir() {
        let (root, env) = sandbox();
        assert_eq!(
            clear_semantic_cache(&env).unwrap(),
            ClearReport::new(false, env.semantic_cache_dir())
        );
        let path = env.embedding_cache_file().unwrap();
        std::fs::create_dir_all(env.semantic_model_dir().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();

        let report = clear_embedding_cache(&env).unwrap();
        assert!(report.removed);
        assert!(!path.exists());
        assert!(env.semantic_model_dir().unwrap().exists());
        assert_eq!(
            clear_embedding_cache(&env).unwrap().message,
            "Semantic cache is already empty."
        );

        let report = clear_semantic_cache(&env).unwrap();
        assert!(report.removed);
        assert_eq!(report.message, "Semantic cache cleared.");
        let semantic = env.semantic_cache_dir().unwrap();
        assert!(!env.semantic_model_dir().unwrap().exists());
        assert_eq!(
            std::fs::read_dir(&semantic)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from(
                crate::semantic::cache::CACHE_LOCK_FILE_NAME
            )],
            "only the lock file stays"
        );
        assert_eq!(
            clear_semantic_cache(&env).unwrap().message,
            "Semantic cache is already empty."
        );

        // A cache root pointed at the CLI's cache is refused.
        let cli_semantic = root
            .path()
            .join(".cache")
            .join("claude-history")
            .join("semantic");
        std::fs::create_dir_all(&cli_semantic).unwrap();
        let misconfigured = env
            .clone()
            .with_cache_root(root.path().join(".cache").join("claude-history"));
        assert!(clear_semantic_cache(&misconfigured).is_err());
        assert!(clear_embedding_cache(&misconfigured).is_err());
        assert!(cli_semantic.exists());
    }
}
