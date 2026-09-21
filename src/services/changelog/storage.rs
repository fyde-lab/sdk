use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

/// Tracks the SDK's cursor position into the server's changelog (persisted
/// in the local settings store — see
/// [`super::storage_settings::SettingsOffsetStorage`]), so
/// [`super::Service::consume`] can resume from where it left off across
/// restarts instead of requiring the caller to track an offset itself.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait OffsetStorage: Send + Sync {
    /// Returns the last offset persisted by [`Self::save_offset`], or `0`
    /// if the changelog has never been consumed.
    async fn get_offset(&self) -> Result<i64>;

    /// Persists `offset` as the cursor to resume from on the next
    /// [`super::Service::consume`] call.
    async fn save_offset(&self, offset: i64) -> Result<()>;
}
