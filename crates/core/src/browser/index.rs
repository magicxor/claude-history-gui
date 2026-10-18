//! Lookup data derived from the browser's corpus (CCR-1, CCR-2): corpus
//! positions by path and by session id, how many conversations each source
//! has, and the rows in scope (exclusions and workspace).
//!
//! The data is built with the corpus, off the UI thread when a
//! [`super::HistoryLoader`] prepares a load or refresh
//! ([`super::PreparedCorpus`]), and kept in step with every edit of the
//! corpus. A rename, a delete, a UUID jump or the install of a prepared
//! corpus therefore never scans, hashes or filters the whole corpus on the
//! caller's thread: lookups hash one key, and an edit moves at most the
//! corpus's pointers and row numbers.

use crate::env::{WorkspaceScope, normalize_windows_path};
use crate::history::{Conversation, Source};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The 64-bit key of a path: a hash of its file name. Paths that are equal
/// (`Path`'s equality ignores repeated separators, `.` components and, on
/// Windows, the separator kind) have equal file names, so equal paths get
/// equal keys; copies of one session in several projects share a key and
/// are told apart by the comparison every lookup makes. Hashing only the
/// file name is several times cheaper than hashing every component through
/// `Path`'s `Hash` (about 1 µs per path on Windows).
pub(super) fn path_key(path: &Path) -> u64 {
    hash_bytes(
        path.file_name()
            .map_or(&[][..], |name| name.as_encoded_bytes()),
    )
}

/// The 64-bit key of a session id, ignoring ASCII case (the UUID jump
/// matches ids that way).
pub(super) fn session_key(session_id: &str) -> u64 {
    let mut buffer = [0u8; 64];
    match buffer.get_mut(..session_id.len()) {
        Some(lower) => {
            lower.copy_from_slice(session_id.as_bytes());
            lower.make_ascii_lowercase();
            hash_bytes(lower)
        }
        None => hash_bytes(session_id.to_ascii_lowercase().as_bytes()),
    }
}

/// A fast 64-bit hash of a byte string: FxHash-style word mixing with the
/// murmur3 finalizer, so both the low bits (bucket) and the high bits
/// (control byte) of the result are well mixed. The keys never leave the
/// process and every lookup verifies its element, so speed matters more
/// than resistance to crafted collisions.
fn hash_bytes(bytes: &[u8]) -> u64 {
    const SEED: u64 = 0x517c_c1b7_2722_0a95;
    let mut hash = bytes.len() as u64;
    let (words, rest) = bytes.as_chunks::<8>();
    for word in words {
        hash = (hash.rotate_left(5) ^ u64::from_le_bytes(*word)).wrapping_mul(SEED);
    }
    if !rest.is_empty() {
        let mut word = [0u8; 8];
        word[..rest.len()].copy_from_slice(rest);
        hash = (hash.rotate_left(5) ^ u64::from_le_bytes(word)).wrapping_mul(SEED);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    hash ^ (hash >> 33)
}

/// Hasher of keys that already are hashes ([`path_key`], [`session_key`]).
#[derive(Default)]
pub(super) struct PrehashedKey(u64);

impl Hasher for PrehashedKey {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Only `write_u64` is used for the u64 keys; fold anything else.
        for &byte in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(byte);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

/// The positions sharing one key, ascending.
#[derive(Clone, Debug)]
enum Slot {
    One(usize),
    Many(Vec<usize>),
}

impl Slot {
    fn positions(&self) -> &[usize] {
        match self {
            Slot::One(position) => std::slice::from_ref(position),
            Slot::Many(positions) => positions,
        }
    }

    fn positions_mut(&mut self) -> &mut [usize] {
        match self {
            Slot::One(position) => std::slice::from_mut(position),
            Slot::Many(positions) => positions,
        }
    }

    fn insert(&mut self, position: usize) {
        match self {
            Slot::One(existing) => {
                let existing = *existing;
                if existing != position {
                    *self = Slot::Many(if existing < position {
                        vec![existing, position]
                    } else {
                        vec![position, existing]
                    });
                }
            }
            Slot::Many(positions) => {
                if let Err(at) = positions.binary_search(&position) {
                    positions.insert(at, position);
                }
            }
        }
    }

    /// Remove `position`; returns whether the slot is now empty.
    fn remove(&mut self, position: usize) -> bool {
        match self {
            Slot::One(existing) => *existing == position,
            Slot::Many(positions) => {
                if let Ok(at) = positions.binary_search(&position) {
                    positions.remove(at);
                }
                match positions.as_slice() {
                    [] => true,
                    [single] => {
                        *self = Slot::One(*single);
                        false
                    }
                    _ => false,
                }
            }
        }
    }
}

/// Corpus positions by a 64-bit key. Callers verify the element at a
/// position (a hash collision then only costs one comparison).
#[derive(Clone, Debug, Default)]
pub(super) struct KeyIndex {
    map: HashMap<u64, Slot, BuildHasherDefault<PrehashedKey>>,
}

impl KeyIndex {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            map: HashMap::with_capacity_and_hasher(capacity, Default::default()),
        }
    }

    /// The positions with this key, ascending.
    fn positions(&self, key: u64) -> &[usize] {
        self.map.get(&key).map_or(&[], Slot::positions)
    }

    fn insert(&mut self, key: u64, position: usize) {
        match self.map.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(Slot::One(position));
            }
            Entry::Occupied(mut entry) => entry.get_mut().insert(position),
        }
    }

    fn remove(&mut self, key: u64, position: usize) {
        if let Entry::Occupied(mut entry) = self.map.entry(key)
            && entry.get_mut().remove(position)
        {
            entry.remove();
        }
    }

    /// Close the gaps of `removed` (ascending positions that are no longer
    /// in the index): every later position moves down. No hashing.
    fn shift(&mut self, removed: &[usize]) {
        let Some(&first) = removed.first() else {
            return;
        };
        for slot in self.map.values_mut() {
            for position in slot.positions_mut() {
                if *position > first {
                    *position -= removed.partition_point(|&gone| gone < *position);
                }
            }
        }
    }
}

/// How many conversations of each source the corpus holds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SourceCounts([usize; 3]);

impl SourceCounts {
    fn slot(source: Source) -> usize {
        match source {
            Source::Claude => 0,
            Source::Pi => 1,
            Source::Omp => 2,
        }
    }

    fn add(&mut self, source: Source) {
        self.0[Self::slot(source)] += 1;
    }

    fn remove(&mut self, source: Source) {
        let count = &mut self.0[Self::slot(source)];
        *count = count.saturating_sub(1);
    }

    /// Whether more than one source has conversations.
    pub(super) fn mixed(&self) -> bool {
        self.0.iter().filter(|&&count| count > 0).count() > 1
    }
}

/// What decides whether a row is in scope: the excluded project names and,
/// when the workspace scope is on, the workspace.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ScopeSettings {
    pub(super) excluded: HashSet<String>,
    pub(super) workspace: Option<WorkspaceScope>,
}

/// The scope test of one conversation (upstream
/// `filter_conversation_indices`, one row at a time).
///
/// * A conversation is excluded when its project name is in the excluded
///   names, or is a worktree `parent/branch` whose parent is. Conversations
///   without a project name are never excluded.
/// * With a workspace, Claude conversations are kept when their project
///   directory is the same project as the workspace (worktree aware);
///   Pi/OMP conversations when their project path (or cwd) is the workspace
///   directory. Upstream compared with the process cwd.
pub(super) struct ScopeFilter<'a> {
    excluded: &'a HashSet<String>,
    workspace: Option<&'a WorkspaceScope>,
    /// The workspace canonicalized once (upstream: once per row).
    workspace_dir: Option<PathBuf>,
}

impl<'a> ScopeFilter<'a> {
    pub(super) fn new(
        excluded: &'a HashSet<String>,
        workspace: Option<&'a WorkspaceScope>,
    ) -> Self {
        Self {
            excluded,
            workspace,
            workspace_dir: None,
        }
    }

    pub(super) fn passes(&mut self, conversation: &Conversation) -> bool {
        if conversation
            .project_name
            .as_deref()
            .is_some_and(|name| project_is_excluded(name, self.excluded))
        {
            return false;
        }
        let Some(workspace) = self.workspace else {
            return true;
        };
        if conversation.source != Source::Claude {
            let current = self
                .workspace_dir
                .get_or_insert_with(|| comparable_dir(&workspace.canonical_dir()));
            return conversation
                .project_path
                .as_ref()
                .or(conversation.cwd.as_ref())
                .is_some_and(|path| comparable_canonical(path) == *current);
        }
        conversation
            .path
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| {
                crate::history::path::is_same_project(
                    &name.to_string_lossy(),
                    workspace.project_dir_name(),
                )
            })
    }
}

/// `path` canonicalized when possible, in comparable form.
fn comparable_canonical(path: &Path) -> PathBuf {
    comparable_dir(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

/// Windows: compare `C:\x` and `\\?\C:\x` (canonical) equally (W1). A no-op
/// for paths without a drive or verbatim prefix.
fn comparable_dir(path: &Path) -> PathBuf {
    normalize_windows_path(path)
}

fn project_is_excluded(project_name: &str, excluded_projects: &HashSet<String>) -> bool {
    excluded_projects.contains(project_name)
        || project_name
            .split_once('/')
            .is_some_and(|(parent, _)| excluded_projects.contains(parent))
}

/// The rows in scope for one corpus and one [`ScopeSettings`]: ascending
/// positions plus a membership flag per position.
#[derive(Clone, Debug, Default)]
pub(super) struct ScopeIndex {
    /// False until computed, and after an edit it could not follow.
    valid: bool,
    settings: ScopeSettings,
    rows: Arc<Vec<usize>>,
    member: Vec<bool>,
}

impl ScopeIndex {
    /// Evaluate every conversation (O(n); off the UI thread for loads).
    pub(super) fn compute(conversations: &[Arc<Conversation>], settings: ScopeSettings) -> Self {
        let mut filter = ScopeFilter::new(&settings.excluded, settings.workspace.as_ref());
        let member = conversations
            .iter()
            .map(|conversation| filter.passes(conversation))
            .collect::<Vec<_>>();
        let rows = member
            .iter()
            .enumerate()
            .filter_map(|(position, &member)| member.then_some(position))
            .collect();
        Self {
            valid: true,
            settings,
            rows: Arc::new(rows),
            member,
        }
    }

    /// Whether this is the scope of a corpus of `len` conversations under
    /// these settings.
    pub(super) fn is_current(
        &self,
        excluded: &HashSet<String>,
        workspace: Option<&WorkspaceScope>,
        len: usize,
    ) -> bool {
        self.valid
            && self.member.len() == len
            && self.settings.workspace.as_ref() == workspace
            && self.settings.excluded == *excluded
    }

    /// The positions in scope, ascending (shared with the semantic worker).
    pub(super) fn rows(&self) -> &Arc<Vec<usize>> {
        &self.rows
    }

    /// Whether the conversation at `position` is in scope.
    pub(super) fn contains(&self, position: usize) -> bool {
        self.member.get(position).copied().unwrap_or(false)
    }

    fn invalidate(&mut self) {
        self.valid = false;
        self.rows = Arc::new(Vec::new());
        self.member = Vec::new();
    }

    /// Evaluate the conversations appended at `start..`.
    fn extend(&mut self, conversations: &[Arc<Conversation>], start: usize) {
        if !self.valid || self.member.len() != start {
            self.invalidate();
            return;
        }
        let mut filter =
            ScopeFilter::new(&self.settings.excluded, self.settings.workspace.as_ref());
        let added = conversations[start..]
            .iter()
            .map(|conversation| filter.passes(conversation))
            .collect::<Vec<_>>();
        let rows = Arc::make_mut(&mut self.rows);
        rows.extend(
            added
                .iter()
                .enumerate()
                .filter_map(|(offset, &member)| member.then_some(start + offset)),
        );
        self.member.extend(added);
    }

    /// Re-evaluate the conversation at `position` (it was replaced).
    fn update(&mut self, conversations: &[Arc<Conversation>], position: usize) {
        if !self.valid || self.member.len() != conversations.len() {
            self.invalidate();
            return;
        }
        let mut filter =
            ScopeFilter::new(&self.settings.excluded, self.settings.workspace.as_ref());
        let member = filter.passes(&conversations[position]);
        if self.member[position] == member {
            return;
        }
        self.member[position] = member;
        let rows = Arc::make_mut(&mut self.rows);
        match rows.binary_search(&position) {
            Ok(at) if !member => {
                rows.remove(at);
            }
            Err(at) if member => rows.insert(at, position),
            _ => {}
        }
    }

    /// Drop `removed` (ascending positions of a corpus of `old_len`) and
    /// renumber the later rows.
    fn remove(&mut self, removed: &[usize], old_len: usize) {
        if !self.valid || self.member.len() != old_len {
            self.invalidate();
            return;
        }
        remove_positions_in_place(&mut self.member, removed);
        let rows = Arc::make_mut(&mut self.rows);
        rows.retain(|position| removed.binary_search(position).is_err());
        for position in rows.iter_mut() {
            *position -= removed.partition_point(|&gone| gone < *position);
        }
    }
}

/// Remove the elements at `removed` (ascending positions) from `items`
/// without cloning any element.
pub(super) fn remove_positions_in_place<T>(items: &mut Vec<T>, removed: &[usize]) {
    if removed.is_empty() {
        return;
    }
    let mut position = 0;
    let mut next = 0;
    items.retain(|_| {
        let gone = removed.get(next) == Some(&position);
        if gone {
            next += 1;
        }
        position += 1;
        !gone
    });
}

/// `items` without the elements at `removed` (ascending positions), for a
/// vector still shared with another thread: one clone per kept element.
pub(super) fn without_positions<T: Clone>(items: &[T], removed: &[usize]) -> Vec<T> {
    let mut kept = Vec::with_capacity(items.len().saturating_sub(removed.len()));
    let mut start = 0;
    for &gone in removed {
        kept.extend_from_slice(&items[start..gone]);
        start = gone + 1;
    }
    kept.extend_from_slice(&items[start..]);
    kept
}

/// Every lookup structure of one corpus, kept consistent with it: the
/// positions are positions in that corpus.
#[derive(Clone, Debug, Default)]
pub(super) struct CorpusIndex {
    paths: KeyIndex,
    sessions: KeyIndex,
    sources: SourceCounts,
    pub(super) scope: ScopeIndex,
}

impl CorpusIndex {
    /// Index `conversations` (O(n)), with the scope under `settings` when
    /// given (else the scope is computed when first needed).
    pub(super) fn build(
        conversations: &[Arc<Conversation>],
        settings: Option<ScopeSettings>,
    ) -> Self {
        let mut index = Self {
            paths: KeyIndex::with_capacity(conversations.len()),
            sessions: KeyIndex::with_capacity(conversations.len()),
            sources: SourceCounts::default(),
            scope: ScopeIndex::default(),
        };
        for (position, conversation) in conversations.iter().enumerate() {
            index.add(conversation, position);
        }
        if let Some(settings) = settings {
            index.scope = ScopeIndex::compute(conversations, settings);
        }
        index
    }

    fn add(&mut self, conversation: &Conversation, position: usize) {
        self.paths.insert(path_key(&conversation.path), position);
        self.sessions
            .insert(session_key(&conversation.session_id), position);
        self.sources.add(conversation.source);
    }

    /// Index the conversations appended at `start..`.
    pub(super) fn extend(&mut self, conversations: &[Arc<Conversation>], start: usize) {
        for (offset, conversation) in conversations[start..].iter().enumerate() {
            self.add(conversation, start + offset);
        }
        self.scope.extend(conversations, start);
    }

    /// The conversation at `position` was replaced; `previous` is the one it
    /// replaced (same path).
    pub(super) fn replace(
        &mut self,
        conversations: &[Arc<Conversation>],
        position: usize,
        previous: &Conversation,
    ) {
        let current = &conversations[position];
        if current.path != previous.path {
            self.paths.remove(path_key(&previous.path), position);
            self.paths.insert(path_key(&current.path), position);
        }
        if current.session_id != previous.session_id {
            self.sessions
                .remove(session_key(&previous.session_id), position);
            self.sessions
                .insert(session_key(&current.session_id), position);
        }
        if current.source != previous.source {
            self.sources.remove(previous.source);
            self.sources.add(current.source);
        }
        self.scope.update(conversations, position);
    }

    /// The conversations at `removed` (ascending positions; `gone` holds
    /// them, in the same order) left a corpus of `old_len`.
    pub(super) fn remove(&mut self, removed: &[usize], gone: &[Arc<Conversation>], old_len: usize) {
        for (&position, conversation) in removed.iter().zip(gone) {
            self.paths.remove(path_key(&conversation.path), position);
            self.sessions
                .remove(session_key(&conversation.session_id), position);
            self.sources.remove(conversation.source);
        }
        self.paths.shift(removed);
        self.sessions.shift(removed);
        self.scope.remove(removed, old_len);
    }

    /// Every position whose conversation has this path, ascending.
    pub(super) fn path_positions<'a>(
        &'a self,
        conversations: &'a [Arc<Conversation>],
        path: &'a Path,
    ) -> impl Iterator<Item = usize> + 'a {
        self.paths
            .positions(path_key(path))
            .iter()
            .copied()
            .filter(move |&position| {
                conversations
                    .get(position)
                    .is_some_and(|conversation| conversation.path == path)
            })
    }

    /// The first position whose conversation has this path.
    pub(super) fn first_path(
        &self,
        conversations: &[Arc<Conversation>],
        path: &Path,
    ) -> Option<usize> {
        self.path_positions(conversations, path).next()
    }

    /// The first position whose conversation has this session id (ignoring
    /// ASCII case).
    pub(super) fn first_session(
        &self,
        conversations: &[Arc<Conversation>],
        session_id: &str,
    ) -> Option<usize> {
        self.sessions
            .positions(session_key(session_id))
            .iter()
            .copied()
            .find(|&position| {
                conversations.get(position).is_some_and(|conversation| {
                    conversation.session_id.eq_ignore_ascii_case(session_id)
                })
            })
    }

    /// Whether the corpus mixes sources.
    pub(super) fn mixed_sources(&self) -> bool {
        self.sources.mixed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_keep_positions_ascending() {
        let mut index = KeyIndex::default();
        index.insert(7, 4);
        index.insert(7, 1);
        index.insert(7, 9);
        index.insert(7, 4);
        assert_eq!(index.positions(7), &[1, 4, 9]);
        index.remove(7, 4);
        assert_eq!(index.positions(7), &[1, 9]);
        index.remove(7, 1);
        assert_eq!(index.positions(7), &[9]);
        index.remove(7, 3);
        assert_eq!(index.positions(7), &[9], "an absent position is ignored");
        index.remove(7, 9);
        assert!(index.positions(7).is_empty());
        assert!(index.map.is_empty());
    }

    #[test]
    fn shift_closes_the_gaps_of_removed_positions() {
        let mut index = KeyIndex::default();
        for (key, position) in [(1, 0), (2, 2), (3, 3), (3, 6), (4, 8)] {
            index.insert(key, position);
        }
        // Positions 1, 4, 5 and 7 left (they were never indexed here).
        index.shift(&[1, 4, 5, 7]);
        assert_eq!(index.positions(1), &[0]);
        assert_eq!(index.positions(2), &[1]);
        assert_eq!(index.positions(3), &[2, 3]);
        assert_eq!(index.positions(4), &[4]);
    }

    #[test]
    fn keys_follow_path_equality_and_ignore_session_case() {
        for (left, right) in [
            ("a/b/c.jsonl", "a//b/./c.jsonl"),
            ("a/b/c.jsonl", "a/b/c.jsonl/"),
            ("/x/y", "/x/./y"),
        ] {
            assert_eq!(Path::new(left), Path::new(right));
            assert_eq!(path_key(Path::new(left)), path_key(Path::new(right)));
        }
        #[cfg(windows)]
        {
            let (left, right) = (Path::new(r"C:\p\s.jsonl"), Path::new("C:/p/s.jsonl"));
            assert_eq!(left, right);
            assert_eq!(path_key(left), path_key(right));
        }
        assert_ne!(path_key(Path::new("a/b")), path_key(Path::new("a/c")));
        assert_eq!(
            session_key("AAAAAAAA-1111-4111-8111-111111111111"),
            session_key("aaaaaaaa-1111-4111-8111-111111111111")
        );
        let long = "X".repeat(100);
        assert_eq!(session_key(&long), session_key(&long.to_lowercase()));
        assert_ne!(session_key("a"), session_key("b"));
        // Every byte counts, also past the last full word.
        assert_ne!(hash_bytes(b"123456789"), hash_bytes(b"123456788"));
        assert_ne!(hash_bytes(b""), hash_bytes(b"\0"));
    }

    #[test]
    fn copies_sharing_a_file_name_are_told_apart() {
        let conversation = |path: &str| {
            let mut conversation =
                crate::browser::test_helpers::plain_conversation(PathBuf::from(path));
            conversation.session_id = "same".to_string();
            Arc::new(conversation)
        };
        let corpus = vec![
            conversation("/p1/s.jsonl"),
            conversation("/p2/s.jsonl"),
            conversation("/p3/s.jsonl"),
        ];
        let mut index = CorpusIndex::build(&corpus, None);
        assert_eq!(index.first_path(&corpus, Path::new("/p2/s.jsonl")), Some(1));
        assert_eq!(index.first_path(&corpus, Path::new("/p4/s.jsonl")), None);
        assert_eq!(index.first_session(&corpus, "SAME"), Some(0));
        // The first copy leaves: the others move up and stay found.
        let gone = vec![corpus[0].clone()];
        index.remove(&[0], &gone, 3);
        let corpus = corpus[1..].to_vec();
        assert_eq!(index.first_path(&corpus, Path::new("/p3/s.jsonl")), Some(1));
        assert_eq!(index.first_session(&corpus, "same"), Some(0));
        assert_eq!(
            index
                .path_positions(&corpus, Path::new("/p1/s.jsonl"))
                .count(),
            0
        );
    }

    #[test]
    fn positions_are_removed_without_cloning_or_with_one_clone_each() {
        let mut items = (0..10).collect::<Vec<_>>();
        remove_positions_in_place(&mut items, &[0, 3, 9]);
        assert_eq!(items, [1, 2, 4, 5, 6, 7, 8]);
        assert_eq!(without_positions(&[1, 2, 3, 4], &[1, 2]), [1, 4]);
        assert_eq!(without_positions(&[1, 2, 3], &[]), [1, 2, 3]);
    }

    #[test]
    fn source_counts_report_a_mix() {
        let mut counts = SourceCounts::default();
        assert!(!counts.mixed());
        counts.add(Source::Claude);
        counts.add(Source::Claude);
        assert!(!counts.mixed());
        counts.add(Source::Pi);
        assert!(counts.mixed());
        counts.remove(Source::Pi);
        assert!(!counts.mixed());
    }
}
