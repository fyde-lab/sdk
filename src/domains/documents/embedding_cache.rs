//! The in-memory [`EmbeddingCache`]: stores entries under their document id
//! within a byte budget. Which entry to put, reuse or remove is up to the
//! documents service (see `service.rs`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use uuid::Uuid;

use super::{CachedEmbeddings, EmbeddingCache};

/// An [`EmbeddingCache`] holding at most `max_bytes` of entries (see
/// [`entry_bytes`]), evicting the oldest put first to make room. Never
/// persisted.
pub(crate) struct MemoryEmbeddingCache {
    max_bytes: usize,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    entries: HashMap<Uuid, Entry>,
    /// Document ids by the order they were put in, oldest first.
    order: BTreeMap<u64, Uuid>,
    next_sequence: u64,
    bytes: usize,
}

struct Entry {
    cached: Arc<CachedEmbeddings>,
    sequence: u64,
    bytes: usize,
}

impl State {
    fn take(&mut self, document_id: Uuid) -> Option<Entry> {
        let entry = self.entries.remove(&document_id)?;
        self.order.remove(&entry.sequence);
        self.bytes -= entry.bytes;
        Some(entry)
    }
}

impl MemoryEmbeddingCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl EmbeddingCache for MemoryEmbeddingCache {
    fn get(&self, document_id: Uuid) -> Option<Arc<CachedEmbeddings>> {
        self.state()
            .entries
            .get(&document_id)
            .map(|entry| entry.cached.clone())
    }

    /// Replaces any entry for the same document, the new one counting as
    /// the newest. One larger than the whole budget isn't kept (nor is the
    /// one it replaces).
    fn put(&self, cached: Arc<CachedEmbeddings>) {
        let document_id = cached.key.document_id;
        let bytes = entry_bytes(&cached);
        let mut state = self.state();
        state.take(document_id);
        if bytes > self.max_bytes {
            return;
        }

        while state.bytes + bytes > self.max_bytes {
            let Some((_, oldest)) = state.order.pop_first() else {
                break;
            };
            if let Some(entry) = state.entries.remove(&oldest) {
                state.bytes -= entry.bytes;
            }
        }

        let sequence = state.next_sequence;
        state.next_sequence += 1;
        state.order.insert(sequence, document_id);
        state.bytes += bytes;
        state.entries.insert(
            document_id,
            Entry {
                cached,
                sequence,
                bytes,
            },
        );
    }

    fn remove(&self, document_id: Uuid) {
        self.state().take(document_id);
    }

    fn clear(&self) {
        *self.state() = State::default();
    }
}

/// About how much memory `cached` takes: its vectors, plus each chunk's and
/// the entry's own size and model names.
fn entry_bytes(cached: &CachedEmbeddings) -> usize {
    let chunks: usize = cached
        .embeddings
        .chunks
        .iter()
        .map(|chunk| std::mem::size_of_val(chunk) + std::mem::size_of_val(chunk.vector.as_slice()))
        .sum();

    std::mem::size_of::<CachedEmbeddings>()
        + cached.key.model.len()
        + cached.embeddings.model.len()
        + chunks
}

#[cfg(test)]
mod tests {
    use super::super::models::{FakeCachedEmbeddings, FakeChunkEmbedding, FakeDocumentEmbeddings};
    use super::*;

    /// An entry for a new document with `chunks` chunks, all the same size
    /// for the same `chunks`.
    fn entry(chunks: usize) -> Arc<CachedEmbeddings> {
        let mut cached = FakeCachedEmbeddings::new()
            .with_embeddings(
                FakeDocumentEmbeddings::new()
                    .with_model("e5.onnx")
                    .with_chunks(
                        (0..chunks)
                            .map(|_| FakeChunkEmbedding::new().build())
                            .collect(),
                    )
                    .build(),
            )
            .build();
        cached.key.model = "e5.onnx".to_string();
        Arc::new(cached)
    }

    fn id(cached: &CachedEmbeddings) -> Uuid {
        cached.key.document_id
    }

    /// Whether each of `entries` is in `cache`.
    fn cache_state(cache: &MemoryEmbeddingCache, entries: &[&Arc<CachedEmbeddings>]) -> Vec<bool> {
        entries
            .iter()
            .map(|entry| cache.get(id(entry)).is_some())
            .collect()
    }

    #[test]
    fn get_returns_what_put_stored_until_it_is_removed() {
        let cache = MemoryEmbeddingCache::new(usize::MAX);
        let (first, second) = (entry(2), entry(1));
        cache.put(first.clone());
        cache.put(second.clone());

        assert_eq!(cache.get(id(&first)), Some(first.clone()));
        assert_eq!(cache.get(id(&second)), Some(second.clone()));
        assert_eq!(cache.get(Uuid::now_v7()), None);

        cache.remove(id(&first));

        assert_eq!(cache.get(id(&first)), None);
        assert_eq!(cache.get(id(&second)), Some(second));
        cache.remove(id(&first));
    }

    #[test]
    fn put_replaces_the_entry_of_the_same_document() {
        let cache = MemoryEmbeddingCache::new(usize::MAX);
        let first = entry(1);
        let replacement = Arc::new(
            FakeCachedEmbeddings::new()
                .with_document_id(id(&first))
                .build(),
        );

        cache.put(first.clone());
        cache.put(replacement.clone());

        assert_eq!(cache.get(id(&first)), Some(replacement.clone()));
        assert_eq!(cache.state().bytes, entry_bytes(&replacement));
    }

    #[test]
    fn put_evicts_the_oldest_entries_until_the_new_one_fits() {
        let (a, b, c) = (entry(1), entry(1), entry(1));
        let cache = MemoryEmbeddingCache::new(entry_bytes(&a) + entry_bytes(&b));
        cache.put(a.clone());
        cache.put(b.clone());
        // Reading an entry doesn't make it any younger.
        cache.get(id(&a));

        cache.put(c.clone());

        assert_eq!(cache_state(&cache, &[&a, &b, &c]), [false, true, true]);

        let big = entry(2);
        cache.put(big.clone());

        assert_eq!(cache_state(&cache, &[&b, &c, &big]), [false, false, true]);
        assert!(cache.state().bytes <= cache.max_bytes);
    }

    #[test]
    fn a_replaced_entry_counts_as_the_newest() {
        let (a, b, c) = (entry(1), entry(1), entry(1));
        let cache = MemoryEmbeddingCache::new(entry_bytes(&a) + entry_bytes(&b));
        cache.put(a.clone());
        cache.put(b.clone());
        cache.put(a.clone());

        cache.put(c.clone());

        assert_eq!(cache_state(&cache, &[&a, &b, &c]), [true, false, true]);
    }

    #[test]
    fn put_keeps_nothing_larger_than_the_whole_budget_and_evicts_nothing_for_it() {
        let small = entry(1);
        let huge = entry(3);
        let cache = MemoryEmbeddingCache::new(entry_bytes(&huge) - 1);
        cache.put(small.clone());

        cache.put(huge.clone());

        assert_eq!(cache_state(&cache, &[&small, &huge]), [true, false]);
    }

    #[test]
    fn clear_removes_every_entry() {
        let cache = MemoryEmbeddingCache::new(usize::MAX);
        let (a, b) = (entry(1), entry(2));
        cache.put(a.clone());
        cache.put(b.clone());

        cache.clear();

        assert_eq!(cache_state(&cache, &[&a, &b]), [false, false]);
        assert_eq!(cache.state().bytes, 0);
        cache.put(a.clone());
        assert_eq!(cache.get(id(&a)), Some(a));
    }

    #[test]
    fn entry_bytes_counts_every_vector() {
        let one = entry(1);
        let mut embeddings = one.embeddings.clone();
        embeddings.chunks.push(FakeChunkEmbedding::new().build());
        let two = FakeCachedEmbeddings::new()
            .with_key(one.key.clone())
            .with_embeddings(embeddings)
            .build();

        assert_eq!(
            entry_bytes(&two) - entry_bytes(&one),
            std::mem::size_of::<super::super::ChunkEmbedding>()
                + super::super::embedder::DIMENSIONS * std::mem::size_of::<f32>()
        );
    }
}
