//! The sync log every write goes through. The server is a blind relay of
//! client-encrypted events (`crypto.rs`: AES-256-GCM, a per-event DEK wrapped
//! by a KEK derived from the master key); every device rebuilds its local
//! state by consuming them.
//!
//! The consume job replays from the persisted cursor, then tails live,
//! materializing documents (through `documents::Indexer`), folders (through
//! `folders::Applier`) and script installations, one event per action (see
//! [`EventType`]). An installation event for another script version than
//! the one saved first deletes that installation's saved scraper session,
//! so the new version refetches everything; a `ScriptUninstalled` deletes
//! its sessions, cookies and reports too.
//!
//! Each event's `previous_id` (the server chains a user's events) must equal
//! the last event the pass went through, checked before decrypting: otherwise
//! the pass fails rather than skip past a gap or apply another account's
//! event. Applied events are recorded by ciphertext digest, so one the server
//! serves again is skipped. Before a pass's first document event the job calls
//! `Indexer::prepare`, so a missing model fails the pass rather than letting
//! the cursor pass an unindexed document. A failed pass is retried after a
//! backoff; a cleanly ended stream is reopened after a pause, never at once.
//! An event failing `POISON_PILL_PASSES` passes in a row is logged
//! at error level, but never skipped.
//!
//! [`SyncProgress`] counts events (not documents: the server can't tell them
//! apart) gone through since the job started, against the server's
//! `CountSince` from each pass's cursor. A failed count leaves `total` as it
//! was and never fails the pass.

mod crypto;
mod grpc_client;
mod models;
mod service;
mod storage;
mod storage_settings;

#[cfg(any(test, feature = "testing"))]
pub use models::FakeChangelogEvent;
pub use models::{ChangelogEvent, EventType, SyncProgress};

/// The largest encrypted event [`Service::send`] publishes, in bytes. The
/// server decodes requests with tonic's default 4 MiB message limit, so a
/// larger `RecordEvent` would only fail on the wire, with a transport error
/// the user can't make sense of; 1 KiB of that is left for the request's
/// own framing.
pub(crate) const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024 - 1024;

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use tonic::transport::Channel;

use crate::Result;
use crate::domains::documents::{self, Metadata};
use crate::domains::secrets::Service as SecretsService;
use crate::domains::server_state::Service as ServerStateService;
use crate::domains::sessions::SessionsClient;
use crate::domains::settings::Service as SettingsService;
use crate::domains::{folders, scrapers, scripts};

/// Publishes and consumes the fyde server's changelog: a blind relay for
/// opaque, client-encrypted write events — document writes, plus script
/// installations and folders, one event per action (see [`EventType`]).
/// `send`/`stop_consume_job`
/// take `&self` so implementations can be shared behind `Arc<dyn Service>`;
/// `start_consume_job` takes `self: Arc<Self>` since it spawns the consume
/// loop onto its own background task, which needs an owned, `'static`
/// handle on `self` to outlive the call that started it. Not `automock`-able
/// (mockall doesn't support `Fn` trait-object parameters — see
/// `start_consume_job`), so tests use hand-written fakes instead.
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Fails with the same error [`Service::send`] would eventually hit
    /// while encrypting, if no master key is on hand yet — i.e. before any
    /// account has been created or logged into on this device — without
    /// actually encrypting anything. Exposed so a caller doing unrelated
    /// authenticated work before it ever reaches `send` (e.g.
    /// `documents::Service::upload` listing a user's enabled scripts) can
    /// fail fast with this specific, recognizable error instead of
    /// whatever that other work happens to fail with first.
    async fn ensure_master_key(&self) -> Result<()>;

    /// Encrypts and publishes a new event about `subject_id` — a document
    /// or script id depending on `event_type` (see [`ChangelogEvent`]).
    /// `content`/`metadata` are `None` for event types that don't carry
    /// them (e.g. a `Deleted` event only carries `subject_id`).
    async fn send(
        &self,
        event_type: EventType,
        subject_id: uuid::Uuid,
        content: Option<&[u8]>,
        metadata: Option<&Metadata>,
    ) -> Result<()>;

    /// Spawns a background task that streams and decrypts every event since
    /// the last consumed id, oldest first (replaying persisted history,
    /// then continuing with the live tail — see `ConsumeSince` in
    /// `changelog.proto`), resuming from the cursor persisted locally in
    /// the settings store (from the very beginning of the changelog if it's
    /// never been consumed before). For each `DocumentUploaded` event, caches the
    /// resulting document in local storage before invoking `callback`
    /// (replaces its metadata for a `DocumentRenamed`,
    /// `DocumentAttributesUpdated` or `DocumentMoved` event, removes it for
    /// a `DocumentDeleted` one, and likewise saves the installation for
    /// `ScriptInstalled` and the other `Installation*` events,
    /// via the `scripts` domain's local storage, first clearing that
    /// installation's saved scraper session if the event installs another
    /// version of the script than the one installed so far, and deletes the
    /// installation and all its scraper state for a `ScriptUninstalled`); the
    /// cursor is persisted as each event is processed, so a later call
    /// resumes right after the last event seen.
    ///
    /// Returns as soon as the task is spawned, not when consumption stops.
    /// The spawned task never exits on its own; starting a job ends any job
    /// still running, so only one task ever consumes. If a pass over the stream
    /// fails — including with [`crate::ErrorCode::InvalidChangelogEvent`] if a
    /// `DocumentUploaded` event is missing its `content` or `metadata`, since a
    /// `DocumentUploaded` event must carry both, or if an installation event's
    /// `content` is missing, isn't a valid installation or names another
    /// installation than its subject — the error is logged and
    /// consumption is retried from the persisted cursor after a backoff
    /// that doubles with each failure in a row (500 ms up to 30 s), the
    /// next pass also waiting for
    /// [`crate::domains::server_state::Service::is_server_reachable`]. If a
    /// pass ends cleanly (the server closes the stream), the stream is
    /// reopened after a 1 s pause, never at once.
    async fn start_consume_job(
        self: Arc<Self>,
        callback: Box<dyn FnMut(ChangelogEvent) + Send>,
    ) -> Result<()>;

    /// Signals a running [`Self::start_consume_job`] task to return.
    /// Idempotent, and safe to call before [`Self::start_consume_job`] has
    /// started its task (in which case that task returns immediately
    /// without ever opening a stream) or after it has already returned (a
    /// no-op). Interrupts the consume loop promptly even while it's parked
    /// awaiting the next event on an otherwise idle stream, rather than
    /// only being checked between passes.
    fn stop_consume_job(&self);

    /// How far the job [`Self::start_consume_job`] started last has got
    /// (see [`SyncProgress`]): reset when a job starts, its total counted by
    /// the server at the start of each pass. A count that fails is logged
    /// and leaves the total as it was, never failing the pass.
    fn sync_progress(&self) -> SyncProgress;
}

/// Initializes the changelog service: connects to the fyde server over the
/// shared `channel`, and uses `indexer` to cache (and index) the documents
/// materialized from consumed events, `pool` to spot the ones already
/// cached (and to save installed
/// script installations into the `scripts` domain's `installed_scripts`
/// table, clearing an installation's `scraper_sessions` row when its
/// script changes version and all its scraper state when it's
/// uninstalled), `settings`
/// to persist the changelog cursor consumed so far (see
/// [`storage_settings::LocalCursorStorage`]), `secrets` to read the
/// account master key `crypto` derives every event's KEK from, and `sessions` to
/// attach the session token as a bearer `authorization` header on every
/// outgoing call once a session is opened (see
/// [`crate::domains::sessions::Service::authenticated_request`]). The
/// caller must make sure no other client writes the database `pool` opens
/// (see `Client::init`'s instance lock).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn init(
    channel: Channel,
    pool: SqlitePool,
    settings: Arc<dyn SettingsService>,
    secrets: Arc<dyn SecretsService>,
    sessions: Arc<SessionsClient>,
    server_state: Arc<dyn ServerStateService>,
    indexer: Arc<dyn documents::Indexer>,
    folders: Arc<dyn folders::Applier>,
) -> Result<Arc<dyn Service>> {
    let document_storage = documents::SqliteStorage::new(pool.clone());
    let script_storage = scripts::SqliteStorage::new(pool.clone());
    let scraper_state_storage = scrapers::StateSqliteStorage::new(pool.clone());
    let cursor_storage = storage_settings::LocalCursorStorage::new(settings, pool);
    let client = service::ChangelogClient::new(
        grpc_client::GrpcClient::new(channel, sessions),
        document_storage,
        indexer,
        script_storage,
        scraper_state_storage,
        folders,
        cursor_storage,
        server_state,
        secrets,
    );

    Ok(Arc::new(client))
}
