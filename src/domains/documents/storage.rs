use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use uuid::Uuid;

use crate::Result;

use super::{Document, Metadata};

/// Caches documents, unencrypted, locally. Implementations are injected
/// into [`super::service::DocumentsClient`] (to cache what it uploads) and
/// into `changelog::ChangelogClient` (to cache documents materialized from
/// consumed events) at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Persists a document as-is, unencrypted.
    async fn save_document(&self, document: &Document) -> Result<()>;

    /// Fetches a document previously saved locally by [`Self::save_document`],
    /// or `None` if it doesn't exist.
    async fn get_document(&self, id: Uuid) -> Result<Option<Document>>;

    /// Lists documents previously saved locally by [`Self::save_document`],
    /// ordered oldest first, skipping `offset` and returning at most `limit`.
    async fn list_documents(&self, offset: i64, limit: i64) -> Result<Vec<Document>>;

    /// Replaces the metadata of the document `id`, leaving its content
    /// untouched. Does nothing if no document with `id` was previously
    /// saved locally.
    async fn update_metadata(&self, id: Uuid, metadata: &Metadata) -> Result<()>;
}
