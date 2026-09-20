use std::sync::Arc;

use crate::services::settings::InternalService as SettingsInternalService;
use crate::{ErrorContext as _, Result};

use super::storage::OffsetStorage;

/// The key under which the changelog consumption offset is persisted in
/// the SDK's local settings store.
const OFFSET_KEY: &str = "changelog_offset";

/// An [`OffsetStorage`] backed by the SDK's local settings store (see
/// `settings::InternalService`) rather than a dedicated table, storing the
/// offset as its base-10 string representation.
pub(crate) struct SettingsOffsetStorage {
    settings: Arc<dyn SettingsInternalService>,
}

impl SettingsOffsetStorage {
    pub(crate) fn new(settings: Arc<dyn SettingsInternalService>) -> Self {
        Self { settings }
    }
}

impl OffsetStorage for SettingsOffsetStorage {
    async fn get_offset(&self) -> Result<i64> {
        let value = self
            .settings
            .get_internal(OFFSET_KEY)
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
            .set_internal(OFFSET_KEY, &offset.to_string())
            .await
            .with_context(|| format!("failed to save changelog offset {offset} to local settings"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;

    /// An in-memory [`SettingsInternalService`] fake, for tests that don't
    /// need to touch SQLite.
    #[derive(Default)]
    struct InMemorySettings {
        values: Mutex<HashMap<String, String>>,
    }

    #[async_trait]
    impl SettingsInternalService for InMemorySettings {
        async fn get_internal(&self, key: &str) -> Result<Option<String>> {
            Ok(self.values.lock().unwrap().get(key).cloned())
        }

        async fn set_internal(&self, key: &str, value: &str) -> Result<()> {
            self.values
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn get_offset_defaults_to_zero_when_never_saved() {
        let storage = SettingsOffsetStorage::new(Arc::new(InMemorySettings::default()));

        assert_eq!(storage.get_offset().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn save_offset_then_get_offset_returns_the_saved_value() {
        let storage = SettingsOffsetStorage::new(Arc::new(InMemorySettings::default()));

        storage.save_offset(42).await.unwrap();

        assert_eq!(storage.get_offset().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn save_offset_overwrites_a_previously_saved_value() {
        let storage = SettingsOffsetStorage::new(Arc::new(InMemorySettings::default()));

        storage.save_offset(1).await.unwrap();
        storage.save_offset(2).await.unwrap();

        assert_eq!(storage.get_offset().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn get_offset_rejects_a_non_numeric_stored_value() {
        let settings = Arc::new(InMemorySettings::default());
        settings
            .set_internal(OFFSET_KEY, "not-a-number")
            .await
            .unwrap();
        let storage = SettingsOffsetStorage::new(settings);

        assert!(storage.get_offset().await.is_err());
    }
}
