use uuid::Uuid;

use crate::Result;

use super::Document;

/// Caches documents, unencrypted, locally. Implementations are injected
/// into [`super::service::DocumentsClient`] (to cache what it uploads) and
/// into `changelog::ChangelogClient` (to cache documents materialized from
/// consumed events) at construction.
pub(crate) trait Storage: Send + Sync {
    /// Persists a document as-is, unencrypted.
    fn save_document(&self, document: &Document) -> impl Future<Output = Result<()>> + Send;

    /// Fetches a document previously saved locally by [`Self::save_document`],
    /// or `None` if it doesn't exist.
    fn get_document(&self, id: Uuid) -> impl Future<Output = Result<Option<Document>>> + Send;

    /// Lists documents previously saved locally by [`Self::save_document`],
    /// ordered oldest first, skipping `offset` and returning at most `limit`.
    fn list_documents(
        &self,
        offset: i64,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<Document>>> + Send;
}
