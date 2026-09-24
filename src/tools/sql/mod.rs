mod sqlite;

pub use sqlite::{IN_MEMORY_DB, SqliteClient};

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

/// Wipes all data in the SDK's local database, used to clear local state on
/// logout. Trait methods take `&self` (not `&mut self`) so implementations
/// can be shared behind `Arc<dyn LocalDatabase>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait LocalDatabase: Send + Sync {
    async fn wipe(&self) -> Result<()>;
}

#[async_trait]
impl LocalDatabase for SqliteClient {
    async fn wipe(&self) -> Result<()> {
        SqliteClient::wipe(self).await
    }
}
