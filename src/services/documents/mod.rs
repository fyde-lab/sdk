mod crypto;
mod grpc_client;
mod service;
mod storage;
mod storage_sqlite;

pub use service::DocumentsClient;
pub use storage::Storage;
pub use storage_sqlite::SqliteStorage;

use async_trait::async_trait;
use uuid::Uuid;

use crate::Result;

/// A file, as returned by [`Service::fetch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: Uuid,
    pub name: String,
    pub content_type: String,
    pub content: Vec<u8>,
}

/// A file to be persisted by [`Service::save`] or [`Service::upload`].
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
    /// Saves `document` as-is, unencrypted, in local storage, returning its
    /// generated id.
    async fn save(&self, document: NewDocument) -> Result<Uuid>;

    /// Encrypts `document` and uploads it to the server, returning its
    /// generated id.
    async fn upload(&self, document: NewDocument) -> Result<Uuid>;

    /// Fetches a document's content by id, or `None` if it doesn't exist.
    async fn fetch(&self, id: Uuid) -> Result<Option<Document>>;

    /// Fetches a document previously saved via [`Service::save`] from local
    /// storage, or `None` if it doesn't exist. Unlike [`Service::fetch`],
    /// this does not talk to the server.
    async fn get(&self, id: Uuid) -> Result<Option<Document>>;

    /// Fetches multiple documents' content by id in a single call. Ids that
    /// don't exist are omitted from the result.
    async fn fetch_many(&self, ids: Vec<Uuid>) -> Result<Vec<Document>>;
}
