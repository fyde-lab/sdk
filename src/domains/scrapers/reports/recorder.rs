use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::models::{Report, ReportEntry};

/// Accumulates one scraper run's debug report in memory as it happens —
/// ported from `demo-rust-fyde`'s `Report` (`report.rs`): every `fyde.log`,
/// `fyde.http`, `fyde.progress` and `fyde.input` call records a
/// `(event_type, value)` entry via [`record`]. Unlike `demo-rust-fyde`,
/// which rewrote a file to disk on every entry so a crash mid-run never
/// lost debug output, this just keeps entries in memory — [`finish`] hands
/// the finished [`Report`] to `super::Service::save` once the run is over,
/// whether it succeeded or failed. `host::install` builds one per run and
/// threads it (behind an `Arc`) into `host::log`/`http`/`progress`/`input`.
pub(crate) struct Recorder {
    scraper_name: String,
    started_at_ms: i64,
    entries: Mutex<Vec<ReportEntry>>,
}

impl Recorder {
    pub(crate) fn new(scraper_name: &str) -> Self {
        Self {
            scraper_name: scraper_name.to_string(),
            started_at_ms: now_ms(),
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Adds one `(event_type, value)` entry to the report.
    pub(crate) fn record(&self, event: &str, value: Value) {
        let entry = ReportEntry {
            ts_ms: now_ms(),
            event: event.to_string(),
            value,
        };

        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries.push(entry);
    }

    /// Snapshots every entry recorded so far into a finished [`Report`],
    /// ready to persist via `super::Service::save`.
    pub(crate) fn finish(&self) -> Report {
        let entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };

        Report {
            scraper_name: self.scraper_name.clone(),
            started_at_ms: self.started_at_ms,
            entries: entries.clone(),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_returns_every_recorded_entry_in_order() {
        let recorder = Recorder::new("didaxis");

        recorder.record(
            "log",
            serde_json::json!({ "level": "info", "message": "a" }),
        );
        recorder.record(
            "progress",
            serde_json::json!({ "kind": "step", "name": "b" }),
        );

        let report = recorder.finish();

        assert_eq!(report.scraper_name, "didaxis");
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].event, "log");
        assert_eq!(report.entries[1].event, "progress");
    }

    #[test]
    fn finish_can_be_called_without_any_entries_recorded() {
        let recorder = Recorder::new("didaxis");

        let report = recorder.finish();

        assert_eq!(report.entries, Vec::new());
    }
}
