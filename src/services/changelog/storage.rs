use crate::Result;

/// Persists and retrieves the local cursor into the server's changelog, so
/// [`super::Service::consume`] can resume from where it left off.
/// Implementations are injected into [`super::ChangelogClient`] at
/// construction.
pub(super) trait Storage: Send + Sync {
    /// Reads the persisted changelog cursor, returning 0 (the beginning of
    /// the changelog) if no offset has been recorded yet.
    fn read_offset(&self) -> impl Future<Output = Result<i64>> + Send;

    /// Persists the changelog cursor.
    fn write_offset(&self, offset: i64) -> impl Future<Output = Result<()>> + Send;
}
