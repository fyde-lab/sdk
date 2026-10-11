/// Progress callback payload — see
/// [`crate::ClientConfig::on_scraper_progress`] — for a running Lua scraper
/// script's `fyde.progress.step`/`fyde.progress.update` calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// `fyde.progress.step(name)` — announces a new high-level stage of the
    /// scrape (login, a given document section, ...).
    Step { name: String },
    /// `fyde.progress.update(current, total, message)` — fine-grained
    /// progress within the current step (e.g. "3/12 documents downloaded").
    Update {
        current: i64,
        total: i64,
        message: String,
    },
}

/// The per-run switches [`super::Service::run`] takes (see its doc comment
/// for what each one does), carried together from there down to the Lua
/// host tables and the browser driver instead of as five loose arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunOptions {
    pub(super) debug_http_dump: bool,
    pub(super) wreq_emulation: bool,
    pub(super) follow_redirects: bool,
    pub(super) allowed_domains: Vec<String>,
    pub(super) browser_visible: bool,
}
