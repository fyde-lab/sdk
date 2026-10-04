use async_trait::async_trait;
use serde_json::Value;

use crate::Result;

use super::Service;
use super::storage::Storage;

/// The default [`Service`] implementation, delegating persistence to an
/// injected [`Storage`].
pub(super) struct SessionClient<S: Storage> {
    storage: S,
}

impl<S: Storage> SessionClient<S> {
    pub(super) fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: Storage> Service for SessionClient<S> {
    async fn load(&self, scraper_name: &str) -> Result<Value> {
        // A scraper that has never run before has no saved session data yet
        // — defaulting to an empty object here (rather than `Null`) means a
        // script's `fyde.session` is always a table it can freely read/write
        // fields on, on both a first run and every run after.
        let data = self.storage.get(scraper_name).await?;
        Ok(data.unwrap_or_else(|| Value::Object(serde_json::Map::new())))
    }

    async fn save(&self, scraper_name: &str, data: Value) -> Result<()> {
        self.storage.set(scraper_name, data).await
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;
    use serde_json::json;

    use super::super::storage::MockStorage;
    use super::*;

    #[tokio::test]
    async fn load_defaults_to_an_empty_object_when_never_saved() {
        let mut storage = MockStorage::new();
        storage
            .expect_get()
            .with(eq("didaxis"))
            .return_once(|_| Ok(None));
        let client = SessionClient::new(storage);

        assert_eq!(client.load("didaxis").await.unwrap(), json!({}));
    }

    #[tokio::test]
    async fn load_returns_the_storages_saved_data() {
        let mut storage = MockStorage::new();
        storage
            .expect_get()
            .with(eq("didaxis"))
            .return_once(|_| Ok(Some(json!({"cursor": "abc"}))));
        let client = SessionClient::new(storage);

        assert_eq!(
            client.load("didaxis").await.unwrap(),
            json!({"cursor": "abc"})
        );
    }

    #[tokio::test]
    async fn save_replaces_the_scrapers_session_data_in_storage() {
        let mut storage = MockStorage::new();
        storage
            .expect_set()
            .withf(|scraper_name, data| {
                scraper_name == "didaxis" && *data == json!({"cursor": "def"})
            })
            .times(1)
            .returning(|_, _| Ok(()));
        let client = SessionClient::new(storage);

        client
            .save("didaxis", json!({"cursor": "def"}))
            .await
            .unwrap();
    }
}
