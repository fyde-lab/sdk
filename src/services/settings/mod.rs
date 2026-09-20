mod service;
mod storage;
mod storage_sqlite;

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::Result;

/// Persists arbitrary key/value settings in the SDK's local SQLite
/// database. This is the user-facing surface, exposed outside the SDK
/// (e.g. to `cli`/`application` through FFI) for settings the user
/// themselves manages; it does not carry the internal state (auth token,
/// master key, changelog offset) that the SDK keeps to itself via
/// `SettingsClient::get_internal`/`set_internal`, which are not part of
/// this trait and never leave the crate. Trait methods take `&self` (not
/// `&mut self`) so implementations can be shared behind `Arc<dyn
/// Service>`.
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
/// pairs in the local `settings` table.
pub(crate) fn init(pool: SqlitePool) -> Arc<dyn Service> {
    let storage = storage_sqlite::SqliteStorage::new(pool);
    Arc::new(service::SettingsClient::new(storage))
}
