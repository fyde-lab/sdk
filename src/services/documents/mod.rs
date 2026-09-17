mod crypto;
mod grpc_client;
mod service;
mod storage;
mod storage_sqlite;
mod transcript;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

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

/// A file, as returned by [`Service::download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: Uuid,
    pub content: Vec<u8>,
    pub metadata: Metadata,
}

/// A file to be persisted by [`Service::upload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDocument {
    pub name: String,
    pub content_type: String,
    pub content: Vec<u8>,
}

/// Saves and fetches documents, either unencrypted in local storage or
/// encrypted on the fyde server.
#[async_trait]
pub trait Service: Send + Sync {
    /// Encrypts `document` and uploads it to the server, returning its
    /// generated id.
    ///
    /// Only `application/pdf` content is accepted; anything else is
    /// rejected with [`crate::Error::UnsupportedContentType`].
    async fn upload(&self, document: NewDocument) -> Result<Uuid>;

    /// Fetches a document's content by id, or `None` if it doesn't exist.
    async fn download(&self, id: Uuid) -> Result<Option<Document>>;

    /// Fetches a document previously cached in local storage by
    /// [`Service::download`] or [`Service::download_many`], or `None` if it
    /// doesn't exist. Unlike [`Service::download`], this does not talk to the
    /// server.
    async fn get(&self, id: Uuid) -> Result<Option<Document>>;

    /// Fetches multiple documents' content by id in a single call. Ids that
    /// don't exist are omitted from the result.
    async fn download_many(&self, ids: Vec<Uuid>) -> Result<Vec<Document>>;

    /// Lists documents previously cached in local storage, oldest first,
    /// one page at a time. Like [`Service::get`], this does not talk to the
    /// server.
    ///
    /// Paginate by repeatedly incrementing `offset` by the returned page's
    /// length until it comes back shorter than `limit`.
    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Document>>;
}

/// Initializes the documents service: connects to the fyde server at
/// `base_url`, and wires up local SQLite-backed caching (via `pool`) of
/// documents fetched from it.
pub(crate) async fn init(base_url: impl AsRef<str>, pool: SqlitePool) -> Result<Arc<dyn Service>> {
    let storage = storage_sqlite::SqliteStorage::new(pool);
    let client = service::DocumentsClient::new(base_url, storage)
        .await
        .context("failed to create documents client")?;

    Ok(Arc::new(client))
}
