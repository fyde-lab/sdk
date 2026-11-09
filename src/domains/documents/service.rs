use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::{ErrorCode, ErrorContext as _, ErrorKind, Result};

use crate::domains::changelog::{
    ChangelogEvent, EventType, MAX_EVENT_BYTES, Service as ChangelogService,
};

use super::attributes::{self, Service as AttributesService};
use super::chunk;
use super::embedder::Embedder;
use super::storage::{FacetField, Storage};
use super::transcript;
use super::{
    CachedEmbeddings, Category, ChunkEmbedding, Document, DocumentEmbeddings, EmbeddingCache,
    EmbeddingKey, FacetCount, Indexer, Metadata, SearchFacets, SearchFilters, SearchOrder,
    SearchQuery, Service, Subcategory, UploadRequest, UploadSource,
};

/// The only content type [`Service::upload`] currently accepts.
pub(super) const PDF_CONTENT_TYPE: &str = "application/pdf";

/// The size of the chunks a document's text is embedded in, in characters
/// of whitespace-collapsed text (see [`chunk::chunk`]).
const CHUNK_CHARS: usize = 1000;

/// The version of how a document's text becomes the passages it's embedded
/// as (see [`passages`]), part of every [`EmbeddingKey`]: bump it whenever
/// [`chunk::chunk`], [`CHUNK_CHARS`] or the passages' format changes, so
/// embeddings computed the old way are never reused.
pub(super) const CHUNKER_VERSION: u32 = 1;

/// How many of its best matches each ranking contributes to a hybrid
/// search (see [`DocumentsClient::rank`]): no page is found past their
/// fusion.
const CANDIDATES: usize = 200;

/// Reciprocal Rank Fusion's `k` (see [`fuse`]): the usual 60 keeps the very
/// first ranks from drowning out a document found by both rankings.
const RRF_K: f64 = 60.0;

/// The cosine similarity to a query below which a document's best chunk
/// doesn't count as a match. multilingual-e5-small squeezes every cosine
/// into roughly 0.7–0.9, so even unrelated text scores high. Tuned on a real
/// French corpus: documents answering a query scored 0.85–0.91, while most
/// off-topic queries' best matches stayed below 0.85.
const SIMILARITY_FLOOR: f32 = 0.85;

/// The default [`Service`] implementation: publishes new documents as
/// encrypted changelog events via an injected [`ChangelogService`], and
/// delegates local, unencrypted caching to an injected [`Storage`].
pub(super) struct DocumentsClient<S: Storage> {
    storage: S,
    changelog: Arc<dyn ChangelogService>,
    /// Invoked once per event by [`Service::start_sync`]'s consume job, when
    /// set — see [`crate::ClientConfig::on_document_change`].
    on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
    attributes: Arc<dyn AttributesService>,
    /// What [`Service::start_sync`]'s consume job caches documents through.
    indexer: Arc<dyn Indexer>,
    /// What uploads and searches embed their text with.
    embedder: Arc<dyn Embedder>,
    /// Where uploads leave the embeddings they computed, for `indexer` to
    /// reuse.
    cache: Arc<dyn EmbeddingCache>,
    /// When embedding an upload last failed (see [`UPLOAD_EMBED_RETRY_AFTER`]).
    embed_failed_at: Mutex<Option<tokio::time::Instant>>,
}

/// How long uploads skip embedding after it failed: failing usually means
/// waiting for the model's download (a timeout, say), which every upload
/// would otherwise wait for again. The documents uploaded meanwhile are
/// embedded once synced.
const UPLOAD_EMBED_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// An upload's embeddings in the [`EmbeddingCache`], removed when dropped
/// unless [`Self::keep`] is called once its event is published: when
/// publishing fails, or the upload is given up on (a stopped scraper run)
/// while publishing, and may never be published.
struct CachedUntilPublished<'a> {
    cache: &'a dyn EmbeddingCache,
    document_id: Uuid,
}

impl CachedUntilPublished<'_> {
    fn keep(self) {
        std::mem::forget(self);
    }
}

impl Drop for CachedUntilPublished<'_> {
    fn drop(&mut self) {
        self.cache.remove(self.document_id);
    }
}

impl<S: Storage> DocumentsClient<S> {
    /// Creates a documents client using `storage` to read cached documents,
    /// `changelog` to publish new ones and drive [`Service::start_sync`],
    /// `on_document_change` as the callback [`Service::start_sync`] invokes
    /// per consumed event, `attributes` to check the attributes of every
    /// document before it's published, `indexer` as what that job caches
    /// documents through, `embedder` to embed uploads' and searches' text,
    /// and `cache` to hand uploads' embeddings over to `indexer`.
    pub(super) fn new(
        storage: S,
        changelog: Arc<dyn ChangelogService>,
        on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
        attributes: Arc<dyn AttributesService>,
        indexer: Arc<dyn Indexer>,
        embedder: Arc<dyn Embedder>,
        cache: Arc<dyn EmbeddingCache>,
    ) -> Self {
        Self {
            storage,
            changelog,
            on_document_change,
            attributes,
            indexer,
            embedder,
            cache,
            embed_failed_at: Mutex::new(None),
        }
    }
}

impl<S: Storage> DocumentsClient<S> {
    /// Rejects metadata whose fields contradict each other, or whose
    /// attributes aren't valid (see [`AttributesService::check`]).
    async fn check_metadata(&self, metadata: &Metadata) -> Result<()> {
        check_categories(metadata.category, metadata.subcategory)?;
        self.attributes
            .check(&metadata.attributes)
            .await
            .context("invalid document attributes")
    }

    /// Embeds an uploaded document's text exactly as [`DocumentIndexer`]
    /// would once the document syncs, under the key it looks them up by.
    /// `None` for a document with no text (indexed with no chunks, no model
    /// needed), or if embedding fails (the model can't be fetched, say) or
    /// failed less than [`UPLOAD_EMBED_RETRY_AFTER`] ago: the document is
    /// then published anyway, and embedded when it syncs.
    async fn embed_upload(&self, metadata: &Metadata) -> Option<Arc<CachedEmbeddings>> {
        let passages = passages(metadata);
        if passages.texts.is_empty() {
            return None;
        }

        let failed_at = *self
            .embed_failed_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if failed_at.is_some_and(|at| at.elapsed() < UPLOAD_EMBED_RETRY_AFTER) {
            tracing::debug!(
                "not embedding document {} at upload, embedding failed recently: it'll be embedded once synced",
                metadata.id
            );
            return None;
        }

        let model = self.embedder.model();
        let key = embedding_key(metadata.id, &model, CHUNKER_VERSION, &passages.texts);
        match embed(&*self.embedder, model, passages).await {
            Ok(embeddings) => Some(Arc::new(CachedEmbeddings { key, embeddings })),
            Err(err) => {
                *self
                    .embed_failed_at
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(tokio::time::Instant::now());
                tracing::warn!(
                    code = ?err.code(),
                    "failed to embed document {} at upload, it'll be embedded once synced: {err}",
                    metadata.id
                );
                None
            }
        }
    }

    /// Embeds a search's `text`, or `None` if that fails (the model isn't
    /// downloaded yet, say): searching then falls back to its terms alone,
    /// rather than fail because of the model.
    async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        match self.embedder.embed_query(text.trim().to_string()).await {
            Ok(vector) => Some(vector),
            Err(err) => {
                tracing::warn!(
                    code = ?err.code(),
                    "searching by terms only, the search text couldn't be embedded: {err}"
                );
                None
            }
        }
    }

    /// The two rankings a hybrid search fuses, each best first and at most
    /// [`CANDIDATES`] long, among documents matching `filters`: by `terms`
    /// (`bm25`), then by the similarity of their best chunk to `vector`,
    /// only those at or above [`SIMILARITY_FLOOR`].
    async fn rank(
        &self,
        terms: &[String],
        vector: &[f32],
        filters: &SearchFilters,
    ) -> Result<[Vec<Uuid>; 2]> {
        let by_terms = self
            .storage
            .rank_by_terms(terms, filters, CANDIDATES)
            .await
            .context("failed to rank local documents by their terms")?;
        let by_similarity = self
            .storage
            .rank_by_similarity(vector, &self.embedder.model(), filters, CANDIDATES)
            .await
            .context("failed to rank local documents by their similarity")?
            .into_iter()
            .filter(|(_, similarity)| *similarity >= SIMILARITY_FLOOR)
            .map(|(id, _)| id)
            .collect();

        Ok([by_terms, by_similarity])
    }

    /// Every document a hybrid search (see [`Self::rank`]) could return,
    /// in no particular order.
    async fn hybrid_matches(
        &self,
        terms: &[String],
        vector: &[f32],
        filters: &SearchFilters,
    ) -> Result<Vec<Uuid>> {
        let mut ids = self.rank(terms, vector, filters).await?.concat();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Counts the documents matching `terms` and `filters` — minus
    /// `field`'s own filter — per value of `field`: those a hybrid search
    /// could return when given the search text's `vector`.
    async fn count_documents_by(
        &self,
        terms: &[String],
        vector: Option<&[f32]>,
        filters: &SearchFilters,
        field: FacetField,
    ) -> Result<Vec<FacetCount<String>>> {
        let mut filters = filters.clone();
        match field {
            FacetField::Category => filters.categories.clear(),
            FacetField::Subcategory => filters.subcategories.clear(),
            FacetField::Type => filters.types.clear(),
            FacetField::ContentType => filters.content_types.clear(),
        }

        match vector {
            Some(vector) => {
                let ids = self.hybrid_matches(terms, vector, &filters).await?;
                self.storage.count_documents_among(&ids, field).await
            }
            None => {
                self.storage
                    .count_documents_by(terms, &filters, field)
                    .await
            }
        }
        .with_context(|| format!("failed to count matching documents by {field:?}"))
    }

    /// The creation date range of the documents matching `terms` and
    /// `filters` — minus the date filters themselves: those a hybrid search
    /// could return when given the search text's `vector`.
    async fn created_at_range(
        &self,
        terms: &[String],
        vector: Option<&[f32]>,
        filters: &SearchFilters,
    ) -> Result<Option<(i64, i64)>> {
        let filters = SearchFilters {
            created_from: None,
            created_until: None,
            ..filters.clone()
        };

        match vector {
            Some(vector) => {
                let ids = self.hybrid_matches(terms, vector, &filters).await?;
                self.storage.created_at_range_among(&ids).await
            }
            None => self.storage.created_at_range(terms, &filters).await,
        }
        .context("failed to find the creation date range of matching documents")
    }
}

#[async_trait]
impl<S: Storage> Service for DocumentsClient<S> {
    /// Reads the PDF file at `path`, encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its
    /// generated id.
    ///
    /// This follows an envelope encryption scheme (see
    /// `changelog::crypto::encrypt_event`): a fresh, random data encryption
    /// key (DEK) is generated for the event, used to encrypt it with
    /// AES-256-GCM, and is itself wrapped under a key-encryption-key before
    /// being sent alongside the ciphertext. Only the wrapped DEK and
    /// ciphertext ever leave this process.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn upload(&self, mut request: UploadRequest) -> Result<Uuid> {
        // Checked before anything else: publishing the event needs a
        // session, so a caller who never logged in gets this specific,
        // recognizable error before any time is spent reading and parsing
        // the document.
        self.changelog
            .ensure_master_key()
            .await
            .context("cannot upload a document before logging in")?;

        resolve_upload_request(&mut request)?;

        let (content, original_name) = match request.source {
            UploadSource::Path(path) => {
                check_extension(&path)?;
                let content = tokio::fs::read(&path)
                    .instrument(tracing::debug_span!("read_document"))
                    .await
                    .with_context(|| format!("failed to read document at {}", path.display()))?;
                let original_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                (content, original_name)
            }
            UploadSource::Raw { name, content } => {
                check_extension(std::path::Path::new(&name))?;
                (content, name)
            }
        };

        // The document is published whole in one event, so one larger than
        // the largest event can never be saved: refused before hashing and
        // parsing it. The event also carries the metadata (transcript
        // included), so `changelog::Service::send` checks the encrypted
        // event itself again.
        if content.len() > MAX_EVENT_BYTES {
            return Err(ErrorKind::DocumentTooLarge {
                what: format!("document {original_name:?}"),
                size: content.len(),
                max: MAX_EVENT_BYTES,
            }
            .into());
        }

        // Checked before extracting the transcript: no point paying for
        // that only to reject the document. This only
        // sees documents the sync job already cached locally, so the consume
        // job checks again before caching one (see `changelog::service`).
        let checksum = checksum(&content);
        if let Some(existing) = self
            .storage
            .find_document_by_checksum(&checksum)
            .instrument(tracing::debug_span!("find_document_by_checksum"))
            .await
            .context("failed to look up local documents with the same content")?
        {
            return Err(ErrorKind::DuplicateDocument(existing).into());
        }

        let mut metadata = parse_content(&content, &original_name, checksum)
            .with_context(|| format!("failed to parse document {original_name:?}"))?;
        if let Some(name) = request.name {
            metadata.name = name;
        }
        if let Some(r#type) = request.r#type {
            metadata.r#type = Some(r#type);
        }
        if let Some(category) = request.category {
            metadata.category = Some(category);
            metadata.subcategory = request.subcategory;
        }
        metadata.attributes.extend(request.attributes);

        self.check_metadata(&metadata)
            .instrument(tracing::debug_span!("check_metadata"))
            .await
            .with_context(|| format!("invalid metadata for document {original_name:?}"))?;

        let id = metadata.id;

        // Never part of the event: only handed over to this client's own
        // consumer, which may receive the event before `send` even returns,
        // hence cached first.
        let embeddings = self
            .embed_upload(&metadata)
            .instrument(tracing::debug_span!("embed_document"))
            .await;
        let cached = embeddings.as_ref().map(|embeddings| {
            self.cache.put(embeddings.clone());
            CachedUntilPublished {
                cache: &*self.cache,
                document_id: id,
            }
        });

        self.changelog
            .send(EventType::SaveDocument, id, Some(&content), Some(&metadata))
            .instrument(tracing::debug_span!("publish_event"))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        if let Some(cached) = cached {
            cached.keep();
        }
        Ok(id)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Document>> {
        self.storage.get_document(id).await
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Metadata>> {
        self.storage.list_documents(offset, limit).await
    }

    async fn search(&self, query: SearchQuery) -> Result<Vec<Metadata>> {
        if query.offset < 0 {
            return Err(ErrorKind::InvalidSearchQuery(format!(
                "offset must not be negative, got {}",
                query.offset
            ))
            .into());
        }
        if query.limit <= 0 {
            return Err(ErrorKind::InvalidSearchQuery(format!(
                "limit must be positive, got {}",
                query.limit
            ))
            .into());
        }
        check_created_range(&query.filters)?;

        let terms = search_terms(&query.text);

        if query.order == SearchOrder::Relevance
            && !terms.is_empty()
            && let Some(vector) = self.embed_query(&query.text).await
        {
            let rankings = self.rank(&terms, &vector, &query.filters).await?;
            let fused = fuse(&rankings);
            let page = page(&fused, query.offset, query.limit);

            let mut found: HashMap<Uuid, Metadata> = self
                .storage
                .get_metadatas(page)
                .await
                .context("failed to load the metadata of the documents found")?
                .into_iter()
                .map(|metadata| (metadata.id, metadata))
                .collect();
            return Ok(page.iter().filter_map(|id| found.remove(id)).collect());
        }

        self.storage
            .search_documents(
                &terms,
                &query.filters,
                query.order,
                query.offset,
                query.limit,
            )
            .await
            .context("failed to search local documents")
    }

    async fn search_facets(&self, text: &str, filters: &SearchFilters) -> Result<SearchFacets> {
        check_created_range(filters)?;

        let terms = search_terms(text);
        // Counting what a hybrid search finds whenever there's text, as
        // `search` does with `SearchOrder::Relevance`: what callers pick when
        // searching text, and the one order that ever finds documents
        // sharing no term with it.
        let vector = if terms.is_empty() {
            None
        } else {
            self.embed_query(text).await
        };
        let vector = vector.as_deref();

        // Each field is counted with its own filter lifted, so the values on
        // offer for it are every alternative compatible with the *rest* of
        // the search, not only the one(s) already picked — while every other
        // field narrows to what's left under the picked ones.
        let categories = self
            .count_documents_by(&terms, vector, filters, FacetField::Category)
            .await?;
        let subcategories = self
            .count_documents_by(&terms, vector, filters, FacetField::Subcategory)
            .await?;
        let types = self
            .count_documents_by(&terms, vector, filters, FacetField::Type)
            .await?;
        let content_types = self
            .count_documents_by(&terms, vector, filters, FacetField::ContentType)
            .await?;
        let created_at_range = self.created_at_range(&terms, vector, filters).await?;

        Ok(SearchFacets {
            categories: parse_facet(categories, "category")?,
            subcategories: parse_facet(subcategories, "subcategory")?,
            types: parse_facet(types, "document type")?,
            content_types,
            earliest_created_at: created_at_range.map(|(earliest, _)| earliest),
            latest_created_at: created_at_range.map(|(_, latest)| latest),
        })
    }

    async fn update_metadata(&self, metadata: Metadata) -> Result<()> {
        let id = metadata.id;
        self.check_metadata(&metadata)
            .await
            .with_context(|| format!("invalid metadata for document {id}"))?;

        self.changelog
            .send(EventType::UpdateMetadata, id, None, Some(&metadata))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        Ok(())
    }

    async fn start_sync(&self) -> Result<()> {
        let on_document_change = self.on_document_change.clone();
        let callback: Box<dyn FnMut(ChangelogEvent) + Send> = Box::new(move |event| {
            if let Some(on_document_change) = &on_document_change {
                on_document_change(event);
            }
        });

        self.changelog.clone().start_consume_job(callback).await
    }

    fn stop_sync(&self) {
        self.changelog.stop_consume_job();
    }

    fn indexing_model_error(&self) -> Option<ErrorCode> {
        self.indexer.model_error()
    }
}

/// The default [`Indexer`]: embeds a document's text in chunks with an
/// injected [`Embedder`], unless an injected [`EmbeddingCache`] holds the
/// embeddings its upload computed, then caches it with its embeddings
/// through an injected [`Storage`], in one call.
pub(crate) struct DocumentIndexer<S: Storage> {
    storage: S,
    embedder: Arc<dyn Embedder>,
    cache: Arc<dyn EmbeddingCache>,
    /// Why the last [`Indexer::prepare`] failed, if it did.
    model_error: Mutex<Option<ErrorCode>>,
}

impl<S: Storage> DocumentIndexer<S> {
    pub(crate) fn new(
        storage: S,
        embedder: Arc<dyn Embedder>,
        cache: Arc<dyn EmbeddingCache>,
    ) -> Self {
        Self {
            storage,
            embedder,
            cache,
            model_error: Mutex::new(None),
        }
    }

    fn set_model_error(&self, error: Option<ErrorCode>) {
        *self
            .model_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = error;
    }
}

#[async_trait]
impl<S: Storage> Indexer for DocumentIndexer<S> {
    async fn prepare(&self) -> Result<()> {
        match self.embedder.load().await {
            Ok(()) => {
                self.set_model_error(None);
                Ok(())
            }
            Err(err) => {
                tracing::error!(
                    code = ?err.code(),
                    "the indexing model is unavailable: documents won't sync until it can be fetched: {err}"
                );
                self.set_model_error(Some(err.code()));
                Err(err).context("the indexing model is unavailable")
            }
        }
    }

    async fn save_document(&self, document: &Document) -> Result<()> {
        let id = document.id;
        let model = self.embedder.model();
        let passages = passages(&document.metadata);
        let key = embedding_key(id, &model, CHUNKER_VERSION, &passages.texts);

        // Reused only if computed for this very text, model and chunking:
        // anything else (an older SDK's chunking, a renamed document...)
        // is embedded again, as with no entry at all.
        let cached = self.cache.get(id);
        let embeddings = match &cached {
            Some(cached) if cached.key == key => cached.embeddings.clone(),
            _ => embed(&*self.embedder, model, passages)
                .await
                .with_context(|| format!("failed to index document {id}"))?,
        };

        self.storage
            .save_document(document, &embeddings)
            .await
            .with_context(|| format!("failed to cache document {id} locally"))?;

        // Only once saved: an event that failed is retried, and should
        // still find its entry then.
        if cached.is_some() {
            self.cache.remove(id);
        }
        Ok(())
    }

    fn skip_document(&self, id: Uuid) {
        self.cache.remove(id);
    }

    async fn update_metadata(&self, id: Uuid, metadata: &Metadata) -> Result<()> {
        let previous = self
            .storage
            .get_metadata(id)
            .await
            .with_context(|| format!("failed to read cached document {id}"))?;

        // Re-embedding takes about a second per document: only when the
        // text it embeds changes, not for a new category or attribute.
        let embeddings = match previous {
            Some(previous)
                if title(&previous.name) != title(&metadata.name)
                    || previous.transcript != metadata.transcript =>
            {
                Some(
                    embed(&*self.embedder, self.embedder.model(), passages(metadata))
                        .await
                        .with_context(|| format!("failed to re-index document {id}"))?,
                )
            }
            _ => None,
        };

        self.storage
            .update_metadata(id, metadata, embeddings)
            .await
            .with_context(|| format!("failed to update cached document {id} locally"))
    }

    fn model_error(&self) -> Option<ErrorCode> {
        *self
            .model_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A document's text as it's embedded: each chunk of its transcript (see
/// [`chunk::chunk`]), after its title (see [`title`]).
pub(super) struct Passages {
    /// Where each chunk is in the transcript.
    pub(super) ranges: Vec<Range<usize>>,
    /// The text embedded for each chunk.
    pub(super) texts: Vec<String>,
}

/// The passages `metadata`'s text is embedded as. A blank transcript has no
/// chunks, so none. The one function both uploads and [`DocumentIndexer`]
/// build them with, so their [`embedding_key`]s match.
pub(super) fn passages(metadata: &Metadata) -> Passages {
    let ranges = chunk::chunk(&metadata.transcript, CHUNK_CHARS);
    let title = title(&metadata.name);
    let texts = ranges
        .iter()
        .map(|range| format!("{title}\n{}", &metadata.transcript[range.clone()]))
        .collect();
    Passages { ranges, texts }
}

/// The key embeddings of `document_id`'s `passages` (see [`passages`]) are
/// cached under: what they were computed from, the text only by its
/// SHA-256. Each passage is hashed after its length, so splitting the same
/// text differently changes it.
pub(super) fn embedding_key(
    document_id: Uuid,
    model: &str,
    chunker_version: u32,
    passages: &[String],
) -> EmbeddingKey {
    let mut hasher = Sha256::new();
    for passage in passages {
        hasher.update((passage.len() as u64).to_le_bytes());
        hasher.update(passage.as_bytes());
    }
    EmbeddingKey {
        document_id,
        model: model.to_string(),
        chunker_version,
        text_sha256: hasher.finalize().into(),
    }
}

/// Embeds `passages` with `embedder`, as `model`'s embeddings. None for no
/// passages at all, without calling `embedder`.
async fn embed(
    embedder: &dyn Embedder,
    model: String,
    passages: Passages,
) -> Result<DocumentEmbeddings> {
    let Passages { ranges, texts } = passages;
    if ranges.is_empty() {
        return Ok(DocumentEmbeddings {
            model,
            chunks: Vec::new(),
        });
    }

    let vectors = embedder
        .embed_passages(texts)
        .await
        .context("failed to embed the document's text")?;
    if vectors.len() != ranges.len() {
        return Err(ErrorKind::InvalidModel(format!(
            "the embedding model returned {} vectors for {} chunks",
            vectors.len(),
            ranges.len()
        ))
        .into());
    }

    Ok(DocumentEmbeddings {
        model,
        chunks: ranges
            .into_iter()
            .zip(vectors)
            .map(|(range, vector)| ChunkEmbedding { range, vector })
            .collect(),
    })
}

/// Fuses `rankings` (ids, best first) with Reciprocal Rank Fusion: each
/// document scores `1 / (RRF_K + rank)` (from rank 1) in every ranking it's
/// in, summed, best first. Ties go to the best single rank, then to the
/// earliest ranking it's in.
fn fuse(rankings: &[Vec<Uuid>]) -> Vec<Uuid> {
    struct Fused {
        score: f64,
        best_rank: usize,
        best_ranking: usize,
    }

    let mut fused = HashMap::<Uuid, Fused>::new();
    for (ranking, ids) in rankings.iter().enumerate() {
        for (i, id) in ids.iter().enumerate() {
            let rank = i + 1;
            let entry = fused.entry(*id).or_insert(Fused {
                score: 0.0,
                best_rank: rank,
                best_ranking: ranking,
            });
            entry.score += 1.0 / (RRF_K + rank as f64);
            if rank < entry.best_rank {
                entry.best_rank = rank;
                entry.best_ranking = ranking;
            }
        }
    }

    let mut fused = fused.into_iter().collect::<Vec<_>>();
    fused.sort_by(|(_, a), (_, b)| {
        b.score
            .total_cmp(&a.score)
            .then(a.best_rank.cmp(&b.best_rank))
            .then(a.best_ranking.cmp(&b.best_ranking))
    });
    fused.into_iter().map(|(id, _)| id).collect()
}

/// The page of `ids` skipping `offset` then at most `limit` long (both
/// already checked not negative), empty past the end.
fn page(ids: &[Uuid], offset: i64, limit: i64) -> &[Uuid] {
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(ids.len());
    let end = start
        .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
        .min(ids.len());
    &ids[start..end]
}

/// The title a document's text is embedded under: its name without its
/// extension, like a file stem (`"Facture mars.pdf"` is `"Facture mars"`).
fn title(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// Completes `request` with what its fields imply — a subcategory alone
/// implies its category — and rejects one whose fields contradict each
/// other. Shared with the dev [`Service`] (see `super::dev`), so a scraper
/// run without a server fails on the same requests a real upload would.
pub(super) fn resolve_upload_request(request: &mut UploadRequest) -> Result<()> {
    if request.category.is_none() {
        request.category = request
            .subcategory
            .map(|subcategory| subcategory.category());
    }
    check_categories(request.category, request.subcategory)?;
    // Only what can be checked without a server: the dev `Service` has none
    // to fetch attribute definitions from. `upload` checks them in full once
    // the document is parsed.
    attributes::check_values(&request.attributes)
}

/// Rejects a subcategory that doesn't narrow down `category` (or that's
/// set without any category).
pub(super) fn check_categories(
    category: Option<Category>,
    subcategory: Option<Subcategory>,
) -> Result<()> {
    if let Some(subcategory) = subcategory
        && Some(subcategory.category()) != category
    {
        return Err(ErrorKind::MismatchedSubcategory(format!(
            "subcategory {subcategory} belongs to {}, not {}",
            subcategory.category(),
            category.map_or("no category".to_string(), |category| category.to_string())
        ))
        .into());
    }
    Ok(())
}

/// Rejects a `created_from` later than `created_until`, a range no
/// document can fall in.
fn check_created_range(filters: &SearchFilters) -> Result<()> {
    if let (Some(from), Some(until)) = (filters.created_from, filters.created_until)
        && from > until
    {
        return Err(ErrorKind::InvalidSearchQuery(format!(
            "created_from ({from}) is later than created_until ({until})"
        ))
        .into());
    }
    Ok(())
}

/// Parses the canonical string form [`Storage::count_documents_by`] returns
/// each value of a classification field in (e.g. `"real_estate"`) back into
/// its enum, `field` naming it in the error.
fn parse_facet<T: std::str::FromStr<Err = crate::Error>>(
    counts: Vec<FacetCount<String>>,
    field: &str,
) -> Result<Vec<FacetCount<T>>> {
    counts
        .into_iter()
        .map(|facet| {
            Ok(FacetCount {
                value: facet.value.parse().with_context(|| {
                    format!("invalid {field} {:?} in local database", facet.value)
                })?,
                count: facet.count,
            })
        })
        .collect()
}

/// Splits a search's free text into the words that must each match: runs
/// of letters/digits, everything else (spaces, punctuation, quotes...)
/// separating them — the same boundaries the index tokenizes documents on,
/// so "relevé-2024" searches for "relevé" and "2024".
fn search_terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

/// Validates that `name`'s extension is a supported document type; only
/// `.pdf` is currently accepted.
fn check_extension(name: &std::path::Path) -> Result<()> {
    let extension = name
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension != "pdf" {
        return Err(ErrorKind::UnsupportedDocumentExtension(extension).into());
    }
    Ok(())
}

/// The hex-encoded SHA-256 checksum of a document's `content`, as stored in
/// its [`Metadata::checksum`].
#[tracing::instrument(level = "debug", skip_all, fields(size = content.len()))]
fn checksum(content: &[u8]) -> String {
    crate::tools::hex::encode(&Sha256::digest(content))
}

/// Builds the [`Metadata`] of a freshly uploaded PDF `content`, originally
/// named `original_name`, whose [`checksum`] is `checksum`: a new id, its
/// name (the file name without extension), size and transcript. Its type,
/// classification and attributes are left empty, for the upload request to
/// fill in.
#[tracing::instrument(level = "debug", skip(content, checksum), fields(size = content.len()))]
fn parse_content(content: &[u8], original_name: &str, checksum: String) -> Result<Metadata> {
    let name = Path::new(original_name)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let transcript =
        transcript::extract(content).context("failed to extract document transcript")?;

    Ok(Metadata {
        id: Uuid::now_v7(),
        original_name: original_name.to_string(),
        name,
        content_type: PDF_CONTENT_TYPE.to_string(),
        created_at,
        size: content.len() as u64,
        checksum,
        transcript,
        r#type: None,
        category: None,
        subcategory: None,
        attributes: BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Mutex;

    use tempfile::NamedTempFile;

    use super::super::attributes::MockService as MockAttributesService;
    use super::super::embedder::MockEmbedder;
    use super::super::models::{FakeCachedEmbeddings, FakeChunkEmbedding, FakeDocumentEmbeddings};
    use super::super::storage::MockStorage;
    use super::super::transcript::tests::build_pdf;
    use super::super::{Attribute, DocumentType};
    use super::super::{FakeDocument, FakeMetadata, MockEmbeddingCache, MockIndexer};
    use super::*;

    /// An [`Indexer`] fake for tests that never sync.
    fn unused_indexer() -> Arc<dyn Indexer> {
        Arc::new(MockIndexer::new())
    }

    /// An [`AttributesService`] fake accepting any attributes, for tests
    /// that don't exercise attribute checks.
    /// An embedder whose every call panics: for tests that never search.
    fn unused_embedder() -> Arc<dyn Embedder> {
        Arc::new(MockEmbedder::new())
    }

    /// An embedder whose model can't be fetched: for tests whose uploads
    /// are then published without embeddings.
    fn embedder_without_model() -> Arc<dyn Embedder> {
        let mut embedder = MockEmbedder::new();
        embedder.expect_model().return_const("e5.onnx".to_string());
        embedder
            .expect_embed_passages()
            .returning(|_| Err(ErrorKind::ModelDownloadStatus(503).into()));
        Arc::new(embedder)
    }

    /// An embedding cache whose every call panics: for tests that never
    /// embed an upload.
    fn unused_cache() -> Arc<dyn EmbeddingCache> {
        Arc::new(MockEmbeddingCache::new())
    }

    fn accepting_attributes() -> Arc<dyn AttributesService> {
        let mut attributes = MockAttributesService::new();
        attributes.expect_check().returning(|_| Ok(()));
        Arc::new(attributes)
    }

    /// An [`AttributesService`] fake rejecting any attributes, as it would
    /// an invalid one.
    fn rejecting_attributes() -> Arc<dyn AttributesService> {
        let mut attributes = MockAttributesService::new();
        attributes
            .expect_check()
            .returning(|_| Err(crate::ErrorKind::InvalidAttribute("rejected".to_string()).into()));
        Arc::new(attributes)
    }

    #[tokio::test]
    async fn upload_fails_fast_with_no_master_key_before_ever_reading_the_document() {
        let file = write_temp_file("pdf", b"hello world");

        let changelog = Arc::new(RecordingChangelog {
            no_master_key: true,
            ..Default::default()
        });
        // `MockStorage` panics on any call, proving `upload` never reaches
        // the duplicate check (or anything after it) once this check fails.
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog,
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let err = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("no master key found"),
            "unexpected error: {err}"
        );
    }

    /// A single `send` call recorded by [`RecordingChangelog`].
    type SentEvent = (EventType, Uuid, Option<Vec<u8>>, Option<Metadata>);

    /// A [`ChangelogService`] fake recording every event passed to
    /// [`ChangelogService::send`], and replaying `to_consume` through
    /// [`ChangelogService::start_consume_job`]'s callback, for tests that
    /// don't need a live server.
    #[derive(Default)]
    struct RecordingChangelog {
        sent: Mutex<Vec<SentEvent>>,
        to_consume: Mutex<Vec<ChangelogEvent>>,
        stopped: std::sync::atomic::AtomicBool,
        /// When set, `ensure_master_key` fails the same way the real
        /// `ChangelogClient` does before any account has ever logged in —
        /// for tests exercising `Service::upload`'s fail-fast check.
        no_master_key: bool,
        /// When set, `send` records the event, then fails.
        fail_send: bool,
        /// When set, `send` records the event, notifies it, then never
        /// returns.
        hang_send: Option<Arc<tokio::sync::Notify>>,
    }

    #[async_trait]
    impl ChangelogService for RecordingChangelog {
        async fn ensure_master_key(&self) -> Result<()> {
            if self.no_master_key {
                return Err(ErrorKind::Encryption(
                    "no master key found in local settings; log in or create an account first"
                        .into(),
                )
                .into());
            }
            Ok(())
        }

        async fn send(
            &self,
            event_type: EventType,
            subject_id: Uuid,
            content: Option<&[u8]>,
            metadata: Option<&Metadata>,
        ) -> Result<()> {
            self.sent.lock().unwrap().push((
                event_type,
                subject_id,
                content.map(<[u8]>::to_vec),
                metadata.cloned(),
            ));
            if self.fail_send {
                return Err(ErrorKind::InvalidChangelogEvent("refused".to_string()).into());
            }
            if let Some(reached) = &self.hang_send {
                reached.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(())
        }

        async fn start_consume_job(
            self: Arc<Self>,
            mut callback: Box<dyn FnMut(ChangelogEvent) + Send>,
        ) -> Result<()> {
            for event in self.to_consume.lock().unwrap().drain(..) {
                callback(event);
            }
            Ok(())
        }

        fn stop_consume_job(&self) {
            self.stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Writes `content` to a temporary file with the given `extension`, for
    /// tests that need a real path to pass to [`Service::upload`].
    fn write_temp_file(extension: &str, content: &[u8]) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .unwrap();
        file.write_all(content).unwrap();
        file
    }

    #[tokio::test]
    async fn upload_publishes_a_save_document_event_for_the_document() {
        let pdf = build_pdf("hello world");
        let file = write_temp_file("pdf", &pdf);
        let file_name = file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let mut storage = MockStorage::new();
        let expected_checksum = checksum(&pdf);
        storage
            .expect_find_document_by_checksum()
            .withf(move |checksum| checksum == expected_checksum)
            .times(1)
            .returning(|_| Ok(None));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            embedder_without_model(),
            unused_cache(),
        );

        let id = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, document_id, content, sent_metadata) = &sent[0];
        assert_eq!(*event_type, EventType::SaveDocument);
        assert_eq!(*document_id, id);
        assert_eq!(content.as_deref(), Some(pdf.as_slice()));
        let sent_metadata = sent_metadata.as_ref().unwrap();
        assert_eq!(sent_metadata.id, id);
        assert_eq!(sent_metadata.original_name, file_name);
        assert_eq!(sent_metadata.name, file_name.strip_suffix(".pdf").unwrap());
        assert_eq!(sent_metadata.content_type, PDF_CONTENT_TYPE);
        assert_eq!(sent_metadata.size, pdf.len() as u64);
        assert_eq!(sent_metadata.checksum, checksum(&pdf));
        assert!(
            sent_metadata.transcript.contains("hello world"),
            "unexpected transcript: {:?}",
            sent_metadata.transcript
        );
        assert_eq!(sent_metadata.r#type, None);
        assert_eq!(sent_metadata.category, None);
        assert_eq!(sent_metadata.subcategory, None);
        assert!(sent_metadata.attributes.is_empty());
    }

    #[tokio::test]
    async fn upload_rejects_a_document_already_saved_locally_before_parsing_or_publishing_it() {
        // Not a valid PDF: parsing it would fail, so this also proves the
        // duplicate is rejected before parsing.
        let pdf = b"hello world".to_vec();
        let file = write_temp_file("pdf", &pdf);
        let existing = Uuid::now_v7();

        let mut storage = MockStorage::new();
        let expected_checksum = checksum(&pdf);
        storage
            .expect_find_document_by_checksum()
            .withf(move |checksum| checksum == expected_checksum)
            .times(1)
            .returning(move |_| Ok(Some(existing)));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let err = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap_err();

        assert!(
            matches!(err.kind(), ErrorKind::DuplicateDocument(id) if *id == existing),
            "unexpected error: {err}"
        );
        assert_eq!(err.code(), crate::ErrorCode::DuplicateDocument);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upload_rejects_non_pdf_extensions() {
        let file = write_temp_file("txt", b"hello world");
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let result = client.upload(UploadRequest::from_path(file.path())).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn upload_refuses_a_document_larger_than_an_event_before_reading_it() {
        let changelog = Arc::new(RecordingChangelog::default());
        // No `expect_find_document_by_checksum()`: the mock panics if the
        // document gets as far as the duplicate check.
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let err = client
            .upload(UploadRequest::from_raw(
                "huge.pdf".to_string(),
                vec![0; MAX_EVENT_BYTES + 1],
            ))
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::DocumentTooLarge);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upload_applies_metadata_overrides_from_the_request() {
        let raw = build_pdf("hello world");

        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(1)
            .returning(|_| Ok(None));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            embedder_without_model(),
            unused_cache(),
        );

        let request = UploadRequest {
            source: super::super::UploadSource::Raw {
                name: "raw.pdf".to_string(),
                content: raw.clone(),
            },
            name: Some("overridden.pdf".to_string()),
            r#type: Some(DocumentType::Payslip),
            category: Some(super::super::Category::Work),
            subcategory: None,
            attributes: BTreeMap::new(),
        };

        client.upload(request).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (_, _, _, sent_metadata) = &sent[0];
        let sent_metadata = sent_metadata.as_ref().unwrap();
        assert_eq!(sent_metadata.original_name, "raw.pdf");
        assert_eq!(sent_metadata.name, "overridden.pdf");
        assert_eq!(sent_metadata.r#type, Some(DocumentType::Payslip));
        assert_eq!(sent_metadata.category, Some(super::super::Category::Work));
    }

    /// Uploads a PDF with `request`'s overrides, returning the metadata it
    /// was published with.
    async fn upload_with(mut request: UploadRequest) -> Metadata {
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(1)
            .returning(|_| Ok(None));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            embedder_without_model(),
            unused_cache(),
        );

        request.source = super::super::UploadSource::Raw {
            name: "raw.pdf".to_string(),
            content: build_pdf("hello world"),
        };
        client.upload(request).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        sent[0].3.clone().unwrap()
    }

    #[tokio::test]
    async fn upload_infers_the_category_of_a_lone_subcategory() {
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.subcategory = Some(Subcategory::Energy);

        let metadata = upload_with(request).await;

        assert_eq!(metadata.category, Some(Category::Housing));
        assert_eq!(metadata.subcategory, Some(Subcategory::Energy));
    }

    #[tokio::test]
    async fn upload_rejects_a_subcategory_of_another_category_before_parsing() {
        // `MockStorage` panics on any call, proving the request is rejected
        // before the duplicate check and parsing.
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.category = Some(Category::Money);
        request.subcategory = Some(Subcategory::Energy);

        let err = client.upload(request).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::MismatchedSubcategory);
    }

    #[tokio::test]
    async fn upload_rejects_attributes_the_attributes_service_rejects() {
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            rejecting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );
        let mut request = UploadRequest::from_raw("raw.pdf", build_pdf("hello world"));
        request.attributes = BTreeMap::from([("price".to_string(), Attribute::Integer(1))]);

        let err = client.upload(request).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upload_keeps_the_requested_attributes() {
        let attributes = BTreeMap::from([
            ("price".to_string(), Attribute::Float(32.3)),
            ("paid".to_string(), Attribute::Boolean(true)),
        ]);
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.attributes = attributes.clone();

        let metadata = upload_with(request).await;

        assert_eq!(metadata.attributes, attributes);
    }

    #[tokio::test]
    async fn upload_rejects_a_requested_non_finite_attribute_before_parsing() {
        // `MockStorage` panics on any call, proving the request is rejected
        // before the duplicate check and parsing.
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.attributes = BTreeMap::from([("price".to_string(), Attribute::Float(f64::NAN))]);

        let err = client.upload(request).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
    }

    #[tokio::test]
    async fn update_metadata_rejects_attributes_the_attributes_service_rejects() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            rejecting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );
        let metadata = super::super::FakeMetadata::new()
            .with_attributes([("price".to_string(), Attribute::Integer(1))])
            .build();

        let err = client.update_metadata(metadata).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_metadata_publishes_the_given_attributes() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );
        let attributes = BTreeMap::from([("price".to_string(), Attribute::Float(32.3))]);
        let metadata = super::super::FakeMetadata::new()
            .with_attributes(attributes.clone())
            .build();

        client.update_metadata(metadata).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent[0].3.as_ref().unwrap().attributes, attributes);
    }

    #[tokio::test]
    async fn update_metadata_rejects_a_subcategory_of_another_category() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );
        let metadata = super::super::FakeMetadata::new()
            .with_category(Category::Money)
            .with_subcategory(Subcategory::Energy)
            .build();

        let err = client.update_metadata(metadata).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::MismatchedSubcategory);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_returns_a_document_previously_cached_locally() {
        let id = Uuid::now_v7();
        let document = Document {
            id,
            content: b"hello".to_vec(),
            metadata: Metadata {
                id,
                name: "report.pdf".to_string(),
                original_name: "report.pdf".to_string(),
                content_type: PDF_CONTENT_TYPE.to_string(),
                created_at: 1_700_000_000,
                size: 5,
                checksum: "deadbeef".to_string(),
                transcript: String::new(),
                r#type: None,
                category: None,
                subcategory: None,
                attributes: BTreeMap::new(),
            },
        };

        let mut storage = MockStorage::new();
        let expected = document.clone();
        storage
            .expect_get_document()
            .withf(move |queried_id| *queried_id == id)
            .times(1)
            .returning(move |_| Ok(Some(expected.clone())));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        assert_eq!(client.get(id).await.unwrap(), Some(document));
    }

    #[tokio::test]
    async fn list_pages_through_documents_previously_cached_locally() {
        let page = vec![
            super::super::FakeMetadata::new().with_name("two").build(),
            super::super::FakeMetadata::new().with_name("three").build(),
        ];

        let mut storage = MockStorage::new();
        let expected_page = page.clone();
        storage
            .expect_list_documents()
            .withf(|offset, limit| *offset == 1 && *limit == 2)
            .times(1)
            .returning(move |_, _| Ok(expected_page.clone()));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let page = client.list(1, 2).await.unwrap();

        assert_eq!(
            page.into_iter()
                .map(|metadata| metadata.name)
                .collect::<Vec<_>>(),
            vec!["two", "three"]
        );
    }

    #[tokio::test]
    async fn search_splits_the_text_into_terms_and_forwards_the_query_to_storage() {
        let filters = super::super::SearchFilters {
            categories: vec![super::super::Category::Money],
            created_from: Some(1),
            created_until: Some(2),
            ..Default::default()
        };
        let found = vec![super::super::FakeMetadata::new().build()];

        let mut storage = MockStorage::new();
        let expected_filters = filters.clone();
        let returned = found.clone();
        storage
            .expect_search_documents()
            .withf(move |terms, filters, order, offset, limit| {
                terms == ["Relevé", "2024", "EDF"]
                    && *filters == expected_filters
                    && *order == super::super::SearchOrder::Oldest
                    && *offset == 10
                    && *limit == 5
            })
            .times(1)
            .returning(move |_, _, _, _, _| Ok(returned.clone()));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let documents = client
            .search(SearchQuery {
                text: "  Relevé-2024 \"EDF\" ".to_string(),
                filters,
                order: super::super::SearchOrder::Oldest,
                offset: 10,
                limit: 5,
            })
            .await
            .unwrap();

        assert_eq!(documents, found);
    }

    #[tokio::test]
    async fn search_forwards_no_terms_for_blank_text() {
        let mut storage = MockStorage::new();
        storage
            .expect_search_documents()
            .withf(|terms, _, _, _, _| terms.is_empty())
            .times(1)
            .returning(|_, _, _, _, _| Ok(Vec::new()));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        client.search(SearchQuery::new(" - ", 0, 10)).await.unwrap();
    }

    #[tokio::test]
    async fn search_rejects_invalid_queries_without_querying_storage() {
        let mut inverted_dates = SearchQuery::new("", 0, 10);
        inverted_dates.filters.created_from = Some(2);
        inverted_dates.filters.created_until = Some(1);

        for query in [
            SearchQuery::new("", -1, 10),
            SearchQuery::new("", 0, 0),
            inverted_dates,
        ] {
            // No `expect_search_documents()`: storage must never be reached.
            let client = DocumentsClient::new(
                MockStorage::new(),
                Arc::new(RecordingChangelog::default()),
                None,
                accepting_attributes(),
                unused_indexer(),
                unused_embedder(),
                unused_cache(),
            );

            let err = client.search(query.clone()).await.unwrap_err();

            assert_eq!(
                err.code(),
                crate::ErrorCode::InvalidSearchQuery,
                "query {query:?}"
            );
        }
    }

    /// `filters` minus `field`'s own filter: what the service must pass to
    /// storage when counting `field`.
    fn without_own_filter(
        filters: &super::super::SearchFilters,
        field: FacetField,
    ) -> super::super::SearchFilters {
        let mut filters = filters.clone();
        match field {
            FacetField::Category => filters.categories.clear(),
            FacetField::Subcategory => filters.subcategories.clear(),
            FacetField::Type => filters.types.clear(),
            FacetField::ContentType => filters.content_types.clear(),
        }
        filters
    }

    #[tokio::test]
    async fn search_facets_counts_term_matches_when_the_text_cant_be_embedded() {
        use super::super::{Category, SearchFilters, Subcategory};

        let filters = SearchFilters {
            categories: vec![Category::Money],
            subcategories: vec![Subcategory::Banking],
            types: vec![DocumentType::Statement],
            content_types: vec!["application/pdf".to_string()],
            created_from: Some(1),
            created_until: Some(2),
        };
        let expected = super::super::FakeSearchFacets::new()
            .with_created_at_range(Some((10, 20)))
            .build();
        let as_strings = |values: Vec<String>, counts: Vec<u64>| -> Vec<FacetCount<String>> {
            values
                .into_iter()
                .zip(counts)
                .map(|(value, count)| FacetCount { value, count })
                .collect()
        };
        let per_field = [
            (
                FacetField::Category,
                as_strings(
                    expected
                        .categories
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.categories.iter().map(|f| f.count).collect(),
                ),
            ),
            (
                FacetField::Subcategory,
                as_strings(
                    expected
                        .subcategories
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.subcategories.iter().map(|f| f.count).collect(),
                ),
            ),
            (
                FacetField::Type,
                as_strings(
                    expected
                        .types
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.types.iter().map(|f| f.count).collect(),
                ),
            ),
            (FacetField::ContentType, expected.content_types.clone()),
        ];

        let mut storage = MockStorage::new();
        for (field, returned) in per_field {
            let expected_filters = without_own_filter(&filters, field);
            storage
                .expect_count_documents_by()
                .withf(move |terms, filters, queried_field| {
                    terms == ["facture"] && *filters == expected_filters && *queried_field == field
                })
                .times(1)
                .returning(move |_, _, _| Ok(returned.clone()));
        }
        let date_filters = SearchFilters {
            created_from: None,
            created_until: None,
            ..filters.clone()
        };
        storage
            .expect_created_at_range()
            .withf(move |terms, filters| terms == ["facture"] && *filters == date_filters)
            .times(1)
            .returning(|_, _| Ok(Some((10, 20))));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unembeddable_queries(),
            unused_cache(),
        );

        let facets = client.search_facets(" facture ", &filters).await.unwrap();

        assert_eq!(facets, expected);
    }

    #[tokio::test]
    async fn search_facets_fails_on_an_unknown_classification_value() {
        let mut storage = MockStorage::new();
        storage
            .expect_count_documents_by()
            .returning(|_, _, field| {
                Ok(if field == FacetField::Category {
                    vec![FacetCount {
                        value: "not-a-category".to_string(),
                        count: 1,
                    }]
                } else {
                    Vec::new()
                })
            });
        storage.expect_created_at_range().returning(|_, _| Ok(None));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let err = client
            .search_facets("", &super::super::SearchFilters::default())
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidCategory);
    }

    #[tokio::test]
    async fn search_facets_rejects_an_inverted_date_range_without_querying_storage() {
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        let err = client
            .search_facets(
                "",
                &super::super::SearchFilters {
                    created_from: Some(2),
                    created_until: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidSearchQuery);
    }

    /// An embedder that can't embed queries: its model isn't downloaded.
    fn unembeddable_queries() -> Arc<dyn Embedder> {
        let mut embedder = MockEmbedder::new();
        embedder.expect_embed_query().returning(|_| {
            Err(ErrorKind::ModelNotDownloaded("multilingual-e5-small.onnx".to_string()).into())
        });
        Arc::new(embedder)
    }

    const QUERY_MODEL: &str = "e5";

    /// An embedder embedding exactly `text` once as `vector`, under
    /// [`QUERY_MODEL`].
    fn query_embedder(text: &'static str, vector: Vec<f32>) -> Arc<dyn Embedder> {
        let mut embedder = MockEmbedder::new();
        embedder
            .expect_embed_query()
            .withf(move |query| query == text)
            .times(1)
            .returning(move |_| Ok(vector.clone()));
        embedder
            .expect_model()
            .return_const(QUERY_MODEL.to_string());
        Arc::new(embedder)
    }

    fn ids(count: usize) -> Vec<Uuid> {
        (0..count).map(|_| Uuid::new_v4()).collect()
    }

    #[test]
    fn fuse_ranks_a_document_in_both_rankings_above_ones_in_a_single_ranking() {
        let [both, by_terms, by_similarity] = ids(3).try_into().unwrap();

        let fused = fuse(&[vec![by_terms, both], vec![by_similarity, both]]);

        // 2 / 62 for `both`, against 1 / 61 for the others.
        assert_eq!(fused, [both, by_terms, by_similarity]);
    }

    #[test]
    fn fuse_breaks_ties_by_best_rank_then_by_earliest_ranking() {
        // At rank 62 in both rankings, `both` scores 2 / 122: exactly as much
        // as the first of either ranking, 1 / 61.
        let [both, first_by_terms, first_by_similarity] = ids(3).try_into().unwrap();
        let mut by_terms = vec![first_by_terms];
        by_terms.extend(ids(60));
        by_terms.push(both);
        let mut by_similarity = vec![first_by_similarity];
        by_similarity.extend(ids(60));
        by_similarity.push(both);

        let fused = fuse(&[by_terms, by_similarity]);

        assert_eq!(fused[..3], [first_by_terms, first_by_similarity, both]);
        assert_eq!(fused.len(), 123);
    }

    #[test]
    fn fuse_keeps_a_lone_ranking_in_its_order() {
        let ranking = ids(3);

        assert_eq!(fuse(&[ranking.clone(), Vec::new()]), ranking);
        assert_eq!(fuse(&[Vec::new(), ranking.clone()]), ranking);
        assert!(fuse(&[Vec::new(), Vec::new()]).is_empty());
    }

    #[test]
    fn page_skips_offset_then_keeps_at_most_limit() {
        let all = ids(5);

        assert_eq!(page(&all, 0, 2), &all[..2]);
        assert_eq!(page(&all, 2, 2), &all[2..4]);
        assert_eq!(page(&all, 4, 2), &all[4..], "the last page is partial");
        assert_eq!(page(&all, 0, i64::MAX), &all[..]);
        assert!(page(&all, 5, 2).is_empty());
        assert!(page(&all, 6, 2).is_empty(), "offset past the end");
        assert!(page(&all, i64::MAX, i64::MAX).is_empty());
    }

    /// A client over `storage`, embedding queries with `embedder`.
    fn searching(
        storage: MockStorage,
        embedder: Arc<dyn Embedder>,
    ) -> DocumentsClient<MockStorage> {
        DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_indexer(),
            embedder,
            unused_cache(),
        )
    }

    /// Expects one ranking of each kind, returning `by_terms` and
    /// `by_similarity`.
    fn expect_rankings(
        storage: &mut MockStorage,
        by_terms: Vec<Uuid>,
        by_similarity: Vec<(Uuid, f32)>,
    ) {
        storage
            .expect_rank_by_terms()
            .times(1)
            .returning(move |_, _, _| Ok(by_terms.clone()));
        storage
            .expect_rank_by_similarity()
            .times(1)
            .returning(move |_, _, _, _| Ok(by_similarity.clone()));
    }

    /// Expects the metadata of exactly `ids` to be loaded once, returning it
    /// in reverse order.
    fn expect_page(storage: &mut MockStorage, ids: Vec<Uuid>) {
        storage
            .expect_get_metadatas()
            .withf({
                let ids = ids.clone();
                move |requested| requested == ids
            })
            .times(1)
            .returning(move |_| {
                Ok(ids
                    .iter()
                    .rev()
                    .map(|id| super::super::FakeMetadata::new().with_id(*id).build())
                    .collect())
            });
    }

    fn found_ids(found: Vec<Metadata>) -> Vec<Uuid> {
        found.into_iter().map(|metadata| metadata.id).collect()
    }

    #[tokio::test]
    async fn search_by_relevance_fuses_both_rankings_and_loads_only_the_requested_page() {
        let [both, by_terms, by_similarity] = ids(3).try_into().unwrap();
        let filters = super::super::SearchFilters {
            categories: vec![super::super::Category::Money],
            ..Default::default()
        };
        let mut storage = MockStorage::new();
        let expected_filters = filters.clone();
        storage
            .expect_rank_by_terms()
            .withf(move |terms, filters, limit| {
                terms == ["impôts", "2016"] && *filters == expected_filters && *limit == CANDIDATES
            })
            .times(1)
            .returning(move |_, _, _| Ok(vec![both, by_terms]));
        let expected_filters = filters.clone();
        storage
            .expect_rank_by_similarity()
            .withf(move |query, model, filters, limit| {
                query == [0.6, 0.8]
                    && model == QUERY_MODEL
                    && *filters == expected_filters
                    && *limit == CANDIDATES
            })
            .times(1)
            .returning(move |_, _, _, _| Ok(vec![(by_similarity, 0.9), (both, 0.85)]));
        // Fused: `both`, `by_similarity` (1 / 61), `by_terms` (1 / 62).
        expect_page(&mut storage, vec![by_similarity, by_terms]);

        let found = searching(storage, query_embedder("impôts 2016 ?", vec![0.6, 0.8]))
            .search(SearchQuery {
                text: " impôts 2016 ? ".to_string(),
                filters,
                order: super::super::SearchOrder::Relevance,
                offset: 1,
                limit: 5,
            })
            .await
            .unwrap();

        assert_eq!(found_ids(found), [by_similarity, by_terms]);
    }

    #[tokio::test]
    async fn search_by_relevance_loads_nothing_past_the_last_match() {
        let mut storage = MockStorage::new();
        expect_rankings(&mut storage, ids(2), Vec::new());
        expect_page(&mut storage, Vec::new());

        let found = searching(storage, query_embedder("impôts", vec![1.0]))
            .search(SearchQuery::new("impôts", 2, 10))
            .await
            .unwrap();

        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn search_by_relevance_finds_a_document_without_vectors_by_its_terms() {
        let id = Uuid::new_v4();
        let mut storage = MockStorage::new();
        expect_rankings(&mut storage, vec![id], Vec::new());
        expect_page(&mut storage, vec![id]);

        let found = searching(storage, query_embedder("impôts", vec![1.0]))
            .search(SearchQuery::new("impôts", 0, 10))
            .await
            .unwrap();

        assert_eq!(found_ids(found), [id]);
    }

    #[tokio::test]
    async fn search_by_relevance_drops_similarities_below_the_floor() {
        let [at_floor, below_floor] = ids(2).try_into().unwrap();
        let mut storage = MockStorage::new();
        expect_rankings(
            &mut storage,
            Vec::new(),
            vec![
                (at_floor, SIMILARITY_FLOOR),
                (below_floor, SIMILARITY_FLOOR - 0.001),
            ],
        );
        expect_page(&mut storage, vec![at_floor]);

        let found = searching(storage, query_embedder("impôts", vec![1.0]))
            .search(SearchQuery::new("impôts", 0, 10))
            .await
            .unwrap();

        assert_eq!(found_ids(found), [at_floor]);
    }

    #[tokio::test]
    async fn search_by_relevance_ranks_by_terms_alone_when_the_text_cant_be_embedded() {
        for err in [
            ErrorKind::ModelNotDownloaded("multilingual-e5-small.onnx".to_string()),
            ErrorKind::InvalidModel("broken".to_string()),
        ] {
            let mut embedder = MockEmbedder::new();
            let case = format!("{err:?}");
            let mut err = Some(err);
            embedder
                .expect_embed_query()
                .times(1)
                .returning(move |_| Err(err.take().unwrap().into()));
            let found = vec![super::super::FakeMetadata::new().build()];
            let mut storage = MockStorage::new();
            let returned = found.clone();
            storage
                .expect_search_documents()
                .withf(|terms, _, order, offset, limit| {
                    terms == ["impôts"]
                        && *order == super::super::SearchOrder::Relevance
                        && *offset == 0
                        && *limit == 10
                })
                .times(1)
                .returning(move |_, _, _, _, _| Ok(returned.clone()));

            let documents = searching(storage, Arc::new(embedder))
                .search(SearchQuery::new("impôts", 0, 10))
                .await
                .unwrap();

            assert_eq!(documents, found, "{case}");
        }
    }

    #[tokio::test]
    async fn search_by_relevance_fails_with_context_when_a_ranking_fails() {
        let mut storage = MockStorage::new();
        storage
            .expect_rank_by_terms()
            .returning(|_, _, _| Err(ErrorKind::InvalidSearchQuery("boom".to_string()).into()));

        let err = searching(storage, query_embedder("impôts", vec![1.0]))
            .search(SearchQuery::new("impôts", 0, 10))
            .await
            .unwrap_err();

        assert!(err.has_context());
    }

    #[tokio::test]
    async fn search_never_embeds_by_date_or_without_terms() {
        use super::super::SearchOrder;

        for (text, order) in [
            ("impôts", SearchOrder::Newest),
            ("impôts", SearchOrder::Oldest),
            (" - ", SearchOrder::Relevance),
            ("", SearchOrder::Relevance),
        ] {
            let mut storage = MockStorage::new();
            storage
                .expect_search_documents()
                .withf(move |_, _, queried, _, _| *queried == order)
                .times(1)
                .returning(|_, _, _, _, _| Ok(Vec::new()));

            // `unused_embedder` panics if a query is embedded.
            searching(storage, unused_embedder())
                .search(SearchQuery {
                    order,
                    ..SearchQuery::new(text, 0, 10)
                })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn search_facets_counts_what_a_hybrid_search_finds_when_the_text_embeds() {
        use super::super::{Category, SearchFilters};

        let filters = SearchFilters {
            categories: vec![Category::Money],
            types: vec![DocumentType::Statement],
            created_from: Some(1),
            created_until: Some(2),
            ..Default::default()
        };
        let [by_terms, both, by_similarity, weak] = ids(4).try_into().unwrap();
        let mut found = vec![by_terms, both, by_similarity];
        found.sort_unstable();
        let lifted = [
            without_own_filter(&filters, FacetField::Category),
            without_own_filter(&filters, FacetField::Subcategory),
            without_own_filter(&filters, FacetField::Type),
            without_own_filter(&filters, FacetField::ContentType),
            SearchFilters {
                created_from: None,
                created_until: None,
                ..filters.clone()
            },
        ];

        let mut storage = MockStorage::new();
        for expected in lifted {
            let expected_filters = expected.clone();
            storage
                .expect_rank_by_terms()
                .withf(move |terms, filters, _| terms == ["impôts"] && *filters == expected_filters)
                .times(1)
                .returning(move |_, _, _| Ok(vec![by_terms, both]));
            storage
                .expect_rank_by_similarity()
                .withf(move |_, _, filters, _| *filters == expected)
                .times(1)
                .returning(move |_, _, _, _| {
                    Ok(vec![(both, 0.9), (by_similarity, 0.9), (weak, 0.1)])
                });
        }
        for field in [
            FacetField::Category,
            FacetField::Subcategory,
            FacetField::Type,
            FacetField::ContentType,
        ] {
            let found = found.clone();
            storage
                .expect_count_documents_among()
                .withf(move |ids, queried| ids == found && *queried == field)
                .times(1)
                .returning(move |_, field| {
                    Ok(if field == FacetField::Category {
                        vec![FacetCount {
                            value: Category::Money.as_str().to_string(),
                            count: 3,
                        }]
                    } else {
                        Vec::new()
                    })
                });
        }
        storage
            .expect_created_at_range_among()
            .withf(move |ids| ids == found)
            .times(1)
            .returning(|_| Ok(Some((10, 20))));

        let facets = searching(storage, query_embedder("impôts", vec![1.0]))
            .search_facets(" impôts ", &filters)
            .await
            .unwrap();

        assert_eq!(
            facets,
            super::super::FakeSearchFacets::new()
                .with_categories(vec![FacetCount {
                    value: Category::Money,
                    count: 3
                }])
                .with_subcategories(Vec::new())
                .with_types(Vec::new())
                .with_content_types(Vec::new())
                .with_created_at_range(Some((10, 20)))
                .build()
        );
    }

    #[tokio::test]
    async fn update_metadata_publishes_an_update_metadata_event_with_the_given_metadata() {
        let metadata = super::super::FakeMetadata::new()
            .with_name("new.pdf")
            .build();
        let id = metadata.id;

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        client.update_metadata(metadata).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, document_id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::UpdateMetadata);
        assert_eq!(*document_id, id);
        assert_eq!(*content, None);
        let metadata = metadata.as_ref().unwrap();
        assert_eq!(metadata.name, "new.pdf");
    }

    #[tokio::test]
    async fn start_sync_invokes_on_document_change_for_every_consumed_event() {
        let event = super::super::FakeChangelogEvent::new().build();

        let changelog = Arc::new(RecordingChangelog {
            to_consume: Mutex::new(vec![event.clone()]),
            ..Default::default()
        });

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let on_document_change: Arc<dyn Fn(ChangelogEvent) + Send + Sync> =
            Arc::new(move |event| received_in_callback.lock().unwrap().push(event));
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog,
            Some(on_document_change),
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        client.start_sync().await.unwrap();

        assert_eq!(*received.lock().unwrap(), vec![event]);
    }

    #[tokio::test]
    async fn start_sync_does_not_require_an_on_document_change_callback() {
        let event = super::super::FakeChangelogEvent::new().build();

        let changelog = Arc::new(RecordingChangelog {
            to_consume: Mutex::new(vec![event]),
            ..Default::default()
        });
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog,
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        client.start_sync().await.unwrap();
    }

    #[tokio::test]
    async fn stop_sync_delegates_to_the_changelog_services_stop_consume_job() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        client.stop_sync();

        assert!(changelog.stopped.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn indexer(storage: MockStorage, embedder: MockEmbedder) -> DocumentIndexer<MockStorage> {
        indexer_with(storage, embedder, empty_cache())
    }

    fn indexer_with(
        storage: MockStorage,
        embedder: MockEmbedder,
        cache: MockEmbeddingCache,
    ) -> DocumentIndexer<MockStorage> {
        DocumentIndexer::new(storage, Arc::new(embedder), Arc::new(cache))
    }

    /// A cache holding nothing, expected to never be written to.
    fn empty_cache() -> MockEmbeddingCache {
        let mut cache = MockEmbeddingCache::new();
        cache.expect_get().returning(|_| None);
        cache
    }

    /// An embedder for `model`, expected to never embed anything.
    fn idle_embedder(model: &'static str) -> MockEmbedder {
        let mut embedder = MockEmbedder::new();
        embedder.expect_model().return_const(model.to_string());
        embedder
    }

    /// An embedder for `model` expecting exactly `passages` once, and
    /// returning `vectors` for them.
    fn embedder_expecting(
        model: &'static str,
        passages: &[&str],
        vectors: Vec<Vec<f32>>,
    ) -> MockEmbedder {
        let passages: Vec<String> = passages.iter().map(|p| p.to_string()).collect();
        let mut embedder = idle_embedder(model);
        embedder
            .expect_embed_passages()
            .withf(move |given| *given == passages)
            .times(1)
            .return_once(move |_| Ok(vectors));
        embedder
    }

    fn vector(seed: f32) -> Vec<f32> {
        vec![seed; 4]
    }

    fn embeddings(
        model: &str,
        chunks: Vec<(std::ops::Range<usize>, Vec<f32>)>,
    ) -> DocumentEmbeddings {
        FakeDocumentEmbeddings::new()
            .with_model(model)
            .with_chunks(
                chunks
                    .into_iter()
                    .map(|(range, vector)| {
                        FakeChunkEmbedding::new()
                            .with_range(range)
                            .with_vector(vector)
                            .build()
                    })
                    .collect(),
            )
            .build()
    }

    fn document_with(name: &str, transcript: &str) -> Document {
        FakeDocument::new()
            .with_metadata(
                FakeMetadata::new()
                    .with_name(name)
                    .with_transcript(transcript)
                    .build(),
            )
            .with_id(Uuid::now_v7())
            .build()
    }

    #[tokio::test]
    async fn indexer_save_document_embeds_each_chunk_after_the_title_and_saves_them_with_it() {
        let document = document_with(
            "Facture mars.pdf",
            "Montant : 42 €\n\n---\n\nÉchéance : avril",
        );
        let embedder = embedder_expecting(
            "e5.onnx",
            &[
                "Facture mars\nMontant : 42 €",
                "Facture mars\nÉchéance : avril",
            ],
            vec![vector(0.1), vector(0.2)],
        );
        let expected = embeddings(
            "e5.onnx",
            vec![
                (0.."Montant : 42 €".len(), vector(0.1)),
                (
                    "Montant : 42 €\n\n---\n\n".len()..document.metadata.transcript.len(),
                    vector(0.2),
                ),
            ],
        );
        let mut storage = MockStorage::new();
        let saved = document.clone();
        storage
            .expect_save_document()
            .withf(move |document, embeddings| *document == saved && *embeddings == expected)
            .times(1)
            .returning(|_, _| Ok(()));

        indexer(storage, embedder)
            .save_document(&document)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn indexer_save_document_saves_a_document_with_no_text_as_indexed_without_embedding() {
        let document = document_with("scan.pdf", " \n\n ");
        let mut storage = MockStorage::new();
        storage
            .expect_save_document()
            .withf(|_, embeddings| embeddings.model == "e5.onnx" && embeddings.chunks.is_empty())
            .times(1)
            .returning(|_, _| Ok(()));

        indexer(storage, idle_embedder("e5.onnx"))
            .save_document(&document)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn indexer_save_document_saves_nothing_when_embedding_fails() {
        let document = document_with("a.pdf", "some text");
        let mut embedder = idle_embedder("e5.onnx");
        embedder
            .expect_embed_passages()
            .returning(|_| Err(ErrorKind::InvalidModel("boom".to_string()).into()));

        let err = indexer(MockStorage::new(), embedder)
            .save_document(&document)
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.to_string().contains(&document.id.to_string()), "{err}");
    }

    #[tokio::test]
    async fn indexer_save_document_rejects_a_vector_per_chunk_missing() {
        let document = document_with("a.pdf", "one\n\n---\n\ntwo");
        let mut embedder = idle_embedder("e5.onnx");
        embedder
            .expect_embed_passages()
            .returning(|_| Ok(vec![vector(0.1)]));

        let err = indexer(MockStorage::new(), embedder)
            .save_document(&document)
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.has_context());
    }

    #[tokio::test]
    async fn indexer_save_document_fails_with_context_when_storage_fails() {
        let document = document_with("a.pdf", "");
        let mut storage = MockStorage::new();
        storage
            .expect_save_document()
            .returning(|_, _| Err(ErrorKind::InvalidChangelogEvent("refused".to_string()).into()));

        let err = indexer(storage, idle_embedder("e5.onnx"))
            .save_document(&document)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("failed to cache document"),
            "{err}"
        );
    }

    /// Updates a cached document from `previous` to `updated` metadata,
    /// expecting the storage update to carry `expected` embeddings.
    async fn update_cached(
        previous: Option<Metadata>,
        updated: &Metadata,
        embedder: MockEmbedder,
        expected: Option<DocumentEmbeddings>,
    ) {
        let id = updated.id;
        let mut storage = MockStorage::new();
        storage
            .expect_get_metadata()
            .withf(move |given| *given == id)
            .times(1)
            .return_once(move |_| Ok(previous));
        let saved = updated.clone();
        storage
            .expect_update_metadata()
            .withf(move |given, metadata, embeddings| {
                *given == id && *metadata == saved && *embeddings == expected
            })
            .times(1)
            .returning(|_, _, _| Ok(()));

        indexer(storage, embedder)
            .update_metadata(id, updated)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn indexer_update_metadata_re_embeds_a_changed_transcript() {
        let previous = FakeMetadata::new()
            .with_name("a.pdf")
            .with_transcript("old")
            .build();
        let updated = Metadata {
            transcript: "new".to_string(),
            ..previous.clone()
        };
        let embedder = embedder_expecting("e5.onnx", &["a\nnew"], vec![vector(0.3)]);
        let expected = embeddings("e5.onnx", vec![(0..3, vector(0.3))]);

        update_cached(Some(previous), &updated, embedder, Some(expected)).await;
    }

    #[tokio::test]
    async fn indexer_update_metadata_re_embeds_a_changed_title() {
        let previous = FakeMetadata::new()
            .with_name("a.pdf")
            .with_transcript("text")
            .build();
        let updated = Metadata {
            name: "Facture.pdf".to_string(),
            ..previous.clone()
        };
        let embedder = embedder_expecting("e5.onnx", &["Facture\ntext"], vec![vector(0.3)]);
        let expected = embeddings("e5.onnx", vec![(0..4, vector(0.3))]);

        update_cached(Some(previous), &updated, embedder, Some(expected)).await;
    }

    #[tokio::test]
    async fn indexer_update_metadata_can_re_embed_to_no_chunks_at_all() {
        let previous = FakeMetadata::new()
            .with_name("a.pdf")
            .with_transcript("text")
            .build();
        let updated = Metadata {
            transcript: String::new(),
            ..previous.clone()
        };
        let expected = embeddings("e5.onnx", Vec::new());

        update_cached(
            Some(previous),
            &updated,
            idle_embedder("e5.onnx"),
            Some(expected),
        )
        .await;
    }

    #[tokio::test]
    async fn indexer_update_metadata_keeps_the_embeddings_when_their_text_is_unchanged() {
        let previous = FakeMetadata::new()
            .with_name("a.pdf")
            .with_transcript("text")
            .build();
        let updated = Metadata {
            name: "a.PDF".to_string(),
            category: Some(Category::Housing),
            attributes: BTreeMap::from([("paid".to_string(), Attribute::Boolean(true))]),
            ..previous.clone()
        };

        update_cached(Some(previous), &updated, MockEmbedder::new(), None).await;
    }

    #[tokio::test]
    async fn indexer_update_metadata_embeds_nothing_for_a_document_not_cached() {
        let updated = FakeMetadata::new().build();

        update_cached(None, &updated, MockEmbedder::new(), None).await;
    }

    #[tokio::test]
    async fn indexer_update_metadata_updates_nothing_when_re_embedding_fails() {
        let previous = FakeMetadata::new().with_transcript("old").build();
        let updated = Metadata {
            transcript: "new".to_string(),
            ..previous.clone()
        };
        let mut storage = MockStorage::new();
        storage
            .expect_get_metadata()
            .return_once(move |_| Ok(Some(previous)));
        let mut embedder = idle_embedder("e5.onnx");
        embedder
            .expect_embed_passages()
            .returning(|_| Err(ErrorKind::InvalidModel("boom".to_string()).into()));

        let err = indexer(storage, embedder)
            .update_metadata(updated.id, &updated)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("failed to re-index document"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn indexer_prepare_records_why_the_model_is_unavailable_until_it_loads() {
        let mut embedder = MockEmbedder::new();
        let mut attempts = 0;
        embedder.expect_load().times(2).returning(move || {
            attempts += 1;
            if attempts == 1 {
                Err(ErrorKind::ModelDownloadStatus(503).into())
            } else {
                Ok(())
            }
        });
        let indexer = indexer(MockStorage::new(), embedder);
        assert_eq!(indexer.model_error(), None);

        let err = indexer.prepare().await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::ModelDownloadFailed);
        assert!(err.has_context());
        assert_eq!(indexer.model_error(), Some(ErrorCode::ModelDownloadFailed));

        indexer.prepare().await.unwrap();
        assert_eq!(indexer.model_error(), None);
    }

    #[tokio::test]
    async fn indexing_model_error_is_the_indexers() {
        let mut indexer = MockIndexer::new();
        indexer
            .expect_model_error()
            .return_const(Some(ErrorCode::ModelUnavailable));
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            Arc::new(indexer),
            unused_embedder(),
            unused_cache(),
        );

        assert_eq!(
            client.indexing_model_error(),
            Some(ErrorCode::ModelUnavailable)
        );
    }

    #[test]
    fn passages_are_each_chunk_of_the_transcript_after_the_title() {
        let metadata = FakeMetadata::new()
            .with_name("Facture mars.pdf")
            .with_transcript("Montant : 42 €\n\n---\n\nÉchéance : avril")
            .build();

        let passages = passages(&metadata);

        assert_eq!(
            passages.texts,
            [
                "Facture mars\nMontant : 42 €",
                "Facture mars\nÉchéance : avril"
            ]
        );
        assert_eq!(
            passages.ranges,
            [
                0.."Montant : 42 €".len(),
                "Montant : 42 €\n\n---\n\n".len()..metadata.transcript.len()
            ]
        );
    }

    #[test]
    fn passages_of_a_blank_transcript_are_none() {
        let metadata = FakeMetadata::new().with_transcript(" \n\n ").build();

        let passages = passages(&metadata);

        assert!(passages.texts.is_empty() && passages.ranges.is_empty());
    }

    fn key_for(metadata: &Metadata, model: &str, chunker_version: u32) -> EmbeddingKey {
        embedding_key(
            metadata.id,
            model,
            chunker_version,
            &passages(metadata).texts,
        )
    }

    #[test]
    fn embedding_key_is_the_same_for_an_upload_and_the_document_its_event_carries() {
        let metadata = FakeMetadata::new()
            .with_name("Facture mars.pdf")
            .with_transcript("Montant : 42 €")
            .build();
        let uploaded = FakeCachedEmbeddings::new()
            .for_metadata(&metadata, "e5.onnx")
            .build();
        let consumed = Document::new(metadata.id, b"content".to_vec(), metadata.clone());

        assert_eq!(
            uploaded.key,
            embedding_key(
                consumed.id,
                "e5.onnx",
                CHUNKER_VERSION,
                &passages(&consumed.metadata).texts
            )
        );
        assert_eq!(uploaded.key, key_for(&metadata, "e5.onnx", CHUNKER_VERSION));
    }

    #[test]
    fn embedding_key_changes_with_anything_the_embeddings_depend_on() {
        let metadata = FakeMetadata::new()
            .with_name("Facture mars.pdf")
            .with_transcript("Montant : 42 €")
            .build();
        let key = key_for(&metadata, "e5.onnx", CHUNKER_VERSION);
        let renamed = Metadata {
            name: "Facture avril.pdf".to_string(),
            ..metadata.clone()
        };
        let retranscribed = Metadata {
            transcript: "Montant : 43 €".to_string(),
            ..metadata.clone()
        };
        let another_document = Metadata {
            id: Uuid::now_v7(),
            ..metadata.clone()
        };

        for (what, other) in [
            ("name", key_for(&renamed, "e5.onnx", CHUNKER_VERSION)),
            (
                "transcript",
                key_for(&retranscribed, "e5.onnx", CHUNKER_VERSION),
            ),
            ("model", key_for(&metadata, "e5-v2.onnx", CHUNKER_VERSION)),
            (
                "chunker version",
                key_for(&metadata, "e5.onnx", CHUNKER_VERSION + 1),
            ),
            (
                "document",
                key_for(&another_document, "e5.onnx", CHUNKER_VERSION),
            ),
        ] {
            assert_ne!(other, key, "a new {what} kept the key");
        }
        assert_eq!(key_for(&metadata, "e5.onnx", CHUNKER_VERSION), key);
    }

    #[test]
    fn embedding_key_tells_the_same_text_split_differently_apart() {
        let id = Uuid::now_v7();
        let split = |passages: &[&str]| {
            let passages: Vec<String> = passages.iter().map(|p| p.to_string()).collect();
            embedding_key(id, "e5.onnx", CHUNKER_VERSION, &passages)
        };

        assert_ne!(split(&["ab", "c"]), split(&["a", "bc"]));
        assert_ne!(split(&["abc"]), split(&["abc", ""]));
    }

    #[test]
    fn embedding_key_holds_a_digest_of_the_text_never_the_text() {
        let metadata = FakeMetadata::new()
            .with_name("secret.pdf")
            .with_transcript("Numéro fiscal 1234567890123")
            .build();

        let key = key_for(&metadata, "e5.onnx", CHUNKER_VERSION);

        assert!(!format!("{key:?}").contains("1234567890123"));
        assert_eq!(
            key.text_sha256,
            <[u8; 32]>::from(Sha256::digest(
                [
                    &("secret\nNuméro fiscal 1234567890123".len() as u64).to_le_bytes()[..],
                    "secret\nNuméro fiscal 1234567890123".as_bytes(),
                ]
                .concat()
            ))
        );
    }

    /// A storage only ever asked whether a new upload is a duplicate: any
    /// write panics.
    fn storage_for_a_new_upload() -> MockStorage {
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(1)
            .returning(|_| Ok(None));
        storage
    }

    /// An embedder for `"e5.onnx"` expected to embed exactly once, returning
    /// `vector(i)` for passage `i`, and the passages it was given.
    fn embedder_embedding_once() -> (MockEmbedder, Arc<Mutex<Vec<String>>>) {
        let given = Arc::new(Mutex::new(Vec::new()));
        let recorded = given.clone();
        let mut embedder = idle_embedder("e5.onnx");
        embedder
            .expect_embed_passages()
            .times(1)
            .returning(move |passages| {
                let vectors = (0..passages.len()).map(|i| vector(i as f32)).collect();
                *recorded.lock().unwrap() = passages;
                Ok(vectors)
            });
        (embedder, given)
    }

    /// A cache expecting exactly one `put`, before anything was published
    /// to `changelog`, and what was put.
    #[allow(clippy::type_complexity)]
    fn cache_expecting_a_put_before_publishing(
        changelog: Arc<RecordingChangelog>,
    ) -> (
        MockEmbeddingCache,
        Arc<Mutex<Option<Arc<CachedEmbeddings>>>>,
    ) {
        let put = Arc::new(Mutex::new(None));
        let captured = put.clone();
        let mut cache = MockEmbeddingCache::new();
        cache.expect_put().times(1).returning(move |cached| {
            assert!(
                changelog.sent.lock().unwrap().is_empty(),
                "the embeddings were cached after the event was published"
            );
            *captured.lock().unwrap() = Some(cached);
        });
        (cache, put)
    }

    #[tokio::test]
    async fn upload_embeds_the_document_once_and_caches_its_embeddings_before_publishing_it() {
        let pdf = build_pdf("Montant : 42 €");
        let changelog = Arc::new(RecordingChangelog::default());
        let (embedder, given) = embedder_embedding_once();
        let (cache, put) = cache_expecting_a_put_before_publishing(changelog.clone());
        let client = DocumentsClient::new(
            storage_for_a_new_upload(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            Arc::new(embedder),
            Arc::new(cache),
        );

        let id = client
            .upload(UploadRequest::from_raw("Facture mars.pdf", pdf.clone()))
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        let [(EventType::SaveDocument, sent_id, Some(content), Some(metadata))] = &sent[..] else {
            panic!("unexpected events: {sent:?}");
        };
        assert_eq!((*sent_id, metadata.id), (id, id));
        assert_eq!(*content, pdf);
        let expected = passages(metadata);
        assert!(!expected.texts.is_empty());
        assert_eq!(*given.lock().unwrap(), expected.texts);
        let put = put.lock().unwrap().clone().unwrap();
        assert_eq!(put.key, key_for(metadata, "e5.onnx", CHUNKER_VERSION));
        assert_eq!(
            put.embeddings,
            embeddings(
                "e5.onnx",
                expected
                    .ranges
                    .into_iter()
                    .enumerate()
                    .map(|(i, range)| (range, vector(i as f32)))
                    .collect()
            )
        );
    }

    #[tokio::test]
    async fn upload_without_the_model_publishes_the_document_and_caches_nothing() {
        let changelog = Arc::new(RecordingChangelog::default());
        // Strict: any `put` panics.
        let client = DocumentsClient::new(
            storage_for_a_new_upload(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            embedder_without_model(),
            unused_cache(),
        );

        let id = client
            .upload(UploadRequest::from_raw(
                "a.pdf",
                build_pdf("Montant : 42 €"),
            ))
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, id);
    }

    #[tokio::test]
    async fn upload_of_a_document_with_no_text_neither_embeds_nor_caches_anything() {
        let changelog = Arc::new(RecordingChangelog::default());
        // Both strict: embedding or caching panics.
        let client = DocumentsClient::new(
            storage_for_a_new_upload(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            unused_embedder(),
            unused_cache(),
        );

        client
            .upload(UploadRequest::from_raw("scan.pdf", build_pdf(" ")))
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert!(sent[0].3.as_ref().unwrap().transcript.trim().is_empty());
    }

    #[tokio::test]
    async fn upload_drops_the_embeddings_it_cached_when_publishing_fails() {
        let changelog = Arc::new(RecordingChangelog {
            fail_send: true,
            ..Default::default()
        });
        let (embedder, _) = embedder_embedding_once();
        let (mut cache, put) = cache_expecting_a_put_before_publishing(changelog.clone());
        let removed = Arc::new(Mutex::new(None));
        let captured = removed.clone();
        cache
            .expect_remove()
            .times(1)
            .returning(move |id| *captured.lock().unwrap() = Some(id));
        let client = DocumentsClient::new(
            storage_for_a_new_upload(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            Arc::new(embedder),
            Arc::new(cache),
        );

        let err = client
            .upload(UploadRequest::from_raw(
                "a.pdf",
                build_pdf("Montant : 42 €"),
            ))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("failed to publish"), "{err}");
        let put = put.lock().unwrap().clone().unwrap();
        assert_eq!(*removed.lock().unwrap(), Some(put.key.document_id));
        assert_eq!(put.key.document_id, changelog.sent.lock().unwrap()[0].1);
    }

    #[tokio::test]
    async fn upload_drops_the_embeddings_it_cached_when_given_up_on_while_publishing() {
        let reached = Arc::new(tokio::sync::Notify::new());
        let changelog = Arc::new(RecordingChangelog {
            hang_send: Some(reached.clone()),
            ..Default::default()
        });
        let (embedder, _) = embedder_embedding_once();
        let (mut cache, put) = cache_expecting_a_put_before_publishing(changelog.clone());
        let removed = Arc::new(Mutex::new(None));
        let captured = removed.clone();
        cache
            .expect_remove()
            .times(1)
            .returning(move |id| *captured.lock().unwrap() = Some(id));
        let client = DocumentsClient::new(
            storage_for_a_new_upload(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            Arc::new(embedder),
            Arc::new(cache),
        );

        let upload = client.upload(UploadRequest::from_raw(
            "a.pdf",
            build_pdf("Montant : 42 €"),
        ));
        // Dropping the upload once it's publishing, as a stopped run does.
        tokio::select! {
            () = reached.notified() => {}
            result = upload => panic!("the upload finished: {result:?}"),
        }

        let put = put.lock().unwrap().clone().unwrap();
        assert_eq!(*removed.lock().unwrap(), Some(put.key.document_id));
    }

    #[tokio::test(start_paused = true)]
    async fn upload_skips_embedding_for_a_while_after_embedding_failed() {
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(4)
            .returning(|_| Ok(None));
        let mut order = mockall::Sequence::new();
        let mut embedder = idle_embedder("e5.onnx");
        embedder
            .expect_embed_passages()
            .times(1)
            .in_sequence(&mut order)
            .returning(|_| Err(ErrorKind::ModelDownloadStatus(503).into()));
        embedder
            .expect_embed_passages()
            .times(1)
            .in_sequence(&mut order)
            .returning(|passages| Ok(passages.iter().map(|_| vector(0.5)).collect()));
        let put = Arc::new(Mutex::new(None));
        let captured = put.clone();
        let mut cache = MockEmbeddingCache::new();
        cache
            .expect_put()
            .times(1)
            .returning(move |cached| *captured.lock().unwrap() = Some(cached));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            Arc::new(embedder),
            Arc::new(cache),
        );
        let upload = |text| client.upload(UploadRequest::from_raw("a.pdf", build_pdf(text)));

        // Fails to embed, then skips embedding until the pause is over.
        upload("first").await.unwrap();
        upload("second").await.unwrap();
        tokio::time::advance(UPLOAD_EMBED_RETRY_AFTER - std::time::Duration::from_secs(1)).await;
        upload("third").await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        let id = upload("fourth").await.unwrap();

        assert_eq!(changelog.sent.lock().unwrap().len(), 4);
        assert_eq!(put.lock().unwrap().as_ref().unwrap().key.document_id, id);
    }

    /// Uploads a document with a mocked model, then has an indexer over a
    /// cache holding what the upload put there consume the event the upload
    /// published, as the changelog consumer would.
    #[tokio::test]
    async fn an_uploads_embeddings_are_stored_as_is_when_its_event_is_consumed() {
        let pdf = build_pdf("Montant : 42 €\n\n---\n\nÉchéance : avril");
        let changelog = Arc::new(RecordingChangelog::default());
        let (embedder, _) = embedder_embedding_once();
        let (cache, put) = cache_expecting_a_put_before_publishing(changelog.clone());
        let client = DocumentsClient::new(
            storage_for_a_new_upload(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_indexer(),
            Arc::new(embedder),
            Arc::new(cache),
        );
        let id = client
            .upload(UploadRequest::from_raw("Facture mars.pdf", pdf))
            .await
            .unwrap();
        let put = put.lock().unwrap().clone().unwrap();
        let (_, subject_id, content, metadata) = changelog.sent.lock().unwrap()[0].clone();
        let document = Document::new(subject_id, content.unwrap(), metadata.unwrap());

        let mut cache = MockEmbeddingCache::new();
        let cached = put.clone();
        cache
            .expect_get()
            .withf(move |given| *given == id)
            .return_once(move |_| Some(cached));
        cache
            .expect_remove()
            .withf(move |given| *given == id)
            .times(1)
            .return_const(());
        let mut storage = MockStorage::new();
        let expected = put.embeddings.clone();
        storage
            .expect_save_document()
            .withf(move |_, embeddings| *embeddings == expected)
            .times(1)
            .returning(|_, _| Ok(()));

        // `idle_embedder`: embedding again panics.
        indexer_with(storage, idle_embedder("e5.onnx"), cache)
            .save_document(&document)
            .await
            .unwrap();
    }

    /// A cache holding `cached` for `id`, expecting it to be removed only
    /// after `storage` saved the document (see `in_sequence`).
    fn cache_holding(
        id: Uuid,
        cached: CachedEmbeddings,
        sequence: &mut mockall::Sequence,
    ) -> MockEmbeddingCache {
        let mut cache = MockEmbeddingCache::new();
        let cached = Arc::new(cached);
        cache
            .expect_get()
            .withf(move |given| *given == id)
            .times(1)
            .in_sequence(sequence)
            .return_once(move |_| Some(cached));
        cache
    }

    #[tokio::test]
    async fn indexer_save_document_stores_the_cached_embeddings_without_embedding_then_drops_them()
    {
        let document = document_with("Facture mars.pdf", "Montant : 42 €");
        let cached = FakeCachedEmbeddings::new()
            .for_metadata(&document.metadata, "e5.onnx")
            .build();
        let expected = cached.embeddings.clone();
        let mut sequence = mockall::Sequence::new();
        let mut cache = cache_holding(document.id, cached, &mut sequence);
        let mut storage = MockStorage::new();
        let saved = document.clone();
        storage
            .expect_save_document()
            .withf(move |document, embeddings| *document == saved && *embeddings == expected)
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(()));
        let id = document.id;
        cache
            .expect_remove()
            .withf(move |given| *given == id)
            .times(1)
            .in_sequence(&mut sequence)
            .return_const(());

        indexer_with(storage, idle_embedder("e5.onnx"), cache)
            .save_document(&document)
            .await
            .unwrap();
    }

    /// Saves `document` with `cached` in the cache, which must not match:
    /// the document is embedded inline as if nothing were cached, then the
    /// stale entry dropped.
    async fn save_with_stale_entry(document: Document, cached: CachedEmbeddings) {
        let embedder = embedder_expecting(
            "e5.onnx",
            &["Facture mars\nMontant : 42 €"],
            vec![vector(0.7)],
        );
        let expected = embeddings(
            "e5.onnx",
            vec![(0..document.metadata.transcript.len(), vector(0.7))],
        );
        let mut sequence = mockall::Sequence::new();
        let mut cache = cache_holding(document.id, cached, &mut sequence);
        let mut storage = MockStorage::new();
        storage
            .expect_save_document()
            .withf(move |_, embeddings| *embeddings == expected)
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(()));
        let id = document.id;
        cache
            .expect_remove()
            .withf(move |given| *given == id)
            .times(1)
            .in_sequence(&mut sequence)
            .return_const(());

        indexer_with(storage, embedder, cache)
            .save_document(&document)
            .await
            .unwrap();
    }

    fn facture() -> Document {
        document_with("Facture mars.pdf", "Montant : 42 €")
    }

    #[tokio::test]
    async fn indexer_save_document_embeds_again_for_another_model() {
        let document = facture();
        let cached = FakeCachedEmbeddings::new()
            .for_metadata(&document.metadata, "e5-old.onnx")
            .build();

        save_with_stale_entry(document, cached).await;
    }

    #[tokio::test]
    async fn indexer_save_document_embeds_again_for_another_chunker_version() {
        let document = facture();
        let key = embedding_key(
            document.id,
            "e5.onnx",
            CHUNKER_VERSION + 1,
            &passages(&document.metadata).texts,
        );
        let cached = FakeCachedEmbeddings::new().with_key(key).build();

        save_with_stale_entry(document, cached).await;
    }

    #[tokio::test]
    async fn indexer_save_document_embeds_again_for_another_transcript() {
        let document = facture();
        let uploaded = Metadata {
            transcript: "Montant : 41 €".to_string(),
            ..document.metadata.clone()
        };
        let cached = FakeCachedEmbeddings::new()
            .for_metadata(&uploaded, "e5.onnx")
            .build();

        save_with_stale_entry(document, cached).await;
    }

    #[tokio::test]
    async fn indexer_save_document_embeds_again_for_another_name() {
        let document = facture();
        let uploaded = Metadata {
            name: "Facture avril.pdf".to_string(),
            ..document.metadata.clone()
        };
        let cached = FakeCachedEmbeddings::new()
            .for_metadata(&uploaded, "e5.onnx")
            .build();

        save_with_stale_entry(document, cached).await;
    }

    #[tokio::test]
    async fn indexer_save_document_embeds_again_for_another_documents_entry() {
        let document = facture();
        let cached = FakeCachedEmbeddings::new()
            .for_metadata(&document.metadata, "e5.onnx")
            .with_document_id(Uuid::now_v7())
            .build();

        save_with_stale_entry(document, cached).await;
    }

    #[tokio::test]
    async fn indexer_save_document_keeps_the_cached_embeddings_until_a_retry_saves_them() {
        let document = facture();
        let cached = Arc::new(
            FakeCachedEmbeddings::new()
                .for_metadata(&document.metadata, "e5.onnx")
                .build(),
        );
        let expected = cached.embeddings.clone();
        let id = document.id;
        let mut sequence = mockall::Sequence::new();
        let mut cache = MockEmbeddingCache::new();
        cache
            .expect_get()
            .withf(move |given| *given == id)
            .times(2)
            .returning(move |_| Some(cached.clone()));
        let mut storage = MockStorage::new();
        storage
            .expect_save_document()
            .withf(move |_, embeddings| *embeddings == expected)
            .times(1)
            .in_sequence(&mut sequence)
            .returning(
                |_, _| Err(ErrorKind::InvalidChangelogEvent("disk full".to_string()).into()),
            );
        storage
            .expect_save_document()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(()));
        // Only once the retry saved it.
        cache
            .expect_remove()
            .withf(move |given| *given == id)
            .times(1)
            .in_sequence(&mut sequence)
            .return_const(());
        let indexer = indexer_with(storage, idle_embedder("e5.onnx"), cache);

        let err = indexer.save_document(&document).await.unwrap_err();
        assert!(
            err.to_string().contains("failed to cache document"),
            "{err}"
        );

        indexer.save_document(&document).await.unwrap();
    }

    #[test]
    fn indexer_skip_document_drops_the_documents_cached_embeddings_and_stores_nothing() {
        let id = Uuid::now_v7();
        let mut cache = MockEmbeddingCache::new();
        cache
            .expect_remove()
            .withf(move |removed| *removed == id)
            .times(1)
            .return_const(());

        // Strict storage and embedder: any call panics.
        indexer_with(MockStorage::new(), MockEmbedder::new(), cache).skip_document(id);
    }

    #[tokio::test]
    async fn indexer_update_metadata_never_reads_the_cache() {
        let previous = FakeMetadata::new()
            .with_name("a.pdf")
            .with_transcript("old")
            .build();
        let updated = Metadata {
            transcript: "new".to_string(),
            ..previous.clone()
        };
        let id = updated.id;
        let mut storage = MockStorage::new();
        storage
            .expect_get_metadata()
            .return_once(move |_| Ok(Some(previous)));
        storage.expect_update_metadata().returning(|_, _, _| Ok(()));

        // Strict cache: any call panics.
        indexer_with(
            storage,
            embedder_expecting("e5.onnx", &["a\nnew"], vec![vector(0.3)]),
            MockEmbeddingCache::new(),
        )
        .update_metadata(id, &updated)
        .await
        .unwrap();
    }

    #[test]
    fn title_is_the_name_without_its_extension() {
        assert_eq!(title("Facture mars.pdf"), "Facture mars");
        assert_eq!(title("relevé.2024.PDF"), "relevé.2024");
        assert_eq!(title("no extension"), "no extension");
        assert_eq!(title(".pdf"), ".pdf");
        assert_eq!(title(""), "");
    }
}
