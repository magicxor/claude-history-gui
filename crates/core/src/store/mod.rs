//! Disk snapshot of the conversation list and its lexical index, readable
//! before any history is loaded.
//!
//! The snapshot is a generation directory under `v3/roots-<hash>/` in the
//! cache dir (see `default_root` and `generation`). The TUI lists the
//! generation's cards, searches its index, and swaps to each newer generation
//! that `reconcile` publishes.
//!
//! # Differences from upstream
//!
//! * **Root (hermetic).** [`default_root`] takes the [`HistoryEnv`] and
//!   derives `v3/roots-<hash>/` from [`HistoryEnv::cache_base_dir`]
//!   (the fork's own cache root, `~/.cache/claude-history-gui`, namespaced by
//!   `CLAUDE_CONFIG_DIR` like upstream) and the Pi/OMP session roots resolved
//!   from the same env (the Pi root also follows the workspace's
//!   `.pi/settings.json`). Upstream returned `None` under
//!   `cfg(test)`; here tests are kept off the user's cache by
//!   [`HistoryEnv::sandbox`] and temporary roots instead.
//! * **Time filter at runtime.** [`Snapshot::with_time_filter`] gives a
//!   snapshot of the same mapped generation under another time filter: the
//!   GUI changes the filter while it runs (upstream fixed it at launch).
//! * **Sources from the env.** Reconcile lists the instances of
//!   [`crate::history::sources::Registry::from_env`], which takes the
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
//! * **Semantic cards.** [`SemanticCards::load_cancellable`] gives up when
//!   a flag is set (the browser joins the thread that reads them when it is
//!   dropped), and [`SemanticCards::generation_id`] names the generation
//!   across store roots like [`Snapshot::generation_id`].
//!   [`Snapshot::show_last`] stays: the browser's generation opens use it
//!   (upstream ecc2f2f removed it with its last TUI user).
//! * **Whole conversations from the store.** [`load_conversations`] takes
//!   the [`HistoryEnv`] for the root, the sources and the fallback
//!   ([`load_in_memory`], which takes it too). Its pass is upstream's
//!   uncancellable [`reconcile::reconcile`] on the caller's thread (a GUI
//!   tool or Deep search worker) and rayon's global pool. The `LOCK` it may
//!   wait on is usually held by the list's own `store-reconcile` thread
//!   rather than another process: the lock belongs to an open file handle,
//!   so a thread of the same process waits for it like another process.
//!   [`load_conversations_cancellable`] (agent search, whose jobs the GUI
//!   cancels) gives the pass, that wait
//!   ([`generation::PublishLock::wait_cancellable`], which asks for the lock
//!   every [`generation::LOCK_WAIT_POLL`]) and the card reads up once a flag
//!   is set. Each pass's outcome goes to the debug sink at the caller's
//!   level (upstream used the level only when it parsed the files instead).
//! * **Indexing progress in-process.** A pass's [`progress::Reporter`]
//!   writes `PROGRESS` from a thread of the GUI process that runs the pass
//!   (upstream: of the detached `--reconcile` child), and the list reads it
//!   on the pass's `store-watch` thread, which wakes the poll when it
//!   changes (upstream's TUI read it at every `CURRENT` poll). A failed pass
//!   still records `failed <message>`, but the GUI takes the message from
//!   the pass's error. Off Unix [`progress::read`] cannot tell a dead
//!   process's line (as upstream); the next pass to take `LOCK` removes it.
//! * **Old caches.** [`legacy`] cleans the GUI's own cache dir, where fork
//!   releases wrote the same per-project and Pi/OMP caches as upstream; a
//!   pass never cleans a cache dir inside the installed CLI's
//!   ([`crate::history::sources::Registry::cli_cache`]), and a pass given
//!   up skips it.
//!
//! The format, the publish protocol and the collection rules are upstream's.
//! On Windows (NTFS) `std::fs` removes and renames over files that another
//! handle maps (POSIX semantics), so collection behaves as on Unix and an
//! open map stays readable; only writing or truncating a mapped file fails
//! (`ERROR_USER_MAPPED_FILE`), and published files (manifest, text and semantic
//! segments, index segments) are never written again. NTFS hard-links the unchanged
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
pub mod legacy;
pub mod lexindex;
pub mod progress;
pub mod reconcile;
pub mod search;
pub mod segments;
pub mod semantic;
pub mod text;

pub use generation::{Generation, ProjectView};

use crate::env::HistoryEnv;
use crate::history::{Conversation, Workspace};
use crate::time_filter::TimeFilter;
use chrono::{DateTime, Local};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Store root: `v3/roots-<hash>/` beside the other caches. The cache dir is
/// namespaced by `CLAUDE_CONFIG_DIR` ([`HistoryEnv::cache_base_dir`]),
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
    env.cache_base_dir()
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

/// What `load_conversations` reads besides the cards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Contents {
    /// Only what the cards hold, as the list shows them.
    Cards,
    /// The cards with their full text and semantic data, as the parser
    /// produces them.
    Full,
}

/// Every conversation, newest first, from the store: a reconcile pass first
/// brings the current generation up to date with the session files (or
/// waits for the process doing so), then its cards are read with
/// `contents`. Without a cache dir, or when the store cannot be read, the
/// session files are parsed instead. Fork: each pass's outcome is a
/// `debug_level` message (`Reconcile: {outcome:?}` at Debug), so the GUI's
/// Debug log level still shows something of a load from the store.
pub fn load_conversations(
    env: &HistoryEnv,
    show_last: bool,
    debug_level: Option<crate::debug::DebugLevel>,
    contents: Contents,
) -> crate::error::Result<Vec<Conversation>> {
    load_conversations_where(env, show_last, debug_level, contents, &|_| true)
}

/// The conversations of [`load_conversations`] whose card `keep` accepts.
/// Contents beyond the card are read only for those; a card whose contents
/// cannot be read still fails the whole load.
pub fn load_conversations_where(
    env: &HistoryEnv,
    show_last: bool,
    debug_level: Option<crate::debug::DebugLevel>,
    contents: Contents,
    keep: &(dyn Fn(&Conversation) -> bool + Sync),
) -> crate::error::Result<Vec<Conversation>> {
    load(env, show_last, debug_level, contents, None, keep)
}

/// [`load_conversations`] that gives up soon after `cancelled()` returns
/// true, with [`generation::cancelled_build`] and without parsing the
/// session files instead (fork: agent search, whose jobs the GUI cancels,
/// may wait for the list's own pass). Its pass is
/// [`reconcile::reconcile_cancellable`], it waits for `LOCK` with
/// [`generation::PublishLock::wait_cancellable`], and it stops reading
/// cards once cancelled.
pub fn load_conversations_cancellable(
    env: &HistoryEnv,
    show_last: bool,
    debug_level: Option<crate::debug::DebugLevel>,
    contents: Contents,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> crate::error::Result<Vec<Conversation>> {
    load_conversations_where_cancellable(env, show_last, debug_level, contents, cancelled, &|_| {
        true
    })
}

/// [`load_conversations_where`] that is cancellable like
/// [`load_conversations_cancellable`].
pub fn load_conversations_where_cancellable(
    env: &HistoryEnv,
    show_last: bool,
    debug_level: Option<crate::debug::DebugLevel>,
    contents: Contents,
    cancelled: &(dyn Fn() -> bool + Sync),
    keep: &(dyn Fn(&Conversation) -> bool + Sync),
) -> crate::error::Result<Vec<Conversation>> {
    load(env, show_last, debug_level, contents, Some(cancelled), keep)
}

fn load(
    env: &HistoryEnv,
    show_last: bool,
    debug_level: Option<crate::debug::DebugLevel>,
    contents: Contents,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    keep: &(dyn Fn(&Conversation) -> bool + Sync),
) -> crate::error::Result<Vec<Conversation>> {
    if let Some(conversations) = default_root(env).and_then(|root| {
        load_from_store(
            env,
            &root,
            show_last,
            debug_level,
            contents,
            cancelled,
            keep,
        )
    }) {
        return Ok(conversations);
    }
    if cancelled.is_some_and(|cancelled| cancelled()) {
        return Err(generation::cancelled_build().into());
    }
    let mut conversations = load_in_memory(env, show_last)?;
    conversations.retain(|conversation| keep(conversation));
    Ok(conversations)
}

/// Every conversation the environment's instances hold, parsed in memory
/// and newest first: the corpus of a run without a store. Fails when no
/// instance has a history to read.
pub fn load_in_memory(
    env: &HistoryEnv,
    show_last: bool,
) -> crate::error::Result<Vec<Conversation>> {
    let sources = crate::history::sources::Registry::from_env(env);
    if !sources
        .adapters()
        .iter()
        .any(|adapter| adapter.instance().root.exists())
    {
        let root = crate::history::get_claude_projects_root(env)?;
        return Err(crate::error::AppError::ProjectsDirNotFound(
            root.display().to_string(),
        ));
    }
    let mut conversations = reconcile::parse_everything(&sources);
    conversations.sort_by_key(|conversation| std::cmp::Reverse(conversation.timestamp));
    for (index, conversation) in conversations.iter_mut().enumerate() {
        conversation.index = index;
        conversation.preview = if show_last {
            conversation.preview_last.clone()
        } else {
            conversation.preview_first.clone()
        };
    }
    Ok(conversations)
}

fn load_from_store(
    env: &HistoryEnv,
    root: &Path,
    show_last: bool,
    debug_level: Option<crate::debug::DebugLevel>,
    contents: Contents,
    cancellable: Option<&(dyn Fn() -> bool + Sync)>,
    keep: &(dyn Fn(&Conversation) -> bool + Sync),
) -> Option<Vec<Conversation>> {
    let never = || false;
    let cancelled = cancellable.unwrap_or(&never);
    let sources = crate::history::sources::Registry::from_env(env);
    for _ in 0..2 {
        // `reconcile::reconcile` is this pass never cancelled.
        let outcome = reconcile::reconcile_cancellable(
            root,
            &sources,
            generation::MergePolicy::DEFAULT,
            cancelled,
        )
        .ok()?;
        crate::debug::debug(debug_level, &format!("Reconcile: {outcome:?}"));
        if outcome.publish != generation::Publish::Busy {
            break;
        }
        let free = match cancellable {
            None => generation::PublishLock::wait(root).map(|()| true),
            Some(cancelled) => generation::PublishLock::wait_cancellable(root, cancelled),
        };
        if !free.ok()? {
            return None;
        }
    }
    let cards = Snapshot::open(root, show_last, TimeFilter::default())?;
    read_conversations(&cards, contents, cancelled, keep)
}

/// Every card of `cards` that `keep` accepts, with `contents`, in doc
/// order; `None` once `cancelled()` (fork).
fn read_conversations(
    cards: &Snapshot,
    contents: Contents,
    cancelled: &(dyn Fn() -> bool + Sync),
    keep: &(dyn Fn(&Conversation) -> bool + Sync),
) -> Option<Vec<Conversation>> {
    let generation = &cards.generation;
    let files = match contents {
        Contents::Cards => None,
        Contents::Full => Some((
            generation.open_text_files()?,
            generation.open_own_files()?,
            generation.open_semantic_files()?,
        )),
    };
    // A card whose contents cannot be read fails the whole load.
    let read: Option<Vec<Option<Conversation>>> = (0..generation.rows().len() as u32)
        .into_par_iter()
        .map(|doc| {
            if cancelled() {
                return None;
            }
            let mut conversation = cards.card(doc);
            if !keep(&conversation) {
                return Some(None);
            }
            if let Some((text, own, semantic)) = &files {
                let row = &generation.rows()[doc as usize];
                conversation.full_text = text.read(row.text)?.into();
                conversation.own_text = own.read(generation.owns()[doc as usize].blob)?.into();
                let sem = &generation.sems()[doc as usize];
                if sem.hash != 0 {
                    semantic::SemanticDoc::decode(&semantic.read_bytes(sem.blob)?)?
                        .apply(&mut conversation);
                }
            }
            Some(Some(conversation))
        })
        .collect();
    Some(read?.into_iter().flatten().collect())
}

/// Cards of one generation.
#[derive(Clone)]
pub struct Snapshot {
    generation: Arc<Generation>,
    show_last: bool,
    time: TimeFilter,
    /// Doc of each `CardRow::key_hash`, built on first lookup.
    by_key: Arc<OnceLock<HashMap<u64, u32>>>,
}

impl Snapshot {
    /// Opens the current generation under `root`; `None` when there is no
    /// usable one.
    pub fn open(root: &Path, show_last: bool, time: TimeFilter) -> Option<Self> {
        Some(Self {
            generation: Arc::new(Generation::open(root)?),
            show_last,
            time,
            by_key: Arc::default(),
        })
    }

    pub fn show_last(&self) -> bool {
        self.show_last
    }

    /// Whether `timestamp` is inside the time filter.
    pub fn in_time(&self, timestamp: DateTime<Local>) -> bool {
        !self.time.is_active() || self.time.matches(timestamp)
    }

    /// `CardRow::key_hash` of card `doc`.
    pub fn key_hash(&self, doc: u32) -> u64 {
        self.generation.rows()[doc as usize].key_hash
    }

    /// Doc of every card by `CardRow::key_hash`.
    pub fn docs_by_key(&self) -> &HashMap<u64, u32> {
        self.by_key.get_or_init(|| {
            self.generation
                .rows()
                .iter()
                .enumerate()
                .map(|(doc, row)| (row.key_hash, doc as u32))
                .collect()
        })
    }

    /// Whether a card has `CardRow::key_hash` `key`.
    pub fn contains_key(&self, key: u64) -> bool {
        self.docs_by_key().contains_key(&key)
    }

    /// Doc of the card stored at `path`.
    pub fn find(&self, path: &Path) -> Option<u32> {
        let path = path.to_str()?;
        let doc = *self.docs_by_key().get(&generation::key_hash(path))?;
        let row = &self.generation.rows()[doc as usize];
        (self.generation.str(row.path) == path).then_some(doc)
    }

    /// Doc of the card whose session id is `session_id`, ignoring case.
    pub fn find_session(&self, session_id: &str) -> Option<u32> {
        self.generation
            .rows()
            .iter()
            .position(|row| {
                self.generation
                    .str(row.session_id)
                    .eq_ignore_ascii_case(session_id)
            })
            .map(|doc| doc as u32)
    }

    /// Doc of the card of the conversation `child` names as its parent; see
    /// `history::is_parent_of`.
    pub fn parent_of(&self, child: &Conversation) -> Option<u32> {
        let parent = child.parent.as_deref()?;
        let source = card::source_to_u8(child.source);
        self.generation
            .rows()
            .iter()
            .position(|row| {
                row.source == source
                    && self.generation.str(row.session_id) == parent
                    && crate::history::parent_in_place(
                        child.source,
                        Path::new(self.generation.str(row.path)),
                        &child.path,
                    )
            })
            .map(|doc| doc as u32)
    }

    /// Docs of the cards of the conversations naming `parent` as theirs,
    /// oldest first.
    pub fn children_of(&self, parent: &Conversation) -> Vec<u32> {
        let source = card::source_to_u8(parent.source);
        let mut docs: Vec<u32> = self
            .generation
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.source == source
                    && row.parent.len > 0
                    && self.generation.str(row.parent) == parent.session_id
                    && crate::history::parent_in_place(
                        parent.source,
                        &parent.path,
                        Path::new(self.generation.str(row.path)),
                    )
            })
            .map(|(doc, _)| doc as u32)
            .collect();
        docs.reverse();
        docs
    }

    /// Whether the cards come from more than one source.
    pub fn has_multiple_sources(&self) -> bool {
        let mut rows = self.generation.rows().iter();
        rows.next()
            .is_some_and(|first| rows.any(|row| row.source != first.source))
    }

    pub fn generation_name(&self) -> &str {
        self.generation.name()
    }

    /// Identifies the generation across store roots (its directory; the
    /// name alone repeats from root to root).
    pub fn generation_id(&self) -> String {
        self.generation.dir().display().to_string()
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

    /// Ids of the newest `n` listed cards inside the time filter whose
    /// project passes `project_visible`, whose cwd starts with none of
    /// `excluded_paths` and that belong to `workspace`, if given.
    /// `all_kinds` lists automated and child conversations too.
    pub fn head(
        &self,
        n: usize,
        all_kinds: bool,
        excluded_paths: &[String],
        workspace: Option<&Workspace>,
        project_visible: impl FnMut(&ProjectView) -> bool,
    ) -> Vec<u32> {
        let in_scope = self.in_scope(all_kinds, excluded_paths, workspace, project_visible);
        self.generation.head(n, in_scope)
    }

    /// For every card, whether `head` would list it.
    pub fn scope(
        &self,
        all_kinds: bool,
        excluded_paths: &[String],
        workspace: Option<&Workspace>,
        project_visible: impl FnMut(&ProjectView) -> bool,
    ) -> Vec<bool> {
        let in_scope = self.in_scope(all_kinds, excluded_paths, workspace, project_visible);
        self.generation.rows().iter().map(in_scope).collect()
    }

    /// The card test `head` and `scope` apply.
    fn in_scope<'a>(
        &'a self,
        all_kinds: bool,
        excluded_paths: &'a [String],
        workspace: Option<&'a Workspace>,
        project_visible: impl FnMut(&ProjectView) -> bool,
    ) -> impl Fn(&card::CardRow) -> bool + 'a {
        let visible = self.project_visibility(project_visible);
        let in_workspace = workspace.map(|workspace| {
            self.project_visibility(|project| {
                workspace.contains_project_key(project.workspace_key, project.key_is_path)
            })
        });
        move |row| {
            listed(row, all_kinds)
                && visible[row.project_id as usize]
                && in_time(&self.time, row)
                && !self.cwd_excluded(row, excluded_paths)
                && workspace
                    .zip(in_workspace.as_ref())
                    .is_none_or(|(workspace, projects)| {
                        projects[row.project_id as usize]
                            || self
                                .generation
                                .cwds(row)
                                .any(|cwd| workspace.contains_cwd(Path::new(cwd)))
                    })
        }
    }

    /// Whether the card records a cwd starting with one of `prefixes`.
    fn cwd_excluded(&self, row: &card::CardRow, prefixes: &[String]) -> bool {
        !prefixes.is_empty()
            && row.flags & card::card_flags::CWD != 0
            && crate::history::path::cwd_is_excluded(
                Some(Path::new(self.generation.str(row.cwd))),
                prefixes,
            )
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

    /// This snapshot under the time filter `time` (fork: the GUI changes the
    /// filter at runtime). The generation stays mapped once.
    pub fn with_time_filter(&self, time: TimeFilter) -> Self {
        Self {
            generation: self.generation.clone(),
            show_last: self.show_last,
            time,
            by_key: self.by_key.clone(),
        }
    }

    /// The time filter the snapshot applies (fork).
    pub fn time_filter(&self) -> &TimeFilter {
        &self.time
    }
}

/// Whether the list shows the card: automated and child conversations stay
/// out of it unless `all_kinds`.
fn listed(row: &card::CardRow, all_kinds: bool) -> bool {
    all_kinds || row.flags & card::card_flags::HIDDEN == 0
}

fn in_time(time: &TimeFilter, row: &card::CardRow) -> bool {
    !time.is_active()
        || chrono::DateTime::from_timestamp_millis(row.timestamp_ms)
            .is_some_and(|timestamp| time.matches(timestamp))
}

/// A generation's cards, suffix indexes and texts, ready for lexical search.
pub struct SearchIndex {
    cards: Snapshot,
    index: segments::IndexSet,
    text: text::TextSegments,
    docs: search::SearchDocs,
    own: OwnText,
}

/// The index and text of what the person typed, searched without card
/// fields.
struct OwnText {
    index: segments::IndexSet,
    text: text::TextSegments,
    docs: search::SearchDocs,
    generation: Arc<Generation>,
}

impl SearchIndex {
    /// Opens the indexes and texts of the generation behind `cards`; `None`
    /// when it has none usable.
    pub fn open(cards: Snapshot) -> Option<Self> {
        let (index, text) = cards.generation.open_search_files(lexindex::Field::Body)?;
        let (own_index, own_text) = cards.generation.open_search_files(lexindex::Field::Own)?;
        let docs = search::SearchDocs::from_generation(&cards.generation);
        Some(Self {
            own: OwnText {
                index: own_index,
                text: own_text,
                docs: docs.without_fields(),
                generation: cards.generation.clone(),
            },
            docs,
            cards,
            index,
            text,
        })
    }

    pub fn cards(&self) -> &Snapshot {
        &self.cards
    }

    /// Starts `query` over the documents for which `scope` holds. With
    /// `user_only`, a document is its own text alone (`Conversation::
    /// own_text`): every word and literal must occur there, and title,
    /// summary and project neither match nor score.
    pub fn search<'a>(
        &'a self,
        query: &str,
        now: DateTime<Local>,
        scope: &(dyn Fn(u32) -> bool + Sync),
        user_only: bool,
    ) -> search::Ranking<'a> {
        if user_only {
            let own = &self.own;
            search::search(&own.index, &own.docs, own, query, now, scope)
        } else {
            search::search(&self.index, &self.docs, self, query, now, scope)
        }
    }

    /// `Conversation::full_text` of card `doc`, decompressed from its frame.
    pub fn text(&self, doc: u32) -> String {
        let row = &self.cards.generation.rows()[doc as usize];
        self.text.read(row.text).unwrap_or_default()
    }

    /// `Conversation::own_text` of card `doc`, decompressed from its frame.
    pub fn own_text(&self, doc: u32) -> String {
        self.own.read(doc)
    }
}

impl OwnText {
    fn read(&self, doc: u32) -> String {
        let row = &self.generation.owns()[doc as usize];
        self.text.read(row.blob).unwrap_or_default()
    }
}

impl search::TextSource for OwnText {
    fn full_text(&self, doc: u32) -> Cow<'_, str> {
        Cow::Owned(self.read(doc))
    }
}

/// Every card of a generation, in doc order, as a conversation carrying its
/// semantic data, for semantic search.
pub struct SemanticCards {
    generation: String,
    /// [`Snapshot::generation_id`] of the generation (fork).
    id: String,
    conversations: Arc<Vec<Arc<Conversation>>>,
    /// `(CardRow::key_hash, SemRow::hash)` of each card.
    hashes: Vec<(u64, u64)>,
}

impl SemanticCards {
    /// Reads the semantic data of every card of `cards`. Data a card shares
    /// with the same card in `previous` is reused rather than read, so equal
    /// turns stay the same allocation. `None` when the generation's semantic
    /// segments are missing or invalid.
    pub fn load(cards: &Snapshot, previous: Option<&SemanticCards>) -> Option<Self> {
        Self::load_cancellable(cards, previous, &|| false)
    }

    /// [`SemanticCards::load`] that gives up, with `None`, soon after
    /// `cancelled` returns true (fork: the browser waits for the thread
    /// that reads the cards when it is dropped).
    pub fn load_cancellable(
        cards: &Snapshot,
        previous: Option<&SemanticCards>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Option<Self> {
        let generation = &cards.generation;
        let segments = generation.open_semantic_files()?;
        let reusable: HashMap<u64, (u64, &Arc<Conversation>)> = previous
            .map(|previous| {
                previous
                    .hashes
                    .iter()
                    .zip(previous.conversations.iter())
                    .map(|(&(key, sem), conversation)| (key, (sem, conversation)))
                    .collect()
            })
            .unwrap_or_default();
        let hashes: Vec<(u64, u64)> = generation
            .rows()
            .iter()
            .zip(generation.sems())
            .map(|(row, sem)| (row.key_hash, sem.hash))
            .collect();
        let conversations = (0..hashes.len())
            .into_par_iter()
            .map(|doc| {
                if cancelled() {
                    return None;
                }
                let mut card = cards.card(doc as u32);
                let (key, hash) = hashes[doc];
                match reusable.get(&key) {
                    Some(&(previous_hash, previous)) if previous_hash == hash => {
                        card.semantic_turns = previous.semantic_turns.clone();
                        card.semantic_turn_ranges = previous.semantic_turn_ranges.clone();
                        card.semantic_route_text = previous.semantic_route_text.clone();
                        card.agent_search_text = previous.agent_search_text.clone();
                    }
                    _ if hash == 0 => {}
                    _ => {
                        let bytes = segments.read_bytes(generation.sems()[doc].blob)?;
                        semantic::SemanticDoc::decode(&bytes)?.apply(&mut card);
                    }
                }
                Some(Arc::new(card))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            generation: generation.name().to_owned(),
            id: cards.generation_id(),
            conversations: Arc::new(conversations),
            hashes,
        })
    }

    /// Cards named as generation `generation`, without one on disk (its id
    /// is the name too).
    #[cfg(test)]
    pub fn from_conversations(generation: &str, conversations: Vec<Conversation>) -> Self {
        Self {
            generation: generation.to_owned(),
            id: generation.to_owned(),
            hashes: vec![(0, 0); conversations.len()],
            conversations: Arc::new(conversations.into_iter().map(Arc::new).collect()),
        }
    }

    pub fn generation_name(&self) -> &str {
        &self.generation
    }

    /// Identifies the generation across store roots, like
    /// [`Snapshot::generation_id`] (fork).
    pub fn generation_id(&self) -> &str {
        &self.id
    }

    /// Conversation of each card; the index of card `doc` is `doc`.
    pub fn conversations(&self) -> &Arc<Vec<Arc<Conversation>>> {
        &self.conversations
    }
}

impl search::TextSource for SearchIndex {
    fn full_text(&self, doc: u32) -> Cow<'_, str> {
        Cow::Owned(self.text(doc))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::history::Conversation;
    use crate::history::model::SourceKind;
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
            source: SourceKind::Claude,
            session_id,
            path,
            index: 0,
            timestamp: Local.timestamp_millis_opt(timestamp_ms).unwrap(),
            preview: "first".into(),
            preview_first: "first".into(),
            preview_last: "last".into(),
            full_text: "full text".into(),
            own_text: "".into(),
            agent_search_text: empty.clone(),
            semantic_route_text: empty.clone(),
            semantic_turns: Arc::from(Vec::new()),
            semantic_turn_ranges: Vec::new(),
            project_name,
            project_path: None,
            cwd: None,
            cwds: Vec::new(),
            message_count: 3,
            prompt_count: 1,
            tool_call_count: 0,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 42,
            duration_minutes: None,
            provenance: Default::default(),
            parent: None,
        }
    }
}

/// Fork-only: the env-derived root and sources, and the cards a pass over
/// them writes for what the history loader loads.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::sources::Registry;
    use std::fs;

    #[test]
    fn semantic_cards_carry_semantic_data_and_reuse_unchanged_turns() {
        let dir = tempfile::tempdir().unwrap();
        let mut conversations: Vec<_> = (0..3)
            .map(|i| test_support::conversation(&format!("/p/-a/s{i}.jsonl"), 1_000 - i))
            .collect();
        conversations[0].semantic_turns = vec!["zero".to_owned()].into();
        conversations[0].semantic_turn_ranges = vec![crate::agent::refs::MessageRange::single(1)];
        conversations[1].semantic_turns = vec!["one".to_owned()].into();
        conversations[1].semantic_route_text = "route".into();
        conversations[1].agent_search_text = "agent".into();
        generation::publish(dir.path(), &conversations).unwrap();
        let open = || Snapshot::open(dir.path(), false, TimeFilter::default()).unwrap();

        let first = SemanticCards::load(&open(), None).unwrap();
        let cards = first.conversations();
        assert_eq!(cards.len(), 3);
        for (doc, card) in cards.iter().enumerate() {
            assert_eq!(card.index, doc);
            assert_eq!(card.path, conversations[doc].path);
            assert_eq!(card.semantic_turns, conversations[doc].semantic_turns);
            assert_eq!(
                card.semantic_turn_ranges,
                conversations[doc].semantic_turn_ranges
            );
            assert_eq!(
                card.semantic_route_text,
                conversations[doc].semantic_route_text
            );
            assert_eq!(card.agent_search_text, conversations[doc].agent_search_text);
        }

        conversations[0].semantic_turns = vec!["changed".to_owned()].into();
        conversations[1].custom_title = Some("renamed".to_owned());
        generation::publish(dir.path(), &conversations).unwrap();
        let next = SemanticCards::load(&open(), Some(&first)).unwrap();
        assert_ne!(next.generation_name(), first.generation_name());
        let cards = next.conversations();
        assert_eq!(&*cards[0].semantic_turns, ["changed".to_owned()]);
        assert!(Arc::ptr_eq(
            &cards[1].semantic_turns,
            &first.conversations()[1].semantic_turns
        ));
        assert_eq!(cards[1].custom_title.as_deref(), Some("renamed"));
    }

    /// Fork: the id names the generation's directory, and a cancelled read
    /// gives up.
    #[test]
    fn semantic_cards_name_their_generation_and_give_up_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let conversations: Vec<_> = (0..3)
            .map(|i| test_support::conversation(&format!("/p/-a/s{i}.jsonl"), 1_000 - i))
            .collect();
        generation::publish(dir.path(), &conversations).unwrap();
        let snapshot = Snapshot::open(dir.path(), false, TimeFilter::default()).unwrap();

        let cards = SemanticCards::load(&snapshot, None).unwrap();
        assert_eq!(cards.generation_name(), snapshot.generation_name());
        assert_eq!(cards.generation_id(), snapshot.generation_id());
        assert!(SemanticCards::load_cancellable(&snapshot, None, &|| true).is_none());
    }

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

        let sources = Registry::from_env(&env);
        let claude = sources
            .adapters()
            .iter()
            .find(|adapter| adapter.instance().kind == crate::history::model::SourceKind::Claude)
            .unwrap();
        assert_eq!(claude.instance().root, projects);
        let outcome = reconcile::reconcile(&root, &sources).unwrap();
        assert!(
            matches!(outcome.publish, generation::Publish::Published(_)),
            "{outcome:?}"
        );
        assert_eq!((outcome.scanned, outcome.parsed), (3, 3));

        let loaded = load_in_memory(&env, true).unwrap();
        assert_eq!(loaded.len(), 3);
        let snapshot = Snapshot::open(&root, true, TimeFilter::default()).unwrap();
        assert_eq!(snapshot.len(), 3);
        // The list shows cards instead of loaded conversations: each card
        // carries what the loader gives its row.
        for conversation in &loaded {
            let doc = snapshot
                .find(&conversation.path)
                .unwrap_or_else(|| panic!("no card for {:?}", conversation.path));
            let card = snapshot.card(doc);
            assert_eq!(card.source, conversation.source);
            assert_eq!(card.session_id, conversation.session_id);
            assert_eq!(
                card.timestamp.timestamp_millis(),
                conversation.timestamp.timestamp_millis()
            );
            assert_eq!(card.project_path, conversation.project_path);
            assert_eq!(card.project_name, conversation.project_name);
            assert_eq!(card.cwd, conversation.cwd);
            assert_eq!(card.cwds, conversation.cwds);
            assert_eq!(card.preview, conversation.preview);
            assert_eq!(card.message_count, conversation.message_count);
            assert_eq!(card.prompt_count, conversation.prompt_count);
            assert_eq!(card.tool_call_count, conversation.tool_call_count);
            assert_eq!(
                snapshot.find_session(&conversation.session_id.to_uppercase()),
                Some(doc)
            );
        }
        assert!(snapshot.has_multiple_sources(), "Claude and Pi cards");
        let index = SearchIndex::open(snapshot).expect("the pass wrote the index");
        let alpha = (0..3)
            .find(|&doc| index.text(doc).contains("alpha words"))
            .expect("the text of a.jsonl");
        assert_eq!(
            index.cards().card(alpha).path,
            projects.join("-tmp-work").join("a.jsonl")
        );
    }

    /// A sandbox with two Claude sessions, one holding a malformed line,
    /// dated an hour back so their fingerprints are known.
    fn sessions_with_a_parse_error(home: &Path) -> HistoryEnv {
        let env = HistoryEnv::sandbox(home);
        let dir = env.claude_projects_root().unwrap().join("-tmp-work");
        fs::create_dir_all(&dir).unwrap();
        let line = |text: &str, minute: u32| {
            serde_json::json!({
                "type": "user",
                "timestamp": format!("2024-01-01T00:{minute:02}:00Z"),
                "cwd": "/tmp/work",
                "message": {"role": "user", "content": text}
            })
            .to_string()
        };
        for (name, lines) in [
            ("good.jsonl", vec![line("alpha words", 1)]),
            (
                "broken.jsonl",
                vec![line("beta words", 2), "{not json".to_owned()],
            ),
        ] {
            let path = dir.join(name);
            fs::write(&path, lines.join("\n") + "\n").unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
                .unwrap();
        }
        env
    }

    /// Fork: upstream has no test of `load_conversations`. Whole
    /// conversations from the store carry what the parser gives them, parse
    /// errors included; cards only carry what the list shows.
    #[test]
    fn load_conversations_reads_the_store_like_the_loader() {
        let home = tempfile::tempdir().unwrap();
        let env = sessions_with_a_parse_error(home.path());
        let parsed = load_in_memory(&env, false).unwrap();
        assert_eq!(parsed.len(), 2);

        let full = load_conversations(&env, false, None, Contents::Full).unwrap();
        let root = default_root(&env).unwrap();
        assert!(root.join("CURRENT").is_file(), "the pass published");
        assert_eq!(full.len(), 2);
        for conversation in &parsed {
            let stored = full
                .iter()
                .find(|stored| stored.path == conversation.path)
                .unwrap_or_else(|| panic!("no conversation for {:?}", conversation.path));
            assert_eq!(stored.session_id, conversation.session_id);
            assert_eq!(stored.preview, conversation.preview);
            assert_eq!(stored.project_name, conversation.project_name);
            assert_eq!(stored.message_count, conversation.message_count);
            assert_eq!(stored.prompt_count, conversation.prompt_count);
            assert_eq!(stored.tool_call_count, conversation.tool_call_count);
            assert_eq!(stored.full_text, conversation.full_text);
            assert_eq!(stored.agent_search_text, conversation.agent_search_text);
            assert_eq!(stored.semantic_route_text, conversation.semantic_route_text);
            assert_eq!(stored.semantic_turns, conversation.semantic_turns);
            assert_eq!(stored.parse_errors, conversation.parse_errors);
        }
        assert!(
            full.iter()
                .any(|conversation| conversation.parse_errors.len() == 1)
        );

        let cards = load_conversations(&env, false, None, Contents::Cards).unwrap();
        assert_eq!(cards.len(), 2);
        assert!(cards.iter().all(|card| card.full_text.is_empty()
            && card.parse_errors.is_empty()
            && !card.preview.is_empty()));
    }

    /// Fork (upstream d05e135 has no test): `keep` drops cards by their
    /// card alone, from the store and from the in-memory fallback.
    #[test]
    fn load_conversations_where_keeps_only_accepted_cards() {
        let home = tempfile::tempdir().unwrap();
        let env = sessions_with_a_parse_error(home.path());
        let all = load_conversations(&env, false, None, Contents::Full).unwrap();
        assert_eq!(all.len(), 2);
        let wanted = all[0].path.clone();
        let kept = load_conversations_where(&env, false, None, Contents::Full, &|card| {
            card.path == wanted
        })
        .unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, wanted);
        assert!(!kept[0].full_text.is_empty());
        let none = load_conversations_where(&env, false, None, Contents::Full, &|_| false).unwrap();
        assert!(none.is_empty());
    }

    /// Fork: the pass another thread of the process runs (the list's
    /// `store-reconcile` thread) holds `LOCK`; loading waits for it, then
    /// runs its own pass.
    #[test]
    fn load_conversations_waits_for_a_pass_another_thread_holds() {
        let home = tempfile::tempdir().unwrap();
        let env = sessions_with_a_parse_error(home.path());
        let root = default_root(&env).unwrap();
        let lock = generation::PublishLock::try_acquire(&root)
            .unwrap()
            .expect("the lock is free");
        let loading = {
            let env = env.clone();
            std::thread::spawn(move || load_conversations(&env, false, None, Contents::Full))
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !loading.is_finished(),
            "loading waits while the lock is held"
        );
        assert!(!root.join("CURRENT").exists());
        drop(lock);
        let loaded = loading.join().unwrap().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(root.join("CURRENT").is_file(), "the second pass published");
    }

    /// Fork: a cancellable load gives up its wait for `LOCK` once
    /// cancelled, without parsing the session files instead; the wait itself
    /// answers whether the lock came free.
    #[test]
    fn a_cancelled_load_stops_waiting_for_the_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let home = tempfile::tempdir().unwrap();
        let env = sessions_with_a_parse_error(home.path());
        let root = default_root(&env).unwrap();
        let lock = generation::PublishLock::try_acquire(&root)
            .unwrap()
            .expect("the lock is free");
        assert!(!generation::PublishLock::wait_cancellable(&root, &|| true).unwrap());

        let cancel = Arc::new(AtomicBool::new(false));
        let loading = {
            let (env, cancel) = (env.clone(), cancel.clone());
            std::thread::spawn(move || {
                load_conversations_cancellable(&env, false, None, Contents::Full, &|| {
                    cancel.load(Ordering::SeqCst)
                })
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !loading.is_finished(),
            "loading waits while the lock is held"
        );
        cancel.store(true, Ordering::SeqCst);
        // An error rather than the parsed session files: no fallback parse.
        let error = loading.join().unwrap().unwrap_err();
        assert!(
            matches!(&error, crate::error::AppError::Io(error)
                if error.kind() == std::io::ErrorKind::Interrupted),
            "{error:?}"
        );

        drop(lock);
        assert!(generation::PublishLock::wait_cancellable(&root, &|| false).unwrap());
        let loaded =
            load_conversations_cancellable(&env, false, None, Contents::Cards, &|| false).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    /// Without a usable store (here its cache root is a file), the session
    /// files are parsed.
    #[test]
    fn load_conversations_parses_the_files_when_the_store_cannot_be_used() {
        let home = tempfile::tempdir().unwrap();
        let blocked = home.path().join("not-a-dir");
        fs::write(&blocked, "").unwrap();
        let env = sessions_with_a_parse_error(home.path()).with_cache_root(&blocked);
        assert!(default_root(&env).unwrap().starts_with(&blocked));

        let loaded = load_conversations(&env, false, None, Contents::Cards).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            loaded
                .iter()
                .all(|conversation| !conversation.full_text.is_empty()),
            "parsed, not read from cards"
        );
    }

    /// Writes the caches fork releases before the store kept in `cache`:
    /// one Claude project cache and one Pi session-root cache.
    fn old_caches(cache: &Path) -> [PathBuf; 2] {
        let project = cache.join("projects").join("-tmp-work.bin");
        let pi = cache
            .join("pi")
            .join("root-0123456789abcdef")
            .join("sessions.bin");
        for (path, magic) in [(&project, b"CLHIST01"), (&pi, b"PIHIST01")] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, magic).unwrap();
        }
        [project, pi]
    }

    /// Fork (upstream 75df2d4 tests `legacy` alone): a pass over the env's
    /// store root that publishes removes the old caches of its cache dir,
    /// and a pass that publishes nothing leaves them.
    #[test]
    fn a_publishing_pass_removes_the_old_caches_of_its_cache_dir() {
        let home = tempfile::tempdir().unwrap();
        let env = sessions_with_a_parse_error(home.path());
        let cache = env.cache_root().unwrap();
        let [project, pi] = old_caches(&cache);
        let launch = cache.join("launch").join("resume-1.command");
        fs::create_dir_all(launch.parent().unwrap()).unwrap();
        fs::write(&launch, "x").unwrap();
        let root = default_root(&env).unwrap();
        let sources = Registry::from_env(&env);

        let outcome = reconcile::reconcile(&root, &sources).unwrap();
        assert!(outcome.published > 0, "{outcome:?}");
        assert_eq!(
            outcome.removed,
            legacy::Removed {
                files: 2,
                bytes: 16,
                dirs: 3
            }
        );
        assert!(!project.exists() && !pi.exists());
        assert!(!cache.join("projects").exists() && !cache.join("pi").exists());
        assert!(launch.exists(), "the launcher's scripts stay");
        assert!(root.join("CURRENT").is_file());

        let [project, pi] = old_caches(&cache);
        let outcome = reconcile::reconcile(&root, &sources).unwrap();
        assert_eq!(outcome.published, 0, "{outcome:?}");
        assert_eq!(outcome.removed, legacy::Removed::default());
        assert!(project.exists() && pi.exists());
    }

    /// Fork: a pass never removes old caches from the installed CLI's cache
    /// dir, even when the env's cache root points there.
    #[test]
    fn a_pass_keeps_the_old_caches_of_the_cli_cache_dir() {
        let home = tempfile::tempdir().unwrap();
        let cli_cache = home.path().join(".cache").join("claude-history");
        let env = sessions_with_a_parse_error(home.path()).with_cache_root(&cli_cache);
        assert_eq!(env.cli_cache_root().unwrap(), cli_cache);
        let [project, pi] = old_caches(&cli_cache);
        let root = default_root(&env).unwrap();
        assert_eq!(legacy::cache_dir_of(&root), Some(cli_cache.as_path()));

        let outcome = reconcile::reconcile(&root, &Registry::from_env(&env)).unwrap();
        assert!(outcome.published > 0, "{outcome:?}");
        assert_eq!(outcome.removed, legacy::Removed::default());
        assert!(project.exists() && pi.exists());
    }
}
