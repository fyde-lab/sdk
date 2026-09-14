use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{Error, Result};

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
}
