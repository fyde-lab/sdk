mod changelog;
mod models;
mod service;
mod storage;
mod storage_sqlite;
mod transcript;

#[cfg(test)]
pub(crate) use changelog::FakeChangelogEvent;
pub use changelog::{ChangelogEvent, EventType};
pub use models::{Document, Metadata};
#[cfg(test)]
pub(crate) use models::{FakeDocument, FakeMetadata};
#[cfg(test)]
pub(crate) use storage::MockStorage;
pub(crate) use storage::Storage;
pub(crate) use storage_sqlite::SqliteStorage;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::domains::sessions::SessionsClient;
use crate::domains::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

/// Uploads documents by publishing them as encrypted changelog events, and
/// reads back documents materialized locally from consumed events (see
/// [`Self::sync`]) — there is no server-side document store to fetch from.
#[async_trait]
pub trait Service: Send + Sync {
    /// Reads the file at `path`, encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its
    /// generated id.
    ///
    /// The file type is determined from `path`'s extension; only `.pdf` is
    /// currently accepted, anything else is rejected with
    /// [`crate::Error::UnsupportedDocumentExtension`].
    async fn upload(&self, path: &Path) -> Result<Uuid>;

    /// Fetches a document previously cached in local storage by
    /// [`Self::sync`], or `None` if it doesn't exist.
    async fn get(&self, id: Uuid) -> Result<Option<Document>>;

    /// Lists documents previously cached in local storage, oldest first,
    /// one page at a time.
    ///
    /// Paginate by repeatedly incrementing `offset` by the returned page's
    /// length until it comes back shorter than `limit`.
    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Document>>;

    /// Renames a document: replaces `metadata`'s `name` with `new_name`,
    /// encrypts the result, and publishes it as an "update metadata"
    /// changelog event.
    async fn update_name(&self, metadata: Metadata, new_name: String) -> Result<()>;

    /// Streams and decrypts every changelog event since the last consumed
    /// id, oldest first, invoking `callback` once per event (replaying
    /// persisted history, then continuing with the live tail). Resumes
    /// from the cursor persisted locally by a previous call, or from the
    /// beginning of the changelog if it's never been consumed before. For
    /// each "created" event, caches the resulting document in local
    /// storage before invoking `callback`, so it becomes visible to
    /// [`Self::get`]/[`Self::list`].
    ///
    /// Delegates entirely to the internal changelog service's `consume` —
    /// the changelog itself is private to this domain, this is the only
    /// way to reach it from outside.
    ///
    /// Runs until the server closes the stream or an error occurs.
    async fn sync(&self, callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()>;
}

/// Initializes the documents service: connects the internal changelog
/// service to the fyde server at `base_url` (see `changelog::init`), wires
/// up local SQLite-backed caching (via `pool`) of documents materialized
/// from its consumed events, and publishes new documents through it.
pub(crate) async fn init(
    base_url: impl AsRef<str>,
    pool: SqlitePool,
    settings: Arc<dyn SettingsService>,
    sessions: Arc<SessionsClient>,
) -> Result<Arc<dyn Service>> {
    let storage = storage_sqlite::SqliteStorage::new(pool.clone());
    let changelog = changelog::init(base_url, pool, settings, sessions)
        .await
        .context("failed to initialize changelog service")?;

    Ok(Arc::new(service::DocumentsClient::new(storage, changelog)))
}
