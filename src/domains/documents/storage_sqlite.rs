use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::storage::Storage;
use super::{Document, Metadata, Purpose, SourceCategory, SourceSubCategory};

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
        .bind(&document.metadata.r#type)
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
                r#type: row.get("type"),
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

        rows.into_iter()
            .map(|row| {
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
                        r#type: row.get("type"),
                        source_category: source_category_from_column(row.get("source_category"))?,
                        source_sub_category: source_sub_category_from_column(
                            row.get("source_sub_category"),
                        )?,
                        subjects: subjects_from_column(row.get("subjects"))?,
                        purpose: purpose_from_column(row.get("purpose"))?,
                    },
                    content,
                })
            })
            .collect()
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
        .bind(&metadata.r#type)
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

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

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
                    r#type: String::new(),
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
                    r#type: String::new(),
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
                    r#type: String::new(),
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
                    r#type: String::new(),
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
                        r#type: String::new(),
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
                    r#type: String::new(),
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
                    r#type: String::new(),
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
                    r#type: String::new(),
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            )
            .await;

        assert!(result.is_ok());
    }
}
