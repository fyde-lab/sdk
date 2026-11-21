mod models;
mod recorder;
mod redact;
mod service;
mod storage;
mod storage_file;
mod storage_sqlite;

pub(super) use models::ReportedScript;
pub use models::{ScraperRun, ScraperRunOutcome, ScraperRunUpdate};
pub(super) use recorder::Recorder;
pub(crate) use redact::redact_url;
pub(super) use service::RunTracker;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::Result;

/// Keeps a report of each scraper run: how it went — when it ran, how it
/// ended, how far it got and how many documents it saved — and its debug
/// log, the logs, HTTP requests, browser steps, progress updates, input
/// prompts and errors recorded by [`Recorder`] while it ran (ported from
/// `demo-rust-fyde`'s `report.rs`). A run's report is opened as it starts
/// and completed once it ends, so a run cut short by its process ending is
/// still on record. Unlike `super::cookies`/`super::session`, which persist
/// state read back and built on by the next run, every run adds its own
/// report to the scraper's history. Trait methods take `&self` (not `&mut
/// self`) so implementations can be shared behind `Arc<dyn Service>`.
#[async_trait]
pub(super) trait Service: Send + Sync {
    /// Opens the report of a run of `name` starting now, of `script` if
    /// it's known (a run of an installation). Returns where to record its
    /// log and progress, to hand to [`Self::finish`].
    async fn start(&self, name: &str, script: Option<ReportedScript>) -> Result<RunTracker>;

    /// Completes `tracker`'s report with how its run ended — `result` is
    /// what it returned (`ScraperRunStopped` meaning it was stopped) — and
    /// everything it recorded.
    async fn finish(&self, tracker: RunTracker, result: &Result<()>) -> Result<()>;

    /// Each scraper's latest run, latest first. One still marked running
    /// but not started by this service (its process ended mid-run) reads
    /// as [`ScraperRunOutcome::Interrupted`].
    async fn last_runs(&self) -> Result<Vec<ScraperRun>>;
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

/// A reports service held in memory, for tests of the scrapers service.
#[cfg(test)]
pub(super) fn in_memory() -> Arc<dyn Service> {
    Arc::new(service::ReportsClient::new(
        storage::MemoryStorage::default(),
    ))
}
