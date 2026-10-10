use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

use super::InstalledScript;

/// Persists installed scripts, unencrypted, locally. Injected into
/// [`super::service::ScriptsClient`] (to list them) and into
/// `changelog::ChangelogClient` (to save the ones materialized from
/// consumed `ScriptInstalled` events) at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Persists `installed`, replacing any previously saved installation of
    /// the same script id.
    async fn save_installed_script(&self, installed: &InstalledScript) -> Result<()>;

    /// Lists every script previously saved by
    /// [`Self::save_installed_script`], oldest installation first.
    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>>;
}
