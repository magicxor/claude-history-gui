//! Test helpers (upstream `tui/app/semantic_test_helpers.rs`, plus sandbox
//! construction). Every browser built here lives in a temporary sandbox and
//! uses the fake embedder, so no test touches the real home directory or
//! the network.

use super::*;
use crate::semantic::types::{
    SemanticChunkIdentity, SemanticExplanation, SemanticQuality, SemanticRationaleKind,
    SemanticScoreBreakdown,
};
use crate::semantic_worker::test_support::fake_embedder_factory;
use crate::semantic_worker::{
    SemanticSearchMessage, SemanticSearchResponse, SemanticWorkerCommand,
};
use std::collections::HashMap;
use std::sync::mpsc;

/// A temporary sandbox home plus its environment.
pub(crate) struct Sandbox {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) env: Arc<HistoryEnv>,
}

impl Sandbox {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let env = Arc::new(HistoryEnv::sandbox(dir.path()));
        Self { dir, env }
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Options rooted in this sandbox, with the fake embedder.
    pub(crate) fn options(&self) -> BrowserOptions {
        BrowserOptions::new(self.env.clone()).with_embedder_factory(fake_embedder_factory().0)
    }

    /// `<sandbox>/.claude/projects`
    pub(crate) fn projects_root(&self) -> PathBuf {
        self.env.claude_projects_root().unwrap()
    }
}

pub(crate) fn connect_semantic_search_channels(
    browser: &mut Browser,
) -> (
    mpsc::Sender<SemanticWorkerCommand>,
    mpsc::Receiver<SemanticWorkerCommand>,
    mpsc::Sender<SemanticSearchMessage>,
) {
    let (request_tx, request_rx) = mpsc::channel::<SemanticWorkerCommand>();
    let (response_tx, response_rx) = mpsc::channel::<SemanticSearchMessage>();
    browser.semantic_search.worker_tx = Some(request_tx);
    browser.semantic_search.worker_rx = Some(response_rx);
    (
        browser.semantic_search.worker_tx.clone().unwrap(),
        request_rx,
        response_tx,
    )
}

pub(crate) fn send_semantic_complete_response(
    response_tx: &mpsc::Sender<SemanticSearchMessage>,
    generation: u64,
    filtered: Vec<usize>,
    metadata: HashMap<usize, SemanticResultMetadata>,
    progress: SemanticProgress,
) {
    response_tx
        .send(SemanticSearchMessage::Complete(SemanticSearchResponse {
            generation,
            filtered,
            metadata,
            error: None,
            progress,
            prewarm: false,
        }))
        .unwrap();
}

pub(crate) fn send_semantic_progress_response(
    response_tx: &mpsc::Sender<SemanticSearchMessage>,
    generation: u64,
    progress: SemanticProgress,
) {
    response_tx
        .send(SemanticSearchMessage::Progress {
            generation,
            progress,
        })
        .unwrap();
}

pub(crate) fn test_semantic_metadata(
    conversation_index: usize,
    evidence_preview: &str,
) -> SemanticResultMetadata {
    SemanticResultMetadata {
        score_breakdown: SemanticScoreBreakdown {
            hybrid: 1.0,
            semantic: 1.0,
            lexical: 0.0,
        },
        explanation: SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "strong",
            matched_terms: Vec::new(),
            evidence_preview: evidence_preview.to_string(),
            rationale_kind: SemanticRationaleKind::SemanticOnly,
            chunk: SemanticChunkIdentity {
                conversation_index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: "test-session".to_string(),
                chunk_index: 0,
                message_range: crate::agent::refs::MessageRange::single(1),
            },
        },
    }
}

/// A minimal Claude conversation at `path` (session id = file stem).
pub(crate) fn plain_conversation(path: PathBuf) -> Conversation {
    Conversation {
        source: crate::history::Source::Claude,
        session_id: path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        path,
        index: 0,
        timestamp: chrono::Local::now(),
        preview: String::new(),
        preview_first: String::new(),
        preview_last: String::new(),
        full_text: String::new(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns: Vec::new(),
        semantic_turn_ranges: Vec::new(),
        search_text_lower: String::new(),
        project_name: None,
        project_path: None,
        cwd: None,
        message_count: 0,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

/// Insert `c` at the end of the query, like typing a key in the TUI.
pub(crate) fn type_char(browser: &mut Browser, c: char) {
    let mut query = browser.query().to_string();
    query.push(c);
    browser.set_query(query);
}

/// Set the query without dispatching (upstream `set_query_for_test`).
pub(crate) fn set_query_for_test(browser: &mut Browser, query: &str) {
    browser.query = query.to_string();
}

/// Apply a pending lexical answer, waiting for the worker thread.
pub(crate) fn wait_for_search(browser: &mut Browser) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while browser.has_search_work_in_flight() {
        assert!(
            std::time::Instant::now() < deadline,
            "search did not finish in time"
        );
        if !browser.poll() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
