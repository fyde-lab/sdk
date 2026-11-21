use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::models::{
    Report, ReportEntry, ReportSummary, ReportedScript, ScraperRun, ScraperRunUpdate,
};
use super::storage::{Storage, outcome_columns, outcome_from_columns};

/// A [`Storage`] backed by one JSON file per run under `dir`, the way
/// `demo-rust-fyde` persisted its debug report (as
/// `<report_dir>/<scraper_name>_<timestamp>.json`) before this sub-domain
/// existed. Meant for `../scripts`' standalone CLI runner, which has no
/// local SQLite database of its own — see [`crate::init_dev_scrapers`].
pub(crate) struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, report: &Report) -> PathBuf {
        self.dir.join(format!(
            "{}_{}.json",
            report.run.name, report.run.started_at_ms
        ))
    }
}

/// A [`Report`] as its file holds it.
#[derive(Serialize, Deserialize)]
struct ReportFile {
    id: Uuid,
    scraper_name: String,
    started_at_ms: i64,
    ended_at_ms: Option<i64>,
    outcome: String,
    error_code: Option<u32>,
    step: Option<String>,
    update: Option<(i64, i64)>,
    found: Option<i64>,
    saved: u64,
    /// Absent from files written before reports kept their script.
    #[serde(default)]
    script: Option<ReportedScript>,
    entries: Vec<ReportEntry>,
}

impl ReportFile {
    fn summary(self) -> Result<ReportSummary> {
        let name = self.scraper_name;
        let outcome = outcome_from_columns(&name, &self.outcome, self.error_code)?;
        let step = self
            .step
            .map(|step| step.parse())
            .transpose()
            .with_context(|| format!("failed to parse the step of a report of scraper {name:?}"))?;

        Ok(ReportSummary {
            id: self.id,
            run: ScraperRun {
                started_at_ms: self.started_at_ms,
                ended_at_ms: self.ended_at_ms,
                outcome,
                step,
                update: self
                    .update
                    .map(|(current, total)| ScraperRunUpdate { current, total }),
                found: self.found,
                saved: self.saved,
                name,
            },
        })
    }
}

#[async_trait]
impl Storage for FileStorage {
    async fn save(&self, report: &Report) -> Result<()> {
        let path = self.path(report);
        let run = &report.run;
        let (outcome, error_code) = outcome_columns(run.outcome);
        let file = ReportFile {
            id: report.id,
            scraper_name: run.name.clone(),
            started_at_ms: run.started_at_ms,
            ended_at_ms: run.ended_at_ms,
            outcome: outcome.to_string(),
            error_code,
            step: run.step.map(|step| step.as_str().to_string()),
            update: run.update.map(|update| (update.current, update.total)),
            found: run.found,
            saved: run.saved,
            script: report.script,
            entries: report.entries.clone(),
        };

        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| {
                format!("failed to create reports directory {}", self.dir.display())
            })?;

        let contents = serde_json::to_string_pretty(&file)
            .with_context(|| format!("failed to serialize report for scraper {:?}", run.name))?;

        tokio::fs::write(&path, contents)
            .await
            .with_context(|| format!("failed to write report file {}", path.display()))?;

        Ok(())
    }

    async fn list_latest(&self) -> Result<Vec<ReportSummary>> {
        let mut dir = match tokio::fs::read_dir(&self.dir).await {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            dir => dir.with_context(|| {
                format!("failed to list reports directory {}", self.dir.display())
            })?,
        };

        let mut latest: HashMap<String, ReportSummary> = HashMap::new();
        while let Some(entry) = dir
            .next_entry()
            .await
            .with_context(|| format!("failed to list reports directory {}", self.dir.display()))?
        {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let text = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("failed to read report file {}", path.display()))?;
            let file: ReportFile = serde_json::from_str(&text)
                .with_context(|| format!("failed to parse report file {}", path.display()))?;
            let summary = file.summary()?;
            let newer = latest.get(&summary.run.name).is_none_or(|kept| {
                (summary.run.started_at_ms, summary.id) > (kept.run.started_at_ms, kept.id)
            });
            if newer {
                latest.insert(summary.run.name.clone(), summary);
            }
        }

        Ok(latest.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{FakeReport, ScraperRunOutcome};
    use super::*;
    use crate::ErrorCode;

    fn setup() -> (tempfile::TempDir, FileStorage) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FileStorage::new(tmp.path().to_path_buf());
        (tmp, storage)
    }

    #[tokio::test]
    async fn save_writes_a_file_named_after_the_scraper_and_start_time() {
        let (tmp, storage) = setup();
        let report = FakeReport::new().with_scraper_name("didaxis").build();

        storage.save(&report).await.unwrap();

        let path = tmp
            .path()
            .join(format!("didaxis_{}.json", report.run.started_at_ms));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn save_writes_the_full_log_as_json() {
        let (tmp, storage) = setup();
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

        let path = tmp
            .path()
            .join(format!("didaxis_{}.json", report.run.started_at_ms));
        let saved: ReportFile =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved.entries, entries);
        assert_eq!(saved.script, report.script);
        assert_eq!(saved.summary().unwrap(), ReportSummary::from(&report));
    }

    #[test]
    fn a_file_written_before_reports_kept_their_script_still_reads() {
        let file: ReportFile = serde_json::from_value(serde_json::json!({
            "id": Uuid::now_v7(),
            "scraper_name": "didaxis",
            "started_at_ms": 1_000,
            "ended_at_ms": 2_000,
            "outcome": "succeeded",
            "error_code": null,
            "step": null,
            "update": null,
            "found": null,
            "saved": 0,
            "entries": [],
        }))
        .unwrap();

        assert_eq!(file.script, None);
    }

    #[tokio::test]
    async fn save_creates_the_directory_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("reports");
        let storage = FileStorage::new(dir.clone());
        let report = FakeReport::new().with_scraper_name("didaxis").build();

        storage.save(&report).await.unwrap();

        assert!(
            dir.join(format!("didaxis_{}.json", report.run.started_at_ms))
                .exists()
        );
    }

    #[tokio::test]
    async fn list_latest_is_empty_when_the_directory_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FileStorage::new(tmp.path().join("reports"));

        assert_eq!(storage.list_latest().await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn every_run_keeps_its_file_and_the_latest_of_each_scraper_is_listed() {
        let (tmp, storage) = setup();
        let first = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_started_at_ms(1_000)
            .build();
        let second = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_started_at_ms(2_000)
            .with_outcome(ScraperRunOutcome::Failed {
                code: ErrorCode::ScraperSiteUnreachable,
            })
            .build();
        let other = FakeReport::new().with_scraper_name("impots").build();
        for report in [&first, &second, &other] {
            storage.save(report).await.unwrap();
        }

        assert!(tmp.path().join("didaxis_1000.json").exists());
        let mut latest = storage.list_latest().await.unwrap();
        latest.sort_by(|a, b| a.run.name.cmp(&b.run.name));
        assert_eq!(
            latest,
            vec![ReportSummary::from(&second), ReportSummary::from(&other)]
        );
    }

    #[tokio::test]
    async fn saving_a_run_again_rewrites_its_own_file() {
        let (tmp, storage) = setup();
        let started = FakeReport::new()
            .with_scraper_name("didaxis")
            .running()
            .build();
        storage.save(&started).await.unwrap();
        let mut finished = started.clone();
        finished.run.outcome = ScraperRunOutcome::Stopped;
        finished.run.ended_at_ms = Some(started.run.started_at_ms + 1);

        storage.save(&finished).await.unwrap();

        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
        assert_eq!(
            storage.list_latest().await.unwrap(),
            vec![ReportSummary::from(&finished)]
        );
    }
}
