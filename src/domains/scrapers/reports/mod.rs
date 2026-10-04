mod models;
mod recorder;
mod service;
mod storage;
mod storage_file;
mod storage_sqlite;

pub(super) use models::Report;
pub(super) use recorder::Recorder;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;

use crate::Result;

/// Persists a scraper run's debug report — the logs, HTTP requests/
/// responses, progress updates and input prompts recorded by
/// [`Recorder`] while the run was in progress — ported from
/// `demo-rust-fyde`'s `report.rs`. Trait methods take `&self` (not `&mut
/// self`) so implementations can be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait Service: Send + Sync {
    /// Saves `report`, a completed run's full entry list. Unlike
    /// `super::cookies`/`super::session`, which persist state read back and
    /// built on by the next run, a report is never read back — every call
    /// just adds another run's report to this scraper's history.
    async fn save(&self, report: Report) -> Result<()>;
}

/// Selects which [`storage::Storage`] backend [`init`] builds the reports
/// service on top of.
pub(super) enum StorageConfig {
    /// Persists each run's report as its own JSON file under the given
    /// directory (see [`storage_file::FileStorage`]) — used by `../scripts`'
    /// standalone CLI runner, which has no local SQLite database of its own.
    File(PathBuf),
    /// Persists each run's report as a row in the local `scraper_reports`
    /// table of the given SQLite pool (see [`storage_sqlite::SqliteStorage`]).
    Sqlite(SqlitePool),
}

/// Initializes the reports service on top of the [`storage::Storage`]
/// backend selected by `config`.
pub(super) fn init(config: StorageConfig) -> Arc<dyn Service> {
    match config {
        StorageConfig::File(dir) => {
            let storage = storage_file::FileStorage::new(dir);
            Arc::new(service::ReportsClient::new(storage))
        }
        StorageConfig::Sqlite(pool) => {
            let storage = storage_sqlite::SqliteStorage::new(pool);
            Arc::new(service::ReportsClient::new(storage))
        }
    }
}
