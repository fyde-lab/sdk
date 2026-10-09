mod service;
mod storage;
mod storage_keystore;
mod storage_memory;

pub(crate) use storage_keystore::use_in_process_store;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sha2::{Digest, Sha256};

use crate::{ErrorContext as _, Result};

/// The secret under which the account master key's *raw, decrypted* bytes
/// are kept (JSON-encoded, see [`crate::domains::users::encode_master_key`]),
/// once `users::Service::create`/`login` have decrypted it — read back by
/// `changelog::crypto` to derive the KEK protecting every changelog event,
/// and by `users::Service::change_password` to re-wrap it. Deliberately never
/// the wrapped/encrypted form the server stores: the SDK only needs that
/// ciphertext transiently, to send over the wire.
pub(crate) const MASTER_KEY_SECRET: &str = "master_key";

/// The secret under which the session token opened by the most recent
/// `users::Service::create`/`login` call is kept, attached to every
/// authenticated gRPC call by `sessions`.
pub(crate) const SESSION_TOKEN_SECRET: &str = "session_token";

/// Every secret this SDK keeps — what [`Service::clear`] deletes. A new
/// secret must be added here, or `logout` will leave it behind.
const ALL_SECRETS: &[&str] = &[MASTER_KEY_SECRET, SESSION_TOKEN_SECRET];

/// Keeps the handful of values that would compromise the account if read off
/// the device — the raw master key and the session token — out of the local
/// SQLite database, which holds them in plaintext on disk, and in the OS's
/// native credential store instead (see `storage_keystore.rs` for which
/// store each platform uses). Crate-private on purpose, unlike
/// `settings::Service` (exposed via `Client::settings`): nothing outside the
/// SDK has any business reading these. Same `get`/`set`/`delete` shape as
/// `settings::Service`, so callers that moved from one to the other didn't
/// change how they use it. Trait methods take `&self` so implementations can
/// be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Returns the secret stored under `key`, or `None` if it was never set
    /// (or has since been deleted).
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Stores `value` under `key`, overwriting any value previously stored
    /// under it.
    async fn set(&self, key: &str, value: &str) -> Result<()>;

    /// Removes the secret stored under `key`, if any. A no-op if `key` was
    /// never set.
    async fn delete(&self, key: &str) -> Result<()>;

    /// Removes every secret this SDK keeps (see [`ALL_SECRETS`]), used by
    /// `users::Service::logout` to leave nothing of the account behind.
    async fn clear(&self) -> Result<()>;
}

/// Selects where [`init`] keeps secrets.
pub(crate) enum StorageConfig<'a> {
    /// The OS's native credential store, namespaced to the local database
    /// at this path — so two `Client`s backed by two different databases
    /// (e.g. two profiles) never read or overwrite each other's secrets.
    /// Moving the database file therefore leaves its secrets behind, which
    /// just means logging in again.
    Keystore { database_path: &'a Path },
    /// Process memory only, gone when the process exits — the counterpart
    /// of `Storage::Memory`'s in-memory database, so an ephemeral client
    /// never touches the OS's credential store at all.
    Memory,
}

/// Initializes the secrets service on the backend `config` selects.
pub(crate) fn init(config: StorageConfig<'_>) -> Result<Arc<dyn Service>> {
    Ok(match config {
        StorageConfig::Keystore { database_path } => {
            let namespace = namespace_for(database_path)?;
            Arc::new(service::SecretsClient::new(
                storage_keystore::KeystoreStorage::new(&namespace)
                    .context("failed to open the OS credential store")?,
            ))
        }
        StorageConfig::Memory => Arc::new(service::SecretsClient::new(
            storage_memory::MemoryStorage::default(),
        )),
    })
}

/// A stable identifier for the database at `database_path`: the first 16
/// bytes of the SHA-256 of its canonical absolute path, hex-encoded — short
/// enough for every native store's name limits, and with no path separators
/// or other characters any of them might reject.
fn namespace_for(database_path: &Path) -> Result<String> {
    let canonical = database_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve local database path {}",
            database_path.display()
        )
    })?;

    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    Ok(digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_for_is_stable_for_the_same_database() {
        let file = tempfile::NamedTempFile::new().unwrap();

        assert_eq!(
            namespace_for(file.path()).unwrap(),
            namespace_for(file.path()).unwrap()
        );
    }

    #[test]
    fn namespace_for_differs_between_databases() {
        let a = tempfile::NamedTempFile::new().unwrap();
        let b = tempfile::NamedTempFile::new().unwrap();

        assert_ne!(
            namespace_for(a.path()).unwrap(),
            namespace_for(b.path()).unwrap()
        );
    }

    #[test]
    fn namespace_for_is_32_hex_characters() {
        let file = tempfile::NamedTempFile::new().unwrap();

        let namespace = namespace_for(file.path()).unwrap();

        assert_eq!(namespace.len(), 32);
        assert!(namespace.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn init_memory_keeps_secrets_for_the_process_lifetime() {
        let secrets = init(StorageConfig::Memory).unwrap();

        secrets.set(MASTER_KEY_SECRET, "the-key").await.unwrap();

        assert_eq!(
            secrets.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("the-key".to_string())
        );
    }
}
