mod cookies;
mod host;
mod models;
mod reports;
mod service;
mod session;

pub use models::ProgressEvent;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;

use crate::Result;
use crate::domains::documents::Service as DocumentsService;

/// Runs Lua scraper scripts: a sandboxed Lua 5.4 VM with host capabilities
/// exposed under a single `fyde` global table (`fyde.http`, `fyde.html`,
/// `fyde.json`, `fyde.log`, `fyde.progress`, `fyde.input`,
/// `fyde.save_document`, `fyde.session`), ported from `demo-rust-fyde`.
/// Scripts loaded here must follow the same contract as
/// `demo-rust-fyde`'s: return a table with a single `run(parameters)`
/// function, calling only into `fyde`. Trait methods take `&self` (not
/// `&mut self`) so implementations can be shared behind `Arc<dyn Service>`.
#[async_trait]
pub trait Service: Send + Sync {
    /// Runs `script` (Lua source) as the scraper named `name`. `name` scopes
    /// this scraper's persisted session data and cookie jar (see the
    /// private `session`/`cookies` sub-domains) — two different scripts
    /// sharing a `name` share session/cookie state, so callers should use a
    /// stable, unique name per script (e.g. its file stem, as
    /// `demo-rust-fyde` does). `parameters` is handed to the script's
    /// `run(parameters)` entrypoint as-is (as a Lua table) — this service
    /// never looks inside it, so a script is free to read whatever fields it
    /// needs (conventionally `username`/`password`) back out of it.
    ///
    /// Loads this scraper's previously saved session data and cookies before
    /// running, and persists their final state back — including
    /// `fyde.session`'s contents and every cookie `fyde.http` picked up for
    /// a host it visited — once the script's `run` function returns, whether
    /// it succeeded or failed, so state from a partial run isn't lost.
    async fn run(&self, name: &str, script: &str, parameters: Value) -> Result<()>;
}

/// Selects which storage backend [`init`] builds the `session`/`cookies`/
/// `reports` sub-domains on top of — propagated down to each sub-domain's
/// own `StorageConfig` (`session::StorageConfig`, `cookies::StorageConfig`,
/// `reports::StorageConfig`).
pub(crate) enum StorageConfig {
    /// Persists session data, cookies and reports as JSON files under the
    /// given directory (one file per scraper — or, for `reports`, one file
    /// per run — under a `session`/`cookies`/`reports` subdirectory per
    /// sub-domain) — used by [`crate::init_dev_scrapers`], for a standalone
    /// CLI runner with no local SQLite database of its own.
    File(PathBuf),
    /// Persists session data, cookies and reports in the given SQLite
    /// pool's `scraper_sessions`/`scraper_cookies`/`scraper_reports` tables
    /// — used by a full [`crate::Client`].
    Sqlite(SqlitePool),
}

/// Initializes the scrapers service: uses `storage` to persist each
/// scraper's session data, cookie jar and per-run debug report (see the
/// private `session`/`cookies`/`reports` sub-domains), `documents` so
/// `fyde.save_document` can upload a downloaded document through the SDK's
/// own encrypted upload path, and `on_progress`/`on_question` as the
/// callbacks `fyde.progress`/`fyde.input` invoke — see
/// [`crate::ClientConfig::on_scraper_progress`]/
/// [`crate::ClientConfig::on_scraper_question`].
pub(crate) fn init(
    storage: StorageConfig,
    documents: Arc<dyn DocumentsService>,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
) -> Arc<dyn Service> {
    let (cookies_storage, session_storage, reports_storage) = match storage {
        // Separate subdirectories so a cookie file, a session file and a
        // report file for the same scraper name never collide, even though
        // their own sub-domains already give them distinct naming
        // (`<name>.cookies.json`, `<name>.json`,
        // `<name>_<started_at_ms>.json`) — this keeps the three
        // sub-domains' files visibly separated on disk too.
        StorageConfig::File(dir) => (
            cookies::StorageConfig::File(dir.join("cookies")),
            session::StorageConfig::File(dir.join("session")),
            reports::StorageConfig::File(dir.join("reports")),
        ),
        StorageConfig::Sqlite(pool) => (
            cookies::StorageConfig::Sqlite(pool.clone()),
            session::StorageConfig::Sqlite(pool.clone()),
            reports::StorageConfig::Sqlite(pool),
        ),
    };

    let cookies = cookies::init(cookies_storage);
    let session = session::init(session_storage);
    let reports = reports::init(reports_storage);

    Arc::new(service::ScrapersClient::new(
        cookies,
        session,
        reports,
        documents,
        on_progress,
        on_question,
    ))
}
