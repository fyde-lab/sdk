mod grpc_client;
mod models;
mod service;
mod storage_in_memory;

pub use storage_in_memory::InMemoryScriptStorage;

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
use crate::domains::sessions::SessionsClient;

/// Manages user-authored scripts against the fyde server's scripts service
/// (see `grpc_client.rs`): creating them, fetching them by id,
/// enabling/disabling them for the authenticated user, and listing the ones
/// currently enabled. Running scripts against a document is done by the
/// `documents` domain, using [`Service::list_user_scripts`] together with
/// its own `vm` module. Trait methods take `&self` (not `&mut self`) so
/// implementations can be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Creates a new script owned by the authenticated user, at version 1.
    #[allow(clippy::too_many_arguments)]
    async fn create_script(
        &self,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
    ) -> Result<Script>;

    /// Fetches the script matching `id`.
    async fn fetch_script(&self, id: Uuid) -> Result<Script>;

    /// Updates a script owned by the authenticated user, incrementing its
    /// version. Fails if the script doesn't exist or the authenticated user
    /// isn't its owner.
    #[allow(clippy::too_many_arguments)]
    async fn update_script(
        &self,
        id: Uuid,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
    ) -> Result<Script>;

    /// Enables `script_id` for the authenticated user. Enabling an
    /// already-enabled script is a no-op.
    async fn enable_script(&self, script_id: Uuid) -> Result<()>;

    /// Disables `script_id` for the authenticated user. Disabling a script
    /// that isn't enabled is a no-op.
    async fn disable_script(&self, script_id: Uuid) -> Result<()>;

    /// Lists the scripts currently enabled for the authenticated user.
    async fn list_user_scripts(&self) -> Result<Vec<Script>>;

    /// Lists every script marked public, regardless of who owns it or
    /// whether the authenticated user has it enabled.
    async fn list_public_scripts(&self) -> Result<Vec<Script>>;
}

/// Initializes the scripts service: talks to the fyde server's scripts
/// service over the shared `channel` connection, authenticating every call
/// via `sessions`.
pub(crate) fn init(channel: Channel, sessions: Arc<SessionsClient>) -> Arc<dyn Service> {
    Arc::new(service::ScriptsClient::new(channel, sessions))
}
