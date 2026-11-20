use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::models::ReportEntry;

/// Accumulates one scraper run's debug log in memory as it happens —
/// ported from `demo-rust-fyde`'s `Report` (`report.rs`): every `fyde.log`,
/// `fyde.http`, `fyde.progress`, `fyde.input` and `fyde.browser` call
/// records a `(event_type, value)` entry via [`Recorder::record`]. Unlike
/// `demo-rust-fyde`, which rewrote a file to disk on every entry so a crash
/// mid-run never lost debug output, this just keeps entries in memory —
/// `super::Service::finish` saves them in the run's report once it's over,
/// whether it succeeded or failed. `super::Service::start` builds one per
/// run and `host::install` threads it (behind an `Arc`) into
/// `host::log`/`http`/`progress`/`input`/`browser`.
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

    pub(crate) fn scraper_name(&self) -> &str {
        &self.scraper_name
    }

    /// When this run started, in milliseconds since the Unix epoch.
    pub(crate) fn started_at_ms(&self) -> i64 {
        self.started_at_ms
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

    /// Snapshots every entry recorded so far, in order.
    pub(crate) fn entries(&self) -> Vec<ReportEntry> {
        match self.entries.lock() {
            Ok(entries) => entries.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_returns_every_recorded_entry_in_order() {
        let recorder = Recorder::new("didaxis");

        recorder.record(
            "log",
            serde_json::json!({ "level": "info", "message": "a" }),
        );
        recorder.record(
            "progress",
            serde_json::json!({ "kind": "step", "step": "done" }),
        );

        let entries = recorder.entries();

        assert_eq!(recorder.scraper_name(), "didaxis");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event, "log");
        assert_eq!(entries[1].event, "progress");
        assert!(entries[0].ts_ms >= recorder.started_at_ms());
    }

    #[test]
    fn entries_can_be_read_without_any_recorded() {
        let recorder = Recorder::new("didaxis");

        assert_eq!(recorder.entries(), Vec::new());
    }
}
