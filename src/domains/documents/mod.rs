mod changelog;
mod dev;
mod models;
pub(crate) mod parser;
mod service;
mod storage;
mod storage_sqlite;

#[cfg(test)]
pub(crate) use changelog::FakeChangelogEvent;
pub use changelog::{ChangelogEvent, EventType};
pub use models::{
    Document, Metadata, Purpose, SourceCategory, SourceSubCategory, UploadRequest, UploadSource,
};
#[cfg(test)]
pub(crate) use models::{FakeDocument, FakeMetadata};
#[cfg(test)]
pub(crate) use storage::MockStorage;
pub(crate) use storage::Storage;
pub(crate) use storage_sqlite::SqliteStorage;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::scripts::Service as ScriptsService;
use crate::domains::server_state::Service as ServerStateService;
use crate::domains::sessions::SessionsClient;
use crate::domains::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

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
    /// `source_category`, `source_sub_category`, `subjects`, `purpose`)
    /// override whatever would otherwise be derived from the document
    /// itself and any scripts run against it.
    ///
    /// The file type is determined from the source's name's extension;
    /// only `.pdf` is currently accepted, anything else is rejected with
    /// [`crate::Error::UnsupportedDocumentExtension`].
    async fn upload(&self, request: UploadRequest) -> Result<Uuid>;

    /// Fetches a document previously cached in local storage by
    /// [`Self::start_sync`], or `None` if it doesn't exist.
    async fn get(&self, id: Uuid) -> Result<Option<Document>>;

    /// Lists documents previously cached in local storage, oldest first,
    /// one page at a time.
    ///
    /// Paginate by repeatedly incrementing `offset` by the returned page's
    /// length until it comes back shorter than `limit`.
    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Document>>;

    /// Encrypts `metadata` as given and publishes it as an "update metadata"
    /// changelog event, replacing whatever metadata was previously
    /// associated with `metadata.id`. The caller is responsible for
    /// constructing the full replacement `Metadata` (its fields are public
    /// for this purpose).
    async fn update_metadata(&self, metadata: Metadata) -> Result<()>;

    /// Fetches the scripts currently enabled for the authenticated user and
    /// runs each of them, in its own freshly-constructed sandboxed Lua VM,
    /// scoped to `document`, folding each script's changes into the next's
    /// starting point. Returns the resulting metadata without publishing or
    /// otherwise persisting it anywhere — a script only ever computes new
    /// metadata, it never updates the document itself; that's left to the
    /// caller to do (or not).
    async fn run_scripts(&self, document: &Document) -> Result<Metadata>;

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
    /// Delegates entirely to the internal changelog service's
    /// `start_consume_job` — the changelog itself is private to this
    /// domain, this is the only way to reach it from outside.
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
    /// Delegates to the internal changelog service's `stop_consume_job` —
    /// see [`Self::start_sync`]'s own doc comment for why that's the only
    /// way to reach it.
    fn stop_sync(&self);
}

/// Initializes the documents service: connects the internal changelog
/// service to the fyde server over `channel` (see `changelog::init`), wires
/// up local SQLite-backed caching (via `pool`) of documents materialized
/// from its consumed events, publishes new documents through it, initializes
/// the internal parser service (see `parser::init`) with `scripts` to derive
/// a newly uploaded document's metadata and fill in its classification
/// fields, and calls `on_document_change` (see [`crate::ClientConfig`]) for
/// every event [`Service::start_sync`] consumes.
pub(crate) async fn init(
    channel: Channel,
    pool: SqlitePool,
    settings: Arc<dyn SettingsService>,
    sessions: Arc<SessionsClient>,
    server_state: Arc<dyn ServerStateService>,
    scripts: Arc<dyn ScriptsService>,
    on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
) -> Result<Arc<dyn Service>> {
    let storage = storage_sqlite::SqliteStorage::new(pool.clone());
    let changelog = changelog::init(channel, pool, settings, sessions, server_state)
        .await
        .context("failed to initialize changelog service")?;
    let parser = parser::init(scripts);

    Ok(Arc::new(service::DocumentsClient::new(
        storage,
        changelog,
        parser,
        on_document_change,
    )))
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
