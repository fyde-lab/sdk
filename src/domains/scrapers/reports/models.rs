use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::ErrorCode;
use crate::domains::scrapers::ProgressStep;

/// One `(event_type, value)` entry recorded during a scraper run — e.g. a
/// log call, an HTTP request, a browser step, a progress update, an input
/// prompt or an error.
/// Ported from `demo-rust-fyde`'s `report.rs`, whose `Report::record` built
/// exactly this shape before writing it straight to disk; here it's just
/// data, collected by `super::Recorder` and kept in its run's [`Report`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReportEntry {
    pub(crate) ts_ms: i64,
    pub(crate) event: String,
    pub(crate) value: Value,
}

/// How a scraper run went, as [`crate::ScrapersService::last_runs`] returns
/// it from its [`Report`]: when it ran, how it ended and how far it got.
/// Kept on this device only (never synced), and forgotten with the rest of
/// a scraper's state when its installation is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScraperRun {
    /// The name it ran under: its installation's id, for
    /// [`crate::ScrapersService::run_installed`].
    pub name: String,
    /// When it started, in milliseconds since the Unix epoch.
    pub started_at_ms: i64,
    /// When it ended, in milliseconds since the Unix epoch: `None` while
    /// it's [`ScraperRunOutcome::Running`] or if it was
    /// [`ScraperRunOutcome::Interrupted`].
    pub ended_at_ms: Option<i64>,
    pub outcome: ScraperRunOutcome,
    /// The last step it announced (`fyde.progress.step`), if any.
    pub step: Option<ProgressStep>,
    /// Its last `fyde.progress.update` since that step, if any.
    pub update: Option<ScraperRunUpdate>,
    /// How many documents it found: the total of its last
    /// `fyde.progress.update` while listing or downloading documents,
    /// `None` if it never said.
    pub found: Option<i64>,
    /// How many documents it saved that the vault didn't hold yet (those
    /// already saved, which `fyde.save_document` skips, aren't counted).
    pub saved: u64,
}

/// How a [`ScraperRun`] ended, or that it hasn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScraperRunOutcome {
    /// It's going on now, in this process (or waiting for a slot to run).
    Running,
    Succeeded,
    /// It failed with `code` (see [`ErrorCode::user_message`] for its
    /// wording).
    Failed {
        code: ErrorCode,
    },
    /// It was stopped on request (`ScrapersService::stop`): not a failure,
    /// what it saved stays.
    Stopped,
    /// It never ended: the process running it was closed (or crashed)
    /// mid-run.
    Interrupted,
}

/// A run's `fyde.progress.update(current, total)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScraperRunUpdate {
    pub current: i64,
    pub total: i64,
}

/// One scraper run's report: how it went ([`ScraperRun`]) and its full
/// debug log, every [`ReportEntry`] recorded between the script starting and
/// finishing, in order (empty while it runs). Saved as the run starts, then
/// again once it ends. `id` tells it apart from other runs of the same
/// scraper. Its `run.outcome` is never [`ScraperRunOutcome::Interrupted`]:
/// that's read from a `Running` one no longer going on (see
/// `super::service::ReportsClient::last_runs`). The log is purely a
/// debugging aid — never read back by a scraper script itself. `script` is
/// the script that ran, when it's known: a run of an installation
/// (`run_installed`) knows it, one of a bare script source (`run`) doesn't.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Report {
    pub(crate) id: Uuid,
    pub(crate) run: ScraperRun,
    pub(crate) script: Option<ReportedScript>,
    pub(crate) entries: Vec<ReportEntry>,
}

/// The script a [`Report`]'s run ran: its id and the version it ran at, as
/// the server's `scripts.v1` lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReportedScript {
    pub(crate) id: Uuid,
    pub(crate) version: u64,
}

/// A [`Report`] without its log, as [`super::storage::Storage::list_latest`]
/// returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportSummary {
    pub(crate) id: Uuid,
    pub(crate) run: ScraperRun,
}

impl From<&Report> for ReportSummary {
    fn from(report: &Report) -> Self {
        Self {
            id: report.id,
            run: report.run.clone(),
        }
    }
}

/// Builds a [`Report`] filled with random-but-plausible data, overridable
/// field by field, for use in tests: a finished, successful run by default.
#[cfg(test)]
pub(crate) struct FakeReport {
    report: Report,
}

#[cfg(test)]
impl FakeReport {
    pub(crate) fn new() -> Self {
        let started_at_ms = crate::tools::testing::random_past_timestamp() * 1000;
        Self {
            report: Report {
                id: Uuid::now_v7(),
                run: ScraperRun {
                    name: crate::tools::testing::random_word().to_string(),
                    started_at_ms,
                    ended_at_ms: Some(started_at_ms + 60_000),
                    outcome: ScraperRunOutcome::Succeeded,
                    step: None,
                    update: None,
                    found: None,
                    saved: 0,
                },
                script: Some(ReportedScript {
                    id: Uuid::new_v4(),
                    version: 1,
                }),
                entries: vec![ReportEntry {
                    ts_ms: started_at_ms + 1,
                    event: "log".to_string(),
                    value: serde_json::json!({ "level": "info", "message": crate::tools::testing::random_word() }),
                }],
            },
        }
    }

    pub(crate) fn with_scraper_name(mut self, scraper_name: impl Into<String>) -> Self {
        self.report.run.name = scraper_name.into();
        self
    }

    pub(crate) fn with_started_at_ms(mut self, started_at_ms: i64) -> Self {
        self.report.run.started_at_ms = started_at_ms;
        self.report.run.ended_at_ms = self.report.run.ended_at_ms.map(|_| started_at_ms + 60_000);
        self
    }

    /// `None`: a run of a bare script source, whose script isn't known.
    pub(crate) fn with_script(mut self, script: Option<ReportedScript>) -> Self {
        self.report.script = script;
        self
    }

    pub(crate) fn with_entries(mut self, entries: Vec<ReportEntry>) -> Self {
        self.report.entries = entries;
        self
    }

    /// Still going on (or left so by a process that ended mid-run).
    pub(crate) fn running(mut self) -> Self {
        self.report.run.ended_at_ms = None;
        self.report.run.outcome = ScraperRunOutcome::Running;
        self
    }

    pub(crate) fn with_outcome(mut self, outcome: ScraperRunOutcome) -> Self {
        self.report.run.outcome = outcome;
        self
    }

    pub(crate) fn with_progress(
        mut self,
        step: ProgressStep,
        update: Option<ScraperRunUpdate>,
        found: Option<i64>,
        saved: u64,
    ) -> Self {
        self.report.run.step = Some(step);
        self.report.run.update = update;
        self.report.run.found = found;
        self.report.run.saved = saved;
        self
    }

    pub(crate) fn build(self) -> Report {
        self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_report_builds_with_plausible_defaults() {
        let report = FakeReport::new().build();

        assert!(!report.run.name.is_empty());
        assert!(report.run.ended_at_ms.unwrap() > report.run.started_at_ms);
        assert!(!report.entries.is_empty());
    }

    #[test]
    fn fake_report_with_methods_override_defaults() {
        let entries = vec![ReportEntry {
            ts_ms: 123,
            event: "http_request".to_string(),
            value: serde_json::json!({ "status": 200 }),
        }];

        let report = FakeReport::new()
            .with_scraper_name("didaxis")
            .with_entries(entries.clone())
            .running()
            .build();

        assert_eq!(report.run.name, "didaxis");
        assert_eq!(report.entries, entries);
        assert_eq!(report.run.outcome, ScraperRunOutcome::Running);
        assert_eq!(report.run.ended_at_ms, None);
    }
}
