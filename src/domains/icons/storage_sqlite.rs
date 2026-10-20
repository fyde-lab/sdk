use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::Icon;
use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the `icons`
/// table).
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
    async fn get(&self, id: Uuid) -> Result<Option<Icon>> {
        let row = sqlx::query("SELECT content FROM icons WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("failed to read icon {id} from local database"))?;

        Ok(row.map(|row| Icon {
            id,
            content: row.get("content"),
        }))
    }

    async fn save(&self, icon: &Icon) -> Result<()> {
        sqlx::query("INSERT INTO icons (id, content) VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING")
            .bind(icon.id.to_string())
            .bind(&icon.content)
            .execute(&self.pool)
            .await
            .with_context(|| format!("failed to save icon {} to local database", icon.id))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::domains::icons::FakeIcon;

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
    async fn get_returns_none_for_an_icon_never_saved() {
        let storage = setup().await;

        assert_eq!(storage.get(Uuid::new_v4()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_returns_a_saved_icon() {
        let storage = setup().await;
        let icon = FakeIcon::new().build();

        storage.save(&icon).await.unwrap();

        assert_eq!(storage.get(icon.id()).await.unwrap(), Some(icon));
    }

    #[tokio::test]
    async fn saving_an_icon_already_saved_is_a_no_op() {
        let storage = setup().await;
        let icon = FakeIcon::new().build();
        storage.save(&icon).await.unwrap();

        storage
            .save(&FakeIcon::new().with_id(icon.id()).build())
            .await
            .unwrap();

        assert_eq!(storage.get(icon.id()).await.unwrap(), Some(icon));
    }
}
