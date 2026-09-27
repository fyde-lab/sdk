mod grpc_client;
mod models;
mod service;
mod vm;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;
use uuid::Uuid;

#[cfg(test)]
pub(crate) use models::FakeScript;
pub use models::Script;

use crate::Result;
use crate::domains::documents::Document;
use crate::domains::sessions::SessionsClient;

/// Runs Lua scripts against a document inside a fully sandboxed VM (see
/// `service.rs`), and manages user-authored scripts against the fyde
/// server's scripts service (see `grpc_client.rs`): creating them, fetching
/// them by id, enabling/disabling them for the authenticated user, and
/// listing the ones currently enabled. Trait methods take `&self` (not
/// `&mut self`) so implementations can be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Fetches the scripts currently enabled for the authenticated user and
    /// runs each of them, in its own freshly-constructed sandboxed Lua VM,
    /// scoped to `document`.
    async fn run_for_document(&self, document: &Document) -> Result<()>;

    /// Creates a new script owned by the authenticated user, at version 1.
    async fn create_script(
        &self,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
    ) -> Result<Script>;

    /// Fetches the script matching `id`.
    async fn fetch_script(&self, id: Uuid) -> Result<Script>;

    /// Enables `script_id` for the authenticated user. Enabling an
    /// already-enabled script is a no-op.
    async fn enable_script(&self, script_id: Uuid) -> Result<()>;

    /// Disables `script_id` for the authenticated user. Disabling a script
    /// that isn't enabled is a no-op.
    async fn disable_script(&self, script_id: Uuid) -> Result<()>;

    /// Lists the scripts currently enabled for the authenticated user.
    async fn list_user_scripts(&self) -> Result<Vec<Script>>;
}

/// Initializes the scripts service: talks to the fyde server's scripts
/// service over the shared `channel` connection, authenticating every call
/// via `sessions`.
pub(crate) fn init(channel: Channel, sessions: Arc<SessionsClient>) -> Arc<dyn Service> {
    Arc::new(service::ScriptsClient::new(channel, sessions))
}
