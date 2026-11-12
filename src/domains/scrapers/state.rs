use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;

use crate::{ErrorContext as _, Result};

use super::session::{SqliteStorage as SessionSqliteStorage, Storage as SessionStorage};

/// Forgets what scraper runs kept under a name (an installation's id, for
/// `Service::run_installed`). Injected into `changelog::ChangelogClient`,
/// which calls it as it consumes script installation events.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait StateStorage: Send + Sync {
    /// Forgets the `fyde.session` data kept under `name`, so its next run
    /// starts from an empty session. Its cookies and reports stay. A no-op
    /// if there's none.
    async fn delete_session(&self, name: &str) -> Result<()>;

    /// Forgets everything kept under `name`: its `fyde.session` data, its
    /// cookie jar (a live session on the scraper's site) and its debug
    /// reports. A no-op for what there's none of.
    async fn delete_all(&self, name: &str) -> Result<()>;
}

/// A [`StateStorage`] over the SDK's local SQLite database (the
/// `scraper_sessions`, `scraper_cookies` and `scraper_reports` tables).
pub(crate) struct StateSqliteStorage {
    pool: SqlitePool,
    session: SessionSqliteStorage,
}

impl StateSqliteStorage {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self {
            session: SessionSqliteStorage::new(pool.clone()),
            pool,
        }
    }
}

#[async_trait]
impl StateStorage for StateSqliteStorage {
    async fn delete_session(&self, name: &str) -> Result<()> {
        self.session.delete(name).await
    }

    async fn delete_all(&self, name: &str) -> Result<()> {
        let mut tx =
            self.pool.begin().await.with_context(|| {
                format!("failed to start a transaction to forget scraper {name:?}")
            })?;
        for statement in [
            "DELETE FROM scraper_sessions WHERE scraper_name = ?1",
            "DELETE FROM scraper_cookies WHERE scraper_name = ?1",
            "DELETE FROM scraper_reports WHERE scraper_name = ?1",
        ] {
            sqlx::query(statement)
                .bind(name)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("failed to forget scraper {name:?}: {statement}"))?;
        }
        tx.commit()
            .await
            .with_context(|| format!("failed to commit forgetting scraper {name:?}"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn setup() -> (SqlitePool, StateSqliteStorage) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        (pool.clone(), StateSqliteStorage::new(pool))
    }

    /// Saves a session, a cookie and a report under `name`.
    async fn keep_state(pool: &SqlitePool, name: &str) {
        SessionSqliteStorage::new(pool.clone())
            .set(name, json!({"saved_urls": ["https://example.com/a.pdf"]}))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scraper_cookies (scraper_name, origin, set_cookie) VALUES (?1, ?2, ?3)",
        )
        .bind(name)
        .bind("https://example.com")
        .bind("sid=abc")
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scraper_reports (scraper_name, started_at_ms, entries) VALUES (?1, 0, '[]')",
        )
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    /// How many rows each of the session, cookies and reports tables holds
    /// for `name`.
    async fn rows_of(pool: &SqlitePool, name: &str) -> [i64; 3] {
        let mut counts = [0; 3];
        for (count, statement) in counts.iter_mut().zip([
            "SELECT COUNT(*) FROM scraper_sessions WHERE scraper_name = ?1",
            "SELECT COUNT(*) FROM scraper_cookies WHERE scraper_name = ?1",
            "SELECT COUNT(*) FROM scraper_reports WHERE scraper_name = ?1",
        ]) {
            *count = sqlx::query_scalar(statement)
                .bind(name)
                .fetch_one(pool)
                .await
                .unwrap();
        }
        counts
    }

    #[tokio::test]
    async fn delete_session_forgets_only_that_names_session() {
        let (pool, storage) = setup().await;
        keep_state(&pool, "alice").await;
        keep_state(&pool, "bob").await;

        storage.delete_session("alice").await.unwrap();

        assert_eq!(rows_of(&pool, "alice").await, [0, 1, 1]);
        assert_eq!(rows_of(&pool, "bob").await, [1, 1, 1]);
    }

    #[tokio::test]
    async fn delete_all_forgets_everything_kept_under_that_name_only() {
        let (pool, storage) = setup().await;
        keep_state(&pool, "alice").await;
        keep_state(&pool, "bob").await;

        storage.delete_all("alice").await.unwrap();
        storage.delete_all("never-ran").await.unwrap();

        assert_eq!(rows_of(&pool, "alice").await, [0, 0, 0]);
        assert_eq!(rows_of(&pool, "bob").await, [1, 1, 1]);
    }
}
