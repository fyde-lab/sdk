use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::models::{Report, ReportSummary, ScraperRun, ScraperRunUpdate};
use super::storage::{Storage, outcome_columns, outcome_from_columns};

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `scraper_reports` table, one row per run).
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
        let run = &report.run;
        let (outcome, error_code) = outcome_columns(run.outcome);
        let entries = serde_json::to_string(&report.entries).with_context(|| {
            format!(
                "failed to serialize report entries for scraper {:?}",
                run.name
            )
        })?;

        sqlx::query(
            "INSERT INTO scraper_reports
                 (id, scraper_name, started_at_ms, ended_at_ms, outcome, error_code, step,
                  update_current, update_total, found, saved, script_id, script_version,
                  entries)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                 scraper_name = excluded.scraper_name,
                 started_at_ms = excluded.started_at_ms,
                 ended_at_ms = excluded.ended_at_ms,
                 outcome = excluded.outcome,
                 error_code = excluded.error_code,
                 step = excluded.step,
                 update_current = excluded.update_current,
                 update_total = excluded.update_total,
                 found = excluded.found,
                 saved = excluded.saved,
                 script_id = excluded.script_id,
                 script_version = excluded.script_version,
                 entries = excluded.entries",
        )
        .bind(report.id.to_string())
        .bind(&run.name)
        .bind(run.started_at_ms)
        .bind(run.ended_at_ms)
        .bind(outcome)
        .bind(error_code)
        .bind(run.step.map(|step| step.as_str()))
        .bind(run.update.map(|update| update.current))
        .bind(run.update.map(|update| update.total))
        .bind(run.found)
        // SQLite has no unsigned integers; no run saves 2^63 documents.
        .bind(i64::try_from(run.saved).unwrap_or(i64::MAX))
        .bind(report.script.map(|script| script.id.to_string()))
        // A script's version never reaches 2^63 either.
        .bind(
            report
                .script
                .map(|script| i64::try_from(script.version).unwrap_or(i64::MAX)),
        )
        .bind(entries)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!(
                "failed to save report for scraper {:?} to local database",
                run.name
            )
        })?;

        Ok(())
    }

    async fn list_latest(&self) -> Result<Vec<ReportSummary>> {
        let rows = sqlx::query(
            "SELECT id, scraper_name, started_at_ms, ended_at_ms, outcome, error_code, step,
                    update_current, update_total, found, saved
             FROM (
                 SELECT *, ROW_NUMBER() OVER (
                     PARTITION BY scraper_name ORDER BY started_at_ms DESC, id DESC
                 ) AS recency
                 FROM scraper_reports
             )
             WHERE recency = 1",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list the latest scraper reports from local database")?;

        rows.iter().map(summary_from_row).collect()
    }
}

fn summary_from_row(row: &SqliteRow) -> Result<ReportSummary> {
    let name: String = row.get("scraper_name");
    let id: String = row.get("id");
    let id = Uuid::parse_str(&id)
        .with_context(|| format!("failed to parse the id of a report of scraper {name:?}"))?;
    let outcome: String = row.get("outcome");
    let outcome = outcome_from_columns(&name, &outcome, row.get("error_code"))?;
    let step = row
        .get::<Option<String>, _>("step")
        .map(|step| step.parse())
        .transpose()
        .with_context(|| format!("failed to parse the step of a report of scraper {name:?}"))?;
    let update = match (row.get("update_current"), row.get("update_total")) {
        (Some(current), Some(total)) => Some(ScraperRunUpdate { current, total }),
        _ => None,
    };
    let saved: i64 = row.get("saved");

    Ok(ReportSummary {
        id,
        run: ScraperRun {
            started_at_ms: row.get("started_at_ms"),
            ended_at_ms: row.get("ended_at_ms"),
            outcome,
            step,
            update,
            found: row.get("found"),
            saved: u64::try_from(saved).unwrap_or(0),
            name,
        },
    })
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::super::models::{FakeReport, ReportEntry, ReportedScript, ScraperRunOutcome};
    use super::*;
    use crate::ErrorCode;
    use crate::domains::scrapers::ProgressStep;

    async fn setup() -> SqliteStorage {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteStorage::new(pool)
    }

    async fn saved_script(storage: &SqliteStorage, id: Uuid) -> (Option<String>, Option<i64>) {
        let row =
            sqlx::query("SELECT script_id, script_version FROM scraper_reports WHERE id = ?1")
                .bind(id.to_string())
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        (row.get("script_id"), row.get("script_version"))
    }

    async fn saved_entries(storage: &SqliteStorage, scraper_name: &str) -> Vec<Vec<ReportEntry>> {
        sqlx::query("SELECT entries FROM scraper_reports WHERE scraper_name = ?1")
            .bind(scraper_name)
            .fetch_all(&storage.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| serde_json::from_str(&row.get::<String, _>("entries")).unwrap())
            .collect()
    }

    fn sorted(mut summaries: Vec<ReportSummary>) -> Vec<ReportSummary> {
        summaries.sort_by(|a, b| a.run.name.cmp(&b.run.name));
        summaries
    }

    #[tokio::test]
    async fn list_latest_is_empty_when_nothing_ran() {
        let storage = setup().await;

        assert_eq!(storage.list_latest().await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn save_keeps_the_run_and_its_log() {
        let storage = setup().await;
        let entries = vec![ReportEntry {
            ts_ms: 123,
            event: "log".to_string(),
            value: serde_json::json!({ "level": "info", "message": "hello" }),
        }];
        let report = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_entries(entries.clone())
            .with_progress(
                ProgressStep::Done,
                Some(ScraperRunUpdate {
                    current: 12,
                    total: 12,
                }),
                Some(12),
                3,
            )
            .build();

        storage.save(&report).await.unwrap();

        assert_eq!(saved_entries(&storage, "didaxis").await, vec![entries]);
        assert_eq!(
            storage.list_latest().await.unwrap(),
            vec![ReportSummary::from(&report)]
        );
    }

    #[tokio::test]
    async fn save_keeps_the_script_that_ran_if_known() {
        let storage = setup().await;
        let script = ReportedScript {
            id: Uuid::new_v4(),
            version: 7,
        };
        let installed = FakeReport::new().with_script(Some(script)).build();
        let bare = FakeReport::new().with_script(None).build();

        storage.save(&installed).await.unwrap();
        storage.save(&bare).await.unwrap();

        assert_eq!(
            saved_script(&storage, installed.id).await,
            (Some(script.id.to_string()), Some(7))
        );
        assert_eq!(saved_script(&storage, bare.id).await, (None, None));
    }

    #[tokio::test]
    async fn every_outcome_reads_back_as_saved() {
        let storage = setup().await;
        let reports = [
            FakeReport::new()
                .with_scraper_name("alice")
                .running()
                .build(),
            FakeReport::new().with_scraper_name("bob").build(),
            FakeReport::new()
                .with_scraper_name("carol")
                .with_outcome(ScraperRunOutcome::Failed {
                    code: ErrorCode::InvalidCredentials,
                })
                .with_progress(ProgressStep::LoggingIn, None, None, 0)
                .build(),
            FakeReport::new()
                .with_scraper_name("dave")
                .with_outcome(ScraperRunOutcome::Stopped)
                .build(),
        ];
        for report in &reports {
            storage.save(report).await.unwrap();
        }

        assert_eq!(
            sorted(storage.list_latest().await.unwrap()),
            sorted(reports.iter().map(ReportSummary::from).collect())
        );
    }

    #[tokio::test]
    async fn saving_a_run_again_replaces_its_report_only() {
        let storage = setup().await;
        let started = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_entries(Vec::new())
            .running()
            .build();
        let other = FakeReport::new().with_scraper_name("impots").build();
        storage.save(&started).await.unwrap();
        storage.save(&other).await.unwrap();
        let mut finished = started.clone();
        finished.run.outcome = ScraperRunOutcome::Succeeded;
        finished.run.ended_at_ms = Some(started.run.started_at_ms + 1_000);
        finished.entries = FakeReport::new().build().entries;

        storage.save(&finished).await.unwrap();

        assert_eq!(
            saved_entries(&storage, "didaxis").await,
            vec![finished.entries.clone()]
        );
        assert_eq!(
            sorted(storage.list_latest().await.unwrap()),
            vec![ReportSummary::from(&finished), ReportSummary::from(&other)]
        );
    }

    #[tokio::test]
    async fn every_run_keeps_its_report_and_the_latest_is_listed() {
        let storage = setup().await;
        let first = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_started_at_ms(1_000)
            .build();
        let second = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_started_at_ms(2_000)
            .with_outcome(ScraperRunOutcome::Stopped)
            .build();
        storage.save(&second).await.unwrap();
        storage.save(&first).await.unwrap();

        assert_eq!(saved_entries(&storage, "didaxis").await.len(), 2);
        assert_eq!(
            storage.list_latest().await.unwrap(),
            vec![ReportSummary::from(&second)]
        );
    }
}
