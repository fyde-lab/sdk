use async_trait::async_trait;

use crate::Result;
use crate::domains::documents::Document;

use super::Service;
use super::vm;

/// The default [`Service`] implementation. Delegates VM construction to
/// `vm::sandboxed`, which is the only place that decides what a Lua script
/// is and isn't allowed to touch.
pub(super) struct ScriptsClient;

impl ScriptsClient {
    pub(super) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Service for ScriptsClient {
    async fn run_for_document(&self, _document: &Document) -> Result<()> {
        let _lua = vm::sandboxed()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::FakeDocument;

    #[tokio::test]
    async fn run_for_document_succeeds() {
        let client = ScriptsClient::new();
        let document = FakeDocument::new().build();

        client.run_for_document(&document).await.unwrap();
    }
}
