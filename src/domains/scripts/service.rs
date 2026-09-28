use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::documents::Document;
use crate::domains::documents::Service as DocumentsService;
use crate::domains::sessions::SessionsClient;
use crate::{ErrorContext as _, Result};

use super::Script;
use super::Service;
use super::grpc_client::{FydeClient, GrpcClient};
use super::vm;

/// The default [`Service`] implementation. Delegates VM construction to
/// `vm::sandboxed`, which is the only place that decides what a Lua script
/// is and isn't allowed to touch, composes the injected `grpc` client for
/// talking to the server's scripts service, and publishes renames scripts
/// make through the injected `documents` service.
pub(super) struct ScriptsClient {
    grpc: Box<dyn FydeClient>,
    documents: Arc<dyn DocumentsService>,
}

impl ScriptsClient {
    pub(super) fn new(
        channel: Channel,
        sessions: Arc<SessionsClient>,
        documents: Arc<dyn DocumentsService>,
    ) -> Self {
        Self {
            grpc: Box::new(GrpcClient::new(channel, sessions)),
            documents,
        }
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of
    /// a live server.
    #[cfg(test)]
    fn with_grpc(grpc: impl FydeClient + 'static, documents: Arc<dyn DocumentsService>) -> Self {
        Self {
            grpc: Box::new(grpc),
            documents,
        }
    }
}

#[async_trait]
impl Service for ScriptsClient {
    async fn run_for_document(&self, document: &Document) -> Result<()> {
        let scripts = self.grpc.list_user_scripts().await?;

        for script in &scripts {
            let updated_metadata = {
                let lua = vm::sandboxed()?;
                vm::expose_document(&lua, document)?;
                lua.load(script.script())
                    .exec()
                    .context("failed to run script")?;

                vm::read_metadata(&lua, document.metadata())?
            };

            if updated_metadata != *document.metadata() {
                self.documents
                    .update_metadata(updated_metadata)
                    .await
                    .context("failed to publish the metadata changes a script made")?;
            }
        }

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

    async fn update_script(
        &self,
        id: Uuid,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
    ) -> Result<Script> {
        self.grpc
            .update_script(id, name, is_public, icon, script)
            .await
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
    use std::sync::Mutex;

    use super::*;
    use crate::domains::documents::FakeDocument;
    use crate::domains::documents::Metadata;
    use crate::domains::scripts::FakeScript;
    use crate::domains::scripts::grpc_client::MockFydeClient;

    /// A [`DocumentsService`] fake recording every call to
    /// [`DocumentsService::update_metadata`], for tests that don't need a
    /// live changelog. Every other method is unused by [`ScriptsClient`], so
    /// left unimplemented.
    #[derive(Default)]
    struct RecordingDocuments {
        update_metadata_calls: Mutex<Vec<Metadata>>,
    }

    #[async_trait]
    impl DocumentsService for RecordingDocuments {
        async fn upload(&self, _path: &std::path::Path) -> Result<Uuid> {
            unimplemented!()
        }

        async fn get(&self, _id: Uuid) -> Result<Option<Document>> {
            unimplemented!()
        }

        async fn list(&self, _offset: i64, _limit: i64) -> Result<Vec<Document>> {
            unimplemented!()
        }

        async fn update_metadata(&self, metadata: Metadata) -> Result<()> {
            self.update_metadata_calls.lock().unwrap().push(metadata);
            Ok(())
        }

        async fn start_sync(&self) -> Result<()> {
            unimplemented!()
        }

        fn stop_sync(&self) {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn run_for_document_runs_every_enabled_script() {
        let scripts = vec![
            FakeScript::new().with_script("return 1 + 1").build(),
            FakeScript::new().with_script("return 2 + 2").build(),
        ];
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_user_scripts()
            .times(1)
            .returning(move || Ok(scripts.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));
        let document = FakeDocument::new().build();

        client.run_for_document(&document).await.unwrap();
    }

    #[tokio::test]
    async fn run_for_document_fails_if_a_script_errors() {
        let scripts = vec![
            FakeScript::new()
                .with_script("this is not valid lua")
                .build(),
        ];
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_user_scripts()
            .times(1)
            .returning(move || Ok(scripts.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));
        let document = FakeDocument::new().build();

        client.run_for_document(&document).await.unwrap_err();
    }

    #[tokio::test]
    async fn run_for_document_updates_the_name_when_a_script_renames_the_document() {
        let scripts = vec![
            FakeScript::new()
                .with_script(r#"document.metadata.name = "renamed.pdf""#)
                .build(),
        ];
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_user_scripts()
            .times(1)
            .returning(move || Ok(scripts.clone()));

        let documents = Arc::new(RecordingDocuments::default());
        let client = ScriptsClient::with_grpc(mock_grpc, documents.clone());
        let document = FakeDocument::new().build();

        client.run_for_document(&document).await.unwrap();

        let calls = documents.update_metadata_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let metadata = &calls[0];
        assert_eq!(
            *metadata,
            Metadata {
                name: "renamed.pdf".to_string(),
                ..document.metadata().clone()
            }
        );
    }

    #[tokio::test]
    async fn run_for_document_does_not_update_the_name_when_no_script_renames_the_document() {
        let scripts = vec![FakeScript::new().with_script("return 1 + 1").build()];
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_user_scripts()
            .times(1)
            .returning(move || Ok(scripts.clone()));

        let documents = Arc::new(RecordingDocuments::default());
        let client = ScriptsClient::with_grpc(mock_grpc, documents.clone());
        let document = FakeDocument::new().build();

        client.run_for_document(&document).await.unwrap();

        assert!(documents.update_metadata_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_for_document_does_not_update_the_name_when_a_script_sets_the_same_name() {
        let document = FakeDocument::new().build();
        let same_name = document.metadata().name().to_string();
        let scripts = vec![
            FakeScript::new()
                .with_script(&format!("document.metadata.name = {same_name:?}"))
                .build(),
        ];
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_user_scripts()
            .times(1)
            .returning(move || Ok(scripts.clone()));

        let documents = Arc::new(RecordingDocuments::default());
        let client = ScriptsClient::with_grpc(mock_grpc, documents.clone());

        client.run_for_document(&document).await.unwrap();

        assert!(documents.update_metadata_calls.lock().unwrap().is_empty());
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

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));

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

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));

        assert_eq!(client.fetch_script(id).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn update_script_delegates_to_grpc() {
        let expected = FakeScript::new().build();
        let id = expected.id();
        let returned = expected.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_update_script()
            .withf(move |requested_id, name, is_public, icon, script| {
                *requested_id == id
                    && name == "my-script"
                    && *is_public
                    && icon == b"icon-bytes"
                    && script == "return 1"
            })
            .times(1)
            .returning(move |_, _, _, _, _| Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));

        let script = client
            .update_script(id, "my-script", true, b"icon-bytes".to_vec(), "return 1")
            .await
            .unwrap();

        assert_eq!(script, expected);
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

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));

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

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));

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

        let client = ScriptsClient::with_grpc(mock_grpc, Arc::new(RecordingDocuments::default()));

        assert_eq!(client.list_user_scripts().await.unwrap(), expected);
    }
}
