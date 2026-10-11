use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{ErrorContext as _, ErrorKind, Result};

use super::storage::Storage;
use super::{ALL_SECRETS, SecretLocation, Service};

/// A key never written to the OS credential store: reading it is how
/// [`Service::is_secure_storage_available`] checks the store answers at all.
const AVAILABILITY_PROBE: &str = "availability_probe";

/// The default [`Service`] implementation, over the OS credential store
/// (`secure`, `None` if it couldn't even be opened) with the local database
/// (`database`) as its plaintext fallback, plus a read-through, write-through
/// in-memory cache in front of both: the session token is read on every
/// authenticated gRPC call and the master key on every changelog event
/// decrypted, and each native-store read is a D-Bus/Keychain/JNI round trip.
/// The cache only ever holds what this process itself read or wrote, and is
/// dropped with the `Client`; a logout performed by another process sharing
/// the same local database only takes effect here on the next `Client::init`
/// (the server rejects the revoked token in the meantime).
pub(super) struct SecretsClient<S: Storage, D: Storage> {
    secure: Option<S>,
    database: D,
    /// `None` cached for a key means "known absent", so an unauthenticated
    /// client doesn't hit the store on every call either.
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl<S: Storage, D: Storage> SecretsClient<S, D> {
    pub(super) fn new(secure: Option<S>, database: D) -> Self {
        Self {
            secure,
            database,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<String>>> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl<S: Storage, D: Storage> Service for SecretsClient<S, D> {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        if let Some(cached) = self.cache().get(key) {
            return Ok(cached.clone());
        }

        // A credential store that fails to answer is treated like one that
        // doesn't hold `key`: on a device without a usable one, that's every
        // read, and the secrets are in the database instead.
        let mut secure_failed = false;
        if let Some(secure) = &self.secure {
            match secure.get(key).await {
                Ok(Some(value)) => {
                    self.cache().insert(key.to_string(), Some(value.clone()));
                    return Ok(Some(value));
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        "failed to read secret {key:?} from the OS credential store, trying the local database: {err}"
                    );
                    secure_failed = true;
                }
            }
        }

        let value = self
            .database
            .get(key)
            .await
            .with_context(|| format!("failed to read secret {key:?}"))?;
        // Not cached as absent after a failed credential store read, so a
        // store that comes back later is read again.
        if value.is_some() || !secure_failed {
            self.cache().insert(key.to_string(), value.clone());
        }
        Ok(value)
    }

    async fn set(&self, key: &str, value: &str, location: SecretLocation) -> Result<()> {
        // Dropped first, so a failed write never leaves a stale cached value.
        self.cache().remove(key);
        match location {
            SecretLocation::SecureStorage => match &self.secure {
                Some(secure) => secure.set(key, value).await,
                None => Err(ErrorKind::SecureStorageUnavailable.into()),
            },
            SecretLocation::Database => self.database.set(key, value).await,
        }
        .with_context(|| format!("failed to store secret {key:?}"))?;
        self.cache()
            .insert(key.to_string(), Some(value.to_string()));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.cache().remove(key);
        let from_database =
            self.database.delete(key).await.with_context(|| {
                format!("failed to delete secret {key:?} from the local database")
            });
        let from_secure = match &self.secure {
            Some(secure) => match secure.delete(key).await {
                Ok(()) => Ok(()),
                // A store that can't be reached at all can't be holding
                // anything this SDK managed to write, so there's nothing
                // left behind to report.
                Err(_) if !self.is_secure_storage_available().await => Ok(()),
                Err(err) => Err(err).with_context(|| {
                    format!("failed to delete secret {key:?} from the OS credential store")
                }),
            },
            None => Ok(()),
        };
        from_database.and(from_secure)?;
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

    async fn is_secure_storage_available(&self) -> bool {
        match &self.secure {
            Some(secure) => secure.get(AVAILABILITY_PROBE).await.is_ok(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::super::storage::MockStorage;
    use super::super::{MASTER_KEY_SECRET, SESSION_TOKEN_SECRET};
    use super::*;
    use crate::{ErrorCode, ErrorKind};

    fn keystore_error() -> crate::Error {
        ErrorKind::Encryption("keystore locked".into()).into()
    }

    #[tokio::test]
    async fn get_returns_none_when_never_set() {
        let mut secure = MockStorage::new();
        secure
            .expect_get()
            .with(eq(MASTER_KEY_SECRET))
            .return_once(|_| Ok(None));
        let mut database = MockStorage::new();
        database
            .expect_get()
            .with(eq(MASTER_KEY_SECRET))
            .return_once(|_| Ok(None));
        let client = SecretsClient::new(Some(secure), database);

        assert_eq!(client.get(MASTER_KEY_SECRET).await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_prefers_the_secure_storage_without_reading_the_database() {
        let mut secure = MockStorage::new();
        secure
            .expect_get()
            .with(eq(MASTER_KEY_SECRET))
            .return_once(|_| Ok(Some("secure-key".to_string())));
        // No `expect_get()`: the mock panics if `get` reaches the database.
        let database = MockStorage::new();
        let client = SecretsClient::new(Some(secure), database);

        assert_eq!(
            client.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("secure-key".to_string())
        );
    }

    #[tokio::test]
    async fn get_falls_back_to_the_database_when_the_secure_storage_has_nothing() {
        let mut secure = MockStorage::new();
        secure.expect_get().return_once(|_| Ok(None));
        let mut database = MockStorage::new();
        database
            .expect_get()
            .with(eq(MASTER_KEY_SECRET))
            .return_once(|_| Ok(Some("database-key".to_string())));
        let client = SecretsClient::new(Some(secure), database);

        assert_eq!(
            client.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("database-key".to_string())
        );
    }

    #[tokio::test]
    async fn get_falls_back_to_the_database_when_the_secure_storage_fails() {
        let mut secure = MockStorage::new();
        secure.expect_get().return_once(|_| Err(keystore_error()));
        let mut database = MockStorage::new();
        database
            .expect_get()
            .return_once(|_| Ok(Some("database-key".to_string())));
        let client = SecretsClient::new(Some(secure), database);

        assert_eq!(
            client.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("database-key".to_string())
        );
    }

    #[tokio::test]
    async fn get_reads_only_the_database_without_a_secure_storage() {
        let mut database = MockStorage::new();
        database
            .expect_get()
            .return_once(|_| Ok(Some("database-key".to_string())));
        let client = SecretsClient::<MockStorage, _>::new(None, database);

        assert_eq!(
            client.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("database-key".to_string())
        );
    }

    #[tokio::test]
    async fn get_retries_the_secure_storage_after_it_failed() {
        let mut secure = MockStorage::new();
        secure
            .expect_get()
            .times(1)
            .return_once(|_| Err(keystore_error()));
        secure
            .expect_get()
            .times(1)
            .return_once(|_| Ok(Some("secure-key".to_string())));
        let mut database = MockStorage::new();
        database.expect_get().times(1).return_once(|_| Ok(None));
        let client = SecretsClient::new(Some(secure), database);

        assert_eq!(client.get(MASTER_KEY_SECRET).await.unwrap(), None);

        assert_eq!(
            client.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("secure-key".to_string())
        );
    }

    #[tokio::test]
    async fn get_reads_the_stores_only_once_per_key() {
        let mut secure = MockStorage::new();
        secure
            .expect_get()
            .with(eq(SESSION_TOKEN_SECRET))
            .times(1)
            .return_once(|_| Ok(Some("a-token".to_string())));
        let client = SecretsClient::new(Some(secure), MockStorage::new());

        client.get(SESSION_TOKEN_SECRET).await.unwrap();
        let again = client.get(SESSION_TOKEN_SECRET).await.unwrap();

        assert_eq!(again, Some("a-token".to_string()));
    }

    #[tokio::test]
    async fn set_in_secure_storage_never_touches_the_database() {
        let mut secure = MockStorage::new();
        secure
            .expect_set()
            .with(eq(SESSION_TOKEN_SECRET), eq("a-token"))
            .times(1)
            .return_once(|_, _| Ok(()));
        let client = SecretsClient::new(Some(secure), MockStorage::new());

        client
            .set(
                SESSION_TOKEN_SECRET,
                "a-token",
                SecretLocation::SecureStorage,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_in_database_never_touches_the_secure_storage() {
        let mut database = MockStorage::new();
        database
            .expect_set()
            .with(eq(MASTER_KEY_SECRET), eq("the-key"))
            .times(1)
            .return_once(|_, _| Ok(()));
        let client = SecretsClient::new(Some(MockStorage::new()), database);

        client
            .set(MASTER_KEY_SECRET, "the-key", SecretLocation::Database)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_in_secure_storage_fails_without_one() {
        let client = SecretsClient::<MockStorage, _>::new(None, MockStorage::new());

        let err = client
            .set(MASTER_KEY_SECRET, "the-key", SecretLocation::SecureStorage)
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::SecureStorageUnavailable);
    }

    #[tokio::test]
    async fn get_returns_what_set_wrote_without_reading_the_stores() {
        let mut database = MockStorage::new();
        database.expect_set().return_once(|_, _| Ok(()));
        // No `expect_get()` on either: the mocks panic if `get` reaches them.
        let client = SecretsClient::new(Some(MockStorage::new()), database);

        client
            .set(MASTER_KEY_SECRET, "the-key", SecretLocation::Database)
            .await
            .unwrap();

        assert_eq!(
            client.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("the-key".to_string())
        );
    }

    #[tokio::test]
    async fn a_failed_set_does_not_leave_the_new_value_cached() {
        let mut secure = MockStorage::new();
        secure
            .expect_set()
            .return_once(|_, _| Err(keystore_error()));
        secure.expect_get().times(1).return_once(|_| Ok(None));
        let mut database = MockStorage::new();
        database.expect_get().times(1).return_once(|_| Ok(None));
        let client = SecretsClient::new(Some(secure), database);

        assert!(
            client
                .set(MASTER_KEY_SECRET, "the-key", SecretLocation::SecureStorage)
                .await
                .is_err()
        );

        assert_eq!(client.get(MASTER_KEY_SECRET).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_deletes_every_secret_from_both_stores() {
        let mut secure = MockStorage::new();
        let mut database = MockStorage::new();
        for key in ALL_SECRETS {
            secure
                .expect_delete()
                .with(eq(*key))
                .times(1)
                .return_once(|_| Ok(()));
            database
                .expect_delete()
                .with(eq(*key))
                .times(1)
                .return_once(|_| Ok(()));
        }
        let client = SecretsClient::new(Some(secure), database);

        client.clear().await.unwrap();
    }

    #[tokio::test]
    async fn clear_still_deletes_the_other_secrets_when_one_fails() {
        let mut secure = MockStorage::new();
        secure
            .expect_delete()
            .with(eq(MASTER_KEY_SECRET))
            .times(1)
            .return_once(|_| Err(keystore_error()));
        secure
            .expect_delete()
            .with(eq(SESSION_TOKEN_SECRET))
            .times(1)
            .return_once(|_| Ok(()));
        // The store still answers, so the failed delete is reported.
        secure.expect_get().return_once(|_| Ok(None));
        let mut database = MockStorage::new();
        database.expect_delete().times(2).returning(|_| Ok(()));
        let client = SecretsClient::new(Some(secure), database);

        assert!(client.clear().await.is_err());
    }

    #[tokio::test]
    async fn clear_ignores_an_unreachable_secure_storage() {
        let mut secure = MockStorage::new();
        secure.expect_delete().returning(|_| Err(keystore_error()));
        secure.expect_get().returning(|_| Err(keystore_error()));
        let mut database = MockStorage::new();
        database.expect_delete().times(2).returning(|_| Ok(()));
        let client = SecretsClient::new(Some(secure), database);

        client.clear().await.unwrap();
    }

    #[tokio::test]
    async fn get_returns_none_after_clear_without_reading_the_stores() {
        let mut secure = MockStorage::new();
        secure.expect_set().return_once(|_, _| Ok(()));
        secure.expect_delete().returning(|_| Ok(()));
        let mut database = MockStorage::new();
        database.expect_delete().returning(|_| Ok(()));
        let client = SecretsClient::new(Some(secure), database);
        client
            .set(
                SESSION_TOKEN_SECRET,
                "a-token",
                SecretLocation::SecureStorage,
            )
            .await
            .unwrap();

        client.clear().await.unwrap();

        assert_eq!(client.get(SESSION_TOKEN_SECRET).await.unwrap(), None);
    }

    #[tokio::test]
    async fn secure_storage_is_available_when_it_answers() {
        let mut secure = MockStorage::new();
        secure
            .expect_get()
            .with(eq(AVAILABILITY_PROBE))
            .return_once(|_| Ok(None));
        let client = SecretsClient::new(Some(secure), MockStorage::new());

        assert!(client.is_secure_storage_available().await);
    }

    #[tokio::test]
    async fn secure_storage_is_unavailable_when_it_fails() {
        let mut secure = MockStorage::new();
        secure.expect_get().return_once(|_| Err(keystore_error()));
        let client = SecretsClient::new(Some(secure), MockStorage::new());

        assert!(!client.is_secure_storage_available().await);
    }

    #[tokio::test]
    async fn secure_storage_is_unavailable_when_it_could_not_be_opened() {
        let client = SecretsClient::<MockStorage, _>::new(None, MockStorage::new());

        assert!(!client.is_secure_storage_available().await);
    }
}
