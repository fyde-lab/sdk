use std::sync::Arc;

use async_trait::async_trait;

use crate::domains::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

use super::storage::OffsetStorage;

/// The key under which the changelog consumption offset is persisted in
/// the SDK's local settings store.
const OFFSET_KEY: &str = "changelog_offset";

/// An [`OffsetStorage`] backed by the SDK's local settings store rather
/// than a dedicated table, storing the offset as its base-10 string
/// representation.
pub(crate) struct SettingsOffsetStorage {
    settings: Arc<dyn SettingsService>,
}

impl SettingsOffsetStorage {
    pub(crate) fn new(settings: Arc<dyn SettingsService>) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl OffsetStorage for SettingsOffsetStorage {
    async fn get_offset(&self) -> Result<i64> {
        let value = self
            .settings
            .get(OFFSET_KEY)
            .await
            .context("failed to fetch changelog offset from local settings")?;

        match value {
            Some(value) => value.parse().with_context(|| {
                format!("invalid changelog offset {value:?} stored in local settings")
            }),
            None => Ok(0),
        }
    }

    async fn save_offset(&self, offset: i64) -> Result<()> {
        self.settings
            .set(OFFSET_KEY, &offset.to_string())
            .await
            .with_context(|| format!("failed to save changelog offset {offset} to local settings"))
    }
}

#[cfg(test)]
mod tests {
    use crate::domains::settings::MockService as MockSettingsService;

    use super::*;

    #[tokio::test]
    async fn get_offset_defaults_to_zero_when_never_saved() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == OFFSET_KEY)
            .returning(|_| Ok(None));

        let storage = SettingsOffsetStorage::new(Arc::new(settings));

        assert_eq!(storage.get_offset().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn save_offset_then_get_offset_returns_the_saved_value() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, value| key == OFFSET_KEY && value == "42")
            .times(1)
            .returning(|_, _| Ok(()));
        settings
            .expect_get()
            .withf(|key| key == OFFSET_KEY)
            .returning(|_| Ok(Some("42".to_string())));

        let storage = SettingsOffsetStorage::new(Arc::new(settings));

        storage.save_offset(42).await.unwrap();

        assert_eq!(storage.get_offset().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn save_offset_overwrites_a_previously_saved_value() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == OFFSET_KEY)
            .times(2)
            .returning(|_, _| Ok(()));
        settings
            .expect_get()
            .withf(|key| key == OFFSET_KEY)
            .returning(|_| Ok(Some("2".to_string())));

        let storage = SettingsOffsetStorage::new(Arc::new(settings));

        storage.save_offset(1).await.unwrap();
        storage.save_offset(2).await.unwrap();

        assert_eq!(storage.get_offset().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn get_offset_rejects_a_non_numeric_stored_value() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == OFFSET_KEY)
            .returning(|_| Ok(Some("not-a-number".to_string())));

        let storage = SettingsOffsetStorage::new(Arc::new(settings));

        assert!(storage.get_offset().await.is_err());
    }
}
