use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use uuid::Uuid;

use crate::Result;

use super::Folder;

/// Persists the vault's folders, unencrypted, locally (the `folders`
/// table). Only stores: every rule lives in `service.rs`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Every folder, in no particular order.
    async fn list_folders(&self) -> Result<Vec<Folder>>;

    /// In one transaction, saves every folder of `save` (replacing a saved
    /// one with the same id) and deletes the folders of `delete` (an id
    /// that isn't saved is ignored).
    async fn apply(&self, save: Vec<Folder>, delete: Vec<Uuid>) -> Result<()>;
}
