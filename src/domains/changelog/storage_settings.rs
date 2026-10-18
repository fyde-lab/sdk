use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::domains::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

use super::storage::CursorStorage;

/// The key under which the changelog consumption cursor is persisted in
/// the SDK's local settings store.
const CURSOR_KEY: &str = "changelog_cursor_id";

/// A [`CursorStorage`] keeping the cursor in the SDK's local settings store
/// (as its canonical UUID string representation) and applied events'
/// digests in the local database's `applied_changelog_events` table.
pub(crate) struct LocalCursorStorage {
    settings: Arc<dyn SettingsService>,
    pool: SqlitePool,
}

impl LocalCursorStorage {
    pub(crate) fn new(settings: Arc<dyn SettingsService>, pool: SqlitePool) -> Self {
        Self { settings, pool }
    }
}

#[async_trait]
impl CursorStorage for LocalCursorStorage {
    async fn get_cursor(&self) -> Result<Uuid> {
        let value = self
            .settings
            .get(CURSOR_KEY)
            .await
            .context("failed to fetch changelog cursor from local settings")?;

        match value {
            Some(value) => Uuid::parse_str(&value).with_context(|| {
                format!("invalid changelog cursor {value:?} stored in local settings")
            }),
            None => Ok(Uuid::nil()),
        }
    }

    async fn save_cursor(&self, id: Uuid) -> Result<()> {
        self.settings
            .set(CURSOR_KEY, &id.to_string())
            .await
            .with_context(|| format!("failed to save changelog cursor {id} to local settings"))
    }

    async fn is_applied(&self, digest: &[u8; 32]) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM applied_changelog_events WHERE digest = ?1)",
        )
        .bind(digest.as_slice())
        .fetch_one(&self.pool)
        .await
        .context("failed to look up an applied changelog event in local database")
    }

    async fn mark_applied(&self, digest: &[u8; 32]) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO applied_changelog_events (digest) VALUES (?1)")
            .bind(digest.as_slice())
            .execute(&self.pool)
            .await
            .context("failed to record an applied changelog event in local database")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use crate::domains::settings::MockService as MockSettingsService;

    use super::*;

    async fn storage(settings: MockSettingsService) -> LocalCursorStorage {
        // A single connection, so all queries in a test hit the same
        // in-memory database rather than each getting its own.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        LocalCursorStorage::new(Arc::new(settings), pool)
    }

    #[tokio::test]
    async fn is_applied_is_false_for_an_event_never_marked() {
        let storage = storage(MockSettingsService::new()).await;

        assert!(!storage.is_applied(&[1; 32]).await.unwrap());
    }

    #[tokio::test]
    async fn mark_applied_then_is_applied_recognises_only_that_event() {
        let storage = storage(MockSettingsService::new()).await;

        storage.mark_applied(&[1; 32]).await.unwrap();

        assert!(storage.is_applied(&[1; 32]).await.unwrap());
        assert!(!storage.is_applied(&[2; 32]).await.unwrap());
    }

    #[tokio::test]
    async fn mark_applied_twice_is_not_an_error() {
        let storage = storage(MockSettingsService::new()).await;

        storage.mark_applied(&[1; 32]).await.unwrap();
        storage.mark_applied(&[1; 32]).await.unwrap();

        assert!(storage.is_applied(&[1; 32]).await.unwrap());
    }

    #[tokio::test]
    async fn get_cursor_defaults_to_nil_when_never_saved() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == CURSOR_KEY)
            .returning(|_| Ok(None));

        let storage = storage(settings).await;

        assert_eq!(storage.get_cursor().await.unwrap(), Uuid::nil());
    }

    #[tokio::test]
    async fn save_cursor_then_get_cursor_returns_the_saved_value() {
        let id = Uuid::now_v7();

        let mut settings = MockSettingsService::new();
        let expected_value = id.to_string();
        settings
            .expect_set()
            .withf(move |key, value| key == CURSOR_KEY && value == expected_value)
            .times(1)
            .returning(|_, _| Ok(()));
        let returned_value = id.to_string();
        settings
            .expect_get()
            .withf(|key| key == CURSOR_KEY)
            .returning(move |_| Ok(Some(returned_value.clone())));

        let storage = storage(settings).await;

        storage.save_cursor(id).await.unwrap();

        assert_eq!(storage.get_cursor().await.unwrap(), id);
    }

    #[tokio::test]
    async fn save_cursor_overwrites_a_previously_saved_value() {
        let (first, second) = (Uuid::now_v7(), Uuid::now_v7());

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == CURSOR_KEY)
            .times(2)
            .returning(|_, _| Ok(()));
        let returned_value = second.to_string();
        settings
            .expect_get()
            .withf(|key| key == CURSOR_KEY)
            .returning(move |_| Ok(Some(returned_value.clone())));

        let storage = storage(settings).await;

        storage.save_cursor(first).await.unwrap();
        storage.save_cursor(second).await.unwrap();

        assert_eq!(storage.get_cursor().await.unwrap(), second);
    }

    #[tokio::test]
    async fn get_cursor_rejects_a_non_uuid_stored_value() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == CURSOR_KEY)
            .returning(|_| Ok(Some("not-a-uuid".to_string())));

        let storage = storage(settings).await;

        assert!(storage.get_cursor().await.is_err());
    }
}
