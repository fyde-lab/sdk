use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::domains::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

use super::storage::CursorStorage;

/// The key under which the changelog consumption cursor is persisted in
/// the SDK's local settings store.
const CURSOR_KEY: &str = "changelog_cursor_id";

/// A [`CursorStorage`] backed by the SDK's local settings store rather
/// than a dedicated table, storing the cursor as its canonical UUID string
/// representation.
pub(crate) struct SettingsCursorStorage {
    settings: Arc<dyn SettingsService>,
}

impl SettingsCursorStorage {
    pub(crate) fn new(settings: Arc<dyn SettingsService>) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl CursorStorage for SettingsCursorStorage {
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
}

#[cfg(test)]
mod tests {
    use crate::domains::settings::MockService as MockSettingsService;

    use super::*;

    #[tokio::test]
    async fn get_cursor_defaults_to_nil_when_never_saved() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == CURSOR_KEY)
            .returning(|_| Ok(None));

        let storage = SettingsCursorStorage::new(Arc::new(settings));

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

        let storage = SettingsCursorStorage::new(Arc::new(settings));

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

        let storage = SettingsCursorStorage::new(Arc::new(settings));

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

        let storage = SettingsCursorStorage::new(Arc::new(settings));

        assert!(storage.get_cursor().await.is_err());
    }
}
