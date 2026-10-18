//! Agent fixtures (DESIGN §6, §15.2): the corpora the Deep search and agent
//! panel tests search, outline and read — a conversation with multi-line
//! messages, tool calls and a retry theme; the 25- and 12-message outline
//! fixtures; a malformed transcript; two conversations that share a UUID (an
//! ambiguous ref); a workspace corpus — and the fake embedders (slow, gated,
//! failing). Owned by WP4.
//!
//! Builders use `super::TestWorld` and `super::synth`; GUI-only fixture
//! files go into `crates/gui/tests/fixtures/agent/`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use claude_history_core::semantic::embed::SemanticEmbedder;
use gpui_kit::TestAppContext;

use crate::embedder::ModelFactory;

use super::fixtures_list::Gate;
use super::{TestWorld, synth};

fn json(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

/// The cwd of a synthetic project.
pub fn project_cwd(project: &str) -> PathBuf {
    synth::cwd_base().join(project)
}

/// Builds the JSONL of one Claude session.
pub struct SessionBuilder {
    uuid: String,
    cwd: PathBuf,
    summary: Option<String>,
    lines: Vec<String>,
    tick: usize,
}

impl SessionBuilder {
    pub fn new(number: u32, cwd: PathBuf) -> Self {
        Self {
            uuid: synth::uuid(number),
            cwd,
            summary: None,
            lines: Vec::new(),
            tick: 0,
        }
    }

    pub fn summary(mut self, summary: &str) -> Self {
        self.summary = Some(summary.to_string());
        self
    }

    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    fn stamp(&mut self) -> String {
        let tick = self.tick;
        self.tick += 1;
        format!(
            "2026-09-01T{:02}:{:02}:{:02}Z",
            10 + tick / 3600,
            (tick / 60) % 60,
            tick % 60
        )
    }

    /// A user message.
    pub fn user(mut self, text: &str) -> Self {
        let stamp = self.stamp();
        self.lines.push(format!(
            r#"{{"type":"user","sessionId":"{}","cwd":{},"timestamp":"{stamp}","message":{{"role":"user","content":{}}}}}"#,
            self.uuid,
            json(&self.cwd.display().to_string()),
            json(text)
        ));
        self
    }

    /// An assistant text message.
    pub fn assistant(mut self, text: &str) -> Self {
        let stamp = self.stamp();
        self.lines.push(format!(
            r#"{{"type":"assistant","sessionId":"{}","cwd":{},"timestamp":"{stamp}","message":{{"role":"assistant","model":"claude-opus-4-5","content":[{{"type":"text","text":{}}}]}}}}"#,
            self.uuid,
            json(&self.cwd.display().to_string()),
            json(text)
        ));
        self
    }

    /// An assistant message with a text and a tool call.
    pub fn assistant_tool(mut self, text: &str, tool: &str, input: &str, id: &str) -> Self {
        let stamp = self.stamp();
        self.lines.push(format!(
            r#"{{"type":"assistant","sessionId":"{}","cwd":{},"timestamp":"{stamp}","message":{{"role":"assistant","model":"claude-opus-4-5","content":[{{"type":"text","text":{}}},{{"type":"tool_use","id":"{id}","name":"{tool}","input":{input}}}]}}}}"#,
            self.uuid,
            json(&self.cwd.display().to_string()),
            json(text)
        ));
        self
    }

    /// The result of a tool call (a user record).
    pub fn tool_result(mut self, id: &str, text: &str) -> Self {
        let stamp = self.stamp();
        self.lines.push(format!(
            r#"{{"type":"user","sessionId":"{}","cwd":{},"timestamp":"{stamp}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":{}}}]}}}}"#,
            self.uuid,
            json(&self.cwd.display().to_string()),
            json(text)
        ));
        self
    }

    /// A line that is not JSON (a malformed record).
    pub fn malformed(mut self) -> Self {
        self.lines.push("{this is not json".to_string());
        self
    }

    /// Write `<projects>/<encoded cwd>/<uuid>.jsonl`.
    pub fn write(self, world: &TestWorld) -> PathBuf {
        let mut lines = Vec::new();
        if let Some(summary) = &self.summary {
            lines.push(format!(
                r#"{{"type":"summary","summary":{},"leafUuid":"leaf-{}"}}"#,
                json(summary),
                self.uuid
            ));
        }
        lines.extend(self.lines.iter().cloned());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        world.add_claude_session(&synth::encoded_project_dir(&self.cwd), &self.uuid, &refs)
    }
}

/// The corpus of the agent tests.
pub struct AgentCorpus {
    /// "Token refresh retry": 8 messages (multi-line answers, a tool call).
    pub retry: PathBuf,
    /// "Fix login bug": 4 messages.
    pub login: PathBuf,
    /// 25 messages (Outline: three segments).
    pub outline25: PathBuf,
    /// 12 messages (Outline: one row each).
    pub outline12: PathBuf,
    /// A transcript with one malformed line.
    pub malformed: PathBuf,
}

/// Write the corpus into `world` (every session mentions `alpha` once, so
/// one query hits several conversations).
pub fn agent_corpus(world: &TestWorld) -> AgentCorpus {
    let retry = SessionBuilder::new(1, project_cwd("proj"))
        .summary("Token refresh retry")
        .user("How do I retry the token refresh after a 401? alpha")
        .assistant(
            "Wrap the refresh call in a retry loop.\nUse exponential backoff for the retry delay.\nLog every retry attempt.\nGive up after five retries.",
        )
        .user("Show me the refresh_token call")
        .assistant_tool(
            "Here is the call.",
            "Bash",
            r#"{"command":"grep refresh_token src/auth.rs"}"#,
            "toolu_01",
        )
        .tool_result("toolu_01", "src/auth.rs:42: refresh_token(&client)")
        .assistant("That is the only call site.")
        .user("Thanks, that works")
        .assistant("You are welcome.")
        .write(world);
    let login = SessionBuilder::new(2, project_cwd("proj"))
        .summary("Fix login bug")
        .user("The login page crashes on submit alpha")
        .assistant("The login handler dereferences a missing session.")
        .user("Can you patch the login handler?")
        .assistant("Patched the login handler to check the session first.")
        .write(world);
    let outline25 = outline_session(world, 3, "Outline twenty five", 25);
    let outline12 = outline_session(world, 4, "Outline twelve", 12);
    let malformed = SessionBuilder::new(5, project_cwd("api"))
        .summary("Malformed line")
        .user("alpha before the broken line")
        .malformed()
        .assistant("alpha after the broken line")
        .write(world);
    AgentCorpus {
        retry,
        login,
        outline25,
        outline12,
        malformed,
    }
}

/// A conversation of `messages` alternating messages (`user` first).
pub fn outline_session(world: &TestWorld, number: u32, title: &str, messages: usize) -> PathBuf {
    let mut builder = SessionBuilder::new(number, project_cwd("api")).summary(title);
    for n in 1..=messages {
        builder = if n % 2 == 1 {
            builder.user(&format!("outline topic {n} question"))
        } else {
            builder.assistant(&format!("outline topic {n} answer"))
        };
    }
    builder.write(world)
}

/// Two conversations with the same UUID in two projects: a bare UUID ref is
/// ambiguous. Returns both paths.
pub fn duplicate_uuid_sessions(world: &TestWorld, number: u32) -> (PathBuf, PathBuf) {
    let make = |project: &str, title: &str| {
        SessionBuilder::new(number, project_cwd(project))
            .summary(title)
            .user(&format!("{title} question"))
            .assistant(&format!("{title} answer"))
            .write(world)
    };
    (make("dup-a", "Duplicate A"), make("dup-b", "Duplicate B"))
}

/// Two real workspace folders, each with a session about `needle`:
/// `<home>/work/proj` and `<home>/work/api`. Returns `(proj, api)`.
pub fn workspace_needle_corpus(world: &TestWorld) -> (PathBuf, PathBuf) {
    let proj = world.home().join("work").join("proj");
    let api = world.home().join("work").join("api");
    for (number, dir, title) in [(40, &proj, "Proj needle"), (41, &api, "Api needle")] {
        std::fs::create_dir_all(dir).expect("create the workspace folder");
        SessionBuilder::new(number, dir.clone())
            .summary(title)
            .user(&format!("{title}: where is the needle?"))
            .assistant("The needle is in the haystack.")
            .write(world);
    }
    (proj, api)
}

// ---------------------------------------------------------------------------
// Fake embedders
// ---------------------------------------------------------------------------

/// Controls of the agent fake embedder.
#[derive(Default)]
pub struct AgentEmbedderControl {
    /// Calls of `embed_passages` (non-empty batches).
    pub passage_calls: AtomicUsize,
    /// Passages embedded in total.
    pub passages: AtomicUsize,
    /// Every non-empty `embed_passages` waits for this gate.
    pub gate: Mutex<Option<Arc<Gate>>>,
    /// Every `embed_query` waits for this gate (a slow semantic search).
    pub query_gate: Mutex<Option<Arc<Gate>>>,
    /// Gated calls that have started waiting (a job is really running).
    pub entered: AtomicUsize,
}

struct AgentEmbedder(Arc<AgentEmbedderControl>);

fn embed(text: &str) -> Vec<f32> {
    if text.contains("retry") || text.contains("beta") {
        vec![0.0, 1.0]
    } else {
        vec![1.0, 0.0]
    }
}

impl SemanticEmbedder for AgentEmbedder {
    fn embed_passages(
        &mut self,
        passages: &[String],
    ) -> claude_history_core::Result<Vec<Vec<f32>>> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        self.0.passage_calls.fetch_add(1, Ordering::SeqCst);
        self.0.passages.fetch_add(passages.len(), Ordering::SeqCst);
        let gate = self.0.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            self.0.entered.fetch_add(1, Ordering::SeqCst);
            gate.wait();
        }
        Ok(passages.iter().map(|text| embed(text)).collect())
    }

    fn embed_query(&mut self, query: &str) -> claude_history_core::Result<Option<Vec<f32>>> {
        let gate = self.0.query_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            self.0.entered.fetch_add(1, Ordering::SeqCst);
            gate.wait();
        }
        Ok(Some(embed(query)))
    }
}

/// The fake as a `browser::SemanticEmbedderFactory` (worker tests that
/// start their own `AgentWorkerHandle`).
pub fn agent_embedder_factory(
    control: Arc<AgentEmbedderControl>,
) -> claude_history_core::browser::SemanticEmbedderFactory {
    Arc::new(move || Ok(Box::new(AgentEmbedder(control.clone())) as Box<dyn SemanticEmbedder>))
}

/// Install the agent fake as the app's shared embedder (before the first
/// semantic request); the returned control gates and counts its batches.
pub fn install_agent_embedder(cx: &mut TestAppContext) -> Arc<AgentEmbedderControl> {
    let control = Arc::new(AgentEmbedderControl::default());
    let for_factory = control.clone();
    let factory: ModelFactory = Arc::new(move || {
        Ok(Box::new(AgentEmbedder(for_factory.clone())) as Box<dyn SemanticEmbedder + Send>)
    });
    cx.update(|cx| crate::embedder::install_fake(cx, factory));
    control
}

/// Fill the sandbox's embedding cache with the passages of the conversation
/// at `path` (a semantic Within search with a working fake), so a global
/// semantic or hybrid search has cached chunks to rank and really embeds its
/// query.
pub fn warm_semantic_cache(world: &TestWorld, path: &Path) {
    use claude_history_core::agent::args::{AgentSearchModeArgs, AgentWithinArgs};
    use claude_history_core::agent::{AgentRequest, AgentService};
    use claude_history_core::config::ConfigFile;
    use claude_history_core::search::mode::SearchMode;
    let key = claude_history_core::agent::refs::conversation_key_for_path(
        &world.env,
        &ConfigFile::default(),
        path,
    )
    .expect("keys")
    .expect("a key inside the roots");
    let factory = agent_embedder_factory(Arc::new(AgentEmbedderControl::default()));
    let mut service = AgentService::new(world.env.clone(), ConfigFile::default())
        .with_embedder_factory(Box::new(move || factory()));
    let response = service.respond(AgentRequest::Within(AgentWithinArgs {
        search_mode: AgentSearchModeArgs::explicit(SearchMode::Semantic),
        ..AgentWithinArgs::new(key.conversation_ref().full_ref(), "retry")
    }));
    assert!(response.is_success(), "{}", response.stderr);
}

/// A new file path under `dir` (tests that copy a transcript elsewhere).
pub fn copy_outside_roots(world: &TestWorld, from: &Path) -> PathBuf {
    let outside = world.home().join("outside-the-roots");
    std::fs::create_dir_all(&outside).expect("create the outside folder");
    let to = outside.join(from.file_name().expect("a file name"));
    std::fs::copy(from, &to).expect("copy the transcript");
    to
}
