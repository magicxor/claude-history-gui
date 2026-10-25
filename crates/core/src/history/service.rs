//! Opens conversations by key: resolves the history the key selects
//! through its instance's adapter and decodes it, reusing the transcript
//! decoded last while the history's artifacts are unchanged. The
//! transcripts of the conversations opened most recently are kept.
//!
//! # Differences from upstream
//!
//! * **No process-wide service.** Upstream's `shared()` served the TUI and
//!   CLI from the process environment; here a service is built for a
//!   [`crate::HistoryEnv`] ([`HistoryService::from_env`]) and owned by its
//!   user (the GUI holds one per environment, `AgentService` its own), and
//!   [`HistoryService::default`] has no instances.
//! * **Cancellable opening.** [`HistoryService::open_cancellable`] and
//!   [`HistoryService::open_path_cancellable`] give a decode up once a flag
//!   is set (the GUI viewer abandons an open the user moved past, V-101),
//!   with the error [`super::adapter::is_cancelled`] recognizes.

use super::adapter::{Located, Stamp};
use super::model::{ConversationKey, Revision, Transcript};
use super::sources::Registry;
use crate::error::{AppError, Result};
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// How many decoded transcripts a service keeps.
const KEPT: usize = 8;

type Opened = HashMap<ConversationKey, (u64, Stamp, Arc<Transcript>)>;

pub struct HistoryService {
    registry: Registry,
    opened: Mutex<(u64, Opened)>,
}

/// Fork: no instances (the registry is built from a `HistoryEnv`, never the
/// process environment), so every path opens as a loose session file.
impl Default for HistoryService {
    fn default() -> Self {
        Self::new(Registry::default())
    }
}

impl HistoryService {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            opened: Mutex::default(),
        }
    }

    /// The service over the instances `env` configures (fork: upstream's
    /// `shared()` read the process environment).
    pub fn from_env(env: &crate::HistoryEnv) -> Self {
        Self::new(Registry::from_env(env))
    }

    /// The selected history of conversation `key`.
    pub fn open(&self, key: &ConversationKey) -> Result<(Revision, Arc<Transcript>)> {
        self.open_cancellable(key, &|| false)
    }

    /// [`Self::open`] that gives up once `cancelled()` returns true (fork,
    /// see the module docs).
    pub fn open_cancellable(
        &self,
        key: &ConversationKey,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(Revision, Arc<Transcript>)> {
        let Some(adapter) = self.registry.get(&key.instance) else {
            return Err(not_found(&format!(
                "no {} history is configured at {}",
                key.instance.kind.label(),
                key.instance.root.display()
            )));
        };
        let located = adapter.resolve(&key.native_id, SystemTime::now())?;
        if let Some(transcript) = self.reusable(key, &located) {
            return Ok((transcript.revision, transcript));
        }
        let Some(transcript) = adapter.decode_cancellable(&located, cancelled)? else {
            return Err(not_found(&format!(
                "{} holds no conversation",
                located.path.display()
            )));
        };
        let transcript = Arc::new(transcript);
        if located.stamp.is_known() {
            let mut opened = self.lock();
            let (clock, kept) = &mut *opened;
            *clock += 1;
            if kept.len() >= KEPT && !kept.contains_key(key) {
                let oldest = kept
                    .iter()
                    .min_by_key(|(_, (used, ..))| *used)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    kept.remove(&oldest);
                }
            }
            kept.insert(key.clone(), (*clock, located.stamp, transcript.clone()));
        }
        Ok((transcript.revision, transcript))
    }

    /// The conversation whose own artifact is `path`: through the instance
    /// whose root holds it, else decoded as a loose session file.
    pub fn open_path(&self, path: &Path) -> Result<(Revision, Arc<Transcript>)> {
        self.open_path_cancellable(path, &|| false)
    }

    /// [`Self::open_path`] that gives up once `cancelled()` returns true
    /// (fork, see the module docs).
    pub fn open_path_cancellable(
        &self,
        path: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(Revision, Arc<Transcript>)> {
        match self.registry.key_of(path) {
            Some(key) => self.open_cancellable(&key, cancelled),
            None => {
                let transcript = Arc::new(super::sources::decode_path_cancellable(
                    path, None, false, cancelled,
                )?);
                Ok((transcript.revision, transcript))
            }
        }
    }

    fn reusable(&self, key: &ConversationKey, located: &Located) -> Option<Arc<Transcript>> {
        let mut opened = self.lock();
        let (clock, kept) = &mut *opened;
        let (used, stamp, transcript) = kept.get_mut(key)?;
        if !located.stamp.is_known() || *stamp != located.stamp {
            return None;
        }
        *clock += 1;
        *used = *clock;
        Some(transcript.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u64, Opened)> {
        self.opened
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn not_found(message: &str) -> AppError {
    AppError::Io(io::Error::new(io::ErrorKind::NotFound, message.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::model::SourceInstance;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    const USER: &str = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#;

    fn settle(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    fn fixture() -> (tempfile::TempDir, HistoryService, ConversationKey) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        fs::create_dir_all(root.join("-p")).unwrap();
        let path = root.join("-p/s.jsonl");
        fs::write(&path, format!("{USER}\n")).unwrap();
        settle(&path);
        let service = HistoryService::new(Registry::with_roots(Some(root.clone()), None, None));
        let key = ConversationKey {
            instance: SourceInstance {
                kind: crate::history::model::SourceKind::Claude,
                root,
            },
            native_id: "-p/s.jsonl".to_owned(),
        };
        (dir, service, key)
    }

    #[test]
    fn unchanged_histories_reuse_the_decoded_transcript() {
        let (_dir, service, key) = fixture();
        let (revision, first) = service.open(&key).unwrap();
        let (again, second) = service.open(&key).unwrap();
        assert_eq!(revision, again);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.events.len(), 1);
    }

    #[test]
    fn changed_histories_are_decoded_again() {
        let (_dir, service, key) = fixture();
        let (revision, _) = service.open(&key).unwrap();
        let path = key.instance.root.join(&key.native_id);
        fs::write(&path, format!("{USER}\n{USER}\n")).unwrap();
        settle(&path);
        let (changed, transcript) = service.open(&key).unwrap();
        assert_ne!(revision, changed);
        assert_eq!(transcript.events.len(), 2);
    }

    #[test]
    fn paths_open_through_the_instance_that_holds_them() {
        let (dir, service, key) = fixture();
        let path = key.instance.root.join(&key.native_id);
        let (_, transcript) = service.open_path(&path).unwrap();
        assert_eq!(transcript.key, key);

        let loose = dir.path().join("loose.jsonl");
        fs::write(&loose, format!("{USER}\n")).unwrap();
        let (_, transcript) = service.open_path(&loose).unwrap();
        assert_eq!(transcript.path(), loose);
    }

    /// Fork: upstream has no test of the LRU of [`KEPT`] transcripts.
    #[test]
    fn the_transcripts_opened_most_recently_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        fs::create_dir_all(root.join("-p")).unwrap();
        let service = HistoryService::new(Registry::with_roots(Some(root.clone()), None, None));
        let paths: Vec<PathBuf> = (0..=KEPT)
            .map(|index| {
                let path = root.join(format!("-p/s{index}.jsonl"));
                fs::write(&path, format!("{USER}\n")).unwrap();
                settle(&path);
                path
            })
            .collect();
        let first = service.open_path(&paths[0]).unwrap().1;
        let second = service.open_path(&paths[1]).unwrap().1;
        for path in &paths[2..KEPT] {
            service.open_path(path).unwrap();
        }
        // Using the first again leaves the second the least recently used.
        assert!(Arc::ptr_eq(
            &first,
            &service.open_path(&paths[0]).unwrap().1
        ));
        service.open_path(&paths[KEPT]).unwrap();
        assert_eq!(service.lock().1.len(), KEPT);
        assert!(Arc::ptr_eq(
            &first,
            &service.open_path(&paths[0]).unwrap().1
        ));
        assert!(
            !Arc::ptr_eq(&second, &service.open_path(&paths[1]).unwrap().1),
            "the least recently used transcript was dropped"
        );
    }

    /// Fork (V-101): a cancelled open stops reading the file.
    #[test]
    fn cancelled_opens_stop_reading() {
        let (dir, service, key) = fixture();
        let path = key.instance.root.join(&key.native_id);
        fs::write(&path, format!("{USER}\n").repeat(20_000)).unwrap();
        settle(&path);
        let polls = std::cell::Cell::new(0_usize);
        let cancelled = || {
            polls.set(polls.get() + 1);
            polls.get() > 2
        };
        let error = service.open_cancellable(&key, &cancelled).unwrap_err();
        assert!(super::super::adapter::is_cancelled(&error), "{error}");
        assert!(polls.get() < 10, "stopped after {} polls", polls.get());
        assert!(service.lock().1.is_empty(), "nothing is kept");

        let loose = dir.path().join("loose.jsonl");
        fs::write(&loose, format!("{USER}\n").repeat(20_000)).unwrap();
        polls.set(0);
        let error = service
            .open_path_cancellable(&loose, &cancelled)
            .unwrap_err();
        assert!(super::super::adapter::is_cancelled(&error), "{error}");
        assert!(polls.get() < 10, "stopped after {} polls", polls.get());
        assert_eq!(service.open(&key).unwrap().1.events.len(), 20_000);
    }

    #[test]
    fn missing_histories_are_not_found() {
        let (_dir, service, mut key) = fixture();
        key.native_id = "-p/gone.jsonl".to_owned();
        assert!(matches!(
            service.open(&key),
            Err(AppError::Io(error)) if error.kind() == io::ErrorKind::NotFound
        ));
        key.instance.root = PathBuf::from("/elsewhere");
        assert!(matches!(
            service.open(&key),
            Err(AppError::Io(error)) if error.kind() == io::ErrorKind::NotFound
        ));
    }
}
