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
/// themselves manages. Trait methods take `&self` (not `&mut self`) so
/// implementations can be shared behind `Arc<dyn Service>`.
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

/// Internal counterpart to [`Service`], `pub(crate)` so it never crosses
/// the SDK's public API. Used by other SDK modules to persist their own
/// state (the session auth token, the local master key, the changelog
/// consumption offset — see `changelog::storage_settings`) in the same
/// local `settings` table as user-facing settings, without exposing that
/// state through [`Service`].
#[async_trait]
pub(crate) trait InternalService: Send + Sync {
    /// Returns the value stored under `key`, or `None` if it has never
    /// been set.
    async fn get_internal(&self, key: &str) -> Result<Option<String>>;

    /// Stores `value` under `key`, overwriting any value previously
    /// stored under it.
    async fn set_internal(&self, key: &str, value: &str) -> Result<()>;
}

/// Initializes the settings service: uses `pool` to persist key/value
/// pairs in the local `settings` table. Returns both the public interface
/// (wired into [`crate::Client::settings`]) and the internal one (used by
/// other SDK modules for their own state), sharing the same underlying
/// storage.
pub(crate) fn init(pool: SqlitePool) -> (Arc<dyn Service>, Arc<dyn InternalService>) {
    let storage = storage_sqlite::SqliteStorage::new(pool);
    let client = Arc::new(service::SettingsClient::new(storage));
    (client.clone(), client)
}
