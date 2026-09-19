mod crypto;
mod grpc_client;
mod service;

pub use service::{ChangelogEvent, EventType};

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::services::documents::{self, Metadata};
use crate::{ErrorContext as _, Result};

/// Publishes and consumes the fyde server's changelog: a blind relay for
/// opaque, client-encrypted document write events. Trait methods take
/// `&self` (not `&mut self`) so implementations can be shared behind
/// `Arc<dyn Service>`. Not `automock`-able (mockall doesn't support `Fn`
/// trait-object parameters — see `consume_since`), so tests use hand-written
/// fakes instead.
#[async_trait]
pub trait Service: Send + Sync {
    /// Encrypts and publishes a new event. `content`/`metadata` are `None`
    /// for event types that don't carry them (e.g. a future `Deleted`
    /// event only carries `document_id`).
    async fn send(
        &self,
        event_type: EventType,
        document_id: uuid::Uuid,
        content: Option<&[u8]>,
        metadata: Option<&Metadata>,
    ) -> Result<()>;

    /// Streams and decrypts every event from `offset` onward, oldest first
    /// (replaying persisted history, then continuing with the live tail —
    /// see `ConsumeSince` in `changelog.proto`). For each event carrying
    /// both `content` and `metadata` (currently only `Created` events),
    /// caches the resulting document in local storage before invoking
    /// `callback`.
    ///
    /// Runs until the server closes the stream or an error occurs.
    async fn consume_since(
        &self,
        offset: i64,
        callback: Box<dyn FnMut(ChangelogEvent) + Send>,
    ) -> Result<()>;
}

/// Initializes the changelog service: connects to the fyde server at
/// `base_url`, and uses `pool` to cache documents materialized from
/// consumed events directly into the local `documents` table.
pub(crate) async fn init(base_url: impl AsRef<str>, pool: SqlitePool) -> Result<Arc<dyn Service>> {
    let document_storage = documents::SqliteStorage::new(pool);
    let client = service::ChangelogClient::new(base_url, document_storage)
        .await
        .context("failed to create changelog client")?;

    Ok(Arc::new(client))
}
