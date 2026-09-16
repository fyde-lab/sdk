use uuid::Uuid;

use crate::Result;

use super::Document;

/// Persists documents saved locally, unencrypted, by
/// [`super::DocumentsClient::save`]. Implementations are injected into
/// [`super::DocumentsClient`] at construction.
pub trait Storage: Send + Sync {
    /// Persists a document as-is, unencrypted.
    fn save_document(
        &self,
        document: &Document,
        created_at: i64,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Fetches a document previously saved locally by [`Self::save_document`],
    /// or `None` if it doesn't exist.
    fn get_document(&self, id: Uuid) -> impl Future<Output = Result<Option<Document>>> + Send;
}
