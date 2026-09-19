use sqlx::{Row, SqlitePool};

use crate::{ErrorContext as _, Result};

use super::storage::OffsetStorage;

/// An [`OffsetStorage`] backed by the SDK's local SQLite database (the
/// single-row `changelog_offset` table).
pub(crate) struct SqliteOffsetStorage {
    pool: SqlitePool,
}

impl SqliteOffsetStorage {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl OffsetStorage for SqliteOffsetStorage {
    async fn get_offset(&self) -> Result<i64> {
        let row = sqlx::query("SELECT offset FROM changelog_offset WHERE id = 0")
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch changelog offset from local database")?;

        Ok(row.map(|row| row.get("offset")).unwrap_or(0))
    }

    async fn save_offset(&self, offset: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO changelog_offset (id, offset) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET offset = excluded.offset",
        )
        .bind(offset)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to save changelog offset {offset} to local database"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn setup() -> SqliteOffsetStorage {
        // A single connection, so all queries in a test hit the same
        // in-memory database rather than each getting its own.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteOffsetStorage::new(pool)
    }

    #[tokio::test]
    async fn get_offset_defaults_to_zero_when_never_saved() {
        let storage = setup().await;

        assert_eq!(storage.get_offset().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn save_offset_then_get_offset_returns_the_saved_value() {
        let storage = setup().await;

        storage.save_offset(42).await.unwrap();

        assert_eq!(storage.get_offset().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn save_offset_overwrites_a_previously_saved_value() {
        let storage = setup().await;

        storage.save_offset(1).await.unwrap();
        storage.save_offset(2).await.unwrap();

        assert_eq!(storage.get_offset().await.unwrap(), 2);
    }
}
