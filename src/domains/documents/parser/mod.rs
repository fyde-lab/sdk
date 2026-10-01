mod service;
mod transcript;
mod vm;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;
use crate::domains::scripts::Service as ScriptsService;

use super::{Document, Metadata};

/// The only content type [`Service::parse_content`] currently accepts.
pub(super) const PDF_CONTENT_TYPE: &str = "application/pdf";

/// Derives a freshly uploaded document's [`Metadata`] and runs a user's
/// enabled scripts against a document to fill in/update its classification
/// fields. Unlike `changelog`, this submodule is public: it's also the
/// service external tooling (e.g. `fyde-scripts`, validating a script against
/// a placeholder [`Document`] before publishing it) uses to exercise a
/// script through the same sandboxed Lua VM the server-side pipeline runs,
/// instead of hand-rolling a syntax-only check.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Builds the [`Metadata`] for a freshly uploaded PDF `content`,
    /// originally named `original_name`: derives its checksum, size, and
    /// transcript, then runs every script currently enabled for the
    /// authenticated user against it (see [`Self::run_scripts`]) to fill in
    /// its classification fields (`type`, `source_category`,
    /// `source_sub_category`, `subject`, `purpose`) — left
    /// empty otherwise, since scripts never persist anything themselves.
    async fn parse_content(&self, content: &[u8], original_name: &str) -> Result<Metadata>;

    /// Fetches the scripts currently enabled for the authenticated user and
    /// runs each of them, in its own freshly-constructed sandboxed Lua VM,
    /// scoped to `document`, folding each script's changes into the next's
    /// starting point. Returns the resulting metadata without publishing or
    /// otherwise persisting it anywhere — a script only ever computes new
    /// metadata, it never updates the document itself; that's left to the
    /// caller to do (or not).
    async fn run_scripts(&self, document: &Document) -> Result<Metadata>;
}

/// Initializes the parser service, delegating script execution to `scripts`.
pub fn init(scripts: Arc<dyn ScriptsService>) -> Arc<dyn Service> {
    Arc::new(service::ParserClient::new(scripts))
}
