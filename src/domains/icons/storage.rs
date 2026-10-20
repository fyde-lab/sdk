use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use uuid::Uuid;

use crate::Result;

use super::Icon;

/// Caches icons locally. Injected into [`super::service::IconsClient`] at
/// construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait Storage: Send + Sync {
    /// The cached icon matching `id`, or `None` if it isn't cached.
    async fn get(&self, id: Uuid) -> Result<Option<Icon>>;

    /// Caches `icon`. Caching an icon already cached is a no-op.
    async fn save(&self, icon: &Icon) -> Result<()>;
}
