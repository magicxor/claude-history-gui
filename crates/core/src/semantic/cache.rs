use crate::error::{AppError, Result};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::types::{
    CACHE_SCHEMA_VERSION, CachedChunk, ChunkConfig, DEFAULT_EMBEDDING_BATCH_SIZE, EmbeddedChunk,
    EmbeddingCache, MODEL_NAME, SemanticChunk,
};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const MAX_CACHE_ENTRIES: usize = 50_000;

#[cfg(test)]
pub fn embed_chunks_with_progress_and_save(
    embedder: &mut dyn SemanticEmbedder,
    chunks: Vec<SemanticChunk>,
    cache: &mut EmbeddingCache,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
    progress: impl FnMut(usize, usize),
    save: impl FnMut(&EmbeddingCache),
) -> Result<Vec<EmbeddedChunk>> {
    embed_chunks_with_budget_and_save(embedder, chunks, cache, cancellation, None, progress, save)
}

pub fn embed_chunks_with_budget_and_save(
    embedder: &mut dyn SemanticEmbedder,
    chunks: Vec<SemanticChunk>,
    cache: &mut EmbeddingCache,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
    max_new_embeddings: Option<usize>,
    mut progress: impl FnMut(usize, usize),
    mut save: impl FnMut(&EmbeddingCache),
) -> Result<Vec<EmbeddedChunk>> {
    const SAVE_INTERVAL: usize = 256;

    if cancellation.is_cancelled() {
        return Err(AppError::SemanticSearchCancelled);
    }

    let access = cache.access_counter.saturating_add(1);
    cache.access_counter = access;
    let mut embedded = Vec::with_capacity(chunks.len());
    let mut misses = Vec::<Vec<SemanticChunk>>::new();
    let mut miss_indices = HashMap::<String, usize>::new();

    for chunk in chunks {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        let key = embedding_cache_key(&chunk.text);
        if let Some(entry) = cache.entries.get_mut(&key) {
            entry.last_used = access;
            embedded.push(embedded_chunk(&chunk, entry.embedding.clone()));
        } else if let Some(index) = miss_indices.get(&key).copied() {
            misses[index].push(chunk);
        } else {
            miss_indices.insert(key, misses.len());
            misses.push(vec![chunk]);
        }
    }

    let total_misses = max_new_embeddings.map_or(misses.len(), |limit| misses.len().min(limit));
    misses.truncate(total_misses);
    let mut completed = 0;
    let mut last_saved = 0;
    for batch in misses.chunks(DEFAULT_EMBEDDING_BATCH_SIZE) {
        if cancellation.is_cancelled() {
            save_pending_cache(cache, completed, &mut last_saved, &mut save);
            return Err(AppError::SemanticSearchCancelled);
        }
        let texts = batch
            .iter()
            .map(|chunks| chunks[0].text.clone())
            .collect::<Vec<_>>();
        let embeddings = match embedder.embed_passages(&texts) {
            Ok(embeddings) => embeddings,
            Err(error) => {
                save_pending_cache(cache, completed, &mut last_saved, &mut save);
                return Err(error);
            }
        };
        if let Err(error) = validate_passage_embeddings(&texts, &embeddings) {
            save_pending_cache(cache, completed, &mut last_saved, &mut save);
            return Err(error);
        }

        for (chunks, embedding) in batch.iter().zip(embeddings) {
            let key = embedding_cache_key(&chunks[0].text);
            cache.entries.insert(
                key,
                CachedChunk {
                    embedding: embedding.clone(),
                    last_used: access,
                    protected: false,
                },
            );
            for chunk in chunks {
                embedded.push(embedded_chunk(chunk, embedding.clone()));
            }
        }
        completed += batch.len();
        if completed == total_misses || completed - last_saved >= SAVE_INTERVAL {
            prune_cache(cache);
            save(cache);
            last_saved = completed;
        }
        progress(completed, total_misses);
    }

    Ok(embedded)
}

fn validate_passage_embeddings(texts: &[String], embeddings: &[Vec<f32>]) -> Result<()> {
    if embeddings.len() != texts.len() {
        return Err(AppError::ConfigError(format!(
            "semantic embedder returned {} passage embeddings for {} passages",
            embeddings.len(),
            texts.len()
        )));
    }
    let Some(dimensions) = embeddings.first().map(Vec::len) else {
        return Ok(());
    };
    if dimensions == 0
        || embeddings.iter().any(|embedding| {
            embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
        })
    {
        return Err(AppError::ConfigError(
            "semantic embedder returned empty, inconsistent, or non-finite passage embeddings"
                .to_string(),
        ));
    }
    Ok(())
}

fn save_pending_cache(
    cache: &mut EmbeddingCache,
    completed: usize,
    last_saved: &mut usize,
    save: &mut impl FnMut(&EmbeddingCache),
) {
    if completed > *last_saved {
        prune_cache(cache);
        save(cache);
        *last_saved = completed;
    }
}

fn prune_cache(cache: &mut EmbeddingCache) {
    prune_cache_to_limit(cache, MAX_CACHE_ENTRIES);
}

fn prune_cache_to_limit(cache: &mut EmbeddingCache, max_entries: usize) {
    let excess = cache.entries.len().saturating_sub(max_entries);
    if excess == 0 {
        return;
    }
    let mut oldest = cache
        .entries
        .iter()
        .map(|(key, entry)| (entry.protected, entry.last_used, key.clone()))
        .collect::<Vec<_>>();
    oldest.sort_unstable();
    for (_, _, key) in oldest.into_iter().take(excess) {
        cache.entries.remove(&key);
    }
}

fn embedded_chunk(chunk: &SemanticChunk, embedding: Vec<f32>) -> EmbeddedChunk {
    EmbeddedChunk {
        conversation_index: chunk.conversation_index,
        source: chunk.source,
        session: chunk.session.clone(),
        chunk_index: chunk.chunk_index,
        text: chunk.text.clone(),
        message_range: chunk.message_range,
        embedding,
    }
}

pub fn cached_chunks(
    chunks: Vec<SemanticChunk>,
    cache: &EmbeddingCache,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
) -> Result<(Vec<EmbeddedChunk>, usize)> {
    let mut embedded = Vec::with_capacity(chunks.len());
    let mut misses = HashSet::new();

    for chunk in chunks {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        let key = embedding_cache_key(&chunk.text);
        if let Some(entry) = cache.entries.get(&key) {
            embedded.push(embedded_chunk(&chunk, entry.embedding.clone()));
        } else {
            misses.insert(key);
        }
    }

    Ok((embedded, misses.len()))
}

pub fn cache_miss_count(chunks: &[SemanticChunk], cache: &EmbeddingCache) -> usize {
    chunks
        .iter()
        .map(|chunk| embedding_cache_key(&chunk.text))
        .filter(|key| !cache.entries.contains_key(key))
        .collect::<HashSet<_>>()
        .len()
}

pub fn embedding_cache_key(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

pub fn cache_contains_text(cache: &EmbeddingCache, text: &str) -> bool {
    cache.entries.contains_key(&embedding_cache_key(text))
}

/// Read the embedding cache at `path` (normally
/// [`crate::env::HistoryEnv::embedding_cache_file`]). A missing path, file,
/// or an incompatible/corrupt cache yields an empty cache.
pub fn read_embedding_cache(path: Option<&Path>, config: ChunkConfig) -> EmbeddingCache {
    let Some(path) = path else {
        return empty_embedding_cache(config);
    };
    read_embedding_cache_from_path(path, config)
}

pub fn read_embedding_cache_from_path(path: &Path, config: ChunkConfig) -> EmbeddingCache {
    let Ok(data) = std::fs::read(path) else {
        return empty_embedding_cache(config);
    };
    let Ok(mut cache) = bincode::deserialize::<EmbeddingCache>(&data) else {
        return empty_embedding_cache(config);
    };
    if cache_matches_config(&cache, config) {
        if cache.entries.len() > MAX_CACHE_ENTRIES {
            prune_cache(&mut cache);
            write_embedding_cache_to_path(&cache, path);
        }
        cache
    } else {
        empty_embedding_cache(config)
    }
}

/// Atomically write the embedding cache to `path` (temp file + rename),
/// holding the cache lock ([`lock_semantic_cache_dir`]). Failures are
/// ignored, as upstream; a write that cannot get the lock within
/// [`CACHE_LOCK_WAIT`] is skipped with a Warn debug line.
pub fn write_embedding_cache(path: &Path, cache: &EmbeddingCache) {
    write_embedding_cache_to_path(cache, path);
}

/// A `save` callback for the semantic index that writes to `path`
/// (no-op when `path` is `None`), like [`write_embedding_cache`].
pub fn embedding_cache_writer(path: Option<PathBuf>) -> impl FnMut(&EmbeddingCache) {
    move |cache| {
        if let Some(path) = &path {
            write_embedding_cache_to_path(cache, path);
        }
    }
}

/// File name of the lock that serializes semantic cache writers, inside the
/// semantic cache directory (K6). It is never deleted, not even by
/// [`clear_semantic_cache_files`].
pub const CACHE_LOCK_FILE_NAME: &str = "embeddings.lock";

/// How long a cache write (or clear) waits for the cache lock.
pub const CACHE_LOCK_WAIT: Duration = Duration::from_secs(2);

/// An exclusive lock on `<semantic dir>/embeddings.lock`, held until it is
/// dropped. It makes every embedding-cache writer of this process and of
/// other processes (a second GUI instance, the agent subcommand) take turns:
/// the list's semantic worker, Deep Search, cache generation and clearing.
#[derive(Debug)]
pub struct CacheLock {
    _file: std::fs::File,
}

/// Take the cache lock of `semantic_dir` (created if missing), waiting up
/// to `wait` for another holder. `Ok(None)` when it is still held then.
pub fn lock_semantic_cache_dir(
    semantic_dir: &Path,
    wait: Duration,
) -> std::io::Result<Option<CacheLock>> {
    std::fs::create_dir_all(semantic_dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(semantic_dir.join(CACHE_LOCK_FILE_NAME))?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(CacheLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                std::thread::sleep((deadline - now).min(Duration::from_millis(20)));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Remove the semantic cache (the embedding cache AND the downloaded model):
/// everything in the semantic cache directory except the lock file, under
/// the cache lock. Returns whether anything was removed. Fails with
/// [`std::io::ErrorKind::WouldBlock`] when another writer holds the lock for
/// longer than [`CACHE_LOCK_WAIT`].
///
/// The directory and its [`CACHE_LOCK_FILE_NAME`] stay: deleting the lock
/// file would let a writer that opened it before the delete and the next
/// writer, which creates a new one, both hold "the" exclusive lock.
///
/// Links are never followed: when the directory itself is a symbolic link
/// (or a junction on Windows) only the link is removed (as upstream's
/// `remove_dir_all` did), and links inside it are removed without touching
/// what they point to.
pub fn clear_semantic_cache_files(semantic_dir: Option<&Path>) -> std::io::Result<bool> {
    clear_semantic_cache_files_waiting(semantic_dir, CACHE_LOCK_WAIT)
}

fn clear_semantic_cache_files_waiting(
    semantic_dir: Option<&Path>,
    wait: Duration,
) -> std::io::Result<bool> {
    let Some(path) = semantic_dir else {
        return Ok(false);
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        remove_link(path)?;
        return Ok(true);
    }
    let _lock = lock_semantic_cache_dir(path, wait)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "the semantic cache is being written by another process; try again",
        )
    })?;
    let mut removed = false;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() == CACHE_LOCK_FILE_NAME {
            continue;
        }
        // `DirEntry::file_type` does not follow links (on Windows a
        // junction is a link too).
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            remove_link(&entry.path())?;
        } else if file_type.is_dir() {
            // Does not follow links inside either.
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
        removed = true;
    }
    Ok(removed)
}

/// Remove the symbolic link (or Windows junction) at `path`, never its
/// target.
fn remove_link(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        // A directory symlink or a junction: `RemoveDirectoryW` removes the
        // link itself, whatever its target holds.
        Err(error) if cfg!(windows) => std::fs::remove_dir(path).map_err(|_| error),
        Err(error) => Err(error),
    }
}

fn write_embedding_cache_to_path(cache: &EmbeddingCache, path: &Path) {
    write_embedding_cache_locked(cache, path, CACHE_LOCK_WAIT);
}

/// Write under the cache lock, waiting at most `wait` for it. Returns
/// whether the file was written.
pub(crate) fn write_embedding_cache_locked(
    cache: &EmbeddingCache,
    path: &Path,
    wait: Duration,
) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let lock = match lock_semantic_cache_dir(parent, wait) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            crate::debug::warn(
                Some(crate::debug::DebugLevel::Warn),
                &format!(
                    "Semantic cache write skipped: {} is locked by another writer",
                    parent.join(CACHE_LOCK_FILE_NAME).display()
                ),
            );
            return false;
        }
        Err(error) => {
            crate::debug::warn(
                Some(crate::debug::DebugLevel::Warn),
                &format!(
                    "Semantic cache write skipped: cannot lock {}: {error}",
                    parent.join(CACHE_LOCK_FILE_NAME).display()
                ),
            );
            return false;
        }
    };
    let Ok(data) = bincode::serialize(cache) else {
        return false;
    };
    let Ok(mut tmp) = tempfile::NamedTempFile::new_in(parent) else {
        return false;
    };
    if tmp.write_all(&data).is_err() {
        return false;
    }
    let written = tmp.persist(path).is_ok();
    drop(lock);
    written
}

pub fn empty_embedding_cache(config: ChunkConfig) -> EmbeddingCache {
    EmbeddingCache {
        schema_version: CACHE_SCHEMA_VERSION,
        model: MODEL_NAME.to_string(),
        chunk_target_chars: config.target_chars,
        chunk_overlap_chars: config.overlap_chars,
        chunk_context_turns: config.context_turns,
        access_counter: 0,
        entries: HashMap::new(),
    }
}

fn cache_matches_config(cache: &EmbeddingCache, config: ChunkConfig) -> bool {
    cache.schema_version == CACHE_SCHEMA_VERSION
        && cache.model == MODEL_NAME
        && cache.chunk_target_chars == config.target_chars
        && cache.chunk_overlap_chars == config.overlap_chars
        && cache.chunk_context_turns == config.context_turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::types::SemanticCancellationToken;

    struct FakeEmbedder {
        calls: usize,
    }

    impl SemanticEmbedder for FakeEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls += 1;
            Ok(passages
                .iter()
                .map(|text| vec![text.len() as f32, 1.0])
                .collect())
        }

        fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    struct CancellingEmbedder {
        cancellation: SemanticCancellationToken,
    }

    impl SemanticEmbedder for CancellingEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            self.cancellation.cancel();
            Ok(passages.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    fn chunk(key: &str, text: &str) -> SemanticChunk {
        let chunk_index = key
            .rsplit_once(':')
            .and_then(|(_, index)| index.parse::<usize>().ok())
            .unwrap_or(0);
        SemanticChunk {
            conversation_index: 0,
            source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
            session: "session".to_string(),
            chunk_index,
            text: text.to_string(),
            message_range: crate::agent::refs::MessageRange::single(1),
        }
    }

    fn cached() -> CachedChunk {
        CachedChunk {
            embedding: vec![0.5, 0.5],
            last_used: 0,
            protected: false,
        }
    }

    fn cache_text(cache: &mut EmbeddingCache, text: &str) {
        cache.entries.insert(embedding_cache_key(text), cached());
    }

    #[test]
    fn passage_embedding_validation_rejects_invalid_output() {
        let texts = vec!["alpha".to_string(), "beta".to_string()];

        assert!(validate_passage_embeddings(&texts, &[vec![1.0, 0.0]]).is_err());
        assert!(validate_passage_embeddings(&texts, &[vec![1.0], vec![1.0, 0.0]]).is_err());
        assert!(validate_passage_embeddings(&texts, &[vec![f32::NAN], vec![1.0]]).is_err());
    }

    #[test]
    fn cache_pruning_keeps_most_recent_entries() {
        let mut cache = empty_embedding_cache(ChunkConfig::default());
        for (text, last_used) in [("oldest", 1), ("middle", 2), ("newest", 3)] {
            cache.entries.insert(
                embedding_cache_key(text),
                CachedChunk {
                    embedding: vec![last_used as f32],
                    last_used,
                    protected: false,
                },
            );
        }

        prune_cache_to_limit(&mut cache, 2);

        assert_eq!(cache.entries.len(), 2);
        assert!(!cache_contains_text(&cache, "oldest"));
        assert!(cache_contains_text(&cache, "middle"));
        assert!(cache_contains_text(&cache, "newest"));
    }

    #[test]
    fn cache_pruning_preserves_protected_core_entries() {
        let mut cache = empty_embedding_cache(ChunkConfig::default());
        cache_text(&mut cache, "adaptive newest");
        cache
            .entries
            .get_mut(&embedding_cache_key("adaptive newest"))
            .unwrap()
            .last_used = 10;
        cache_text(&mut cache, "core oldest");
        let core = cache
            .entries
            .get_mut(&embedding_cache_key("core oldest"))
            .unwrap();
        core.last_used = 1;
        core.protected = true;

        prune_cache_to_limit(&mut cache, 1);

        assert!(cache_contains_text(&cache, "core oldest"));
        assert!(!cache_contains_text(&cache, "adaptive newest"));
    }

    #[test]
    fn embed_chunks_respects_new_embedding_budget() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");
        let chunks = std::iter::once(chunk("session:0", "cached text"))
            .chain((1..=40).map(|index| {
                chunk(
                    &format!("session:{index}"),
                    &format!("missing text {index}"),
                )
            }))
            .collect::<Vec<_>>();
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_budget_and_save(
            &mut embedder,
            chunks,
            &mut cache,
            &SemanticCancellationToken::new(),
            Some(32),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded.len(), 33);
        assert_eq!(cache.entries.len(), 33);
        assert!(cache_contains_text(&cache, "missing text 32"));
        assert!(!cache_contains_text(&cache, "missing text 33"));
    }

    #[test]
    fn embed_chunks_reuses_matching_cache_entry() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![chunk("session:0", "cached text")],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 0);
        assert_eq!(embedded[0].embedding, vec![0.5, 0.5]);
        assert_eq!(embedded[0].chunk_index, 0);
    }

    #[test]
    fn embed_chunks_embeds_cache_misses() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![chunk("session:0", "new text")],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded[0].embedding, vec![8.0, 1.0]);
        assert_eq!(embedded[0].chunk_index, 0);
        assert!(cache.entries.contains_key(&embedding_cache_key("new text")));
    }

    #[test]
    fn embed_chunks_reuses_content_across_chunk_keys() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "shared text");
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![
                chunk("first-session:0", "shared text"),
                chunk("second-session:4", "shared text"),
            ],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 0);
        assert_eq!(embedded.len(), 2);
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn embed_chunks_embeds_duplicate_misses_once() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let mut embedder = FakeEmbedder { calls: 0 };
        let mut progress = Vec::new();

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![
                chunk("first-session:0", "duplicate text"),
                chunk("second-session:0", "duplicate text"),
            ],
            &mut cache,
            &SemanticCancellationToken::new(),
            |completed, total| progress.push((completed, total)),
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded.len(), 2);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(progress, vec![(1, 1)]);
    }

    #[test]
    fn embed_chunks_checkpoints_large_runs_and_saves_the_final_batch() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let mut embedder = FakeEmbedder { calls: 0 };
        let chunks = (0..300)
            .map(|index| chunk(&format!("session:{index}"), &format!("text {index}")))
            .collect();
        let mut saved_entry_counts = Vec::new();

        embed_chunks_with_progress_and_save(
            &mut embedder,
            chunks,
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |cache| saved_entry_counts.push(cache.entries.len()),
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 10);
        assert_eq!(saved_entry_counts, vec![256, 300]);
    }

    #[test]
    fn embed_chunks_saves_completed_work_when_cancelled() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let cancellation = SemanticCancellationToken::new();
        let mut embedder = CancellingEmbedder {
            cancellation: cancellation.clone(),
        };
        let chunks = (0..33)
            .map(|index| chunk(&format!("session:{index}"), &format!("text {index}")))
            .collect();
        let mut saved_entry_counts = Vec::new();

        let result = embed_chunks_with_progress_and_save(
            &mut embedder,
            chunks,
            &mut cache,
            &cancellation,
            |_, _| {},
            |cache| saved_entry_counts.push(cache.entries.len()),
        );

        assert!(matches!(result, Err(AppError::SemanticSearchCancelled)));
        assert_eq!(saved_entry_counts, vec![32]);
    }

    #[test]
    fn changed_text_creates_a_distinct_cache_entry() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "old text");
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![chunk("session:0", "new text")],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        let restored = cache
            .entries
            .get(&embedding_cache_key("new text"))
            .expect("cache entry");
        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded[0].embedding, vec![8.0, 1.0]);
        assert_eq!(restored.embedding, vec![8.0, 1.0]);
        assert!(cache_contains_text(&cache, "old text"));
    }

    #[test]
    fn cache_config_mismatch_invalidates_cache() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache.chunk_target_chars += 1;

        assert!(!cache_matches_config(&cache, config));
    }

    #[test]
    fn wrong_schema_cache_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache.schema_version = CACHE_SCHEMA_VERSION - 1;
        cache_text(&mut cache, "stale");
        write_embedding_cache_to_path(&cache, &path);

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.schema_version, CACHE_SCHEMA_VERSION);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn previous_cache_layout_is_ignored() {
        #[derive(serde::Serialize)]
        struct PreviousCache {
            schema_version: u32,
            model: String,
            chunk_target_chars: usize,
            chunk_overlap_chars: usize,
            chunk_context_turns: usize,
            entries: HashMap<String, PreviousEntry>,
        }

        #[derive(serde::Serialize)]
        struct PreviousEntry {
            file_size: u64,
            mtime_secs: u64,
            mtime_nsecs: u32,
            text: String,
            embedding: Vec<f32>,
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let previous = PreviousCache {
            schema_version: CACHE_SCHEMA_VERSION - 1,
            model: MODEL_NAME.to_string(),
            chunk_target_chars: config.target_chars,
            chunk_overlap_chars: config.overlap_chars,
            chunk_context_turns: config.context_turns,
            entries: HashMap::from([(
                "session:0".to_string(),
                PreviousEntry {
                    file_size: 10,
                    mtime_secs: 20,
                    mtime_nsecs: 30,
                    text: "cached text".to_string(),
                    embedding: vec![0.5, 0.5],
                },
            )]),
        };
        std::fs::write(&path, bincode::serialize(&previous).unwrap()).unwrap();

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.schema_version, CACHE_SCHEMA_VERSION);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn corrupt_cache_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        std::fs::write(&path, b"not bincode").expect("write corrupt cache");

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.schema_version, CACHE_SCHEMA_VERSION);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn mismatched_config_cache_is_ignored_when_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache.chunk_overlap_chars += 1;
        cache_text(&mut cache, "stale");
        write_embedding_cache_to_path(&cache, &path);

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.chunk_overlap_chars, config.overlap_chars);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn cache_round_trips_when_config_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");

        write_embedding_cache_to_path(&cache, &path);
        let restored = read_embedding_cache_from_path(&path, config);

        assert!(cache_contains_text(&restored, "cached text"));
    }

    #[test]
    fn cache_miss_count_matches_cached_chunks() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");
        cache_text(&mut cache, "old text");
        let chunks = vec![
            chunk("session:0", "cached text"),
            chunk("session:1", "new text"),
            chunk("session:2", "uncached text"),
        ];

        let (_, cached_chunk_misses) =
            cached_chunks(chunks.clone(), &cache, &SemanticCancellationToken::new()).unwrap();

        assert_eq!(cache_miss_count(&chunks, &cache), cached_chunk_misses);
        assert_eq!(cache_miss_count(&chunks, &cache), 2);
    }

    #[test]
    fn semantic_cache_paths_use_the_isolated_gui_namespace() {
        let env = crate::env::HistoryEnv::sandbox(Path::new("/home/example"));

        assert_eq!(
            env.embedding_cache_file().unwrap(),
            Path::new("/home/example/.cache/claude-history-gui/semantic/embeddings-v1.bin")
        );
        assert_eq!(
            env.semantic_model_dir().unwrap(),
            Path::new("/home/example/.cache/claude-history-gui/semantic/fastembed")
        );
    }

    #[test]
    fn clear_semantic_cache_removes_the_semantic_directory_only() {
        let root = tempfile::tempdir().unwrap();
        let env = crate::env::HistoryEnv::sandbox(root.path());
        let semantic = env.semantic_cache_dir().unwrap();
        let sibling = env.cache_root().unwrap().join("projects");
        std::fs::create_dir_all(semantic.join("fastembed")).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        write_embedding_cache(
            &env.embedding_cache_file().unwrap(),
            &empty_embedding_cache(ChunkConfig::default()),
        );

        assert!(clear_semantic_cache_files(Some(&semantic)).unwrap());
        assert_eq!(entry_names(&semantic), vec![CACHE_LOCK_FILE_NAME]);
        assert!(sibling.exists());
        assert!(!clear_semantic_cache_files(Some(&semantic)).unwrap());
        assert!(!clear_semantic_cache_files(None).unwrap());
        let missing = root.path().join("missing");
        assert!(!clear_semantic_cache_files(Some(&missing)).unwrap());
        assert!(!missing.exists());
    }

    fn entry_names(dir: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// Make `link` a directory link to `target`: a symbolic link, else (on
    /// Windows without the symlink privilege) a junction, as `mklink /J`
    /// makes it.
    fn dir_link(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(target, link).is_err() {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let output = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn clear_never_deletes_through_a_linked_semantic_dir_or_links_inside_it() {
        // A user who moved the model cache to another drive with a link
        // (symlink or junction) keeps that folder's other contents.
        let root = tempfile::tempdir().unwrap();
        let elsewhere = root.path().join("other-models");
        std::fs::create_dir_all(elsewhere.join("someone-elses-model")).unwrap();
        std::fs::write(elsewhere.join("someone-elses-model/weights.bin"), b"w").unwrap();
        std::fs::write(elsewhere.join("notes.txt"), b"n").unwrap();
        let cache_root = root.path().join("cache");
        std::fs::create_dir_all(&cache_root).unwrap();

        // The semantic directory itself is a link: only the link goes.
        let semantic = cache_root.join("semantic");
        dir_link(&elsewhere, &semantic);
        assert!(clear_semantic_cache_files(Some(&semantic)).unwrap());
        assert!(
            std::fs::symlink_metadata(&semantic).is_err(),
            "link removed"
        );
        assert_eq!(
            entry_names(&elsewhere),
            vec!["notes.txt", "someone-elses-model"]
        );
        assert!(elsewhere.join("someone-elses-model/weights.bin").exists());

        // A link inside a real semantic directory: unlinked, target kept.
        std::fs::create_dir_all(&semantic).unwrap();
        dir_link(&elsewhere, &semantic.join("fastembed"));
        std::fs::write(semantic.join("embeddings-v1.bin"), b"cache").unwrap();
        assert!(clear_semantic_cache_files(Some(&semantic)).unwrap());
        assert_eq!(entry_names(&semantic), vec![CACHE_LOCK_FILE_NAME]);
        assert_eq!(
            entry_names(&elsewhere),
            vec!["notes.txt", "someone-elses-model"]
        );
        assert!(elsewhere.join("someone-elses-model/weights.bin").exists());
    }

    #[test]
    fn clear_keeps_the_lock_file_so_the_lock_stays_exclusive() {
        // K6: a writer that opened embeddings.lock before a clear and the
        // next writer must lock the same file. Deleting it let both hold
        // "the" exclusive lock.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("semantic");
        write_embedding_cache(
            &dir.join("embeddings-v1.bin"),
            &empty_embedding_cache(ChunkConfig::default()),
        );
        // The queued writer has the lock file open, waiting for its turn.
        let queued = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(CACHE_LOCK_FILE_NAME))
            .unwrap();
        assert!(clear_semantic_cache_files(Some(&dir)).unwrap());
        assert!(!dir.join("embeddings-v1.bin").exists());
        // Its turn comes ...
        queued.try_lock().unwrap();
        // ... and nobody else can take the lock meanwhile.
        assert!(
            lock_semantic_cache_dir(&dir, Duration::from_millis(30))
                .unwrap()
                .is_none()
        );
        let error =
            clear_semantic_cache_files_waiting(Some(&dir), Duration::from_millis(30)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        drop(queued);
        assert!(
            lock_semantic_cache_dir(&dir, Duration::ZERO)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn s031_cache_capacity_and_chunking_defaults_match_upstream() {
        // 50k-entry LRU (pruning: cache_pruning_* tests); chunks of 2400
        // characters with 300 of overlap and one context turn.
        assert_eq!(MAX_CACHE_ENTRIES, 50_000);
        let config = ChunkConfig::default();
        assert_eq!(
            (
                config.target_chars,
                config.overlap_chars,
                config.context_turns
            ),
            (2400, 300, 1)
        );
    }

    #[test]
    fn a_cache_write_is_skipped_while_another_writer_holds_the_lock() {
        // K6 / X-041: one writer at a time; a write that cannot get the lock
        // in time is skipped and leaves the existing file intact.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("semantic");
        let path = dir.join("embeddings-v1.bin");
        let config = ChunkConfig::default();
        let mut first = empty_embedding_cache(config);
        first.entries.insert("first".to_string(), cached());
        assert!(write_embedding_cache_locked(&first, &path, CACHE_LOCK_WAIT));
        let before = std::fs::read(&path).unwrap();

        let held = lock_semantic_cache_dir(&dir, Duration::ZERO)
            .unwrap()
            .expect("lock is free");
        // A second holder (another handle, as another process would use)
        // cannot take it.
        assert!(
            lock_semantic_cache_dir(&dir, Duration::from_millis(30))
                .unwrap()
                .is_none()
        );
        let mut second = empty_embedding_cache(config);
        second.entries.insert("second".to_string(), cached());
        let started = std::time::Instant::now();
        assert!(!write_embedding_cache_locked(
            &second,
            &path,
            Duration::from_millis(50)
        ));
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert_eq!(std::fs::read(&path).unwrap(), before, "not corrupted");
        let restored = read_embedding_cache(Some(&path), config);
        assert!(restored.entries.contains_key("first"));

        drop(held);
        assert!(write_embedding_cache_locked(
            &second,
            &path,
            Duration::from_millis(50)
        ));
        assert!(
            read_embedding_cache(Some(&path), config)
                .entries
                .contains_key("second")
        );
    }

    #[test]
    fn a_cache_write_waits_for_a_lock_released_in_time() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("semantic");
        let path = dir.join("embeddings-v1.bin");
        let held = lock_semantic_cache_dir(&dir, Duration::ZERO)
            .unwrap()
            .unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
        });
        let mut cache = empty_embedding_cache(ChunkConfig::default());
        cache.entries.insert("late".to_string(), cached());
        assert!(write_embedding_cache_locked(&cache, &path, CACHE_LOCK_WAIT));
        releaser.join().unwrap();
        assert_eq!(
            read_embedding_cache(Some(&path), ChunkConfig::default())
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn clear_waits_for_the_cache_lock() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("semantic");
        write_embedding_cache(
            &dir.join("embeddings-v1.bin"),
            &empty_embedding_cache(ChunkConfig::default()),
        );
        let held = lock_semantic_cache_dir(&dir, Duration::ZERO)
            .unwrap()
            .unwrap();
        let error =
            clear_semantic_cache_files_waiting(Some(&dir), Duration::from_millis(30)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(dir.join("embeddings-v1.bin").exists(), "nothing removed");
        drop(held);
        assert!(clear_semantic_cache_files(Some(&dir)).unwrap());
        assert_eq!(entry_names(&dir), vec![CACHE_LOCK_FILE_NAME]);
    }

    #[test]
    fn writer_callback_round_trips_through_reader() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("semantic").join("embeddings-v1.bin");
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache.entries.insert("key".to_string(), cached());

        let mut save = embedding_cache_writer(Some(path.clone()));
        save(&cache);

        let restored = read_embedding_cache(Some(&path), config);
        assert_eq!(restored.entries.len(), 1);
        assert!(read_embedding_cache(None, config).entries.is_empty());
        embedding_cache_writer(None)(&cache);
    }
}
