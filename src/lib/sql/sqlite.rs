use std::path::PathBuf;

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

use crate::{Error, ErrorContext as _, Result};

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

    async fn connect_at(path: PathBuf) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);

        // SQLite only supports a single writer at a time, so a single
        // pooled connection avoids lock-contention errors under concurrent
        // writes.
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
}
