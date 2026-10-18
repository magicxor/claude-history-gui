//! Drives [`crate::history::load_all_conversations_streaming`] into a
//! [`Browser`] (upstream `tui/runtime.rs:184-368`): the initial streaming
//! load and the buffered refresh (Ctrl+L), with upstream's status strings.
//!
//! [`HistoryLoader::poll`] never runs O(corpus) work on the caller's
//! thread: when a stream ends, the corpus is prepared
//! ([`Browser::prepare_corpus`]) on a `corpus-prepare` thread and a later
//! poll installs it and returns the [`LoadEvent`].

use super::corpus::corpus_edit_mark;
use super::{Browser, PreparedCorpus, Waker};
use crate::debug::DebugLevel;
use crate::env::HistoryEnv;
use crate::error::AppError;
use crate::history::{Conversation, LoaderMessage};
use crate::time_filter::TimeFilter;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Status shown when a refresh starts (upstream wording).
pub const REFRESHING_STATUS: &str = "Refreshing sessions...";

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
    /// Called from the loader after each message it posts, and when the
    /// corpus preparation at the end of a load finishes.
    pub waker: Option<Waker>,
}

impl Default for LoaderOptions {
    /// Last-message previews (upstream default), no debug output, no time
    /// filter, no waker.
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
}

impl std::fmt::Debug for LoaderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoaderOptions")
            .field("show_last", &self.show_last)
            .field("debug_level", &self.debug_level)
            .field("time_filter", &self.time_filter)
            .field("waker", &self.waker.is_some())
            .finish()
    }
}

/// What a refresh does when a project (or a Pi/OMP root) failed to load.
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
    /// The initial load finished and the browser is ready.
    /// `project_errors` counts projects (or Pi/OMP roots) that failed to
    /// load; upstream ignored them during the initial load.
    Loaded {
        conversations: usize,
        project_errors: usize,
    },
    /// The initial load found nothing:
    /// `NoHistoryFound("selected scope")`, or `"selected scope within the
    /// requested time range"` when a time filter is active. Upstream exited
    /// with this error; the browser is ready and empty.
    NoHistory(AppError),
    /// The initial load failed before producing any data (for example
    /// `ProjectsDirNotFound`). Upstream exited with this error; the browser
    /// is ready (and normally empty), so a refresh can retry the load.
    LoadFailed(AppError),
    /// A refresh replaced the corpus. `project_errors` counts projects
    /// that failed to load (only non-zero with
    /// [`RefreshPolicy::ApplyPartial`]).
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
    /// Batches appended to the browser (initial load only; refresh batches
    /// are buffered).
    pub batches: usize,
    /// The load ended with this event.
    pub event: Option<LoadEvent>,
    /// The stream has ended and the corpus is being prepared on the
    /// `corpus-prepare` thread; a later poll (the waker fires when it is
    /// done) installs it and returns the event.
    pub preparing: bool,
}

impl LoaderPoll {
    /// Whether the browser may have changed.
    pub fn changed(&self) -> bool {
        self.batches > 0 || self.event.is_some()
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

/// Runs the streaming history loader and applies its messages to a
/// [`Browser`].
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
    /// Tests: the next preparation waits for a message on this channel.
    #[cfg(test)]
    prepare_gate: Option<Receiver<()>>,
}

impl HistoryLoader {
    pub fn new(env: Arc<HistoryEnv>, options: LoaderOptions) -> Self {
        Self {
            env,
            options,
            active: None,
            #[cfg(test)]
            prepare_gate: None,
        }
    }

    pub fn options(&self) -> &LoaderOptions {
        &self.options
    }

    /// The time filter of the next load.
    pub fn time_filter(&self) -> &TimeFilter {
        &self.options.time_filter
    }

    /// Change the time filter used by the next initial load or refresh.
    pub fn set_time_filter(&mut self, time_filter: TimeFilter) {
        self.options.time_filter = time_filter;
    }

    /// Whether a load (or its corpus preparation) is running.
    pub fn is_running(&self) -> bool {
        self.active.is_some()
    }

    /// Whether a refresh is running (streaming or preparing).
    pub fn is_refreshing(&self) -> bool {
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

    /// Whether the stream has ended and the corpus is being prepared.
    pub fn is_preparing(&self) -> bool {
        matches!(
            self.active,
            Some(ActiveLoad {
                phase: Phase::Preparing(_),
                ..
            })
        )
    }

    /// Start the initial load. Returns false (and does nothing) while
    /// another load is running.
    pub fn start_initial_load(&mut self) -> bool {
        if self.active.is_some() {
            return false;
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
    /// (and does nothing) while another load is running.
    pub fn start_refresh_with(&mut self, policy: RefreshPolicy) -> bool {
        if self.active.is_some() {
            return false;
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
            phase: Phase::Streaming(Streaming { rx, kind }),
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

    /// Apply every pending loader message to `browser` (never blocks, and
    /// never sorts, deduplicates or indexes the corpus on this thread).
    pub fn poll(&mut self, browser: &mut Browser) -> LoaderPoll {
        let mut result = LoaderPoll::default();
        let Some(mut active) = self.active.take() else {
            return result;
        };
        browser.track_corpus_edits_since(active.edit_mark);
        if let Phase::Streaming(streaming) = &mut active.phase {
            match drain_stream(streaming, browser, &mut result) {
                StreamState::Running => {
                    self.active = Some(active);
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
}

/// A corpus preparation to run on the `corpus-prepare` thread.
type PrepareJob = Box<dyn FnOnce() -> PreparedCorpus + Send>;

enum StreamEnd {
    Event(LoadEvent),
    Prepare(PrepareJob, Ending),
}

enum StreamState {
    Running,
    Ended(StreamEnd),
}

/// Apply the pending messages of a running stream.
fn drain_stream(
    streaming: &mut Streaming,
    browser: &mut Browser,
    result: &mut LoaderPoll,
) -> StreamState {
    loop {
        match streaming.rx.try_recv() {
            Ok(LoaderMessage::Batch(conversations)) => match &mut streaming.kind {
                LoadKind::Initial { .. } => {
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
            Ok(LoaderMessage::Done) => return StreamState::Ended(stream_done(streaming, browser)),
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

/// The stream finished normally.
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
    let snapshot = browser.corpus_snapshot();
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
        let scope = if time_filter_active {
            "selected scope within the requested time range"
        } else {
            "selected scope"
        };
        LoadEvent::NoHistory(AppError::NoHistoryFound(scope.to_string()))
    } else {
        LoadEvent::Loaded {
            conversations: browser.conversations().len(),
            project_errors,
        }
    }
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
