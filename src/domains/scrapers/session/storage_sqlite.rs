use async_trait::async_trait;
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::{ErrorContext as _, Result};

use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `scraper_sessions` table).
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
    async fn get(&self, scraper_name: &str) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT data FROM scraper_sessions WHERE scraper_name = ?1")
            .bind(scraper_name)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| {
                format!(
                    "failed to fetch session data for scraper {scraper_name:?} from local database"
                )
            })?;

        row.map(|row| {
            let data: String = row.get("data");
            serde_json::from_str(&data).with_context(|| {
                format!("failed to parse saved session data for scraper {scraper_name:?}")
            })
        })
        .transpose()
    }

    async fn set(&self, scraper_name: &str, data: Value) -> Result<()> {
        let serialized = serde_json::to_string(&data).with_context(|| {
            format!("failed to serialize session data for scraper {scraper_name:?}")
        })?;

        sqlx::query(
            "INSERT INTO scraper_sessions (scraper_name, data) VALUES (?1, ?2)
             ON CONFLICT(scraper_name) DO UPDATE SET data = excluded.data",
        )
        .bind(scraper_name)
        .bind(serialized)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!("failed to save session data for scraper {scraper_name:?} to local database")
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn setup() -> SqliteStorage {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteStorage::new(pool)
    }

    #[tokio::test]
    async fn get_returns_none_when_never_saved() {
        let storage = setup().await;

        assert_eq!(storage.get("didaxis").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_returns_the_saved_data() {
        let storage = setup().await;

        storage
            .set("didaxis", json!({"cursor": "abc"}))
            .await
            .unwrap();

        assert_eq!(
            storage.get("didaxis").await.unwrap(),
            Some(json!({"cursor": "abc"}))
        );
    }

    #[tokio::test]
    async fn set_overwrites_previously_saved_data() {
        let storage = setup().await;

        storage
            .set("didaxis", json!({"cursor": "old"}))
            .await
            .unwrap();
        storage
            .set("didaxis", json!({"cursor": "new"}))
            .await
            .unwrap();

        assert_eq!(
            storage.get("didaxis").await.unwrap(),
            Some(json!({"cursor": "new"}))
        );
    }

    #[tokio::test]
    async fn distinct_scrapers_are_stored_independently() {
        let storage = setup().await;

        storage
            .set("didaxis", json!({"cursor": "a"}))
            .await
            .unwrap();
        storage.set("impots", json!({"cursor": "b"})).await.unwrap();

        assert_eq!(
            storage.get("didaxis").await.unwrap(),
            Some(json!({"cursor": "a"}))
        );
        assert_eq!(
            storage.get("impots").await.unwrap(),
            Some(json!({"cursor": "b"}))
        );
    }
}
