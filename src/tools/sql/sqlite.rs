use std::path::PathBuf;

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

use crate::{ErrorContext as _, Result};

/// Value accepted by [`SqliteClient::connect_with`] to select a private
/// in-memory database instead of a file on disk.
pub const IN_MEMORY_DB: &str = ":memory:";

/// A local SQLite database used by the SDK to persist data on disk, at a
/// caller-provided path (see [`SqliteClient::connect_with`]), created if it
/// doesn't exist.
pub struct SqliteClient {
    pool: SqlitePool,
}

impl SqliteClient {
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

    pub(crate) async fn connect_at(path: PathBuf) -> Result<Self> {
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

    /// Deletes every row from every table in the local database, used to
    /// wipe all local state (cached documents, settings — including the
    /// session token — and the changelog offset) on logout. The single
    /// pooled connection stays open throughout, unlike deleting the
    /// database file itself would: the SDK keeps working against the same
    /// (now empty) database for the rest of the process's lifetime, so a
    /// subsequent login persists correctly instead of writing into an
    /// orphaned, unlinked file.
    pub(crate) async fn wipe(&self) -> Result<()> {
        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             AND name != '_sqlx_migrations'",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list local database tables")?;

        for (table,) in tables {
            // `table` comes from `sqlite_master`, not external input, so
            // interpolating it into the query is safe.
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
                .execute(&self.pool)
                .await
                .with_context(|| format!("failed to wipe local database table {table}"))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> PathBuf {
        std::env::temp_dir().join(format!("fyde-sdk-test-{}.db", uuid::Uuid::now_v7()))
    }

    #[tokio::test]
    async fn connects_and_creates_the_database_file() {
        let path = temp_db_path();

        let client = SqliteClient::connect_at(path.clone()).await.unwrap();
        drop(client);

        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn connects_to_an_in_memory_database() {
        let client = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();

        // A no-op query just confirms the pool is actually usable (i.e.
        // migrations ran against it) rather than merely constructed.
        sqlx::query("SELECT 1")
            .execute(client.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn wipe_deletes_all_rows_from_every_table_but_keeps_the_database_usable() {
        let client = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();
        sqlx::query("INSERT INTO settings (key, value) VALUES ('session_token', 'a-token')")
            .execute(client.pool())
            .await
            .unwrap();

        client.wipe().await.unwrap();

        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings")
            .fetch_one(client.pool())
            .await
            .unwrap();
        assert_eq!(row.0, 0);

        // The connection still works afterwards, e.g. for a subsequent
        // login to persist a fresh session token.
        sqlx::query("INSERT INTO settings (key, value) VALUES ('session_token', 'new-token')")
            .execute(client.pool())
            .await
            .unwrap();
    }
}
