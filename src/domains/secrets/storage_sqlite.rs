use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::{ErrorContext as _, Result};

use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `insecure_secrets` table) — the plaintext fallback for a device with no
/// usable OS credential store (see [`super::SecretLocation::Database`]).
pub(super) struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub(super) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM insecure_secrets WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("failed to fetch secret {key:?} from local database"))?;

        Ok(row.map(|row| row.get("value")))
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO insecure_secrets (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to save secret {key:?} to local database"))?;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM insecure_secrets WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await
            .with_context(|| format!("failed to delete secret {key:?} from local database"))?;

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
    async fn get_returns_none_when_never_set() {
        let storage = setup().await;

        assert_eq!(storage.get("master_key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_overwrites_the_previous_value() {
        let storage = setup().await;

        storage.set("master_key", "old").await.unwrap();
        storage.set("master_key", "new").await.unwrap();

        assert_eq!(
            storage.get("master_key").await.unwrap(),
            Some("new".to_string())
        );
    }

    #[tokio::test]
    async fn delete_removes_the_secret_and_tolerates_a_missing_one() {
        let storage = setup().await;
        storage.set("master_key", "the-key").await.unwrap();

        storage.delete("master_key").await.unwrap();
        storage.delete("master_key").await.unwrap();

        assert_eq!(storage.get("master_key").await.unwrap(), None);
    }
}
