use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::Result;
use crate::domains::documents::Document;
use crate::domains::sessions::SessionsClient;

use super::Script;
use super::Service;
use super::grpc_client::{FydeClient, GrpcClient};
use super::vm;

/// The default [`Service`] implementation. Delegates VM construction to
/// `vm::sandboxed`, which is the only place that decides what a Lua script
/// is and isn't allowed to touch, and composes the injected `grpc` client
/// for talking to the server's scripts service.
pub(super) struct ScriptsClient {
    grpc: Box<dyn FydeClient>,
}

impl ScriptsClient {
    pub(super) fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self {
            grpc: Box::new(GrpcClient::new(channel, sessions)),
        }
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of
    /// a live server.
    #[cfg(test)]
    fn with_grpc(grpc: impl FydeClient + 'static) -> Self {
        Self {
            grpc: Box::new(grpc),
        }
    }
}

#[async_trait]
impl Service for ScriptsClient {
    async fn run_for_document(&self, _document: &Document) -> Result<()> {
        let _lua = vm::sandboxed()?;
        Ok(())
    }

    async fn create_script(
        &self,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
    ) -> Result<Script> {
        self.grpc.create_script(name, is_public, icon, script).await
    }

    async fn fetch_script(&self, id: Uuid) -> Result<Script> {
        self.grpc.fetch_script(id).await
    }

    async fn enable_script(&self, script_id: Uuid) -> Result<()> {
        self.grpc.enable_script(script_id).await
    }

    async fn disable_script(&self, script_id: Uuid) -> Result<()> {
        self.grpc.disable_script(script_id).await
    }

    async fn list_user_scripts(&self) -> Result<Vec<Script>> {
        self.grpc.list_user_scripts().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::FakeDocument;
    use crate::domains::scripts::FakeScript;
    use crate::domains::scripts::grpc_client::MockFydeClient;

    #[tokio::test]
    async fn run_for_document_succeeds() {
        let client = ScriptsClient::with_grpc(MockFydeClient::new());
        let document = FakeDocument::new().build();

        client.run_for_document(&document).await.unwrap();
    }

    #[tokio::test]
    async fn create_script_delegates_to_grpc() {
        let expected = FakeScript::new().build();
        let returned = expected.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_create_script()
            .withf(|name, is_public, icon, script| {
                name == "my-script" && *is_public && icon == b"icon-bytes" && script == "return 1"
            })
            .times(1)
            .returning(move |_, _, _, _| Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc);

        let script = client
            .create_script("my-script", true, b"icon-bytes".to_vec(), "return 1")
            .await
            .unwrap();

        assert_eq!(script, expected);
    }

    #[tokio::test]
    async fn fetch_script_delegates_to_grpc() {
        let expected = FakeScript::new().build();
        let id = expected.id();
        let returned = expected.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_fetch_script()
            .withf(move |requested_id| *requested_id == id)
            .times(1)
            .returning(move |_| Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc);

        assert_eq!(client.fetch_script(id).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn enable_script_delegates_to_grpc() {
        let id = Uuid::now_v7();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_enable_script()
            .withf(move |script_id| *script_id == id)
            .times(1)
            .returning(|_| Ok(()));

        let client = ScriptsClient::with_grpc(mock_grpc);

        client.enable_script(id).await.unwrap();
    }

    #[tokio::test]
    async fn disable_script_delegates_to_grpc() {
        let id = Uuid::now_v7();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_disable_script()
            .withf(move |script_id| *script_id == id)
            .times(1)
            .returning(|_| Ok(()));

        let client = ScriptsClient::with_grpc(mock_grpc);

        client.disable_script(id).await.unwrap();
    }

    #[tokio::test]
    async fn list_user_scripts_delegates_to_grpc() {
        let expected = vec![FakeScript::new().build(), FakeScript::new().build()];
        let returned = expected.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_user_scripts()
            .times(1)
            .returning(move || Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc);

        assert_eq!(client.list_user_scripts().await.unwrap(), expected);
    }
}
