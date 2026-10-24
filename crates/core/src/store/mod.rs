//! Disk snapshot of the conversation list, readable before any history is
//! loaded.
//!
//! The snapshot is a generation directory under `v3/roots-<hash>/` in the cache
//! dir (see `default_root` and `generation`). The TUI renders its first frame
//! from the newest cards and replaces them with fully loaded conversations
//! once loading finishes.
//!
//! # Differences from upstream
//!
//! * **Root (hermetic).** [`default_root`] takes the [`HistoryEnv`] and
//!   derives `v3/roots-<hash>/` from [`HistoryEnv::history_cache_base_dir`]
//!   (the fork's own cache root, `~/.cache/claude-history-gui`, namespaced by
//!   `CLAUDE_CONFIG_DIR` like upstream) and the Pi/OMP session roots resolved
//!   from the same env (the Pi root also follows the workspace's
//!   `.pi/settings.json`). Upstream returned `None` under
//!   `cfg(test)`; here tests are kept off the user's cache by
//!   [`HistoryEnv::sandbox`] and temporary roots instead.
//! * **Shared corpus.** [`write_snapshot`] and
//!   [`generation::build_manifest`] accept any slice of
//!   `Borrow<Conversation>`, so the browser's `Arc<Conversation>` corpus is
//!   written without copying. The manifest bytes are the same.
//! * **Windows tests.** `lexindex`'s corrupt-file test drops its open index
//!   (three live maps) before it rewrites and truncates the index files,
//!   which Windows refuses for a mapped file. The code is upstream's.
//!
//! The format, the publish protocol and the collection rules are upstream's.
//! On Windows (NTFS) `std::fs` removes and renames over files that another
//! handle maps (POSIX semantics), so collection behaves as on Unix and an
//! open map stays readable; only writing or truncating a mapped file fails
//! (`ERROR_USER_MAPPED_FILE`), and published files are never written again.
//! On a file system without POSIX semantics, removing a generation that a
//! reader still maps fails silently, like every collection error, and a later
//! publish removes it once the map is dropped.

pub mod card;
pub mod generation;
#[cfg_attr(not(test), allow(dead_code))]
pub mod lexindex;

pub use generation::{Generation, ProjectView, Publish};

use crate::env::HistoryEnv;
use crate::history::Conversation;
use crate::time_filter::TimeFilter;
use std::borrow::Borrow;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Store root: `v3/roots-<hash>/` beside the other caches. The cache dir is
/// namespaced by `CLAUDE_CONFIG_DIR` ([`HistoryEnv::history_cache_base_dir`]),
/// and the hash covers the Pi and OMP session roots resolved from `env`, so a
/// snapshot is only read by runs that load the same sources. `None` without a
/// cache root.
pub fn default_root(env: &HistoryEnv) -> Option<PathBuf> {
    let pi = crate::history::pi_loader::session_root(env).ok();
    let omp = crate::history::omp_loader::session_root(env).ok();
    let namespace = source_roots_namespace(
        pi.as_ref().map(|root| root.path.as_path()),
        omp.as_ref().map(|root| root.path.as_path()),
    );
    env.history_cache_base_dir()
        .map(|base| base.join("v3").join(namespace))
}

/// Directory name identifying the Pi and OMP session roots, canonicalized
/// when they exist.
fn source_roots_namespace(pi: Option<&Path>, omp: Option<&Path>) -> String {
    let mut hasher = blake3::Hasher::new();
    for root in [pi, omp] {
        match root {
            Some(path) => {
                let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let bytes = resolved.as_os_str().as_encoded_bytes();
                hasher.update(&[1]);
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(bytes);
            }
            None => {
                hasher.update(&[0]);
            }
        }
    }
    format!("roots-{}", &hasher.finalize().to_hex()[..16])
}

/// Cards of the current generation, shown until the full corpus is loaded.
pub struct Snapshot {
    generation: Generation,
    show_last: bool,
    time: TimeFilter,
}

impl Snapshot {
    /// Opens the current generation under `root`; `None` when there is no
    /// usable one.
    pub fn open(root: &Path, show_last: bool, time: TimeFilter) -> Option<Self> {
        Some(Self {
            generation: Generation::open(root)?,
            show_last,
            time,
        })
    }

    pub fn generation_name(&self) -> &str {
        self.generation.name()
    }

    /// The newest `n` cards inside the time filter whose project passes
    /// `project_visible`, as list-only conversations indexed from 0.
    pub fn conversations(
        &self,
        n: usize,
        mut project_visible: impl FnMut(&ProjectView) -> bool,
    ) -> Vec<Conversation> {
        let generation = &self.generation;
        let visible: Vec<bool> = (0..generation.projects().len() as u32)
            .map(|id| project_visible(&generation.project(id)))
            .collect();
        let time = &self.time;
        generation
            .head(n, |row| {
                visible[row.project_id as usize]
                    && (!time.is_active()
                        || chrono::DateTime::from_timestamp_millis(row.timestamp_ms)
                            .is_some_and(|timestamp| time.matches(timestamp)))
            })
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let mut conversation = generation.conversation(row, self.show_last);
                conversation.index = index;
                conversation
            })
            .collect()
    }
}

/// Writes a generation for `conversations` (the complete corpus, loaded
/// starting at `loaded_since`) unless `CURRENT` already matches it.
pub fn write_snapshot<C: Borrow<Conversation>>(
    root: &Path,
    conversations: &[C],
    loaded_since: SystemTime,
) -> std::io::Result<Publish> {
    let fingerprints: Vec<_> = conversations
        .iter()
        .map(|conversation| generation::fingerprint(&conversation.borrow().path, loaded_since))
        .collect();
    let manifest = generation::build_manifest(conversations, &fingerprints);
    generation::publish(root, &manifest)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::card::Fingerprint;
    use crate::history::{Conversation, Source};
    use chrono::{Local, TimeZone};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// A Claude conversation stored at `path` with `timestamp_ms`.
    pub(crate) fn conversation(path: &str, timestamp_ms: i64) -> Conversation {
        let path = PathBuf::from(path);
        let session_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned();
        let project_name = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .and_then(|name| name.rsplit('-').next())
            .map(str::to_owned);
        let empty: Arc<str> = Arc::from("");
        Conversation {
            source: Source::Claude,
            session_id,
            path,
            index: 0,
            timestamp: Local.timestamp_millis_opt(timestamp_ms).unwrap(),
            preview: "first".into(),
            preview_first: "first".into(),
            preview_last: "last".into(),
            full_text: "full text".into(),
            agent_search_text: empty.clone(),
            semantic_route_text: empty.clone(),
            semantic_turns: Arc::from(Vec::new()),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: "full text".into(),
            project_name,
            project_path: None,
            cwd: None,
            message_count: 3,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 42,
            duration_minutes: None,
        }
    }

    pub(crate) fn unknown_fingerprints(count: usize) -> Vec<Fingerprint> {
        vec![Fingerprint::default(); count]
    }
}

/// Fork-only: the env-derived root and the `Borrow<Conversation>` writer.
#[cfg(test)]
mod tests {
    use super::test_support::conversation;
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn snapshot_namespace_follows_pi_and_omp_roots() {
        let pi = Path::new("/tmp/claude-history-test/pi-sessions");
        let other_pi = Path::new("/tmp/claude-history-test/other-pi-sessions");
        let omp = Path::new("/tmp/claude-history-test/omp-sessions");

        let base = source_roots_namespace(Some(pi), Some(omp));
        assert!(base.starts_with("roots-"));
        assert_eq!(base, source_roots_namespace(Some(pi), Some(omp)));
        for other in [
            source_roots_namespace(Some(other_pi), Some(omp)),
            source_roots_namespace(Some(pi), Some(other_pi)),
            source_roots_namespace(Some(omp), Some(pi)),
            source_roots_namespace(None, Some(omp)),
            source_roots_namespace(Some(pi), None),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn default_root_follows_the_resolved_pi_session_root() {
        let home = tempfile::tempdir().unwrap();
        let base = default_root(&HistoryEnv::sandbox(home.path())).unwrap();
        let again = default_root(&HistoryEnv::sandbox(home.path())).unwrap();
        assert_eq!(base, again);
        let moved = default_root(
            &HistoryEnv::sandbox(home.path()).with_pi_session_dir(home.path().join("elsewhere")),
        )
        .unwrap();
        assert_ne!(base, moved);
        assert_eq!(base.parent(), moved.parent());
        assert!(base.parent().unwrap().ends_with("v3"));
    }

    #[test]
    fn shared_corpus_writes_the_same_snapshot_under_the_sandbox_root() {
        let home = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(home.path());
        let root = default_root(&env).unwrap();
        assert!(root.starts_with(home.path()));

        // Real files under the sandbox, so the fingerprints (size, mtime of
        // files older than `since`) are non-default and nothing outside the
        // sandbox is read.
        let project_dir = home.path().join("-Users-me-app");
        std::fs::create_dir_all(&project_dir).unwrap();
        let paths: Vec<PathBuf> = (0..3)
            .map(|i| {
                let path = project_dir.join(format!("{i}.jsonl"));
                std::fs::write(
                    &path,
                    format!(
                        "{{\"n\":{i}}}
"
                    )
                    .repeat(i + 1),
                )
                .unwrap();
                path
            })
            .collect();
        let shared: Vec<Arc<Conversation>> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| Arc::new(conversation(path.to_str().unwrap(), i as i64 * 1000)))
            .collect();
        let since = SystemTime::now() + Duration::from_secs(60);
        assert!(matches!(
            write_snapshot(&root, &shared, since).unwrap(),
            Publish::Published(_)
        ));
        let owned: Vec<Conversation> = shared.iter().map(|c| (**c).clone()).collect();
        assert_eq!(
            write_snapshot(&root, &owned, since).unwrap(),
            Publish::Unchanged
        );

        let snapshot = Snapshot::open(&root, false, TimeFilter::default()).unwrap();
        let cards = snapshot.conversations(2, |_| true);
        let listed: Vec<_> = cards.iter().map(|c| (c.index, c.path.clone())).collect();
        assert_eq!(listed, vec![(0, paths[2].clone()), (1, paths[1].clone()),]);
        assert!(
            snapshot
                .conversations(10, |project| project.name != Some("app"))
                .is_empty()
        );
    }
}
