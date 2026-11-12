mod grpc_client;
mod models;
mod service;
mod storage;
mod storage_sqlite;

#[cfg(test)]
pub(crate) use storage::MockStorage;
pub(crate) use storage::Storage;
pub(crate) use storage_sqlite::SqliteStorage;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;
use tonic::transport::Channel;
use uuid::Uuid;

#[cfg(any(test, feature = "testing"))]
pub use models::FakeScript;
#[cfg(test)]
pub(crate) use models::{FakeInstalledScript, FakeScriptMetadata};
pub use models::{
    InstallScriptRequest, InstalledScript, MAX_INSTALLATION_LABEL_CHARS, Script, ScriptMetadata,
    ScriptParameter, ScriptParameterType, ScriptType,
};

use crate::Result;
use crate::domains::changelog::Service as ChangelogService;
use crate::domains::sessions::SessionsClient;

/// Reads scripts from the fyde server's scripts service (see
/// `grpc_client.rs`): fetching them, enabling/disabling them for the
/// authenticated user, listing the ones currently enabled or public, and
/// installing them. Writing scripts is admin-only and out of this SDK: the
/// server takes it through `admin.v1.AdminService`. Running scripts against a document is done
/// by the `documents` domain's `parser` submodule, using
/// [`Service::list_user_scripts`] and [`Service::fetch_script`] together
/// with its own sandboxed Lua VM. Trait methods take `&self` (not `&mut self`) so
/// implementations can be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Fetches the script matching `id`.
    async fn fetch_script(&self, id: Uuid) -> Result<Script>;

    /// Enables `script_id` for the authenticated user. Enabling an
    /// already-enabled script is a no-op.
    async fn enable_script(&self, script_id: Uuid) -> Result<()>;

    /// Disables `script_id` for the authenticated user. Disabling a script
    /// that isn't enabled is a no-op.
    async fn disable_script(&self, script_id: Uuid) -> Result<()>;

    /// Lists the metadata of the scripts currently enabled for the
    /// authenticated user, most recently enabled first, fetching every page
    /// of the server's paginated listing. Fetch a script with
    /// [`Self::fetch_script`] for its content, and its icon (by
    /// [`ScriptMetadata::icon_id`]) with [`crate::IconsService::fetch_icon`].
    async fn list_user_scripts(&self) -> Result<Vec<ScriptMetadata>>;

    /// Lists the metadata of every script marked public, regardless of who
    /// owns it or whether the authenticated user has it enabled, most
    /// recently updated first, fetching every page of the server's
    /// paginated listing. Fetch a script with [`Self::fetch_script`] for its
    /// content, and its icon (by [`ScriptMetadata::icon_id`]) with
    /// [`crate::IconsService::fetch_icon`].
    async fn list_public_scripts(&self) -> Result<Vec<ScriptMetadata>>;

    /// Installs the script `request.script_id` for the authenticated user,
    /// to be run with `request.parameters`: as a new installation if
    /// `request.installation_id` is `None` (a user can install the same
    /// script several times, once per account on its site), else replacing
    /// that installation. Fetches the script (see [`Self::fetch_script`]),
    /// then serializes it together with the installation's id, label and
    /// parameters as an [`InstalledScript`] JSON document and publishes it,
    /// encrypted, as an `InstallScript` changelog event whose subject is the
    /// installation id, which it returns. Fails with
    /// [`crate::ErrorCode::MissingScriptParameter`] if a parameter the script
    /// marks `required` has no value, and with
    /// [`crate::ErrorCode::InvalidInstallationLabel`] if the trimmed label
    /// is over [`MAX_INSTALLATION_LABEL_CHARS`] characters or holds a control character. A blank
    /// label defaults to the first non-blank non-secret text parameter,
    /// `username` first then the others by name, else the script's name.
    ///
    /// Returns once the event is published, not once it's saved locally:
    /// like every other changelog event, it's only materialized into local
    /// storage (and so only shows up in [`Self::list_installed_scripts`])
    /// once the background sync job consumes it back — on every device
    /// logged into the same account, not just this one. Doesn't enable the
    /// script on the server: callers do, with [`Self::enable_script`].
    async fn install_script(&self, request: InstallScriptRequest) -> Result<Uuid>;

    /// Uninstalls installation `installation_id`: publishes an
    /// `UninstallScript` changelog event. Consuming it deletes the
    /// installation and every scraper state kept under it (session,
    /// cookies, debug reports) on each device. No local check, so an
    /// installation that hasn't synced here yet can be uninstalled. Doesn't
    /// disable the script on the server: callers do, with
    /// [`Self::disable_script`], once none of its installations remains.
    async fn uninstall_script(&self, installation_id: Uuid) -> Result<()>;

    /// Lists every installation for the authenticated user that's been
    /// synced to this device so far, oldest installation first. Never talks
    /// to the server: reads straight from local storage.
    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>>;

    /// The installation `installation_id` synced to this device — the
    /// script plus the parameter values to run it with. Fails with
    /// `ErrorCode::ScriptNotInstalled` if there's none (yet).
    async fn fetch_installed_script(&self, installation_id: Uuid) -> Result<InstalledScript>;
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
        grpc_client::GrpcClient::new(channel, sessions),
        SqliteStorage::new(pool),
        changelog,
    ))
}
