use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::{ErrorCode, ErrorKind, Result};

use uuid::Uuid;

use super::models::{Report, ReportCursor, ReportSummary, ScraperRunOutcome};

/// Persists scraper runs' reports. Implementations are injected into
/// [`super::service::ReportsClient`] at construction.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Saves `report`, replacing the one saved under its id (the same run,
    /// saved as it started) if any. Each run has its own report: one of
    /// another run of the same scraper is never replaced.
    async fn save(&self, report: &Report) -> Result<()>;

    /// Each scraper's latest report (the one that started last), without
    /// its log, in no particular order.
    async fn list_latest(&self) -> Result<Vec<ReportSummary>>;
}

/// Keeps track of which reports were uploaded to the server (see
/// `super::service::ReportUploader`). Only the local SQLite database
/// implements it: `crate::init_dev_scrapers`' reports, kept as files, are
/// never uploaded.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait UploadStorage: Send + Sync {
    /// Up to `limit` reports not uploaded yet of runs that ended and whose
    /// script is known, with their log, by when they started then by id
    /// (oldest first), after `after` if given.
    async fn list_to_upload(&self, after: Option<ReportCursor>, limit: u32) -> Result<Vec<Report>>;

    /// Notes that the report `id` was uploaded, so it's never listed by
    /// [`Self::list_to_upload`] again. Saving its run again doesn't undo it.
    async fn mark_uploaded(&self, id: Uuid) -> Result<()>;
}

/// `outcome` as the `outcome` and `error_code` a stored report keeps.
/// `Interrupted` is never stored (see [`Report`]): kept as the `Running` it
/// was read from.
pub(super) fn outcome_columns(outcome: ScraperRunOutcome) -> (&'static str, Option<u32>) {
    match outcome {
        ScraperRunOutcome::Running | ScraperRunOutcome::Interrupted => ("running", None),
        ScraperRunOutcome::Succeeded => ("succeeded", None),
        ScraperRunOutcome::Failed { code } => ("failed", Some(code.as_u32())),
        ScraperRunOutcome::Stopped => ("stopped", None),
    }
}

/// The outcome a stored report's `outcome` and `error_code` mean, for
/// scraper `name`'s run.
pub(super) fn outcome_from_columns(
    name: &str,
    outcome: &str,
    error_code: Option<u32>,
) -> Result<ScraperRunOutcome> {
    match (outcome, error_code) {
        ("running", _) => Ok(ScraperRunOutcome::Running),
        ("succeeded", _) => Ok(ScraperRunOutcome::Succeeded),
        ("stopped", _) => Ok(ScraperRunOutcome::Stopped),
        ("failed", Some(value)) => match ErrorCode::from_u32(value) {
            Some(code) => Ok(ScraperRunOutcome::Failed { code }),
            None => Err(ErrorKind::InvalidScraperRun(format!(
                "unknown error code {value} in a report of scraper {name:?}"
            ))
            .into()),
        },
        (other, code) => Err(ErrorKind::InvalidScraperRun(format!(
            "unknown outcome {other:?} (error code {code:?}) in a report of scraper {name:?}"
        ))
        .into()),
    }
}

/// A [`Storage`] held in memory, for tests of the services built on reports.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryStorage {
    reports: std::sync::Mutex<Vec<Report>>,
}

#[cfg(test)]
impl MemoryStorage {
    /// Every report saved, in the order their runs were first saved.
    pub(crate) fn reports(&self) -> Vec<Report> {
        self.reports.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait]
impl Storage for MemoryStorage {
    async fn save(&self, report: &Report) -> Result<()> {
        let mut reports = self.reports.lock().unwrap();
        match reports.iter_mut().find(|saved| saved.id == report.id) {
            Some(saved) => *saved = report.clone(),
            None => reports.push(report.clone()),
        }
        Ok(())
    }

    async fn list_latest(&self) -> Result<Vec<ReportSummary>> {
        let mut latest: std::collections::HashMap<&str, &Report> = std::collections::HashMap::new();
        let reports = self.reports.lock().unwrap();
        for report in reports.iter() {
            let name = report.run.name.as_str();
            let newer = latest.get(name).is_none_or(|kept| {
                (report.run.started_at_ms, report.id) > (kept.run.started_at_ms, kept.id)
            });
            if newer {
                latest.insert(name, report);
            }
        }
        Ok(latest.into_values().map(ReportSummary::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_outcome_round_trips_through_its_columns() {
        for outcome in [
            ScraperRunOutcome::Running,
            ScraperRunOutcome::Succeeded,
            ScraperRunOutcome::Failed {
                code: ErrorCode::ScraperCredentialsRejected,
            },
            ScraperRunOutcome::Stopped,
        ] {
            let (text, code) = outcome_columns(outcome);
            assert_eq!(outcome_from_columns("a", text, code).unwrap(), outcome);
        }
    }

    #[test]
    fn an_interrupted_run_is_stored_as_running() {
        assert_eq!(
            outcome_columns(ScraperRunOutcome::Interrupted),
            ("running", None)
        );
    }

    #[test]
    fn an_unknown_outcome_or_error_code_is_refused() {
        assert!(outcome_from_columns("a", "exploded", None).is_err());
        assert!(outcome_from_columns("a", "failed", Some(0)).is_err());
        assert!(outcome_from_columns("a", "failed", None).is_err());
    }
}
