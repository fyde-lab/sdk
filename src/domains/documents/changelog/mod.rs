mod crypto;
mod grpc_client;
mod models;
mod service;
mod storage;
mod storage_settings;

#[cfg(test)]
pub(crate) use models::FakeChangelogEvent;
pub use models::{ChangelogEvent, EventType};

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use tonic::transport::Channel;

use crate::domains::documents::{self, Metadata};
use crate::domains::server_state::Service as ServerStateService;
use crate::domains::sessions::SessionsClient;
use crate::domains::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

/// Publishes and consumes the fyde server's changelog: a blind relay for
/// opaque, client-encrypted document write events. Trait methods take
/// `&self` (not `&mut self`) so implementations can be shared behind
/// `Arc<dyn Service>`. Not `automock`-able (mockall doesn't support `Fn`
/// trait-object parameters — see `consume`), so tests use hand-written
/// fakes instead.
#[async_trait]
pub(super) trait Service: Send + Sync {
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

    /// Streams and decrypts every event since the last consumed id, oldest
    /// first (replaying persisted history, then continuing with the live
    /// tail — see `ConsumeSince` in `changelog.proto`), resuming from the
    /// cursor persisted locally in the settings store (from the very
    /// beginning of the changelog if it's never been consumed before). For
    /// each `Created` event, caches the resulting document in
    /// local storage before invoking `callback`; the cursor is persisted
    /// as each event is processed, so a later call resumes right after the
    /// last event seen.
    ///
    /// Never returns. If a pass over the stream fails — including with
    /// [`crate::Error::InvalidChangelogEvent`] if a `Created` event is
    /// missing its `content` or `metadata`, since a `Created` event must
    /// carry both — the error is logged and consumption is retried
    /// immediately from the persisted cursor: the next pass's wait for
    /// [`crate::domains::server_state::Service::is_server_reachable`]
    /// naturally paces the retries if the failure was caused by the server
    /// being unreachable. If a pass ends cleanly (the server closes the
    /// stream), it's also retried immediately.
    async fn consume(&self, callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()>;
}

/// Initializes the changelog service: connects to the fyde server over the
/// shared `channel`, and uses `pool` to cache documents materialized from
/// consumed events directly into the local `documents` table, `settings`
/// to persist the changelog cursor consumed so far (see
/// [`storage_settings::SettingsCursorStorage`]) and to read the account
/// master key `crypto` derives every event's KEK from, and `sessions` to
/// attach the session token as a bearer `authorization` header on every
/// outgoing call once a session is opened (see
/// [`crate::domains::sessions::Service::authenticated_request`]).
pub(super) async fn init(
    channel: Channel,
    pool: SqlitePool,
    settings: Arc<dyn SettingsService>,
    sessions: Arc<SessionsClient>,
    server_state: Arc<dyn ServerStateService>,
) -> Result<Arc<dyn Service>> {
    let document_storage = documents::SqliteStorage::new(pool);
    let cursor_storage = storage_settings::SettingsCursorStorage::new(settings.clone());
    let client = service::ChangelogClient::new(
        channel,
        document_storage,
        cursor_storage,
        sessions,
        server_state,
        settings,
    )
    .await
    .context("failed to create changelog client")?;

    Ok(Arc::new(client))
}
