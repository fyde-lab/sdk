use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use uuid::Uuid;

use crate::Result;

/// Tracks the SDK's cursor position into the server's changelog (persisted
/// in the local settings store — see
/// [`super::storage_settings::SettingsCursorStorage`]), so
/// [`super::Service::consume`] can resume from where it left off across
/// restarts instead of requiring the caller to track an id itself.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait CursorStorage: Send + Sync {
    /// Returns the last cursor persisted by [`Self::save_cursor`], or
    /// [`Uuid::nil`] if the changelog has never been consumed — the id
    /// [`super::Service::consume`] passes to `ConsumeSince` to mean "from
    /// the beginning".
    async fn get_cursor(&self) -> Result<Uuid>;

    /// Persists `id` as the cursor to resume from on the next
    /// [`super::Service::consume`] call.
    async fn save_cursor(&self, id: Uuid) -> Result<()>;
}
