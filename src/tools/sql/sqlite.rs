use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

use crate::{Error, ErrorContext as _, Result};

/// Value accepted by [`SqliteClient::connect_with`] to select a private
/// in-memory database instead of a file on disk.
pub const IN_MEMORY_DB: &str = ":memory:";

/// A local SQLite database used by the SDK to persist data on disk, at a
/// caller-provided path (see [`SqliteClient::connect_with`]), created if it
/// doesn't exist.
///
/// Opening a file-backed database also takes an OS-level exclusive lock, so
/// a second process (or a second [`SqliteClient`] in the same process)
/// pointed at the same database file fails to connect instead of racing
/// SQLite's own locking.
pub struct SqliteClient {
    pool: SqlitePool,
    // Held for the lifetime of the client: an OS-level exclusive lock on a
    // sibling `.lock` file that keeps a second SDK instance from opening the
    // same database concurrently. `None` for an in-memory database, which
    // has no path to lock and is never shared across processes. Dropping
    // this file releases the lock.
    _lock: Option<File>,
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
        let lock = lock_database_file(&path)
            .with_context(|| format!("failed to lock local database at {}", path.display()))?;

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);

        Self::connect_with_options(options, Some(lock)).await
    }

    async fn connect_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::new().in_memory(true);

        Self::connect_with_options(options, None).await
    }

    async fn connect_with_options(
        options: SqliteConnectOptions,
        lock: Option<File>,
    ) -> Result<Self> {
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

        Ok(Self { pool, _lock: lock })
    }

    /// Returns the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Acquires an OS-level exclusive lock on a `.lock` file next to `db_path`,
/// so a second SDK instance pointed at the same database fails fast instead
/// of racing SQLite's own locking. A sibling file is used, rather than
/// locking the database file itself, to stay independent of SQLite's own
/// (POSIX `fcntl`-based) locking of that file. The returned `File` must be
/// kept alive for as long as the lock should be held; dropping it releases
/// the lock.
fn lock_database_file(db_path: &Path) -> Result<File> {
    let mut lock_path = db_path.as_os_str().to_owned();
    lock_path.push(".lock");

    let file = File::create(&lock_path).context("failed to open lock file")?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(err) if err.kind() == ErrorKind::WouldBlock => Err(Error::Context {
            message: "local database is already in use by another instance".to_string(),
            source: Box::new(Error::Io(err)),
        }),
        Err(err) => Err(err).context("failed to acquire local database lock"),
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
        std::fs::remove_file(format!("{}.lock", path.display())).unwrap();
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
    async fn rejects_a_second_instance_on_the_same_database_file() {
        let path = temp_db_path();

        let first = SqliteClient::connect_at(path.clone()).await.unwrap();

        let err = match SqliteClient::connect_at(path.clone()).await {
            Ok(_) => panic!("a second instance must not be able to open the same database"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("already in use by another instance")
        );

        drop(first);

        // Once the first instance releases the lock, a new one can connect.
        let second = SqliteClient::connect_at(path.clone()).await.unwrap();
        drop(second);

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(format!("{}.lock", path.display())).unwrap();
    }
}
