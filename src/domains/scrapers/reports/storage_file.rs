use std::path::PathBuf;

use async_trait::async_trait;

use crate::{ErrorContext as _, Result};

use super::models::Report;
use super::storage::Storage;

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
            report.scraper_name, report.started_at_ms
        ))
    }
}

#[async_trait]
impl Storage for FileStorage {
    async fn save(&self, report: &Report) -> Result<()> {
        let path = self.path(report);

        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| {
                format!("failed to create reports directory {}", self.dir.display())
            })?;

        let contents = serde_json::to_string_pretty(report).with_context(|| {
            format!(
                "failed to serialize report for scraper {:?}",
                report.scraper_name
            )
        })?;

        tokio::fs::write(&path, contents)
            .await
            .with_context(|| format!("failed to write report file {}", path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{FakeReport, ReportEntry};
    use super::*;

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
            .join(format!("didaxis_{}.json", report.started_at_ms));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn save_writes_the_full_report_as_json() {
        let (tmp, storage) = setup();
        let entries = vec![ReportEntry {
            ts_ms: 123,
            event: "log".to_string(),
            value: serde_json::json!({ "level": "info", "message": "hello" }),
        }];
        let report = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_entries(entries)
            .build();

        storage.save(&report).await.unwrap();

        let path = tmp
            .path()
            .join(format!("didaxis_{}.json", report.started_at_ms));
        let saved: Report =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved, report);
    }

    #[tokio::test]
    async fn save_creates_the_directory_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("reports");
        let storage = FileStorage::new(dir.clone());
        let report = FakeReport::new().with_scraper_name("didaxis").build();

        storage.save(&report).await.unwrap();

        assert!(
            dir.join(format!("didaxis_{}.json", report.started_at_ms))
                .exists()
        );
    }

    #[tokio::test]
    async fn saving_two_runs_for_the_same_scraper_keeps_both_files() {
        let (tmp, storage) = setup();
        let first = FakeReport::new().with_scraper_name("didaxis").build();
        let mut second = FakeReport::new().with_scraper_name("didaxis").build();
        second.started_at_ms = first.started_at_ms + 1;

        storage.save(&first).await.unwrap();
        storage.save(&second).await.unwrap();

        assert!(
            tmp.path()
                .join(format!("didaxis_{}.json", first.started_at_ms))
                .exists()
        );
        assert!(
            tmp.path()
                .join(format!("didaxis_{}.json", second.started_at_ms))
                .exists()
        );
    }
}
