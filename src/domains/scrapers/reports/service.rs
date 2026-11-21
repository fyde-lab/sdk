use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use crate::{ErrorCode, ErrorContext as _, Result};

use super::grpc_client::FydeClient;
use super::models::{
    DeviceType, Report, ReportSummary, ReportUpload, ReportedScript, ScraperRun, ScraperRunOutcome,
    ScraperRunUpdate,
};
use super::recorder::{Recorder, now_ms};
use super::storage::{Storage, UploadStorage, outcome_columns};
use super::{Service, UploadWake};
use crate::domains::scrapers::{ProgressEvent, ProgressStep};

/// The default [`Service`] implementation, delegating persistence to an
/// injected [`Storage`]. `going_on` holds the id of every run started
/// through it that hasn't finished: a stored `Running` report missing from
/// it was cut short by its process ending, so reads as
/// [`ScraperRunOutcome::Interrupted`].
pub(super) struct ReportsClient<S: Storage> {
    storage: S,
    going_on: Arc<Mutex<HashSet<Uuid>>>,
    /// Notified each time a run's report is completed, for the uploads
    /// (see [`upload_reports`]) to look for it. `None` where reports are
    /// never uploaded.
    upload_wake: Option<Arc<UploadWake>>,
}

impl<S: Storage> ReportsClient<S> {
    pub(super) fn new(storage: S) -> Self {
        Self {
            storage,
            going_on: Arc::new(Mutex::new(HashSet::new())),
            upload_wake: None,
        }
    }

    /// Notifies `wake` each time a run's report is completed.
    pub(super) fn waking(mut self, wake: Arc<UploadWake>) -> Self {
        self.upload_wake = Some(wake);
        self
    }
}

/// A run [`Service::start`] opened a report for, to hand back to
/// [`Service::finish`] once it ends. Its [`Self::recorder`] collects the
/// run's log, its [`Self::progress`] how far it got. Dropped without being
/// finished (the run's future was dropped), the run reads as interrupted.
pub(in crate::domains::scrapers) struct RunTracker {
    id: Uuid,
    script: Option<ReportedScript>,
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
            script: self.script,
            entries: self.recorder.entries(),
        }
    }
}

#[async_trait]
impl<S: Storage> Service for ReportsClient<S> {
    async fn start(&self, name: &str, script: Option<ReportedScript>) -> Result<RunTracker> {
        let tracker = RunTracker {
            id: Uuid::now_v7(),
            script,
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
            })?;
        if let Some(wake) = &self.upload_wake {
            wake.notify_one();
        }
        Ok(())
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

/// The most a report's JSON may weigh: the server refuses one over 1 MiB.
pub(super) const MAX_REPORT_BYTES: usize = 1024 * 1024;

/// The longest run duration the server accepts. A longer run is uploaded
/// as lasting this long.
const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1000;

/// How many reports [`ReportUploader::upload_pending`] reads from storage
/// at a time.
const UPLOAD_BATCH: u32 = 16;

/// How long [`upload_reports`] waits after a pass failed (the server
/// unreachable, no session...) before trying again, unless a run ends
/// first.
pub(super) const UPLOAD_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// Uploads finished runs' reports to the server's `reports.v1`, which
/// links them to no user, each once. Only runs of a known script are
/// uploaded (the server needs its id and version), as
/// [`report_json`] words them. Each goes with its run's id, which the
/// server stores once: a report sent again (uploaded, but not noted so
/// before the process ended) isn't stored twice.
pub(super) struct ReportUploader<S: UploadStorage, C: FydeClient> {
    storage: S,
    client: C,
    device_type: DeviceType,
    client_id: String,
}

impl<S: UploadStorage, C: FydeClient> ReportUploader<S, C> {
    /// Uploads through `client` the reports `storage` holds, from the
    /// client application named `client_name` (see
    /// [`crate::ClientConfig::client_name`]), running on this device.
    pub(super) fn new(storage: S, client: C, client_name: &str) -> Self {
        Self {
            storage,
            client,
            device_type: this_device(),
            client_id: client_id(client_name),
        }
    }

    /// Uploads every report not uploaded yet, oldest first, and returns how
    /// many it uploaded. One the server refuses as invalid, or whose row
    /// can't be read, is left to try again on the next pass, and the pass
    /// goes on with the others: it's never dropped, and never holds them
    /// back. Any
    /// other failure to upload (the server unreachable, no session...)
    /// would fail the next ones too, so ends the pass with it.
    pub(super) async fn upload_pending(&self) -> Result<usize> {
        let mut uploaded = 0;
        let mut after = None;
        loop {
            let batch = self
                .storage
                .list_to_upload(after, UPLOAD_BATCH)
                .await
                .context("failed to list the scraper reports to upload")?;
            let Some(last) = batch.last() else {
                return Ok(uploaded);
            };
            after = Some(last.cursor.clone());
            let complete = batch.len() < UPLOAD_BATCH as usize;

            for listed in batch {
                let report = match listed.report {
                    Ok(report) => report,
                    Err(err) => {
                        tracing::warn!(
                            report = %listed.cursor.id,
                            "can't read scraper report, keeping it to try again: {err}"
                        );
                        continue;
                    }
                };
                let report = &report;
                let Some(upload) = self.upload_of(report).with_context(|| {
                    format!("failed to prepare scraper report {} for upload", report.id)
                })?
                else {
                    continue;
                };
                match self.client.upload_report(upload).await {
                    Ok(()) => {}
                    Err(err) if err.code() == ErrorCode::InvalidRequest => {
                        tracing::warn!(
                            report = %report.id,
                            "the server refused scraper report, keeping it to try again: {err}"
                        );
                        continue;
                    }
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!("failed to upload scraper report {}", report.id)
                        });
                    }
                }
                self.storage
                    .mark_uploaded(report.id)
                    .await
                    .with_context(|| {
                        format!("failed to note scraper report {} uploaded", report.id)
                    })?;
                uploaded += 1;
            }

            if complete {
                return Ok(uploaded);
            }
        }
    }

    /// `report` as it's uploaded, `None` if its script isn't known (never
    /// listed to upload).
    fn upload_of(&self, report: &Report) -> Result<Option<ReportUpload>> {
        let Some(script) = report.script else {
            return Ok(None);
        };
        let run = &report.run;
        let duration_ms = run.ended_at_ms.map_or(0, |ended_at_ms| {
            ended_at_ms.saturating_sub(run.started_at_ms)
        });
        Ok(Some(ReportUpload {
            report: report_json(report, MAX_REPORT_BYTES)
                .with_context(|| format!("failed to encode scraper report {}", report.id))?,
            device_type: self.device_type,
            client_id: self.client_id.clone(),
            script_id: script.id,
            script_version: script.version,
            run_id: run_id(report.id),
            duration_ms: u64::try_from(duration_ms).unwrap_or(0).min(MAX_DURATION_MS),
        }))
    }
}

/// Runs `uploader`'s passes forever: one now, then one each time `wake` is
/// notified (a run's report was completed). After a failed pass, tries
/// again after [`UPLOAD_RETRY_AFTER`], or once woken.
pub(super) async fn upload_reports<S: UploadStorage, C: FydeClient>(
    uploader: ReportUploader<S, C>,
    wake: Arc<UploadWake>,
) {
    loop {
        match uploader.upload_pending().await {
            Ok(uploaded) => {
                if uploaded > 0 {
                    tracing::debug!("uploaded {uploaded} scraper reports");
                }
                wake.notified().await;
            }
            Err(err) => {
                tracing::warn!(
                    code = ?err.code(),
                    "failed to upload the scraper reports, trying again in {}s: {err}",
                    UPLOAD_RETRY_AFTER.as_secs()
                );
                tokio::select! {
                    () = wake.notified() => {}
                    () = tokio::time::sleep(UPLOAD_RETRY_AFTER) => {}
                }
            }
        }
    }
}

/// `report` as the JSON object uploaded, of at most `max_bytes`: how the
/// run ended and how far it got, then its log, each entry timed from the
/// run's start. Neither the name it ran under (an installation's id,
/// replaced by `[run]` wherever the log mentions it) nor when it ran is
/// sent. A log too long to fit loses entries from its
/// middle, keeping its start and its end (where a failure is), and says
/// how many it lost as `omitted_entries`.
pub(super) fn report_json(report: &Report, max_bytes: usize) -> Result<String> {
    let run = &report.run;
    let (outcome, error_code) = outcome_columns(run.outcome);
    let mut body = json!({
        "outcome": outcome,
        "error_code": error_code,
        "step": run.step.map(|step| step.as_str()),
        "update": run.update.map(|update| json!({
            "current": update.current,
            "total": update.total,
        })),
        "found": run.found,
        "saved": run.saved,
        "omitted_entries": 0,
        "entries": [],
    });
    let entries = report
        .entries
        .iter()
        .map(|entry| {
            let mut value = entry.value.clone();
            scrub(&mut value, &run.name);
            json!({
                "at_ms": entry.ts_ms.saturating_sub(run.started_at_ms),
                "event": entry.event,
                "value": value,
            })
        })
        .collect::<Vec<_>>();
    let sizes = entries
        .iter()
        .map(|entry| serde_json::to_string(entry).map(|text| text.len()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to encode a scraper report's log")?;

    // Room left for the entries once the rest is written, keeping enough
    // for `omitted_entries` to grow to any count.
    let rest = serde_json::to_string(&body)
        .context("failed to encode a scraper report")?
        .len();
    let mut budget = max_bytes.saturating_sub(rest + u64::MAX.to_string().len());
    // Taken in turn from the start and from the end, each side stopping
    // at its first entry that doesn't fit. Each entry but the first costs
    // a comma too.
    let (mut front, mut back) = (0, entries.len());
    // Whether the start (0) and the end (1) still take entries.
    let mut open = [true, true];
    while front < back && open.contains(&true) {
        for side in [0, 1] {
            if front == back || !open[side] {
                continue;
            }
            let index = if side == 0 { front } else { back - 1 };
            let kept = front + (entries.len() - back);
            let cost = sizes[index] + usize::from(kept > 0);
            if cost > budget {
                open[side] = false;
                continue;
            }
            budget -= cost;
            if side == 0 {
                front += 1;
            } else {
                back -= 1;
            }
        }
    }

    let omitted = back - front;
    let mut kept = entries;
    kept.drain(front..back);
    body["omitted_entries"] = json!(omitted);
    body["entries"] = serde_json::Value::Array(kept);
    serde_json::to_string(&body).context("failed to encode a scraper report")
}

/// Replaces `name` by `[run]` in every string within `value`.
fn scrub(value: &mut serde_json::Value, name: &str) {
    use serde_json::Value;

    if name.is_empty() {
        return;
    }
    match value {
        Value::String(text) if text.contains(name) => *text = text.replace(name, "[run]"),
        Value::Array(items) => items.iter_mut().for_each(|item| scrub(item, name)),
        Value::Object(fields) => fields.values_mut().for_each(|field| scrub(field, name)),
        _ => {}
    }
}

/// Namespace of [`run_id`]'s name-based UUIDs.
const RUN_ID_NAMESPACE: Uuid = Uuid::from_u128(0x5f0b_2d4e_8c1a_4b7e_9d3f_6a2c_1e8b_7d40);

/// The id a report with `report_id` is uploaded under, for the server to
/// store each run's report once: derived from the run's own id, so the same
/// on every try, but a hash of it, so it doesn't tell when the run started
/// (`report_id` is a UUIDv7, which does).
fn run_id(report_id: Uuid) -> Uuid {
    Uuid::new_v5(&RUN_ID_NAMESPACE, report_id.as_bytes())
}

/// The kind of device the SDK was built for.
fn this_device() -> DeviceType {
    if cfg!(target_os = "android") {
        DeviceType::Android
    } else if cfg!(target_os = "ios") {
        DeviceType::Ios
    } else {
        DeviceType::Desktop
    }
}

/// `client_name` (1 to 64 ASCII letters, digits, `-`, `_` or `.`; see
/// [`crate::ClientConfig::client_name`]) as the client id the server
/// accepts: lowercase, `_` and `.` turned into `-`.
fn client_id(client_name: &str) -> String {
    client_name
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::grpc_client::MockFydeClient;
    use super::super::models::{FakeReport, ListedReport, ReportCursor, ReportEntry};
    use super::super::storage::{MemoryStorage, MockStorage, MockUploadStorage};
    use super::*;
    use crate::{Error, ErrorKind};

    fn client() -> ReportsClient<MemoryStorage> {
        ReportsClient::new(MemoryStorage::default())
    }

    #[tokio::test]
    async fn start_opens_a_running_report_and_records_the_start() {
        let client = client();

        let tracker = client.start("alice", None).await.unwrap();

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
    async fn the_report_keeps_the_script_that_ran_from_start_to_finish() {
        let client = client();
        let script = ReportedScript {
            id: Uuid::new_v4(),
            version: 3,
        };

        let tracker = client.start("alice", Some(script)).await.unwrap();
        assert_eq!(client.storage.reports()[0].script, Some(script));
        client.finish(tracker, &Ok(())).await.unwrap();

        assert_eq!(client.storage.reports()[0].script, Some(script));
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
            let tracker = client.start("alice", None).await.unwrap();
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
        let tracker = client.start("alice", None).await.unwrap();
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

        drop(client.start("alice", None).await.unwrap());

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
        let first = client.start("alice", None).await.unwrap();
        client.finish(first, &Ok(())).await.unwrap();

        let second = client.start("alice", None).await.unwrap();

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

        let err = client.start("alice", None).await.err().unwrap();

        assert!(err.has_context());
        assert!(client.going_on.lock().unwrap().is_empty());
    }

    fn uploader(
        storage: MockUploadStorage,
        client: MockFydeClient,
    ) -> ReportUploader<MockUploadStorage, MockFydeClient> {
        ReportUploader::new(storage, client, "Desktop_app.2")
    }

    fn parsed(upload: &ReportUpload) -> serde_json::Value {
        serde_json::from_str(&upload.report).unwrap()
    }

    #[tokio::test]
    async fn finish_wakes_the_uploads() {
        let wake = Arc::new(UploadWake::new());
        let client = ReportsClient::new(MemoryStorage::default()).waking(wake.clone());
        let tracker = client.start("alice", None).await.unwrap();

        client.finish(tracker, &Ok(())).await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), wake.notified())
            .await
            .expect("finishing a run should wake the uploads");
    }

    #[tokio::test]
    async fn upload_pending_uploads_each_report_once_and_notes_it_uploaded() {
        let script = ReportedScript {
            id: Uuid::new_v4(),
            version: 4,
        };
        let report = FakeReport::new()
            .with_started_at_ms(10_000)
            .with_script(Some(script))
            .build();
        let id = report.id;
        let mut storage = MockUploadStorage::new();
        storage
            .expect_list_to_upload()
            .withf(|after, limit| after.is_none() && *limit == UPLOAD_BATCH)
            .times(1)
            .return_once(move |_, _| Ok(vec![report.into()]));
        storage
            .expect_mark_uploaded()
            .withf(move |marked| *marked == id)
            .times(1)
            .returning(|_| Ok(()));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut client = MockFydeClient::new();
        let capture = sent.clone();
        client
            .expect_upload_report()
            .times(1)
            .returning(move |upload| {
                capture.lock().unwrap().push(upload);
                Ok(())
            });

        let uploaded = uploader(storage, client).upload_pending().await.unwrap();

        assert_eq!(uploaded, 1);
        let sent = sent.lock().unwrap();
        let upload = &sent[0];
        assert_eq!(upload.script_id, script.id);
        assert_eq!(upload.script_version, 4);
        assert_eq!(upload.client_id, "desktop-app-2");
        assert_eq!(upload.device_type, DeviceType::Desktop);
        assert_eq!(upload.duration_ms, 60_000);
        assert_eq!(upload.run_id, run_id(id));
        assert_eq!(parsed(upload)["outcome"], "succeeded");
    }

    #[tokio::test]
    async fn upload_pending_pages_through_every_report_to_upload() {
        let first: Vec<Report> = (0..UPLOAD_BATCH)
            .map(|_| FakeReport::new().build())
            .collect();
        let cursor = ReportCursor::from(first.last().unwrap());
        let last = FakeReport::new().build();
        let mut storage = MockUploadStorage::new();
        let mut sequence = mockall::Sequence::new();
        storage
            .expect_list_to_upload()
            .withf(|after, _| after.is_none())
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_, _| Ok(first.into_iter().map(Into::into).collect()));
        storage
            .expect_list_to_upload()
            .withf(move |after, _| after.as_ref() == Some(&cursor))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_, _| Ok(vec![last.into()]));
        storage
            .expect_mark_uploaded()
            .times(UPLOAD_BATCH as usize + 1)
            .returning(|_| Ok(()));
        let mut client = MockFydeClient::new();
        client
            .expect_upload_report()
            .times(UPLOAD_BATCH as usize + 1)
            .returning(|_| Ok(()));

        let uploaded = uploader(storage, client).upload_pending().await.unwrap();

        assert_eq!(uploaded, UPLOAD_BATCH as usize + 1);
    }

    #[tokio::test]
    async fn a_report_the_server_refuses_is_kept_and_the_others_still_go() {
        let refused = FakeReport::new().build();
        let accepted = FakeReport::new().build();
        let (refused_script, accepted_id) = (refused.script.unwrap().id, accepted.id);
        let mut storage = MockUploadStorage::new();
        storage
            .expect_list_to_upload()
            .times(1)
            .return_once(move |_, _| Ok(vec![refused.into(), accepted.into()]));
        storage
            .expect_mark_uploaded()
            .withf(move |marked| *marked == accepted_id)
            .times(1)
            .returning(|_| Ok(()));
        let mut client = MockFydeClient::new();
        client
            .expect_upload_report()
            .times(2)
            .returning(move |upload| {
                if upload.script_id == refused_script {
                    Err(tonic::Status::invalid_argument("report: too large").into())
                } else {
                    Ok(())
                }
            });

        let uploaded = uploader(storage, client).upload_pending().await.unwrap();

        assert_eq!(uploaded, 1);
    }

    #[tokio::test]
    async fn a_report_that_cannot_be_read_is_kept_and_the_others_still_go() {
        let unreadable = ListedReport {
            cursor: ReportCursor {
                started_at_ms: 1_000,
                id: "not-a-uuid".to_string(),
            },
            report: Err(std::io::Error::other("corrupted log").into()),
        };
        let readable = FakeReport::new().with_started_at_ms(2_000).build();
        let readable_id = readable.id;
        let mut storage = MockUploadStorage::new();
        storage
            .expect_list_to_upload()
            .times(1)
            .return_once(move |_, _| Ok(vec![unreadable, readable.into()]));
        storage
            .expect_mark_uploaded()
            .withf(move |marked| *marked == readable_id)
            .times(1)
            .returning(|_| Ok(()));
        let mut client = MockFydeClient::new();
        client.expect_upload_report().times(1).returning(|_| Ok(()));

        let uploaded = uploader(storage, client).upload_pending().await.unwrap();

        assert_eq!(uploaded, 1);
    }

    #[tokio::test]
    async fn a_batch_ending_in_an_unreadable_report_pages_on_past_it() {
        let mut first: Vec<ListedReport> = (1..UPLOAD_BATCH)
            .map(|_| FakeReport::new().build().into())
            .collect();
        let unreadable_cursor = ReportCursor {
            started_at_ms: i64::MAX,
            id: "not-a-uuid".to_string(),
        };
        first.push(ListedReport {
            cursor: unreadable_cursor.clone(),
            report: Err(std::io::Error::other("corrupted log").into()),
        });
        let mut storage = MockUploadStorage::new();
        let mut sequence = mockall::Sequence::new();
        storage
            .expect_list_to_upload()
            .withf(|after, _| after.is_none())
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_, _| Ok(first));
        storage
            .expect_list_to_upload()
            .withf(move |after, _| after.as_ref() == Some(&unreadable_cursor))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(|_, _| Ok(Vec::new()));
        storage
            .expect_mark_uploaded()
            .times(UPLOAD_BATCH as usize - 1)
            .returning(|_| Ok(()));
        let mut client = MockFydeClient::new();
        client
            .expect_upload_report()
            .times(UPLOAD_BATCH as usize - 1)
            .returning(|_| Ok(()));

        let uploaded = uploader(storage, client).upload_pending().await.unwrap();

        assert_eq!(uploaded, UPLOAD_BATCH as usize - 1);
    }

    #[tokio::test]
    async fn any_other_upload_failure_ends_the_pass_with_context() {
        let mut storage = MockUploadStorage::new();
        storage
            .expect_list_to_upload()
            .times(1)
            .return_once(|_, _| {
                Ok(vec![
                    FakeReport::new().build().into(),
                    FakeReport::new().build().into(),
                ])
            });
        storage.expect_mark_uploaded().never();
        let mut client = MockFydeClient::new();
        client
            .expect_upload_report()
            .times(1)
            .returning(|_| Err(tonic::Status::unavailable("down").into()));

        let err = uploader(storage, client)
            .upload_pending()
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::ServerUnavailable);
        assert!(err.has_context());
    }

    #[tokio::test]
    async fn a_run_longer_than_the_server_accepts_is_sent_as_its_longest() {
        let mut report = FakeReport::new().with_started_at_ms(0).build();
        report.run.ended_at_ms = Some(i64::try_from(MAX_DURATION_MS).unwrap() * 2);
        let mut storage = MockUploadStorage::new();
        storage
            .expect_list_to_upload()
            .return_once(move |_, _| Ok(vec![report.into()]));
        storage.expect_mark_uploaded().returning(|_| Ok(()));
        let mut client = MockFydeClient::new();
        client
            .expect_upload_report()
            .withf(|upload| upload.duration_ms == MAX_DURATION_MS)
            .times(1)
            .returning(|_| Ok(()));

        uploader(storage, client).upload_pending().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_upload_job_tries_again_a_while_after_a_failed_pass() {
        let mut storage = MockUploadStorage::new();
        let mut sequence = mockall::Sequence::new();
        storage
            .expect_list_to_upload()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Err(std::io::Error::other("disk gone").into()));
        let (passed, mut second_pass) = tokio::sync::mpsc::unbounded_channel();
        storage
            .expect_list_to_upload()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_, _| {
                passed.send(()).unwrap();
                Ok(Vec::new())
            });
        let job = tokio::spawn(upload_reports(
            uploader(storage, MockFydeClient::new()),
            Arc::new(UploadWake::new()),
        ));

        tokio::time::sleep(UPLOAD_RETRY_AFTER - Duration::from_secs(1)).await;
        assert!(second_pass.try_recv().is_err(), "tried again too soon");
        tokio::time::sleep(Duration::from_secs(2)).await;
        second_pass.recv().await.unwrap();
        job.abort();
    }

    #[tokio::test]
    async fn the_upload_job_looks_again_each_time_it_is_woken() {
        let (passed, mut passes) = tokio::sync::mpsc::unbounded_channel();
        let mut storage = MockUploadStorage::new();
        storage
            .expect_list_to_upload()
            .times(2)
            .returning(move |_, _| {
                passed.send(()).unwrap();
                Ok(Vec::new())
            });
        let wake = Arc::new(UploadWake::new());
        let job = tokio::spawn(upload_reports(
            uploader(storage, MockFydeClient::new()),
            wake.clone(),
        ));

        passes.recv().await.unwrap();
        wake.notify_one();
        passes.recv().await.unwrap();
        job.abort();
    }

    #[test]
    fn report_json_says_how_the_run_went_without_its_name_or_when_it_ran() {
        let report = FakeReport::new()
            .with_scraper_name("installation-id")
            .with_started_at_ms(1_000_000)
            .with_outcome(ScraperRunOutcome::Failed {
                code: ErrorCode::ScraperCredentialsRejected,
            })
            .with_progress(
                ProgressStep::LoggingIn,
                Some(ScraperRunUpdate {
                    current: 1,
                    total: 2,
                }),
                None,
                0,
            )
            .with_entries(vec![ReportEntry {
                ts_ms: 1_000_250,
                event: "log".to_string(),
                value: json!({ "message": "hi" }),
            }])
            .build();

        let text = report_json(&report, MAX_REPORT_BYTES).unwrap();

        assert!(!text.contains("installation-id"));
        assert!(!text.contains("1000000"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            json!({
                "outcome": "failed",
                "error_code": ErrorCode::ScraperCredentialsRejected.as_u32(),
                "step": "logging_in",
                "update": { "current": 1, "total": 2 },
                "found": null,
                "saved": 0,
                "omitted_entries": 0,
                "entries": [{ "at_ms": 250, "event": "log", "value": { "message": "hi" } }],
            })
        );
    }

    #[test]
    fn report_json_scrubs_the_runs_name_from_its_log() {
        let name = Uuid::new_v4().to_string();
        let report = FakeReport::new()
            .with_scraper_name(name.clone())
            .with_entries(vec![
                ReportEntry {
                    ts_ms: 0,
                    event: "run_started".to_string(),
                    value: json!({ "script": name }),
                },
                ReportEntry {
                    ts_ms: 1,
                    event: "error".to_string(),
                    value: json!({ "messages": [format!("failed to run scraper {name:?}")] }),
                },
            ])
            .build();

        let text = report_json(&report, MAX_REPORT_BYTES).unwrap();

        assert!(!text.contains(&name));
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["entries"][0]["value"], json!({ "script": "[run]" }));
        assert_eq!(
            body["entries"][1]["value"],
            json!({ "messages": ["failed to run scraper \"[run]\""] })
        );
    }

    #[test]
    fn report_json_drops_the_middle_of_a_log_too_long_to_fit() {
        let entries: Vec<ReportEntry> = (0..100)
            .map(|i| ReportEntry {
                ts_ms: i,
                event: "log".to_string(),
                value: json!({ "message": format!("{i:03}{}", "x".repeat(100)) }),
            })
            .collect();
        let report = FakeReport::new()
            .with_started_at_ms(0)
            .with_entries(entries)
            .build();
        let max_bytes = 3_000;

        let text = report_json(&report, max_bytes).unwrap();

        assert!(text.len() <= max_bytes);
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        let kept: Vec<i64> = body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["at_ms"].as_i64().unwrap())
            .collect();
        assert!(kept.len() > 2);
        assert_eq!(body["omitted_entries"], 100 - kept.len());
        assert_eq!(kept[0], 0, "the log's start is kept");
        assert_eq!(*kept.last().unwrap(), 99, "the log's end is kept");
        assert!(kept.windows(2).all(|pair| pair[0] < pair[1]), "in order");
    }

    #[test]
    fn report_json_leaves_out_an_entry_bigger_than_the_whole_report_may_be() {
        let report = FakeReport::new()
            .with_entries(vec![ReportEntry {
                ts_ms: 0,
                event: "http_response".to_string(),
                value: json!({ "body": "x".repeat(10_000) }),
            }])
            .build();

        let text = report_json(&report, 1_000).unwrap();

        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["entries"], json!([]));
        assert_eq!(body["omitted_entries"], 1);
    }

    #[test]
    fn a_runs_upload_id_is_its_own_and_tells_nothing_of_when_it_ran() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();

        assert_eq!(run_id(first), run_id(first), "the same on every try");
        assert_ne!(run_id(first), run_id(second));
        assert_eq!(run_id(first).get_version(), Some(uuid::Version::Sha1));
        assert_eq!(run_id(first).get_timestamp(), None);
    }

    #[test]
    fn client_id_is_the_client_name_as_the_server_accepts_it() {
        assert_eq!(client_id("android"), "android");
        assert_eq!(client_id("My_Client.1-b"), "my-client-1-b");
    }
}
