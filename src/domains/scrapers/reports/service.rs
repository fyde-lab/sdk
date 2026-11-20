use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use crate::{ErrorCode, ErrorContext as _, Result};

use super::Service;
use super::models::{Report, ReportSummary, ScraperRun, ScraperRunOutcome, ScraperRunUpdate};
use super::recorder::{Recorder, now_ms};
use super::storage::Storage;
use crate::domains::scrapers::{ProgressEvent, ProgressStep};

/// The default [`Service`] implementation, delegating persistence to an
/// injected [`Storage`]. `going_on` holds the id of every run started
/// through it that hasn't finished: a stored `Running` report missing from
/// it was cut short by its process ending, so reads as
/// [`ScraperRunOutcome::Interrupted`].
pub(super) struct ReportsClient<S: Storage> {
    storage: S,
    going_on: Arc<Mutex<HashSet<Uuid>>>,
}

impl<S: Storage> ReportsClient<S> {
    pub(super) fn new(storage: S) -> Self {
        Self {
            storage,
            going_on: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

/// A run [`Service::start`] opened a report for, to hand back to
/// [`Service::finish`] once it ends. Its [`Self::recorder`] collects the
/// run's log, its [`Self::progress`] how far it got. Dropped without being
/// finished (the run's future was dropped), the run reads as interrupted.
pub(in crate::domains::scrapers) struct RunTracker {
    id: Uuid,
    recorder: Arc<Recorder>,
    progress: Arc<RunProgress>,
    going_on: Arc<Mutex<HashSet<Uuid>>>,
}

impl RunTracker {
    /// Where the run's host functions record its debug log.
    pub(in crate::domains::scrapers) fn recorder(&self) -> Arc<Recorder> {
        self.recorder.clone()
    }

    /// What the run's progress callbacks feed, from its blocking thread.
    pub(in crate::domains::scrapers) fn progress(&self) -> Arc<RunProgress> {
        self.progress.clone()
    }
}

impl Drop for RunTracker {
    fn drop(&mut self) {
        self.going_on
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.id);
    }
}

/// How far a run has got, as its `fyde.progress` calls and saved documents
/// tell it.
#[derive(Default)]
pub(in crate::domains::scrapers) struct RunProgress {
    state: Mutex<ProgressState>,
}

#[derive(Default, Clone, Copy)]
struct ProgressState {
    step: Option<ProgressStep>,
    update: Option<ScraperRunUpdate>,
    found: Option<i64>,
    saved: u64,
}

impl RunProgress {
    /// Notes a `fyde.progress` call. A step clears the previous step's
    /// update; an update's total, while listing or downloading documents,
    /// is how many the run found.
    pub(in crate::domains::scrapers) fn record(&self, event: &ProgressEvent) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match *event {
            ProgressEvent::Step { step } => {
                state.step = Some(step);
                state.update = None;
            }
            ProgressEvent::Update { current, total } => {
                state.update = Some(ScraperRunUpdate { current, total });
                let counting = matches!(
                    state.step,
                    Some(ProgressStep::ListingDocuments | ProgressStep::DownloadingDocuments)
                );
                if counting && total > 0 {
                    state.found = Some(total);
                }
            }
        }
    }

    /// Notes a document saved that the vault didn't hold yet.
    pub(in crate::domains::scrapers) fn document_saved(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).saved += 1;
    }

    fn snapshot(&self) -> ProgressState {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl RunTracker {
    /// The run's report as it stands: `outcome`, ended at `ended_at_ms`.
    fn report(&self, outcome: ScraperRunOutcome, ended_at_ms: Option<i64>) -> Report {
        let progress = self.progress.snapshot();
        Report {
            id: self.id,
            run: ScraperRun {
                name: self.recorder.scraper_name().to_string(),
                started_at_ms: self.recorder.started_at_ms(),
                ended_at_ms,
                outcome,
                step: progress.step,
                update: progress.update,
                found: progress.found,
                saved: progress.saved,
            },
            entries: self.recorder.entries(),
        }
    }
}

#[async_trait]
impl<S: Storage> Service for ReportsClient<S> {
    async fn start(&self, name: &str) -> Result<RunTracker> {
        let tracker = RunTracker {
            id: Uuid::now_v7(),
            recorder: Arc::new(Recorder::new(name)),
            progress: Arc::default(),
            going_on: self.going_on.clone(),
        };
        self.storage
            .save(&tracker.report(ScraperRunOutcome::Running, None))
            .await
            .with_context(|| format!("failed to open a report for scraper {name:?}'s run"))?;
        self.going_on
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(tracker.id);
        tracker
            .recorder
            .record("run_started", json!({ "script": name }));
        Ok(tracker)
    }

    async fn finish(&self, tracker: RunTracker, result: &Result<()>) -> Result<()> {
        let outcome = match result {
            Ok(()) => {
                tracker
                    .recorder
                    .record("run_finished", json!({ "status": "ok" }));
                ScraperRunOutcome::Succeeded
            }
            Err(err) if err.code() == ErrorCode::ScraperRunStopped => {
                tracker.recorder.record("run_stopped", json!({}));
                ScraperRunOutcome::Stopped
            }
            Err(err) => {
                tracker.recorder.record(
                    "error",
                    json!({ "action": "run", "message": err.to_string() }),
                );
                ScraperRunOutcome::Failed { code: err.code() }
            }
        };
        self.storage
            .save(&tracker.report(outcome, Some(now_ms())))
            .await
            .with_context(|| {
                format!(
                    "failed to save the report of scraper {:?}'s run",
                    tracker.recorder.scraper_name()
                )
            })
    }

    async fn last_runs(&self) -> Result<Vec<ScraperRun>> {
        let latest = self
            .storage
            .list_latest()
            .await
            .context("failed to list the latest scraper reports")?;
        let going_on = self.going_on.lock().unwrap_or_else(|p| p.into_inner());
        let mut runs: Vec<ScraperRun> = latest
            .into_iter()
            .map(|ReportSummary { id, mut run }| {
                if run.outcome == ScraperRunOutcome::Running && !going_on.contains(&id) {
                    run.outcome = ScraperRunOutcome::Interrupted;
                }
                run
            })
            .collect();
        runs.sort_by(|a, b| {
            b.started_at_ms
                .cmp(&a.started_at_ms)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::FakeReport;
    use super::super::storage::{MemoryStorage, MockStorage};
    use super::*;
    use crate::{Error, ErrorKind};

    fn client() -> ReportsClient<MemoryStorage> {
        ReportsClient::new(MemoryStorage::default())
    }

    #[tokio::test]
    async fn start_opens_a_running_report_and_records_the_start() {
        let client = client();

        let tracker = client.start("alice").await.unwrap();

        let saved = client.storage.reports();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].run.name, "alice");
        assert_eq!(saved[0].run.outcome, ScraperRunOutcome::Running);
        assert_eq!(saved[0].run.ended_at_ms, None);
        assert_eq!(tracker.recorder().entries()[0].event, "run_started");
        let runs = client.last_runs().await.unwrap();
        assert_eq!(runs[0].outcome, ScraperRunOutcome::Running);
        drop(tracker);
    }

    #[tokio::test]
    async fn finish_saves_how_the_run_ended_with_its_log() {
        let cases: [(Result<()>, ScraperRunOutcome, &str); 3] = [
            (Ok(()), ScraperRunOutcome::Succeeded, "run_finished"),
            (
                Err(ErrorKind::ScraperRunStopped.into()),
                ScraperRunOutcome::Stopped,
                "run_stopped",
            ),
            (
                Err(Error::from(ErrorKind::InvalidCredentials)),
                ScraperRunOutcome::Failed {
                    code: ErrorCode::InvalidCredentials,
                },
                "error",
            ),
        ];
        for (result, expected, last_event) in cases {
            let client = client();
            let tracker = client.start("alice").await.unwrap();
            tracker.recorder().record("log", json!({ "message": "hi" }));

            client.finish(tracker, &result).await.unwrap();

            let saved = client.storage.reports();
            assert_eq!(saved.len(), 1, "a run keeps a single report");
            let report = &saved[0];
            assert_eq!(report.run.outcome, expected);
            assert!(report.run.ended_at_ms.unwrap() >= report.run.started_at_ms);
            let events: Vec<&str> = report.entries.iter().map(|e| e.event.as_str()).collect();
            assert_eq!(events, ["run_started", "log", last_event]);
        }
    }

    #[tokio::test]
    async fn finish_saves_the_last_step_update_found_and_saved_counts() {
        let client = client();
        let tracker = client.start("alice").await.unwrap();
        let progress = tracker.progress();

        progress.record(&ProgressEvent::Step {
            step: ProgressStep::ListingDocuments,
        });
        progress.record(&ProgressEvent::Update {
            current: 0,
            total: 12,
        });
        progress.record(&ProgressEvent::Step {
            step: ProgressStep::DownloadingDocuments,
        });
        progress.record(&ProgressEvent::Update {
            current: 3,
            total: 12,
        });
        progress.document_saved();
        progress.document_saved();
        client.finish(tracker, &Ok(())).await.unwrap();

        let run = client.last_runs().await.unwrap().remove(0);
        assert_eq!(run.step, Some(ProgressStep::DownloadingDocuments));
        assert_eq!(
            run.update,
            Some(ScraperRunUpdate {
                current: 3,
                total: 12
            })
        );
        assert_eq!(run.found, Some(12));
        assert_eq!(run.saved, 2);
    }

    #[test]
    fn a_step_clears_the_update_and_only_listing_or_downloading_counts_as_found() {
        let progress = RunProgress::default();

        progress.record(&ProgressEvent::Step {
            step: ProgressStep::LoggingIn,
        });
        progress.record(&ProgressEvent::Update {
            current: 1,
            total: 3,
        });
        assert_eq!(progress.snapshot().found, None);

        progress.record(&ProgressEvent::Step {
            step: ProgressStep::ListingDocuments,
        });
        progress.record(&ProgressEvent::Update {
            current: 0,
            total: 0,
        });
        assert_eq!(progress.snapshot().found, None);

        progress.record(&ProgressEvent::Update {
            current: 0,
            total: 7,
        });
        progress.record(&ProgressEvent::Step {
            step: ProgressStep::Done,
        });
        let state = progress.snapshot();
        assert_eq!(state.step, Some(ProgressStep::Done));
        assert_eq!(state.update, None);
        assert_eq!(state.found, Some(7));
    }

    #[tokio::test]
    async fn a_run_dropped_without_finishing_reads_as_interrupted() {
        let client = client();

        drop(client.start("alice").await.unwrap());

        let run = client.last_runs().await.unwrap().remove(0);
        assert_eq!(run.outcome, ScraperRunOutcome::Interrupted);
        assert_eq!(run.ended_at_ms, None);
    }

    #[tokio::test]
    async fn a_run_left_running_by_another_process_reads_as_interrupted() {
        let storage = MemoryStorage::default();
        let left = FakeReport::new()
            .with_scraper_name("alice")
            .running()
            .build();
        storage.save(&left).await.unwrap();
        let finished = FakeReport::new().with_scraper_name("bob").build();
        storage.save(&finished).await.unwrap();
        let client = ReportsClient::new(storage);

        let mut runs = client.last_runs().await.unwrap();
        runs.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(runs[0].outcome, ScraperRunOutcome::Interrupted);
        assert_eq!(runs[1], finished.run);
    }

    #[tokio::test]
    async fn last_runs_lists_each_scrapers_latest_latest_first() {
        let storage = MemoryStorage::default();
        for (name, started_at_ms) in [("alice", 1_000), ("alice", 3_000), ("bob", 2_000)] {
            storage
                .save(
                    &FakeReport::new()
                        .with_scraper_name(name)
                        .with_started_at_ms(started_at_ms)
                        .build(),
                )
                .await
                .unwrap();
        }
        let client = ReportsClient::new(storage);

        let runs: Vec<(String, i64)> = client
            .last_runs()
            .await
            .unwrap()
            .into_iter()
            .map(|run| (run.name, run.started_at_ms))
            .collect();

        assert_eq!(
            runs,
            [("alice".to_string(), 3_000), ("bob".to_string(), 2_000)]
        );
    }

    #[tokio::test]
    async fn each_run_keeps_its_own_report() {
        let client = client();
        let first = client.start("alice").await.unwrap();
        client.finish(first, &Ok(())).await.unwrap();

        let second = client.start("alice").await.unwrap();

        assert_eq!(client.storage.reports().len(), 2);
        let runs = client.last_runs().await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].outcome, ScraperRunOutcome::Running);
        drop(second);
    }

    #[tokio::test]
    async fn start_fails_with_context_when_the_storage_does() {
        let mut storage = MockStorage::new();
        storage
            .expect_save()
            .returning(|_| Err(std::io::Error::other("disk full").into()));
        let client = ReportsClient::new(storage);

        let err = client.start("alice").await.err().unwrap();

        assert!(err.has_context());
        assert!(client.going_on.lock().unwrap().is_empty());
    }
}
