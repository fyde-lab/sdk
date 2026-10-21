use std::path::{Path, PathBuf};

use std::sync::Mutex;

use sqlx::{
    ConnectOptions as _, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePoolOptions},
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
    /// For an in-memory database only: a connection held open, outside the
    /// pool, for as long as this client lives — see
    /// [`Self::connect_in_memory`]. Never queried; the `Mutex` only makes
    /// it `Sync`.
    _in_memory_keeper: Option<Mutex<SqliteConnection>>,
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
        create_owner_only(&path)?;

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);

        Self::connect_with_options(options).await
    }

    async fn connect_in_memory() -> Result<Self> {
        // A plain `:memory:` database is private to the connection that
        // opened it, and sqlx closes and reopens a pooled connection on its
        // own (e.g. after a query on it was cancelled midway) — which would
        // silently swap in an empty, unmigrated database. Naming it and
        // sharing its cache lets every connection the pool ever opens
        // attach to the same one, and the keeper connection stops it from
        // being freed in between, when the pool briefly has none open.
        let options = SqliteConnectOptions::new()
            .filename(format!("file:fyde-sdk-{}", uuid::Uuid::new_v4()))
            .in_memory(true)
            .shared_cache(true);
        let keeper = options
            .connect()
            .await
            .context("failed to open in-memory local database")?;

        let mut client = Self::connect_with_options(options).await?;
        client._in_memory_keeper = Some(Mutex::new(keeper));
        Ok(client)
    }

    async fn connect_with_options(options: SqliteConnectOptions) -> Result<Self> {
        // SQLite only supports a single writer at a time, so a single
        // pooled connection avoids lock-contention errors under concurrent
        // writes. (It does not by itself keep an in-memory database alive:
        // sqlx may still replace that one connection — see
        // `connect_in_memory`.)
        // `secure_delete` makes SQLite overwrite deleted content with zeros
        // instead of just marking its pages free, so a cached document or a
        // scraper's cookies deleted on `wipe` can't be carved back out of the
        // file afterwards.
        let options = options.pragma("secure_delete", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .context("failed to connect to local database")?;

        sqlx::migrate!()
            .run(&pool)
            .await
            .context("failed to run local database migrations")?;

        Ok(Self {
            pool,
            _in_memory_keeper: None,
        })
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
        // `pragma_table_list` (unlike `sqlite_master`) tells a virtual table
        // apart from the `shadow` tables backing it, which must never be
        // written to directly — clearing an FTS5 index's `_data`/`_config`
        // rows by hand corrupts it. Virtual tables are listed last, so
        // nothing deleting from a regular table (e.g. `documents`'s FTS
        // triggers) writes to them after they've been cleared.
        let tables: Vec<(String, String)> = sqlx::query_as(
            "SELECT name, type FROM pragma_table_list \
             WHERE schema = 'main' AND type IN ('table', 'virtual') \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             AND name != '_sqlx_migrations' \
             ORDER BY type = 'virtual'",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list local database tables")?;

        for (table, kind) in tables {
            // `table` comes from `pragma_table_list`, not external input, so
            // interpolating it into the query is safe.
            let statement = if kind == "virtual" {
                // Every virtual table here is a contentless FTS5 index (see
                // the `documents_fts` migration). A plain `DELETE` would
                // only tombstone its rows, leaving their tokens readable in
                // the index's segments; `delete-all` drops them outright.
                format!("INSERT INTO {table} ({table}) VALUES ('delete-all')")
            } else {
                format!("DELETE FROM {table}")
            };
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .execute(&self.pool)
                .await
                .with_context(|| format!("failed to wipe local database table {table}"))?;
        }

        // Rebuilds the file from what's left, so no page that ever held the
        // wiped rows survives in it, `secure_delete` notwithstanding (e.g.
        // pages freed before it was turned on).
        sqlx::query("VACUUM")
            .execute(&self.pool)
            .await
            .context("failed to compact local database after wiping it")?;

        Ok(())
    }
}

/// Every file SQLite keeps for the database at `path`: the database itself
/// and the journal files it creates beside it (`-wal`/`-shm` in WAL mode,
/// sqlx's default, or a rollback `-journal`).
pub(crate) fn database_files(path: &Path) -> Vec<PathBuf> {
    let mut files = vec![path.to_path_buf()];
    files.extend(["-wal", "-shm", "-journal"].into_iter().map(|suffix| {
        let mut file = path.as_os_str().to_os_string();
        file.push(suffix);
        PathBuf::from(file)
    }));
    files
}

/// Creates the database file at `path` (if it doesn't exist yet) readable
/// and writable by its owner only, before SQLite opens it: it holds every
/// cached document in plaintext, so it must not inherit a umask-derived,
/// typically world-readable mode. SQLite's own journal files copy the main
/// file's permissions. A no-op off Unix, where the OS's per-user profile
/// directories (Windows) or per-app sandboxes (mobile) already scope access.
fn create_owner_only(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to create local database {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;

    Ok(())
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
    async fn an_in_memory_database_survives_its_pooled_connection_being_replaced() {
        let client = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();
        sqlx::query("INSERT INTO settings (key, value) VALUES ('role', 'admin')")
            .execute(client.pool())
            .await
            .unwrap();

        // Takes the pool's only connection out of it and closes it, as sqlx
        // does on its own when e.g. a query on it is cancelled midway; the
        // next query then runs on a freshly opened connection.
        client.pool().acquire().await.unwrap().detach();

        let role: (String,) = sqlx::query_as("SELECT value FROM settings WHERE key = 'role'")
            .fetch_one(client.pool())
            .await
            .unwrap();
        assert_eq!(role.0, "admin");
    }

    #[tokio::test]
    async fn in_memory_databases_are_private_to_their_client() {
        let first = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();
        let second = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();
        sqlx::query("INSERT INTO settings (key, value) VALUES ('role', 'admin')")
            .execute(first.pool())
            .await
            .unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings")
            .fetch_one(second.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 0);
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

    #[tokio::test]
    async fn wipe_leaves_no_trace_of_documents_in_the_search_index() {
        let client = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();
        let insert_document = |id: &'static str, name: &'static str| {
            sqlx::query(
                "INSERT INTO documents (id, name, original_name, content_type, content, created_at)
                 VALUES (?1, ?2, ?2, 'application/pdf', x'', 0)",
            )
            .bind(id)
            .bind(name)
            .execute(client.pool())
        };
        insert_document("one", "secret zyxwvutsrqp").await.unwrap();
        // An update leaves the old tokens behind in the index's segments
        // until they're merged, so this checks `wipe` clears those too.
        sqlx::query("UPDATE documents SET name = 'renamed' WHERE id = 'one'")
            .execute(client.pool())
            .await
            .unwrap();

        client.wipe().await.unwrap();

        let (residue,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM documents_fts_data WHERE instr(block, 'zyxwvutsrqp') > 0",
        )
        .fetch_one(client.pool())
        .await
        .unwrap();
        assert_eq!(residue, 0);

        // The index still works afterwards, for documents synced after a
        // subsequent login.
        insert_document("two", "fresh").await.unwrap();
        let (matches,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM documents_fts WHERE documents_fts MATCH 'fresh'")
                .fetch_one(client.pool())
                .await
                .unwrap();
        assert_eq!(matches, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn creates_the_database_file_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = temp_db_path();

        let client = SqliteClient::connect_at(path.clone()).await.unwrap();
        drop(client);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn turns_secure_delete_on() {
        let client = SqliteClient::connect_with(IN_MEMORY_DB).await.unwrap();

        let (secure_delete,): (i64,) = sqlx::query_as("PRAGMA secure_delete")
            .fetch_one(client.pool())
            .await
            .unwrap();

        assert_eq!(secure_delete, 1);
    }

    #[tokio::test]
    async fn wipe_leaves_no_trace_of_the_wiped_rows_in_the_database_file() {
        let path = temp_db_path();
        let client = SqliteClient::connect_at(path.clone()).await.unwrap();
        let marker = "wiped-marker-0123456789";
        sqlx::query("INSERT INTO settings (key, value) VALUES ('k', ?)")
            .bind(marker)
            .execute(client.pool())
            .await
            .unwrap();

        client.wipe().await.unwrap();
        drop(client);

        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(marker.len())
                .any(|window| window == marker.as_bytes())
        );
        std::fs::remove_file(&path).unwrap();
    }
}
