mod grpc_client;
mod models;
mod service;
mod storage;
mod storage_sqlite;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::Result;
use crate::domains::sessions::SessionsClient;

#[cfg(test)]
pub(crate) use models::FakeIcon;
pub use models::Icon;

/// The images the application shows for other entities (e.g. a script's
/// icon in a listing), stored by the server's `icons.v1.IconsService` under
/// their own id, which those entities carry instead of the image. An icon
/// never changes once created, so this service caches every icon it
/// fetches or creates in the local database for good: an icon is only ever
/// downloaded once per device.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Stores `content` (an encoded image, e.g. a PNG) on the server as an
    /// icon, and caches it. Storing an image the server already has returns
    /// the existing icon. Admin-only: fails for any other user (the
    /// server's `PERMISSION_DENIED`).
    async fn create_icon(&self, content: Vec<u8>) -> Result<Icon>;

    /// The icon matching `id`: from the local cache if it's there,
    /// otherwise fetched from the server and cached.
    async fn fetch_icon(&self, id: Uuid) -> Result<Icon>;
}

/// Initializes the icons service: talks to the fyde server's icons service
/// over the shared `channel` connection, authenticating every call via
/// `sessions`, and caches icons in the local SQLite database (`pool`).
pub(crate) fn init(
    channel: Channel,
    sessions: Arc<SessionsClient>,
    pool: SqlitePool,
) -> Arc<dyn Service> {
    Arc::new(service::IconsClient::new(
        grpc_client::GrpcClient::new(channel, sessions),
        storage_sqlite::SqliteStorage::new(pool),
    ))
}
