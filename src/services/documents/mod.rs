mod crypto;
mod grpc_client;
mod service;
mod storage;
mod storage_sqlite;

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::Result;

/// A file, as returned by [`Service::download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: Uuid,
    pub name: String,
    pub content_type: String,
    pub content: Vec<u8>,
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
    let client = service::DocumentsClient::new(base_url, storage).await?;

    Ok(Arc::new(client))
}
