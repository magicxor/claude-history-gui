//! Caches (DESIGN §9.7): selection handles capped by count, syntect tokens,
//! lazy search texts. WP2.
//!
//! * [`HandleCache`]: one `TextSelectionHandle` per rendered segment, keyed
//!   by `(ItemId, segment)`, capped by **handle count** ([`HANDLE_CAP`]);
//!   after each scroll the handles of the items farthest from the top of the
//!   viewport are evicted (live selection handles make every frame slower,
//!   spike-viewer §3.2).
//! * [`TokenCache`]: syntect tokens per `(ItemId, segment, ThemeKind)`,
//!   capped at [`TOKEN_CAP`] segments, least recently used evicted.
//! * Search texts live with the installed items (`search.rs`), built on the
//!   first search and dropped on open.
//!
//! Everything is cleared on open; the token cache also on a theme change.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use claude_history_core::theme::ThemeKind;
use gpui_kit::base::TextSelectionHandle;
use gpui_kit::{App, EntityId};

use super::code::Tokens;
use super::items::ItemId;

/// Live selection handles kept at most (a table counts one per row).
pub const HANDLE_CAP: usize = 1_500;
/// Highlighted code segments kept at most.
pub const TOKEN_CAP: usize = 2_000;

/// A small LRU map: every access stamps the entry; inserting beyond the
/// capacity drops the least recently used tenth.
pub struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    tick: u64,
    cap: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            cap: cap.max(1),
        }
    }

    pub fn get(&mut self, key: &K) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|(value, used)| {
            *used = tick;
            value.clone()
        })
    }

    pub fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    pub fn insert(&mut self, key: K, value: V) {
        self.tick += 1;
        self.map.insert(key, (value, self.tick));
        if self.map.len() > self.cap {
            let mut ticks: Vec<(u64, K)> = self
                .map
                .iter()
                .map(|(key, (_, used))| (*used, key.clone()))
                .collect();
            ticks.sort_by_key(|(used, _)| *used);
            let drop = (self.map.len() - self.cap) + self.cap / 10;
            for (_, key) in ticks.into_iter().take(drop) {
                self.map.remove(&key);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Every entry (tests; no access stamp).
    #[cfg(test)]
    pub fn entries(&self) -> Vec<(K, V)> {
        self.map
            .iter()
            .map(|(key, (value, _))| (key.clone(), value.clone()))
            .collect()
    }
}

/// Key of a highlighted code segment.
pub type TokenKey = (ItemId, u16, ThemeKind);

/// Syntect tokens of code segments, plus the requests in flight.
pub struct TokenCache {
    pub lru: Lru<TokenKey, Tokens>,
    /// Requested from the background executor and not answered yet.
    pub requested: HashSet<TokenKey>,
    /// Requests collected while painting, started after the frame.
    pub wanted: Vec<(TokenKey, String, String)>,
    /// Completed highlight batches (probe for tests).
    pub batches_done: usize,
}

impl Default for TokenCache {
    fn default() -> Self {
        Self {
            lru: Lru::new(TOKEN_CAP),
            requested: HashSet::new(),
            wanted: Vec::new(),
            batches_done: 0,
        }
    }
}

impl TokenCache {
    pub fn clear(&mut self) {
        self.lru.clear();
        self.requested.clear();
        self.wanted.clear();
    }
}

struct HandleEntry {
    handle: TextSelectionHandle,
    /// The item's index when the handle was last used.
    item_ix: usize,
}

/// Selection handles per rendered segment, capped by count.
pub struct HandleCache {
    map: HashMap<(ItemId, u32), HandleEntry>,
    cap: usize,
}

impl Default for HandleCache {
    fn default() -> Self {
        Self::new(HANDLE_CAP)
    }
}

impl HandleCache {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            cap: cap.max(2),
        }
    }

    /// The handle of segment `segment` of item `id` (created on first use).
    pub fn get_or_create(
        &mut self,
        id: &ItemId,
        segment: u32,
        item_ix: usize,
        text: &str,
        cx: &mut App,
    ) -> TextSelectionHandle {
        let entry = self
            .map
            .entry((id.clone(), segment))
            .or_insert_with(|| HandleEntry {
                handle: TextSelectionHandle::new(text.to_string(), cx),
                item_ix,
            });
        entry.item_ix = item_ix;
        entry.handle.clone()
    }

    /// Over the cap: keep the half nearest to `top_ix`.
    pub fn evict_far(&mut self, top_ix: usize) -> usize {
        if self.map.len() <= self.cap {
            return 0;
        }
        let mut keys: Vec<((ItemId, u32), usize)> = self
            .map
            .iter()
            .map(|(key, entry)| (key.clone(), entry.item_ix.abs_diff(top_ix)))
            .collect();
        keys.sort_by_key(|(_, distance)| *distance);
        let keep = self.cap / 2;
        let mut evicted = 0;
        for (key, _) in keys.into_iter().skip(keep) {
            self.map.remove(&key);
            evicted += 1;
        }
        evicted
    }

    /// The item index whose handle has this entity id.
    pub fn item_of(&self, entity: EntityId) -> Option<usize> {
        self.map
            .values()
            .find(|entry| entry.handle.entity_id() == entity)
            .map(|entry| entry.item_ix)
    }

    /// Every live handle with its item index.
    pub fn handles(&self) -> impl Iterator<Item = (&TextSelectionHandle, usize)> {
        self.map
            .values()
            .map(|entry| (&entry.handle, entry.item_ix))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Item indices change on a rebuild: drop the handles of ids that no
    /// longer exist (`keep` says whether an id survives).
    pub fn retain(&mut self, keep: impl Fn(&ItemId) -> Option<usize>) {
        self.map.retain(|(id, _), entry| match keep(id) {
            Some(ix) => {
                entry.item_ix = ix;
                true
            }
            None => false,
        });
    }
}
