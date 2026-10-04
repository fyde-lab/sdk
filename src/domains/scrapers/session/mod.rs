mod service;
mod storage;
mod storage_file;
mod storage_sqlite;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use serde_json::Value;
use sqlx::SqlitePool;

use crate::Result;

/// Persists a scraper's arbitrary session data across runs — the session
/// half of `demo-rust-fyde`'s `host/session.rs`, split out into its own
/// sub-domain (see `super::cookies` for the other half). A script reads and
/// writes this freely as `fyde.session` to carry its own state between runs
/// (cursors, "already seen" markers, ...) — this service never looks inside
/// it. Trait methods take `&self` (not `&mut self`) so implementations can
/// be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait Service: Send + Sync {
    /// Returns the session data previously saved for `scraper_name`, or an
    /// empty JSON object on a first run.
    async fn load(&self, scraper_name: &str) -> Result<Value>;

    /// Replaces the session data saved for `scraper_name` with `data` —
    /// `fyde.session`'s full contents at the end of a run.
    async fn save(&self, scraper_name: &str, data: Value) -> Result<()>;
}

/// Initializes the session service: uses `pool` to persist session data in
/// the local `scraper_sessions` table.
pub(super) fn init(pool: SqlitePool) -> Arc<dyn Service> {
    let storage = storage_sqlite::SqliteStorage::new(pool);
    Arc::new(service::SessionClient::new(storage))
}
