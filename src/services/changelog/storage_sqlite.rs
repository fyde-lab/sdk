use sqlx::SqlitePool;

use crate::{Error, Result};

use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `changelog_offset` table).
pub(super) struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub(super) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Storage for SqliteStorage {
    async fn read_offset(&self) -> Result<i64> {
        let offset: Option<i64> =
            sqlx::query_scalar("SELECT offset FROM changelog_offset WHERE id = 0")
                .fetch_optional(&self.pool)
                .await
                .map_err(Error::Database)?;

        Ok(offset.unwrap_or(0))
    }

    async fn write_offset(&self, offset: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO changelog_offset (id, offset) VALUES (0, ?1)
             ON CONFLICT (id) DO UPDATE SET offset = excluded.offset",
        )
        .bind(offset)
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(())
    }
}
