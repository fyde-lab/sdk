use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::{ErrorContext as _, Result};

use super::models::Report;
use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `scraper_reports` table). Every call inserts a new row — unlike
/// `cookies`/`session`, a report is never replaced, since each run produces
/// its own.
pub(crate) struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn save(&self, report: &Report) -> Result<()> {
        let entries = serde_json::to_string(&report.entries).with_context(|| {
            format!(
                "failed to serialize report entries for scraper {:?}",
                report.scraper_name
            )
        })?;

        sqlx::query(
            "INSERT INTO scraper_reports (scraper_name, started_at_ms, entries) VALUES (?1, ?2, ?3)",
        )
        .bind(&report.scraper_name)
        .bind(report.started_at_ms)
        .bind(entries)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!(
                "failed to save report for scraper {:?} to local database",
                report.scraper_name
            )
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Row;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::super::models::{FakeReport, ReportEntry};
    use super::*;

    async fn setup() -> SqliteStorage {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteStorage::new(pool)
    }

    async fn saved_rows(storage: &SqliteStorage, scraper_name: &str) -> Vec<(i64, String)> {
        sqlx::query("SELECT started_at_ms, entries FROM scraper_reports WHERE scraper_name = ?1")
            .bind(scraper_name)
            .fetch_all(&storage.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get("started_at_ms"), row.get("entries")))
            .collect()
    }

    #[tokio::test]
    async fn save_inserts_a_row_for_the_report() {
        let storage = setup().await;
        let entries = vec![ReportEntry {
            ts_ms: 123,
            event: "log".to_string(),
            value: serde_json::json!({ "level": "info", "message": "hello" }),
        }];
        let report = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_entries(entries.clone())
            .build();

        storage.save(&report).await.unwrap();

        let rows = saved_rows(&storage, "didaxis").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, report.started_at_ms);
        assert_eq!(
            serde_json::from_str::<Vec<ReportEntry>>(&rows[0].1).unwrap(),
            entries
        );
    }

    #[tokio::test]
    async fn saving_two_runs_for_the_same_scraper_keeps_both_rows() {
        let storage = setup().await;
        let first = FakeReport::new().with_scraper_name("didaxis").build();
        let second = FakeReport::new().with_scraper_name("didaxis").build();

        storage.save(&first).await.unwrap();
        storage.save(&second).await.unwrap();

        assert_eq!(saved_rows(&storage, "didaxis").await.len(), 2);
    }

    #[tokio::test]
    async fn distinct_scrapers_are_stored_independently() {
        let storage = setup().await;
        let didaxis = FakeReport::new().with_scraper_name("didaxis").build();
        let impots = FakeReport::new().with_scraper_name("impots").build();

        storage.save(&didaxis).await.unwrap();
        storage.save(&impots).await.unwrap();

        assert_eq!(saved_rows(&storage, "didaxis").await.len(), 1);
        assert_eq!(saved_rows(&storage, "impots").await.len(), 1);
    }
}
