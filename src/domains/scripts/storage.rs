use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use uuid::Uuid;

use crate::Result;

use super::InstalledScript;

/// Persists script installations, unencrypted, locally, keyed by
/// installation id. Injected into [`super::service::ScriptsClient`] (to read
/// them) and into `changelog::ChangelogClient` (to save and delete the ones
/// materialized from consumed `InstallScript`/`UninstallScript` events) at
/// construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Persists `installed`, replacing any previously saved installation
    /// with the same installation id (other installations of the same
    /// script stay). A replaced installation keeps its place in
    /// [`Self::list_installed_scripts`]' order.
    async fn save_installed_script(&self, installed: &InstalledScript) -> Result<()>;

    /// Lists every installation previously saved by
    /// [`Self::save_installed_script`], oldest installation first.
    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>>;

    /// Returns the saved installation `installation_id`, if any.
    async fn find_installed_script(&self, installation_id: Uuid)
    -> Result<Option<InstalledScript>>;

    /// Deletes the saved installation `installation_id`. A no-op if there's
    /// none.
    async fn delete_installed_script(&self, installation_id: Uuid) -> Result<()>;
}
