pub(crate) mod attributes;
mod chunk;
mod classification;
mod dev;
mod embedder;
mod embedding_cache;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "not wired into any document flow yet")
)]
mod gliner;
mod models;
mod service;
mod storage;
mod storage_sqlite;
pub(crate) mod transcript;

#[cfg(any(test, feature = "testing"))]
pub use crate::domains::changelog::FakeChangelogEvent;
pub use crate::domains::changelog::{ChangelogEvent, EventType};
pub use attributes::{
    Attribute, AttributeDefinition, AttributeType, Service as AttributesService,
    SetAttributeRequest,
};
#[cfg(test)]
pub(crate) use embedder::MockEmbedder;
#[cfg(test)]
pub(crate) use models::FakeCachedEmbeddings;
#[cfg(test)]
pub(crate) use models::FakeSearchFacets;
pub(crate) use models::{CachedEmbeddings, ChunkEmbedding, DocumentEmbeddings, EmbeddingKey};
pub use models::{
    Document, DocumentType, FacetCount, FolderFilter, Metadata, SearchFacets, SearchFilters,
    SearchOrder, SearchQuery, SimilarDocument, UploadRequest, UploadSource,
};
#[cfg(any(test, feature = "testing"))]
pub use models::{FakeDocument, FakeMetadata};
#[cfg(test)]
pub(crate) use service::DocumentIndexer;
#[cfg(test)]
pub(crate) use storage::MockStorage;
pub(crate) use storage::Storage;
pub(crate) use storage_sqlite::SqliteStorage;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::domains::changelog;
use crate::tools::models::ModelStore;
use crate::{ErrorCode, Result};

/// The `min_confidence` to pass [`Service::similar`] without a reason to
/// pick another: about as close as a search needs a document to be to match
/// by meaning.
pub const DEFAULT_SIMILAR_MIN_CONFIDENCE: f32 = 0.25;

/// Uploads documents by publishing them as encrypted changelog events, and
/// reads back documents materialized locally from consumed events (see
/// [`Self::start_sync`]) — there is no server-side document store to fetch
/// from.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Reads the content described by `request.source` (either a file on
    /// disk or already-in-memory bytes), encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its
    /// generated id. Its original name is the source's file name, its name
    /// `request.name` or, without one, that file name without its
    /// extension; it has no attribute, and its type and folder are guessed
    /// from its text, on device: the folder is the one whose description it
    /// reads like (see the `folders` domain). Each is `None` when the
    /// classifier isn't sure (the document then lands at the root, À
    /// classer), and both when the embedding model isn't available.
    ///
    /// The file type is determined from the source's name's extension;
    /// only `.pdf` is currently accepted, anything else is rejected with
    /// [`crate::ErrorCode::UnsupportedDocumentExtension`].
    async fn upload(&self, request: UploadRequest) -> Result<Uuid>;

    /// Fetches a document previously cached in local storage by
    /// [`Self::start_sync`], content included, or `None` if it doesn't
    /// exist. The only way to read a document's content: [`Self::list`]
    /// and [`Self::search`] return metadata alone, so fetch it with this
    /// once the document is actually shown.
    async fn get(&self, id: Uuid) -> Result<Option<Document>>;

    /// Lists the metadata of documents previously cached in local storage,
    /// oldest first, one page at a time. Never loads their content (see
    /// [`Self::get`]).
    ///
    /// Paginate by repeatedly incrementing `offset` by the returned page's
    /// length until it comes back shorter than `limit`.
    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Metadata>>;

    /// Searches documents previously cached in local storage for
    /// `query.text` (see [`SearchQuery::text`] for which fields it's
    /// matched against and how), keeping only those matching every one of
    /// `query.filters`, one page at a time in `query.order`, returning their
    /// metadata. Like [`Self::list`], never talks to the server nor loads
    /// their content. [`SearchOrder::Relevance`] also matches by meaning,
    /// but never downloads the model for it, nor fails without it.
    ///
    /// Fails with [`crate::ErrorCode::InvalidSearchQuery`] for a negative
    /// `offset`, a non-positive `limit`, or a `created_from` later than
    /// `created_until`.
    async fn search(&self, query: SearchQuery) -> Result<Vec<Metadata>>;

    /// Lists, for every field [`SearchFilters`] can filter on, the values
    /// still worth offering for a search of `text` narrowed by `filters`
    /// (see [`SearchFacets`]): each field's values come from the documents
    /// [`Self::search`] would match with every filter *but that field's
    /// own*, so a UI can propose only the choices — and sub-choices, once a
    /// parent filter is picked — that actually lead to documents. Counts
    /// what a [`SearchOrder::Relevance`] search finds, documents close in
    /// meaning included.
    ///
    /// Fails with [`crate::ErrorCode::InvalidSearchQuery`] for a
    /// `created_from` later than `created_until`.
    async fn search_facets(&self, text: &str, filters: &SearchFilters) -> Result<SearchFacets>;

    /// Lists the other documents cached locally whose text is close in
    /// meaning to that of the document `id`, most similar first (ties by
    /// id), at most `limit`, keeping only those whose
    /// [`SimilarDocument::confidence`] is at least `min_confidence` (see
    /// [`DEFAULT_SIMILAR_MIN_CONFIDENCE`]). Compares the embeddings indexing
    /// stored, so never needs the model, nor talks to the server, nor loads
    /// their content. A document with no text (or not indexed by the
    /// current model yet) has no similar document.
    ///
    /// Fails with [`crate::ErrorCode::DocumentNotFound`] for a document not
    /// synced here, and [`crate::ErrorCode::InvalidSearchQuery`] for a
    /// `min_confidence` outside `0.0..=1.0` or a non-positive `limit`.
    async fn similar(
        &self,
        id: Uuid,
        min_confidence: f32,
        limit: i64,
    ) -> Result<Vec<SimilarDocument>>;

    /// Encrypts `metadata` as given and publishes it as an "update metadata"
    /// changelog event, replacing whatever metadata was previously
    /// associated with `metadata.id`. The caller is responsible for
    /// constructing the full replacement `Metadata` (its fields are public
    /// for this purpose), `attributes` included: an attribute left out of
    /// it is removed. Rejects invalid attributes like [`Self::upload`].
    async fn update_metadata(&self, metadata: Metadata) -> Result<()>;

    /// Moves the document `id` into the folder `folder_id` (`None` for the
    /// root, À classer), publishing its metadata as [`Self::update_metadata`]
    /// does. Fails with [`crate::ErrorCode::DocumentNotFound`] for a
    /// document not synced here, and [`crate::ErrorCode::FolderNotFound`]
    /// for a folder not synced here. Moving it where it already is
    /// publishes nothing.
    async fn move_document(&self, id: Uuid, folder_id: Option<Uuid>) -> Result<()>;

    /// Publishes a "deleted" changelog event for the document `id`. Like
    /// [`Self::upload`]'s, its effect is local only once the sync job
    /// consumes it back (on this device as on every other): the document
    /// then disappears from local storage, embeddings and search index
    /// included, and `on_document_change` gets the event. Doesn't check that
    /// `id` is cached, so a document uploaded but not synced yet can be
    /// deleted too; deleting an unknown or already deleted document is a
    /// no-op once consumed. A deleted document's content can be uploaded
    /// again afterwards.
    async fn delete(&self, id: Uuid) -> Result<()>;

    /// Starts a background job that streams and decrypts every changelog
    /// event since the last consumed id, oldest first (replaying persisted
    /// history, then continuing with the live tail). Resumes from the
    /// cursor persisted locally by a previous call, or from the beginning
    /// of the changelog if it's never been consumed before. For each
    /// "created"/"update metadata"/"deleted" event, updates local storage, indexing
    /// the document's text along the way (about a second per document on a
    /// desktop CPU), before invoking the `on_document_change` callback
    /// configured on the client (see [`crate::ClientConfig`]), so it's
    /// already visible via [`Self::get`]/[`Self::list`] by the time that
    /// callback runs. Until the indexing model can be fetched, syncing
    /// waits (see [`Self::indexing_model_error`]).
    /// [`crate::domains::users::Service::create`]/`login` call this
    /// automatically once a session is open; most callers never need to
    /// call it directly.
    ///
    /// Delegates entirely to the [`crate::domains::changelog`] service's
    /// `start_consume_job` — this is the only way to reach it from outside
    /// the `sdk` crate, since that service's own API is `pub(crate)`.
    ///
    /// Returns as soon as the background job is spawned, not when it stops.
    /// The job itself runs until the server closes the stream or an error
    /// occurs, retrying immediately either way, until
    /// [`Self::stop_sync`] signals it to return.
    async fn start_sync(&self) -> Result<()>;

    /// Signals the job started by a prior [`Self::start_sync`] call to
    /// return. Idempotent, and safe to call before [`Self::start_sync`] has
    /// started its job (in which case that job returns immediately) or
    /// after it has already returned (a no-op).
    /// [`crate::domains::users::Service::logout`] calls this automatically.
    /// Delegates to the [`crate::domains::changelog`] service's
    /// `stop_consume_job` — see [`Self::start_sync`]'s own doc comment for
    /// why that's the only way to reach it.
    fn stop_sync(&self);

    /// Why documents can't be indexed right now, if they can't: the model
    /// indexing needs couldn't be fetched or loaded (e.g.
    /// [`ErrorCode::ModelDownloadFailed`] while offline). Syncing is paused
    /// meanwhile, since a document is never cached without being indexed,
    /// and resumes by itself once the model is available, which clears
    /// this. A UI can poll it to tell the user why new documents don't
    /// show up.
    fn indexing_model_error(&self) -> Option<ErrorCode>;
}

/// Caches the documents consumed changelog events carry, each with the
/// embeddings of its text (title included), written together: a document
/// is never cached without being indexed.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Indexer: Send + Sync {
    /// Fetches and loads the indexing model, unless that's already done,
    /// failing if it can't (see [`Service::indexing_model_error`]).
    async fn prepare(&self) -> Result<()>;

    /// Caches `document`, unencrypted, with the embeddings of its text.
    async fn save_document(&self, document: &Document) -> Result<()>;

    /// Called instead of [`Self::save_document`] for a document that won't
    /// be cached (a duplicate): drops anything held for it.
    fn skip_document(&self, id: Uuid);

    /// Replaces the metadata of the cached document `id`, and its
    /// embeddings too if that changes the text they embed. Does nothing if
    /// no document `id` is cached.
    async fn update_metadata(&self, id: Uuid, metadata: &Metadata) -> Result<()>;

    /// Removes the cached document `id` with its embeddings. Does nothing if
    /// no document `id` is cached.
    async fn delete_document(&self, id: Uuid) -> Result<()>;

    /// Why the last [`Self::prepare`] failed, if it did.
    fn model_error(&self) -> Option<ErrorCode>;
}

/// Holds the embeddings [`Service::upload`] computes until the changelog
/// consumer caches the document, so it reuses them instead of embedding the
/// same text twice. Plaintext-derived, so in memory only, and cleared on
/// logout.
#[cfg_attr(test, automock)]
pub(crate) trait EmbeddingCache: Send + Sync {
    fn get(&self, document_id: Uuid) -> Option<Arc<CachedEmbeddings>>;

    /// Stores `embeddings` under their `key.document_id`.
    fn put(&self, embeddings: Arc<CachedEmbeddings>);

    fn remove(&self, document_id: Uuid);

    fn clear(&self);
}

/// How much memory the [`EmbeddingCache`] may hold: a scraper can upload
/// hundreds of documents before sync consumes them.
const EMBEDDING_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Initializes the [`EmbeddingCache`] shared by the documents [`Service`],
/// its [`Indexer`] and logout.
pub(crate) fn init_embedding_cache() -> Arc<dyn EmbeddingCache> {
    Arc::new(embedding_cache::MemoryEmbeddingCache::new(
        EMBEDDING_CACHE_BYTES,
    ))
}

/// Initializes the on-device embedding model `models` holds, shared by the
/// [`Indexer`] (which fetches it on the first [`Indexer::prepare`], unless
/// the [`ModelPreloader`] already did) and the documents [`Service`]'s
/// searches, so it's loaded once.
pub(crate) fn init_embedder(models: Arc<ModelStore>) -> Arc<dyn embedder::Embedder> {
    Arc::new(embedder::OnnxEmbedder::new(models))
}

/// Fetches and loads the on-device models in the background, ahead of their
/// first use, so it doesn't wait minutes for them.
#[cfg_attr(test, automock)]
pub(crate) trait ModelPreloader: Send + Sync {
    /// Starts downloading every model, each on its own task so they
    /// download in parallel, without loading them. Returns at once, and
    /// does nothing while a previous download is still under way.
    fn start_download(&self);

    /// Starts loading every model in memory (downloading it first if
    /// needed), each on its own task. Returns at once, and does nothing
    /// while a previous load is still under way. Loaded models stay in
    /// memory for the `Client`'s lifetime.
    fn start_loading(&self);
}

/// Initializes the [`ModelPreloader`] over `embedder` (see
/// [`init_embedder`]) and the GLiNER model `models` holds (not used by any
/// flow yet: only fetched and loaded ahead of its first use). Dropping it
/// stops its downloads and loads.
pub(crate) fn init_model_preloader(
    models: Arc<ModelStore>,
    embedder: Arc<dyn embedder::Embedder>,
) -> Arc<dyn ModelPreloader> {
    Arc::new(service::ModelPreloaderClient::new(
        embedder,
        Arc::new(gliner::OnnxGliner::new(models)),
    ))
}

/// Initializes the [`Indexer`] the changelog consumer caches documents
/// through, into the database behind `pool`, embedding them with
/// `embedder` (see [`init_embedder`]), unless `cache` holds the embeddings
/// their upload computed.
pub(crate) fn init_indexer(
    pool: SqlitePool,
    embedder: Arc<dyn embedder::Embedder>,
    cache: Arc<dyn EmbeddingCache>,
) -> Arc<dyn Indexer> {
    Arc::new(service::DocumentIndexer::new(
        storage_sqlite::SqliteStorage::new(pool),
        embedder,
        cache,
    ))
}

/// Initializes the documents service: publishes new documents through the
/// shared `changelog` service (which also materializes consumed events
/// into the local `documents` table through `indexer` — see
/// `changelog::init`), wires up local SQLite-backed reads (via `pool`) of
/// the documents it cached, and calls `on_document_change` (see
/// [`crate::ClientConfig`]) for every event [`Service::start_sync`]
/// consumes. Uploads and searches embed their text with `embedder` (see
/// [`init_embedder`]), uploads leaving theirs in `cache` for `indexer`, and
/// are classified with it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn init(
    pool: SqlitePool,
    changelog: Arc<dyn changelog::Service>,
    attributes: Arc<dyn AttributesService>,
    indexer: Arc<dyn Indexer>,
    embedder: Arc<dyn embedder::Embedder>,
    cache: Arc<dyn EmbeddingCache>,
    folders: Arc<dyn crate::domains::folders::Service>,
    on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
) -> Arc<dyn Service> {
    let storage = storage_sqlite::SqliteStorage::new(pool);

    Arc::new(service::DocumentsClient::new(
        storage,
        changelog,
        on_document_change,
        attributes,
        indexer,
        embedder.clone(),
        cache,
        classification::init(embedder),
        folders,
    ))
}

/// Initializes a dev [`Service`] for standalone use without a fyde server
/// connection (see [`crate::init_dev_scrapers`]): [`Service::upload`] just
/// copies the file to `out_dir/scraper_name` as plain bytes instead of
/// encrypting and publishing it through the changelog.
pub(crate) fn init_dev(
    out_dir: std::path::PathBuf,
    scraper_name: impl Into<String>,
) -> Arc<dyn Service> {
    Arc::new(dev::DevDocumentsClient::new(out_dir, scraper_name))
}
