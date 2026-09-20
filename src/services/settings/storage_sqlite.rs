use sqlx::{Row, SqlitePool};

use crate::{ErrorContext as _, Result};

use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the `settings`
/// table).
pub(crate) struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Storage for SqliteStorage {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM settings WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("failed to fetch setting {key:?} from local database"))?;

        Ok(row.map(|row| row.get("value")))
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to save setting {key:?} to local database"))?;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await
            .with_context(|| format!("failed to delete setting {key:?} from local database"))?;

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

        assert_eq!(storage.get("auth_token").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_returns_the_stored_value() {
        let storage = setup().await;

        storage.set("auth_token", "a-token").await.unwrap();

        assert_eq!(
            storage.get("auth_token").await.unwrap(),
            Some("a-token".to_string())
        );
    }

    #[tokio::test]
    async fn set_overwrites_a_previously_stored_value() {
        let storage = setup().await;

        storage.set("auth_token", "old-token").await.unwrap();
        storage.set("auth_token", "new-token").await.unwrap();

        assert_eq!(
            storage.get("auth_token").await.unwrap(),
            Some("new-token".to_string())
        );
    }

    #[tokio::test]
    async fn distinct_keys_are_stored_independently() {
        let storage = setup().await;

        storage.set("auth_token", "a-token").await.unwrap();
        storage.set("master_key", "a-key").await.unwrap();

        assert_eq!(
            storage.get("auth_token").await.unwrap(),
            Some("a-token".to_string())
        );
        assert_eq!(
            storage.get("master_key").await.unwrap(),
            Some("a-key".to_string())
        );
    }

    #[tokio::test]
    async fn delete_removes_the_stored_value() {
        let storage = setup().await;
        storage.set("auth_token", "a-token").await.unwrap();

        storage.delete("auth_token").await.unwrap();

        assert_eq!(storage.get("auth_token").await.unwrap(), None);
    }

    #[tokio::test]
    async fn delete_is_a_no_op_when_never_set() {
        let storage = setup().await;

        storage.delete("auth_token").await.unwrap();

        assert_eq!(storage.get("auth_token").await.unwrap(), None);
    }
}
