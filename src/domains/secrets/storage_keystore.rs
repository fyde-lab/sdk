use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use keyring_core::{CredentialStore, Entry};

use crate::{Error, ErrorContext as _, Result};

use super::storage::Storage;

/// A [`Storage`] backed by the OS's native credential store, via
/// `keyring-core`:
///
/// - macOS: the user's login Keychain.
/// - iOS: the app's data-protection Keychain (its default access group).
/// - Windows: the Credential Manager.
/// - Android: an Android Keystore-backed `SharedPreferences` vault. The app
///   must hand this library its JNI context once at startup, before the
///   first `Client::init`: this crate's shared library exports
///   `Java_io_crates_keyring_Keyring_00024Companion_initializeNdkContext`
///   (from `android-native-keyring-store`) for exactly that — see the sdk
///   CLAUDE.md's `secrets` section.
/// - Linux and other desktop Unixes: the freedesktop Secret Service (GNOME
///   Keyring, KWallet, ...) over D-Bus. A headless machine without one
///   running fails here rather than silently falling back to plaintext.
///
/// Every secret lives under the service name `fyde-sdk.<namespace>` (see
/// `super::namespace_for`), with the secret's key as the user name. Every
/// call runs on a blocking thread: the native stores are synchronous and
/// some (D-Bus, JNI) do real I/O.
pub(super) struct KeystoreStorage {
    store: Arc<CredentialStore>,
    service: String,
}

impl KeystoreStorage {
    pub(super) fn new(namespace: &str) -> Result<Self> {
        Ok(Self::with_store(native_store()?, namespace))
    }

    fn with_store(store: Arc<CredentialStore>, namespace: &str) -> Self {
        Self {
            store,
            service: format!("fyde-sdk.{namespace}"),
        }
    }

    /// Builds the entry for `key` and runs `op` on it, on a blocking thread.
    async fn with_entry<T: Send + 'static>(
        &self,
        key: &str,
        op: impl FnOnce(Entry) -> keyring_core::Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        let service = self.service.clone();
        let key = key.to_string();
        let result = tokio::task::spawn_blocking(move || op(store.build(&service, &key, None)?))
            .await
            .context("credential store thread panicked")?;
        Ok(result?)
    }
}

#[async_trait]
impl Storage for KeystoreStorage {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        self.with_entry(key, |entry| match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(err) => Err(err),
        })
        .await
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        let value = value.to_string();
        self.with_entry(key, move |entry| entry.set_password(&value))
            .await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.with_entry(key, |entry| match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(err) => Err(err),
        })
        .await
    }
}

/// The process-wide credential store, created on first use and shared by
/// every `Client` afterwards — some stores (Android's) refuse a second
/// instance of the same store within one process.
static STORE: Mutex<Option<Arc<CredentialStore>>> = Mutex::new(None);

/// Replaces the OS credential store with an in-process one
/// (`keyring-core`'s mock store) for every `Client` created afterwards in
/// this process: secrets still outlive any one `Client` — so a test can
/// drop one and re-`init` another against the same database, like an app
/// restart — but never the process, and never touch the machine's real
/// keychain. Only for tests (see `tests/reconnect_lifecycle_test.rs`).
pub(crate) fn use_in_process_store() -> Result<()> {
    let store: Arc<CredentialStore> = keyring_core::mock::Store::new()?;
    *STORE.lock().unwrap_or_else(|p| p.into_inner()) = Some(store);
    Ok(())
}

/// The process-wide credential store (see [`STORE`]): the OS's native one,
/// unless [`use_in_process_store`] swapped it out.
fn native_store() -> Result<Arc<CredentialStore>> {
    let mut store = STORE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(store) = store.as_ref() {
        return Ok(store.clone());
    }
    let created = new_native_store().map_err(Error::from)?;
    *store = Some(created.clone());
    Ok(created)
}

#[cfg(target_os = "macos")]
fn new_native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(target_os = "ios")]
fn new_native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(apple_native_keyring_store::protected::Store::new()?)
}

#[cfg(target_os = "windows")]
fn new_native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(windows_native_keyring_store::Store::new()?)
}

#[cfg(target_os = "android")]
fn new_native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(android_native_keyring_store::Store::new()?)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn new_native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(dbus_secret_service_keyring_store::Store::new()?)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn new_native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Err(keyring_core::Error::NotSupportedByStore(
        "no native credential store is available on this platform".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`KeystoreStorage`] over `keyring-core`'s in-memory mock store —
    /// exercising this adapter's real `keyring-core` calls (entry naming,
    /// `NoEntry` handling) without touching the machine's actual keychain.
    fn storage(namespace: &str, store: Arc<CredentialStore>) -> KeystoreStorage {
        KeystoreStorage::with_store(store, namespace)
    }

    fn mock_store() -> Arc<CredentialStore> {
        keyring_core::mock::Store::new().unwrap()
    }

    #[tokio::test]
    async fn get_returns_none_when_never_set() {
        let storage = storage("ns", mock_store());

        assert_eq!(storage.get("master_key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_returns_what_set_stored() {
        let storage = storage("ns", mock_store());

        storage.set("master_key", "the-key").await.unwrap();

        assert_eq!(
            storage.get("master_key").await.unwrap(),
            Some("the-key".to_string())
        );
    }

    #[tokio::test]
    async fn delete_removes_the_secret_and_tolerates_a_missing_one() {
        let storage = storage("ns", mock_store());
        storage.set("master_key", "the-key").await.unwrap();

        storage.delete("master_key").await.unwrap();
        storage.delete("master_key").await.unwrap();

        assert_eq!(storage.get("master_key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn secrets_in_different_namespaces_never_collide() {
        let store = mock_store();
        let a = storage("a", store.clone());
        let b = storage("b", store);

        a.set("master_key", "a's key").await.unwrap();

        assert_eq!(b.get("master_key").await.unwrap(), None);
    }
}
