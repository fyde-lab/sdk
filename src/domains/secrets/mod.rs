mod service;
mod storage;
mod storage_keystore;
mod storage_memory;
mod storage_sqlite;

pub(crate) use storage_keystore::use_in_process_store;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

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

/// Where [`Service::set`] writes a secret. [`Service::get`] doesn't take
/// one: it always looks in the OS credential store first, then in the local
/// database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecretLocation {
    /// The OS's native credential store — the only place a secret goes
    /// unless the user explicitly accepted otherwise.
    SecureStorage,
    /// The local SQLite database, in plaintext: the fallback for a device
    /// with no usable credential store, which the user must have opted into
    /// (see `users::Service::create`'s `allow_insecure_storage`).
    Database,
}

/// Keeps the handful of values that would compromise the account if read off
/// the device — the raw master key and the session token — out of the local
/// SQLite database, which holds them in plaintext on disk, and in the OS's
/// native credential store instead (see `storage_keystore.rs` for which
/// store each platform uses) — unless the device has no usable credential
/// store and the user accepted keeping them in the database anyway (see
/// [`SecretLocation::Database`]). Crate-private on purpose, unlike
/// `settings::Service` (exposed via `Client::settings`): nothing outside the
/// SDK has any business reading these. Same `get`/`set`/`delete` shape as
/// `settings::Service`, so callers that moved from one to the other didn't
/// change how they use it. Trait methods take `&self` so implementations can
/// be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Returns the secret stored under `key` — looked up in the OS
    /// credential store first, then in the local database if it isn't
    /// there (or the credential store can't be reached) — or `None` if it
    /// was never set (or has since been deleted).
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Stores `value` under `key` at `location`, overwriting any value
    /// previously stored under it there. Fails with
    /// [`crate::ErrorCode::SecureStorageUnavailable`] for
    /// [`SecretLocation::SecureStorage`] if this device has no OS credential
    /// store at all.
    async fn set(&self, key: &str, value: &str, location: SecretLocation) -> Result<()>;

    /// Removes the secret stored under `key` from both locations, if any. A
    /// no-op if `key` was never set.
    async fn delete(&self, key: &str) -> Result<()>;

    /// Returns whether the OS credential store can currently be used, i.e.
    /// whether [`SecretLocation::SecureStorage`] is an option on this
    /// device. Never fails: an unreachable store just reports `false`.
    async fn is_secure_storage_available(&self) -> bool;

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

/// Initializes the secrets service on the secure backend `config` selects,
/// falling back to the `insecure_secrets` table of the local database behind
/// `pool` (see [`SecretLocation::Database`]). An OS credential store that
/// can't even be opened (e.g. no D-Bus session on a headless Linux box) is
/// not an error here — it only makes [`SecretLocation::SecureStorage`]
/// unavailable, which `users::Service::create` reports to the user.
pub(crate) fn init(config: StorageConfig<'_>, pool: SqlitePool) -> Result<Arc<dyn Service>> {
    let database = storage_sqlite::SqliteStorage::new(pool);
    Ok(match config {
        StorageConfig::Keystore { database_path } => {
            let namespace = namespace_for(database_path)?;
            let keystore = storage_keystore::KeystoreStorage::new(&namespace)
                .inspect_err(|err| {
                    tracing::warn!("OS credential store unavailable: {err}");
                })
                .ok();
            Arc::new(service::SecretsClient::new(keystore, database))
        }
        StorageConfig::Memory => Arc::new(service::SecretsClient::new(
            Some(storage_memory::MemoryStorage::default()),
            database,
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
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let secrets = init(StorageConfig::Memory, pool).unwrap();

        secrets
            .set(MASTER_KEY_SECRET, "the-key", SecretLocation::SecureStorage)
            .await
            .unwrap();

        assert_eq!(
            secrets.get(MASTER_KEY_SECRET).await.unwrap(),
            Some("the-key".to_string())
        );
    }
}
