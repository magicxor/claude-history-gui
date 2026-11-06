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
//!
//! An open also counts the file's malformed lines from the transcript it
//! decoded ([`ParseCount`]): a card's header (store mode) does not know
//! them (V-008), and a second decode beside the build would double the
//! work of every open and escape the abandoning above. Only a header that
//! arrives after the file changed has them counted again ([`Recounter`]),
//! one recount at a time and abandoned like a build.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use claude_history_core::browser::ListHit;
use claude_history_core::history::adapter::is_cancelled;
use claude_history_core::view::{ConversationDocument, ViewPrefs, ViewerModel};

use super::hit::{self, HitFound};
use super::items::{self, BuildInfo, ViewerItem};
use crate::globals::EnvHistory;

/// What to build. With a `hit`, the build also finds the list's search hit
/// in its items (L-109), so they are installed together.
pub enum BuildJob {
    /// Decode `path` through `history` (`ViewerModel::open`) with `prefs`,
    /// then build.
    Open {
        path: PathBuf,
        prefs: ViewPrefs,
        history: EnvHistory,
        hit: Option<ListHit>,
    },
    /// Rebuild from the decoded transcript of `model` (toggles, expansion,
    /// width): the file is never re-read (V-002).
    Rebuild {
        model: ViewerModel,
        hit: Option<ListHit>,
    },
}

/// A finished build.
pub struct Built {
    pub model: ViewerModel,
    pub doc: Arc<ConversationDocument>,
    pub items: Arc<Vec<ViewerItem>>,
    pub info: Arc<BuildInfo>,
    pub cols: usize,
    /// The job's search hit and what was found for it in `items`.
    pub hit: Option<(ListHit, HitFound)>,
    /// An open's count of the file's malformed lines (`None` for a rebuild).
    pub parse_count: Option<ParseCount>,
    /// Wall time of the build (background thread).
    pub build_ms: f64,
}

/// The malformed lines of a file, counted from a transcript decoded from it
/// (V-008; core `export::display_parse_errors` reports each of them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseCount {
    pub path: PathBuf,
    /// The file's modification time, read before the decode.
    pub modified: Option<SystemTime>,
    pub errors: usize,
}

/// The modification time of `path`, if it can be read.
fn file_modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
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
    let (opened, path, model, hit, parse_count) = match job {
        BuildJob::Open {
            path,
            prefs,
            history,
            hit,
        } => {
            // Read before the decode: a write during it makes the count
            // older than the file, never newer.
            let modified = file_modified(&path);
            match ViewerModel::open_cancellable(history.service(), &path, prefs, stale) {
                // Superseded mid-parse (V-101): abandon at once.
                Ok(None) => return None,
                Ok(Some(model)) => {
                    let count = ParseCount {
                        path: path.clone(),
                        modified,
                        errors: model.transcript.diagnostics.len(),
                    };
                    (true, path, model, hit, Some(count))
                }
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
        BuildJob::Rebuild { model, hit } => (false, model.path.clone(), model, hit, None),
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
    let hit = hit.map(|hit| {
        let found = hit::find(&items, &info, &hit);
        (hit, found)
    });
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
            hit,
            parse_count,
            build_ms: started.elapsed().as_secs_f64() * 1000.0,
        }),
    })
}

/// Counts the open file's malformed lines again for a header that arrives
/// after the file changed (V-008): the viewer never re-reads a file it shows
/// (V-082), so no build counts the new version. Single flight like the
/// builds: one recount runs at a time (the gate), and cancelling it (another
/// open, a cleared viewer, a newer recount) stops its decode at the next
/// read of the file (V-101) or before it starts.
#[derive(Default)]
pub struct Recounter {
    gate: Arc<Mutex<()>>,
    /// The latest recount's flag.
    cancel: Arc<AtomicBool>,
}

impl Recounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel the running recount.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// The work of a recount of `known.path` (run it on the background
    /// executor); cancels the previous recount. `None` when the file did not
    /// change since `known` was counted, cannot be read, or the recount was
    /// cancelled.
    pub fn job(
        &mut self,
        known: ParseCount,
        history: EnvHistory,
    ) -> impl FnOnce() -> Option<ParseCount> + Send + 'static {
        self.cancel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let gate = self.gate.clone();
        move || {
            let cancelled = || cancel.load(Ordering::SeqCst);
            if cancelled() {
                return None;
            }
            let _gate = gate.lock().unwrap_or_else(|e| e.into_inner());
            recount(known, &history, &cancelled)
        }
    }
}

/// Count `known.path` again when its modification time changed.
fn recount(
    known: ParseCount,
    history: &EnvHistory,
    cancelled: &dyn Fn() -> bool,
) -> Option<ParseCount> {
    if cancelled() {
        return None;
    }
    let modified = file_modified(&known.path)?;
    if known.modified == Some(modified) {
        return None;
    }
    match history
        .service()
        .open_path_cancellable(&known.path, cancelled)
    {
        Ok((_, transcript)) => Some(ParseCount {
            path: known.path,
            modified: Some(modified),
            errors: transcript.diagnostics.len(),
        }),
        Err(error) if is_cancelled(&error) => None,
        // The new version does not decode: keep the last count, and do not
        // try this version again.
        Err(_) => Some(ParseCount {
            modified: Some(modified),
            ..known
        }),
    }
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
                history: EnvHistory::default(),
                hit: None,
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
                history: EnvHistory::default(),
                hit: None,
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

    const USER: &str = r#"{"type":"user","uuid":"u","timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#;

    /// A session file of `lines` user records and `malformed` broken lines.
    fn session(dir: &Path, lines: usize, malformed: usize) -> PathBuf {
        let path = dir.join("s.jsonl");
        let mut text = format!("{USER}\n").repeat(lines);
        text.push_str(&"{not json\n".repeat(malformed));
        std::fs::write(&path, text).unwrap();
        path
    }

    /// Move the file's modification time on, whatever the file system's
    /// resolution.
    fn touch_later(path: &Path) {
        let modified = std::fs::metadata(path).unwrap().modified().unwrap();
        std::fs::File::options()
            .append(true)
            .open(path)
            .unwrap()
            .set_modified(modified + std::time::Duration::from_secs(2))
            .unwrap();
    }

    /// V-008: an open counts the malformed lines of the transcript it
    /// decoded, with the file's modification time; no second decode, and a
    /// rebuild counts nothing.
    #[test]
    fn an_open_counts_the_malformed_lines_it_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = session(dir.path(), 2, 2);
        let history = EnvHistory::default();
        let worker = ViewerWorker::new();
        let generation = worker.begin();
        let job = BuildJob::Open {
            path: path.clone(),
            prefs: ViewPrefs::default(),
            history: history.clone(),
            hit: None,
        };
        let built = run(generation, job, 80, &|| false)
            .unwrap()
            .outcome
            .unwrap();
        assert_eq!(
            built.parse_count,
            Some(ParseCount {
                path: path.clone(),
                modified: file_modified(&path),
                errors: 2,
            })
        );
        assert_eq!(history.service().decodes(), 1);

        let job = BuildJob::Rebuild {
            model: built.model,
            hit: None,
        };
        let rebuilt = run(worker.begin(), job, 80, &|| false)
            .unwrap()
            .outcome
            .unwrap();
        assert_eq!(rebuilt.parse_count, None);
        assert_eq!(history.service().decodes(), 1, "a rebuild reads nothing");
    }

    /// V-008: a recount decodes only a file that changed since its count.
    #[test]
    fn a_recount_decodes_only_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = session(dir.path(), 2, 1);
        let history = EnvHistory::default();
        let mut recounter = Recounter::new();
        let known = ParseCount {
            path: path.clone(),
            modified: file_modified(&path),
            errors: 1,
        };
        assert_eq!(recounter.job(known.clone(), history.clone())(), None);
        assert_eq!(history.service().decodes(), 0, "unchanged: a stat only");

        let mut file = std::fs::File::options().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, b"{not json either\n").unwrap();
        drop(file);
        touch_later(&path);
        let counted = recounter.job(known, history.clone())().expect("changed");
        assert_eq!(counted.errors, 2);
        assert_eq!(counted.modified, file_modified(&path));
        assert_eq!(history.service().decodes(), 1);
    }

    /// V-101: a recount waiting for the gate gives up when it is cancelled,
    /// before it reads the file.
    #[test]
    fn a_cancelled_recount_waiting_for_the_gate_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = session(dir.path(), 2, 1);
        let history = EnvHistory::default();
        let mut recounter = Recounter::new();
        let known = ParseCount {
            path,
            modified: None,
            errors: 0,
        };
        let job = recounter.job(known, history.clone());
        let held = recounter.gate.lock().unwrap();
        let thread = std::thread::spawn(job);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !thread.is_finished(),
            "the recount waits for the running one"
        );
        recounter.cancel();
        drop(held);
        assert_eq!(thread.join().unwrap(), None);
        assert_eq!(history.service().decodes(), 0);
    }

    /// V-101: a recount whose decode already started is abandoned mid-file
    /// when it is cancelled, long before the file's end.
    #[test]
    fn a_running_recount_is_abandoned_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let path = session(dir.path(), 5000, 1);
        let history = EnvHistory::default();
        // Count cancel polls; cancel on the third (inside the file loop).
        let polls = std::cell::Cell::new(0usize);
        let cancelled = || {
            polls.set(polls.get() + 1);
            polls.get() >= 3
        };
        let known = ParseCount {
            path,
            modified: None,
            errors: 0,
        };
        assert_eq!(recount(known, &history, &cancelled), None);
        assert_eq!(history.service().decodes(), 1, "the decode had started");
        assert!(
            polls.get() < 10,
            "stopped after {} polls, not at EOF",
            polls.get()
        );
    }
}
