//! The adapters of each source, and the registry of the instances the
//! environment configures.
//!
//! # Differences from upstream
//!
//! * **Roots from a [`HistoryEnv`].** [`Registry::from_env`] takes the env
//!   (the Claude projects root and the Pi/OMP session roots the history
//!   loader reads; the Pi root follows the workspace's `.pi/settings.json`;
//!   the Codex home and state database directory,
//!   [`HistoryEnv::codex_home`] and [`HistoryEnv::codex_sqlite_home`])
//!   instead of reading the process environment. Like upstream's, a registry
//!   is built for each pass, so it follows a workspace change.
//! * **The CLI's cache dir.** A registry built from an env also carries the
//!   installed CLI's cache root ([`Registry::cli_cache`],
//!   [`HistoryEnv::cli_cache_root`]): a reconcile pass never removes old
//!   caches from a cache dir inside it (`store::legacy`).
//!   [`Registry::with_roots`] leaves it unset.
//! * **Cancellable decoding.** The Claude, Pi and OMP adapters'
//!   [`Adapter::decode_cancellable`] and [`decode_path_cancellable`] read
//!   the session file through a reader that fails once the caller's flag is
//!   set ([`super::adapter::is_cancelled`]); `decode_file` is that decode
//!   with a flag that never is.
//! * **Files opened in an env.** [`decode_path_in`] (and the streaming
//!   `decode_path_into` given an env) decodes a rollout of the env's own
//!   Codex home through the env's adapter, with its `CODEX_SQLITE_HOME`
//!   (upstream's adapters read it from the process environment, see
//!   `codex::view`); [`decode_path`] and [`decode_path_cancellable`] have no
//!   env.

pub mod claude;
pub mod codex;
pub mod pi;

use super::adapter::{Adapter, FileId, Located, Stamp};
use super::model::{
    ConversationKey, Event, EventKind, SourceInstance, SourceKind, Transcript, Visibility,
};
use super::omp_loader::{self, OmpSessionRoot};
use super::pi_loader::{self, PiSessionRoot};
use crate::env::HistoryEnv;
use crate::error::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// The adapters of every configured history root, in a fixed order: Claude,
/// Pi, OMP, Codex.
#[derive(Clone, Default)]
pub struct Registry {
    adapters: Vec<Arc<dyn Adapter>>,
    /// Fork: the installed CLI's cache root, see [`Self::cli_cache`].
    cli_cache: Option<PathBuf>,
}

impl Registry {
    /// The roots `env` selects (`CLAUDE_CONFIG_DIR`, the Pi and OMP
    /// settings, `CODEX_HOME`), whether or not they exist (fork: from the
    /// [`HistoryEnv`], not the process environment), and the CLI's cache
    /// root.
    pub fn from_env(env: &HistoryEnv) -> Self {
        let mut registry = Self::with_roots(
            env.claude_projects_root().ok(),
            pi_loader::session_root(env).ok(),
            omp_loader::session_root(env).ok(),
        )
        .with_cli_cache(env.cli_cache_root());
        if let Some(codex) = codex::CodexAdapter::from_env(env) {
            registry.adapters.push(Arc::new(codex));
        }
        registry
    }

    /// Adapters for a Claude `projects` directory and Pi and OMP session
    /// roots.
    pub fn with_roots(
        claude: Option<PathBuf>,
        pi: Option<PiSessionRoot>,
        omp: Option<OmpSessionRoot>,
    ) -> Self {
        let mut adapters: Vec<Arc<dyn Adapter>> = Vec::new();
        if let Some(root) = claude {
            adapters.push(Arc::new(claude::ClaudeAdapter::new(root)));
        }
        if let Some(root) = pi {
            adapters.push(Arc::new(pi::PiAdapter::pi(root)));
        }
        if let Some(root) = omp {
            adapters.push(Arc::new(pi::PiAdapter::omp(root)));
        }
        Self {
            adapters,
            cli_cache: None,
        }
    }

    /// Fork: sets the installed CLI's cache root
    /// ([`HistoryEnv::cli_cache_root`]).
    pub fn with_cli_cache(mut self, root: Option<PathBuf>) -> Self {
        self.cli_cache = root;
        self
    }

    /// Fork: the installed CLI's cache root. A reconcile pass never removes
    /// old caches from a cache dir inside it, even when the env's cache root
    /// was pointed there.
    pub fn cli_cache(&self) -> Option<&Path> {
        self.cli_cache.as_deref()
    }

    /// Adds the adapter of a Codex home (fork: without a `CODEX_SQLITE_HOME`;
    /// [`Self::from_env`] adds the env's through `CodexAdapter::from_env`).
    pub fn with_codex(mut self, home: Option<PathBuf>) -> Self {
        if let Some(home) = home {
            self.adapters.push(Arc::new(codex::CodexAdapter::new(home)));
        }
        self
    }

    pub fn adapters(&self) -> &[Arc<dyn Adapter>] {
        &self.adapters
    }

    /// Fork: the instances whose kind `keep` accepts (the in-memory list
    /// loader reads the Claude projects itself and the others through this).
    pub fn filtered(&self, keep: impl Fn(SourceKind) -> bool) -> Self {
        Self {
            adapters: self
                .adapters
                .iter()
                .filter(|adapter| keep(adapter.instance().kind))
                .cloned()
                .collect(),
            cli_cache: self.cli_cache.clone(),
        }
    }

    /// The conversation whose own artifact is `path`, in the first instance
    /// whose root holds it.
    pub fn key_of(&self, path: &Path) -> Option<ConversationKey> {
        self.adapters.iter().find_map(|adapter| {
            let instance = adapter.instance();
            if !path.starts_with(&instance.root) {
                return None;
            }
            Some(ConversationKey {
                instance: instance.clone(),
                native_id: adapter.native_id_of(path)?,
            })
        })
    }

    /// The adapter that acts on conversations `source` wrote: its first
    /// configured instance, else one without a root.
    pub fn for_source(&self, source: SourceKind) -> Arc<dyn Adapter> {
        if let Some(adapter) = self
            .adapters
            .iter()
            .find(|adapter| adapter.instance().kind == source)
        {
            return adapter.clone();
        }
        match source {
            SourceKind::Claude => Arc::new(claude::ClaudeAdapter::new(PathBuf::new())),
            SourceKind::Pi => Arc::new(pi::PiAdapter::pi(PiSessionRoot {
                path: PathBuf::new(),
                flat: false,
            })),
            SourceKind::Omp => Arc::new(pi::PiAdapter::omp(OmpSessionRoot {
                path: PathBuf::new(),
                flat: false,
            })),
            SourceKind::Codex => Arc::new(codex::CodexAdapter::with_sqlite_home(
                PathBuf::new(),
                PathBuf::new(),
            )),
        }
    }

    /// The adapter of `instance`.
    /// The adapter acting on `path`, a session file of `source`: the
    /// instance of that kind whose root holds it, else as `for_source`.
    pub fn for_path(&self, source: SourceKind, path: &Path) -> Arc<dyn Adapter> {
        self.adapters
            .iter()
            .find(|adapter| {
                let instance = adapter.instance();
                instance.kind == source && path.starts_with(&instance.root)
            })
            .cloned()
            .unwrap_or_else(|| self.for_source(source))
    }

    pub fn get(&self, instance: &SourceInstance) -> Option<&Arc<dyn Adapter>> {
        self.adapters
            .iter()
            .find(|adapter| adapter.instance() == instance)
    }
}

/// `Located` for each of `paths`, an instance's session files, one per
/// conversation, stated in parallel. Files that cannot be stated are left
/// out.
fn locate_files(
    instance: &SourceInstance,
    paths: Vec<PathBuf>,
    scan_started: SystemTime,
) -> Vec<Located> {
    paths
        .into_par_iter()
        .filter_map(|path| locate_file(instance, path, scan_started))
        .collect()
}

/// `Located` for the session file `native_id` names in `instance`.
fn resolve_file(
    instance: &SourceInstance,
    native_id: &str,
    scan_started: SystemTime,
) -> Result<Located> {
    let path = instance.root.join(native_id);
    let metadata = fs::metadata(&path)?;
    Ok(located(instance, path, &metadata, scan_started))
}

fn locate_file(
    instance: &SourceInstance,
    path: PathBuf,
    scan_started: SystemTime,
) -> Option<Located> {
    let metadata = fs::metadata(&path).ok()?;
    Some(located(instance, path, &metadata, scan_started))
}

fn located(
    instance: &SourceInstance,
    path: PathBuf,
    metadata: &fs::Metadata,
    scan_started: SystemTime,
) -> Located {
    let modified = metadata.modified().ok();
    let size = metadata.len();
    Located {
        key: ConversationKey {
            instance: instance.clone(),
            native_id: native_id(&instance.root, &path),
        },
        stamp: Stamp::of(size, modified, scan_started),
        file_id: file_id(metadata),
        path,
        size,
        modified,
        native: None,
    }
}

/// Native id of a session file: its path relative to the instance root.
fn native_id(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Decodes a session file outside any configured root: a Codex rollout as
/// the thread of the Codex home holding it, else recognizing its source
/// from its contents (as OMP when it starts with a session header and
/// `omp` is set). `modified` dates a session that records no activity time.
pub fn decode_path(path: &Path, modified: Option<SystemTime>, omp: bool) -> Result<Transcript> {
    decode_path_collected(None, path, modified, omp)
}

/// [`decode_path`] of a file opened in `env` (fork, see the module docs): a
/// rollout of `env`'s own Codex home decodes through the env's adapter, with
/// its `CODEX_SQLITE_HOME`, as the history service opens it.
pub fn decode_path_in(
    env: &HistoryEnv,
    path: &Path,
    modified: Option<SystemTime>,
    omp: bool,
) -> Result<Transcript> {
    decode_path_collected(Some(env), path, modified, omp)
}

fn decode_path_collected(
    env: Option<&HistoryEnv>,
    path: &Path,
    modified: Option<SystemTime>,
    omp: bool,
) -> Result<Transcript> {
    let mut decoded = Vec::new();
    let transcript = decode_path_into(env, path, modified, omp, &mut |event| decoded.push(event))?;
    Ok(Transcript {
        events: decoded,
        ..transcript
    })
}

/// [`decode_path`], or [`decode_path_in`] with an `env`, passing the events
/// to `events` as they are decoded; the returned transcript has none. (Fork:
/// the list loaders stream a file into the indexer through this, so a file's
/// events are never all held at once.)
pub(crate) fn decode_path_into(
    env: Option<&HistoryEnv>,
    path: &Path,
    modified: Option<SystemTime>,
    omp: bool,
    events: &mut dyn FnMut(Event),
) -> Result<Transcript> {
    decode_path_into_cancellable(env, path, modified, omp, events, &|| false)
}

/// [`decode_path`] that gives up once `cancelled()` returns true, with the
/// error [`super::adapter::is_cancelled`] recognizes (fork, see the module
/// docs). A Codex rollout is checked before and after its thread is
/// decoded.
pub fn decode_path_cancellable(
    path: &Path,
    modified: Option<SystemTime>,
    omp: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Transcript> {
    if cancelled() {
        return Err(super::adapter::cancelled_error());
    }
    let mut decoded = Vec::new();
    let transcript = decode_path_into_cancellable(
        None,
        path,
        modified,
        omp,
        &mut |event| decoded.push(event),
        cancelled,
    )?;
    Ok(Transcript {
        events: decoded,
        ..transcript
    })
}

fn decode_path_into_cancellable(
    env: Option<&HistoryEnv>,
    path: &Path,
    modified: Option<SystemTime>,
    omp: bool,
    events: &mut dyn FnMut(Event),
    cancelled: &dyn Fn() -> bool,
) -> Result<Transcript> {
    if let Some(transcript) = codex::view::decode_path_into(env, path, events) {
        return match transcript {
            Ok(_) if cancelled() => Err(super::adapter::cancelled_error()),
            transcript => transcript,
        };
    }
    let instance = SourceInstance {
        kind: SourceKind::Claude,
        root: path.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let located = Located {
        key: ConversationKey {
            native_id: native_id(&instance.root, path),
            instance,
        },
        path: path.to_path_buf(),
        size: 0,
        modified,
        file_id: None,
        stamp: Stamp::default(),
        native: None,
    };
    Ok(decode_file_cancellable(&located, omp, events, cancelled)?
        .expect("a file that is not a Pi session decodes as a Claude session"))
}

/// Decodes the session file `located` names: as a Pi or OMP session when it
/// starts with a session header (always as OMP when `omp` is set), else as
/// a Claude session. `None` when it holds neither.
fn decode_file(
    located: &Located,
    omp: bool,
    events: &mut dyn FnMut(Event),
) -> Result<Option<Transcript>> {
    decode_file_cancellable(located, omp, events, &|| false)
}

/// [`decode_file`] reading the file through [`Polled`] (fork).
fn decode_file_cancellable(
    located: &Located,
    omp: bool,
    events: &mut dyn FnMut(Event),
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Transcript>> {
    let path = &located.path;
    if super::pi::detect_source(path)?.is_some() {
        let mut reader = Counted::new(BufReader::new(Polled::new(File::open(path)?, cancelled)));
        let expected = omp.then_some(SourceKind::Omp);
        if let Some(session) = super::pi::parse_reader(&mut reader, expected)? {
            return Ok(Some(pi::transcript_of(
                located.key.clone(),
                path,
                session,
                reader.bytes,
                located.modified,
                events,
            )));
        }
    }
    let reader = BufReader::new(Polled::new(File::open(path)?, cancelled));
    claude::decode_reader(located.key.clone(), path, reader, located.modified, events).map(Some)
}

/// When a conversation was last active: the newest timestamp of its own
/// user and assistant messages, tool results included, else of the
/// messages it inherited from the conversation it was forked from.
#[derive(Default)]
pub(crate) struct LastActivity {
    own: Option<DateTime<Utc>>,
    inherited: Option<DateTime<Utc>>,
}

impl LastActivity {
    pub(crate) fn observe(&mut self, event: &Event) {
        if !matches!(event.kind, EventKind::Message(_)) {
            return;
        }
        let latest = match event.visibility {
            Visibility::Dialogue => &mut self.own,
            Visibility::Inherited => &mut self.inherited,
            Visibility::Injected => return,
        };
        *latest = (*latest).max(event.timestamp);
    }

    pub(crate) fn at(&self) -> Option<DateTime<Utc>> {
        self.own.or(self.inherited)
    }
}

/// [`decode_file_cancellable`] collecting the events into the transcript:
/// the Claude, Pi and OMP adapters' [`Adapter::decode_cancellable`] (fork).
fn decode_file_collected(
    located: &Located,
    omp: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Transcript>> {
    if cancelled() {
        return Err(super::adapter::cancelled_error());
    }
    let mut decoded = Vec::new();
    let transcript =
        decode_file_cancellable(located, omp, &mut |event| decoded.push(event), cancelled)?;
    Ok(transcript.map(|transcript| Transcript {
        events: decoded,
        ..transcript
    }))
}

/// A reader that fails with [`super::adapter::cancelled_io_error`] once
/// `cancelled()` returns true, checked at every read (fork; behind a
/// `BufReader`, once per buffer fill).
struct Polled<'a, R> {
    inner: R,
    cancelled: &'a dyn Fn() -> bool,
}

impl<'a, R> Polled<'a, R> {
    fn new(inner: R, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self { inner, cancelled }
    }
}

impl<R: Read> Read for Polled<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(super::adapter::cancelled_io_error());
        }
        self.inner.read(buf)
    }
}

/// A reader that counts the bytes consumed through it.
struct Counted<R> {
    inner: R,
    bytes: u64,
}

impl<R> Counted<R> {
    fn new(inner: R) -> Self {
        Self { inner, bytes: 0 }
    }
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes += read as u64;
        Ok(read)
    }
}

impl<R: BufRead> BufRead for Counted<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.bytes += amount as u64;
        self.inner.consume(amount);
    }
}

#[cfg(unix)]
fn file_id(metadata: &fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_id(_metadata: &fs::Metadata) -> Option<FileId> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::model::{Message, Origin, Role};

    fn event(minute: u32, visibility: Visibility, message: bool) -> Event {
        let kind = if message {
            EventKind::Message(Message {
                role: Role::User,
                id: None,
                parts: Vec::new(),
                model: None,
                usage: None,
                parent_tool_use_id: None,
            })
        } else {
            EventKind::Other
        };
        Event {
            origin: Origin::default(),
            timestamp: DateTime::from_timestamp(i64::from(minute) * 60, 0),
            visibility,
            kind,
        }
    }

    #[test]
    fn last_activity_prefers_own_messages_over_inherited_ones() {
        let mut activity = LastActivity::default();
        activity.observe(&event(30, Visibility::Inherited, true));
        assert_eq!(activity.at(), DateTime::from_timestamp(30 * 60, 0));
        activity.observe(&event(10, Visibility::Dialogue, true));
        activity.observe(&event(5, Visibility::Dialogue, true));
        activity.observe(&event(40, Visibility::Injected, true));
        activity.observe(&event(50, Visibility::Dialogue, false));
        assert_eq!(activity.at(), DateTime::from_timestamp(10 * 60, 0));
    }
}
