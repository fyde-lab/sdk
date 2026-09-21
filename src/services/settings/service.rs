use async_trait::async_trait;

use crate::Result;

use super::Service;
use super::storage::Storage;

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

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::super::storage::MockStorage;
    use super::*;

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
