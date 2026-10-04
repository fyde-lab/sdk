mod cookies;
mod host;
mod models;
mod service;
mod session;

pub use models::ProgressEvent;

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

/// Initializes the scrapers service: uses `pool` to persist each scraper's
/// session data and cookie jar (see the private `session`/`cookies`
/// sub-domains, each backed by its own sqlite table), `documents` so
/// `fyde.save_document` can upload a downloaded document through the SDK's
/// own encrypted upload path, and `on_progress`/`on_question` as the
/// callbacks `fyde.progress`/`fyde.input` invoke — see
/// [`crate::ClientConfig::on_scraper_progress`]/
/// [`crate::ClientConfig::on_scraper_question`].
pub(crate) fn init(
    pool: SqlitePool,
    documents: Arc<dyn DocumentsService>,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
) -> Arc<dyn Service> {
    let cookies = cookies::init(pool.clone());
    let session = session::init(pool);

    Arc::new(service::ScrapersClient::new(
        cookies,
        session,
        documents,
        on_progress,
        on_question,
    ))
}
