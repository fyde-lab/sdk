use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::storage::{FacetField, Storage};
use super::{
    Document, DocumentType, FacetCount, Metadata, Purpose, SearchFilters, SearchOrder,
    SourceCategory, SourceSubCategory,
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
    async fn save_document(&self, document: &Document) -> Result<()> {
        sqlx::query(
            "INSERT INTO documents (id, name, original_name, content_type, content, checksum, created_at, transcript, type, source_category, source_sub_category, subjects, purpose)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
        .bind(source_category_to_column(document.metadata.source_category))
        .bind(source_sub_category_to_column(
            document.metadata.source_sub_category,
        ))
        .bind(subjects_to_column(&document.metadata.subjects))
        .bind(purpose_to_column(document.metadata.purpose))
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to save document {} to local database", document.id))?;

        Ok(())
    }

    async fn get_document(&self, id: Uuid) -> Result<Option<Document>> {
        let row = sqlx::query(
            "SELECT name, original_name, content_type, content, checksum, created_at, transcript, type, source_category, source_sub_category, subjects, purpose FROM documents WHERE id = ?1",
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
                source_category: source_category_from_column(row.get("source_category"))?,
                source_sub_category: source_sub_category_from_column(
                    row.get("source_sub_category"),
                )?,
                subjects: subjects_from_column(row.get("subjects"))?,
                purpose: purpose_from_column(row.get("purpose"))?,
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

    async fn list_documents(&self, offset: i64, limit: i64) -> Result<Vec<Document>> {
        let rows = sqlx::query(
            "SELECT id, name, original_name, content_type, content, checksum, created_at, transcript, type, source_category, source_sub_category, subjects, purpose FROM documents
             ORDER BY created_at ASC, id ASC
             LIMIT ?1 OFFSET ?2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("failed to list documents at offset {offset}"))?;

        rows.iter().map(document_from_row).collect()
    }

    async fn search_documents(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        order: SearchOrder,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Document>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT d.id, d.name, d.original_name, d.content_type, d.content, d.checksum, d.created_at, d.transcript, d.type, d.source_category, d.source_sub_category, d.subjects, d.purpose",
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

        rows.iter().map(document_from_row).collect()
    }

    async fn count_documents_by(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        field: FacetField,
    ) -> Result<Vec<FacetCount<String>>> {
        let column = match field {
            FacetField::SourceCategory => "d.source_category",
            FacetField::SourceSubCategory => "d.source_sub_category",
            FacetField::Purpose => "d.purpose",
            FacetField::Type => "d.type",
            FacetField::ContentType => "d.content_type",
            FacetField::Subject => "d.subjects",
        };

        // `''` and `NULL` both mean "unset" here: see the `*_to_column`
        // helpers above.
        let mut query = QueryBuilder::<Sqlite>::new("");
        if field == FacetField::Subject {
            query.push(format_args!(
                "SELECT j.value AS value, COUNT(DISTINCT m.id) AS count FROM (SELECT d.id AS id, {column} AS subjects"
            ));
            push_matching(&mut query, terms, filters);
            query.push(") m, json_each(m.subjects) j WHERE j.value != '' GROUP BY j.value");
        } else {
            query.push(format_args!(
                "SELECT value, COUNT(*) AS count FROM (SELECT {column} AS value"
            ));
            push_matching(&mut query, terms, filters);
            query.push(") WHERE value IS NOT NULL AND value != '' GROUP BY value");
        }
        query.push(" ORDER BY count DESC, value ASC");

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to count documents by {field:?}"))?;

        Ok(rows
            .iter()
            .map(|row| FacetCount {
                value: row.get("value"),
                count: row.get::<i64, _>("count") as u64,
            })
            .collect())
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

    async fn update_metadata(&self, id: Uuid, metadata: &Metadata) -> Result<()> {
        sqlx::query(
            "UPDATE documents
             SET name = ?1, original_name = ?2, content_type = ?3, checksum = ?4, created_at = ?5, transcript = ?6, type = ?7, source_category = ?8, source_sub_category = ?9, subjects = ?10, purpose = ?11
             WHERE id = ?12",
        )
        .bind(&metadata.name)
        .bind(&metadata.original_name)
        .bind(&metadata.content_type)
        .bind(&metadata.checksum)
        .bind(metadata.created_at)
        .bind(&metadata.transcript)
        .bind(type_to_column(metadata.r#type))
        .bind(source_category_to_column(metadata.source_category))
        .bind(source_sub_category_to_column(
            metadata.source_sub_category,
        ))
        .bind(subjects_to_column(&metadata.subjects))
        .bind(purpose_to_column(metadata.purpose))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to update document {id} metadata in local database"))?;

        Ok(())
    }
}

/// The `source_category` column has no NULL state (`NOT NULL DEFAULT ''`),
/// so an unclassified document is stored as an empty string.
fn source_category_to_column(source_category: Option<SourceCategory>) -> String {
    source_category.map_or_else(String::new, |category| category.as_str().to_string())
}

fn source_category_from_column(column: String) -> Result<Option<SourceCategory>> {
    if column.is_empty() {
        return Ok(None);
    }

    column
        .parse()
        .map(Some)
        .with_context(|| format!("invalid source category in local database: {column}"))
}

fn source_sub_category_to_column(source_sub_category: Option<SourceSubCategory>) -> Option<String> {
    source_sub_category.map(|category| category.as_str().to_string())
}

fn source_sub_category_from_column(column: Option<String>) -> Result<Option<SourceSubCategory>> {
    column
        .map(|column| {
            column
                .parse()
                .with_context(|| format!("invalid source sub-category in local database: {column}"))
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

/// The `subjects` column stores a JSON-encoded array, since SQLite has no
/// native array type.
fn subjects_to_column(subjects: &[String]) -> String {
    serde_json::to_string(subjects).expect("serializing a Vec<String> to JSON cannot fail")
}

fn subjects_from_column(column: String) -> Result<Vec<String>> {
    serde_json::from_str(&column)
        .with_context(|| format!("invalid subjects json in local database: {column}"))
}

/// The `purpose` column has no NULL state (`NOT NULL DEFAULT ''`), so an
/// unclassified document is stored as an empty string.
fn purpose_to_column(purpose: Option<Purpose>) -> String {
    purpose.map_or_else(String::new, |purpose| purpose.as_str().to_string())
}

fn purpose_from_column(column: String) -> Result<Option<Purpose>> {
    if column.is_empty() {
        return Ok(None);
    }

    column
        .parse()
        .map(Some)
        .with_context(|| format!("invalid purpose in local database: {column}"))
}

/// Maps a row selecting every `documents` column (`id` included) back to a
/// [`Document`].
fn document_from_row(row: &SqliteRow) -> Result<Document> {
    let id: String = row.get("id");
    let content: Vec<u8> = row.get("content");
    let id = Uuid::parse_str(&id)
        .with_context(|| format!("invalid document id in local database: {id}"))?;

    Ok(Document {
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
            source_category: source_category_from_column(row.get("source_category"))?,
            source_sub_category: source_sub_category_from_column(row.get("source_sub_category"))?,
            subjects: subjects_from_column(row.get("subjects"))?,
            purpose: purpose_from_column(row.get("purpose"))?,
        },
        content,
    })
}

/// Orders `documents_fts` matches best first (`bm25` scores lower for a
/// better match), weighting each column in its declaration order in the
/// `0014_create_documents_fts` migration: a hit in the name counts the
/// most, one in the (long, noisy) transcript the least.
const FTS_RANK: &str = "bm25(documents_fts,
    10.0, -- name
    5.0,  -- original_name
    1.0,  -- transcript
    4.0,  -- type
    4.0,  -- source_category
    4.0,  -- source_sub_category
    6.0,  -- subjects
    4.0   -- purpose
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

    push_any_of(
        query,
        "d.source_category",
        filters.source_categories.iter().map(SourceCategory::as_str),
    );
    push_any_of(
        query,
        "d.source_sub_category",
        filters
            .source_sub_categories
            .iter()
            .map(SourceSubCategory::as_str),
    );
    push_any_of(
        query,
        "d.purpose",
        filters.purposes.iter().map(Purpose::as_str),
    );
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
    if !filters.subjects.is_empty() {
        query.push(" AND EXISTS (SELECT 1 FROM json_each(d.subjects) WHERE json_each.value IN (");
        let mut subjects = query.separated(", ");
        for subject in &filters.subjects {
            subjects.push_bind(subject.clone());
        }
        query.push("))");
    }
    if let Some(created_from) = filters.created_from {
        query.push(" AND d.created_at >= ").push_bind(created_from);
    }
    if let Some(created_until) = filters.created_until {
        query.push(" AND d.created_at <= ").push_bind(created_until);
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::super::{FakeDocument, FakeMetadata};
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

        SqliteStorage::new(pool)
    }

    #[tokio::test]
    async fn find_document_by_checksum_returns_the_id_of_a_document_with_that_checksum() {
        let storage = setup().await;
        let document = FakeDocument::new()
            .with_metadata(FakeMetadata::new().with_checksum("cafebabe").build())
            .build();
        storage.save_document(&document).await.unwrap();

        let found = storage.find_document_by_checksum("cafebabe").await.unwrap();

        assert_eq!(found, Some(document.id()));
    }

    #[tokio::test]
    async fn find_document_by_checksum_returns_none_when_no_document_has_that_checksum() {
        let storage = setup().await;
        let document = FakeDocument::new()
            .with_metadata(FakeMetadata::new().with_checksum("cafebabe").build())
            .build();
        storage.save_document(&document).await.unwrap();

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
            .save_document(&Document {
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
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            })
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
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            }
        );
    }

    #[tokio::test]
    async fn save_document_does_not_affect_other_documents() {
        let storage = setup().await;
        let (id1, id2) = (Uuid::now_v7(), Uuid::now_v7());

        storage
            .save_document(&Document {
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
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            })
            .await
            .unwrap();
        storage
            .save_document(&Document {
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
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            })
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
                .save_document(&Document {
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
                        source_category: None,
                        source_sub_category: None,
                        subjects: Vec::new(),
                        purpose: None,
                    },
                })
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
            page.into_iter()
                .map(|d| d.metadata.name)
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
    }

    #[tokio::test]
    async fn list_documents_skips_offset() {
        let storage = setup().await;
        save_documents(&storage, &["one", "two", "three"]).await;

        let page = storage.list_documents(1, 2).await.unwrap();

        assert_eq!(
            page.into_iter()
                .map(|d| d.metadata.name)
                .collect::<Vec<_>>(),
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
    async fn update_metadata_replaces_metadata_and_leaves_content_untouched() {
        let storage = setup().await;
        let id = Uuid::now_v7();
        storage
            .save_document(&Document {
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
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            })
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
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
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
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
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
            .with_source_category(None)
            .with_source_sub_category(None)
            .with_subjects(Vec::<String>::new())
            .with_purpose(None)
    }

    async fn save(storage: &SqliteStorage, metadata: FakeMetadata) {
        let metadata = metadata.build();
        storage
            .save_document(
                &FakeDocument::new()
                    .with_id(metadata.id)
                    .with_metadata(metadata)
                    .build(),
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
            .map(|document| document.metadata.name)
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
        save(
            &storage,
            searchable("by-category", 4).with_source_category(SourceCategory::RealEstate),
        )
        .await;
        save(
            &storage,
            searchable("by-sub-category", 5).with_source_sub_category(SourceSubCategory::Mobile),
        )
        .await;
        save(
            &storage,
            searchable("by-subjects", 6).with_subjects(vec!["Loyer Paris".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("by-purpose", 7).with_purpose(Purpose::Attestation),
        )
        .await;

        for (term, expected) in [
            ("originalname", "by-original"),
            ("euros", "by-transcript"),
            ("payslip", "by-type"),
            ("estate", "by-category"),
            ("mobile", "by-sub-category"),
            ("loyer", "by-subjects"),
            ("attestation", "by-purpose"),
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
    async fn search_documents_keeps_documents_matching_any_value_within_a_filter() {
        let storage = setup().await;
        save(
            &storage,
            searchable("bank", 1).with_source_category(SourceCategory::Bank),
        )
        .await;
        save(
            &storage,
            searchable("energy", 2).with_source_category(SourceCategory::Energy),
        )
        .await;
        save(&storage, searchable("unclassified", 3)).await;

        let names = search_names(
            &storage,
            &[],
            &SearchFilters {
                source_categories: vec![SourceCategory::Bank, SourceCategory::Energy],
                ..Default::default()
            },
            SearchOrder::Oldest,
        )
        .await;

        assert_eq!(names, vec!["bank", "energy"]);
    }

    #[tokio::test]
    async fn search_documents_keeps_only_documents_matching_every_filter() {
        let storage = setup().await;
        save(
            &storage,
            searchable("match", 10)
                .with_source_category(SourceCategory::Health)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["alice".to_string(), "bob".to_string()]),
        )
        .await;
        // Each differs from "match" in exactly one filtered field.
        save(
            &storage,
            searchable("wrong-category", 10)
                .with_source_category(SourceCategory::Bank)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["bob".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("wrong-sub-category", 10)
                .with_source_category(SourceCategory::Health)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["bob".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("wrong-purpose", 10)
                .with_source_category(SourceCategory::Health)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Contract)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["bob".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("wrong-type", 10)
                .with_source_category(SourceCategory::Health)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Statement)
                .with_subjects(vec!["bob".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("wrong-subjects", 10)
                .with_source_category(SourceCategory::Health)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["carol".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("wrong-content-type", 10)
                .with_content_type("text/plain")
                .with_source_category(SourceCategory::Health)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["bob".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("too-old", 1)
                .with_source_category(SourceCategory::Health)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["bob".to_string()]),
        )
        .await;
        save(
            &storage,
            searchable("too-new", 100)
                .with_source_category(SourceCategory::Health)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Receipt)
                .with_subjects(vec!["bob".to_string()]),
        )
        .await;

        let names = search_names(
            &storage,
            &[],
            &SearchFilters {
                source_categories: vec![SourceCategory::Health],
                source_sub_categories: vec![SourceSubCategory::Tax],
                purposes: vec![Purpose::Invoice],
                types: vec![DocumentType::Receipt],
                subjects: vec!["bob".to_string(), "dave".to_string()],
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
        save(
            &storage,
            searchable("facture bank", 1).with_source_category(SourceCategory::Bank),
        )
        .await;
        save(
            &storage,
            searchable("facture telecom", 2).with_source_category(SourceCategory::Telecom),
        )
        .await;
        save(
            &storage,
            searchable("releve bank", 3).with_source_category(SourceCategory::Bank),
        )
        .await;

        let names = search_names(
            &storage,
            &["facture"],
            &SearchFilters {
                source_categories: vec![SourceCategory::Bank],
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
        assert_eq!(page[0].metadata.name, "two");

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
        let storage = SqliteStorage::new(pool.clone());
        save(&storage, searchable("preexisting", 1)).await;

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
    async fn count_documents_by_counts_each_value_most_common_first_skipping_unset_ones() {
        let storage = setup().await;
        save(
            &storage,
            searchable("a", 1)
                .with_source_category(SourceCategory::Energy)
                .with_source_sub_category(SourceSubCategory::Tax)
                .with_purpose(Purpose::Invoice)
                .with_type(DocumentType::Invoice),
        )
        .await;
        save(
            &storage,
            searchable("b", 2)
                .with_source_category(SourceCategory::Bank)
                .with_purpose(Purpose::Invoice)
                .with_content_type("image/png"),
        )
        .await;
        save(
            &storage,
            searchable("c", 3).with_source_category(SourceCategory::Bank),
        )
        .await;
        save(&storage, searchable("unclassified", 4)).await;
        let filters = SearchFilters::default();

        assert_eq!(
            facet(&storage, &[], &filters, FacetField::SourceCategory).await,
            counts(&[("bank", 2), ("energy", 1)])
        );
        assert_eq!(
            facet(&storage, &[], &filters, FacetField::SourceSubCategory).await,
            counts(&[("tax", 1)])
        );
        assert_eq!(
            facet(&storage, &[], &filters, FacetField::Purpose).await,
            counts(&[("invoice", 2)])
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
                .with_source_category(SourceCategory::Energy)
                .with_purpose(Purpose::Invoice),
        )
        .await;
        save(
            &storage,
            searchable("facture orange", 2)
                .with_source_category(SourceCategory::Telecom)
                .with_purpose(Purpose::Contract),
        )
        .await;
        save(
            &storage,
            searchable("releve", 3)
                .with_source_category(SourceCategory::Bank)
                .with_purpose(Purpose::Invoice),
        )
        .await;

        assert_eq!(
            facet(
                &storage,
                &["facture"],
                &SearchFilters {
                    purposes: vec![Purpose::Invoice],
                    ..Default::default()
                },
                FacetField::SourceCategory,
            )
            .await,
            counts(&[("energy", 1)])
        );
    }

    #[tokio::test]
    async fn count_documents_by_subject_counts_each_document_once_per_subject() {
        let storage = setup().await;
        save(
            &storage,
            searchable("a", 1).with_subjects(vec![
                "rent".to_string(),
                "rent".to_string(),
                "paris".to_string(),
            ]),
        )
        .await;
        save(
            &storage,
            searchable("b", 2).with_subjects(vec!["rent".to_string(), String::new()]),
        )
        .await;

        assert_eq!(
            facet(
                &storage,
                &[],
                &SearchFilters::default(),
                FacetField::Subject
            )
            .await,
            counts(&[("rent", 2), ("paris", 1)])
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
}
