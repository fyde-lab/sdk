use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

use super::models::Report;

/// Persists a completed scraper run's debug report. Implementations are
/// injected into [`super::service::ReportsClient`] at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Saves `report`. Each run produces its own report, so unlike
    /// `cookies`/`session`'s storage this never replaces a previously saved
    /// one for the same scraper name — every call inserts a new report.
    async fn save(&self, report: &Report) -> Result<()>;
}
