use super::{SemanticIndexCandidate, SemanticIndexRequest};
use crate::error::{AppError, Result};
use crate::semantic::chunk::build_chunks_with_sources;
use crate::semantic::rank::{PreparedChunk, PreparedText};
use crate::semantic::types::{
    ChunkConfig, EmbeddedChunk, SemanticCancellationToken, SemanticChunk, SemanticChunkSource,
};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ConversationKey {
    path: PathBuf,
    source: SemanticChunkSource,
}

impl ConversationKey {
    fn of(candidate: &SemanticIndexCandidate) -> Self {
        Self {
            path: candidate.conversation.path.clone(),
            source: candidate.source,
        }
    }
}

#[derive(Clone)]
struct ConversationSignature {
    key: ConversationKey,
    index: usize,
    conversation: Arc<crate::history::Conversation>,
}

impl ConversationSignature {
    fn of(candidate: &SemanticIndexCandidate) -> Self {
        Self {
            key: ConversationKey::of(candidate),
            index: candidate.index,
            conversation: Arc::clone(&candidate.conversation),
        }
    }

    fn content_matches(&self, candidate: &SemanticIndexCandidate) -> bool {
        self.key.source == candidate.source
            && self.key.path == candidate.conversation.path
            && (Arc::ptr_eq(&self.conversation, &candidate.conversation)
                || (self.conversation.semantic_turns == candidate.conversation.semantic_turns
                    && self.conversation.semantic_turn_ranges
                        == candidate.conversation.semantic_turn_ranges))
    }

    fn matches(&self, candidate: &SemanticIndexCandidate) -> bool {
        self.index == candidate.index && self.content_matches(candidate)
    }
}

#[derive(Clone)]
pub(super) struct CorpusSignature {
    corpus_version: u64,
    chunk_config: ChunkConfig,
    conversations: Vec<ConversationSignature>,
}

impl CorpusSignature {
    pub(super) fn of(request: &SemanticIndexRequest<'_>, chunk_config: ChunkConfig) -> Self {
        Self {
            corpus_version: request.corpus_version,
            chunk_config,
            conversations: request
                .full_corpus
                .iter()
                .map(ConversationSignature::of)
                .collect(),
        }
    }

    pub(super) fn matches(
        &self,
        request: &SemanticIndexRequest<'_>,
        chunk_config: ChunkConfig,
    ) -> bool {
        self.corpus_version == request.corpus_version
            && self.chunk_config == chunk_config
            && self.conversations.len() == request.full_corpus.len()
            && self
                .conversations
                .iter()
                .zip(request.full_corpus)
                .all(|(stored, candidate)| stored.matches(candidate))
    }
}

struct ResidentChunk {
    embedded: EmbeddedChunk,
    prepared: PreparedText,
}

struct ResidentConversation {
    signature: Option<ConversationSignature>,
    chunks: Vec<ResidentChunk>,
}

#[derive(Default)]
pub(super) struct ResidentIndex {
    conversations: HashMap<SemanticChunkSource, HashMap<PathBuf, ResidentConversation>>,
    chunk_count: usize,
    #[cfg(test)]
    prepared_chunk_count: usize,
}

impl ResidentIndex {
    pub(super) fn clear(&mut self) {
        self.conversations.clear();
        self.chunk_count = 0;
    }

    pub(super) fn chunk_count(&self) -> usize {
        self.chunk_count
    }

    #[cfg(test)]
    pub(super) fn conversation_count(&self) -> usize {
        self.conversations.values().map(HashMap::len).sum()
    }

    #[cfg(test)]
    pub(super) fn prepared_chunk_count(&self) -> usize {
        self.prepared_chunk_count
    }

    pub(super) fn plan_refresh(
        &mut self,
        request: &SemanticIndexRequest<'_>,
        chunk_config: ChunkConfig,
        cancellation: &SemanticCancellationToken,
    ) -> Result<RefreshPlan> {
        let live = request
            .full_corpus
            .iter()
            .map(ConversationKey::of)
            .collect::<HashSet<_>>();
        self.conversations.retain(|source, conversations| {
            conversations.retain(|path, conversation| {
                let keep = live.contains(&ConversationKey {
                    path: path.clone(),
                    source: *source,
                });
                if !keep {
                    self.chunk_count -= conversation.chunks.len();
                }
                keep
            });
            !conversations.is_empty()
        });

        let mut pending = Vec::new();
        let mut seen = HashSet::new();
        for candidate in request.full_corpus {
            if cancellation.is_cancelled() {
                return Err(AppError::SemanticSearchCancelled);
            }
            let key = ConversationKey::of(candidate);
            if !seen.insert(key.clone()) {
                return Err(AppError::ConfigError(format!(
                    "duplicate semantic conversation candidate: {} ({:?})",
                    key.path.display(),
                    key.source
                )));
            }
            if let Some(resident) = self
                .conversations
                .get_mut(&key.source)
                .and_then(|conversations| conversations.get_mut(&key.path))
                && resident
                    .signature
                    .as_ref()
                    .is_some_and(|signature| signature.content_matches(candidate))
            {
                for chunk in &mut resident.chunks {
                    chunk.embedded.conversation_index = candidate.index;
                }
                if let Some(signature) = &mut resident.signature {
                    signature.index = candidate.index;
                    signature.conversation = Arc::clone(&candidate.conversation);
                }
                continue;
            }
            pending.push(candidate);
        }

        let mut conversations = Vec::with_capacity(pending.len());
        let mut chunks = Vec::new();
        for candidate in pending {
            let candidate_chunks = build_chunks_with_sources(
                std::iter::once((
                    candidate.index,
                    candidate.source,
                    candidate.conversation.as_ref(),
                )),
                chunk_config,
            );
            conversations.push(PendingConversation {
                key: ConversationKey::of(candidate),
                signature: ConversationSignature::of(candidate),
                expected_chunks: candidate_chunks.len(),
            });
            chunks.extend(candidate_chunks);
        }
        Ok(RefreshPlan {
            conversations,
            chunks,
        })
    }

    pub(super) fn absorb(&mut self, plan: RefreshPlan, embedded: Vec<EmbeddedChunk>) {
        let slot_by_position = plan
            .conversations
            .iter()
            .enumerate()
            .map(|(slot, pending)| ((pending.signature.index, pending.key.source), slot))
            .collect::<HashMap<_, _>>();
        let mut grouped = plan
            .conversations
            .iter()
            .map(|_| Vec::new())
            .collect::<Vec<Vec<EmbeddedChunk>>>();
        for chunk in embedded {
            if let Some(slot) = slot_by_position.get(&(chunk.conversation_index, chunk.source)) {
                grouped[*slot].push(chunk);
            }
        }

        let prepared = plan
            .conversations
            .into_par_iter()
            .zip(grouped)
            .map(|(pending, mut chunks)| {
                chunks.sort_by_key(|chunk| chunk.chunk_index);
                let complete = chunks.len() == pending.expected_chunks;
                let chunks = chunks
                    .into_iter()
                    .map(|embedded| ResidentChunk {
                        prepared: PreparedText::new(&embedded),
                        embedded,
                    })
                    .collect::<Vec<_>>();
                (pending, complete, chunks)
            })
            .collect::<Vec<_>>();

        for (pending, complete, chunks) in prepared {
            self.chunk_count += chunks.len();
            #[cfg(test)]
            {
                self.prepared_chunk_count += chunks.len();
            }
            let conversations = self.conversations.entry(pending.key.source).or_default();
            if let Some(previous) = conversations.insert(
                pending.key.path,
                ResidentConversation {
                    signature: complete.then_some(pending.signature),
                    chunks,
                },
            ) {
                self.chunk_count -= previous.chunks.len();
            }
        }
    }

    pub(super) fn is_complete(&self, request: &SemanticIndexRequest<'_>) -> bool {
        request.full_corpus.iter().all(|candidate| {
            self.conversations
                .get(&candidate.source)
                .and_then(|conversations| conversations.get(&candidate.conversation.path))
                .and_then(|resident| resident.signature.as_ref())
                .is_some_and(|signature| signature.content_matches(candidate))
        })
    }

    pub(super) fn scoped<'a>(
        &'a self,
        scope: &[SemanticIndexCandidate],
        cancellation: &SemanticCancellationToken,
    ) -> Result<Vec<PreparedChunk<'a>>> {
        let mut chunks = Vec::new();
        for candidate in scope {
            if cancellation.is_cancelled() {
                return Err(AppError::SemanticSearchCancelled);
            }
            if let Some(resident) = self
                .conversations
                .get(&candidate.source)
                .and_then(|conversations| conversations.get(&candidate.conversation.path))
            {
                chunks.extend(resident.chunks.iter().map(|chunk| PreparedChunk {
                    chunk: &chunk.embedded,
                    prepared: &chunk.prepared,
                }));
            }
        }
        Ok(chunks)
    }
}

pub(super) struct RefreshPlan {
    conversations: Vec<PendingConversation>,
    pub(super) chunks: Vec<SemanticChunk>,
}

impl RefreshPlan {
    pub(super) fn take_chunks(mut self) -> (Self, Vec<SemanticChunk>) {
        let chunks = std::mem::take(&mut self.chunks);
        (self, chunks)
    }
}

struct PendingConversation {
    key: ConversationKey,
    signature: ConversationSignature,
    expected_chunks: usize,
}

pub(super) fn corpus_has_chunks(
    request: &SemanticIndexRequest<'_>,
    chunk_config: ChunkConfig,
) -> bool {
    request.full_corpus.iter().any(|candidate| {
        !build_chunks_with_sources(
            std::iter::once((
                candidate.index,
                candidate.source,
                candidate.conversation.as_ref(),
            )),
            chunk_config,
        )
        .is_empty()
    })
}
