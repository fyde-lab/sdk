mod models;
mod service;
mod storage;
mod storage_file;
mod storage_sqlite;

pub(super) use models::Cookie;
#[cfg(test)]
pub(super) use models::FakeCookie;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;

use crate::Result;

/// Persists a scraper's cookie jar across runs — the cookies half of
/// `demo-rust-fyde`'s `host/session.rs`, split out into its own sub-domain
/// (see `super::session` for the other half). Trait methods take `&self`
/// (not `&mut self`) so implementations can be shared behind `Arc<dyn
/// Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait Service: Send + Sync {
    /// Returns every cookie previously saved for `scraper_name`, or an empty
    /// list on a first run.
    async fn load(&self, scraper_name: &str) -> Result<Vec<Cookie>>;

    /// Replaces every cookie previously saved for `scraper_name` with
    /// `cookies` — the full set a scraper run's `fyde.http` client picked up
    /// for a host it actually visited.
    async fn save(&self, scraper_name: &str, cookies: Vec<Cookie>) -> Result<()>;
}

/// Selects which [`storage::Storage`] backend [`init`] builds the cookies
/// service on top of.
pub(super) enum StorageConfig {
    /// Persists cookies as one JSON file per scraper under the given
    /// directory (see [`storage_file::FileStorage`]) — used by `../scripts`'
    /// standalone CLI runner, which has no local SQLite database of its own.
    File(PathBuf),
    /// Persists cookies in the local `scraper_cookies` table of the given
    /// SQLite pool (see [`storage_sqlite::SqliteStorage`]).
    Sqlite(SqlitePool),
}

/// Initializes the cookies service on top of the [`storage::Storage`]
/// backend selected by `config`.
pub(super) fn init(config: StorageConfig) -> Arc<dyn Service> {
    match config {
        StorageConfig::File(dir) => {
            let storage = storage_file::FileStorage::new(dir);
            Arc::new(service::CookiesClient::new(storage))
        }
        StorageConfig::Sqlite(pool) => {
            let storage = storage_sqlite::SqliteStorage::new(pool);
            Arc::new(service::CookiesClient::new(storage))
        }
    }
}
