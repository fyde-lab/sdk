mod grpc_client;
mod service;
mod storage;
mod storage_sqlite;

pub use service::{ChangelogClient, ChangelogEvent};
pub use storage::Storage;
pub use storage_sqlite::SqliteStorage;

use async_trait::async_trait;

use crate::Result;

/// Subscribes to the fyde server's changelog, keeping a local cursor
/// persisted via an injected [`Storage`] up to date. Trait methods take
/// `&self` (not `&mut self`) so implementations can be shared behind
/// `Arc<dyn Service>`.
#[async_trait]
pub trait Service: Send + Sync {
    /// Opens a subscription to live changelog events and, for each one
    /// received, catches up on everything recorded since the offset
    /// persisted in storage, advancing that offset afterwards.
    ///
    /// `callback` is invoked once for every [`ChangelogEvent`] encountered,
    /// during both the initial catch-up and the live subscription. Boxed
    /// (rather than generic) so the trait stays object-safe for
    /// `Arc<dyn Service>`.
    ///
    /// Runs until the server closes the watch stream or an error occurs.
    async fn consume(&self, callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()>;
}
