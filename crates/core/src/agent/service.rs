//! Agent protocol requests (`agent search|within|read|outline`).
//!
//! Upstream `claude-history agent ...` printed the compact protocol text to
//! stdout and semantic-search notices to stderr. Here [`execute`] returns an
//! [`AgentResponse`] with both the structured answer and that exact text
//! (stdout, stderr, exit code); [`execute_text`] / [`AgentService::execute`]
//! return just the protocol text, with errors carrying the pre-formatted
//! protocol envelope ([`AppError::AgentProtocol`]). Notices are collected by
//! the [`AgentService`] (see [`AgentNotice`]) instead of being printed (R6).
//! Every location comes from the [`HistoryEnv`] and every setting from the
//! [`ConfigFile`] passed in (R1): nothing reads the process environment.

use crate::agent;
use crate::agent::args::{
    AgentOutlineArgs, AgentReadArgs, AgentRequest, AgentSearchArgs, AgentWithinArgs,
};
use crate::agent::diagnostic::{AgentError, AgentErrorKind, AgentWarning, AgentWarningKind};
use crate::agent::response::{
    AgentConversationInfo, AgentOutlineResult, AgentReadResult, AgentResponse, AgentResult,
    AgentSearchResult, conversations_for_output,
};
use crate::config::{AgentConfig, AgentScopeConfig, ConfigFile};
use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history;
use crate::search;
use crate::search::mode::SearchMode;
use crate::semantic;
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::index::SemanticIndexProgress;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

type ResolvedReadRefs = Vec<(agent::refs::ReadRef, agent::refs::ResolvedConversation)>;

const DEFAULT_OUTPUT_CHARS: usize = 6000;
const DEFAULT_SEARCH_TOP: usize = 10;
const DEFAULT_WITHIN_TOP: usize = 20;
const DEFAULT_HITS_PER_CONVERSATION: usize = 2;

fn configured_usize(cli_value: Option<usize>, default: usize, configured: Option<usize>) -> usize {
    cli_value.or(configured).unwrap_or(default)
}

fn configured_visibility(cli_value: bool, configured: Option<bool>) -> bool {
    cli_value || configured.unwrap_or(false)
}

fn configured_render_policy(config: &AgentConfig) -> agent::visibility::ContentVisibility {
    agent::visibility::ContentVisibility {
        tools: config.tools.unwrap_or(false),
        tool_results: config.tool_results.unwrap_or(false),
        thinking: config.thinking.unwrap_or(false),
        subagents: config.subagents.unwrap_or(false),
    }
}

fn apply_configured_render_policy(
    output: &mut agent::search::AgentSearchOutput,
    config: &AgentConfig,
) {
    let policy = configured_render_policy(config);
    for hit in &mut output.hits {
        hit.render_options.merge(policy);
    }
    for hit in output
        .groups
        .iter_mut()
        .flat_map(|group| group.hits.iter_mut())
    {
        hit.render_options.merge(policy);
    }
}

fn configured_budget(
    no_budget: bool,
    cli_budget: Option<usize>,
    configured: Option<usize>,
) -> Option<usize> {
    (!no_budget).then(|| configured_usize(cli_budget, DEFAULT_OUTPUT_CHARS, configured))
}

fn configured_scope(
    args: &AgentSearchArgs,
    config: &AgentConfig,
) -> agent::search::AgentSearchScope {
    if args.local {
        agent::search::AgentSearchScope::Local
    } else if args.all {
        agent::search::AgentSearchScope::Global
    } else {
        match config.scope.unwrap_or(AgentScopeConfig::Global) {
            AgentScopeConfig::Global => agent::search::AgentSearchScope::Global,
            AgentScopeConfig::Local => agent::search::AgentSearchScope::Local,
        }
    }
}

fn project_is_excluded(path: &Path, excluded: &[String]) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|project| {
            excluded
                .iter()
                .any(|excluded| history::is_same_project(project, excluded))
        })
}

/// Creates the semantic embedder used by semantic and hybrid requests.
pub type EmbedderFactory = Box<dyn Fn() -> Result<Box<dyn SemanticEmbedder>>>;

/// Receives each [`AgentNotice`] as it is produced.
pub type NoticeSink = Box<dyn FnMut(&AgentNotice)>;

/// A side-channel message upstream printed to stderr while serving a
/// request. Its [`fmt::Display`] text is upstream's exact line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentNotice {
    /// Semantic index progress (`Semantic search: {progress:?}`).
    SemanticProgress(SemanticIndexProgress),
    /// Passages that are still not embedded after the request.
    UncachedPassages {
        count: usize,
        /// `false` for global search (no new embeddings are computed),
        /// `true` for `within` (a bounded top-up ran).
        bounded_top_up: bool,
    },
}

impl fmt::Display for AgentNotice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentNotice::SemanticProgress(progress) => {
                write!(f, "Semantic search: {progress:?}")
            }
            AgentNotice::UncachedPassages {
                count,
                bounded_top_up: false,
            } => write!(
                f,
                "Semantic search: {count} passage(s) are not cached; run claude-history --generate-semantic-cache"
            ),
            AgentNotice::UncachedPassages {
                count,
                bounded_top_up: true,
            } => write!(
                f,
                "Semantic search: {count} passage(s) remain uncached after the bounded top-up; rerun this search to cache more"
            ),
        }
    }
}

/// Serves agent requests. Holds the environment, the effective
/// configuration (upstream re-read `config.toml` per request; core takes it
/// explicitly), a per-invocation transcript cache, and collected notices.
#[derive(Default)]
pub struct AgentService {
    env: HistoryEnv,
    config: ConfigFile,
    embedder_factory: Option<EmbedderFactory>,
    notices: RefCell<Vec<AgentNotice>>,
    notice_sink: RefCell<Option<NoticeSink>>,
    transcripts: RefCell<
        HashMap<PathBuf, std::result::Result<agent::transcript::AgentTranscript, AgentError>>,
    >,
    cancellation: Option<semantic::types::SemanticCancellationToken>,
    #[cfg(test)]
    transcript_parse_count: std::cell::Cell<usize>,
}

/// Run one agent request and return its [`AgentResponse`]: the structured
/// answer (hits, messages, outline records, diagnostics) and the exact text
/// the CLI printed (`stdout`, `stderr`, `exit_code`).
///
/// The request is trimmed and validated first ([`AgentRequest::normalized`],
/// the checks upstream's clap parser applied); invalid requests fail with
/// an `invalid-ref` envelope like every other error.
pub fn execute(env: &HistoryEnv, config: &ConfigFile, request: AgentRequest) -> AgentResponse {
    AgentService::new(env.clone(), config.clone()).respond(request)
}

/// Run one agent request and return only the compact protocol text. On
/// failure the error is [`AppError::AgentProtocol`] holding the formatted
/// `protocol agent-error` envelope (upstream printed it to stderr and
/// exited 1).
pub fn execute_text(
    env: &HistoryEnv,
    config: &ConfigFile,
    request: AgentRequest,
) -> Result<String> {
    AgentService::new(env.clone(), config.clone()).execute(request)
}

/// A successfully served request: protocol text plus structured answer.
pub(crate) struct Served {
    text: String,
    result: AgentResult,
    warnings: Vec<AgentWarning>,
}

impl Served {
    /// `agent search` / `agent within` output, with the conversations it
    /// refers to resolved from `resolved`.
    fn search(
        output: agent::search::AgentSearchOutput,
        warnings: Vec<AgentWarning>,
        resolved: &[agent::refs::ResolvedConversation],
    ) -> Self {
        let text = agent::search::format_agent_output_with_warnings(&output, &warnings);
        let conversations = conversations_for_output(&output, resolved);
        let protocol = output.protocol;
        let result = AgentSearchResult {
            output,
            conversations,
        };
        Self {
            text,
            result: match protocol {
                agent::search::AgentProtocolKind::Search => AgentResult::Search(result),
                agent::search::AgentProtocolKind::Within => AgentResult::Within(result),
            },
            warnings,
        }
    }
}

/// The error envelope upstream printed (and the structured error behind it).
fn error_envelope(error: AppError) -> (String, Option<AgentError>) {
    match structured_agent_error(error) {
        AppError::Agent(error) => (agent::diagnostic::format_error(&error), Some(error)),
        AppError::AgentProtocol(text) => (text, None),
        other => (format!("Error: {other}\n"), None),
    }
}

impl AgentService {
    pub fn new(env: HistoryEnv, config: ConfigFile) -> Self {
        Self {
            env,
            config,
            ..Self::default()
        }
    }

    /// Use `factory` instead of the fastembed model (for tests, or to share
    /// an already loaded model).
    pub fn with_embedder_factory(mut self, factory: EmbedderFactory) -> Self {
        self.embedder_factory = Some(factory);
        self
    }

    /// Also deliver every notice to `sink` as it happens (for progress UI).
    pub fn with_notice_sink(self, sink: NoticeSink) -> Self {
        *self.notice_sink.borrow_mut() = Some(sink);
        self
    }

    /// Let `token` cancel the requests this service serves (K7): it is
    /// checked between conversations (every transcript load), between the
    /// steps of a search, and between retrieval and embedding batches of
    /// semantic and hybrid requests. A cancelled request fails with
    /// [`AppError::SemanticSearchCancelled`]; [`AgentService::respond`]
    /// returns exit code 1, no structured error and the stderr text
    /// `Error: Semantic search cancelled\n`
    /// ([`AgentResponse::CANCELLED_TEXT`], after any notices).
    pub fn with_cancellation(mut self, token: semantic::types::SemanticCancellationToken) -> Self {
        self.cancellation = Some(token);
        self
    }

    /// `Err(SemanticSearchCancelled)` once the cancellation token fired.
    fn check_cancelled(&self) -> Result<()> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(semantic::types::SemanticCancellationToken::is_cancelled)
        {
            Err(AppError::SemanticSearchCancelled)
        } else {
            Ok(())
        }
    }

    /// The token semantic work checks: the service's token, or one that
    /// never fires.
    fn semantic_cancellation(&self) -> semantic::types::SemanticCancellationToken {
        self.cancellation.clone().unwrap_or_default()
    }

    pub fn env(&self) -> &HistoryEnv {
        &self.env
    }

    pub fn config(&self) -> &ConfigFile {
        &self.config
    }

    /// Notices collected so far (and clears them).
    pub fn take_notices(&self) -> Vec<AgentNotice> {
        std::mem::take(&mut self.notices.borrow_mut())
    }

    fn notify(&self, notice: AgentNotice) {
        if let Some(sink) = self.notice_sink.borrow_mut().as_mut() {
            sink(&notice);
        }
        self.notices.borrow_mut().push(notice);
    }

    fn agent_config(&self) -> AgentConfig {
        self.config.agent.clone().unwrap_or_default()
    }

    fn create_embedder(&self) -> Result<Box<dyn SemanticEmbedder>> {
        if let Some(factory) = &self.embedder_factory {
            return factory();
        }
        let model_dir = self.env.semantic_model_dir().ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine home directory",
            ))
        })?;
        // Refuses under cfg(test): tests inject an embedder factory.
        semantic::fastembed::quiet_embedder_in(model_dir)
    }

    /// Run `request` and return the compact protocol text (errors carry the
    /// formatted envelope, see [`execute_text`]). Notices stay collected
    /// ([`AgentService::take_notices`]).
    pub fn execute(&mut self, request: AgentRequest) -> Result<String> {
        self.serve(request)
            .map(|served| served.text)
            .map_err(|error| {
                let error = structured_agent_error(error);
                if let AppError::Agent(agent_error) = error {
                    AppError::AgentProtocol(agent::diagnostic::format_error(&agent_error))
                } else {
                    error
                }
            })
    }

    /// Run `request` and return the full [`AgentResponse`] (see
    /// [`execute`]). The notices produced are moved into the response.
    pub fn respond(&mut self, request: AgentRequest) -> AgentResponse {
        let served = self.serve(request);
        let notices = self.take_notices();
        match served {
            Ok(served) => {
                AgentResponse::success(served.text, served.result, served.warnings, notices)
            }
            Err(error) => {
                let (envelope, error) = error_envelope(error);
                AgentResponse::failure(envelope, error, notices)
            }
        }
    }

    fn serve(&mut self, request: AgentRequest) -> Result<Served> {
        let served = self.execute_inner(request)?;
        ensure_complete_compact_output(&served.text)?;
        Ok(served)
    }

    fn execute_inner(&mut self, request: AgentRequest) -> Result<Served> {
        match request.normalized().map_err(AppError::ConfigError)? {
            AgentRequest::Search(args) => self.run_search(&args),
            AgentRequest::Within(args) => self.run_within(&args),
            AgentRequest::Read(args) => self.serve_read(&args, None),
            AgentRequest::Outline(args) => self.serve_outline(&args, None),
        }
    }

    fn load_transcript(&self, path: &Path) -> Result<agent::transcript::AgentTranscript> {
        // Checked between conversations: every search loads transcripts one
        // by one.
        self.check_cancelled()?;
        if let Some(cached) = self.transcripts.borrow().get(path) {
            return cached.clone().map_err(AppError::from);
        }
        #[cfg(test)]
        self.transcript_parse_count
            .set(self.transcript_parse_count.get() + 1);
        let loaded = agent::transcript::AgentTranscript::load(path).map_err(|error| match error {
            AppError::Agent(error) => error,
            AppError::Io(error) => AgentError::io(
                Some(&path.to_string_lossy()),
                format!("failed to read transcript: {error}"),
            ),
            AppError::Json(error) => AgentError::malformed_transcript(
                Some(&path.to_string_lossy()),
                format!("failed to parse transcript JSONL: {error}"),
            ),
            error => {
                AgentError::malformed_transcript(Some(&path.to_string_lossy()), error.to_string())
            }
        });
        self.transcripts
            .borrow_mut()
            .insert(path.to_path_buf(), loaded.clone());
        loaded.map_err(AppError::from)
    }

    fn run_search(&self, args: &AgentSearchArgs) -> Result<Served> {
        let search_config = self.config.search.clone().unwrap_or_default();
        let agent_config = self.agent_config();
        // Resolved before loading so an inverted range fails without paying for
        // a full corpus parse.
        let time = args.time.resolve()?;
        self.check_cancelled()?;
        let mut conversations = history::load_all_conversations(&self.env, false, None)?;
        self.check_cancelled()?;
        conversations.retain(|conversation| {
            !project_is_excluded(&conversation.path, &agent_config.exclude_projects)
                && time.matches(conversation.timestamp)
        });
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        let scope = configured_scope(args, &agent_config);
        // Upstream derived the local scope from the process cwd; core uses
        // the environment's workspace (W1: encoded from the non-canonical
        // path).
        let workspace = if scope == agent::search::AgentSearchScope::Local {
            self.env.workspace()?
        } else {
            None
        };
        let current_project_dir_name = workspace
            .as_ref()
            .map(|workspace| workspace.project_dir_name().to_string());
        let scoped = agent::search::scoped_conversation_inputs(
            &conversations,
            scope,
            current_project_dir_name.as_deref(),
        )?;
        let request = agent::search::AgentSearchRequest {
            query: args.query.clone(),
            top: configured_usize(args.top, DEFAULT_SEARCH_TOP, agent_config.top),
            cli_mode: args.mode_override(),
            config_mode: agent_config.mode.or(search_config.mode),
            tui_semantic_search: None,
            flat: args.flat,
            hits_per_conversation: configured_usize(
                args.hits_per_conv,
                DEFAULT_HITS_PER_CONVERSATION,
                agent_config.hits_per_conversation,
            ),
            retrieval_hits_per_conversation: None,
            all_hits: args.all_hits,
            budget: configured_budget(args.no_budget, args.budget, agent_config.output_chars),
        };
        let mode = agent::search::effective_agent_mode(
            &request.query,
            request.cli_mode,
            request.config_mode,
            request.tui_semantic_search,
        );
        let (mut keys, mut base_warnings) = discover_agent_keys(&self.env, workspace.as_ref())?;
        keys.retain(|key| !project_is_excluded(&key.path, &agent_config.exclude_projects));
        if time.is_active() {
            // Key discovery walks the projects directory independently, so
            // without this every conversation outside the window would be
            // reported as a skipped transcript rather than simply filtered out.
            // Tested against each file's own timestamp, not against membership
            // in `conversations`, so that transcripts inside the window which
            // failed to parse still report their diagnostics.
            keys.retain(|key| transcript_timestamp(&key.path).is_none_or(|at| time.matches(at)));
        }
        base_warnings.extend(warnings_for_skipped_transcripts(
            self,
            &conversations,
            &keys,
        ));
        self.check_cancelled()?;
        match mode {
            SearchMode::Lexical | SearchMode::Exact => {
                let ranked = lexically_rank_scoped(&conversations, &args.query, &scoped);
                self.check_cancelled()?;
                let warnings = RefCell::new(base_warnings.clone());
                let mut output = agent::search::run_global_lexical_search_reporting(
                    &request,
                    &conversations,
                    &keys,
                    &ranked,
                    |key| self.load_transcript(&key.path),
                    |key, error| {
                        warnings.borrow_mut().push(AgentWarning::from_app_error(
                            error,
                            Some(
                                &agent::refs::resolved_conversation_for_key(&keys, key)
                                    .reference
                                    .canonical(),
                            ),
                        ));
                    },
                )?;
                // Transcript loads after a cancel fail fast; report the
                // cancel, not those failures.
                self.check_cancelled()?;
                apply_configured_render_policy(&mut output, &agent_config);
                return Ok(Served::search(
                    output,
                    warnings.into_inner(),
                    &agent::refs::resolved_conversations_for_keys(&keys),
                ));
            }
            SearchMode::Semantic => {
                let (mut output, mut warnings) =
                    run_agent_semantic_search(self, &request, &conversations, &keys, &scoped)?;
                self.check_cancelled()?;
                apply_configured_render_policy(&mut output, &agent_config);
                warnings.splice(0..0, base_warnings);
                return Ok(Served::search(
                    output,
                    warnings,
                    &agent::refs::resolved_conversations_for_keys(&keys),
                ));
            }
            SearchMode::Hybrid => {
                let lexical_request = agent::search::AgentSearchRequest {
                    top: agent::search::modality_candidate_depth(&request),
                    cli_mode: Some(SearchMode::Lexical),
                    flat: true,
                    retrieval_hits_per_conversation: Some(
                        request.hits_per_conversation.saturating_mul(4).max(1),
                    ),
                    ..request.clone()
                };
                let ranked = lexically_rank_scoped(&conversations, &args.query, &scoped);
                let warnings = RefCell::new(base_warnings.clone());
                let lexical = agent::search::run_global_lexical_search_reporting(
                    &lexical_request,
                    &conversations,
                    &keys,
                    &ranked,
                    |key| self.load_transcript(&key.path),
                    |key, error| {
                        warnings.borrow_mut().push(AgentWarning::from_app_error(
                            error,
                            Some(
                                &agent::refs::resolved_conversation_for_key(&keys, key)
                                    .reference
                                    .canonical(),
                            ),
                        ));
                    },
                )?;
                self.check_cancelled()?;
                let inputs = agent_inputs_for_indices(&conversations, &keys, &scoped)?;
                match run_agent_semantic_hits(self, &args.query, &inputs) {
                    Ok((semantic, semantic_warnings)) => {
                        warnings.borrow_mut().extend(semantic_warnings);
                        let mut output = agent::search::run_global_hybrid_search(
                            &request, lexical, &semantic, &inputs,
                        );
                        attach_input_transcript_metadata(self, &mut output, &inputs);
                        self.check_cancelled()?;
                        apply_configured_render_policy(&mut output, &agent_config);
                        return Ok(Served::search(
                            output,
                            warnings.into_inner(),
                            &agent::refs::resolved_conversations_for_keys(&keys),
                        ));
                    }
                    // A cancel is not a semantic failure to fall back from.
                    Err(AppError::SemanticSearchCancelled) => {
                        return Err(AppError::SemanticSearchCancelled);
                    }
                    Err(error) => {
                        warnings
                            .borrow_mut()
                            .push(AgentWarning::from_app_error(&error, None));
                        let lexical = agent::search::run_global_lexical_search_reporting(
                            &request,
                            &conversations,
                            &keys,
                            &ranked,
                            |key| self.load_transcript(&key.path),
                            |key, error| {
                                warnings.borrow_mut().push(AgentWarning::from_app_error(
                                    error,
                                    Some(
                                        &agent::refs::resolved_conversation_for_key(&keys, key)
                                            .reference
                                            .canonical(),
                                    ),
                                ));
                            },
                        )?;
                        let mut output = lexical;
                        output.mode = SearchMode::Hybrid;
                        apply_configured_render_policy(&mut output, &agent_config);
                        return Ok(Served::search(
                            output,
                            warnings.into_inner(),
                            &agent::refs::resolved_conversations_for_keys(&keys),
                        ));
                    }
                }
            }
        }
    }

    fn run_within(&self, args: &AgentWithinArgs) -> Result<Served> {
        let search_config = self.config.search.clone().unwrap_or_default();
        let agent_config = self.agent_config();
        let (keys, _) = discover_agent_keys(&self.env, None)?;
        let resolved = resolve_agent_conversation_arg(&self.env, &args.conversation, Some(&keys))?;
        let transcript = self
            .load_transcript(&resolved.key.path)
            .map_err(|error| target_error(error, &resolved))?;
        let conversation = conversation_from_agent_transcript(&transcript, resolved.key.source);
        let transcript_warnings = transcript_warning(&transcript, &resolved.reference.canonical())
            .into_iter()
            .collect::<Vec<_>>();
        let request = agent::search::AgentWithinRequest {
            query: args.query.clone(),
            top: configured_usize(args.top, DEFAULT_WITHIN_TOP, agent_config.within_top),
            cli_mode: args.mode_override(),
            config_mode: agent_config.mode.or(search_config.mode),
            tui_semantic_search: None,
            budget: configured_budget(args.no_budget, args.budget, agent_config.output_chars),
        };
        let mode = agent::search::effective_agent_mode(
            &request.query,
            request.cli_mode,
            request.config_mode,
            request.tui_semantic_search,
        );
        let mut output = match mode {
            SearchMode::Lexical | SearchMode::Exact => agent::search::run_within_search(
                &request,
                &conversation,
                &resolved,
                &transcript,
                &[],
            ),
            SearchMode::Semantic => run_agent_within_semantic(
                self,
                &request,
                &conversation,
                &resolved,
                &transcript,
                SemanticToolContent::All,
            )?,
            SearchMode::Hybrid => {
                match run_agent_within_semantic(
                    self,
                    &request,
                    &conversation,
                    &resolved,
                    &transcript,
                    SemanticToolContent::MatchingQuery(&request.query),
                ) {
                    Ok(output) => output,
                    Err(AppError::SemanticSearchCancelled) => {
                        return Err(AppError::SemanticSearchCancelled);
                    }
                    Err(error) => {
                        let mut output = agent::search::run_within_search(
                            &agent::search::AgentWithinRequest {
                                cli_mode: Some(SearchMode::Lexical),
                                ..request.clone()
                            },
                            &conversation,
                            &resolved,
                            &transcript,
                            &[],
                        );
                        output.mode = SearchMode::Hybrid;
                        agent::search::attach_transcript_metadata(
                            &mut output,
                            &resolved,
                            &transcript,
                        );
                        apply_configured_render_policy(&mut output, &agent_config);
                        let mut warnings = transcript_warnings.clone();
                        warnings.push(AgentWarning::from_app_error(&error, None));
                        return Ok(Served::search(
                            output,
                            warnings,
                            std::slice::from_ref(&resolved),
                        ));
                    }
                }
            }
        };
        agent::search::attach_transcript_metadata(&mut output, &resolved, &transcript);
        apply_configured_render_policy(&mut output, &agent_config);
        Ok(Served::search(
            output,
            transcript_warnings,
            std::slice::from_ref(&resolved),
        ))
    }
}

/// Discover every Claude/Pi/OMP transcript key (public so a GUI can turn the
/// candidates of an ambiguous-ref error into durable full refs). With `local`, only the
/// workspace's Claude project directories (worktree-aware) and the Pi/OMP
/// sessions whose working directory is the workspace are returned.
pub fn discover_agent_keys(
    env: &HistoryEnv,
    local: Option<&WorkspaceScope>,
) -> Result<(Vec<agent::refs::AgentConversationKey>, Vec<AgentWarning>)> {
    let project_filter = local.map(WorkspaceScope::project_dir_name);
    // Upstream compared against the canonicalized process cwd.
    let current = local.map(WorkspaceScope::canonical_dir);
    let root = env.claude_projects_root().map_err(structured_agent_error)?;
    let projects = if root.exists() {
        std::fs::read_dir(&root)
            .map_err(|error| {
                AgentError::io(
                    Some(&root.to_string_lossy()),
                    format!("failed to list projects: {error}"),
                )
            })?
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut keys = Vec::new();
    let mut warnings = Vec::new();
    for project in projects {
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                warnings.push(AgentWarning {
                    kind: AgentWarningKind::Io,
                    reference: None,
                    detail: format!("failed to read project entry: {error}"),
                });
                continue;
            }
        };
        let project_path = project.path();
        if !project_path.is_dir() {
            continue;
        }
        let Some(project_name) = project_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if project_filter.is_some_and(|filter| !history::is_same_project(project_name, filter)) {
            continue;
        }
        let entries = match std::fs::read_dir(&project_path) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(AgentWarning {
                    kind: AgentWarningKind::Io,
                    reference: None,
                    detail: format!(
                        "failed to list project transcripts at {}: {error}",
                        project_path.display()
                    ),
                });
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(AgentWarning {
                        kind: AgentWarningKind::Io,
                        reference: None,
                        detail: format!(
                            "failed to read transcript entry in {}: {error}",
                            project_path.display()
                        ),
                    });
                    continue;
                }
            };
            let path = entry.path();
            let Some(filename) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
                && !filename.starts_with("agent-")
            {
                keys.push(agent::refs::AgentConversationKey::new(
                    project_name,
                    filename,
                    path,
                ));
            }
        }
    }
    if let Ok(pi_root) = history::pi_loader::session_root(env)
        && let Ok(pi_files) = history::pi_loader::discover_files(&pi_root)
    {
        for path in pi_files {
            let Ok(Some(projection)) = history::pi::parse_file(&path) else {
                continue;
            };
            if projection.source != history::Source::Pi {
                continue;
            }
            if project_filter.is_some() {
                // Upstream additionally re-encoded the canonicalized cwd and
                // compared it with the filter derived from the same cwd. On
                // Windows the canonical form (`\\?\C:\...`) never matched,
                // so local Pi/OMP sessions were always dropped (W1). The
                // workspace scope already is that project, so only the
                // working-directory equality remains.
                let cwd = projection
                    .header
                    .cwd
                    .canonicalize()
                    .unwrap_or_else(|_| projection.header.cwd.clone());
                if current.as_ref() != Some(&cwd) {
                    continue;
                }
            }
            let project_identity = projection
                .header
                .cwd
                .canonicalize()
                .unwrap_or(projection.header.cwd)
                .to_string_lossy()
                .into_owned();
            let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            keys.push(agent::refs::AgentConversationKey {
                source: history::Source::Pi,
                project_dir_name: project_identity,
                session_filename: filename.to_owned(),
                session_id: projection.header.id,
                path,
            });
        }
    }
    if let Ok(omp_root) = history::omp_loader::session_root(env)
        && let Ok(omp_files) = history::omp_loader::discover_files(&omp_root)
    {
        for path in omp_files {
            let Ok(Some(projection)) = history::pi::parse_omp_file(&path) else {
                continue;
            };
            if project_filter.is_some() {
                // Upstream additionally re-encoded the canonicalized cwd and
                // compared it with the filter derived from the same cwd. On
                // Windows the canonical form (`\\?\C:\...`) never matched,
                // so local Pi/OMP sessions were always dropped (W1). The
                // workspace scope already is that project, so only the
                // working-directory equality remains.
                let cwd = projection
                    .header
                    .cwd
                    .canonicalize()
                    .unwrap_or_else(|_| projection.header.cwd.clone());
                if current.as_ref() != Some(&cwd) {
                    continue;
                }
            }
            let project_identity = projection
                .header
                .cwd
                .canonicalize()
                .unwrap_or(projection.header.cwd)
                .to_string_lossy()
                .into_owned();
            let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            keys.push(agent::refs::AgentConversationKey {
                source: history::Source::Omp,
                project_dir_name: project_identity,
                session_filename: filename.to_owned(),
                session_id: projection.header.id,
                path,
            });
        }
    }
    if keys.is_empty() && !root.exists() {
        return Err(AgentError::io(
            Some(&root.to_string_lossy()),
            "no Claude, Pi, or OMP history storage is available",
        )
        .into());
    }
    keys.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((keys, warnings))
}

fn warnings_for_skipped_transcripts(
    service: &AgentService,
    conversations: &[history::Conversation],
    keys: &[agent::refs::AgentConversationKey],
) -> Vec<AgentWarning> {
    let key_paths = keys
        .iter()
        .map(|key| key.path.as_path())
        .collect::<std::collections::HashSet<_>>();
    let known = conversations
        .iter()
        .map(|conversation| conversation.path.as_path())
        .collect::<std::collections::HashSet<_>>();
    let mut warnings = Vec::new();
    for conversation in conversations {
        if key_paths.contains(conversation.path.as_path()) && !conversation.parse_errors.is_empty()
        {
            let key = agent::refs::AgentConversationKey::from_conversation(conversation).ok();
            warnings.push(AgentWarning {
                kind: crate::agent::diagnostic::AgentWarningKind::MalformedTranscript,
                reference: key.as_ref().map(|key| {
                    agent::refs::resolved_conversation_for_key(keys, key)
                        .reference
                        .canonical()
                }),
                detail: format!(
                    "transcript contains {} malformed JSONL record(s)",
                    conversation.parse_errors.len()
                ),
            });
        }
    }
    for key in keys {
        if known.contains(key.path.as_path()) {
            continue;
        }
        let reference = agent::refs::resolved_conversation_for_key(keys, key)
            .reference
            .canonical();
        match service.load_transcript(&key.path) {
            Ok(transcript) => warnings.push(AgentWarning::skipped(
                Some(&reference),
                if transcript.is_empty() {
                    "transcript has no visible messages"
                } else {
                    "transcript has no searchable conversation metadata"
                },
            )),
            Err(error) => warnings.push(AgentWarning::from_app_error(&error, Some(&reference))),
        }
    }
    warnings
}

fn conversation_from_agent_transcript(
    transcript: &agent::transcript::AgentTranscript,
    source: history::Source,
) -> history::Conversation {
    let message_text = transcript
        .messages
        .iter()
        .map(|message| {
            message
                .parts
                .iter()
                .filter_map(agent::transcript::agent_part_search_text)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    let preview = message_text
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ... ");
    let preview_last = message_text
        .iter()
        .rev()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ... ");
    let full_text = message_text.join(" ");
    let timestamp = std::fs::metadata(&transcript.path)
        .and_then(|metadata| metadata.modified())
        .map(chrono::DateTime::<chrono::Local>::from)
        .unwrap_or_else(|_| chrono::Local::now());
    let semantic_route_text = history::semantic_route_text(&full_text, "");
    history::Conversation {
        source,
        session_id: transcript
            .path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        path: transcript.path.clone(),
        index: 0,
        timestamp,
        preview: preview.clone(),
        preview_first: preview,
        preview_last,
        search_text_lower: crate::search::normalize_for_search(&full_text),
        full_text,
        agent_search_text: String::new(),
        semantic_route_text,
        semantic_turns: Vec::new(),
        semantic_turn_ranges: Vec::new(),
        project_name: None,
        project_path: None,
        cwd: None,
        message_count: transcript.messages.len(),
        parse_errors: Vec::new(),
        summary: transcript.summary.clone(),
        custom_title: transcript.custom_title.clone(),
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn transcript_warning(
    transcript: &agent::transcript::AgentTranscript,
    reference: &str,
) -> Option<AgentWarning> {
    transcript
        .malformed_warning_detail()
        .map(|detail| AgentWarning {
            kind: AgentWarningKind::MalformedTranscript,
            reference: Some(reference.to_string()),
            detail,
        })
}

fn target_error(error: AppError, resolved: &agent::refs::ResolvedConversation) -> AppError {
    match error {
        AppError::Agent(mut error) => {
            error.reference = Some(resolved.reference.canonical());
            error.into()
        }
        error => structured_agent_error(error),
    }
}

fn ensure_complete_compact_output(output: &str) -> Result<()> {
    if output.ends_with('\n') {
        Ok(())
    } else {
        Err(AgentError::new(
            AgentErrorKind::BudgetTooSmall,
            None,
            "output budget cannot fit a complete compact record; increase --budget",
        )
        .into())
    }
}

/// The timestamp a transcript is filtered on, matching how conversation
/// timestamps are derived. `None` when the file cannot be inspected, so callers
/// keep it rather than silently dropping it.
fn transcript_timestamp(path: &std::path::Path) -> Option<chrono::DateTime<chrono::Local>> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(chrono::DateTime::<chrono::Local>::from)
}

fn structured_agent_error(error: AppError) -> AppError {
    match error {
        // Not a protocol error: `Error: Semantic search cancelled` (K7).
        AppError::Agent(_) | AppError::SemanticSearchCancelled => error,
        AppError::SessionNotFound(reference) => AgentError::new(
            AgentErrorKind::NotFound,
            Some(&reference),
            "conversation was not found",
        )
        .into(),
        AppError::Json(error) => {
            AgentError::malformed_transcript(None, format!("failed to parse transcript: {error}"))
                .into()
        }
        AppError::Io(error) => AgentError::io(None, error.to_string()).into(),
        AppError::ConfigError(detail) => AgentError::invalid_ref("command", detail).into(),
        AppError::TimeFilter(error) => {
            AgentError::out_of_range(Some("command"), error.to_string()).into()
        }
        error => AgentError::io(None, error.to_string()).into(),
    }
}

fn run_agent_semantic_search(
    service: &AgentService,
    request: &agent::search::AgentSearchRequest,
    conversations: &[history::Conversation],
    keys: &[agent::refs::AgentConversationKey],
    indices: &[usize],
) -> Result<(agent::search::AgentSearchOutput, Vec<AgentWarning>)> {
    let inputs = agent_inputs_for_indices(conversations, keys, indices)?;
    let (semantic, warnings) = run_agent_semantic_hits(service, &request.query, &inputs)?;
    let mut output = agent::search::run_global_semantic_search(request, &inputs, &semantic);
    attach_input_transcript_metadata(service, &mut output, &inputs);
    Ok((output, warnings))
}

fn attach_input_transcript_metadata(
    service: &AgentService,
    output: &mut agent::search::AgentSearchOutput,
    inputs: &[agent::search::AgentConversationInput<'_>],
) {
    let referenced = output
        .hits
        .iter()
        .chain(output.groups.iter().flat_map(|group| group.hits.iter()))
        .map(|hit| hit.conversation_ref.clone())
        .collect::<std::collections::HashSet<_>>();
    for input in inputs {
        if !referenced.contains(input.resolved.reference.canonical().as_str()) {
            continue;
        }
        if let Ok(transcript) = service.load_transcript(&input.resolved.key.path) {
            agent::search::attach_transcript_metadata(output, &input.resolved, &transcript);
        }
    }
}

fn run_agent_semantic_hits(
    service: &AgentService,
    query: &str,
    inputs: &[agent::search::AgentConversationInput<'_>],
) -> Result<(Vec<semantic::types::SemanticHit>, Vec<AgentWarning>)> {
    let mut candidates = Vec::with_capacity(inputs.len().saturating_mul(2));
    for input in inputs {
        candidates.push(semantic::index::SemanticIndexCandidate {
            index: input.original_index,
            source: semantic::types::SemanticChunkSource::VisibleDialogue,
            conversation: std::sync::Arc::new(stripped_semantic_conversation(
                input.conversation,
                input.conversation.path.clone(),
                input.conversation.semantic_turns.clone(),
                input.conversation.semantic_turn_ranges.clone(),
            )),
        });
        if !input.conversation.semantic_turns.is_empty()
            && let Some(route) = agent_route_semantic_conversation(input.conversation)
        {
            candidates.push(semantic::index::SemanticIndexCandidate {
                index: input.original_index,
                source: semantic::types::SemanticChunkSource::AgentRoute,
                conversation: std::sync::Arc::new(route),
            });
        }
    }
    run_agent_semantic_hits_for_candidates(
        service,
        query,
        &candidates,
        semantic::types::MAX_GLOBAL_INTERACTIVE_PASSAGE_EMBEDDINGS,
    )
    .map(|hits| (hits, Vec::new()))
}

pub fn agent_route_semantic_conversation(
    conversation: &history::Conversation,
) -> Option<history::Conversation> {
    let route_text = if conversation.semantic_route_text.is_empty() {
        conversation
            .custom_title
            .as_deref()
            .or(conversation.summary.as_deref())?
            .to_string()
    } else {
        conversation.semantic_route_text.clone()
    };
    let file_name = conversation
        .path
        .file_name()
        .map(|name| format!("{}.agent-route-semantic", name.to_string_lossy()))?;
    Some(stripped_semantic_conversation(
        conversation,
        conversation.path.with_file_name(file_name),
        vec![route_text],
        vec![
            conversation
                .semantic_turn_ranges
                .first()
                .copied()
                .unwrap_or_else(|| agent::refs::MessageRange::single(1)),
        ],
    ))
}

fn stripped_semantic_conversation(
    conversation: &history::Conversation,
    path: PathBuf,
    semantic_turns: Vec<String>,
    semantic_turn_ranges: Vec<agent::refs::MessageRange>,
) -> history::Conversation {
    history::Conversation {
        source: conversation.source,
        session_id: conversation.session_id.clone(),
        path,
        index: conversation.index,
        timestamp: conversation.timestamp,
        preview: String::new(),
        preview_first: String::new(),
        preview_last: String::new(),
        full_text: String::new(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns,
        semantic_turn_ranges,
        search_text_lower: String::new(),
        project_name: None,
        project_path: None,
        cwd: None,
        message_count: conversation.message_count,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn run_agent_semantic_hits_for_candidates(
    service: &AgentService,
    query: &str,
    candidates: &[semantic::index::SemanticIndexCandidate],
    max_new_embeddings: usize,
) -> Result<Vec<semantic::types::SemanticHit>> {
    let parsed = search::query::ParsedQuery::parse(query);
    let request = semantic::index::SemanticIndexRequest {
        query: parsed.semantic_text(),
        literal_filters: parsed.literals(),
        full_corpus: candidates,
        scope: candidates,
        corpus_version: 3,
        prewarm: false,
        include_chunk_hits: true,
    };
    service.check_cancelled()?;
    let cache_path = service.env.embedding_cache_file();
    let mut state = semantic::index::SemanticIndexState::load(cache_path.as_deref());
    let mut embedder = service.create_embedder().map_err(|error| {
        AgentError::semantic_unavailable(format!("failed to initialize semantic search: {error}"))
    })?;
    // Checked between retrieval and embedding batches (K7).
    let cancellation = service.semantic_cancellation();
    let response = state
        .refresh_or_prewarm_with_budget(
            &request,
            embedder.as_mut(),
            &cancellation,
            Some(max_new_embeddings),
            |progress| service.notify(AgentNotice::SemanticProgress(progress)),
            semantic::cache::embedding_cache_writer(cache_path.clone()),
        )
        .map_err(|error| match error {
            AppError::SemanticSearchCancelled => error,
            error => {
                AgentError::semantic_unavailable(format!("semantic search failed: {error}")).into()
            }
        })?;
    if response.missing_chunk_count > 0 {
        service.notify(AgentNotice::UncachedPassages {
            count: response.missing_chunk_count,
            bounded_top_up: max_new_embeddings != 0,
        });
    }
    Ok(response.chunk_hits)
}

#[cfg(test)]
pub(crate) fn agent_semantic_candidates(
    inputs: &[agent::search::AgentConversationInput<'_>],
) -> (
    Vec<semantic::index::SemanticIndexCandidate>,
    Vec<AgentWarning>,
) {
    agent_semantic_candidates_with_loader(
        inputs,
        |path| agent::transcript::AgentTranscript::load(path),
        SemanticToolContent::All,
    )
}

#[cfg(test)]
pub(crate) fn agent_semantic_candidates_matching_tools(
    inputs: &[agent::search::AgentConversationInput<'_>],
    query: &str,
) -> (
    Vec<semantic::index::SemanticIndexCandidate>,
    Vec<AgentWarning>,
) {
    agent_semantic_candidates_with_loader(
        inputs,
        |path| agent::transcript::AgentTranscript::load(path),
        SemanticToolContent::MatchingQuery(query),
    )
}

#[cfg(test)]
fn agent_semantic_candidates_with_loader<'a>(
    inputs: &[agent::search::AgentConversationInput<'_>],
    load_transcript: impl Fn(&Path) -> Result<agent::transcript::AgentTranscript>,
    tool_content: SemanticToolContent<'a>,
) -> (
    Vec<semantic::index::SemanticIndexCandidate>,
    Vec<AgentWarning>,
) {
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();
    for input in inputs {
        match load_transcript(&input.resolved.key.path) {
            Ok(transcript) => {
                if let Some(warning) =
                    transcript_warning(&transcript, &input.resolved.reference.canonical())
                {
                    warnings.push(warning);
                }
                push_agent_semantic_candidates(&mut candidates, input, &transcript, tool_content)
            }
            Err(error) => warnings.push(AgentWarning::from_app_error(
                &error,
                Some(&input.resolved.reference.canonical()),
            )),
        }
    }
    (candidates, warnings)
}

#[derive(Clone, Copy)]
enum SemanticToolContent<'a> {
    All,
    MatchingQuery(&'a str),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SemanticAgentPartKind {
    Dialogue,
    Tool,
    Thinking,
}

fn push_agent_semantic_candidates<'a>(
    candidates: &mut Vec<semantic::index::SemanticIndexCandidate>,
    input: &agent::search::AgentConversationInput<'_>,
    transcript: &agent::transcript::AgentTranscript,
    tool_content: SemanticToolContent<'a>,
) {
    for (subagents, kind, source) in [
        (
            false,
            SemanticAgentPartKind::Dialogue,
            semantic::types::SemanticChunkSource::VisibleDialogue,
        ),
        (
            false,
            SemanticAgentPartKind::Tool,
            semantic::types::SemanticChunkSource::AgentTool,
        ),
        (
            false,
            SemanticAgentPartKind::Thinking,
            semantic::types::SemanticChunkSource::AgentThinking,
        ),
        (
            true,
            SemanticAgentPartKind::Dialogue,
            semantic::types::SemanticChunkSource::AgentSubagentDialogue,
        ),
        (
            true,
            SemanticAgentPartKind::Tool,
            semantic::types::SemanticChunkSource::AgentSubagentTool,
        ),
        (
            true,
            SemanticAgentPartKind::Thinking,
            semantic::types::SemanticChunkSource::AgentSubagentThinking,
        ),
    ] {
        push_agent_semantic_candidate(
            candidates,
            input,
            transcript,
            subagents,
            kind,
            source,
            tool_content,
        );
    }
}

fn push_agent_semantic_candidate<'a>(
    candidates: &mut Vec<semantic::index::SemanticIndexCandidate>,
    input: &agent::search::AgentConversationInput<'_>,
    transcript: &agent::transcript::AgentTranscript,
    subagents: bool,
    kind: SemanticAgentPartKind,
    source: semantic::types::SemanticChunkSource,
    tool_content: SemanticToolContent<'a>,
) {
    let Some(conversation) = agent_semantic_conversation(
        input.conversation,
        transcript,
        subagents,
        kind,
        tool_content,
    ) else {
        return;
    };
    candidates.push(semantic::index::SemanticIndexCandidate {
        index: input.original_index,
        source,
        conversation: std::sync::Arc::new(conversation),
    });
}

fn agent_semantic_conversation<'a>(
    conversation: &history::Conversation,
    transcript: &agent::transcript::AgentTranscript,
    subagents: bool,
    kind: SemanticAgentPartKind,
    tool_content: SemanticToolContent<'a>,
) -> Option<history::Conversation> {
    let mut semantic_turns = Vec::new();
    let mut semantic_turn_ranges = Vec::new();
    for message in &transcript.messages {
        if message.parent_tool_use_id.is_some() != subagents {
            continue;
        }
        let role = match message.role {
            agent::transcript::AgentMessageRole::User => semantic::filter::SemanticTurnRole::User,
            agent::transcript::AgentMessageRole::Assistant => {
                semantic::filter::SemanticTurnRole::Assistant
            }
        };
        for part in &message.parts {
            let matches_kind = matches!(
                (kind, part),
                (
                    SemanticAgentPartKind::Dialogue,
                    agent::transcript::AgentMessagePart::Text { .. }
                ) | (
                    SemanticAgentPartKind::Tool,
                    agent::transcript::AgentMessagePart::ToolUse { .. }
                        | agent::transcript::AgentMessagePart::ToolResult { .. }
                ) | (
                    SemanticAgentPartKind::Thinking,
                    agent::transcript::AgentMessagePart::Thinking { .. }
                )
            );
            if !matches_kind {
                continue;
            }
            let texts = match (kind, part) {
                (SemanticAgentPartKind::Tool, part) => {
                    agent::transcript::agent_part_search_text(part)
                        .map(|text| semantic_tool_texts(&text, tool_content))
                        .unwrap_or_default()
                }
                _ => agent::transcript::agent_part_search_text(part)
                    .into_iter()
                    .collect(),
            };
            for text in texts {
                if let Some(turn) = semantic::filter::filter_turn(role, &text) {
                    semantic_turns.push(turn);
                    semantic_turn_ranges.push(agent::refs::MessageRange::single(message.ordinal));
                }
            }
        }
    }
    if semantic_turns.is_empty() {
        return None;
    }
    let mut conversation = conversation.clone();
    let scope = if subagents { "subagent" } else { "main" };
    let content = match kind {
        SemanticAgentPartKind::Dialogue => "dialogue",
        SemanticAgentPartKind::Tool => "tool",
        SemanticAgentPartKind::Thinking => "thinking",
    };
    let file_name = conversation.path.file_name().map(|name| {
        format!(
            "{}.agent-{scope}-{content}-semantic",
            name.to_string_lossy()
        )
    })?;
    conversation.path = conversation.path.with_file_name(file_name);
    conversation.semantic_turns = semantic_turns;
    conversation.semantic_turn_ranges = semantic_turn_ranges;
    Some(conversation)
}

fn semantic_tool_texts(text: &str, tool_content: SemanticToolContent<'_>) -> Vec<String> {
    let segments = stable_text_segments(text, semantic::types::DEFAULT_CHUNK_TARGET_CHARS);
    match tool_content {
        SemanticToolContent::All => segments,
        SemanticToolContent::MatchingQuery(query) => {
            let terms = semantic_tool_query_terms(query);
            let required_matches = if terms.len() >= 3 { 2 } else { 1 };
            segments
                .into_iter()
                .filter_map(|segment| {
                    let matches = semantic_tool_text_match_count(&segment, &terms);
                    (matches >= required_matches).then_some((matches, segment))
                })
                .max_by_key(|(matches, _)| *matches)
                .map(|(_, segment)| vec![segment])
                .unwrap_or_default()
        }
    }
}

fn semantic_tool_text_match_count(text: &str, terms: &[String]) -> usize {
    let normalized = normalize_semantic_tool_text(text);
    terms
        .iter()
        .filter(|term| crate::text_match::contains_prefix_match(&normalized, term))
        .count()
}

fn normalize_semantic_tool_text(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut previous_was_lowercase = false;
    for ch in text.chars() {
        if previous_was_lowercase && ch.is_uppercase() {
            expanded.push(' ');
        }
        expanded.push(ch);
        previous_was_lowercase = ch.is_lowercase();
    }
    crate::text_match::normalize_for_search(&expanded)
}

fn semantic_tool_query_terms(query: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "after", "all", "an", "and", "are", "as", "at", "be", "before", "but", "by", "can", "do",
        "does", "for", "from", "has", "have", "how", "if", "in", "is", "it", "its", "my", "no",
        "not", "of", "on", "or", "our", "so", "than", "that", "the", "them", "then", "they",
        "this", "to", "up", "was", "we", "what", "when", "where", "which", "why", "with", "would",
        "you", "your",
    ];

    normalize_semantic_tool_text(query)
        .split_whitespace()
        .map(|term| term.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| !STOP_WORDS.contains(term))
        .map(str::to_string)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

fn stable_text_segments(mut text: &str, max_bytes: usize) -> Vec<String> {
    let mut segments = Vec::new();
    while !text.is_empty() {
        let mut end = text.len().min(max_bytes);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            end = text
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(index, _)| index);
        }
        segments.push(text[..end].to_string());
        text = &text[end..];
    }
    segments
}

fn run_agent_within_semantic<'a>(
    service: &AgentService,
    request: &agent::search::AgentWithinRequest,
    conversation: &history::Conversation,
    resolved: &agent::refs::ResolvedConversation,
    transcript: &agent::transcript::AgentTranscript,
    tool_content: SemanticToolContent<'a>,
) -> Result<agent::search::AgentSearchOutput> {
    let input = agent::search::AgentConversationInput {
        conversation,
        resolved: resolved.clone(),
        original_index: 0,
    };
    let mut candidates = Vec::new();
    push_agent_semantic_candidates(&mut candidates, &input, transcript, tool_content);
    let semantic = run_agent_semantic_hits_for_candidates(
        service,
        &request.query,
        &candidates,
        semantic::types::MAX_WITHIN_INTERACTIVE_PASSAGE_EMBEDDINGS,
    )?;
    Ok(agent::search::run_within_search(
        request,
        conversation,
        resolved,
        transcript,
        &semantic,
    ))
}

fn agent_inputs_for_indices<'a>(
    conversations: &'a [history::Conversation],
    keys: &[agent::refs::AgentConversationKey],
    indices: &[usize],
) -> Result<Vec<agent::search::AgentConversationInput<'a>>> {
    let resolved_by_path = agent::refs::resolved_conversations_for_keys(keys)
        .into_iter()
        .map(|resolved| (resolved.key.path.clone(), resolved))
        .collect::<std::collections::HashMap<_, _>>();
    indices
        .iter()
        .filter_map(|index| {
            let conversation = conversations.get(*index)?;
            let resolved = resolved_by_path.get(&conversation.path)?;
            Some(Ok(agent::search::AgentConversationInput {
                conversation,
                resolved: resolved.clone(),
                original_index: *index,
            }))
        })
        .collect()
}

impl AgentService {
    #[cfg(test)]
    pub(crate) fn run_read(
        &self,
        args: &AgentReadArgs,
        keys: Option<&[agent::refs::AgentConversationKey]>,
    ) -> Result<String> {
        self.serve_read(args, keys).map(|served| served.text)
    }

    fn serve_read(
        &self,
        args: &AgentReadArgs,
        keys: Option<&[agent::refs::AgentConversationKey]>,
    ) -> Result<Served> {
        let discovered;
        let keys = match keys {
            Some(keys) => keys,
            None => {
                discovered = discover_agent_keys(&self.env, None)?.0;
                &discovered
            }
        };
        let agent_config = self.agent_config();
        let (mut resolved_refs, focus) = resolve_agent_read_args(&self.env, args, Some(keys))?;
        let options = agent_protocol_options(
            args.output.no_budget,
            args.output.budget,
            args.output.tools,
            args.output.tool_results,
            args.output.thinking,
            args.output.subagents,
            &agent_config,
        );
        let transcripts = resolved_refs
            .iter()
            .map(|(_, resolved)| {
                self.load_transcript(&resolved.key.path)
                    .map_err(|error| target_error(error, resolved))
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(anchor) = args.anchor.as_deref() {
            let ordinal = transcripts[0].resolve_anchor(&resolved_refs[0].1, anchor)?;
            resolved_refs[0].0.range = Some(agent::refs::MessageRange::single(ordinal));
        }
        let requests = resolved_refs
            .iter()
            .zip(transcripts.iter())
            .map(
                |((read_ref, resolved), transcript)| agent::protocol::ReadRequest {
                    resolved,
                    transcript,
                    range: read_ref.range,
                },
            )
            .collect::<Vec<_>>();
        let protocol_focus = focus
            .map(|focus| -> Result<agent::protocol::ProtocolFocus> {
                let conversation_full_ref = focus
                    .conversation
                    .as_ref()
                    .map(|conversation| {
                        agent::refs::resolve_conversation_ref(keys, conversation)
                            .map(|resolved| resolved.reference.full_ref())
                    })
                    .transpose()?;
                Ok(agent::protocol::ProtocolFocus {
                    conversation_full_ref,
                    range: focus.range,
                })
            })
            .transpose()?;
        let slice = if let Some(range) = args.lines {
            Some(agent::protocol::ReadSlice::Lines(range))
        } else {
            args.match_query
                .as_ref()
                .map(|query| agent::protocol::ReadSlice::Match {
                    query: query.clone(),
                    context: args.context,
                })
        };
        let warnings = resolved_refs
            .iter()
            .zip(transcripts.iter())
            .filter_map(|((_, resolved), transcript)| {
                transcript_warning(transcript, &resolved.reference.canonical())
            })
            .collect::<Vec<_>>();
        let read_error = |error| match resolved_refs.first() {
            Some((_, resolved)) => target_error(error, resolved),
            None => structured_agent_error(error),
        };
        let text = agent::protocol::format_read_with_warnings(
            &requests,
            protocol_focus.clone(),
            slice.as_ref(),
            options,
            &warnings,
        )
        .map_err(read_error)?;
        let messages = agent::protocol::read_messages(
            &requests,
            protocol_focus.as_ref(),
            slice.as_ref(),
            options,
        )
        .map_err(read_error)?;
        let mut conversations: Vec<AgentConversationInfo> = Vec::new();
        for (_, resolved) in &resolved_refs {
            let info = AgentConversationInfo::from(resolved);
            if !conversations.contains(&info) {
                conversations.push(info);
            }
        }
        Ok(Served {
            text,
            result: AgentResult::Read(AgentReadResult {
                conversations,
                messages,
                focus: protocol_focus,
                slice,
                options,
            }),
            warnings,
        })
    }

    #[cfg(test)]
    pub(crate) fn run_outline(
        &self,
        args: &AgentOutlineArgs,
        keys: Option<&[agent::refs::AgentConversationKey]>,
    ) -> Result<String> {
        self.serve_outline(args, keys).map(|served| served.text)
    }

    fn serve_outline(
        &self,
        args: &AgentOutlineArgs,
        keys: Option<&[agent::refs::AgentConversationKey]>,
    ) -> Result<Served> {
        let discovered;
        let keys = match keys {
            Some(keys) => keys,
            None => {
                discovered = discover_agent_keys(&self.env, None)?.0;
                &discovered
            }
        };
        let agent_config = self.agent_config();
        let resolved = resolve_agent_conversation_arg(&self.env, &args.conversation, Some(keys))?;
        let transcript = self
            .load_transcript(&resolved.key.path)
            .map_err(|error| target_error(error, &resolved))?;
        let warnings: Vec<AgentWarning> =
            transcript_warning(&transcript, &resolved.reference.canonical())
                .into_iter()
                .collect();
        let options = agent_protocol_options(
            args.output.no_budget,
            args.output.budget,
            args.output.tools,
            args.output.tool_results,
            args.output.thinking,
            args.output.subagents,
            &agent_config,
        );
        let text = agent::protocol::format_outline_with_warnings(
            &resolved,
            &transcript,
            options,
            &warnings,
        );
        let (items, visible_messages) =
            agent::protocol::outline_items(&resolved, &transcript, options);
        Ok(Served {
            text,
            result: AgentResult::Outline(AgentOutlineResult {
                conversation: AgentConversationInfo::from(&resolved),
                items,
                total_messages: transcript.messages.len(),
                visible_messages,
                options,
            }),
            warnings,
        })
    }
}

pub(crate) fn resolve_agent_read_args(
    env: &HistoryEnv,
    args: &AgentReadArgs,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<(ResolvedReadRefs, Option<agent::refs::FocusRef>)> {
    let refs = args
        .refs
        .iter()
        .map(|reference| agent::refs::parse_read_ref(reference))
        .collect::<Result<Vec<_>>>()?;
    if args.anchor.is_some() && (refs.len() != 1 || refs[0].range.is_some()) {
        return Err(AppError::ConfigError(
            "--anchor requires exactly one conversation ref without an mN range".to_string(),
        ));
    }
    if (args.lines.is_some() || args.match_query.is_some())
        && (refs.len() != 1
            || (!refs[0].range.is_some_and(|range| range.start == range.end)
                && args.anchor.is_none()))
    {
        return Err(AppError::ConfigError(
            "--lines and --match require exactly one single-message ref such as ch_...:m7"
                .to_string(),
        ));
    }
    let loaded_keys;
    let keys = if let Some(keys) = keys {
        keys
    } else {
        let conversations = history::load_all_conversations(env, false, None)?;
        loaded_keys = agent::refs::conversation_keys_from_conversations(&conversations)?;
        &loaded_keys
    };
    let resolved_refs = refs
        .iter()
        .map(|reference| {
            agent::refs::resolve_conversation_ref(keys, &reference.conversation)
                .map(|resolved| (reference.clone(), resolved))
        })
        .collect::<Result<Vec<_>>>()?;
    let focus = args
        .focus
        .as_deref()
        .map(agent::refs::parse_focus_ref)
        .transpose()?;
    if let Some(focus) = &focus {
        let focus_conversation = focus
            .conversation
            .as_ref()
            .map(|conversation| agent::refs::resolve_conversation_ref(keys, conversation))
            .transpose()?;
        agent::refs::validate_resolved_focus_in_ranges(
            &resolved_refs,
            focus,
            focus_conversation.as_ref(),
        )?;
    }
    Ok((resolved_refs, focus))
}

fn agent_protocol_options(
    no_budget: bool,
    budget: Option<usize>,
    tools: bool,
    tool_results: bool,
    thinking: bool,
    subagents: bool,
    config: &AgentConfig,
) -> agent::protocol::ProtocolOptions {
    agent::protocol::ProtocolOptions {
        budget: configured_budget(no_budget, budget, config.output_chars),
        tools: configured_visibility(tools, config.tools),
        tool_results: configured_visibility(tool_results, config.tool_results),
        thinking: configured_visibility(thinking, config.thinking),
        subagents: configured_visibility(subagents, config.subagents),
    }
}

fn lexically_rank_scoped(
    conversations: &[history::Conversation],
    query: &str,
    scoped: &[usize],
) -> Vec<usize> {
    let searchable = search::precompute_agent_search_text(conversations);
    let ranked_all = search::agent_search(conversations, &searchable, query, chrono::Local::now());
    let scoped_set = scoped
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    ranked_all
        .into_iter()
        .filter(|index| scoped_set.contains(index))
        .collect()
}

pub(crate) fn resolve_agent_conversation_arg(
    env: &HistoryEnv,
    reference: &str,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<agent::refs::ResolvedConversation> {
    let loaded_keys;
    let keys = if let Some(keys) = keys {
        keys
    } else {
        let conversations = history::load_all_conversations(env, false, None)?;
        loaded_keys = agent::refs::conversation_keys_from_conversations(&conversations)?;
        &loaded_keys
    };
    agent::refs::resolve_conversation_ref(keys, reference)
}

#[cfg(test)]
pub(crate) fn run_agent_read(
    args: &AgentReadArgs,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<String> {
    AgentService::default().run_read(args, keys)
}

#[cfg(test)]
pub(crate) fn run_agent_outline(
    args: &AgentOutlineArgs,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<String> {
    AgentService::default().run_outline(args, keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::args::AgentOutputFlags;
    use crate::agent::test_support::user_jsonl_line;
    use crate::config;

    fn output_flags() -> AgentOutputFlags {
        AgentOutputFlags {
            budget: Some(6000),
            no_budget: false,
            tools: false,
            tool_results: false,
            thinking: false,
            subagents: false,
        }
    }

    fn read_args(reference: String) -> AgentReadArgs {
        AgentReadArgs {
            refs: vec![reference],
            anchor: None,
            focus: None,
            lines: None,
            match_query: None,
            context: 3,
            output: output_flags(),
        }
    }

    #[test]
    fn agent_route_uses_compact_precomputed_text() {
        let mut conversation = crate::semantic::test_fixtures::SemanticConversationFixture::new(
            "/tmp/session.jsonl",
            ["dialogue that should not be copied into the route"],
        )
        .with_title("Align step calibration and treadmill UX")
        .with_summary(Some(
            "Calibrate automatic counting across treadmill sessions",
        ))
        .build();
        conversation.full_text =
            "calibration isolation and per-device capability memory from bounded tool evidence"
                .to_string();
        conversation.semantic_route_text =
            history::semantic_route_text(&conversation.full_text, "");

        let route = agent_route_semantic_conversation(&conversation).unwrap();

        assert_eq!(route.semantic_turns.len(), 1);
        assert!(route.semantic_turns[0].chars().count() <= 1_805);
        assert!(route.semantic_turns[0].contains("per-device capability memory"));
        assert_eq!(
            route.semantic_turn_ranges,
            conversation.semantic_turn_ranges[..1]
        );
        assert!(
            route
                .path
                .to_string_lossy()
                .ends_with("session.jsonl.agent-route-semantic")
        );
        assert!(!route.semantic_turns[0].contains("should not be copied"));
    }

    #[test]
    fn anchor_read_resolves_message_after_unrelated_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(
            &path,
            [user_jsonl_line("unrelated"), user_jsonl_line("target")].join("\n"),
        )
        .unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path);
        let resolved = agent::refs::resolved_conversation_for_key(std::slice::from_ref(&key), &key);
        let transcript = agent::transcript::AgentTranscript::load(&resolved.key.path).unwrap();
        let anchor = transcript.message_anchor(&resolved, &transcript.messages[1]);
        let mut args = read_args(resolved.reference.canonical());
        args.anchor = Some(anchor.clone());

        let output = AgentService::default()
            .run_read(&args, Some(std::slice::from_ref(&key)))
            .unwrap();

        assert!(output.contains("message m2 role=user"));
        assert!(output.contains(&format!("anchor={anchor}")));
        assert!(output.contains("| target\n"));
        assert!(!output.contains("| unrelated\n"));
    }

    #[test]
    fn agent_mode_ignores_tui_semantic_search() {
        let config: config::ConfigFile = toml::from_str(
            r#"
[search]
mode = "lexical"
[tui]
semantic_search = true
"#,
        )
        .unwrap();
        let search_config = config.search.unwrap_or_default();

        assert_eq!(
            agent::search::effective_agent_mode("needle", None, search_config.mode, None),
            SearchMode::Lexical
        );
    }

    #[test]
    fn agent_config_overrides_general_search_mode() {
        let config: config::ConfigFile = toml::from_str(
            r#"
[search]
mode = "lexical"
[agent]
mode = "hybrid"
"#,
        )
        .unwrap();

        assert_eq!(
            config.agent.unwrap().mode.or(config.search.unwrap().mode),
            Some(SearchMode::Hybrid)
        );
    }

    #[test]
    fn explicit_agent_values_override_agent_defaults() {
        assert_eq!(configured_usize(Some(7), 10, Some(12)), 7);
        assert_eq!(configured_usize(None, 10, Some(12)), 12);
        assert_eq!(configured_budget(false, Some(6000), Some(9000)), Some(6000));
        assert_eq!(configured_budget(false, None, Some(9000)), Some(9000));
        assert_eq!(configured_budget(true, Some(6000), Some(9000)), None);
    }

    #[test]
    fn invocation_cache_reuses_loaded_target_transcript() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(&path, user_jsonl_line("cached message")).unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path.clone());
        let reference = key.conversation_ref().canonical();
        let args = AgentReadArgs {
            refs: vec![format!("{reference}:m1")],
            anchor: None,
            focus: None,
            lines: None,
            match_query: None,
            context: 3,
            output: output_flags(),
        };
        let service = AgentService::default();

        assert!(
            service
                .run_read(&args, Some(std::slice::from_ref(&key)))
                .is_ok()
        );
        std::fs::write(&path, "{malformed").unwrap();
        let output = service
            .run_read(&args, Some(std::slice::from_ref(&key)))
            .unwrap();

        assert!(output.contains("cached message"));
        assert_eq!(service.transcript_parse_count.get(), 1);
    }

    #[test]
    fn partial_target_reports_structured_warning_and_stable_ordinals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(
            &path,
            [
                user_jsonl_line("first"),
                "{malformed".to_string(),
                user_jsonl_line("third"),
            ]
            .join("\n"),
        )
        .unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path);
        let reference = key.conversation_ref().canonical();
        let args = AgentOutlineArgs {
            conversation: reference.clone(),
            output: output_flags(),
        };

        let output = AgentService::default()
            .run_outline(&args, Some(std::slice::from_ref(&key)))
            .unwrap();

        assert!(output.contains("warnings=1"));
        assert!(output.contains("kind=malformed-transcript"));
        assert!(output.contains("lines%202"));
        assert!(output.contains("m1 role=user"));
        assert!(output.contains("m2 role=user"));
    }

    #[test]
    fn malformed_target_is_a_typed_service_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(&path, "{malformed").unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path);
        let reference = key.conversation_ref().canonical();
        let args = AgentOutlineArgs {
            conversation: reference.clone(),
            output: output_flags(),
        };

        let error = AgentService::default()
            .run_outline(&args, Some(std::slice::from_ref(&key)))
            .unwrap_err();
        let AppError::Agent(error) = error else {
            panic!("expected typed agent error");
        };
        assert_eq!(error.kind, AgentErrorKind::MalformedTranscript);
        assert_eq!(error.reference.as_deref(), Some(reference.as_str()));
    }

    fn pi_session_with_cwd(path: &Path, cwd: &Path) {
        let header = serde_json::json!({
            "type": "session",
            "id": "local_scope_id",
            "timestamp": "2024-01-01T00:00:00.000Z",
            "cwd": cwd,
        });
        let user = r#"{"type":"message","timestamp":"2024-01-01T00:01:00.000Z","message":{"role":"user","content":"local question","timestamp":1704067260000}}"#;
        std::fs::write(path, format!("{header}\n{user}\n")).unwrap();
    }

    #[test]
    fn local_key_discovery_matches_pi_sessions_by_workspace_directory() {
        // W1: upstream re-encoded the canonicalized cwd, which on Windows
        // (`\\?\C:\...`) never matched and dropped every local Pi session.
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("pi-sessions");
        let workspace = home.path().join("workspace");
        let elsewhere = home.path().join("elsewhere");
        let claude_projects = home.path().join(".claude").join("projects");
        for dir in [&sessions, &workspace, &elsewhere, &claude_projects] {
            std::fs::create_dir_all(dir).unwrap();
        }
        pi_session_with_cwd(&sessions.join("local.jsonl"), &workspace);
        let env = HistoryEnv::sandbox(home.path())
            .with_pi_session_dir(&sessions)
            .with_workspace(&workspace);

        // The flat session directory feeds both the Pi and the OMP discovery
        // (upstream behavior), so the same file may yield one key per source.
        let (local, _) = discover_agent_keys(&env, env.workspace().unwrap().as_ref()).unwrap();
        assert!(
            local
                .iter()
                .any(|key| key.source == history::Source::Pi && key.session_id == "local_scope_id"),
            "{local:?}"
        );

        let other = WorkspaceScope::new(&elsewhere).unwrap();
        let (keys, _) = discover_agent_keys(&env, Some(&other)).unwrap();
        assert!(keys.is_empty(), "{keys:?}");

        let (global, _) = discover_agent_keys(&env, None).unwrap();
        assert_eq!(global, local);
    }

    #[test]
    fn local_search_without_a_workspace_is_a_command_error() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join(".claude").join("projects").join("-tmp-p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("s.jsonl"), user_jsonl_line("needle")).unwrap();
        let env = HistoryEnv::sandbox(home.path());
        let request = AgentRequest::Search(AgentSearchArgs {
            local: true,
            ..AgentSearchArgs::new("needle")
        });
        let error = execute_text(&env, &ConfigFile::default(), request.clone()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("local%20agent%20search%20requires%20a%20current%20project"),
            "{error}"
        );

        let response = execute(&env, &ConfigFile::default(), request);
        assert!(!response.is_success());
        assert_eq!(response.exit_code, 1);
        assert_eq!(response.stderr, error.to_string());
        assert_eq!(response.text(), response.stderr);
        assert!(response.stdout.is_empty() && response.result.is_none());
        let agent_error = response.error.unwrap();
        assert_eq!(
            agent::diagnostic::format_error(&agent_error),
            response.stderr
        );
    }

    /// Deterministic offline embedder: every text maps to the same vector.
    struct ConstantEmbedder;

    impl SemanticEmbedder for ConstantEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(passages.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    struct FailingEmbedder;

    impl SemanticEmbedder for FailingEmbedder {
        fn embed_passages(&mut self, _passages: &[String]) -> Result<Vec<Vec<f32>>> {
            Err(AppError::ConfigError("fake embedder offline".to_string()))
        }

        fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
            Err(AppError::ConfigError("fake embedder offline".to_string()))
        }
    }

    const SEMANTIC_UUID: &str = "12345678-1234-4234-9234-123456789abc";

    fn semantic_sandbox() -> (tempfile::TempDir, HistoryEnv) {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join(".claude").join("projects").join("-tmp-p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{SEMANTIC_UUID}.jsonl")),
            [
                user_jsonl_line("semantic needle question"),
                crate::agent::test_support::assistant_jsonl_line("semantic needle answer"),
            ]
            .join("\n"),
        )
        .unwrap();
        let env = HistoryEnv::sandbox(home.path());
        (home, env)
    }

    fn semantic_within() -> AgentRequest {
        let mut within = AgentWithinArgs::new(SEMANTIC_UUID, "semantic needle");
        within.search_mode.semantic = true;
        AgentRequest::Within(within)
    }

    #[test]
    fn semantic_requests_use_the_injected_embedder_and_report_notices() {
        let (_home, env) = semantic_sandbox();
        let created = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = std::rc::Rc::clone(&created);
        let streamed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&streamed);
        let mut service = AgentService::new(env.clone(), ConfigFile::default())
            .with_embedder_factory(Box::new(move || {
                counter.set(counter.get() + 1);
                Ok(Box::new(ConstantEmbedder))
            }))
            .with_notice_sink(Box::new(move |notice| {
                sink.borrow_mut().push(notice.clone())
            }));

        let output = service.execute(semantic_within()).unwrap();

        assert!(
            output.starts_with("protocol agent-within mode=semantic"),
            "{output}"
        );
        assert_eq!(created.get(), 1);
        let notices = service.take_notices();
        assert!(
            notices
                .iter()
                .any(|notice| matches!(notice, AgentNotice::SemanticProgress(_))),
            "{notices:?}"
        );
        assert_eq!(*streamed.borrow(), notices);
        assert!(service.take_notices().is_empty());
        // The bounded top-up cached the passages inside the sandbox; no model
        // was downloaded.
        assert!(env.embedding_cache_file().unwrap().is_file());
        assert!(!env.semantic_model_dir().unwrap().exists());
    }

    #[test]
    fn default_embedder_refuses_to_load_the_real_model_in_tests() {
        // Without an injected factory the service would load (and download)
        // fastembed; under cfg(test) it must refuse instead.
        let (_home, env) = semantic_sandbox();
        let mut service = AgentService::new(env.clone(), ConfigFile::default());

        let AppError::AgentProtocol(text) = service.execute(semantic_within()).unwrap_err() else {
            panic!("expected protocol envelope");
        };
        assert!(
            text.starts_with("protocol agent-error kind=semantic-unavailable"),
            "{text}"
        );
        assert!(text.contains("tests%20must%20inject"), "{text}");
        assert!(!env.semantic_model_dir().unwrap().exists());
    }

    #[test]
    fn semantic_embedder_failures_are_semantic_unavailable_errors() {
        let (_home, env) = semantic_sandbox();
        let mut service = AgentService::new(env.clone(), ConfigFile::default())
            .with_embedder_factory(Box::new(|| Ok(Box::new(FailingEmbedder))));

        let AppError::AgentProtocol(text) = service.execute(semantic_within()).unwrap_err() else {
            panic!("expected protocol envelope");
        };
        assert!(
            text.starts_with("protocol agent-error kind=semantic-unavailable"),
            "{text}"
        );
        assert!(text.contains("semantic%20search%20failed"), "{text}");

        let mut service = AgentService::new(env.clone(), ConfigFile::default())
            .with_embedder_factory(Box::new(|| {
                Err(AppError::ConfigError("no model".to_string()))
            }));
        let AppError::AgentProtocol(text) = service.execute(semantic_within()).unwrap_err() else {
            panic!("expected protocol envelope");
        };
        assert!(
            text.contains("failed%20to%20initialize%20semantic%20search"),
            "{text}"
        );
        assert!(!env.semantic_model_dir().unwrap().exists());
    }

    /// Cancels `token` while embedding the first passage batch, as a user
    /// pressing Cancel during a long search would; counts what it did.
    struct CancellingEmbedder {
        token: semantic::types::SemanticCancellationToken,
        passage_batches: std::rc::Rc<std::cell::Cell<usize>>,
        queries: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl SemanticEmbedder for CancellingEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            self.passage_batches.set(self.passage_batches.get() + 1);
            self.token.cancel();
            Ok(passages.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
            self.queries.set(self.queries.get() + 1);
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    #[test]
    fn a_search_cancelled_while_embedding_ends_before_ranking() {
        // K7: the cancel is seen at the next batch boundary; nothing falls
        // back to lexical results.
        for hybrid in [false, true] {
            let (_home, env) = semantic_sandbox();
            let token = semantic::types::SemanticCancellationToken::new();
            let passage_batches = std::rc::Rc::new(std::cell::Cell::new(0));
            let queries = std::rc::Rc::new(std::cell::Cell::new(0));
            let (embedder_token, batches, query_count) = (
                token.clone(),
                std::rc::Rc::clone(&passage_batches),
                std::rc::Rc::clone(&queries),
            );
            let mut service = AgentService::new(env, ConfigFile::default())
                .with_cancellation(token)
                .with_embedder_factory(Box::new(move || {
                    Ok(Box::new(CancellingEmbedder {
                        token: embedder_token.clone(),
                        passage_batches: std::rc::Rc::clone(&batches),
                        queries: std::rc::Rc::clone(&query_count),
                    }))
                }));
            let mut within = AgentWithinArgs::new(SEMANTIC_UUID, "semantic needle");
            if hybrid {
                within.search_mode.hybrid = true;
            } else {
                within.search_mode.semantic = true;
            }

            let response = service.respond(AgentRequest::Within(within));

            assert_eq!(passage_batches.get(), 1, "hybrid={hybrid}");
            assert_eq!(queries.get(), 0, "ranking never started (hybrid={hybrid})");
            assert_eq!(response.exit_code, 1);
            assert!(response.error.is_none());
            assert!(response.result.is_none());
            assert!(response.stdout.is_empty());
            assert_eq!(response.error_text(), AgentResponse::CANCELLED_TEXT);
            assert!(
                response
                    .stderr
                    .ends_with("Error: Semantic search cancelled\n")
            );
            assert!(response.is_cancelled());
        }
    }

    #[test]
    fn d017_agent_exclude_projects_narrows_search_but_not_read_or_outline() {
        // `[agent] exclude_projects` holds encoded project directory names
        // and is worktree-aware; read/outline/within still reach them.
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join(".claude").join("projects");
        let sessions = [
            ("-tmp-keep", "11111111-1111-4111-8111-111111111111"),
            ("-tmp-hidden", "22222222-2222-4222-8222-222222222222"),
            (
                "-tmp-hidden--worktrees-feature",
                "33333333-3333-4333-8333-333333333333",
            ),
        ];
        for (project, uuid) in sessions {
            let dir = projects.join(project);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("{uuid}.jsonl")),
                [
                    user_jsonl_line("shared needle question"),
                    crate::agent::test_support::assistant_jsonl_line("shared needle answer"),
                ]
                .join("\n"),
            )
            .unwrap();
        }
        let env = HistoryEnv::sandbox(home.path());
        let config = ConfigFile {
            agent: Some(AgentConfig {
                exclude_projects: vec!["-tmp-hidden".to_string()],
                ..AgentConfig::default()
            }),
            ..ConfigFile::default()
        };

        let response = execute(
            &env,
            &config,
            AgentRequest::Search(AgentSearchArgs::new("shared needle")),
        );
        assert!(response.is_success(), "{}", response.stderr);
        let Some(AgentResult::Search(result)) = &response.result else {
            panic!("search result expected");
        };
        let paths = result
            .conversations
            .iter()
            .map(|conversation| conversation.uuid.clone())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![sessions[0].1.to_string()]);

        for (_, uuid) in &sessions[1..] {
            let read = execute(
                &env,
                &config,
                AgentRequest::Read(AgentReadArgs::new([uuid.to_string()])),
            );
            assert!(read.is_success(), "{}", read.stderr);
            assert!(read.stdout.contains("shared needle"), "{}", read.stdout);
            let outline = execute(
                &env,
                &config,
                AgentRequest::Outline(AgentOutlineArgs::new(uuid.to_string())),
            );
            assert!(outline.is_success(), "{}", outline.stderr);
        }
    }

    #[test]
    fn a_cancelled_token_stops_a_search_before_loading_transcripts() {
        let (_home, env) = semantic_sandbox();
        let token = semantic::types::SemanticCancellationToken::new();
        token.cancel();
        let mut service =
            AgentService::new(env.clone(), ConfigFile::default()).with_cancellation(token);
        let response = service.respond(AgentRequest::Search(AgentSearchArgs::new(
            "semantic needle",
        )));
        assert_eq!(service.transcript_parse_count.get(), 0);
        assert_eq!(response.stderr, "Error: Semantic search cancelled\n");
        assert_eq!(response.exit_code, 1);
        assert!(response.error.is_none());
        assert!(response.is_cancelled());
        assert!(matches!(
            service.execute(AgentRequest::Within(AgentWithinArgs::new(
                SEMANTIC_UUID,
                "needle"
            ))),
            Err(AppError::SemanticSearchCancelled)
        ));

        // An untouched token changes nothing.
        let mut service = AgentService::new(env, ConfigFile::default())
            .with_cancellation(semantic::types::SemanticCancellationToken::new());
        let response = service.respond(AgentRequest::Search(AgentSearchArgs::new(
            "semantic needle",
        )));
        assert!(response.is_success(), "{}", response.stderr);
        assert!(!response.is_cancelled());
    }

    #[test]
    fn notices_render_upstream_stderr_lines() {
        assert_eq!(
            AgentNotice::UncachedPassages {
                count: 3,
                bounded_top_up: false
            }
            .to_string(),
            "Semantic search: 3 passage(s) are not cached; run claude-history --generate-semantic-cache"
        );
        assert_eq!(
            AgentNotice::UncachedPassages {
                count: 2,
                bounded_top_up: true
            }
            .to_string(),
            "Semantic search: 2 passage(s) remain uncached after the bounded top-up; rerun this search to cache more"
        );
        assert_eq!(
            AgentNotice::SemanticProgress(SemanticIndexProgress::CacheReady).to_string(),
            "Semantic search: CacheReady"
        );
    }
}
