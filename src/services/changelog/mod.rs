mod crypto;
mod grpc_client;
mod service;
mod storage;
mod storage_sqlite;

pub use service::{ChangelogEvent, EventType};

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::services::documents::{self, Metadata};
use crate::session::SessionTokenStore;
use crate::{ErrorContext as _, Result};

/// Publishes and consumes the fyde server's changelog: a blind relay for
/// opaque, client-encrypted document write events. Trait methods take
/// `&self` (not `&mut self`) so implementations can be shared behind
/// `Arc<dyn Service>`. Not `automock`-able (mockall doesn't support `Fn`
/// trait-object parameters — see `consume`), so tests use hand-written
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

    /// Streams and decrypts every event since the last consumed offset,
    /// oldest first (replaying persisted history, then continuing with the
    /// live tail — see `ConsumeSince` in `changelog.proto`), resuming from
    /// the cursor persisted locally in the `changelog_offset` table (from
    /// the very beginning of the changelog if it's never been consumed
    /// before). For each event carrying both `content` and `metadata`
    /// (currently only `Created` events), caches the resulting document in
    /// local storage before invoking `callback`; the offset cursor is
    /// persisted as each event is processed, so a later call resumes right
    /// after the last event seen.
    ///
    /// Runs until the server closes the stream or an error occurs.
    async fn consume(&self, callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()>;
}

/// Initializes the changelog service: connects to the fyde server at
/// `base_url`, and uses `pool` to cache documents materialized from
/// consumed events directly into the local `documents` table, and to track
/// the changelog offset consumed so far. `tokens` is attached as a bearer
/// `authorization` header on every outgoing call once a session is opened
/// (see [`crate::session::AuthInterceptor`]).
pub(crate) async fn init(
    base_url: impl AsRef<str>,
    pool: SqlitePool,
    tokens: SessionTokenStore,
) -> Result<Arc<dyn Service>> {
    let document_storage = documents::SqliteStorage::new(pool.clone());
    let offset_storage = storage_sqlite::SqliteOffsetStorage::new(pool);
    let client = service::ChangelogClient::new(base_url, document_storage, offset_storage, tokens)
        .await
        .context("failed to create changelog client")?;

    Ok(Arc::new(client))
}
