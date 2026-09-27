mod service;
mod vm;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;
use crate::domains::documents::Document;

/// Runs Lua scripts against a document inside a fully sandboxed VM (see
/// `service.rs`): no access to the filesystem, OS environment, subprocesses,
/// or network from Lua code. Trait methods take `&self` (not `&mut self`)
/// so implementations can be shared behind `Arc<dyn Service>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Starts a sandboxed Lua VM scoped to `document` and runs it. Doesn't
    /// do anything with the VM yet beyond constructing it.
    async fn run_for_document(&self, document: &Document) -> Result<()>;
}

/// Initializes the scripts service.
pub(crate) fn init() -> Arc<dyn Service> {
    Arc::new(service::ScriptsClient::new())
}
