//! Disk snapshot of the conversation list and its lexical index, readable
//! before any history is loaded.
//!
//! The snapshot is a generation directory under `v3/roots-<hash>/` in the
//! cache dir (see `default_root` and `generation`). The TUI renders its first
//! frame from the newest cards, searches the generation's index, and replaces
//! the cards with fully loaded conversations once loading finishes.
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
//! * **Shared corpus.** [`Snapshot::locate`] and [`Snapshot::covers`]
//!   accept any slice of `Borrow<Conversation>`, so the browser's
//!   `Arc<Conversation>` corpus is located without copying.
//! * **Sources from the env.** [`reconcile::Sources::from_env`] takes the
//!   [`HistoryEnv`] (the Claude projects root and the Pi/OMP session roots
//!   the history loader reads) instead of the process environment.
//! * **In-process, cancellable reconcile.** The GUI runs the reconcile pass
//!   on a thread of its own process (`browser::HistoryLoader`; upstream: a
//!   detached `--reconcile` child that outlived the TUI), so quitting must
//!   not wait for a pass: [`reconcile::reconcile_cancellable`],
//!   [`generation::publish_update_cancellable`],
//!   [`generation::compact_cancellable`], [`lexindex::build_cancellable`] and
//!   [`lexindex::merge_cancellable`] give up soon after a flag is set,
//!   remove the `gNNN.tmp` they staged and publish nothing more (the
//!   generations a pass published before stay). [`reconcile::reconcile`],
//!   [`generation::publish_update`], [`generation::compact`],
//!   [`lexindex::build`] and [`lexindex::merge`] are the same code never
//!   cancelled, and write the same bytes as upstream.
//! * **Windows tests.** `lexindex`'s corrupt-file test drops its open index
//!   (three live maps) before it rewrites and truncates the index files,
//!   which Windows refuses for a mapped file, and `reconcile`'s removed-files
//!   test allows the variable only its Unix permission check reads. The code
//!   is upstream's.
//! * **Real-corpus oracle.** The ignored `search` oracle test reads the QA
//!   sandbox named by `CHG_SEARCH_CORPUS_HOME` (it must contain
//!   `.qa-sandbox`) through [`HistoryEnv::sandbox`], never the process home,
//!   and skips when the variable is unset. The code under test is upstream's.
//!
//! The format, the publish protocol and the collection rules are upstream's.
//! On Windows (NTFS) `std::fs` removes and renames over files that another
//! handle maps (POSIX semantics), so collection behaves as on Unix and an
//! open map stays readable; only writing or truncating a mapped file fails
//! (`ERROR_USER_MAPPED_FILE`), and published files (manifest, text segments,
//! index segments) are never written again. NTFS hard-links the unchanged
//! segments a delta generation shares with its predecessor, also while they
//! are mapped; elsewhere they are copied, as upstream falls back. Without
//! file ids off Unix, a session file reached through two paths (a junction)
//! gets a card under each, as upstream (the history loader keeps one, by
//! canonical path). On a file system
//! without POSIX semantics, removing a generation that a reader still maps
//! fails silently, like every collection error, and a later publish removes
//! it once the map is dropped.

pub mod card;
pub mod generation;
pub mod lexindex;
pub mod reconcile;
pub mod search;
pub mod segments;
pub mod text;

pub use generation::{Generation, ProjectView};

use crate::env::HistoryEnv;
use crate::history::Conversation;
use crate::time_filter::TimeFilter;
use chrono::{DateTime, Local};
use std::borrow::{Borrow, Cow};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
#[derive(Clone)]
pub struct Snapshot {
    generation: Arc<Generation>,
    show_last: bool,
    time: TimeFilter,
}

impl Snapshot {
    /// Opens the current generation under `root`; `None` when there is no
    /// usable one.
    pub fn open(root: &Path, show_last: bool, time: TimeFilter) -> Option<Self> {
        Some(Self {
            generation: Arc::new(Generation::open(root)?),
            show_last,
            time,
        })
    }

    pub fn generation_name(&self) -> &str {
        self.generation.name()
    }

    /// Number of cards; doc ids are `0..len()`.
    pub fn len(&self) -> usize {
        self.generation.rows().len()
    }

    /// Whether the generation has no card (fork: the library API's
    /// companion of `len`).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Ids of the newest `n` cards inside the time filter whose project
    /// passes `project_visible`.
    pub fn head(&self, n: usize, project_visible: impl FnMut(&ProjectView) -> bool) -> Vec<u32> {
        let visible = self.project_visibility(project_visible);
        self.generation.head(n, |row| {
            visible[row.project_id as usize] && in_time(&self.time, row)
        })
    }

    /// For every card, whether it is inside the time filter and its project
    /// passes `project_visible`.
    pub fn scope(&self, project_visible: impl FnMut(&ProjectView) -> bool) -> Vec<bool> {
        let visible = self.project_visibility(project_visible);
        self.generation
            .rows()
            .iter()
            .map(|row| visible[row.project_id as usize] && in_time(&self.time, row))
            .collect()
    }

    fn project_visibility(
        &self,
        mut project_visible: impl FnMut(&ProjectView) -> bool,
    ) -> Vec<bool> {
        (0..self.generation.projects().len() as u32)
            .map(|id| project_visible(&self.generation.project(id)))
            .collect()
    }

    /// Card `doc` as a list-only conversation.
    pub fn card(&self, doc: u32) -> Conversation {
        self.generation.conversation(doc, self.show_last)
    }

    /// The preview card `doc` shows.
    pub fn preview(&self, doc: u32) -> &str {
        let row = &self.generation.rows()[doc as usize];
        self.generation.str(if self.show_last {
            row.preview_last
        } else {
            row.preview_first
        })
    }

    /// For every card, the index in `conversations` of the conversation
    /// stored at the same path, if any.
    pub fn locate<C: Borrow<Conversation>>(&self, conversations: &[C]) -> Vec<Option<usize>> {
        let by_path: HashMap<&Path, usize> = conversations
            .iter()
            .enumerate()
            .map(|(index, conversation)| (conversation.borrow().path.as_path(), index))
            .collect();
        self.generation
            .rows()
            .iter()
            .map(|row| {
                by_path
                    .get(Path::new(self.generation.str(row.path)))
                    .copied()
            })
            .collect()
    }

    /// Whether the cards describe exactly `conversations`, given the
    /// result of [`Self::locate`]: every conversation has a card with the
    /// same timestamp. A newer or changed conversation makes it false.
    pub fn covers<C: Borrow<Conversation>>(
        &self,
        conversations: &[C],
        located: &[Option<usize>],
    ) -> bool {
        let mut matched = 0;
        for (row, position) in self.generation.rows().iter().zip(located) {
            if let Some(position) = position {
                if conversations[*position]
                    .borrow()
                    .timestamp
                    .timestamp_millis()
                    != row.timestamp_ms
                {
                    return false;
                }
                matched += 1;
            }
        }
        matched == conversations.len()
    }
}

fn in_time(time: &TimeFilter, row: &card::CardRow) -> bool {
    !time.is_active()
        || chrono::DateTime::from_timestamp_millis(row.timestamp_ms)
            .is_some_and(|timestamp| time.matches(timestamp))
}

/// A generation's cards, suffix index and text, ready for lexical search.
pub struct SearchIndex {
    cards: Snapshot,
    index: segments::IndexSet,
    text: text::TextSegments,
    docs: search::SearchDocs,
}

impl SearchIndex {
    /// Opens the index and text of the generation behind `cards`; `None`
    /// when it has none usable.
    pub fn open(cards: Snapshot) -> Option<Self> {
        let (index, text) = cards.generation.open_search_files()?;
        Some(Self {
            docs: search::SearchDocs::from_generation(&cards.generation),
            cards,
            index,
            text,
        })
    }

    pub fn cards(&self) -> &Snapshot {
        &self.cards
    }

    /// Starts `query` over the documents for which `scope` holds.
    pub fn search<'a>(
        &'a self,
        query: &str,
        now: DateTime<Local>,
        scope: &(dyn Fn(u32) -> bool + Sync),
    ) -> search::Ranking<'a> {
        search::search(&self.index, &self.docs, self, query, now, scope)
    }

    /// `Conversation::full_text` of card `doc`, decompressed from its frame.
    pub fn text(&self, doc: u32) -> String {
        let row = &self.cards.generation.rows()[doc as usize];
        self.text.read(row.text).unwrap_or_default()
    }
}

impl search::TextSource for SearchIndex {
    fn full_text(&self, doc: u32) -> Cow<'_, str> {
        Cow::Owned(self.text(doc))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
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
}

/// Fork-only: the env-derived root and sources, and the cards a pass over
/// them writes for what the history loader loads.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;

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
    fn a_pass_over_the_env_sources_covers_what_the_loader_loads() {
        let home = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(home.path());
        let root = default_root(&env).unwrap();
        assert!(root.starts_with(home.path()), "{root:?}");

        let projects = env.claude_projects_root().unwrap();
        let work = home.path().join("work");
        for (project, name, text) in [
            ("-tmp-work", "a.jsonl", "alpha words"),
            ("-tmp-other", "b.jsonl", "beta words"),
        ] {
            let dir = projects.join(project);
            fs::create_dir_all(&dir).unwrap();
            let line = serde_json::json!({
                "type": "user",
                "timestamp": "2024-01-01T00:00:00Z",
                "cwd": work,
                "message": {"role": "user", "content": text}
            });
            fs::write(
                dir.join(name),
                format!(
                    "{line}
"
                ),
            )
            .unwrap();
        }
        let pi = crate::history::pi_loader::session_root(&env).unwrap();
        assert!(pi.path.starts_with(home.path()), "{pi:?}");
        let pi_dir = pi.path.join("--tmp-pi--");
        fs::create_dir_all(&pi_dir).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v1.jsonl"),
            pi_dir.join("one.jsonl"),
        )
        .unwrap();

        let sources = reconcile::Sources::from_env(&env);
        assert_eq!(sources.claude.as_deref(), Some(projects.as_path()));
        let outcome = reconcile::reconcile(&root, &sources).unwrap();
        assert!(
            matches!(outcome.publish, generation::Publish::Published(_)),
            "{outcome:?}"
        );
        assert_eq!((outcome.scanned, outcome.parsed), (3, 3));

        let loaded: Vec<Arc<Conversation>> =
            crate::history::load_all_conversations(&env, true, None)
                .unwrap()
                .into_iter()
                .map(Arc::new)
                .collect();
        assert_eq!(loaded.len(), 3);
        let snapshot = Snapshot::open(&root, true, TimeFilter::default()).unwrap();
        let located = snapshot.locate(&loaded);
        assert!(located.iter().all(Option::is_some), "{located:?}");
        assert!(snapshot.covers(&loaded, &located));
        for (doc, position) in located.iter().enumerate() {
            let card = snapshot.card(doc as u32);
            let conversation = &loaded[position.unwrap()];
            assert_eq!(card.source, conversation.source);
            assert_eq!(card.session_id, conversation.session_id);
            assert_eq!(card.project_path, conversation.project_path);
            assert_eq!(card.project_name, conversation.project_name);
            assert_eq!(card.preview, conversation.preview);
        }
        let index = SearchIndex::open(snapshot).expect("the pass wrote the index");
        let alpha = (0..3)
            .find(|&doc| index.text(doc).contains("alpha words"))
            .expect("the text of a.jsonl");
        assert_eq!(
            index.cards().card(alpha).path,
            projects.join("-tmp-work").join("a.jsonl")
        );
    }
}
