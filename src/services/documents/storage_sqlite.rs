use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{Error, Result};

use super::Document;
use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `documents` table).
pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Storage for SqliteStorage {
    async fn save_document(&self, document: &Document, created_at: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO documents (id, name, content_type, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(document.id.to_string())
        .bind(&document.name)
        .bind(&document.content_type)
        .bind(&document.content)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(())
    }

    async fn get_document(&self, id: Uuid) -> Result<Option<Document>> {
        let row = sqlx::query("SELECT name, content_type, content FROM documents WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)?;

        let Some(row) = row else {
            return Ok(None);
        };

        Ok(Some(Document {
            id,
            name: row.get("name"),
            content_type: row.get("content_type"),
            content: row.get("content"),
        }))
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

        let result = storage.get_document(Uuid::new_v4()).await.unwrap();

        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn get_document_returns_a_previously_saved_document() {
        let storage = setup().await;
        let id = Uuid::new_v4();

        storage
            .save_document(
                &Document {
                    id,
                    name: "report.pdf".to_string(),
                    content_type: "application/pdf".to_string(),
                    content: b"hello".to_vec(),
                },
                1_700_000_000,
            )
            .await
            .unwrap();

        let document = storage.get_document(id).await.unwrap().unwrap();

        assert_eq!(
            document,
            Document {
                id,
                name: "report.pdf".to_string(),
                content_type: "application/pdf".to_string(),
                content: b"hello".to_vec(),
            }
        );
    }

    #[tokio::test]
    async fn save_document_does_not_affect_other_documents() {
        let storage = setup().await;
        let (id1, id2) = (Uuid::new_v4(), Uuid::new_v4());

        storage
            .save_document(
                &Document {
                    id: id1,
                    name: "one.txt".to_string(),
                    content_type: "text/plain".to_string(),
                    content: b"one".to_vec(),
                },
                1_700_000_000,
            )
            .await
            .unwrap();
        storage
            .save_document(
                &Document {
                    id: id2,
                    name: "two.txt".to_string(),
                    content_type: "text/plain".to_string(),
                    content: b"two".to_vec(),
                },
                1_700_000_001,
            )
            .await
            .unwrap();

        let document = storage.get_document(id1).await.unwrap().unwrap();

        assert_eq!(document.name, "one.txt");
        assert_eq!(document.content, b"one");
    }
}
