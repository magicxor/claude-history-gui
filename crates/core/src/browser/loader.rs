//! Fills a [`Browser`] with the history (upstream `tui/runtime.rs`): from the
//! store's generations (store mode, the GUI's main list), or else by
//! streaming [`crate::history::load_all_conversations_streaming`] (the
//! initial load and the buffered refresh, with upstream's status strings).
//!
//! # Store mode
//!
//! With [`LoaderOptions::snapshot`] on and a store root
//! ([`crate::store::default_root`]) the loader does what upstream's runtime
//! `Store` does (358becb): [`HistoryLoader::open_snapshot`] opens the current
//! generation for the first frame ([`Browser::show_snapshot`]), a
//! `generation-open` thread materializes all its cards and opens its index,
//! and a later [`HistoryLoader::poll`] installs them
//! ([`Browser::install_generation`]). [`HistoryLoader::start_initial_load`]
//! starts a reconcile pass on a `store-reconcile` thread
//! ([`crate::store::reconcile`]): it stats every session file of the env's
//! sources, parses only new or changed ones and publishes them as delta
//! generations. While a pass runs, its `store-watch` thread checks `CURRENT`
//! every [`GENERATION_POLL`] and wakes the poll when it names a newer
//! generation, which is opened off the UI thread and swapped in keeping the
//! query, the scope and the selected session. A rename or a delete asks for
//! another pass ([`Browser::take_reconcile_request`]), run after the one
//! running; a refresh runs a pass and reports
//! [`LoadEvent::Refreshed`] from the generation sizes. Every thread sends its
//! result before it calls the waker, so the poll it schedules finds it.
//!
//! A pass that parses at least [`crate::store::progress::MIN_FILES`] files
//! reports its progress in the store's `PROGRESS` file (upstream 04c714b);
//! the `store-watch` thread reads it with `CURRENT` (from this process or
//! another holding the lock) and wakes the poll when it changes, and the
//! poll hands it to the browser ([`Browser::set_index_progress`]) while a
//! pass runs or is due. A pass shown that way leaves an
//! [`super::IndexNotice::Indexed`] when it ends. A failed pass is reported
//! by [`LoadEvent::RefreshFailed`] during a refresh, by
//! [`LoadEvent::LoadFailed`] (`indexing sessions failed: …`) when the
//! initial load's pass fails and no card is listed, and otherwise by an
//! [`super::IndexNotice::Failed`] ([`Browser::index_failed`]).
//!
//! # In memory
//!
//! Without a store, [`HistoryLoader::poll`] never runs O(corpus) work on the
//! caller's thread: when a stream ends, the corpus is prepared
//! ([`Browser::prepare_corpus`]) on a `corpus-prepare` thread and a later
//! poll installs it and returns the [`LoadEvent`]. While loading, one poll
//! appends batches for at most [`LoaderOptions::drain_budget`] and leaves
//! the rest to the next poll (upstream `tui/runtime.rs` stopped draining the
//! loader channel after 8 ms so a frame could render between batches;
//! upstream 358becb loads in memory synchronously before the first frame
//! instead).
//!
//! # Differences from upstream
//!
//! * **In-process passes.** A pass runs on the `store-reconcile` thread,
//!   with its own pool of at most four threads (half the cores,
//!   `store-reconcile-*`), instead of a detached `--reconcile` child at
//!   nice 10 (decision: the GUI never spawns itself). Upstream's child
//!   outlived the TUI; here dropping the loader (at quit) gives the pass up
//!   ([`crate::store::reconcile::reconcile_cancellable`]) so quitting never
//!   waits for it; the generations it published stay and the next pass goes
//!   on from them. A pass that finds the store locked by another process (a
//!   second GUI) tries again every [`BUSY_RETRY`] on its thread (upstream:
//!   the child exited with status 75 and the TUI started another a second
//!   later). The fork does not lower the threads' priority yet: the
//!   thread-priority port (upstream 8322afe/9ee0a03) covers
//!   `store-reconcile` and its pool as well as the semantic worker.
//! * **Waker-driven `CURRENT` checks.** Upstream's event loop read `CURRENT`
//!   (and `PROGRESS`, 04c714b) every 200 ms while a pass could publish; here
//!   the pass's `store-watch` thread does, so an idle GUI never polls. A
//!   failed pass's message comes from its error, not from `PROGRESS`.
//! * **Off the UI thread.** Upstream opened each generation, its index and
//!   the cards of the browse list on the TUI thread; here a
//!   `generation-open` thread does (see `cards`).
//! * **Events.** The GUI keeps the in-memory events: [`LoadEvent::Loaded`]
//!   when the first generation is shown in full, [`LoadEvent::NoHistory`]
//!   when the first pass leaves no card in the time filter (upstream exited
//!   with no history found), [`LoadEvent::LoadFailed`] when it failed with
//!   no card listed (upstream exited with `indexing sessions failed: …`),
//!   [`LoadEvent::Refreshed`] / [`LoadEvent::RefreshFailed`] when a
//!   refresh's pass and the swap it led to are done (upstream showed
//!   `Indexing failed: …` for a refresh's failed pass). A swap outside a
//!   refresh only sets [`LoaderPoll::swapped`].
//! * **Runtime settings.** The time filter ([`HistoryLoader::set_time_filter`])
//!   re-scopes the cards shown and a new preview mode reopens the
//!   generation; a new workspace whose Pi/OMP session roots differ moves to
//!   that store root ([`HistoryLoader::reconfigure`]). Upstream fixed them at
//!   launch.

use super::cards::GenerationOpen;
use super::corpus::corpus_edit_mark;
use super::{Browser, PreparedCorpus, Waker};
use crate::debug::DebugLevel;
use crate::env::HistoryEnv;
use crate::error::AppError;
use crate::history::sources::Registry;
use crate::history::{Conversation, LoaderMessage};
use crate::store::generation::{MergePolicy, Publish};
use crate::store::progress::{self, Indexing};
use crate::store::reconcile::{self, Outcome};
use crate::store::{Generation, Snapshot};
use crate::time_filter::TimeFilter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Status shown when a refresh starts (upstream wording).
pub const REFRESHING_STATUS: &str = "Refreshing sessions...";

/// How long a reconcile pass that found the store locked by another
/// process waits before it tries again (upstream `BUSY_RETRY`).
pub const BUSY_RETRY: Duration = Duration::from_secs(1);

/// How often `CURRENT` is checked while a reconcile pass may publish
/// (upstream `GENERATION_POLL`).
pub const GENERATION_POLL: Duration = Duration::from_millis(200);

/// Default [`LoaderOptions::drain_budget`]: upstream's TUI stopped draining
/// the loader channel after 8 ms so a frame could render.
pub const DEFAULT_DRAIN_BUDGET: Duration = Duration::from_millis(8);

/// Settings of every load started by a [`HistoryLoader`].
#[derive(Clone)]
pub struct LoaderOptions {
    /// Row preview from the last messages (upstream default) or the first.
    pub show_last: bool,
    /// Loader diagnostics level (upstream `--debug`), sent to
    /// [`crate::debug`]'s sink.
    pub debug_level: Option<DebugLevel>,
    /// Only conversations in this time range (upstream `--since/--before`).
    /// Inactive by default.
    pub time_filter: TimeFilter,
    /// Called from the loader's threads after each result they post.
    /// [`HistoryLoader::poll`] also calls it synchronously on the caller's
    /// thread when the drain budget holds a batch back
    /// ([`LoaderPoll::pending`]), and the start of a load or a refresh in
    /// store mode calls it too, so a waker must only schedule a later poll,
    /// never poll re-entrantly.
    pub waker: Option<Waker>,
    /// How long one [`HistoryLoader::poll`] keeps appending batches to the
    /// browser during an in-memory initial load. A poll always applies at
    /// least one batch; once the budget is spent the next batch waits for a
    /// later poll ([`LoaderPoll::pending`], and the waker is called).
    /// Messages that are not batches never wait. `None` drains everything in
    /// one poll. Default [`DEFAULT_DRAIN_BUDGET`].
    pub drain_budget: Option<Duration>,
    /// List from the store's generations under
    /// [`crate::store::default_root`] (store mode, see the module
    /// documentation) instead of loading the history in memory. Off by
    /// default (tests and tools); the GUI turns it on for its main window.
    pub snapshot: bool,
}

impl Default for LoaderOptions {
    /// Last-message previews (upstream default), no debug output, no time
    /// filter, no waker, the default drain budget.
    fn default() -> Self {
        Self::new(true)
    }
}

impl LoaderOptions {
    pub fn new(show_last: bool) -> Self {
        Self {
            show_last,
            debug_level: None,
            time_filter: TimeFilter::default(),
            waker: None,
            drain_budget: Some(DEFAULT_DRAIN_BUDGET),
            snapshot: false,
        }
    }

    pub fn with_debug_level(mut self, level: Option<DebugLevel>) -> Self {
        self.debug_level = level;
        self
    }

    pub fn with_time_filter(mut self, time_filter: TimeFilter) -> Self {
        self.time_filter = time_filter;
        self
    }

    pub fn with_waker(mut self, waker: Waker) -> Self {
        self.waker = Some(waker);
        self
    }

    pub fn with_drain_budget(mut self, budget: Option<Duration>) -> Self {
        self.drain_budget = budget;
        self
    }

    pub fn with_snapshot(mut self, snapshot: bool) -> Self {
        self.snapshot = snapshot;
        self
    }
}

impl std::fmt::Debug for LoaderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoaderOptions")
            .field("show_last", &self.show_last)
            .field("debug_level", &self.debug_level)
            .field("time_filter", &self.time_filter)
            .field("waker", &self.waker.is_some())
            .field("drain_budget", &self.drain_budget)
            .field("snapshot", &self.snapshot)
            .finish()
    }
}

/// What an in-memory refresh does when a project (or a Pi/OMP root) failed
/// to load. Store mode has no project errors and ignores it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RefreshPolicy {
    /// Keep the current sessions ([`LoadEvent::RefreshIncomplete`]), as
    /// upstream's Ctrl+L.
    #[default]
    KeepOnProjectError,
    /// Apply what was loaded and report the failed projects in
    /// [`LoadEvent::Refreshed`]'s `project_errors`. Used for reloads that
    /// change what is loaded (time filter, workspace), where keeping rows
    /// loaded under the old settings would be wrong.
    ApplyPartial,
}

/// How a load ended.
#[derive(Debug)]
pub enum LoadEvent {
    /// The initial load finished and the browser is ready (store mode: the
    /// first generation is shown in full). `project_errors` counts projects
    /// (or Pi/OMP roots) that failed to load in memory; upstream ignored
    /// them during the initial load.
    Loaded {
        conversations: usize,
        project_errors: usize,
    },
    /// The initial load found nothing (store mode: the first pass left no
    /// card in the time filter): `NoHistoryFound("selected scope")`, or
    /// `"selected scope within the requested time range"` when a time
    /// filter is active. Upstream exited with this error; the browser is
    /// ready and empty.
    NoHistory(AppError),
    /// The initial load failed before producing any data (for example
    /// `ProjectsDirNotFound`, or a first pass that failed with no
    /// generation). Upstream exited with this error; the browser is ready
    /// (and normally empty), so a refresh can retry the load.
    LoadFailed(AppError),
    /// A refresh replaced the corpus (store mode: its pass ended and the
    /// generation it left is shown; the counts are generation sizes).
    /// `project_errors` counts projects that failed to load (only non-zero
    /// in memory with [`RefreshPolicy::ApplyPartial`]).
    Refreshed {
        old: usize,
        new: usize,
        project_errors: usize,
    },
    /// A refresh hit a project error; the existing sessions were kept.
    RefreshIncomplete,
    /// A refresh failed; the existing sessions were kept.
    RefreshFailed(AppError),
    /// The refresh loader stopped without finishing; the existing sessions
    /// were kept.
    RefreshInterrupted,
}

impl LoadEvent {
    /// The upstream status-bar message for this event, if any.
    pub fn status_message(&self) -> Option<String> {
        match self {
            LoadEvent::Loaded { .. } | LoadEvent::NoHistory(_) | LoadEvent::LoadFailed(_) => None,
            LoadEvent::Refreshed { old, new, .. } => {
                Some(format!("Refreshed sessions: {old} -> {new}"))
            }
            LoadEvent::RefreshIncomplete => {
                Some("Refresh incomplete; keeping existing sessions".to_string())
            }
            LoadEvent::RefreshFailed(error) => Some(format!("Refresh failed: {error}")),
            LoadEvent::RefreshInterrupted => {
                Some("Refresh failed; keeping existing sessions".to_string())
            }
        }
    }

    /// Whether this event ends a refresh.
    pub fn is_refresh(&self) -> bool {
        matches!(
            self,
            LoadEvent::Refreshed { .. }
                | LoadEvent::RefreshIncomplete
                | LoadEvent::RefreshFailed(_)
                | LoadEvent::RefreshInterrupted
        )
    }
}

/// What one [`HistoryLoader::poll`] applied.
#[derive(Debug, Default)]
pub struct LoaderPoll {
    /// Batches appended to the browser (in-memory initial load only;
    /// refresh batches are buffered).
    pub batches: usize,
    /// The load ended with this event.
    pub event: Option<LoadEvent>,
    /// The stream has ended and the corpus is being prepared on the
    /// `corpus-prepare` thread; a later poll (the waker fires when it is
    /// done) installs it and returns the event.
    pub preparing: bool,
    /// The drain budget ran out with a batch still waiting: poll again (the
    /// waker, if any, has been called for it).
    pub pending: bool,
    /// Store mode: a generation was installed (shown in full or swapped
    /// in), with or without an event.
    pub swapped: bool,
}

impl LoaderPoll {
    /// Whether the browser may have changed.
    pub fn changed(&self) -> bool {
        self.batches > 0 || self.event.is_some() || self.swapped
    }
}

enum LoadKind {
    Initial {
        project_errors: usize,
    },
    Refresh {
        buffer: Vec<Conversation>,
        project_errors: usize,
        policy: RefreshPolicy,
    },
}

/// The event an installed preparation reports.
enum Ending {
    Loaded { project_errors: usize },
    LoadFailed(AppError),
    Refreshed { project_errors: usize },
}

struct Streaming {
    rx: Receiver<LoaderMessage>,
    kind: LoadKind,
    /// A batch received after the drain budget ran out, applied first by
    /// the next poll.
    held: Option<LoaderMessage>,
}

struct Preparing {
    rx: Receiver<PreparedCorpus>,
    ending: Ending,
}

enum Phase {
    Streaming(Streaming),
    Preparing(Preparing),
}

struct ActiveLoad {
    phase: Phase,
    time_filter_active: bool,
    /// [`corpus_edit_mark`] when the load started: rows the browser
    /// deletes or renames from then on are replayed on its corpus.
    edit_mark: u64,
}

/// What the `store-reconcile` thread produced.
struct PassResult {
    /// The store the pass reconciled.
    root: PathBuf,
    /// The last generation the pass published (`Unchanged` when it wrote
    /// nothing).
    result: std::io::Result<Publish>,
    /// `CURRENT` when the pass ended.
    current: Option<String>,
}

/// Set when the loader is dropped: a running reconcile pass gives up, and a
/// pass waiting to retry a locked store wakes at once. Also ends a pass's
/// `store-watch` thread.
#[derive(Default)]
pub(super) struct Cancel {
    set: AtomicBool,
    wait: Mutex<()>,
    wake: Condvar,
}

impl Cancel {
    pub(super) fn is_set(&self) -> bool {
        self.set.load(Ordering::Relaxed)
    }

    pub(super) fn set(&self) {
        self.set.store(true, Ordering::Relaxed);
        // Taken so that a waiter between its check and its wait gets the
        // notice.
        let _guard = self.wait.lock().unwrap_or_else(PoisonError::into_inner);
        self.wake.notify_all();
    }

    /// Waits for `timeout` unless cancelled meanwhile; whether it was.
    pub(super) fn sleep(&self, timeout: Duration) -> bool {
        let guard = self.wait.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = self
            .wake
            .wait_timeout_while(guard, timeout, |_| !self.is_set())
            .unwrap_or_else(PoisonError::into_inner);
        self.is_set()
    }
}

/// A thread of the loader whose result a poll collects. The thread sends
/// the result before it calls the waker, so the poll the waker schedules
/// finds it even while the thread is still finishing.
struct Background<T> {
    result: Receiver<T>,
    thread: JoinHandle<()>,
}

impl<T: Send + 'static> Background<T> {
    /// Runs `work` on a named thread (8 MiB stack); `None` when the thread
    /// cannot be started.
    fn spawn(
        name: &str,
        waker: Option<Waker>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Option<Self> {
        let (tx, result) = mpsc::channel();
        std::thread::Builder::new()
            .name(name.into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let _ = tx.send(work());
                if let Some(waker) = waker {
                    waker();
                }
            })
            .ok()
            .map(|thread| Self { result, thread })
    }

    /// Waits for the thread and returns its result (`None` if it panicked).
    fn join(self) -> Option<T> {
        let _ = self.thread.join();
        self.result.try_recv().ok()
    }
}

/// The result of `task` once its thread has sent it (or `None` for a
/// thread that panicked), then joins the thread, which only calls the waker
/// after sending. Never waits for the work itself.
fn take_finished<T>(task: &mut Option<Background<T>>) -> Option<Option<T>> {
    let received = match task.as_ref()?.result.try_recv() {
        Ok(value) => Some(value),
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => None,
    };
    if let Some(task) = task.take() {
        let _ = task.thread.join();
    }
    Some(received)
}

/// A generation name (`gNNN`) as a number, to tell which is newer.
fn generation_number(name: &str) -> u64 {
    name.strip_prefix('g')
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// The store the list is read from in store mode (upstream runtime
/// `Store`): generations under `root`, kept up to date by reconcile passes.
struct StoreRun {
    root: PathBuf,
    /// The newest generation `CURRENT` named, as seen by a pass's watcher,
    /// at a pass's end or when the store was opened.
    latest: Arc<Mutex<Option<String>>>,
    /// What `PROGRESS` said at the running pass's watcher's last check: the
    /// progress of the pass indexing the store, by this process or another
    /// (upstream 04c714b). `None` between passes.
    progress: Arc<Mutex<Option<Indexing>>>,
    /// The generation an open was last started for (not started again
    /// until `CURRENT` or the preview mode changes).
    attempted: Option<(String, bool)>,
    /// Cards in the generation shown (a refresh reports the change).
    shown_len: usize,
    /// The `store-reconcile` thread of the running pass.
    pass: Option<Background<PassResult>>,
    /// Another pass was asked for while one was running.
    again: bool,
    /// The `generation-open` thread, and the store it opens a generation of.
    opening: Option<Background<Option<GenerationOpen>>>,
    opening_root: PathBuf,
    /// The initial load has not been reported yet.
    initial: bool,
    /// Generation size before a refresh, while one runs.
    refreshing: Option<usize>,
    /// The error of the last pass, for the initial load or a refresh.
    failed: Option<std::io::Error>,
    /// The outcome of the last pass a poll collected, for
    /// [`HistoryLoader::finish_snapshot_write`].
    written: Option<Publish>,
}

impl StoreRun {
    fn new(root: PathBuf) -> Self {
        let latest = Generation::current_name(&root);
        Self {
            root,
            latest: Arc::new(Mutex::new(latest)),
            progress: Arc::default(),
            attempted: None,
            shown_len: 0,
            pass: None,
            again: false,
            opening: None,
            opening_root: PathBuf::new(),
            initial: false,
            refreshing: None,
            failed: None,
            written: None,
        }
    }

    fn latest(&self) -> Option<String> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records a newer `CURRENT` (never an older one).
    fn saw(&self, name: &str) {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        if latest
            .as_deref()
            .is_none_or(|seen| generation_number(seen) < generation_number(name))
        {
            *latest = Some(name.to_owned());
        }
    }

    /// Takes in what a finished pass left. Returns whether it failed.
    fn collect(&mut self, done: Option<PassResult>) -> bool {
        match done {
            Some(PassResult {
                root,
                result,
                current,
            }) => {
                // A pass over the store of a previous workspace says
                // nothing about this one's generations.
                if let Some(current) = current.filter(|_| root == self.root) {
                    self.saw(&current);
                }
                match result {
                    Ok(publish) => {
                        self.written = Some(publish);
                        self.failed = None;
                        false
                    }
                    Err(error) => {
                        self.failed = Some(error);
                        true
                    }
                }
            }
            None => {
                self.failed = Some(std::io::Error::other("the reconcile pass stopped"));
                true
            }
        }
    }

    /// The progress of the pass indexing the store while one may run
    /// (upstream `Store::progress`).
    fn indexing(&self, pending: bool) -> Option<Indexing> {
        if !pending {
            return None;
        }
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// No pass runs or is due and no generation is being opened.
    fn settled(&self) -> bool {
        self.pass.is_none() && !self.again && self.opening.is_none()
    }
}

/// Fills a [`Browser`] from the store's generations (store mode) or with
/// the streaming history loader (in memory); see the module documentation.
///
/// In memory:
///
/// * [`HistoryLoader::start_initial_load`]: batches are appended to the
///   browser as they arrive (create it with [`Browser::new_loading`]); the
///   end of the stream finishes loading.
/// * [`HistoryLoader::start_refresh`]: a full reload into a hidden buffer;
///   the list stays interactive and is replaced atomically at the end, or
///   kept when any project failed ([`HistoryLoader::start_refresh_with`]
///   chooses).
///
/// At the end of either, the corpus is prepared on a `corpus-prepare`
/// thread ([`LoaderPoll::preparing`]). Rows deleted or renamed through the
/// browser after the load started (while the files were read, buffered or
/// prepared) are replayed on the installed corpus; the browser keeps the
/// last few edits made before a load's first poll for this. Only one load
/// runs at a time (including its preparation). Call [`HistoryLoader::poll`]
/// when the waker fires (or periodically, upstream: every 50 ms while
/// loading).
pub struct HistoryLoader {
    env: Arc<HistoryEnv>,
    options: LoaderOptions,
    active: Option<ActiveLoad>,
    /// Store mode.
    store: Option<StoreRun>,
    /// Set when the loader is dropped: a pass gives up.
    cancel: Arc<Cancel>,
    /// Tests: the next preparation waits for a message on this channel.
    #[cfg(test)]
    prepare_gate: Option<Receiver<()>>,
    /// Tests: the next reconcile pass waits for a message on this channel
    /// before it starts.
    #[cfg(test)]
    write_gate: Option<Receiver<()>>,
}

impl HistoryLoader {
    pub fn new(env: Arc<HistoryEnv>, options: LoaderOptions) -> Self {
        Self {
            env,
            options,
            active: None,
            store: None,
            cancel: Arc::new(Cancel::default()),
            #[cfg(test)]
            prepare_gate: None,
            #[cfg(test)]
            write_gate: None,
        }
    }

    pub fn options(&self) -> &LoaderOptions {
        &self.options
    }

    /// Whether the list comes from the store's generations (store mode).
    pub fn uses_store(&self) -> bool {
        self.store.is_some()
    }

    /// Use `env` and `options` for the next loads (a new workspace, debug
    /// level or preview mode). A running reconcile pass goes on and is
    /// still joined on drop, so replacing the settings never waits for it.
    /// Call it while no in-memory load runs; a running load keeps its
    /// stream. In store mode a workspace whose Pi/OMP session roots differ
    /// moves to their store root: its generation is swapped in once open
    /// (the rows shown stay until then), and the next pass reconciles it.
    pub fn reconfigure(&mut self, env: Arc<HistoryEnv>, options: LoaderOptions) {
        self.env = env;
        self.options = options;
        let root = self.store_root();
        if let (Some(store), Some(root)) = (&mut self.store, root)
            && store.root != root
        {
            // A new record of `CURRENT`: a pass still running over the
            // previous store keeps updating its own.
            store.latest = Arc::new(Mutex::new(Generation::current_name(&root)));
            store.root = root;
            store.attempted = None;
        }
        if self.store.is_some() {
            // The next poll shows the cards of these settings (a reverted
            // workspace too, with nothing else to poll for).
            self.wake();
        }
    }

    /// The store root of store mode, when the options ask for it.
    fn store_root(&self) -> Option<PathBuf> {
        if !self.options.snapshot {
            return None;
        }
        crate::store::default_root(&self.env)
    }

    /// Enters store mode when the options ask for it and there is a store
    /// root. Returns whether the loader is in store mode.
    fn enter_store(&mut self) -> bool {
        if self.store.is_none()
            && let Some(root) = self.store_root()
        {
            self.store = Some(StoreRun::new(root));
        }
        self.store.is_some()
    }

    fn wake(&self) {
        if let Some(waker) = &self.options.waker {
            waker();
        }
    }

    /// The store's current generation, for the first frame
    /// ([`Browser::show_snapshot`], upstream `Store::open`), read with the
    /// options' preview mode and time filter. All its cards and its index
    /// are opened on a `generation-open` thread, and a later
    /// [`HistoryLoader::poll`] installs them ([`Browser::install_generation`];
    /// upstream `App::open_search_index`, on the TUI thread). `None` unless
    /// [`LoaderOptions::snapshot`] is on, or without a usable generation
    /// (first run, corruption, version mismatch): the rows then wait for the
    /// first pass to publish one.
    pub fn open_snapshot(&mut self) -> Option<Snapshot> {
        if !self.enter_store() {
            return None;
        }
        let store = self.store.as_mut()?;
        let snapshot = Snapshot::open(
            &store.root,
            self.options.show_last,
            self.options.time_filter,
        )?;
        store.saw(snapshot.generation_name());
        if store.opening.is_none() {
            let wanted = (
                snapshot.generation_name().to_owned(),
                self.options.show_last,
            );
            let cards = snapshot.clone();
            store.opening =
                Background::spawn("generation-open", self.options.waker.clone(), move || {
                    let open = GenerationOpen::build(cards, None, None);
                    crate::bench::mark("index_open");
                    Some(open)
                });
            store.attempted = Some(wanted);
            store.opening_root = store.root.clone();
        }
        Some(snapshot)
    }

    /// Waits for the running reconcile pass, and the ones asked for after
    /// it, so they are not cut short (upstream `SnapshotStore::finish`), and
    /// returns the last generation the last pass published (`Unchanged`
    /// when it wrote nothing), also when a poll already collected it (once).
    /// A failed pass (logged to the debug log by its thread) returns `None`,
    /// as does no pass. The generation it left is shown by the next poll.
    pub fn finish_snapshot_write(&mut self) -> Option<Publish> {
        loop {
            let store = self.store.as_mut()?;
            if let Some(pass) = store.pass.take() {
                let done = pass.join();
                // A failure is reported by the next poll.
                store.collect(done);
            }
            if std::mem::take(&mut store.again) {
                self.start_pass();
                continue;
            }
            break;
        }
        self.store.as_mut()?.written.take()
    }

    /// Starts a reconcile pass, or another one after the running one
    /// (upstream `Store::reconcile`).
    fn start_pass(&mut self) {
        let Some(store) = &mut self.store else {
            return;
        };
        if store.pass.is_some() {
            store.again = true;
            return;
        }
        let job = ReconcileJob {
            root: store.root.clone(),
            env: self.env.clone(),
            cancel: self.cancel.clone(),
            log_path: self.env.debug_log_file(),
            latest: store.latest.clone(),
            progress: store.progress.clone(),
            waker: self.options.waker.clone(),
        };
        #[cfg(test)]
        let gate = self.write_gate.take();
        store.pass = Background::spawn("store-reconcile", self.options.waker.clone(), move || {
            #[cfg(test)]
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            reconcile_store(job)
        });
        if store.pass.is_none() {
            store.failed = Some(std::io::Error::other("cannot start a reconcile pass"));
        }
    }

    /// The time filter of the next load (and of the cards shown in store
    /// mode).
    pub fn time_filter(&self) -> &TimeFilter {
        &self.options.time_filter
    }

    /// Change the time filter used by the next initial load or refresh. In
    /// store mode the next poll lists the cards shown under it
    /// ([`Browser::set_time_filter`]); the waker schedules that poll (a
    /// filter reverted after a failed reload has nothing else to poll for).
    pub fn set_time_filter(&mut self, time_filter: TimeFilter) {
        let changed = self.options.time_filter != time_filter;
        self.options.time_filter = time_filter;
        if changed && self.store.is_some() {
            self.wake();
        }
    }

    /// Whether a load the list waits for runs: an in-memory load (or its
    /// corpus preparation), or in store mode the initial load until it is
    /// reported, or a refresh. A pass asked for by an edit does not count.
    pub fn is_running(&self) -> bool {
        match &self.store {
            Some(store) => store.initial || store.refreshing.is_some(),
            None => self.active.is_some(),
        }
    }

    /// Whether a refresh is running (streaming or preparing; store mode:
    /// until its pass and the swap it led to are done).
    pub fn is_refreshing(&self) -> bool {
        if let Some(store) = &self.store {
            return store.refreshing.is_some();
        }
        match &self.active {
            Some(ActiveLoad {
                phase: Phase::Streaming(streaming),
                ..
            }) => matches!(streaming.kind, LoadKind::Refresh { .. }),
            Some(ActiveLoad {
                phase: Phase::Preparing(preparing),
                ..
            }) => matches!(preparing.ending, Ending::Refreshed { .. }),
            None => false,
        }
    }

    /// Whether the stream has ended and the corpus is being prepared (store
    /// mode: a generation is being opened).
    pub fn is_preparing(&self) -> bool {
        if let Some(store) = &self.store {
            return store.opening.is_some();
        }
        matches!(
            self.active,
            Some(ActiveLoad {
                phase: Phase::Preparing(_),
                ..
            })
        )
    }

    /// Start the initial load. Returns false (and does nothing) while
    /// another load is running. In store mode it starts a reconcile pass
    /// (upstream started its `--reconcile` child at launch); the generation
    /// [`HistoryLoader::open_snapshot`] opened, or the first one a pass
    /// publishes, is shown by a later poll.
    pub fn start_initial_load(&mut self) -> bool {
        if self.active.is_some() {
            return false;
        }
        if self.enter_store() {
            if let Some(store) = &mut self.store {
                if store.initial {
                    return false;
                }
                store.initial = true;
            }
            self.start_pass();
            self.wake();
            return true;
        }
        let mark = corpus_edit_mark();
        let rx = self.spawn_loader();
        self.begin(rx, LoadKind::Initial { project_errors: 0 }, mark);
        true
    }

    /// Start a refresh (upstream Ctrl+L) that keeps the current sessions
    /// when a project fails ([`RefreshPolicy::KeepOnProjectError`]).
    /// Returns false (and does nothing) while another load is running, as
    /// upstream. Show [`REFRESHING_STATUS`] when it returns true.
    pub fn start_refresh(&mut self) -> bool {
        self.start_refresh_with(RefreshPolicy::KeepOnProjectError)
    }

    /// Start a refresh with an explicit project-error policy. Returns false
    /// (and does nothing) while another load is running. In store mode it
    /// runs a reconcile pass (after the running one) and
    /// [`LoadEvent::Refreshed`] reports the generation sizes once the
    /// generation it left is shown.
    pub fn start_refresh_with(&mut self, policy: RefreshPolicy) -> bool {
        if self.active.is_some() {
            return false;
        }
        if let Some(store) = &mut self.store {
            if store.initial || store.refreshing.is_some() {
                return false;
            }
            store.refreshing = Some(store.shown_len);
            self.start_pass();
            self.wake();
            return true;
        }
        let mark = corpus_edit_mark();
        let rx = self.spawn_loader();
        self.begin(
            rx,
            LoadKind::Refresh {
                buffer: Vec::new(),
                project_errors: 0,
                policy,
            },
            mark,
        );
        true
    }

    fn begin(&mut self, rx: Receiver<LoaderMessage>, kind: LoadKind, edit_mark: u64) {
        self.active = Some(ActiveLoad {
            phase: Phase::Streaming(Streaming {
                rx,
                kind,
                held: None,
            }),
            time_filter_active: self.options.time_filter.is_active(),
            edit_mark,
        });
    }

    fn spawn_loader(&self) -> Receiver<LoaderMessage> {
        let rx = crate::history::load_all_conversations_streaming(
            self.env.clone(),
            self.options.show_last,
            self.options.debug_level,
            self.options.time_filter,
        );
        match &self.options.waker {
            Some(waker) => forward_with_waker(rx, waker.clone()),
            None => rx,
        }
    }

    /// Apply what the loader's threads produced to `browser`. In memory,
    /// appending batches during the initial load stops once
    /// [`LoaderOptions::drain_budget`] is spent (after at least one batch)
    /// and [`LoaderPoll::pending`] is set. Never blocks, and never sorts,
    /// deduplicates or indexes the corpus, opens a generation or
    /// materializes its cards on this thread.
    pub fn poll(&mut self, browser: &mut Browser) -> LoaderPoll {
        if self.store.is_some() {
            return self.poll_store(browser);
        }
        let mut result = LoaderPoll::default();
        let Some(mut active) = self.active.take() else {
            return result;
        };
        browser.track_corpus_edits_since(active.edit_mark);
        if let Phase::Streaming(streaming) = &mut active.phase {
            match drain_stream(streaming, browser, &mut result, self.options.drain_budget) {
                StreamState::Running => {
                    self.active = Some(active);
                    return result;
                }
                StreamState::Yielded => {
                    // A batch is held back: ask for another poll, so the
                    // caller can draw a frame in between.
                    result.pending = true;
                    self.active = Some(active);
                    self.wake();
                    return result;
                }
                StreamState::Ended(StreamEnd::Event(event)) => {
                    // Nothing is installed: the current rows already
                    // reflect every edit.
                    browser.stop_tracking_corpus_edits();
                    result.event = Some(event);
                    return result;
                }
                StreamState::Ended(StreamEnd::Prepare(job, ending)) => {
                    #[cfg(test)]
                    let job = match self.prepare_gate.take() {
                        Some(gate) => Box::new(move || {
                            let _ = gate.recv();
                            job()
                        }) as PrepareJob,
                        None => job,
                    };
                    active.phase = Phase::Preparing(Preparing {
                        rx: spawn_preparation(job, self.options.waker.clone()),
                        ending,
                    });
                    // Installed by a later poll (the waker fires when the
                    // preparation is done), never by this one: a poll that
                    // appended batches always leaves the browser loading.
                    result.preparing = true;
                    self.active = Some(active);
                    return result;
                }
            }
        }
        let received = match &active.phase {
            Phase::Preparing(preparing) => preparing.rx.try_recv(),
            Phase::Streaming(_) => unreachable!("the stream phase returned above"),
        };
        match received {
            Ok(prepared) => {
                let Phase::Preparing(preparing) = active.phase else {
                    unreachable!();
                };
                result.event = Some(install(
                    browser,
                    prepared,
                    preparing.ending,
                    active.time_filter_active,
                ));
            }
            Err(TryRecvError::Empty) => {
                result.preparing = true;
                self.active = Some(active);
            }
            Err(TryRecvError::Disconnected) => {
                // The preparation thread died (a panic): end the load the
                // way upstream would have, without the prepared corpus.
                browser.stop_tracking_corpus_edits();
                let Phase::Preparing(preparing) = active.phase else {
                    unreachable!();
                };
                result.event = Some(match preparing.ending {
                    Ending::Refreshed { .. } => LoadEvent::RefreshInterrupted,
                    Ending::Loaded { project_errors } => {
                        browser.finish_loading();
                        loaded_event(browser, project_errors, active.time_filter_active)
                    }
                    Ending::LoadFailed(error) => {
                        browser.finish_loading();
                        LoadEvent::LoadFailed(error)
                    }
                });
            }
        }
        result
    }

    /// [`HistoryLoader::poll`] in store mode (upstream: the runtime's
    /// `Reconciled` handling and `Store::show_current`).
    fn poll_store(&mut self, browser: &mut Browser) -> LoaderPoll {
        let mut result = LoaderPoll::default();
        // The cards shown follow the time filter of the options.
        browser.set_time_filter(self.options.time_filter);
        // The browser's env follows the settings'.
        browser.set_env(self.env.clone());
        if browser.take_reconcile_request() {
            self.start_pass();
        }
        let Some(store) = &mut self.store else {
            return result;
        };
        if let Some(done) = take_finished(&mut store.pass) {
            if store.collect(done) {
                // Upstream `index_failed`: the indicator ends without a
                // completion notice; the failure goes to a load event or a
                // notice below.
                browser.end_index_progress();
            }
            if std::mem::take(&mut store.again) {
                self.start_pass();
            }
        }
        let Some(store) = &mut self.store else {
            return result;
        };
        if let Some(opened) = take_finished(&mut store.opening)
            && let Some(Some(open)) = opened
        {
            if store.opening_root != store.root {
                // Opened from the store of a previous workspace: the
                // current one's generation is opened instead.
                store.attempted = None;
            } else {
                let name = open.generation_name().to_owned();
                let show_last = open.show_last();
                store.saw(&name);
                browser.install_generation(open);
                store.attempted = Some((name, show_last));
                store.shown_len = browser.generation_len().unwrap_or(0);
                result.swapped = true;
            }
        }
        // A newer generation, or another preview mode: open it off this
        // thread.
        if store.opening.is_none()
            && let Some(name) = store.latest()
        {
            let wanted = (name, self.options.show_last);
            if store.attempted.as_ref() != Some(&wanted) {
                store.attempted = Some(wanted);
                let root = store.root.clone();
                store.opening_root = root.clone();
                let (show_last, time) = (self.options.show_last, self.options.time_filter);
                let shown = browser.corpus_snapshot();
                let semantic = browser.semantic_conversations_loaded();
                store.opening =
                    Background::spawn("generation-open", self.options.waker.clone(), move || {
                        let snapshot = Snapshot::open(&root, show_last, time)?;
                        Some(GenerationOpen::build(snapshot, Some(shown), semantic))
                    });
            }
        }
        // Upstream `Store::show_progress`.
        let pending = store.pass.is_some() || store.again;
        browser.set_reconciling(pending);
        if !pending && store.failed.is_some() {
            // A failed pass (however it was collected) ends the indicator
            // without a completion notice.
            browser.end_index_progress();
        }
        browser.set_index_progress(store.indexing(pending), pending);
        result.preparing = store.opening.is_some();
        result.event = self.store_event(browser);
        // A failed pass that no load event reports (upstream `index_failed`;
        // its TUI showed every failure in the status line).
        if let Some(store) = &mut self.store
            && !store.initial
            && store.refreshing.is_none()
            && let Some(error) = store.failed.take()
        {
            browser.index_failed(&failure_message(&error));
        }
        result
    }

    /// The event the state of store mode reports, once: the initial load
    /// when the first generation is shown in full (or the first pass left
    /// none), a refresh when its pass and the swap it led to are done.
    fn store_event(&mut self, browser: &mut Browser) -> Option<LoadEvent> {
        let time_filter_active = self.options.time_filter.is_active();
        let store = self.store.as_mut()?;
        let settled = store.settled();
        if store.initial {
            if browser.generation_open() || (settled && browser.shows_snapshot()) {
                let empty = browser.generation_is_empty();
                if empty && !settled {
                    // The first pass may still publish what the list shows.
                    return None;
                }
                store.initial = false;
                return Some(if !empty {
                    LoadEvent::Loaded {
                        conversations: browser.generation_len().unwrap_or(0),
                        project_errors: 0,
                    }
                } else if let Some(error) = store.failed.take() {
                    indexing_failed(&error)
                } else {
                    no_history(time_filter_active)
                });
            }
            if !settled {
                return None;
            }
            // The first pass left no generation: an empty, ready list.
            store.initial = false;
            browser.finish_loading();
            return Some(match store.failed.take() {
                Some(error) => indexing_failed(&error),
                None => no_history(time_filter_active),
            });
        }
        if store.refreshing.is_some() && settled {
            let old = store.refreshing.take().unwrap_or_default();
            return Some(match store.failed.take() {
                Some(error) if browser.generation_is_empty() => {
                    // Nothing listed (a retry of the first launch's error):
                    // the same text as the initial load's failure.
                    LoadEvent::RefreshFailed(AppError::Io(indexing_failed_error(&error)))
                }
                Some(error) => LoadEvent::RefreshFailed(AppError::Io(error)),
                None => LoadEvent::Refreshed {
                    old,
                    new: browser.generation_len().unwrap_or(0),
                    project_errors: 0,
                },
            });
        }
        None
    }
}

/// The first line of a failed pass's error (upstream: the message its
/// `--reconcile` child left in `PROGRESS`).
fn failure_message(error: &std::io::Error) -> String {
    error
        .to_string()
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The initial load's first pass failed and no card is listed (upstream:
/// the TUI exited with this error instead of "no history").
fn indexing_failed(error: &std::io::Error) -> LoadEvent {
    LoadEvent::LoadFailed(AppError::Io(indexing_failed_error(error)))
}

/// The error text of a failed pass with no card listed.
fn indexing_failed_error(error: &std::io::Error) -> std::io::Error {
    std::io::Error::other(format!(
        "indexing sessions failed: {}",
        failure_message(error)
    ))
}

/// `NoHistoryFound` for the scope (and time range) shown.
fn no_history(time_filter_active: bool) -> LoadEvent {
    let scope = if time_filter_active {
        "selected scope within the requested time range"
    } else {
        "selected scope"
    };
    LoadEvent::NoHistory(AppError::NoHistoryFound(scope.to_string()))
}

/// A corpus preparation to run on the `corpus-prepare` thread.
type PrepareJob = Box<dyn FnOnce() -> PreparedCorpus + Send>;

enum StreamEnd {
    Event(LoadEvent),
    Prepare(PrepareJob, Ending),
}

enum StreamState {
    /// Every queued message was applied; the stream is still open.
    Running,
    /// The drain budget ran out with a batch held back.
    Yielded,
    Ended(StreamEnd),
}

/// Apply the pending messages of a running stream. Appending batches to the
/// browser stops once `budget` is spent (after at least one batch).
fn drain_stream(
    streaming: &mut Streaming,
    browser: &mut Browser,
    result: &mut LoaderPoll,
    budget: Option<Duration>,
) -> StreamState {
    let started = Instant::now();
    loop {
        let received = match streaming.held.take() {
            Some(message) => Ok(message),
            None => streaming.rx.try_recv(),
        };
        match received {
            Ok(LoaderMessage::Batch(conversations)) => match &mut streaming.kind {
                LoadKind::Initial { .. } => {
                    if result.batches > 0
                        && budget.is_some_and(|budget| started.elapsed() >= budget)
                    {
                        streaming.held = Some(LoaderMessage::Batch(conversations));
                        return StreamState::Yielded;
                    }
                    browser.append_conversations(conversations);
                    result.batches += 1;
                }
                LoadKind::Refresh { buffer, .. } => buffer.extend(conversations),
            },
            Ok(LoaderMessage::ProjectError) => match &mut streaming.kind {
                LoadKind::Initial { project_errors } | LoadKind::Refresh { project_errors, .. } => {
                    *project_errors += 1
                }
            },
            Ok(LoaderMessage::Fatal(error)) => {
                return StreamState::Ended(match &mut streaming.kind {
                    // The load is over: leave the browser ready (with
                    // whatever arrived) instead of "Loading... 0", so a
                    // later refresh retries normally.
                    LoadKind::Initial { .. } => {
                        StreamEnd::Prepare(loading_rows_job(browser), Ending::LoadFailed(error))
                    }
                    LoadKind::Refresh { .. } => StreamEnd::Event(LoadEvent::RefreshFailed(error)),
                });
            }
            Ok(LoaderMessage::Done) => {
                return StreamState::Ended(stream_done(streaming, browser));
            }
            Err(TryRecvError::Empty) => return StreamState::Running,
            Err(TryRecvError::Disconnected) => {
                // Upstream treats a disconnected initial loader as done.
                let initial = matches!(streaming.kind, LoadKind::Initial { .. });
                return StreamState::Ended(if initial {
                    stream_done(streaming, browser)
                } else {
                    StreamEnd::Event(LoadEvent::RefreshInterrupted)
                });
            }
        }
    }
}

/// The stream finished.
fn stream_done(streaming: &mut Streaming, browser: &Browser) -> StreamEnd {
    match &mut streaming.kind {
        LoadKind::Initial { project_errors } => StreamEnd::Prepare(
            loading_rows_job(browser),
            Ending::Loaded {
                project_errors: *project_errors,
            },
        ),
        LoadKind::Refresh {
            buffer,
            project_errors,
            policy,
        } => {
            if *project_errors > 0 && *policy == RefreshPolicy::KeepOnProjectError {
                StreamEnd::Event(LoadEvent::RefreshIncomplete)
            } else {
                let buffer = std::mem::take(buffer);
                // The scope and where each current row moves are prepared
                // too, so the install does no per-row work (CCR-1).
                let context = browser.prepare_context(!browser.is_loading());
                StreamEnd::Prepare(
                    // Refresh dedupes by canonical path; the initial load
                    // does not (addendum C2).
                    Box::new(move || {
                        Browser::prepare_shared(
                            Arc::new(buffer.into_iter().map(Arc::new).collect()),
                            true,
                            Some(context),
                        )
                    }),
                    Ending::Refreshed {
                        project_errors: *project_errors,
                    },
                )
            }
        }
    }
}

/// Prepare the loading rows of the browser (they stay visible meanwhile),
/// with the rows in scope under its current settings.
fn loading_rows_job(browser: &Browser) -> PrepareJob {
    let snapshot = browser.loading_corpus();
    let context = browser.prepare_context(false);
    Box::new(move || Browser::prepare_shared(snapshot, false, Some(context)))
}

fn spawn_preparation(job: PrepareJob, waker: Option<Waker>) -> Receiver<PreparedCorpus> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("corpus-prepare".into())
        .spawn(move || {
            let prepared = job();
            let _ = tx.send(prepared);
            if let Some(waker) = waker {
                waker();
            }
        })
        .expect("failed to spawn corpus preparation thread");
    rx
}

fn install(
    browser: &mut Browser,
    prepared: PreparedCorpus,
    ending: Ending,
    time_filter_active: bool,
) -> LoadEvent {
    match ending {
        Ending::Loaded { project_errors } => {
            browser.install_prepared(prepared);
            loaded_event(browser, project_errors, time_filter_active)
        }
        Ending::LoadFailed(error) => {
            browser.install_prepared(prepared);
            LoadEvent::LoadFailed(error)
        }
        Ending::Refreshed { project_errors } => {
            let old = browser.conversations().len();
            browser.install_prepared(prepared);
            let new = browser.conversations().len();
            LoadEvent::Refreshed {
                old,
                new,
                project_errors,
            }
        }
    }
}

fn loaded_event(browser: &Browser, project_errors: usize, time_filter_active: bool) -> LoadEvent {
    if browser.conversations().is_empty() {
        no_history(time_filter_active)
    } else {
        LoadEvent::Loaded {
            conversations: browser.conversations().len(),
            project_errors,
        }
    }
}

impl Drop for HistoryLoader {
    /// Gives up a reconcile pass that is still running (or waiting to retry
    /// a locked store) and waits for its thread, which then returns soon,
    /// and for a generation being opened. Upstream's TUI did not wait at
    /// exit: its pass was a detached process that went on afterwards. An
    /// in-process cold pass takes many seconds, which quitting must not wait
    /// for.
    fn drop(&mut self) {
        self.cancel.set();
        if let Some(store) = &mut self.store {
            if let Some(pass) = store.pass.take() {
                pass.join();
            }
            if let Some(opening) = store.opening.take() {
                opening.join();
            }
        }
    }
}

/// What the `store-reconcile` thread works on.
struct ReconcileJob {
    root: PathBuf,
    /// The sources are resolved on the thread (that reads Pi's settings).
    env: Arc<HistoryEnv>,
    /// Set when the loader is dropped: the pass is given up.
    cancel: Arc<Cancel>,
    log_path: Option<PathBuf>,
    /// The newest `CURRENT` the watcher saw, shared with the loader.
    latest: Arc<Mutex<Option<String>>>,
    /// What `PROGRESS` said at the watcher's last check, shared with the
    /// loader.
    progress: Arc<Mutex<Option<Indexing>>>,
    /// Called when the watcher sees a newer `CURRENT` or new progress.
    waker: Option<Waker>,
}

/// The `store-reconcile` thread's work: a reconcile pass over the env's
/// sources into `root`, retried every [`BUSY_RETRY`] while another process
/// holds the store's lock. Its parallel parts (the scan, parsing, zstd
/// frames, the suffix index and merges) run on a pool of their own, so it
/// leaves cores to the UI and the search workers. Meanwhile a `store-watch`
/// thread reads `CURRENT` every [`GENERATION_POLL`] and wakes the loader's
/// poll when it names a newer generation. Each pass's outcome goes to the
/// debug log with upstream's line (`Reconcile: {outcome:?}`; of a store
/// found locked again and again, only the first); a pass given up (the
/// loader was dropped) is not logged.
fn reconcile_store(job: ReconcileJob) -> PassResult {
    let ReconcileJob {
        root,
        env,
        cancel,
        log_path,
        latest,
        progress,
        waker,
    } = job;
    let cancelled = || cancel.is_set();
    let log = |message: &str| {
        if let Some(log_path) = &log_path {
            let _ = crate::debug_log::log_debug(log_path, message);
        }
    };
    let sources = Registry::from_env(&env);
    let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("store-reconcile-{i}"))
        .build()
        .map_err(std::io::Error::other);
    let done = Cancel::default();
    let result = std::thread::scope(|scope| {
        let watching = std::thread::Builder::new()
            .name("store-watch".into())
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, || {
                watch_current(&root, &latest, &progress, waker.as_ref(), &done, &cancel)
            });
        let mut busy_logged = false;
        let result = pool.and_then(|pool| {
            loop {
                let pass = pool.install(|| {
                    reconcile::reconcile_cancellable(
                        &root,
                        &sources,
                        MergePolicy::DEFAULT,
                        &cancelled,
                    )
                });
                let busy = matches!(
                    &pass,
                    Ok(Outcome {
                        publish: Publish::Busy,
                        ..
                    })
                );
                if let Ok(outcome) = &pass
                    && !(busy && busy_logged)
                {
                    log(&format!("Reconcile: {outcome:?}"));
                }
                if !busy {
                    break pass.map(|outcome| outcome.publish);
                }
                busy_logged = true;
                if cancel.sleep(BUSY_RETRY) {
                    break Err(crate::store::generation::cancelled_build());
                }
            }
        });
        done.set();
        drop(watching);
        result
    });
    crate::bench::mark("reconcile_done");
    let given_up =
        cancelled() && matches!(&result, Err(err) if err.kind() == std::io::ErrorKind::Interrupted);
    if let Err(err) = &result
        && !given_up
    {
        log(&format!("Failed to build list snapshot: {err}"));
        // Upstream's `--reconcile` entry point left the message for the TUI
        // that started it; here the loader takes it from the error.
        progress::record_failure(&root, &err.to_string());
    }
    PassResult {
        current: Generation::current_name(&root),
        root,
        result,
    }
}

/// The `store-watch` thread: until `done` (or `cancel`), checks `CURRENT`
/// and `PROGRESS` every [`GENERATION_POLL`], records a newer generation in
/// `latest` and the progress of the pass indexing the store (by this process
/// or another; upstream read it whenever it polled `CURRENT`) in `progress`,
/// and calls the waker when either changed. Clears `progress` when it ends.
fn watch_current(
    root: &Path,
    latest: &Mutex<Option<String>>,
    progress: &Mutex<Option<Indexing>>,
    waker: Option<&Waker>,
    done: &Cancel,
    cancel: &Cancel,
) {
    while !done.sleep(GENERATION_POLL) && !cancel.is_set() {
        let indexing = match progress::read(root) {
            Some(progress::Status::Indexing(indexing)) => Some(indexing),
            Some(progress::Status::Failed(_)) | None => None,
        };
        let mut changed = {
            let mut shown = progress.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::replace(&mut *shown, indexing) != indexing
        };
        if let Some(current) = Generation::current_name(root) {
            let mut seen = latest.lock().unwrap_or_else(PoisonError::into_inner);
            if seen
                .as_deref()
                .is_none_or(|seen| generation_number(seen) < generation_number(&current))
            {
                *seen = Some(current);
                changed = true;
            }
        }
        if changed && let Some(waker) = waker {
            waker();
        }
    }
    // The pass is over: what it reported no longer applies.
    *progress.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Forward loader messages through a new channel, calling `waker` after
/// each one (the history loader itself has no waker).
fn forward_with_waker(rx: Receiver<LoaderMessage>, waker: Waker) -> Receiver<LoaderMessage> {
    let (tx, forwarded) = mpsc::channel();
    std::thread::Builder::new()
        .name("history-loader-forwarder".into())
        .spawn(move || {
            for message in rx {
                if tx.send(message).is_err() {
                    break;
                }
                waker();
            }
            // Wake once more so the receiver sees the disconnect.
            drop(tx);
            waker();
        })
        .expect("failed to spawn history loader forwarder thread");
    forwarded
}

#[cfg(test)]
impl HistoryLoader {
    /// Hold the next corpus preparation until a message is sent on the
    /// returned channel (or it is dropped).
    pub(super) fn gate_next_preparation_for_test(&mut self) -> mpsc::Sender<()> {
        let (tx, rx) = mpsc::channel();
        self.prepare_gate = Some(rx);
        tx
    }

    /// Hold the next reconcile pass until a message is sent on the returned
    /// channel (or it is dropped); also returns the flag the loader sets
    /// when it is dropped.
    pub(super) fn gate_next_snapshot_write_for_test(&mut self) -> (mpsc::Sender<()>, Arc<Cancel>) {
        let (tx, rx) = mpsc::channel();
        self.write_gate = Some(rx);
        (tx, self.cancel.clone())
    }

    /// Drive a hand-made message stream (tests).
    pub(super) fn start_with_receiver_for_test(
        &mut self,
        rx: Receiver<LoaderMessage>,
        refresh: bool,
    ) {
        self.start_with_receiver_and_policy_for_test(rx, refresh, RefreshPolicy::default());
    }

    /// Drive a hand-made message stream with a refresh policy (tests).
    pub(super) fn start_with_receiver_and_policy_for_test(
        &mut self,
        rx: Receiver<LoaderMessage>,
        refresh: bool,
        policy: RefreshPolicy,
    ) {
        let kind = if refresh {
            LoadKind::Refresh {
                buffer: Vec::new(),
                project_errors: 0,
                policy,
            }
        } else {
            LoadKind::Initial { project_errors: 0 }
        };
        self.begin(rx, kind, corpus_edit_mark());
    }
}
