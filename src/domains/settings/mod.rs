mod service;
mod storage;
mod storage_sqlite;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;

use crate::Result;

/// Persists arbitrary key/value settings in the SDK's local SQLite
/// database — used both by other SDK modules for their own state (the
/// session auth token, the local master key, the changelog consumption
/// offset — see `changelog::storage_settings`) and, later, for
/// user-facing settings. Trait methods take `&self` (not `&mut self`) so
/// implementations can be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Returns the value stored under `key`, or `None` if it has never
    /// been set.
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Stores `value` under `key`, overwriting any value previously
    /// stored under it.
    async fn set(&self, key: &str, value: &str) -> Result<()>;

    /// Removes the value stored under `key`, if any. A no-op if `key` was
    /// never set.
    async fn delete(&self, key: &str) -> Result<()>;
}

/// Initializes the settings service: uses `pool` to persist key/value
/// pairs in the local `settings` table. This one is for the SDK's own
/// modules, whose keys (`username`, `role`, the changelog cursor, ...)
/// callers must never reach: hand them [`public`] instead.
pub(crate) fn init(pool: SqlitePool) -> Arc<dyn Service> {
    let storage = storage_sqlite::SqliteStorage::new(pool);
    Arc::new(service::SettingsClient::new(storage))
}

/// Wraps `internal` (from [`init`]) into the settings service
/// `Client::settings` hands callers: every key they pass is stored under
/// [`PUBLIC_KEY_PREFIX`], so no caller key can read, overwrite or delete
/// a value an SDK module keeps in the same table (e.g. the cached
/// username `users::change_password` verifies the old password against).
pub(crate) fn public(internal: Arc<dyn Service>) -> Arc<dyn Service> {
    Arc::new(service::PublicSettings::new(internal))
}

/// The prefix [`public`] stores every caller key under. SDK modules must
/// never use a key starting with it.
pub(crate) const PUBLIC_KEY_PREFIX: &str = "app.";
