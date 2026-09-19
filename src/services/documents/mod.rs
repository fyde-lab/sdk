mod service;
mod storage;
mod storage_sqlite;
mod transcript;

pub(crate) use storage::Storage;
pub(crate) use storage_sqlite::SqliteStorage;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::Result;
use crate::services::changelog::Service as ChangelogService;

/// Cleartext metadata encrypted under a document's DEK before upload, and
/// decrypted back out of it on download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub content_type: String,
    pub created_at: i64,
    pub size: u64,
    pub checksum: String,
    /// Plaintext transcript of the document's PDF text content, extracted
    /// on upload.
    pub transcript: String,
}

/// A file, as returned by [`Service::get`]/[`Service::list`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
    pub metadata: Metadata,
}

/// Uploads documents by publishing them as encrypted changelog events, and
/// reads back documents materialized locally from consumed events (see
/// [`crate::ChangelogService::consume`]) — there is no server-side
/// document store to fetch from.
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
    /// [`crate::ChangelogService::consume`], or `None` if it doesn't
    /// exist.
    async fn get(&self, id: Uuid) -> Result<Option<Document>>;

    /// Lists documents previously cached in local storage, oldest first,
    /// one page at a time.
    ///
    /// Paginate by repeatedly incrementing `offset` by the returned page's
    /// length until it comes back shorter than `limit`.
    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Document>>;
}

/// Initializes the documents service: wires up local SQLite-backed caching
/// (via `pool`) of documents materialized from `changelog`'s consumed
/// events, and publishes new documents through it.
pub(crate) fn init(pool: SqlitePool, changelog: Arc<dyn ChangelogService>) -> Arc<dyn Service> {
    let storage = storage_sqlite::SqliteStorage::new(pool);
    Arc::new(service::DocumentsClient::new(storage, changelog))
}
