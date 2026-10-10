use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One `(event_type, value)` entry recorded during a scraper run — e.g. a
/// log call, an HTTP request, a browser step, a progress update, an input
/// prompt or an error.
/// Ported from `demo-rust-fyde`'s `report.rs`, whose `Report::record` built
/// exactly this shape before writing it straight to disk; here it's just
/// data, collected by `super::Recorder` and handed to [`Report`] once a run
/// finishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReportEntry {
    pub(crate) ts_ms: i64,
    pub(crate) event: String,
    pub(crate) value: Value,
}

/// A completed scraper run's full debug report: every [`ReportEntry`]
/// recorded between the script starting and finishing, in order. Purely a
/// debugging aid — never read back by a scraper script itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Report {
    pub(crate) scraper_name: String,
    pub(crate) started_at_ms: i64,
    pub(crate) entries: Vec<ReportEntry>,
}

/// Builds a [`Report`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeReport {
    report: Report,
}

#[cfg(test)]
impl FakeReport {
    pub(crate) fn new() -> Self {
        Self {
            report: Report {
                scraper_name: crate::testing::random_word().to_string(),
                started_at_ms: crate::testing::random_past_timestamp() * 1000,
                entries: vec![ReportEntry {
                    ts_ms: crate::testing::random_past_timestamp() * 1000,
                    event: "log".to_string(),
                    value: serde_json::json!({ "level": "info", "message": crate::testing::random_word() }),
                }],
            },
        }
    }

    pub(crate) fn with_scraper_name(mut self, scraper_name: impl Into<String>) -> Self {
        self.report.scraper_name = scraper_name.into();
        self
    }

    pub(crate) fn with_entries(mut self, entries: Vec<ReportEntry>) -> Self {
        self.report.entries = entries;
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

        assert!(!report.scraper_name.is_empty());
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
            .build();

        assert_eq!(report.scraper_name, "didaxis");
        assert_eq!(report.entries, entries);
    }
}
