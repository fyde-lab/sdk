use async_trait::async_trait;

use crate::Result;

use super::storage::Storage;
use super::{InternalService, Service};

/// The default [`Service`]/[`InternalService`] implementation, delegating
/// persistence to an injected [`Storage`].
pub(super) struct SettingsClient<S: Storage> {
    storage: S,
}

impl<S: Storage> SettingsClient<S> {
    pub(super) fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: Storage> Service for SettingsClient<S> {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        self.storage.get(key).await
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.storage.set(key, value).await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.storage.delete(key).await
    }
}

#[async_trait]
impl<S: Storage> InternalService for SettingsClient<S> {
    async fn get_internal(&self, key: &str) -> Result<Option<String>> {
        self.storage.get(key).await
    }

    async fn set_internal(&self, key: &str, value: &str) -> Result<()> {
        self.storage.set(key, value).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// An in-memory [`Storage`] mock, for tests that exercise
    /// [`SettingsClient`]'s own behavior without touching SQLite.
    #[derive(Default)]
    struct MockStorage {
        values: Mutex<HashMap<String, String>>,
    }

    impl Storage for MockStorage {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            Ok(self.values.lock().unwrap().get(key).cloned())
        }

        async fn set(&self, key: &str, value: &str) -> Result<()> {
            self.values
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        async fn delete(&self, key: &str) -> Result<()> {
            self.values.lock().unwrap().remove(key);
            Ok(())
        }
    }

    #[tokio::test]
    async fn get_returns_none_when_never_set() {
        let client = SettingsClient::new(MockStorage::default());

        assert_eq!(client.get("theme").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_returns_the_stored_value() {
        let client = SettingsClient::new(MockStorage::default());

        client.set("theme", "dark").await.unwrap();

        assert_eq!(client.get("theme").await.unwrap(), Some("dark".to_string()));
    }

    #[tokio::test]
    async fn delete_removes_the_stored_value() {
        let client = SettingsClient::new(MockStorage::default());
        client.set("theme", "dark").await.unwrap();

        client.delete("theme").await.unwrap();

        assert_eq!(client.get("theme").await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_internal_returns_none_when_never_set() {
        let client = SettingsClient::new(MockStorage::default());

        assert_eq!(client.get_internal("auth_token").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_internal_then_get_internal_returns_the_stored_value() {
        let client = SettingsClient::new(MockStorage::default());

        client.set_internal("auth_token", "a-token").await.unwrap();

        assert_eq!(
            client.get_internal("auth_token").await.unwrap(),
            Some("a-token".to_string())
        );
    }
}
