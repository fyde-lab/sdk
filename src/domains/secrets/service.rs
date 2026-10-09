use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{ErrorContext as _, Result};

use super::storage::Storage;
use super::{ALL_SECRETS, Service};

/// The default [`Service`] implementation, delegating persistence to an
/// injected [`Storage`], with a read-through, write-through in-memory cache
/// in front of it: the session token is read on every authenticated gRPC
/// call and the master key on every changelog event decrypted, and each
/// native-store read is a D-Bus/Keychain/JNI round trip. The cache only ever
/// holds what this process itself read or wrote, and is dropped with the
/// `Client`; a logout performed by another process sharing the same local
/// database only takes effect here on the next `Client::init` (the
/// server rejects the revoked token in the meantime).
pub(super) struct SecretsClient<S: Storage> {
    storage: S,
    /// `None` cached for a key means "known absent", so an unauthenticated
    /// client doesn't hit the store on every call either.
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl<S: Storage> SecretsClient<S> {
    pub(super) fn new(storage: S) -> Self {
        Self {
            storage,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<String>>> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl<S: Storage> Service for SecretsClient<S> {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        if let Some(cached) = self.cache().get(key) {
            return Ok(cached.clone());
        }

        let value = self
            .storage
            .get(key)
            .await
            .with_context(|| format!("failed to read secret {key:?}"))?;
        self.cache().insert(key.to_string(), value.clone());
        Ok(value)
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        // Dropped first, so a failed write never leaves a stale cached value.
        self.cache().remove(key);
        self.storage
            .set(key, value)
            .await
            .with_context(|| format!("failed to store secret {key:?}"))?;
        self.cache()
            .insert(key.to_string(), Some(value.to_string()));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.cache().remove(key);
        self.storage
            .delete(key)
            .await
            .with_context(|| format!("failed to delete secret {key:?}"))?;
        self.cache().insert(key.to_string(), None);
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        // Attempts every key even if one fails, so a single unreachable
        // entry doesn't leave the others behind; reports the first failure.
        let mut first_err = None;
        for key in ALL_SECRETS {
            if let Err(err) = self.delete(key).await {
                first_err.get_or_insert(err);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::super::storage::MockStorage;
    use super::super::{MASTER_KEY_SECRET, SESSION_TOKEN_SECRET};
    use super::*;
    use crate::Error;

    #[tokio::test]
    async fn get_returns_none_when_never_set() {
        let mut storage = MockStorage::new();
        storage
            .expect_get()
            .with(eq(MASTER_KEY_SECRET))
            .return_once(|_| Ok(None));
        let client = SecretsClient::new(storage);

        assert_eq!(client.get(MASTER_KEY_SECRET).await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_stores_the_value_under_the_key() {
        let mut storage = MockStorage::new();
        storage
            .expect_set()
            .with(eq(SESSION_TOKEN_SECRET), eq("a-token"))
            .times(1)
            .return_once(|_, _| Ok(()));
        let client = SecretsClient::new(storage);

        client.set(SESSION_TOKEN_SECRET, "a-token").await.unwrap();
    }

    #[tokio::test]
    async fn clear_deletes_every_secret() {
        let mut storage = MockStorage::new();
        for key in ALL_SECRETS {
            storage
                .expect_delete()
                .with(eq(*key))
                .times(1)
                .return_once(|_| Ok(()));
        }
        let client = SecretsClient::new(storage);

        client.clear().await.unwrap();
    }

    #[tokio::test]
    async fn clear_still_deletes_the_other_secrets_when_one_fails() {
        let mut storage = MockStorage::new();
        storage
            .expect_delete()
            .with(eq(MASTER_KEY_SECRET))
            .times(1)
            .return_once(|_| Err(Error::Encryption("keystore locked".into())));
        storage
            .expect_delete()
            .with(eq(SESSION_TOKEN_SECRET))
            .times(1)
            .return_once(|_| Ok(()));
        let client = SecretsClient::new(storage);

        assert!(client.clear().await.is_err());
    }

    #[tokio::test]
    async fn get_reads_the_store_only_once_per_key() {
        let mut storage = MockStorage::new();
        storage
            .expect_get()
            .with(eq(SESSION_TOKEN_SECRET))
            .times(1)
            .return_once(|_| Ok(Some("a-token".to_string())));
        let client = SecretsClient::new(storage);

        client.get(SESSION_TOKEN_SECRET).await.unwrap();
        let again = client.get(SESSION_TOKEN_SECRET).await.unwrap();

        assert_eq!(again, Some("a-token".to_string()));
    }

    #[tokio::test]
    async fn get_returns_what_set_wrote_without_reading_the_store() {
        let mut storage = MockStorage::new();
        storage.expect_set().return_once(|_, _| Ok(()));
        // No `expect_get()`: the mock panics if `get` reaches the store.
        let client = SecretsClient::new(storage);

        client.set(MASTER_KEY_SECRET, "the-key").await.unwrap();

        assert_eq!(
            client.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("the-key".to_string())
        );
    }

    #[tokio::test]
    async fn get_returns_none_after_clear_without_reading_the_store() {
        let mut storage = MockStorage::new();
        storage.expect_set().return_once(|_, _| Ok(()));
        storage.expect_delete().returning(|_| Ok(()));
        let client = SecretsClient::new(storage);
        client.set(SESSION_TOKEN_SECRET, "a-token").await.unwrap();

        client.clear().await.unwrap();

        assert_eq!(client.get(SESSION_TOKEN_SECRET).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_failed_set_does_not_leave_the_new_value_cached() {
        let mut storage = MockStorage::new();
        storage
            .expect_set()
            .return_once(|_, _| Err(Error::Encryption("keychain locked".into())));
        storage.expect_get().times(1).return_once(|_| Ok(None));
        let client = SecretsClient::new(storage);

        assert!(client.set(MASTER_KEY_SECRET, "the-key").await.is_err());

        assert_eq!(client.get(MASTER_KEY_SECRET).await.unwrap(), None);
    }
}
