use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use uuid::Uuid;

use crate::Result;

use super::{Document, FacetCount, Metadata, SearchFilters, SearchOrder};

/// A document field [`Storage::count_documents_by`] can count values of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FacetField {
    Category,
    Subcategory,
    Type,
    ContentType,
}

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

    /// Returns the id of a document previously saved locally by
    /// [`Self::save_document`] whose content has the given hex-encoded
    /// SHA-256 `checksum`, or `None` if there's none.
    async fn find_document_by_checksum(&self, checksum: &str) -> Result<Option<Uuid>>;

    /// Lists the metadata of documents previously saved locally by
    /// [`Self::save_document`], ordered oldest first, skipping `offset` and
    /// returning at most `limit`. Never loads their content.
    async fn list_documents(&self, offset: i64, limit: i64) -> Result<Vec<Metadata>>;

    /// Lists the metadata of documents previously saved locally by
    /// [`Self::save_document`] that match every one of `terms` (each as a case- and
    /// accent-insensitive prefix of a word in any searchable field — all of
    /// them when `terms` is empty) and every one of `filters`, in `order`,
    /// skipping `offset` and returning at most `limit`. Never loads their
    /// content.
    async fn search_documents(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        order: SearchOrder,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Metadata>>;

    /// Counts, per distinct non-empty value of `field`, how many documents
    /// [`Self::search_documents`] would match for `terms` and `filters`,
    /// most common first (ties by value). Each value is that field's
    /// canonical string form (e.g. [`super::Category::as_str`]).
    async fn count_documents_by(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        field: FacetField,
    ) -> Result<Vec<FacetCount<String>>>;

    /// The oldest and newest `created_at` among the documents
    /// [`Self::search_documents`] would match for `terms` and `filters`, or
    /// `None` if it would match none.
    async fn created_at_range(
        &self,
        terms: &[String],
        filters: &SearchFilters,
    ) -> Result<Option<(i64, i64)>>;

    /// Replaces the metadata of the document `id`, leaving its content
    /// untouched. Does nothing if no document with `id` was previously
    /// saved locally.
    async fn update_metadata(&self, id: Uuid, metadata: &Metadata) -> Result<()>;
}
