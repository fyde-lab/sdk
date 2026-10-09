use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

/// Persistence for [`super::Service`]: a plain key/value store, no business
/// rules. Implemented by `storage_keystore::KeystoreStorage` (the OS's native
/// credential store) and `storage_memory::MemoryStorage` (process memory).
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait Storage: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    /// A no-op, not an error, if nothing is stored under `key`.
    async fn delete(&self, key: &str) -> Result<()>;
}
