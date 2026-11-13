use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use futures::TryStreamExt as _;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::{ErrorContext as _, ErrorKind, Result};

use super::storage::{FacetField, Storage, UNFILED};
use super::{
    Attribute, Document, DocumentEmbeddings, DocumentType, FacetCount, FolderFilter, Metadata,
    SearchFilters, SearchOrder,
};

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `documents` table).
pub(crate) struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn save_document(
        &self,
        document: &Document,
        embeddings: &DocumentEmbeddings,
    ) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .with_context(|| format!("failed to start saving document {}", document.id))?;

        sqlx::query(
            "INSERT INTO documents (id, name, original_name, content_type, content, checksum, created_at, transcript, type, folder_id, attributes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(document.id.to_string())
        .bind(&document.metadata.name)
        .bind(&document.metadata.original_name)
        .bind(&document.metadata.content_type)
        .bind(&document.content)
        .bind(&document.metadata.checksum)
        .bind(document.metadata.created_at)
        .bind(&document.metadata.transcript)
        .bind(type_to_column(document.metadata.r#type))
        .bind(document.metadata.folder_id.map(|id| id.to_string()))
        .bind(attributes_to_column(&document.metadata.attributes)?)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to save document {} to local database", document.id))?;

        insert_embeddings(&mut tx, document.id, embeddings).await?;

        tx.commit()
            .await
            .with_context(|| format!("failed to commit saving document {}", document.id))
    }

    async fn get_document(&self, id: Uuid) -> Result<Option<Document>> {
        let row = sqlx::query(
            "SELECT name, original_name, content_type, content, checksum, created_at, transcript, type, folder_id, attributes FROM documents WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("failed to fetch document {id} from local database"))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let content: Vec<u8> = row.get("content");

        Ok(Some(Document {
            id,
            metadata: Metadata {
                id,
                name: row.get("name"),
                original_name: row.get("original_name"),
                content_type: row.get("content_type"),
                created_at: row.get("created_at"),
                size: content.len() as u64,
                checksum: row.get("checksum"),
                transcript: row.get("transcript"),
                r#type: type_from_column(row.get("type"))?,
                folder_id: folder_from_column(row.get("folder_id"))?,
                attributes: attributes_from_column(row.get("attributes"))?,
            },
            content,
        }))
    }

    async fn find_document_by_checksum(&self, checksum: &str) -> Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM documents WHERE checksum = ?1 LIMIT 1")
            .bind(checksum)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| {
                format!("failed to look up document with checksum {checksum} in local database")
            })?;

        let Some(row) = row else {
            return Ok(None);
        };

        let id: String = row.get("id");
        let id = Uuid::parse_str(&id)
            .with_context(|| format!("invalid document id in local database: {id}"))?;
        Ok(Some(id))
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn list_documents(&self, offset: i64, limit: i64) -> Result<Vec<Metadata>> {
        let rows = sqlx::query(
            "SELECT id, name, original_name, content_type, length(content) AS size, checksum, created_at, transcript, type, folder_id, attributes FROM documents
             ORDER BY created_at ASC, id ASC
             LIMIT ?1 OFFSET ?2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .instrument(tracing::debug_span!("query"))
        .await
        .with_context(|| format!("failed to list documents at offset {offset}"))?;

        tracing::debug_span!("map_rows", rows = rows.len())
            .in_scope(|| rows.iter().map(metadata_from_row).collect())
    }

    async fn search_documents(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        order: SearchOrder,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Metadata>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT d.id, d.name, d.original_name, d.content_type, length(d.content) AS size, d.checksum, d.created_at, d.transcript, d.type, d.folder_id, d.attributes",
        );
        push_matching(&mut query, terms, filters);

        query.push(" ORDER BY ");
        match order {
            SearchOrder::Relevance => {
                // Without terms there's no `documents_fts` match to score,
                // which leaves the newest-first tie-break below as the
                // whole ordering.
                if !terms.is_empty() {
                    query.push(FTS_RANK).push(", ");
                }
                query.push("d.created_at DESC, d.id DESC");
            }
            SearchOrder::Newest => {
                query.push("d.created_at DESC, d.id DESC");
            }
            SearchOrder::Oldest => {
                query.push("d.created_at ASC, d.id ASC");
            }
        }

        query
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to search documents at offset {offset}"))?;

        rows.iter().map(metadata_from_row).collect()
    }

    async fn count_documents_by(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        field: FacetField,
    ) -> Result<Vec<FacetCount<String>>> {
        // `''` and `NULL` both mean "unset" here: see the `*_to_column`
        // helpers below.
        let mut query = QueryBuilder::<Sqlite>::new(format!(
            "SELECT value, COUNT(*) AS count FROM (SELECT {} AS value",
            facet_column(field, "d.")
        ));
        push_matching(&mut query, terms, filters);
        query.push(") WHERE value IS NOT NULL AND value != '' GROUP BY value");
        query.push(" ORDER BY count DESC, value ASC");

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to count documents by {field:?}"))?;

        Ok(rows.iter().map(facet_count_from_row).collect())
    }

    async fn created_at_range(
        &self,
        terms: &[String],
        filters: &SearchFilters,
    ) -> Result<Option<(i64, i64)>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT MIN(d.created_at) AS earliest, MAX(d.created_at) AS latest",
        );
        push_matching(&mut query, terms, filters);

        let row = query
            .build()
            .fetch_one(&self.pool)
            .await
            .context("failed to find the creation date range of matching documents")?;

        let earliest: Option<i64> = row.get("earliest");
        let latest: Option<i64> = row.get("latest");
        Ok(earliest.zip(latest))
    }

    async fn rank_by_terms(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<Uuid>> {
        let mut query = QueryBuilder::<Sqlite>::new("SELECT d.id");
        push_matching(&mut query, terms, filters);
        query
            .push(" ORDER BY ")
            .push(FTS_RANK)
            .push(", d.created_at DESC, d.id DESC LIMIT ")
            .push_bind(limit as i64);

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .context("failed to rank documents by their terms")?;

        rows.iter()
            .map(|row| id_from_column(row.get("id")))
            .collect()
    }

    async fn rank_by_similarity(
        &self,
        query: &[f32],
        model: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>> {
        let query_norm = norm(query);
        let mut sql = QueryBuilder::<Sqlite>::new(
            "SELECT e.document_id AS id, e.vector FROM document_embeddings e WHERE e.model = ",
        );
        sql.push_bind(model)
            .push(" AND e.document_id IN (SELECT d.id");
        push_matching(&mut sql, &[], filters);
        sql.push(")");

        // Streamed: every chunk vector of every document is read, so they're
        // never all held in memory at once.
        let mut best = HashMap::<String, f32>::new();
        let mut rows = sql.build().fetch(&self.pool);
        while let Some(row) = rows
            .try_next()
            .await
            .context("failed to read document embeddings")?
        {
            let id: String = row.get("id");
            let vector: &[u8] = row.get("vector");
            let Some(similarity) = cosine(query, query_norm, vector)
                .with_context(|| format!("invalid embedding of document {id}"))?
            else {
                continue;
            };
            best.entry(id)
                .and_modify(|best| *best = best.max(similarity))
                .or_insert(similarity);
        }

        let mut ranked = best
            .into_iter()
            .map(|(id, similarity)| Ok((id_from_column(id)?, similarity)))
            .collect::<Result<Vec<_>>>()?;
        ranked.sort_by(|(a_id, a), (b_id, b)| b.total_cmp(a).then(a_id.cmp(b_id)));
        ranked.truncate(limit);
        Ok(ranked)
    }

    async fn similarity_by_folder(
        &self,
        query: &[f32],
        model: &str,
        folders: &[Uuid],
    ) -> Result<Vec<(Uuid, f32)>> {
        let query_norm = norm(query);
        // Streamed, like `rank_by_similarity`.
        let mut rows = sqlx::query(
            "SELECT e.document_id AS id, d.folder_id, e.vector
             FROM document_embeddings e JOIN documents d ON d.id = e.document_id
             WHERE e.model = ?1 AND d.folder_id IN (SELECT value FROM json_each(?2))",
        )
        .bind(model)
        .bind(ids_to_json(folders)?)
        .fetch(&self.pool);

        let mut best = HashMap::<String, (String, f32)>::new();
        while let Some(row) = rows
            .try_next()
            .await
            .context("failed to read the embeddings of filed documents")?
        {
            let id: String = row.get("id");
            let folder_id: String = row.get("folder_id");
            let vector: &[u8] = row.get("vector");
            let Some(similarity) = cosine(query, query_norm, vector)
                .with_context(|| format!("invalid embedding of document {id}"))?
            else {
                continue;
            };
            best.entry(id)
                .and_modify(|(_, best)| *best = best.max(similarity))
                .or_insert((folder_id, similarity));
        }

        best.into_values()
            .map(|(folder_id, similarity)| {
                let folder_id = Uuid::parse_str(&folder_id)
                    .with_context(|| format!("invalid folder id in local database: {folder_id}"))?;
                Ok((folder_id, similarity))
            })
            .collect()
    }

    async fn get_embedding_vectors(&self, id: Uuid, model: &str) -> Result<Vec<Vec<f32>>> {
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT vector FROM document_embeddings
             WHERE document_id = ?1 AND model = ?2 ORDER BY chunk",
        )
        .bind(id.to_string())
        .bind(model)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("failed to read document {id}'s embeddings"))?;

        rows.iter()
            .map(|column| {
                vector_from_column(column)
                    .with_context(|| format!("invalid embedding of document {id}"))
            })
            .collect()
    }

    async fn get_metadatas(&self, ids: &[Uuid]) -> Result<Vec<Metadata>> {
        let rows = sqlx::query(
            "SELECT id, name, original_name, content_type, length(content) AS size, checksum, created_at, transcript, type, folder_id, attributes FROM documents
             WHERE id IN (SELECT value FROM json_each(?1))",
        )
        .bind(ids_to_json(ids)?)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("failed to fetch the metadata of {} documents", ids.len()))?;

        rows.iter().map(metadata_from_row).collect()
    }

    async fn count_documents_among(
        &self,
        ids: &[Uuid],
        field: FacetField,
    ) -> Result<Vec<FacetCount<String>>> {
        let column = facet_column(field, "");
        let mut query = QueryBuilder::<Sqlite>::new(format!(
            "SELECT {column} AS value, COUNT(*) AS count FROM documents
             WHERE id IN (SELECT value FROM json_each("
        ));
        query.push_bind(ids_to_json(ids)?).push(format!(
            ")) AND {column} IS NOT NULL AND {column} != ''
             GROUP BY {column} ORDER BY count DESC, value ASC"
        ));
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to count listed documents by {field:?}"))?;

        Ok(rows.iter().map(facet_count_from_row).collect())
    }

    async fn created_at_range_among(&self, ids: &[Uuid]) -> Result<Option<(i64, i64)>> {
        let row = sqlx::query(
            "SELECT MIN(created_at) AS earliest, MAX(created_at) AS latest FROM documents
             WHERE id IN (SELECT value FROM json_each(?1))",
        )
        .bind(ids_to_json(ids)?)
        .fetch_one(&self.pool)
        .await
        .context("failed to find the creation date range of listed documents")?;

        let earliest: Option<i64> = row.get("earliest");
        let latest: Option<i64> = row.get("latest");
        Ok(earliest.zip(latest))
    }

    async fn get_metadata(&self, id: Uuid) -> Result<Option<Metadata>> {
        let row = sqlx::query(
            "SELECT id, name, original_name, content_type, length(content) AS size, checksum, created_at, transcript, type, folder_id, attributes FROM documents WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("failed to fetch document {id} metadata from local database"))?;

        row.as_ref().map(metadata_from_row).transpose()
    }

    async fn update_metadata(
        &self,
        id: Uuid,
        metadata: &Metadata,
        embeddings: Option<DocumentEmbeddings>,
    ) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .with_context(|| format!("failed to start updating document {id} metadata"))?;

        sqlx::query(
            "UPDATE documents
             SET name = ?1, original_name = ?2, content_type = ?3, checksum = ?4, created_at = ?5, transcript = ?6, type = ?7, folder_id = ?8, attributes = ?9
             WHERE id = ?10",
        )
        .bind(&metadata.name)
        .bind(&metadata.original_name)
        .bind(&metadata.content_type)
        .bind(&metadata.checksum)
        .bind(metadata.created_at)
        .bind(&metadata.transcript)
        .bind(type_to_column(metadata.r#type))
        .bind(metadata.folder_id.map(|id| id.to_string()))
        .bind(attributes_to_column(&metadata.attributes)?)
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to update document {id} metadata in local database"))?;

        if let Some(embeddings) = embeddings {
            sqlx::query("DELETE FROM document_embeddings WHERE document_id = ?1")
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .with_context(|| format!("failed to delete document {id}'s previous embeddings"))?;
            insert_embeddings(&mut tx, id, &embeddings).await?;
        }

        tx.commit()
            .await
            .with_context(|| format!("failed to commit updating document {id} metadata"))
    }

    async fn delete_document(&self, id: Uuid) -> Result<()> {
        // Its embeddings cascade, and the `documents_fts` triggers drop it
        // from the full-text index.
        sqlx::query("DELETE FROM documents WHERE id = ?1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .with_context(|| format!("failed to delete document {id} from local database"))?;

        Ok(())
    }

    async fn move_folder_documents(&self, folders: &[Uuid], to: Option<Uuid>) -> Result<()> {
        sqlx::query(
            "UPDATE documents SET folder_id = ?1
             WHERE folder_id IN (SELECT value FROM json_each(?2))",
        )
        .bind(to.map(|id| id.to_string()))
        .bind(ids_to_json(folders)?)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to move the documents of {} folders", folders.len()))?;

        Ok(())
    }

    async fn delete_folder_documents(&self, folders: &[Uuid]) -> Result<()> {
        // Their embeddings cascade, and the `documents_fts` triggers drop
        // them from the full-text index.
        sqlx::query("DELETE FROM documents WHERE folder_id IN (SELECT value FROM json_each(?1))")
            .bind(ids_to_json(folders)?)
            .execute(&self.pool)
            .await
            .with_context(|| {
                format!(
                    "failed to delete the documents of {} folders",
                    folders.len()
                )
            })?;

        Ok(())
    }
}

/// Inserts `embeddings` as the embeddings of the document `id`, chunks
/// numbered in order.
async fn insert_embeddings(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    id: Uuid,
    embeddings: &DocumentEmbeddings,
) -> Result<()> {
    for (chunk, embedding) in embeddings.chunks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO document_embeddings (document_id, chunk, start, \"end\", model, vector)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(id.to_string())
        .bind(chunk as i64)
        .bind(embedding.range.start as i64)
        .bind(embedding.range.end as i64)
        .bind(&embeddings.model)
        .bind(vector_to_column(&embedding.vector))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to save embedding {chunk} of document {id}"))?;
    }

    Ok(())
}

/// The column holding `field`, its table aliased as `alias` (`"d."`, or
/// `""` for none).
fn facet_column(field: FacetField, alias: &str) -> String {
    match field {
        FacetField::Folder => format!("COALESCE({alias}folder_id, '{UNFILED}')"),
        FacetField::Type => format!("{alias}type"),
        FacetField::ContentType => format!("{alias}content_type"),
    }
}

fn facet_count_from_row(row: &SqliteRow) -> FacetCount<String> {
    FacetCount {
        value: row.get("value"),
        count: row.get::<i64, _>("count") as u64,
    }
}

fn id_from_column(id: String) -> Result<Uuid> {
    Uuid::parse_str(&id).with_context(|| format!("invalid document id in local database: {id}"))
}

/// `ids` as a JSON array of their `id` columns, for `json_each`: binding one
/// array rather than one parameter per id keeps under SQLite's limit.
fn ids_to_json(ids: &[Uuid]) -> Result<String> {
    serde_json::to_string(&ids.iter().map(Uuid::to_string).collect::<Vec<_>>())
        .context("failed to encode document ids as json")
}

fn norm(vector: &[f32]) -> f32 {
    vector.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// The cosine similarity of `query` (whose length is `query_norm`) and the
/// vector stored as `column` (see [`vector_to_column`]), or `None` when
/// either is zero, pointing nowhere.
fn cosine(query: &[f32], query_norm: f32, column: &[u8]) -> Result<Option<f32>> {
    let (values, rest) = column.as_chunks::<4>();
    if values.len() != query.len() || !rest.is_empty() {
        return Err(ErrorKind::InvalidModel(format!(
            "a {}-byte embedding can't be compared to a {}-dimension query",
            column.len(),
            query.len()
        ))
        .into());
    }

    let (mut dot, mut squares) = (0.0_f32, 0.0_f32);
    for (bytes, q) in values.iter().zip(query) {
        let value = f32::from_le_bytes(*bytes);
        dot += value * q;
        squares += value * value;
    }
    let norms = squares.sqrt() * query_norm;
    Ok((norms > 0.0).then(|| dot / norms))
}

/// A vector as its `vector` column: each value as a little-endian f32.
fn vector_to_column(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// A `vector` column (see [`vector_to_column`]) as the vector it holds.
fn vector_from_column(column: &[u8]) -> Result<Vec<f32>> {
    let (values, rest) = column.as_chunks::<4>();
    if !rest.is_empty() {
        return Err(ErrorKind::InvalidModel(format!(
            "a {}-byte embedding isn't a vector of f32",
            column.len()
        ))
        .into());
    }
    Ok(values
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect())
}

fn folder_from_column(column: Option<String>) -> Result<Option<Uuid>> {
    column
        .map(|column| {
            Uuid::parse_str(&column)
                .with_context(|| format!("invalid folder id in local database: {column}"))
        })
        .transpose()
}

/// The `type` column has no NULL state (`NOT NULL DEFAULT ''`), so an
/// untyped document is stored as an empty string.
fn type_to_column(r#type: Option<DocumentType>) -> String {
    r#type.map_or_else(String::new, |r#type| r#type.as_str().to_string())
}

fn type_from_column(column: String) -> Result<Option<DocumentType>> {
    if column.is_empty() {
        return Ok(None);
    }

    column
        .parse()
        .map(Some)
        .with_context(|| format!("invalid document type in local database: {column}"))
}

/// The `attributes` column stores a JSON-encoded map, since SQLite has no
/// native map type.
/// Fails on a non-finite float, which JSON can't represent: the service
/// rejects those, but a document synced from another device isn't checked
/// before it's cached.
fn attributes_to_column(attributes: &BTreeMap<String, Attribute>) -> Result<String> {
    serde_json::to_string(attributes).context("failed to encode attributes as json")
}

fn attributes_from_column(column: String) -> Result<BTreeMap<String, Attribute>> {
    serde_json::from_str(&column)
        .with_context(|| format!("invalid attributes json in local database: {column}"))
}

/// Maps a row selecting every `documents` column but `content` (`id`
/// included), with the content's length in bytes as `size`, back to a
/// [`Metadata`] — so listing documents never loads their content.
fn metadata_from_row(row: &SqliteRow) -> Result<Metadata> {
    let id: String = row.get("id");
    let id = Uuid::parse_str(&id)
        .with_context(|| format!("invalid document id in local database: {id}"))?;
    let size: i64 = row.get("size");

    Ok(Metadata {
        id,
        name: row.get("name"),
        original_name: row.get("original_name"),
        content_type: row.get("content_type"),
        created_at: row.get("created_at"),
        size: size as u64,
        checksum: row.get("checksum"),
        transcript: row.get("transcript"),
        r#type: type_from_column(row.get("type"))?,
        folder_id: folder_from_column(row.get("folder_id"))?,
        attributes: attributes_from_column(row.get("attributes"))?,
    })
}

/// Orders `documents_fts` matches best first (`bm25` scores lower for a
/// better match), weighting each column in its declaration order in the
/// `0023_move_documents_into_folders` migration: a hit in the
/// name counts the most, one in the (long, noisy) transcript the least.
const FTS_RANK: &str = "bm25(documents_fts,
    10.0, -- name
    5.0,  -- original_name
    1.0,  -- transcript
    4.0   -- type
)";

/// Builds an FTS5 query matching documents containing every one of `terms`
/// as a token prefix, in any column. Each term is quoted as an FTS5 string
/// (doubling any `"` inside it), so nothing in it is ever parsed as query
/// syntax (`OR`, `NEAR`, `column:`...).
fn fts_match_expression(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Appends ` AND <column> IN (?, ...)` to `query`, binding every one of
/// `values`, or nothing at all when `values` is empty (no constraint).
fn push_any_of<'a>(
    query: &mut QueryBuilder<Sqlite>,
    column: &str,
    values: impl ExactSizeIterator<Item = &'a str>,
) {
    if values.len() == 0 {
        return;
    }

    query.push(format_args!(" AND {column} IN ("));
    let mut separated = query.separated(", ");
    for value in values {
        separated.push_bind(value.to_string());
    }
    query.push(")");
}

/// Appends ` FROM documents d ... WHERE ...` to `query`, selecting (as `d`)
/// every document that contains every one of `terms` as a token prefix (all
/// of them when `terms` is empty) and matches every one of `filters`.
fn push_matching(query: &mut QueryBuilder<Sqlite>, terms: &[String], filters: &SearchFilters) {
    query.push(" FROM documents d");
    if terms.is_empty() {
        query.push(" WHERE 1 = 1");
    } else {
        query.push(
            " JOIN documents_fts_rowid_map m ON m.document_id = d.id
              JOIN documents_fts ON documents_fts.rowid = m.rowid
              WHERE documents_fts MATCH ",
        );
        query.push_bind(fts_match_expression(terms));
    }

    match filters.folder {
        None => {}
        Some(FolderFilter::Unfiled) => {
            query.push(" AND (d.folder_id IS NULL OR d.folder_id NOT IN (SELECT id FROM folders))");
        }
        Some(FolderFilter::Within(folder_id)) => {
            query
                .push(" AND d.folder_id IN (WITH RECURSIVE within(id) AS (SELECT ")
                .push_bind(folder_id.to_string())
                .push(
                    " UNION SELECT f.id FROM folders f JOIN within ON f.parent_id = within.id) \
                     SELECT id FROM within)",
                );
        }
    }
    push_any_of(
        query,
        "d.type",
        filters.types.iter().map(DocumentType::as_str),
    );
    push_any_of(
        query,
        "d.content_type",
        filters.content_types.iter().map(String::as_str),
    );
    if let Some(created_from) = filters.created_from {
        query.push(" AND d.created_at >= ").push_bind(created_from);
    }
    if let Some(created_until) = filters.created_until {
        query.push(" AND d.created_at <= ").push_bind(created_until);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::super::models::{FakeChunkEmbedding, FakeDocumentEmbeddings};
    use super::super::{ChunkEmbedding, FakeDocument, FakeMetadata};
    use super::*;

    async fn setup() -> SqliteStorage {
        // A single connection, so all queries in a test hit the same
        // in-memory database rather than each getting its own.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();
        for (id, parent) in [
            (HOUSING, None),
            (MONEY, None),
            (TELECOM, None),
            (HEALTH, None),
            (BANKING, Some(HEALTH)),
        ] {
            sqlx::query("INSERT INTO folders (id, parent_id, name) VALUES (?1, ?2, ?1)")
                .bind(id.to_string())
                .bind(parent.map(|parent: Uuid| parent.to_string()))
                .execute(&pool)
                .await
                .unwrap();
        }

        SqliteStorage::new(pool)
    }

    // Folders every test database holds: `BANKING` is a subfolder of
    // `HEALTH`, the others are at the top level.
    const HOUSING: Uuid = Uuid::from_u128(1);
    const MONEY: Uuid = Uuid::from_u128(2);
    const TELECOM: Uuid = Uuid::from_u128(3);
    const HEALTH: Uuid = Uuid::from_u128(4);
    const BANKING: Uuid = Uuid::from_u128(5);

    fn embeddings() -> DocumentEmbeddings {
        FakeDocumentEmbeddings::new().build()
    }

    /// The embeddings stored for the document `id`, in chunk order, each
    /// with the model stored beside it.
    pub(in super::super) async fn stored_embeddings(
        storage: &SqliteStorage,
        id: Uuid,
    ) -> Vec<(String, ChunkEmbedding)> {
        let rows: Vec<(i64, i64, i64, String, Vec<u8>)> = sqlx::query_as(
            "SELECT chunk, start, \"end\", model, vector FROM document_embeddings
             WHERE document_id = ?1 ORDER BY chunk",
        )
        .bind(id.to_string())
        .fetch_all(&storage.pool)
        .await
        .unwrap();

        rows.into_iter()
            .enumerate()
            .map(|(i, (chunk, start, end, model, vector))| {
                assert_eq!(chunk, i as i64, "chunks are numbered in order from 0");
                let chunk = FakeChunkEmbedding::new()
                    .with_range(start as usize..end as usize)
                    .with_vector(vector_from_column(&vector).unwrap())
                    .build();
                (model, chunk)
            })
            .collect()
    }

    fn with_model(embeddings: &DocumentEmbeddings) -> Vec<(String, ChunkEmbedding)> {
        embeddings
            .chunks
            .iter()
            .map(|chunk| (embeddings.model.clone(), chunk.clone()))
            .collect()
    }

    /// Makes every insert into `document_embeddings` fail.
    async fn fail_embedding_inserts(storage: &SqliteStorage) {
        sqlx::query(
            "CREATE TRIGGER fail_embedding_inserts BEFORE INSERT ON document_embeddings
             BEGIN SELECT RAISE(ABORT, 'embedding insert refused'); END",
        )
        .execute(&storage.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn save_document_saves_its_embeddings_with_it() {
        let storage = setup().await;
        let document = FakeDocument::new().build();
        let embeddings = FakeDocumentEmbeddings::new()
            .with_chunks(vec![
                FakeChunkEmbedding::new().with_range(0..120).build(),
                FakeChunkEmbedding::new().with_range(122..980).build(),
            ])
            .build();

        storage.save_document(&document, &embeddings).await.unwrap();

        assert!(storage.get_document(document.id).await.unwrap().is_some());
        assert_eq!(
            stored_embeddings(&storage, document.id).await,
            with_model(&embeddings)
        );
    }

    #[tokio::test]
    async fn save_document_with_no_chunks_saves_the_document_alone() {
        let storage = setup().await;
        let document = FakeDocument::new().build();
        let embeddings = FakeDocumentEmbeddings::new()
            .with_chunks(Vec::new())
            .build();

        storage.save_document(&document, &embeddings).await.unwrap();

        assert!(storage.get_document(document.id).await.unwrap().is_some());
        assert!(stored_embeddings(&storage, document.id).await.is_empty());
    }

    #[tokio::test]
    async fn save_document_saves_nothing_when_an_embedding_cant_be_saved() {
        let storage = setup().await;
        fail_embedding_inserts(&storage).await;
        let document = FakeDocument::new().build();

        let err = storage
            .save_document(&document, &embeddings())
            .await
            .unwrap_err();

        assert!(err.has_context());
        assert_eq!(storage.get_document(document.id).await.unwrap(), None);
        assert!(stored_embeddings(&storage, document.id).await.is_empty());
    }

    #[tokio::test]
    async fn delete_document_deletes_only_that_document_with_its_embeddings() {
        let storage = setup().await;
        let document = FakeDocument::new().build();
        let other = FakeDocument::new().build();
        let other_embeddings = embeddings();
        storage
            .save_document(&document, &embeddings())
            .await
            .unwrap();
        storage
            .save_document(&other, &other_embeddings)
            .await
            .unwrap();

        storage.delete_document(document.id).await.unwrap();

        assert_eq!(storage.get_document(document.id).await.unwrap(), None);
        assert!(stored_embeddings(&storage, document.id).await.is_empty());
        assert!(storage.get_document(other.id).await.unwrap().is_some());
        assert_eq!(
            stored_embeddings(&storage, other.id).await,
            with_model(&other_embeddings)
        );
    }

    #[tokio::test]
    async fn delete_document_drops_it_from_the_full_text_index() {
        let storage = setup().await;
        let deleted = Uuid::now_v7();
        save(&storage, searchable("quittance kept", 1)).await;
        save(
            &storage,
            searchable("quittance deleted", 2).with_id(deleted),
        )
        .await;

        storage.delete_document(deleted).await.unwrap();

        assert_eq!(
            search_names(
                &storage,
                &["quittance"],
                &SearchFilters::default(),
                SearchOrder::Relevance
            )
            .await,
            vec!["quittance kept"]
        );
    }

    #[tokio::test]
    async fn delete_document_does_nothing_for_a_missing_document() {
        let storage = setup().await;

        storage.delete_document(Uuid::now_v7()).await.unwrap();
    }

    #[tokio::test]
    async fn get_metadata_returns_a_saved_documents_metadata_without_its_content() {
        let storage = setup().await;
        let document = FakeDocument::new().with_content(b"12345".to_vec()).build();
        storage
            .save_document(&document, &embeddings())
            .await
            .unwrap();

        let metadata = storage.get_metadata(document.id).await.unwrap();

        let expected = Metadata {
            size: 5,
            ..document.metadata
        };
        assert_eq!(metadata, Some(expected));
    }

    #[tokio::test]
    async fn get_metadata_returns_none_when_missing() {
        let storage = setup().await;

        assert_eq!(storage.get_metadata(Uuid::now_v7()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn update_metadata_replaces_the_embeddings_when_given_some() {
        let storage = setup().await;
        let document = FakeDocument::new().build();
        storage
            .save_document(&document, &embeddings())
            .await
            .unwrap();
        let metadata = FakeMetadata::new().with_id(document.id).build();
        let replacement = FakeDocumentEmbeddings::new()
            .with_chunks(vec![FakeChunkEmbedding::new().build()])
            .build();

        storage
            .update_metadata(document.id, &metadata, Some(replacement.clone()))
            .await
            .unwrap();

        let saved = storage.get_metadata(document.id).await.unwrap().unwrap();
        assert_eq!(saved.name, metadata.name);
        assert_eq!(
            stored_embeddings(&storage, document.id).await,
            with_model(&replacement)
        );
    }

    #[tokio::test]
    async fn update_metadata_can_replace_the_embeddings_with_none_at_all() {
        let storage = setup().await;
        let document = FakeDocument::new().build();
        storage
            .save_document(&document, &embeddings())
            .await
            .unwrap();
        let empty = FakeDocumentEmbeddings::new()
            .with_chunks(Vec::new())
            .build();

        storage
            .update_metadata(document.id, &document.metadata, Some(empty))
            .await
            .unwrap();

        assert!(stored_embeddings(&storage, document.id).await.is_empty());
    }

    #[tokio::test]
    async fn update_metadata_keeps_the_embeddings_when_given_none() {
        let storage = setup().await;
        let document = FakeDocument::new().build();
        let embeddings = embeddings();
        storage.save_document(&document, &embeddings).await.unwrap();
        let metadata = FakeMetadata::new().with_id(document.id).build();

        storage
            .update_metadata(document.id, &metadata, None)
            .await
            .unwrap();

        assert_eq!(
            stored_embeddings(&storage, document.id).await,
            with_model(&embeddings)
        );
    }

    #[tokio::test]
    async fn update_metadata_changes_nothing_when_the_new_embeddings_cant_be_saved() {
        let storage = setup().await;
        let document = FakeDocument::new().build();
        let embeddings = embeddings();
        storage.save_document(&document, &embeddings).await.unwrap();
        fail_embedding_inserts(&storage).await;
        let metadata = FakeMetadata::new().with_id(document.id).build();

        let err = storage
            .update_metadata(document.id, &metadata, Some(self::embeddings()))
            .await
            .unwrap_err();

        assert!(err.has_context());
        let saved = storage.get_metadata(document.id).await.unwrap().unwrap();
        assert_eq!(saved.name, document.metadata.name);
        assert_eq!(
            stored_embeddings(&storage, document.id).await,
            with_model(&embeddings)
        );
    }

    #[test]
    fn vectors_are_stored_as_little_endian_f32() {
        assert_eq!(
            vector_to_column(&[1.0, -2.5]),
            [0, 0, 0x80, 0x3f, 0, 0, 0x20, 0xc0]
        );

        let vector = FakeChunkEmbedding::new().build().vector;
        assert_eq!(vector_to_column(&vector).len(), 4 * vector.len());
        assert_eq!(
            vector_from_column(&vector_to_column(&vector)).unwrap(),
            vector
        );
        assert!(vector_from_column(&[0, 0, 0x80]).is_err());
    }

    #[tokio::test]
    async fn find_document_by_checksum_returns_the_id_of_a_document_with_that_checksum() {
        let storage = setup().await;
        let document = FakeDocument::new()
            .with_metadata(FakeMetadata::new().with_checksum("cafebabe").build())
            .build();
        storage
            .save_document(&document, &embeddings())
            .await
            .unwrap();

        let found = storage.find_document_by_checksum("cafebabe").await.unwrap();

        assert_eq!(found, Some(document.id()));
    }

    #[tokio::test]
    async fn find_document_by_checksum_returns_none_when_no_document_has_that_checksum() {
        let storage = setup().await;
        let document = FakeDocument::new()
            .with_metadata(FakeMetadata::new().with_checksum("cafebabe").build())
            .build();
        storage
            .save_document(&document, &embeddings())
            .await
            .unwrap();

        let found = storage.find_document_by_checksum("deadbeef").await.unwrap();

        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn get_document_returns_none_when_missing() {
        let storage = setup().await;

        let result = storage.get_document(Uuid::now_v7()).await.unwrap();

        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn get_document_returns_a_previously_saved_document() {
        let storage = setup().await;
        let id = Uuid::now_v7();

        storage
            .save_document(
                &Document {
                    id,
                    content: b"hello".to_vec(),
                    metadata: Metadata {
                        id,
                        name: "report.pdf".to_string(),
                        original_name: "report.pdf".to_string(),
                        content_type: "application/pdf".to_string(),
                        created_at: 1_700_000_000,
                        size: 5,
                        checksum: "deadbeef".to_string(),
                        transcript: "hello".to_string(),
                        r#type: None,
                        folder_id: None,
                        attributes: BTreeMap::new(),
                    },
                },
                &embeddings(),
            )
            .await
            .unwrap();

        let document = storage.get_document(id).await.unwrap().unwrap();

        assert_eq!(
            document,
            Document {
                id,
                content: b"hello".to_vec(),
                metadata: Metadata {
                    id,
                    name: "report.pdf".to_string(),
                    original_name: "report.pdf".to_string(),
                    content_type: "application/pdf".to_string(),
                    created_at: 1_700_000_000,
                    size: 5,
                    checksum: "deadbeef".to_string(),
                    transcript: "hello".to_string(),
                    r#type: None,
                    folder_id: None,
                    attributes: BTreeMap::new(),
                },
            }
        );
    }

    #[tokio::test]
    async fn attributes_roundtrip_through_save_and_update() {
        let storage = setup().await;
        let metadata = FakeMetadata::new()
            .with_attributes([("price".to_string(), Attribute::Float(32.3))])
            .build();
        let document = FakeDocument::new().with_metadata(metadata).build();
        let id = document.id;

        storage
            .save_document(&document, &embeddings())
            .await
            .unwrap();
        let saved = storage.get_document(id).await.unwrap().unwrap();
        assert_eq!(saved.metadata.attributes, document.metadata.attributes);

        let mut updated = saved.metadata.clone();
        updated
            .attributes
            .insert("paid".to_string(), Attribute::Boolean(true));
        storage.update_metadata(id, &updated, None).await.unwrap();

        let reloaded = storage.get_document(id).await.unwrap().unwrap();
        assert_eq!(reloaded.metadata.attributes, updated.attributes);
    }

    #[tokio::test]
    async fn save_document_does_not_affect_other_documents() {
        let storage = setup().await;
        let (id1, id2) = (Uuid::now_v7(), Uuid::now_v7());

        storage
            .save_document(
                &Document {
                    id: id1,
                    content: b"one".to_vec(),
                    metadata: Metadata {
                        id: id1,
                        name: "one.txt".to_string(),
                        original_name: "one.txt".to_string(),
                        content_type: "text/plain".to_string(),
                        created_at: 1_700_000_000,
                        size: 3,
                        checksum: "checksum-one".to_string(),
                        transcript: String::new(),
                        r#type: None,
                        folder_id: None,
                        attributes: BTreeMap::new(),
                    },
                },
                &embeddings(),
            )
            .await
            .unwrap();
        storage
            .save_document(
                &Document {
                    id: id2,
                    content: b"two".to_vec(),
                    metadata: Metadata {
                        id: id2,
                        name: "two.txt".to_string(),
                        original_name: "two.txt".to_string(),
                        content_type: "text/plain".to_string(),
                        created_at: 1_700_000_001,
                        size: 3,
                        checksum: "checksum-two".to_string(),
                        transcript: String::new(),
                        r#type: None,
                        folder_id: None,
                        attributes: BTreeMap::new(),
                    },
                },
                &embeddings(),
            )
            .await
            .unwrap();

        let document = storage.get_document(id1).await.unwrap().unwrap();

        assert_eq!(document.metadata.name, "one.txt");
        assert_eq!(document.content, b"one");
    }

    async fn save_documents(storage: &SqliteStorage, names: &[&str]) {
        for (i, name) in names.iter().enumerate() {
            let id = Uuid::now_v7();
            storage
                .save_document(
                    &Document {
                        id,
                        content: Vec::new(),
                        metadata: Metadata {
                            id,
                            name: name.to_string(),
                            original_name: name.to_string(),
                            content_type: "text/plain".to_string(),
                            created_at: 1_700_000_000 + i as i64,
                            size: 0,
                            checksum: String::new(),
                            transcript: String::new(),
                            r#type: None,
                            folder_id: None,
                            attributes: BTreeMap::new(),
                        },
                    },
                    &embeddings(),
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn list_documents_returns_at_most_limit_oldest_first() {
        let storage = setup().await;
        save_documents(&storage, &["one", "two", "three"]).await;

        let page = storage.list_documents(0, 2).await.unwrap();

        assert_eq!(
            page.into_iter().map(|d| d.name).collect::<Vec<_>>(),
            vec!["one", "two"]
        );
    }

    #[tokio::test]
    async fn list_documents_skips_offset() {
        let storage = setup().await;
        save_documents(&storage, &["one", "two", "three"]).await;

        let page = storage.list_documents(1, 2).await.unwrap();

        assert_eq!(
            page.into_iter().map(|d| d.name).collect::<Vec<_>>(),
            vec!["two", "three"]
        );
    }

    #[tokio::test]
    async fn list_documents_returns_empty_when_offset_past_the_end() {
        let storage = setup().await;
        save_documents(&storage, &["one"]).await;

        let page = storage.list_documents(5, 2).await.unwrap();

        assert_eq!(page, Vec::new());
    }

    #[tokio::test]
    async fn list_and_search_documents_report_the_content_size_without_loading_it() {
        let storage = setup().await;
        let metadata = FakeMetadata::new().build();
        storage
            .save_document(
                &FakeDocument::new()
                    .with_id(metadata.id)
                    .with_content(b"hello".to_vec())
                    .with_metadata(metadata)
                    .build(),
                &embeddings(),
            )
            .await
            .unwrap();

        let listed = storage.list_documents(0, 1).await.unwrap();
        let found = storage
            .search_documents(&[], &SearchFilters::default(), SearchOrder::Oldest, 0, 1)
            .await
            .unwrap();

        assert_eq!(listed[0].size, 5);
        assert_eq!(found[0].size, 5);
    }

    #[tokio::test]
    async fn update_metadata_replaces_metadata_and_leaves_content_untouched() {
        let storage = setup().await;
        let id = Uuid::now_v7();
        storage
            .save_document(
                &Document {
                    id,
                    content: b"hello".to_vec(),
                    metadata: Metadata {
                        id,
                        name: "old.pdf".to_string(),
                        original_name: "old.pdf".to_string(),
                        content_type: "application/pdf".to_string(),
                        created_at: 1_700_000_000,
                        size: 5,
                        checksum: "deadbeef".to_string(),
                        transcript: "hello".to_string(),
                        r#type: None,
                        folder_id: None,
                        attributes: BTreeMap::new(),
                    },
                },
                &embeddings(),
            )
            .await
            .unwrap();

        storage
            .update_metadata(
                id,
                &Metadata {
                    id,
                    name: "new.pdf".to_string(),
                    original_name: "old.pdf".to_string(),
                    content_type: "application/pdf".to_string(),
                    created_at: 1_700_000_000,
                    size: 0,
                    checksum: "deadbeef".to_string(),
                    transcript: "hello".to_string(),
                    r#type: None,
                    folder_id: None,
                    attributes: BTreeMap::new(),
                },
                None,
            )
            .await
            .unwrap();

        let document = storage.get_document(id).await.unwrap().unwrap();
        assert_eq!(document.metadata.name, "new.pdf");
        assert_eq!(document.content, b"hello");
    }

    #[tokio::test]
    async fn update_metadata_does_nothing_when_no_document_matches_the_id() {
        let storage = setup().await;

        let result = storage
            .update_metadata(
                Uuid::now_v7(),
                &Metadata {
                    id: Uuid::now_v7(),
                    name: "new.pdf".to_string(),
                    original_name: "new.pdf".to_string(),
                    content_type: "application/pdf".to_string(),
                    created_at: 1_700_000_000,
                    size: 0,
                    checksum: String::new(),
                    transcript: String::new(),
                    r#type: None,
                    folder_id: None,
                    attributes: BTreeMap::new(),
                },
                None,
            )
            .await;

        assert!(result.is_ok());
    }

    /// Metadata for a search fixture named `name`, created at `created_at`,
    /// with every other searchable field left blank — unlike
    /// [`FakeMetadata::new`]'s random words, which could match a test's
    /// search terms by accident. Tests set just the fields they search on.
    fn searchable(name: &str, created_at: i64) -> FakeMetadata {
        FakeMetadata::new()
            .with_name(name)
            .with_original_name(name)
            .with_content_type("application/pdf")
            .with_created_at(created_at)
            .with_transcript("")
            .with_type(None)
            .with_folder(None)
    }

    async fn save(storage: &SqliteStorage, metadata: FakeMetadata) {
        let metadata = metadata.build();
        storage
            .save_document(
                &FakeDocument::new()
                    .with_id(metadata.id)
                    .with_metadata(metadata)
                    .build(),
                &embeddings(),
            )
            .await
            .unwrap();
    }

    async fn search_names(
        storage: &SqliteStorage,
        terms: &[&str],
        filters: &SearchFilters,
        order: SearchOrder,
    ) -> Vec<String> {
        let terms: Vec<String> = terms.iter().map(|term| term.to_string()).collect();
        storage
            .search_documents(&terms, filters, order, 0, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|metadata| metadata.name)
            .collect()
    }

    #[tokio::test]
    async fn search_documents_without_terms_or_filters_returns_every_document_newest_first() {
        let storage = setup().await;
        save(&storage, searchable("old", 1)).await;
        save(&storage, searchable("new", 3)).await;
        save(&storage, searchable("middle", 2)).await;

        let names = search_names(
            &storage,
            &[],
            &SearchFilters::default(),
            SearchOrder::Relevance,
        )
        .await;

        assert_eq!(names, vec!["new", "middle", "old"]);
    }

    #[tokio::test]
    async fn search_documents_matches_terms_as_case_and_accent_insensitive_prefixes() {
        let storage = setup().await;
        save(&storage, searchable("Échéancier EDF", 1)).await;
        save(&storage, searchable("other", 2)).await;

        let names = search_names(
            &storage,
            &["echea", "edf"],
            &SearchFilters::default(),
            SearchOrder::Relevance,
        )
        .await;

        assert_eq!(names, vec!["Échéancier EDF"]);
    }

    #[tokio::test]
    async fn search_documents_requires_every_term_to_match() {
        let storage = setup().await;
        save(&storage, searchable("facture electricite", 1)).await;
        save(&storage, searchable("facture internet", 2)).await;

        let names = search_names(
            &storage,
            &["facture", "internet"],
            &SearchFilters::default(),
            SearchOrder::Relevance,
        )
        .await;

        assert_eq!(names, vec!["facture internet"]);
    }

    #[tokio::test]
    async fn search_documents_matches_every_searchable_field() {
        let storage = setup().await;
        save(
            &storage,
            searchable("by-original", 1).with_original_name("scan-originalname.pdf"),
        )
        .await;
        save(
            &storage,
            searchable("by-transcript", 2).with_transcript("Montant total: 42 euros"),
        )
        .await;
        save(
            &storage,
            searchable("by-type", 3).with_type(DocumentType::Payslip),
        )
        .await;

        for (term, expected) in [
            ("originalname", "by-original"),
            ("euros", "by-transcript"),
            ("payslip", "by-type"),
        ] {
            let names = search_names(
                &storage,
                &[term],
                &SearchFilters::default(),
                SearchOrder::Relevance,
            )
            .await;
            assert_eq!(names, vec![expected], "searching for {term:?}");
        }
    }

    #[tokio::test]
    async fn search_documents_ranks_a_name_match_above_a_transcript_match() {
        let storage = setup().await;
        // Newer, so it would come first if relevance weren't ranked.
        save(
            &storage,
            searchable("in-transcript", 2).with_transcript("contains the word quittance"),
        )
        .await;
        save(&storage, searchable("quittance", 1)).await;

        let names = search_names(
            &storage,
            &["quittance"],
            &SearchFilters::default(),
            SearchOrder::Relevance,
        )
        .await;

        assert_eq!(names, vec!["quittance", "in-transcript"]);
    }

    #[tokio::test]
    async fn search_documents_never_parses_terms_as_fts_query_syntax() {
        let storage = setup().await;
        save(&storage, searchable("plain", 1)).await;

        let names = search_names(
            &storage,
            &["\"", "OR", "name:plain", "NEAR("],
            &SearchFilters::default(),
            SearchOrder::Relevance,
        )
        .await;

        assert_eq!(names, Vec::<String>::new());
    }

    #[tokio::test]
    async fn search_documents_keeps_the_documents_within_a_folder_and_its_subfolders() {
        let storage = setup().await;
        save(&storage, searchable("health", 1).with_folder(HEALTH)).await;
        save(&storage, searchable("banking", 2).with_folder(BANKING)).await;
        save(&storage, searchable("money", 3).with_folder(MONEY)).await;
        save(&storage, searchable("unfiled", 4)).await;

        let names = search_names(
            &storage,
            &[],
            &SearchFilters {
                folder: Some(FolderFilter::Within(HEALTH)),
                ..Default::default()
            },
            SearchOrder::Oldest,
        )
        .await;

        assert_eq!(names, vec!["health", "banking"]);
    }

    #[tokio::test]
    async fn search_documents_keeps_unfiled_documents_including_those_whose_folder_is_gone() {
        let storage = setup().await;
        save(&storage, searchable("filed", 1).with_folder(MONEY)).await;
        save(&storage, searchable("unfiled", 2)).await;
        save(
            &storage,
            searchable("folder-gone", 3).with_folder(Uuid::now_v7()),
        )
        .await;

        let names = search_names(
            &storage,
            &[],
            &SearchFilters {
                folder: Some(FolderFilter::Unfiled),
                ..Default::default()
            },
            SearchOrder::Oldest,
        )
        .await;

        assert_eq!(names, vec!["unfiled", "folder-gone"]);
    }

    #[tokio::test]
    async fn search_documents_keeps_only_documents_matching_every_filter() {
        let storage = setup().await;
        save(
            &storage,
            searchable("match", 10)
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;
        // Each differs from "match" in exactly one filtered field.
        save(
            &storage,
            searchable("wrong-folder", 10)
                .with_folder(MONEY)
                .with_type(DocumentType::Receipt),
        )
        .await;
        save(
            &storage,
            searchable("parent-folder", 10)
                .with_folder(HEALTH)
                .with_type(DocumentType::Receipt),
        )
        .await;
        save(
            &storage,
            searchable("wrong-type", 10)
                .with_folder(BANKING)
                .with_type(DocumentType::Statement),
        )
        .await;
        save(
            &storage,
            searchable("wrong-content-type", 10)
                .with_content_type("text/plain")
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;
        save(
            &storage,
            searchable("too-old", 1)
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;
        save(
            &storage,
            searchable("too-new", 100)
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;

        let names = search_names(
            &storage,
            &[],
            &SearchFilters {
                folder: Some(FolderFilter::Within(BANKING)),
                types: vec![DocumentType::Receipt],
                content_types: vec!["application/pdf".to_string()],
                created_from: Some(10),
                created_until: Some(10),
            },
            SearchOrder::Relevance,
        )
        .await;

        assert_eq!(names, vec!["match"]);
    }

    #[tokio::test]
    async fn search_documents_combines_terms_with_filters() {
        let storage = setup().await;
        save(&storage, searchable("facture bank", 1).with_folder(MONEY)).await;
        save(
            &storage,
            searchable("facture telecom", 2).with_folder(TELECOM),
        )
        .await;
        save(&storage, searchable("releve bank", 3).with_folder(MONEY)).await;

        let names = search_names(
            &storage,
            &["facture"],
            &SearchFilters {
                folder: Some(FolderFilter::Within(MONEY)),
                ..Default::default()
            },
            SearchOrder::Relevance,
        )
        .await;

        assert_eq!(names, vec!["facture bank"]);
    }

    #[tokio::test]
    async fn search_documents_pages_in_the_requested_order() {
        let storage = setup().await;
        for (name, created_at) in [("one", 1), ("two", 2), ("three", 3)] {
            save(&storage, searchable(name, created_at)).await;
        }

        let page = storage
            .search_documents(&[], &SearchFilters::default(), SearchOrder::Oldest, 1, 1)
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].name, "two");

        let names = search_names(
            &storage,
            &[],
            &SearchFilters::default(),
            SearchOrder::Newest,
        )
        .await;
        assert_eq!(names, vec!["three", "two", "one"]);
    }

    #[tokio::test]
    async fn search_documents_reflects_metadata_updates() {
        let storage = setup().await;
        // A distinct original name, which the update below keeps, so only
        // the updated `name` can match.
        let metadata = searchable("before", 1)
            .with_original_name("scan.pdf")
            .build();
        let id = metadata.id;
        storage
            .save_document(
                &FakeDocument::new()
                    .with_id(id)
                    .with_metadata(metadata.clone())
                    .build(),
                &embeddings(),
            )
            .await
            .unwrap();

        storage
            .update_metadata(
                id,
                &Metadata {
                    name: "after".to_string(),
                    ..metadata
                },
                None,
            )
            .await
            .unwrap();

        let filters = SearchFilters::default();
        assert_eq!(
            search_names(&storage, &["before"], &filters, SearchOrder::Relevance).await,
            Vec::<String>::new()
        );
        assert_eq!(
            search_names(&storage, &["after"], &filters, SearchOrder::Relevance).await,
            vec!["after"]
        );
    }

    #[tokio::test]
    async fn search_index_migration_indexes_documents_saved_before_it() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut migrator = sqlx::migrate!();
        let full = migrator.migrations.clone();
        let index_migration = full
            .iter()
            .position(|migration| migration.description.contains("documents fts"))
            .unwrap();
        migrator.migrations = full[..index_migration].to_vec().into();
        migrator.run(&pool).await.unwrap();
        // Inserted by hand rather than through `save_document`, which writes
        // columns later migrations add.
        sqlx::query(
            "INSERT INTO documents (id, name, original_name, content_type, content, created_at, subjects)
             VALUES (?1, 'preexisting', 'preexisting', 'application/pdf', x'', 1, '[]')",
        )
        .bind(Uuid::now_v7().to_string())
        .execute(&pool)
        .await
        .unwrap();
        let storage = SqliteStorage::new(pool.clone());

        migrator.migrations = full;
        migrator.run(&pool).await.unwrap();

        assert_eq!(
            search_names(
                &storage,
                &["preexisting"],
                &SearchFilters::default(),
                SearchOrder::Relevance
            )
            .await,
            vec!["preexisting"]
        );
    }

    async fn facet(
        storage: &SqliteStorage,
        terms: &[&str],
        filters: &SearchFilters,
        field: FacetField,
    ) -> Vec<(String, u64)> {
        let terms: Vec<String> = terms.iter().map(|term| term.to_string()).collect();
        storage
            .count_documents_by(&terms, filters, field)
            .await
            .unwrap()
            .into_iter()
            .map(|facet| (facet.value, facet.count))
            .collect()
    }

    fn counts(expected: &[(&str, u64)]) -> Vec<(String, u64)> {
        expected
            .iter()
            .map(|(value, count)| (value.to_string(), *count))
            .collect()
    }

    #[tokio::test]
    async fn count_documents_by_counts_each_value_most_common_first_skipping_unset_ones_but_unfiled()
     {
        let storage = setup().await;
        save(
            &storage,
            searchable("a", 1)
                .with_folder(HOUSING)
                .with_type(DocumentType::Invoice),
        )
        .await;
        save(
            &storage,
            searchable("b", 2)
                .with_folder(MONEY)
                .with_content_type("image/png"),
        )
        .await;
        save(&storage, searchable("c", 3).with_folder(MONEY)).await;
        save(&storage, searchable("unclassified", 4)).await;
        let filters = SearchFilters::default();

        assert_eq!(
            facet(&storage, &[], &filters, FacetField::Folder).await,
            counts(&[
                (&MONEY.to_string(), 2),
                (&HOUSING.to_string(), 1),
                (UNFILED, 1)
            ])
        );
        assert_eq!(
            facet(&storage, &[], &filters, FacetField::Type).await,
            counts(&[("invoice", 1)])
        );
        assert_eq!(
            facet(&storage, &[], &filters, FacetField::ContentType).await,
            counts(&[("application/pdf", 3), ("image/png", 1)])
        );
    }

    #[tokio::test]
    async fn count_documents_by_only_counts_documents_matching_the_terms_and_filters() {
        let storage = setup().await;
        save(
            &storage,
            searchable("facture edf", 1)
                .with_folder(HOUSING)
                .with_type(DocumentType::Invoice),
        )
        .await;
        save(
            &storage,
            searchable("facture orange", 2)
                .with_folder(TELECOM)
                .with_type(DocumentType::Contract),
        )
        .await;
        save(
            &storage,
            searchable("releve", 3)
                .with_folder(MONEY)
                .with_type(DocumentType::Invoice),
        )
        .await;

        assert_eq!(
            facet(
                &storage,
                &["facture"],
                &SearchFilters {
                    types: vec![DocumentType::Invoice],
                    ..Default::default()
                },
                FacetField::Folder,
            )
            .await,
            counts(&[(&HOUSING.to_string(), 1)])
        );
    }

    #[tokio::test]
    async fn created_at_range_spans_the_matching_documents_only() {
        let storage = setup().await;
        save(&storage, searchable("old facture", 10)).await;
        save(&storage, searchable("new facture", 30)).await;
        save(&storage, searchable("other", 50)).await;

        let terms = vec!["facture".to_string()];
        assert_eq!(
            storage
                .created_at_range(&terms, &SearchFilters::default())
                .await
                .unwrap(),
            Some((10, 30))
        );
        assert_eq!(
            storage
                .created_at_range(&["nothing".to_string()], &SearchFilters::default())
                .await
                .unwrap(),
            None
        );
    }

    const MODEL: &str = "e5.onnx";

    /// Saves `metadata` with one chunk per vector of `vectors`, embedded by
    /// `model`, returning its id.
    async fn save_embedded(
        storage: &SqliteStorage,
        metadata: FakeMetadata,
        model: &str,
        vectors: &[&[f32]],
    ) -> Uuid {
        let metadata = metadata.build();
        let id = metadata.id;
        let embeddings = FakeDocumentEmbeddings::new()
            .with_model(model)
            .with_chunks(
                vectors
                    .iter()
                    .map(|vector| {
                        FakeChunkEmbedding::new()
                            .with_vector(vector.to_vec())
                            .build()
                    })
                    .collect(),
            )
            .build();
        storage
            .save_document(
                &FakeDocument::new()
                    .with_id(id)
                    .with_metadata(metadata)
                    .build(),
                &embeddings,
            )
            .await
            .unwrap();
        id
    }

    async fn similar(
        storage: &SqliteStorage,
        query: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Vec<(Uuid, f32)> {
        storage
            .rank_by_similarity(query, MODEL, filters, limit)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn rank_by_similarity_scores_a_document_by_its_best_chunk() {
        let storage = setup().await;
        let one_good_chunk = save_embedded(
            &storage,
            searchable("one-good-chunk", 1),
            MODEL,
            &[&[0.0, 1.0], &[1.0, 0.0], &[-1.0, 0.0]],
        )
        .await;
        // Unnormalized: its cosine is 0.6 whatever its length.
        let fair = save_embedded(&storage, searchable("fair", 2), MODEL, &[&[6.0, 8.0]]).await;

        let ranked = similar(&storage, &[2.0, 0.0], &SearchFilters::default(), 10).await;

        assert_eq!(
            ranked.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [one_good_chunk, fair]
        );
        assert!((ranked[0].1 - 1.0).abs() < 1e-6, "{ranked:?}");
        assert!((ranked[1].1 - 0.6).abs() < 1e-6, "{ranked:?}");
    }

    #[tokio::test]
    async fn rank_by_similarity_keeps_only_documents_matching_every_filter() {
        let storage = setup().await;
        let embedded = |metadata| save_embedded(&storage, metadata, MODEL, &[&[1.0, 0.0]]);
        let matching = embedded(
            searchable("match", 10)
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;
        // Each differs from "match" in exactly one filtered field.
        embedded(
            searchable("wrong-folder", 10)
                .with_folder(MONEY)
                .with_type(DocumentType::Receipt),
        )
        .await;
        embedded(
            searchable("parent-folder", 10)
                .with_folder(HEALTH)
                .with_type(DocumentType::Receipt),
        )
        .await;
        embedded(
            searchable("wrong-type", 10)
                .with_folder(BANKING)
                .with_type(DocumentType::Statement),
        )
        .await;
        embedded(
            searchable("wrong-content-type", 10)
                .with_content_type("text/plain")
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;
        embedded(
            searchable("too-old", 1)
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;
        embedded(
            searchable("too-new", 100)
                .with_folder(BANKING)
                .with_type(DocumentType::Receipt),
        )
        .await;
        let filters = SearchFilters {
            folder: Some(FolderFilter::Within(BANKING)),
            types: vec![DocumentType::Receipt],
            content_types: vec!["application/pdf".to_string()],
            created_from: Some(10),
            created_until: Some(10),
        };

        let ranked = similar(&storage, &[1.0, 0.0], &filters, 10).await;

        assert_eq!(
            ranked.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [matching]
        );
        assert_eq!(
            similar(&storage, &[1.0, 0.0], &SearchFilters::default(), 10)
                .await
                .len(),
            7
        );
    }

    #[tokio::test]
    async fn rank_by_similarity_skips_documents_without_embeddings_from_the_model() {
        let storage = setup().await;
        let embedded = save_embedded(&storage, searchable("e5", 1), MODEL, &[&[1.0, 0.0]]).await;
        save_embedded(
            &storage,
            searchable("other-model", 2),
            "other.onnx",
            &[&[1.0, 0.0]],
        )
        .await;
        save_embedded(&storage, searchable("no-chunks", 3), MODEL, &[]).await;
        save_embedded(
            &storage,
            searchable("zero-vector", 4),
            MODEL,
            &[&[0.0, 0.0]],
        )
        .await;

        let ranked = similar(&storage, &[1.0, 0.0], &SearchFilters::default(), 10).await;

        assert_eq!(
            ranked.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [embedded]
        );
        assert!(
            similar(&storage, &[0.0, 0.0], &SearchFilters::default(), 10)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rank_by_similarity_returns_at_most_limit_most_similar_first_ties_by_id() {
        let storage = setup().await;
        let mut tied = Vec::new();
        for name in ["a", "b", "c"] {
            tied.push(save_embedded(&storage, searchable(name, 1), MODEL, &[&[1.0, 0.0]]).await);
        }
        tied.sort_unstable();
        save_embedded(&storage, searchable("worse", 1), MODEL, &[&[0.0, 1.0]]).await;

        let ranked = similar(&storage, &[1.0, 0.0], &SearchFilters::default(), 2).await;

        assert_eq!(
            ranked.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            tied[..2]
        );
    }

    #[tokio::test]
    async fn rank_by_similarity_fails_on_an_embedding_of_another_length() {
        let storage = setup().await;
        save_embedded(
            &storage,
            searchable("longer", 1),
            MODEL,
            &[&[1.0, 0.0, 0.0]],
        )
        .await;

        let err = storage
            .rank_by_similarity(&[1.0, 0.0], MODEL, &SearchFilters::default(), 10)
            .await
            .unwrap_err();

        assert!(err.has_context());
    }

    /// [`Storage::similarity_by_folder`]'s entries, sorted.
    async fn by_folder(
        storage: &SqliteStorage,
        query: &[f32],
        folders: &[Uuid],
    ) -> Vec<(Uuid, f32)> {
        let mut found = storage
            .similarity_by_folder(query, MODEL, folders)
            .await
            .unwrap();
        found.sort_by(|(a_id, a), (b_id, b)| a_id.cmp(b_id).then(a.total_cmp(b)));
        found
    }

    #[tokio::test]
    async fn similarity_by_folder_scores_each_document_by_its_best_chunk_with_its_folder() {
        let storage = setup().await;
        save_embedded(
            &storage,
            searchable("good-chunk", 1).with_folder(HOUSING),
            MODEL,
            &[&[0.0, 1.0], &[1.0, 0.0]],
        )
        .await;
        // Unnormalized: its cosine is 0.6 whatever its length.
        save_embedded(
            &storage,
            searchable("fair", 2).with_folder(HOUSING),
            MODEL,
            &[&[6.0, 8.0]],
        )
        .await;
        save_embedded(
            &storage,
            searchable("opposite", 3).with_folder(MONEY),
            MODEL,
            &[&[-1.0, 0.0]],
        )
        .await;

        let found = by_folder(&storage, &[2.0, 0.0], &[HOUSING, MONEY]).await;

        assert_eq!(found.len(), 3, "{found:?}");
        let mut housing: Vec<f32> = found
            .iter()
            .filter(|(folder, _)| *folder == HOUSING)
            .map(|(_, similarity)| *similarity)
            .collect();
        housing.sort_by(f32::total_cmp);
        assert!((housing[0] - 0.6).abs() < 1e-6, "{found:?}");
        assert!((housing[1] - 1.0).abs() < 1e-6, "{found:?}");
        assert!(found.contains(&(MONEY, -1.0)), "{found:?}");
    }

    #[tokio::test]
    async fn similarity_by_folder_keeps_only_documents_directly_in_the_folders_given() {
        let storage = setup().await;
        let embedded = |metadata| save_embedded(&storage, metadata, MODEL, &[&[1.0, 0.0]]);
        embedded(searchable("in", 1).with_folder(HEALTH)).await;
        // At the root (À classer), in a subfolder or another folder, or
        // in a folder gone.
        embedded(searchable("root", 2)).await;
        embedded(searchable("subfolder", 3).with_folder(BANKING)).await;
        embedded(searchable("other", 4).with_folder(MONEY)).await;
        embedded(searchable("gone", 5).with_folder(Uuid::now_v7())).await;

        let found = by_folder(&storage, &[1.0, 0.0], &[HEALTH]).await;

        assert_eq!(found, [(HEALTH, 1.0)]);
        assert!(by_folder(&storage, &[1.0, 0.0], &[]).await.is_empty());
    }

    #[tokio::test]
    async fn similarity_by_folder_skips_documents_without_embeddings_from_the_model() {
        let storage = setup().await;
        let in_housing = |name: &str| searchable(name, 1).with_folder(HOUSING);
        save_embedded(&storage, in_housing("e5"), MODEL, &[&[1.0, 0.0]]).await;
        save_embedded(&storage, in_housing("other"), "other.onnx", &[&[1.0, 0.0]]).await;
        save_embedded(&storage, in_housing("no-chunks"), MODEL, &[]).await;
        save_embedded(&storage, in_housing("zero"), MODEL, &[&[0.0, 0.0]]).await;

        let found = by_folder(&storage, &[1.0, 0.0], &[HOUSING]).await;

        assert_eq!(found, [(HOUSING, 1.0)]);
    }

    #[tokio::test]
    async fn similarity_by_folder_fails_on_an_embedding_of_another_length() {
        let storage = setup().await;
        save_embedded(
            &storage,
            searchable("doc", 1).with_folder(HOUSING),
            MODEL,
            &[&[1.0, 0.0, 0.0]],
        )
        .await;

        let err = storage
            .similarity_by_folder(&[1.0, 0.0], MODEL, &[HOUSING])
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::ModelLoadFailed);
    }

    #[tokio::test]
    async fn get_embedding_vectors_returns_a_documents_vectors_from_the_model_in_chunk_order() {
        let storage = setup().await;
        let id = save_embedded(
            &storage,
            searchable("doc", 1),
            MODEL,
            &[&[1.0, 0.0], &[0.0, 1.0], &[0.5, 0.5]],
        )
        .await;
        let other_model = save_embedded(
            &storage,
            searchable("other", 2),
            "other.onnx",
            &[&[1.0, 0.0]],
        )
        .await;

        assert_eq!(
            storage.get_embedding_vectors(id, MODEL).await.unwrap(),
            [vec![1.0, 0.0], vec![0.0, 1.0], vec![0.5, 0.5]]
        );
        assert!(
            storage
                .get_embedding_vectors(other_model, MODEL)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            storage
                .get_embedding_vectors(Uuid::new_v4(), MODEL)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rank_by_terms_ranks_ids_as_search_documents_does_by_relevance() {
        let storage = setup().await;
        // Newer, so it would come first if relevance weren't ranked.
        save(
            &storage,
            searchable("in-transcript", 2).with_transcript("contains the word quittance"),
        )
        .await;
        save(&storage, searchable("quittance", 1)).await;
        save(
            &storage,
            searchable("quittance-health", 1).with_folder(HEALTH),
        )
        .await;
        save(&storage, searchable("unrelated", 3)).await;
        let terms = ["quittance".to_string()];
        let filters = SearchFilters::default();
        let expected = storage
            .search_documents(&terms, &filters, SearchOrder::Relevance, 0, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|metadata| metadata.id)
            .collect::<Vec<_>>();

        let ranked = storage.rank_by_terms(&terms, &filters, 10).await.unwrap();
        let first = storage.rank_by_terms(&terms, &filters, 1).await.unwrap();
        let health = storage
            .rank_by_terms(
                &terms,
                &SearchFilters {
                    folder: Some(FolderFilter::Within(HEALTH)),
                    ..Default::default()
                },
                10,
            )
            .await
            .unwrap();

        assert_eq!(expected.len(), 3);
        assert_eq!(ranked, expected);
        assert_eq!(first, expected[..1]);
        assert_eq!(health.len(), 1);
        assert_eq!(
            storage.get_metadata(health[0]).await.unwrap().unwrap().name,
            "quittance-health"
        );
    }

    #[tokio::test]
    async fn move_folder_documents_moves_only_the_documents_directly_in_those_folders() {
        let storage = setup().await;
        save(&storage, searchable("health", 1).with_folder(HEALTH)).await;
        save(&storage, searchable("banking", 2).with_folder(BANKING)).await;
        save(&storage, searchable("money", 3).with_folder(MONEY)).await;

        storage
            .move_folder_documents(&[HEALTH, MONEY], Some(HOUSING))
            .await
            .unwrap();

        let folders: Vec<(String, Option<Uuid>)> = storage
            .list_documents(0, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|metadata| (metadata.name, metadata.folder_id))
            .collect();
        assert_eq!(
            folders,
            [
                ("health".to_string(), Some(HOUSING)),
                ("banking".to_string(), Some(BANKING)),
                ("money".to_string(), Some(HOUSING)),
            ]
        );
    }

    #[tokio::test]
    async fn move_folder_documents_can_move_them_to_the_root() {
        let storage = setup().await;
        save(&storage, searchable("health", 1).with_folder(HEALTH)).await;

        storage
            .move_folder_documents(&[HEALTH], None)
            .await
            .unwrap();

        assert_eq!(
            storage.list_documents(0, 10).await.unwrap()[0].folder_id,
            None
        );
    }

    #[tokio::test]
    async fn delete_folder_documents_deletes_only_the_documents_in_those_folders() {
        let storage = setup().await;
        save(&storage, searchable("health", 1).with_folder(HEALTH)).await;
        save(&storage, searchable("banking", 2).with_folder(BANKING)).await;
        save(&storage, searchable("money", 3).with_folder(MONEY)).await;
        save(&storage, searchable("unfiled", 4)).await;

        storage
            .delete_folder_documents(&[HEALTH, BANKING])
            .await
            .unwrap();

        let names: Vec<String> = storage
            .list_documents(0, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|metadata| metadata.name)
            .collect();
        assert_eq!(names, ["money", "unfiled"]);
        assert!(
            search_names(
                &storage,
                &["health"],
                &SearchFilters::default(),
                SearchOrder::Relevance
            )
            .await
            .is_empty()
        );
    }

    #[tokio::test]
    async fn get_metadatas_returns_the_listed_documents_skipping_unknown_ids() {
        let storage = setup().await;
        let listed = FakeMetadata::new().build();
        let id = listed.id;
        storage
            .save_document(
                &FakeDocument::new()
                    .with_id(id)
                    .with_content(vec![1, 2, 3])
                    .with_metadata(listed.clone())
                    .build(),
                &embeddings(),
            )
            .await
            .unwrap();
        save(&storage, searchable("not-listed", 1)).await;

        let found = storage.get_metadatas(&[Uuid::new_v4(), id]).await.unwrap();

        assert_eq!(found, [Metadata { size: 3, ..listed }]);
        assert!(storage.get_metadatas(&[]).await.unwrap().is_empty());
    }

    async fn save_ids(storage: &SqliteStorage, metadata: Vec<FakeMetadata>) -> Vec<Uuid> {
        let mut ids = Vec::new();
        for metadata in metadata {
            ids.push(save_embedded(storage, metadata, MODEL, &[]).await);
        }
        ids
    }

    #[tokio::test]
    async fn count_documents_among_counts_only_the_listed_documents() {
        let storage = setup().await;
        let ids = save_ids(
            &storage,
            vec![
                searchable("a", 1)
                    .with_folder(HOUSING)
                    .with_type(DocumentType::Invoice),
                searchable("b", 2)
                    .with_folder(MONEY)
                    .with_content_type("image/png"),
                searchable("c", 3).with_folder(MONEY),
                searchable("unclassified", 4),
                searchable("not-listed", 5).with_folder(HEALTH),
            ],
        )
        .await;
        let listed = &ids[..4];
        let among = async |field| {
            storage
                .count_documents_among(listed, field)
                .await
                .unwrap()
                .into_iter()
                .map(|facet| (facet.value, facet.count))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            among(FacetField::Folder).await,
            counts(&[
                (&MONEY.to_string(), 2),
                (&HOUSING.to_string(), 1),
                (UNFILED, 1)
            ])
        );
        assert_eq!(among(FacetField::Type).await, counts(&[("invoice", 1)]));
        assert_eq!(
            among(FacetField::ContentType).await,
            counts(&[("application/pdf", 3), ("image/png", 1)])
        );
        assert!(
            storage
                .count_documents_among(&[], FacetField::Folder)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn created_at_range_among_spans_the_listed_documents_only() {
        let storage = setup().await;
        let ids = save_ids(
            &storage,
            vec![
                searchable("oldest", 1),
                searchable("a", 5),
                searchable("b", 7),
                searchable("newest", 9),
            ],
        )
        .await;

        assert_eq!(
            storage.created_at_range_among(&ids[1..3]).await.unwrap(),
            Some((5, 7))
        );
        assert_eq!(storage.created_at_range_among(&[]).await.unwrap(), None);
    }
}
