//! The user's documents. No gRPC transport: every write is a `changelog`
//! event, and reads only ever hit the local cache.
//!
//! **Writes.** `upload` needs a master key and rejects content over
//! `changelog::MAX_EVENT_BYTES` (`DocumentTooLarge`) or with a known checksum
//! (`DuplicateDocument`), both before parsing. It then transcribes
//! (`transcript`), embeds, classifies (`classification`), extracts attributes
//! (`extraction`) and publishes an encrypted `DocumentUploaded`. None of these
//! steps but the transcript's parsing fails the upload, and none ever runs on
//! consumption. The document reaches the local cache only when the sync job
//! consumes it back. `delete` only publishes a `Deleted` (id only, no local
//! check, so an unsynced upload can be deleted); consuming it removes the
//! cached document through [`Indexer::delete_document`], a no-op if it isn't
//! cached. `move_document`/`update_metadata` refuse a folder not synced here
//! (`FolderNotFound`).
//!
//! **Placement.** A document's place is `Metadata::folder_id` (`None` = the
//! root; a folder deleted meanwhile counts as the root too). `upload` takes an
//! optional `folder_id` (then the document is only typed, and a folder not
//! synced here is `FolderNotFound` before embedding) and an optional
//! `within_folder_id` (a script installation's folder: compared only with that
//! subtree, it lands in the winner, else in that folder itself, never outside
//! it, even without the model; one not synced here falls back to filing across
//! the whole vault, never failing). `folder_id` wins when both are set.
//!
//! **Reads.** `get`/`list`/`search`/`search_facets` are local-only.
//! `list`/`search` return `Metadata` only (their queries never select
//! `content`): `get` is the only call loading a document's content.
//! `search_facets` counts each field with its own filter lifted, so UIs only
//! offer reachable choices; folder facets count each folder with everything
//! below it. `search` with `Relevance` and non-blank text fuses a bm25 and an
//! embedding-similarity ranking with RRF; if the query can't be embedded (the
//! model isn't downloaded: `ModelStore::find` never downloads) it falls back
//! to bm25 alone, never failing. `similar` ranks by a document's stored
//! vectors (never embeds), its confidence rescaled from
//! `UNRELATED_SIMILARITY` so callers never handle e5 numbers.
//!
//! **Indexing.** A document is never cached without its embeddings: the
//! changelog consumer applies every event writing a document through
//! [`Indexer`], which embeds then writes document and embeddings in one
//! transaction. `upload` already embedded the same passages before publishing
//! and parked the vectors in the in-memory [`EmbeddingCache`] (never in the
//! event, never on disk), so the indexer reuses them when the `embedding_key`
//! matches, and removes the entry only once its transaction committed. An
//! upload that doesn't publish drops its entry (`CachedUntilPublished`).
//!
//! **Thumbnails.** `thumbnail(id)` reads the first page back as a PNG stored in
//! `documents.thumbnail` (NULL = not drawn yet, empty = undrawable), and never
//! draws one. Only the background job (`start_thumbnail_job`) draws them, the
//! missing ones newest first, sleeping until the indexer wakes it; with
//! `BackgroundJobs::thumbnails` off nothing draws them, so there are none.

pub(crate) mod attributes;
mod chunk;
mod classification;
mod dev;
mod embedder;
mod embedding_cache;
mod extraction;
mod gliner;
mod models;
mod service;
mod storage;
mod storage_sqlite;
mod thumbnail;
pub(crate) mod transcript;

#[cfg(any(test, feature = "testing"))]
pub use crate::domains::changelog::FakeChangelogEvent;
pub use crate::domains::changelog::{ChangelogEvent, EventType, SyncProgress};
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
    SearchOrder, SearchQuery, SimilarDocument, Thumbnail, UploadRequest, UploadSource,
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

/// The side, in pixels, of the square every [`Thumbnail`] fits within: an A4
/// page is drawn 362 x 512, about 44 dpi. Scale it down to show it smaller.
pub const THUMBNAIL_SIDE: u32 = 512;

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
    /// extension; its type, folder and attributes are guessed on device.
    /// The type from its text. The folder from the documents
    /// already filed, by content only (never by folder names): the one whose
    /// documents directly in it are the closest to this one, compared as
    /// [`Self::similar`] does, from the embeddings stored for search (so
    /// without loading any document). A folder scores the mean similarity
    /// of its three documents closest to the new one (of all of them if it
    /// holds fewer), and wins only if no other folder comes within 0.01 of
    /// it and it reaches what [`Self::similar`] keeps at
    /// [`DEFAULT_SIMILAR_MIN_CONFIDENCE`]. So a folder holding no document,
    /// and the documents at the root, never draw one. Each is `None` when
    /// the classifier isn't sure (the document then lands at the root, À
    /// classer, as every upload does into a vault with no filed document
    /// yet), and both when the embedding model isn't available. A
    /// `request.folder_id` files it there instead, never guessing one (its
    /// type still is): [`crate::ErrorCode::FolderNotFound`] for a folder not
    /// synced here. A `request.within_folder_id` (a source's folder) only
    /// guesses among that folder and every folder below it, at any depth,
    /// filing it in that folder itself when none wins (or without the
    /// model): never outside it, nor at the root. One not synced here
    /// (deleted since, say) doesn't fail the upload: it's guessed across
    /// the whole vault, as without it.
    ///
    /// The file type is determined from the source's name's extension;
    /// only `.pdf` is currently accepted, anything else is rejected with
    /// [`crate::ErrorCode::UnsupportedDocumentExtension`].
    ///
    /// Its attributes are read from its text by GLiNER (downloaded and
    /// loaded first if it isn't yet: a scraper run loads it while setting
    /// up), depending on its type: an invoice's issuer, reference,
    /// date and total, a payslip's employer, period, gross and net pay, and
    /// so on (an untyped document's issuer, date and amount). At most one
    /// value each, and only those valid against the account's attribute
    /// definitions. Never fails the upload: without the model, or if it
    /// fails, the document is published without attributes, and uploads
    /// don't try again for five minutes.
    async fn upload(&self, request: UploadRequest) -> Result<Uuid>;

    /// Fetches a document previously cached in local storage by
    /// [`Self::start_sync`], content included, or `None` if it doesn't
    /// exist. The only way to read a document's content: [`Self::list`]
    /// and [`Self::search`] return metadata alone, so fetch it with this
    /// once the document is actually shown.
    async fn get(&self, id: Uuid) -> Result<Option<Document>>;

    /// The first page of a document previously cached in local storage by
    /// [`Self::start_sync`], as a PNG as large as fits within
    /// [`THUMBNAIL_SIDE`] x [`THUMBNAIL_SIDE`] pixels, its aspect ratio
    /// kept, on a white background; `None` if the document doesn't exist.
    /// Drawn in the background once the document is cached, and stored
    /// beside it: this only reads the stored one back and never draws one,
    /// so it's also `None` for one not drawn yet. With
    /// [`crate::BackgroundJobs::thumbnails`] off nothing draws them, so it's
    /// always `None` (bar ones stored while the job ran).
    ///
    /// Fails with [`crate::ErrorCode::UnreadablePdf`] for a document whose
    /// first page can't be drawn.
    async fn thumbnail(&self, id: Uuid) -> Result<Option<Thumbnail>>;

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

    /// Changes the cached document `metadata.id`'s name, type, attributes
    /// and folder to `metadata`'s, publishing one changelog event per
    /// action, in turn, each carrying only what it changes:
    /// `DocumentRenamed` for a new name, `DocumentAttributesUpdated`
    /// for a new type or attributes (an attribute left out of `metadata`
    /// is removed), `DocumentMoved` for a new folder (as
    /// [`Self::move_document`]). `metadata`'s other fields are ignored: the
    /// cached document's are kept. Publishes nothing if none of those
    /// changed. Fails with [`crate::ErrorCode::DocumentNotFound`] for a
    /// document not synced here, and rejects invalid attributes and a
    /// folder not synced here like [`Self::upload`].
    async fn update_metadata(&self, metadata: Metadata) -> Result<()>;

    /// Moves the document `id` into the folder `folder_id` (`None` for the
    /// root, À classer), publishing a `DocumentMoved` changelog event
    /// carrying the new folder. Fails with [`crate::ErrorCode::DocumentNotFound`] for a
    /// document not synced here, and [`crate::ErrorCode::FolderNotFound`]
    /// for a folder not synced here. Moving it where it already is
    /// publishes nothing.
    async fn move_document(&self, id: Uuid, folder_id: Option<Uuid>) -> Result<()>;

    /// Publishes a `DocumentDeleted` changelog event for the document `id`. Like
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
    /// document event (see [`EventType`]), updates local storage, indexing
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

    /// How far the job [`Self::start_sync`] started has got through the
    /// changelog (see [`SyncProgress`]): a device just logged into replays
    /// the account's whole history, and a UI can poll this to show how
    /// much of it has arrived. Counted afresh by every job, so it starts
    /// from nothing again at each login. Delegates to the
    /// [`crate::domains::changelog`] service, like [`Self::start_sync`].
    fn sync_progress(&self) -> SyncProgress;

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
/// [`start_model_loading`] already did) and the documents [`Service`]'s
/// searches, so it's loaded once.
pub(crate) fn init_embedder(models: Arc<ModelStore>) -> Arc<dyn embedder::Embedder> {
    Arc::new(embedder::OnnxEmbedder::new(models))
}

/// Initializes the OCR the documents [`Service`] reads uploads' scanned
/// pages with, from the models `models` holds (fetched on its first use,
/// unless [`start_model_loading`] already did).
pub(crate) fn init_ocr(models: Arc<ModelStore>) -> Arc<dyn transcript::Ocr> {
    Arc::new(transcript::OnnxOcr::new(models))
}

/// Initializes GLiNER, which the documents [`Service`] reads uploads'
/// attributes with, from the models `models` holds: fetched and loaded on
/// its first use (or [`AttributeModel::load`], as a scraper run's setup
/// does), never at startup, and unloaded once nothing has used nor held it
/// for `idle_timeout`.
pub(crate) fn init_span_finder(
    models: Arc<ModelStore>,
    idle_timeout: std::time::Duration,
) -> Arc<dyn AttributeModel> {
    Arc::new(gliner::OnnxGliner::new(models, idle_timeout))
}

#[cfg(test)]
pub(crate) use gliner::{MockSpanFinder as MockAttributeModel, Span};
/// The model the documents [`Service`] reads uploads' attributes with (see
/// [`init_span_finder`]), which a scraper run loads, and holds, while it
/// sets up.
pub(crate) use gliner::{ModelHold, SpanFinder as AttributeModel};

pub(crate) use service::ModelLoading;

/// Starts downloading (if needed) and loading in memory, in the background,
/// the models `embedder` (see [`init_embedder`]) and `ocr` (see
/// [`init_ocr`]) run, each on its own task so they're fetched in parallel.
/// Not GLiNER: it's loaded when a scraper run sets up, or by the first
/// upload needing it (see [`init_span_finder`]). Dropping the returned
/// [`ModelLoading`] stops whatever is still under way.
pub(crate) fn start_model_loading(
    embedder: Arc<dyn embedder::Embedder>,
    ocr: Arc<dyn transcript::Ocr>,
) -> ModelLoading {
    service::start_model_loading(embedder, ocr)
}

/// Initializes the [`Indexer`] the changelog consumer caches documents
/// through, into the database behind `pool`, embedding them with
/// `embedder` (see [`init_embedder`]), unless `cache` holds the embeddings
/// their upload computed, and waking `thumbnails` (see
/// [`start_thumbnail_job`]) once one is cached.
pub(crate) fn init_indexer(
    pool: SqlitePool,
    embedder: Arc<dyn embedder::Embedder>,
    cache: Arc<dyn EmbeddingCache>,
    thumbnails: Arc<ThumbnailWake>,
) -> Arc<dyn Indexer> {
    Arc::new(service::DocumentIndexer::new(
        storage_sqlite::SqliteStorage::new(pool),
        embedder,
        cache,
        thumbnails,
    ))
}

/// Wakes the job [`start_thumbnail_job`] starts when a document is cached.
pub(crate) type ThumbnailWake = tokio::sync::Notify;

pub(crate) use service::ThumbnailJob;

/// Starts drawing, in the background, the thumbnail of every document
/// cached in the database behind `pool` that has none yet, newest first,
/// one at a time, then sleeping until `wake` is notified. Dropping the
/// returned [`ThumbnailJob`] stops it.
pub(crate) fn start_thumbnail_job(pool: SqlitePool, wake: Arc<ThumbnailWake>) -> ThumbnailJob {
    service::start_thumbnail_job(storage_sqlite::SqliteStorage::new(pool), wake)
}

/// Initializes the documents service: publishes new documents through the
/// shared `changelog` service (which also materializes consumed events
/// into the local `documents` table through `indexer` — see
/// `changelog::init`), wires up local SQLite-backed reads (via `pool`) of
/// the documents it cached, and calls `on_document_change` (see
/// [`crate::ClientConfig`]) for every event [`Service::start_sync`]
/// consumes. Uploads and searches embed their text with `embedder` (see
/// [`init_embedder`]), uploads leaving theirs in `cache` for `indexer`, and
/// are classified with it; their scanned pages are read with `ocr` (see
/// [`init_ocr`]), and their attributes extracted with `span_finder` (see
/// [`init_span_finder`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn init(
    pool: SqlitePool,
    changelog: Arc<dyn changelog::Service>,
    attributes: Arc<dyn AttributesService>,
    indexer: Arc<dyn Indexer>,
    embedder: Arc<dyn embedder::Embedder>,
    cache: Arc<dyn EmbeddingCache>,
    folders: Arc<dyn crate::domains::folders::Service>,
    ocr: Arc<dyn transcript::Ocr>,
    span_finder: Arc<dyn AttributeModel>,
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
        ocr,
        extraction::init(span_finder),
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
