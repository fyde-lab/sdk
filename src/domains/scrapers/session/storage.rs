use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use serde_json::Value;

use crate::Result;

/// Persists a scraper's session data. Implementations are injected into
/// [`super::service::SessionClient`] at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Returns the session data previously saved for `scraper_name`, or
    /// `None` if it's never been saved.
    async fn get(&self, scraper_name: &str) -> Result<Option<Value>>;

    /// Replaces the session data saved for `scraper_name`.
    async fn set(&self, scraper_name: &str, data: Value) -> Result<()>;
}
