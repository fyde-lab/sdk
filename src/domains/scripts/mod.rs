mod grpc_client;
mod models;
mod service;
mod storage;
mod storage_in_memory;
mod storage_sqlite;

#[cfg(test)]
pub(crate) use storage::MockStorage;
pub(crate) use storage::Storage;
pub use storage_in_memory::InMemoryScriptStorage;
pub(crate) use storage_sqlite::SqliteStorage;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;
use tonic::transport::Channel;
use uuid::Uuid;

#[cfg(test)]
pub(crate) use models::{FakeInstalledScript, FakeScript};
pub use models::{
    InstallScriptRequest, InstalledScript, Script, ScriptParameter, ScriptParameterType, ScriptType,
};

use crate::Result;
use crate::domains::changelog::Service as ChangelogService;
use crate::domains::sessions::SessionsClient;

/// Manages user-authored scripts against the fyde server's scripts service
/// (see `grpc_client.rs`): creating, fetching and updating them,
/// enabling/disabling them for the authenticated user, and listing the ones
/// currently enabled or public. Running scripts against a document is done
/// by the `documents` domain's `parser` submodule, using
/// [`Service::list_user_scripts`] together with its own sandboxed Lua VM. Trait methods take `&self` (not `&mut self`) so
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
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
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
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
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

    /// Installs the script `request.script_id` for the authenticated user,
    /// to be run with `request.parameters`: fetches the script (see
    /// [`Self::fetch_script`]), then serializes it together with those
    /// parameters as an [`InstalledScript`] JSON document and publishes it,
    /// encrypted, as a `ScriptInstalled` changelog event. Fails with
    /// [`crate::Error::MissingScriptParameter`] if a parameter the script
    /// marks `required` has no value.
    ///
    /// Returns once the event is published, not once it's saved locally:
    /// like every other changelog event, it's only materialized into local
    /// storage (and so only shows up in [`Self::list_installed_scripts`])
    /// once the background sync job consumes it back — on every device
    /// logged into the same account, not just this one.
    async fn install_script(&self, request: InstallScriptRequest) -> Result<()>;

    /// Lists every script installed for the authenticated user that's been
    /// synced to this device so far, oldest installation first. Never talks
    /// to the server: reads straight from local storage.
    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>>;
}

/// Initializes the scripts service: talks to the fyde server's scripts
/// service over the shared `channel` connection, authenticating every call
/// via `sessions`, publishes script installations through `changelog`, and
/// reads installed scripts back from the local SQLite database (`pool`)
/// the changelog materializes them into.
pub(crate) fn init(
    channel: Channel,
    sessions: Arc<SessionsClient>,
    pool: SqlitePool,
    changelog: Arc<dyn ChangelogService>,
) -> Arc<dyn Service> {
    Arc::new(service::ScriptsClient::new(
        channel,
        sessions,
        SqliteStorage::new(pool),
        changelog,
    ))
}
