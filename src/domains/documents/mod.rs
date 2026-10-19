pub(crate) mod attributes;
mod dev;
mod models;
mod service;
mod storage;
mod storage_sqlite;
mod transcript;

#[cfg(test)]
pub(crate) use crate::domains::changelog::FakeChangelogEvent;
pub use crate::domains::changelog::{ChangelogEvent, EventType};
pub use attributes::{
    Attribute, AttributeDefinition, AttributeType, Service as AttributesService,
    SetAttributeRequest,
};
pub use models::{
    Category, Document, DocumentType, FacetCount, Metadata, SearchFacets, SearchFilters,
    SearchOrder, SearchQuery, Subcategory, UploadRequest, UploadSource,
};
#[cfg(test)]
pub(crate) use models::{FakeDocument, FakeMetadata, FakeSearchFacets};
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

use crate::Result;
use crate::domains::changelog;

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
    /// generated id. Metadata fields set on `request` (`name`, `type`,
    /// `category`, `subcategory`, `attributes`) override whatever would
    /// otherwise be derived from the document itself: its name comes from
    /// the source's file name, its type, classification and attributes are
    /// left empty. Attributes that aren't valid — see
    /// [`AttributesService::check`] — are rejected with
    /// [`crate::ErrorCode::InvalidAttribute`].
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
    /// their content.
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
    /// parent filter is picked — that actually lead to documents.
    ///
    /// Fails with [`crate::ErrorCode::InvalidSearchQuery`] for a
    /// `created_from` later than `created_until`.
    async fn search_facets(&self, text: &str, filters: &SearchFilters) -> Result<SearchFacets>;

    /// Encrypts `metadata` as given and publishes it as an "update metadata"
    /// changelog event, replacing whatever metadata was previously
    /// associated with `metadata.id`. The caller is responsible for
    /// constructing the full replacement `Metadata` (its fields are public
    /// for this purpose), `attributes` included: an attribute left out of
    /// it is removed. Rejects invalid attributes like [`Self::upload`].
    async fn update_metadata(&self, metadata: Metadata) -> Result<()>;

    /// Starts a background job that streams and decrypts every changelog
    /// event since the last consumed id, oldest first (replaying persisted
    /// history, then continuing with the live tail). Resumes from the
    /// cursor persisted locally by a previous call, or from the beginning
    /// of the changelog if it's never been consumed before. For each
    /// "created"/"update metadata" event, updates local storage before
    /// invoking the `on_document_change` callback configured on the client
    /// (see [`crate::ClientConfig`]), so it's already visible via
    /// [`Self::get`]/[`Self::list`] by the time that callback runs.
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
}

/// Initializes the documents service: publishes new documents through the
/// shared `changelog` service (which also materializes consumed events
/// into the local `documents` table — see `changelog::init`), wires up
/// local SQLite-backed reads (via `pool`) of the documents it cached,
/// and calls `on_document_change` (see
/// [`crate::ClientConfig`]) for every event [`Service::start_sync`]
/// consumes.
pub(crate) fn init(
    pool: SqlitePool,
    changelog: Arc<dyn changelog::Service>,
    attributes: Arc<dyn AttributesService>,
    on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
) -> Arc<dyn Service> {
    let storage = storage_sqlite::SqliteStorage::new(pool);

    Arc::new(service::DocumentsClient::new(
        storage,
        changelog,
        on_document_change,
        attributes,
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
