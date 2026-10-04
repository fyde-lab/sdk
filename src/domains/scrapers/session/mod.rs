mod service;
mod storage;
mod storage_file;
mod storage_sqlite;

use std::path::PathBuf;
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

/// Selects which [`storage::Storage`] backend [`init`] builds the session
/// service on top of.
pub(super) enum StorageConfig {
    /// Persists session data as one JSON file per scraper under the given
    /// directory (see [`storage_file::FileStorage`]) — used by `../scripts`'
    /// standalone CLI runner, which has no local SQLite database of its own.
    File(PathBuf),
    /// Persists session data in the local `scraper_sessions` table of the
    /// given SQLite pool (see [`storage_sqlite::SqliteStorage`]).
    Sqlite(SqlitePool),
}

/// Initializes the session service on top of the [`storage::Storage`]
/// backend selected by `config`.
pub(super) fn init(config: StorageConfig) -> Arc<dyn Service> {
    match config {
        StorageConfig::File(dir) => {
            let storage = storage_file::FileStorage::new(dir);
            Arc::new(service::SessionClient::new(storage))
        }
        StorageConfig::Sqlite(pool) => {
            let storage = storage_sqlite::SqliteStorage::new(pool);
            Arc::new(service::SessionClient::new(storage))
        }
    }
}
