use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

/// Persists key/value settings pairs. Implementations are injected into
/// [`super::service::SettingsClient`] at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Fetches the value stored under `key`, or `None` if it has never
    /// been set.
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Persists `value` under `key`, overwriting any value previously
    /// stored under it.
    async fn set(&self, key: &str, value: &str) -> Result<()>;

    /// Removes the value stored under `key`, if any.
    async fn delete(&self, key: &str) -> Result<()>;
}
