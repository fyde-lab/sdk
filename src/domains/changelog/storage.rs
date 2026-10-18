use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use uuid::Uuid;

use crate::Result;

/// Tracks the SDK's progress through the server's changelog (see
/// [`super::storage_settings::LocalCursorStorage`]): the cursor, so
/// [`super::Service::consume`] can resume from where it left off across
/// restarts instead of requiring the caller to track an id itself, and the
/// events already applied, so a replayed one is recognised.
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

    /// Whether an event whose encrypted content hashes to `digest` (SHA-256)
    /// was already recorded by [`Self::mark_applied`]. The server can't forge
    /// or re-encrypt an event, but it can serve an old one again under a new
    /// id, byte for byte: that's how a replay is told apart.
    async fn is_applied(&self, digest: &[u8; 32]) -> Result<bool>;

    /// Records that the event whose encrypted content hashes to `digest` was
    /// applied (see [`Self::is_applied`]).
    async fn mark_applied(&self, digest: &[u8; 32]) -> Result<()>;
}
