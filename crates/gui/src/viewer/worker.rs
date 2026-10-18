//! The viewer's build worker (DESIGN §7.5, §9.1): a single latest-request
//! slot, an `AtomicU64` generation, parse → document → items → segments off
//! the UI thread; stale work is abandoned.
//!
//! Each request bumps the generation; the pane keeps only the task of the
//! latest request (dropping the previous one cancels it when it has not
//! started), and a running build checks the generation between phases and
//! every 64 messages, so arrowing through the list never queues full parses
//! (V-101). The pane installs a result only when its generation is still
//! current.
//!
//! As built: builds run on the gpui background executor instead of a
//! dedicated `viewer-worker` thread. The GPUI test scheduler rejects wakeups
//! from foreign threads (non-deterministic tests), and the frozen contract
//! tests open viewers without allowing that; background tasks keep every
//! property of the design (single flight, generations, abandoning) and run
//! deterministically in tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use claude_history_core::view::{ConversationDocument, ViewPrefs, ViewerModel};

use super::items::{self, BuildInfo, ViewerItem};

/// What to build.
pub enum BuildJob {
    /// Parse `path` (`ViewerModel::open`) with `prefs`, then build.
    Open { path: PathBuf, prefs: ViewPrefs },
    /// Rebuild from the cached entries of `model` (toggles, expansion,
    /// width): the file is never re-read (V-002).
    Rebuild { model: ViewerModel },
}

/// A finished build.
pub struct Built {
    pub model: ViewerModel,
    pub doc: Arc<ConversationDocument>,
    pub items: Arc<Vec<ViewerItem>>,
    pub info: Arc<BuildInfo>,
    pub cols: usize,
    /// Wall time of the build (background thread).
    pub build_ms: f64,
}

/// A build's outcome.
pub struct BuildResult {
    pub generation: u64,
    pub path: PathBuf,
    /// The job was an open (not a rebuild).
    pub opened: bool,
    pub outcome: Result<Built, String>,
}

/// Generation counter and statistics shared with running builds.
#[derive(Default)]
struct Shared {
    /// Single flight: only one build runs at a time; a request that gets
    /// the gate after being superseded returns without building.
    gate: std::sync::Mutex<()>,
    generation: AtomicU64,
    completed: AtomicUsize,
}

/// The pane's build worker.
#[derive(Default)]
pub struct ViewerWorker {
    shared: Arc<Shared>,
}

impl ViewerWorker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new request: every older build becomes stale. Returns the
    /// request's generation.
    pub fn begin(&self) -> u64 {
        self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Make every running build stale (the viewer was cleared).
    pub fn invalidate(&self) -> u64 {
        self.begin()
    }

    /// The current generation.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::SeqCst)
    }

    /// Builds that ran to completion (stale ones abandoned midway, and
    /// requests replaced before they started, do not count).
    pub fn completed_builds(&self) -> usize {
        self.shared.completed.load(Ordering::SeqCst)
    }

    /// The work of request `generation` (run it on the background executor).
    /// `None` when it became stale.
    pub fn job(
        &self,
        generation: u64,
        job: BuildJob,
        cols: usize,
    ) -> impl FnOnce() -> Option<BuildResult> + Send + 'static {
        let shared = self.shared.clone();
        move || {
            let stale = || shared.generation.load(Ordering::SeqCst) != generation;
            if stale() {
                return None;
            }
            let _gate = shared.gate.lock().unwrap_or_else(|e| e.into_inner());
            if stale() {
                return None;
            }
            let result = run(generation, job, cols, &stale)?;
            shared.completed.fetch_add(1, Ordering::SeqCst);
            Some(result)
        }
    }
}

/// Run one build; `None` when it became stale.
fn run(
    generation: u64,
    job: BuildJob,
    cols: usize,
    stale: &dyn Fn() -> bool,
) -> Option<BuildResult> {
    let started = Instant::now();
    let (opened, path, model) = match job {
        BuildJob::Open { path, prefs } => {
            match ViewerModel::open_cancellable(&path, prefs, stale) {
                // Superseded mid-parse (V-101): abandon at once.
                Ok(None) => return None,
                Ok(Some(model)) => (true, path, model),
                Err(error) => {
                    return Some(BuildResult {
                        generation,
                        path,
                        opened: true,
                        outcome: Err(error.to_string()),
                    });
                }
            }
        }
        BuildJob::Rebuild { model } => (false, model.path.clone(), model),
    };
    if stale() {
        return None;
    }
    let doc = model.document();
    if stale() {
        return None;
    }
    let (items, info) = items::build_with_info(&doc, &model, cols, stale)?;
    if stale() {
        return None;
    }
    Some(BuildResult {
        generation,
        path,
        opened,
        outcome: Ok(Built {
            model,
            doc: Arc::new(doc),
            items: Arc::new(items),
            info: Arc::new(info),
            cols,
            build_ms: started.elapsed().as_secs_f64() * 1000.0,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single flight: a request that waits for the gate while a newer one
    /// was begun returns without building.
    #[test]
    fn a_superseded_request_waiting_for_the_gate_is_dropped() {
        let worker = ViewerWorker::new();
        let first = worker.begin();
        let job = worker.job(
            first,
            BuildJob::Open {
                path: PathBuf::from("does-not-exist.jsonl"),
                prefs: ViewPrefs::default(),
            },
            80,
        );
        let shared = worker.shared.clone();
        let held = shared.gate.lock().unwrap();
        let thread = std::thread::spawn(move || job().is_none());
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!thread.is_finished(), "the build waits for the running one");
        worker.begin();
        drop(held);
        assert!(thread.join().unwrap(), "superseded while it waited");
        assert_eq!(worker.completed_builds(), 0);
    }

    /// V-101: a build that already started its parse is abandoned mid-file
    /// when a newer request begins, long before the file's end.
    #[test]
    fn a_running_parse_is_abandoned_when_superseded() {
        let dir = std::env::temp_dir().join(format!("chg-v101-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.jsonl");
        let line = r#"{"type":"user","uuid":"u","timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#;
        std::fs::write(&path, format!("{line}\n").repeat(5000)).unwrap();
        let worker = ViewerWorker::new();
        let first = worker.begin();
        // Count cancel polls; supersede on the third (inside the file loop).
        let polls = std::cell::Cell::new(0usize);
        let shared = worker.shared.clone();
        let stale = || {
            polls.set(polls.get() + 1);
            if polls.get() == 3 {
                shared.generation.fetch_add(1, Ordering::SeqCst);
            }
            shared.generation.load(Ordering::SeqCst) != first
        };
        let out = run(
            first,
            BuildJob::Open {
                path,
                prefs: ViewPrefs::default(),
            },
            80,
            &stale,
        );
        assert!(out.is_none(), "abandoned");
        assert!(
            polls.get() < 10,
            "stopped after {} polls, not at EOF",
            polls.get()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
