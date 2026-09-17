use std::path::PathBuf;

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

use crate::{Error, ErrorContext as _, Result};

/// Value accepted by [`SqliteClient::connect_with`] to select a private
/// in-memory database instead of a file on disk.
pub const IN_MEMORY_DB: &str = ":memory:";

/// A local SQLite database used by the SDK to persist data on disk.
///
/// The database file lives under the XDG Base Directory Specification's
/// data directory (`$XDG_DATA_HOME/fyde/fyde.db`, falling back to
/// `~/.local/share/fyde/fyde.db`), which is created if it doesn't exist.
pub struct SqliteClient {
    pool: SqlitePool,
}

impl SqliteClient {
    /// Opens the local SQLite database, creating its containing directory
    /// and the database file if they don't already exist.
    pub async fn connect() -> Result<Self> {
        let path = local_database_path().context("failed to resolve local database path")?;
        Self::connect_at(path).await
    }

    /// Opens the local SQLite database at a caller-provided location:
    /// either a filesystem path, or [`IN_MEMORY_DB`] (`":memory:"`) for a
    /// private in-memory database that only lives for the process's
    /// lifetime.
    pub async fn connect_with(db: &str) -> Result<Self> {
        if db == IN_MEMORY_DB {
            Self::connect_in_memory().await
        } else {
            Self::connect_at(PathBuf::from(db)).await
        }
    }

    async fn connect_at(path: PathBuf) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);

        Self::connect_with_options(options).await
    }

    async fn connect_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::new().in_memory(true);

        Self::connect_with_options(options).await
    }

    async fn connect_with_options(options: SqliteConnectOptions) -> Result<Self> {
        // SQLite only supports a single writer at a time, so a single
        // pooled connection avoids lock-contention errors under concurrent
        // writes. For an in-memory database this also keeps the same
        // connection (and thus the same in-memory database) alive for the
        // lifetime of the pool, since each new connection would otherwise
        // get its own private, empty database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .context("failed to connect to local database")?;

        sqlx::migrate!()
            .run(&pool)
            .await
            .context("failed to run local database migrations")?;

        Ok(Self { pool })
    }

    /// Returns the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Resolves the path to the local database file per the XDG Base Directory
/// Specification, creating its parent directory if needed.
fn local_database_path() -> Result<PathBuf> {
    let dirs = xdg::BaseDirectories::with_prefix("fyde")
        .context("failed to resolve XDG base directories")?;
    dirs.place_data_file("fyde.db").map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connects_and_creates_the_database_file() {
        let path = std::env::temp_dir().join(format!("fyde-sdk-test-{}.db", uuid::Uuid::new_v4()));

        let client = SqliteClient::connect_at(path.clone()).await.unwrap();
        drop(client);

        assert!(path.exists());
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn connects_to_an_in_memory_database() {
        let client = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();

        // A no-op query just confirms the pool is actually usable (i.e.
        // migrations ran against it) rather than merely constructed.
        sqlx::query("SELECT 1").execute(client.pool()).await.unwrap();
    }
}
