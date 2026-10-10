use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::changelog::{EventType, Service as ChangelogService};
use crate::domains::sessions::SessionsClient;
use crate::{Error, ErrorContext as _, Result};

use super::grpc_client::{FydeClient, GrpcClient};
use super::storage::Storage;
use super::{InstallScriptRequest, InstalledScript, Script, ScriptParameter, ScriptType, Service};

/// The default [`Service`] implementation. Composes the injected `grpc`
/// client for talking to the server's scripts service, `changelog` to
/// publish script installations, and `storage` to read installed scripts
/// back once the changelog has materialized them locally.
pub(super) struct ScriptsClient<S: Storage> {
    grpc: Box<dyn FydeClient>,
    storage: S,
    changelog: Arc<dyn ChangelogService>,
}

impl<S: Storage> ScriptsClient<S> {
    pub(super) fn new(
        channel: Channel,
        sessions: Arc<SessionsClient>,
        storage: S,
        changelog: Arc<dyn ChangelogService>,
    ) -> Self {
        Self {
            grpc: Box::new(GrpcClient::new(channel, sessions)),
            storage,
            changelog,
        }
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of
    /// a live server.
    #[cfg(test)]
    fn with_grpc(
        grpc: impl FydeClient + 'static,
        storage: S,
        changelog: Arc<dyn ChangelogService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            storage,
            changelog,
        }
    }
}

#[async_trait]
impl<S: Storage> Service for ScriptsClient<S> {
    async fn create_script(
        &self,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script> {
        self.grpc
            .create_script(
                name,
                is_public,
                icon,
                script,
                description,
                short_description,
                allowed_domains,
                script_type,
                parameters,
            )
            .await
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
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script> {
        self.grpc
            .update_script(
                id,
                name,
                is_public,
                icon,
                script,
                description,
                short_description,
                allowed_domains,
                script_type,
                parameters,
            )
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

    async fn list_public_scripts(&self) -> Result<Vec<Script>> {
        self.grpc.list_public_scripts().await
    }

    async fn install_script(&self, request: InstallScriptRequest) -> Result<()> {
        let InstallScriptRequest {
            script_id,
            parameters,
        } = request;
        let script = self
            .grpc
            .fetch_script(script_id)
            .await
            .with_context(|| format!("failed to fetch script {script_id} to install"))?;

        if let Some((name, _)) = script
            .parameters
            .iter()
            .find(|(name, parameter)| parameter.required && !parameters.contains_key(*name))
        {
            return Err(Error::MissingScriptParameter {
                script_id,
                parameter: name.clone(),
            });
        }

        let content = serde_json::to_vec(&InstalledScript { script, parameters })
            .with_context(|| format!("failed to serialize installed script {script_id}"))?;

        self.changelog
            .send(EventType::ScriptInstalled, script_id, Some(&content), None)
            .await
            .with_context(|| format!("failed to publish installation of script {script_id}"))
    }

    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>> {
        self.storage
            .list_installed_scripts()
            .await
            .context("failed to list installed scripts")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::domains::changelog::ChangelogEvent;
    use crate::domains::documents::Metadata;
    use crate::domains::scripts::grpc_client::MockFydeClient;
    use crate::domains::scripts::storage::MockStorage;
    use crate::domains::scripts::{FakeInstalledScript, FakeScript, ScriptParameterType};

    fn no_parameters() -> HashMap<String, ScriptParameter> {
        HashMap::new()
    }

    /// A single `send` call recorded by [`RecordingChangelog`].
    type SentEvent = (EventType, Uuid, Option<Vec<u8>>, Option<Metadata>);

    /// A [`ChangelogService`] fake recording every event passed to
    /// [`ChangelogService::send`]. Hand-written rather than `automock`ed
    /// since `start_consume_job`'s boxed `FnMut` parameter can't be mocked
    /// (see `changelog::Service`'s doc comment).
    #[derive(Default)]
    struct RecordingChangelog {
        sent: Mutex<Vec<SentEvent>>,
    }

    #[async_trait]
    impl ChangelogService for RecordingChangelog {
        async fn ensure_master_key(&self) -> Result<()> {
            unimplemented!("not used by the scripts service")
        }

        async fn send(
            &self,
            event_type: EventType,
            subject_id: Uuid,
            content: Option<&[u8]>,
            metadata: Option<&Metadata>,
        ) -> Result<()> {
            self.sent.lock().unwrap().push((
                event_type,
                subject_id,
                content.map(<[u8]>::to_vec),
                metadata.cloned(),
            ));
            Ok(())
        }

        async fn start_consume_job(
            self: Arc<Self>,
            _callback: Box<dyn FnMut(ChangelogEvent) + Send>,
        ) -> Result<()> {
            unimplemented!("not used by the scripts service")
        }

        fn stop_consume_job(&self) {
            unimplemented!("not used by the scripts service")
        }
    }

    /// A changelog for tests that never publish anything: any recorded
    /// event is simply never looked at.
    fn unused_changelog() -> Arc<dyn ChangelogService> {
        Arc::new(RecordingChangelog::default())
    }

    fn required_parameter() -> ScriptParameter {
        ScriptParameter {
            label: "Identifiant".to_string(),
            placeholder: "jdoe".to_string(),
            parameter_type: ScriptParameterType::String,
            required: true,
            secret: false,
        }
    }

    /// A gRPC mock serving `script` from a single `fetch_script` call.
    fn grpc_fetching(script: Script) -> MockFydeClient {
        let id = script.id();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_fetch_script()
            .withf(move |requested_id| *requested_id == id)
            .times(1)
            .returning(move |_| Ok(script.clone()));
        mock_grpc
    }

    #[tokio::test]
    async fn install_script_publishes_the_script_and_its_parameters_as_json() {
        let script = FakeScript::new()
            .with_parameter("username", required_parameter())
            .build();
        let parameters = HashMap::from([("username".to_string(), json!("alice"))]);
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::with_grpc(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
        );

        client
            .install_script(InstallScriptRequest {
                script_id: script.id(),
                parameters: parameters.clone(),
            })
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::ScriptInstalled);
        assert_eq!(*id, script.id());
        assert_eq!(*metadata, None);
        let installed: InstalledScript =
            serde_json::from_slice(content.as_deref().unwrap()).unwrap();
        assert_eq!(installed.script(), &script);
        assert_eq!(installed.parameters(), &parameters);
    }

    #[tokio::test]
    async fn install_script_rejects_a_missing_required_parameter_without_publishing() {
        let script = FakeScript::new()
            .with_parameter("username", required_parameter())
            .build();
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::with_grpc(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
        );

        let err = client
            .install_script(InstallScriptRequest {
                script_id: script.id(),
                parameters: HashMap::new(),
            })
            .await
            .unwrap_err();

        assert!(
            matches!(&err, Error::MissingScriptParameter { parameter, .. } if parameter == "username"),
            "unexpected error: {err}"
        );
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn install_script_accepts_a_missing_optional_parameter() {
        let script = FakeScript::new()
            .with_parameter(
                "nickname",
                ScriptParameter {
                    required: false,
                    ..required_parameter()
                },
            )
            .build();
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::with_grpc(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
        );

        client
            .install_script(InstallScriptRequest {
                script_id: script.id(),
                parameters: HashMap::new(),
            })
            .await
            .unwrap();

        assert_eq!(changelog.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn install_script_fails_without_publishing_if_the_script_cannot_be_fetched() {
        let id = Uuid::now_v7();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_fetch_script()
            .times(1)
            .returning(|id| Err(Error::ScriptNotFound(id)));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), changelog.clone());

        let err = client
            .install_script(InstallScriptRequest {
                script_id: id,
                parameters: HashMap::new(),
            })
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("not found"),
            "unexpected error: {err}"
        );
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_installed_scripts_reads_from_local_storage() {
        let expected = vec![
            FakeInstalledScript::new().build(),
            FakeInstalledScript::new().build(),
        ];
        let returned = expected.clone();
        let mut storage = MockStorage::new();
        storage
            .expect_list_installed_scripts()
            .times(1)
            .returning(move || Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(MockFydeClient::new(), storage, unused_changelog());

        assert_eq!(client.list_installed_scripts().await.unwrap(), expected);
    }

    #[tokio::test]
    async fn create_script_delegates_to_grpc() {
        let expected = FakeScript::new().build();
        let returned = expected.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_create_script()
            .withf(
                |name,
                 is_public,
                 icon,
                 script,
                 description,
                 short_description,
                 allowed_domains,
                 script_type,
                 parameters| {
                    name == "my-script"
                        && *is_public
                        && icon == b"icon-bytes"
                        && script == "return 1"
                        && description == "does something"
                        && short_description == "does a thing"
                        && *allowed_domains == vec!["example.com".to_string()]
                        && *script_type == ScriptType::Scraper
                        && parameters.is_empty()
                },
            )
            .times(1)
            .returning(move |_, _, _, _, _, _, _, _, _| Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), unused_changelog());

        let script = client
            .create_script(
                "my-script",
                true,
                b"icon-bytes".to_vec(),
                "return 1",
                "does something",
                "does a thing",
                vec!["example.com".to_string()],
                ScriptType::Scraper,
                no_parameters(),
            )
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

        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), unused_changelog());

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
            .withf(
                move |requested_id,
                      name,
                      is_public,
                      icon,
                      script,
                      description,
                      short_description,
                      allowed_domains,
                      script_type,
                      parameters| {
                    *requested_id == id
                        && name == "my-script"
                        && *is_public
                        && icon == b"icon-bytes"
                        && script == "return 1"
                        && description == "does something"
                        && short_description == "does a thing"
                        && *allowed_domains == vec!["example.com".to_string()]
                        && *script_type == ScriptType::Scraper
                        && parameters.is_empty()
                },
            )
            .times(1)
            .returning(move |_, _, _, _, _, _, _, _, _, _| Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), unused_changelog());

        let script = client
            .update_script(
                id,
                "my-script",
                true,
                b"icon-bytes".to_vec(),
                "return 1",
                "does something",
                "does a thing",
                vec!["example.com".to_string()],
                ScriptType::Scraper,
                no_parameters(),
            )
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

        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), unused_changelog());

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

        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), unused_changelog());

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

        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), unused_changelog());

        assert_eq!(client.list_user_scripts().await.unwrap(), expected);
    }

    #[tokio::test]
    async fn list_public_scripts_delegates_to_grpc() {
        let expected = vec![FakeScript::new().build(), FakeScript::new().build()];
        let returned = expected.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_list_public_scripts()
            .times(1)
            .returning(move || Ok(returned.clone()));

        let client = ScriptsClient::with_grpc(mock_grpc, MockStorage::new(), unused_changelog());

        assert_eq!(client.list_public_scripts().await.unwrap(), expected);
    }
}
