//! List fixtures and builders (DESIGN §6, §15.2). Owned by WP1.
//!
//! Builders use `super::TestWorld` and `super::synth`; GUI-only fixture
//! files go into `crates/gui/tests/fixtures/list/`.
//!
//! * [`listed_corpus`]: the standard corpus without its malformed-line
//!   session, so the list shows exactly 8 rows (6 Claude sessions in 3
//!   projects, 1 Pi, 1 OMP) — the numbers FEATURES.md uses.
//! * [`ListFixture`]: a main window over a world with helpers to type, press
//!   keys, wait for the loader and the search workers and read the list.
//! * [`FakeEmbedder`]: deterministic embeddings with switches for failing,
//!   slow (gated) model initialisation and gated embedding batches.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use claude_history_core::semantic::embed::SemanticEmbedder;
use gpui_kit::AppContext as _;
use gpui_kit::{AnyWindowHandle, App, Entity, TestAppContext};

use crate::embedder::ModelFactory;
use crate::list::ListPane;
use crate::workspace::Workspace;

use super::{TestWorld, open_main, synth};

/// The standard corpus minus its malformed-line session: 8 listed rows.
/// Returns the written transcripts (Claude first, then Pi and OMP).
pub fn listed_corpus(world: &TestWorld) -> Vec<PathBuf> {
    let mut written = world.standard_corpus();
    // The malformed-line session is the 8th written file (index 7).
    let malformed = written.remove(7);
    std::fs::remove_file(&malformed).expect("remove the malformed transcript");
    // Distinct modification times in write order (the order the rows had
    // anyway): files written in the same coarse clock tick tie, and a tie's
    // order follows the project folders' order, which a later write changes
    // (integrator, Phase A: refresh_reports_and_keeps_the_selection was
    // flaky under a loaded test run).
    let now = chrono::Utc::now();
    let count = written.len() as i64;
    for (ix, path) in written.iter().enumerate() {
        set_mtime(
            path,
            now - chrono::Duration::seconds(2 * (count - ix as i64)),
        );
    }
    written
}

/// Add a Claude session in `project` (a name under the synthetic base) with
/// a summary `title`; `day` spreads the timestamps. Returns the path.
pub fn add_session(
    world: &TestWorld,
    project: &str,
    number: u32,
    title: &str,
    day: u32,
) -> PathBuf {
    let cwd = synth::cwd_base().join(project);
    let uuid = synth::uuid(number);
    let lines = synth::claude_conversation(&uuid, &cwd, title, day, 1);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.add_claude_session(&synth::encoded_project_dir(&cwd), &uuid, &refs)
}

/// A Claude session to write: summary / custom title, messages with
/// timestamps `start + n x spacing`.
pub struct SessionSpec<'a> {
    pub project: &'a str,
    pub number: u32,
    pub summary: Option<&'a str>,
    pub custom_title: Option<&'a str>,
    /// `(role, text)` with role `user` or `assistant`.
    pub messages: Vec<(&'a str, String)>,
    /// First message time (UTC); `None` = now.
    pub start: Option<chrono::DateTime<chrono::Utc>>,
    pub spacing_minutes: i64,
    /// The session's working directory (default: under `synth::cwd_base()`).
    pub cwd: Option<PathBuf>,
}

impl<'a> SessionSpec<'a> {
    pub fn new(project: &'a str, number: u32) -> Self {
        Self {
            project,
            number,
            summary: None,
            custom_title: None,
            messages: vec![
                ("user", "first question".to_string()),
                ("assistant", "first answer".to_string()),
            ],
            start: None,
            spacing_minutes: 1,
            cwd: None,
        }
    }

    /// Use `cwd` as the working directory (it determines the project).
    pub fn cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }

    pub fn summary(mut self, summary: &'a str) -> Self {
        self.summary = Some(summary);
        self
    }

    pub fn custom_title(mut self, title: &'a str) -> Self {
        self.custom_title = Some(title);
        self
    }

    pub fn messages(mut self, messages: &[(&'a str, &str)]) -> Self {
        self.messages = messages
            .iter()
            .map(|(role, text)| (*role, text.to_string()))
            .collect();
        self
    }

    pub fn start(mut self, start: chrono::DateTime<chrono::Utc>) -> Self {
        self.start = Some(start);
        self
    }

    pub fn spacing_minutes(mut self, minutes: i64) -> Self {
        self.spacing_minutes = minutes;
        self
    }

    /// Write the session; returns its path.
    pub fn write(self, world: &TestWorld) -> PathBuf {
        let cwd = self
            .cwd
            .clone()
            .unwrap_or_else(|| synth::cwd_base().join(self.project));
        let uuid = synth::uuid(self.number);
        let cwd_json = serde_json::to_string(&cwd.display().to_string()).unwrap();
        let mut lines = Vec::new();
        if let Some(summary) = self.summary {
            lines.push(format!(
                r#"{{"type":"summary","summary":{},"leafUuid":"leaf-{uuid}"}}"#,
                serde_json::to_string(summary).unwrap()
            ));
        }
        if let Some(title) = self.custom_title {
            lines.push(format!(
                r#"{{"type":"custom-title","customTitle":{},"sessionId":"{uuid}"}}"#,
                serde_json::to_string(title).unwrap()
            ));
        }
        let start = self.start.unwrap_or_else(chrono::Utc::now);
        for (ix, (role, text)) in self.messages.iter().enumerate() {
            let stamp = (start + chrono::Duration::minutes(self.spacing_minutes * ix as i64))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            let text_json = serde_json::to_string(text).unwrap();
            let line = if *role == "user" {
                format!(
                    r#"{{"type":"user","sessionId":"{uuid}","cwd":{cwd_json},"timestamp":"{stamp}","message":{{"role":"user","content":{text_json}}}}}"#
                )
            } else {
                format!(
                    r#"{{"type":"assistant","sessionId":"{uuid}","cwd":{cwd_json},"timestamp":"{stamp}","message":{{"role":"assistant","model":"claude-opus-4-5","content":[{{"type":"text","text":{text_json}}}]}}}}"#
                )
            };
            lines.push(line);
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = world.add_claude_session(&synth::encoded_project_dir(&cwd), &uuid, &refs);
        let last = start
            + chrono::Duration::minutes(
                self.spacing_minutes * self.messages.len().saturating_sub(1) as i64,
            );
        set_mtime(&path, last);
        path
    }
}

/// Set a transcript's modification time (core's conversation timestamp and
/// time filter both use it).
pub fn set_mtime(path: &std::path::Path, at: chrono::DateTime<chrono::Utc>) {
    let time: std::time::SystemTime = at.into();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open the transcript");
    file.set_modified(time).expect("set the modification time");
}

/// A corpus with real workspace folders (`--workspace` needs an existing
/// directory): two sessions in each of `<home>/work/proj`, its worktree
/// `<home>/work/proj/.worktrees/feature` and `<home>/work/api`. Returns
/// `(proj, worktree, api)` directories.
pub fn workspace_corpus(world: &TestWorld) -> (PathBuf, PathBuf, PathBuf) {
    let proj = world.home().join("work").join("proj");
    let worktree = proj.join(".worktrees").join("feature");
    let api = world.home().join("work").join("api");
    for dir in [&proj, &worktree, &api] {
        std::fs::create_dir_all(dir).expect("create the workspace folder");
    }
    let now = chrono::Utc::now();
    let mut number = 100;
    for (dir, project) in [(&proj, "proj"), (&worktree, "worktree"), (&api, "api")] {
        for n in 0..2 {
            number += 1;
            SessionSpec::new(project, number)
                .cwd(dir.clone())
                .summary(&format!("{project} session {n}"))
                .start(now - chrono::Duration::hours(i64::from(number)))
                .write(world);
        }
    }
    (proj, worktree, api)
}

/// The events recorded by [`ListFixture::record_events`].
#[derive(Clone, Default)]
pub struct Events(pub std::rc::Rc<std::cell::RefCell<Vec<crate::list::ListPaneEvent>>>);

impl Events {
    pub fn all(&self) -> Vec<crate::list::ListPaneEvent> {
        self.0.borrow().clone()
    }

    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// The `SelectionChanged` events.
    pub fn selections(&self) -> Vec<(Option<crate::model::ConversationTarget>, bool)> {
        self.0
            .borrow()
            .iter()
            .filter_map(|event| match event {
                crate::list::ListPaneEvent::SelectionChanged { target, user } => {
                    Some((target.clone(), *user))
                }
                _ => None,
            })
            .collect()
    }

    pub fn count_matching(&self, f: impl Fn(&crate::list::ListPaneEvent) -> bool) -> usize {
        self.0.borrow().iter().filter(|event| f(event)).count()
    }
}

/// A main window over a world, with helpers.
pub struct ListFixture {
    pub handle: AnyWindowHandle,
    pub ws: Entity<Workspace>,
    pub list: Entity<ListPane>,
}

impl ListFixture {
    /// Open the main window on `world` with `args` (no waiting).
    pub fn open(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) -> Self {
        let (handle, ws) = open_main(cx, world, args);
        let list = cx.read(|cx| ws.read(cx).list().clone());
        Self { handle, ws, list }
    }

    /// Initialise the test app (state, services, globals) without opening a
    /// window, so a test can install a fake embedder before the first load.
    pub fn init_app(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) {
        let launch = super::launch_for_args(world, args);
        let recording = Arc::new(crate::services::RecordingServices::new());
        cx.update(|cx| crate::app::init_for_test(cx, world, launch, recording));
        cx.executor().allow_parking();
    }

    /// Open and wait for the initial load (real time, ≤ 30 s).
    pub fn open_loaded(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) -> Self {
        let fixture = Self::open(cx, world, args);
        fixture.wait_loaded(cx);
        fixture
    }

    /// Wait until the initial load and every loader/search worker are idle.
    pub fn wait_loaded(&self, cx: &mut TestAppContext) {
        self.wait_until(cx, "the list to finish loading", |list, _| {
            !list.is_loading() && !list.loader().is_running()
        });
        self.wait_idle(cx);
    }

    /// Wait until no search worker has pending work.
    pub fn wait_idle(&self, cx: &mut TestAppContext) {
        self.wait_until(cx, "the search workers to go idle", |list, _| {
            !list.browser().has_search_work_in_flight() && !list.loader().is_running()
        });
    }

    /// Wait (real time, ≤ 30 s) until `done` holds.
    pub fn wait_until(
        &self,
        cx: &mut TestAppContext,
        what: &str,
        done: impl Fn(&ListPane, &App) -> bool,
    ) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| done(self.list.read(cx), cx)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {}",
                cx.read(|cx| self.list.read(cx).debug_state())
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    /// Open with the poll loop paused: the load only advances through
    /// [`ListFixture::step`], so a test can observe and act mid-load.
    pub fn open_paused(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) -> Self {
        crate::list::PAUSE_NEW_POLLS.with(|pause| pause.set(true));
        let fixture = Self::open(cx, world, args);
        crate::list::PAUSE_NEW_POLLS.with(|pause| pause.set(false));
        fixture
    }

    /// One poll of the list by hand (paused fixtures).
    pub fn step(&self, cx: &mut TestAppContext) {
        let list = self.list.clone();
        cx.update_window(self.handle, |_, window, cx| {
            list.update(cx, |list, cx| list.step(window, cx))
        })
        .expect("the window is open");
    }

    /// Step by hand (real time, <= 30 s) until `done` holds.
    pub fn step_until(
        &self,
        cx: &mut TestAppContext,
        what: &str,
        done: impl Fn(&ListPane) -> bool,
    ) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.step(cx);
            cx.run_until_parked();
            if cx.read(|cx| done(self.list.read(cx))) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {}",
                cx.read(|cx| self.list.read(cx).debug_state())
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    /// Give the poll loop back to the wake-ups (paused fixtures).
    pub fn resume(&self, cx: &mut TestAppContext) {
        self.list.update(cx, |list, _| list.set_poll_paused(false));
        self.step(cx);
    }

    /// Run `f` in the main window with the list.
    pub fn with_window<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut ListPane, &mut gpui_kit::Window, &mut gpui_kit::Context<ListPane>) -> R,
    ) -> R {
        let list = self.list.clone();
        cx.update_window(self.handle, |_, window, cx| {
            list.update(cx, |list, cx| f(list, window, cx))
        })
        .expect("the window is open")
    }

    /// Record the events the list emits from now on.
    pub fn record_events(&self, cx: &mut TestAppContext) -> Events {
        let events: Events = Events::default();
        let sink = events.clone();
        let list = self.list.clone();
        cx.update(|cx| {
            cx.subscribe(&list, move |_, event: &crate::list::ListPaneEvent, _| {
                sink.0.borrow_mut().push(event.clone());
            })
            .detach();
        });
        events
    }

    /// Read the list.
    pub fn read<R>(&self, cx: &TestAppContext, f: impl FnOnce(&ListPane, &App) -> R) -> R {
        cx.read(|cx| f(self.list.read(cx), cx))
    }

    /// The listed rows' paths in display order.
    pub fn paths(&self, cx: &TestAppContext) -> Vec<PathBuf> {
        self.read(cx, |list, _| list.filtered_paths())
    }

    /// The listed rows' titles (line 1) in display order.
    pub fn titles(&self, cx: &TestAppContext) -> Vec<String> {
        self.read(cx, |list, _| {
            list.row_models(0..list.filtered_paths().len(), 480.0)
                .into_iter()
                .map(|row| row.title_text().to_string())
                .collect()
        })
    }

    /// Type `text` into the focused element (one keystroke per character).
    pub fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        cx.simulate_input(self.handle, text);
    }

    /// Press keys (`"down down enter"`).
    pub fn press(&self, cx: &mut TestAppContext, keys: &str) {
        cx.simulate_keystrokes(self.handle, keys);
    }

    /// The selected row's path.
    pub fn selected_path(&self, cx: &TestAppContext) -> Option<PathBuf> {
        self.read(cx, |list, _| list.selected_target().map(|t| t.path))
    }

    /// The selected position in the filtered list.
    pub fn selected_position(&self, cx: &TestAppContext) -> Option<usize> {
        self.read(cx, |list, _| list.browser().selected())
    }
}

// ---------------------------------------------------------------------------
// Fake embedder
// ---------------------------------------------------------------------------

/// A gate that blocks until opened (worker threads wait on it).
#[derive(Default)]
pub struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    pub fn closed() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    /// Block (≤ 30 s) until opened.
    pub fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !*open {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            open = self.changed.wait_timeout(open, left).unwrap().0;
        }
    }
}

/// Switches and counters of the fake embedder.
#[derive(Default)]
pub struct FakeEmbedderControl {
    /// Models created.
    pub loads: AtomicUsize,
    /// Calls of `embed_passages`.
    pub passage_calls: AtomicUsize,
    /// Passages embedded in total.
    pub passages: AtomicUsize,
    /// Model creation waits for this gate (progress: InitializingModel).
    pub init_gate: Mutex<Option<Arc<Gate>>>,
    /// `embed_passages` calls from the second on wait for this gate.
    pub batch_gate: Mutex<Option<Arc<Gate>>>,
}

/// Deterministic embeddings: text mentioning "beta" points one way,
/// everything else the other.
pub struct FakeEmbedder {
    control: Arc<FakeEmbedderControl>,
}

fn embedding(text: &str) -> Vec<f32> {
    if text.contains("beta") {
        vec![0.0, 1.0]
    } else {
        vec![1.0, 0.0]
    }
}

impl SemanticEmbedder for FakeEmbedder {
    fn embed_passages(
        &mut self,
        passages: &[String],
    ) -> claude_history_core::Result<Vec<Vec<f32>>> {
        if passages.is_empty() {
            // The list's warm-up call that only loads the model.
            return Ok(Vec::new());
        }
        let call = self.control.passage_calls.fetch_add(1, Ordering::SeqCst);
        self.control
            .passages
            .fetch_add(passages.len(), Ordering::SeqCst);
        if call >= 1 {
            let gate = self.control.batch_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait();
            }
        }
        Ok(passages.iter().map(|passage| embedding(passage)).collect())
    }

    fn embed_query(&mut self, query: &str) -> claude_history_core::Result<Option<Vec<f32>>> {
        Ok(Some(embedding(query)))
    }
}

/// A model factory producing [`FakeEmbedder`]s controlled by `control`.
pub fn fake_factory(control: Arc<FakeEmbedderControl>) -> ModelFactory {
    Arc::new(move || {
        control.loads.fetch_add(1, Ordering::SeqCst);
        let gate = control.init_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.wait();
        }
        Ok(Box::new(FakeEmbedder {
            control: control.clone(),
        }) as Box<dyn SemanticEmbedder + Send>)
    })
}

/// Install the fake as the app's shared embedder; returns its control.
pub fn install_fake_embedder(cx: &mut TestAppContext) -> Arc<FakeEmbedderControl> {
    let control = Arc::new(FakeEmbedderControl::default());
    let factory = fake_factory(control.clone());
    cx.update(|cx| crate::embedder::install_fake(cx, factory));
    control
}

/// Install a model factory that fails with `message`.
pub fn install_failing_embedder(cx: &mut TestAppContext, message: &'static str) {
    cx.update(|cx| {
        crate::embedder::install_fake(cx, crate::embedder::refusing_factory(message));
    });
}
