use std::sync::Arc;

use async_trait::async_trait;

use crate::Result;

use super::storage::Storage;
use super::{PUBLIC_KEY_PREFIX, Service};

/// The default [`Service`] implementation, delegating persistence to an
/// injected [`Storage`].
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

/// The [`Service`] handed to callers (see [`super::public`]): delegates to
/// the SDK's own settings, with every key under [`PUBLIC_KEY_PREFIX`].
pub(super) struct PublicSettings {
    internal: Arc<dyn Service>,
}

impl PublicSettings {
    pub(super) fn new(internal: Arc<dyn Service>) -> Self {
        Self { internal }
    }
}

fn public_key(key: &str) -> String {
    format!("{PUBLIC_KEY_PREFIX}{key}")
}

#[async_trait]
impl Service for PublicSettings {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        self.internal.get(&public_key(key)).await
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.internal.set(&public_key(key), value).await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.internal.delete(&public_key(key)).await
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::super::MockService;
    use super::super::storage::MockStorage;
    use super::*;

    #[tokio::test]
    async fn public_get_reads_the_prefixed_key() {
        let mut internal = MockService::new();
        internal
            .expect_get()
            .with(eq("app.username"))
            .return_once(|_| Ok(Some("theirs".to_string())));
        let client = PublicSettings::new(Arc::new(internal));

        assert_eq!(
            client.get("username").await.unwrap(),
            Some("theirs".to_string())
        );
    }

    #[tokio::test]
    async fn public_set_never_reaches_an_internal_key() {
        let mut internal = MockService::new();
        internal
            .expect_set()
            .with(eq("app.username"), eq("mallory"))
            .return_once(|_, _| Ok(()));
        let client = PublicSettings::new(Arc::new(internal));

        client.set("username", "mallory").await.unwrap();
    }

    #[tokio::test]
    async fn public_delete_removes_the_prefixed_key() {
        let mut internal = MockService::new();
        internal
            .expect_delete()
            .with(eq("app.role"))
            .return_once(|_| Ok(()));
        let client = PublicSettings::new(Arc::new(internal));

        client.delete("role").await.unwrap();
    }

    #[tokio::test]
    async fn get_returns_none_when_never_set() {
        let mut storage = MockStorage::new();
        storage
            .expect_get()
            .with(eq("theme"))
            .return_once(|_| Ok(None));
        let client = SettingsClient::new(storage);

        assert_eq!(client.get("theme").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_call_the_storage() {
        let mut storage = MockStorage::new();
        storage
            .expect_set()
            .with(eq("theme"), eq("dark"))
            .return_once(|_, _| Ok(()));
        let client = SettingsClient::new(storage);

        client.set("theme", "dark").await.unwrap();
    }

    #[tokio::test]
    async fn delete_removes_the_stored_value() {
        let mut storage = MockStorage::new();
        storage
            .expect_delete()
            .with(eq("theme"))
            .return_once(|_| Ok(()));
        let client = SettingsClient::new(storage);

        client.delete("theme").await.unwrap();
    }
}
