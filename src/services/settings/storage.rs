use crate::Result;

/// Persists key/value settings pairs. Implementations are injected into
/// [`super::service::SettingsClient`] at construction.
pub(crate) trait Storage: Send + Sync {
    /// Fetches the value stored under `key`, or `None` if it has never
    /// been set.
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Persists `value` under `key`, overwriting any value previously
    /// stored under it.
    fn set(&self, key: &str, value: &str) -> impl Future<Output = Result<()>> + Send;

    /// Removes the value stored under `key`, if any.
    fn delete(&self, key: &str) -> impl Future<Output = Result<()>> + Send;
}
