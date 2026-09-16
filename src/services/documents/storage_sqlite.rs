use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{Error, Result};

use super::storage::Storage;
use super::Document;

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
    async fn save_document(
        &self,
        id: Uuid,
        name: &str,
        content_type: &str,
        content: &[u8],
        created_at: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO documents (id, name, content_type, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(id.to_string())
        .bind(name)
        .bind(content_type)
        .bind(content)
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
