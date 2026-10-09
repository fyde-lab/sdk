use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

use super::models::Cookie;

/// Persists a scraper's cookie jar. Implementations are injected into
/// [`super::service::CookiesClient`] at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Returns every cookie previously saved for `scraper_name`, or an empty
    /// list if none have been saved yet.
    async fn list(&self, scraper_name: &str) -> Result<Vec<Cookie>>;

    /// Replaces every cookie previously saved for `scraper_name` with
    /// `cookies`.
    async fn replace_all(&self, scraper_name: &str, cookies: Vec<Cookie>) -> Result<()>;

    /// Deletes every cookie saved for `scraper_name`. Not an error if none
    /// were saved.
    async fn delete_all(&self, scraper_name: &str) -> Result<()>;
}
