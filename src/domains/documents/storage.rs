use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use uuid::Uuid;

use crate::Result;

use super::{Document, DocumentEmbeddings, FacetCount, Metadata, SearchFilters, SearchOrder};

/// A document field [`Storage::count_documents_by`] can count values of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FacetField {
    Category,
    Subcategory,
    Type,
    ContentType,
}

/// Caches documents, unencrypted, locally, with their embeddings.
/// Implementations are injected into [`super::service::DocumentsClient`]
/// (to read cached documents back), [`super::service::DocumentIndexer`] (to
/// cache documents materialized from consumed events) and
/// `changelog::ChangelogClient` (to spot duplicates) at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Persists a document as-is, unencrypted, with `embeddings` as its
    /// embeddings: both or neither.
    async fn save_document(
        &self,
        document: &Document,
        embeddings: &DocumentEmbeddings,
    ) -> Result<()>;

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

    /// The ids of the documents [`Self::search_documents`] would match for
    /// `terms` (at least one) and `filters`, best `bm25` match first (the
    /// order [`SearchOrder::Relevance`] lists them in), at most `limit`.
    async fn rank_by_terms(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<Uuid>>;

    /// The ids of the documents matching every one of `filters` that have
    /// embeddings from `model`, each with its best chunk's cosine similarity
    /// to `query`, most similar first (ties by id), at most `limit`.
    async fn rank_by_similarity(
        &self,
        query: &[f32],
        model: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>>;

    /// The metadata of every document among `ids` previously saved locally
    /// by [`Self::save_document`], in no particular order, skipping ids with
    /// no document. Never loads their content.
    async fn get_metadatas(&self, ids: &[Uuid]) -> Result<Vec<Metadata>>;

    /// Like [`Self::count_documents_by`], over the documents among `ids`.
    async fn count_documents_among(
        &self,
        ids: &[Uuid],
        field: FacetField,
    ) -> Result<Vec<FacetCount<String>>>;

    /// Like [`Self::created_at_range`], over the documents among `ids`.
    async fn created_at_range_among(&self, ids: &[Uuid]) -> Result<Option<(i64, i64)>>;

    /// Fetches the metadata of a document previously saved locally by
    /// [`Self::save_document`], or `None` if it doesn't exist. Never loads
    /// its content.
    async fn get_metadata(&self, id: Uuid) -> Result<Option<Metadata>>;

    /// Replaces the metadata of the document `id`, leaving its content
    /// untouched, and its embeddings with `embeddings` when given: all or
    /// nothing. Does nothing if no document with `id` was previously saved
    /// locally and no `embeddings` are given (they'd have no document to
    /// belong to, so fail).
    async fn update_metadata(
        &self,
        id: Uuid,
        metadata: &Metadata,
        embeddings: Option<DocumentEmbeddings>,
    ) -> Result<()>;
}
