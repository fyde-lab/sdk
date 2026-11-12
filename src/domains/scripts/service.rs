use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use crate::domains::changelog::{EventType, Service as ChangelogService};
use crate::domains::folders::Service as FoldersService;
use crate::{ErrorContext as _, ErrorKind, Result};

use super::grpc_client::{FydeClient, ScriptMetadataPage};
use super::storage::Storage;
use super::{
    InstallScriptRequest, InstalledScript, MAX_INSTALLATION_LABEL_CHARS, Script, ScriptMetadata,
    ScriptParameterType, Service,
};

/// Collects every page of one of the server's paginated listings, from the
/// first one on: `list_page` lists the page a token points at (`None` for
/// the first), and listing stops at the page handing out no next token.
async fn all_pages<F>(list_page: impl Fn(Option<String>) -> F) -> Result<Vec<ScriptMetadata>>
where
    F: Future<Output = Result<ScriptMetadataPage>>,
{
    let mut scripts = Vec::new();
    let mut page_token = None;
    loop {
        let page = list_page(page_token).await?;
        scripts.extend(page.scripts);
        match page.next_page_token {
            Some(next_page_token) => page_token = Some(next_page_token),
            None => return Ok(scripts),
        }
    }
}

/// The default [`Service`] implementation. Composes the injected `grpc`
/// client for talking to the server's scripts service, `changelog` to
/// publish script installations, `storage` to read installed scripts back
/// once the changelog has materialized them locally, and `folders` to check
/// the folder an installation files its documents in.
pub(super) struct ScriptsClient<S: Storage> {
    grpc: Box<dyn FydeClient>,
    storage: S,
    changelog: Arc<dyn ChangelogService>,
    folders: Arc<dyn FoldersService>,
}

impl<S: Storage> ScriptsClient<S> {
    pub(super) fn new(
        grpc: impl FydeClient + 'static,
        storage: S,
        changelog: Arc<dyn ChangelogService>,
        folders: Arc<dyn FoldersService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            storage,
            changelog,
            folders,
        }
    }
}

#[async_trait]
impl<S: Storage> Service for ScriptsClient<S> {
    async fn fetch_script(&self, id: Uuid) -> Result<Script> {
        self.grpc.fetch_script(id).await
    }

    async fn enable_script(&self, script_id: Uuid) -> Result<()> {
        self.grpc.enable_script(script_id).await
    }

    async fn disable_script(&self, script_id: Uuid) -> Result<()> {
        self.grpc.disable_script(script_id).await
    }

    async fn list_user_scripts(&self) -> Result<Vec<ScriptMetadata>> {
        all_pages(|page_token| self.grpc.list_user_scripts(page_token)).await
    }

    async fn list_public_scripts(&self) -> Result<Vec<ScriptMetadata>> {
        all_pages(|page_token| self.grpc.list_public_scripts(page_token)).await
    }

    async fn install_script(&self, request: InstallScriptRequest) -> Result<Uuid> {
        let InstallScriptRequest {
            installation_id,
            script_id,
            label,
            parameters,
            folder_id,
        } = request;

        // Checked first, against the folders synced here: no point fetching
        // the script for an installation that can't be published.
        if let Some(folder_id) = folder_id
            && !self
                .folders
                .list()
                .await
                .with_context(|| format!("failed to list folders to install script {script_id}"))?
                .iter()
                .any(|folder| folder.id == folder_id)
        {
            return Err(ErrorKind::FolderNotFound(folder_id).into());
        }

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
            return Err(ErrorKind::MissingScriptParameter {
                script_id,
                parameter: name.clone(),
            }
            .into());
        }

        let label = match label.trim() {
            "" => default_label(&script, &parameters),
            label => {
                check_label(label)?;
                label.to_string()
            }
        };
        let installation_id = installation_id.unwrap_or_else(Uuid::now_v7);

        let content = serde_json::to_vec(&InstalledScript {
            installation_id,
            label,
            script,
            parameters,
            folder_id,
        })
        .with_context(|| format!("failed to serialize installation {installation_id}"))?;

        self.changelog
            .send(
                EventType::InstallScript,
                installation_id,
                Some(&content),
                None,
            )
            .await
            .with_context(|| {
                format!("failed to publish installation {installation_id} of script {script_id}")
            })?;
        Ok(installation_id)
    }

    async fn uninstall_script(&self, installation_id: Uuid) -> Result<()> {
        self.changelog
            .send(EventType::UninstallScript, installation_id, None, None)
            .await
            .with_context(|| format!("failed to publish uninstallation {installation_id}"))
    }

    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>> {
        self.storage
            .list_installed_scripts()
            .await
            .context("failed to list installed scripts")
    }

    async fn fetch_installed_script(&self, installation_id: Uuid) -> Result<InstalledScript> {
        self.storage
            .find_installed_script(installation_id)
            .await
            .with_context(|| format!("failed to look up installation {installation_id}"))?
            .ok_or_else(|| ErrorKind::ScriptNotInstalled(installation_id).into())
    }
}

/// The parameter a blank label defaults to first (see [`default_label`]):
/// the name most scrapers give their login.
const USERNAME_PARAMETER: &str = "username";

/// Refuses a (trimmed, non-blank) installation label over
/// [`MAX_INSTALLATION_LABEL_CHARS`] or holding a control character.
fn check_label(label: &str) -> Result<()> {
    let chars = label.chars().count();
    if chars > MAX_INSTALLATION_LABEL_CHARS {
        return Err(ErrorKind::InvalidInstallationLabel(format!(
            "{chars} characters, over {MAX_INSTALLATION_LABEL_CHARS}"
        ))
        .into());
    }
    if label.chars().any(char::is_control) {
        return Err(ErrorKind::InvalidInstallationLabel(
            "contains a control character".to_string(),
        )
        .into());
    }
    Ok(())
}

/// The label an installation installed with a blank one gets: the first
/// non-blank value of `script`'s non-secret text parameters, `username`
/// first (most scrapers', telling the user's accounts on the site apart),
/// then the others by name, else the script's name. Whitespace is collapsed and
/// control characters dropped, then it's cut to
/// [`MAX_INSTALLATION_LABEL_CHARS`], so it always passes [`check_label`].
fn default_label(script: &Script, parameters: &HashMap<String, Value>) -> String {
    let mut names: Vec<&String> = script
        .parameters
        .iter()
        .filter(|(_, parameter)| {
            !parameter.secret && parameter.parameter_type == ScriptParameterType::String
        })
        .map(|(name, _)| name)
        .collect();
    names.sort_by_key(|name| (name.as_str() != USERNAME_PARAMETER, name.as_str()));
    let value = names
        .into_iter()
        .filter_map(|name| parameters.get(name)?.as_str())
        .map(clean_label)
        .find(|value| !value.is_empty());
    let label = value.unwrap_or_else(|| clean_label(&script.name));
    let label: String = label.chars().take(MAX_INSTALLATION_LABEL_CHARS).collect();
    label.trim_end().to_string()
}

/// `text` with its control characters dropped and its whitespace runs
/// collapsed to single spaces, trimmed.
fn clean_label(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::domains::changelog::ChangelogEvent;
    use crate::domains::documents::Metadata;
    use crate::domains::folders::{
        FakeFolder, MockService as MockFoldersService, folders_listing, no_folders,
    };
    use crate::domains::scripts::grpc_client::MockFydeClient;
    use crate::domains::scripts::storage::MockStorage;
    use crate::domains::scripts::{
        FakeInstalledScript, FakeScript, FakeScriptMetadata, ScriptParameter, ScriptParameterType,
    };

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

    /// A folders service whose every call panics: for tests that never
    /// look at folders.
    fn unused_folders() -> Arc<dyn FoldersService> {
        Arc::new(MockFoldersService::new())
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

    /// The installation `changelog` recorded as the one event it was sent,
    /// an `InstallScript` for `installation_id` without metadata.
    fn sent_installation(changelog: &RecordingChangelog, installation_id: Uuid) -> InstalledScript {
        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::InstallScript);
        assert_eq!(*id, installation_id);
        assert_eq!(*metadata, None);
        serde_json::from_slice(content.as_deref().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn install_script_publishes_the_installation_as_json() {
        let script = FakeScript::new()
            .with_parameter("username", required_parameter())
            .build();
        let parameters = HashMap::from([("username".to_string(), json!("alice"))]);
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::new(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
            unused_folders(),
        );

        let installation_id = client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: script.id(),
                label: "  Compte joint ".to_string(),
                parameters: parameters.clone(),
                folder_id: None,
            })
            .await
            .unwrap();

        let installed = sent_installation(&changelog, installation_id);
        assert_eq!(installed.installation_id(), installation_id);
        assert_eq!(installed.label(), "Compte joint");
        assert_eq!(installed.script(), &script);
        assert_eq!(installed.parameters(), &parameters);
    }

    #[tokio::test]
    async fn install_script_gives_each_new_installation_its_own_id() {
        let script = FakeScript::new().build();
        let mut mock_grpc = MockFydeClient::new();
        let served = script.clone();
        mock_grpc
            .expect_fetch_script()
            .times(2)
            .returning(move |_| Ok(served.clone()));
        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );
        let request = InstallScriptRequest {
            installation_id: None,
            script_id: script.id(),
            label: String::new(),
            parameters: HashMap::new(),
            folder_id: None,
        };

        let first = client.install_script(request.clone()).await.unwrap();
        let second = client.install_script(request).await.unwrap();

        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn install_script_replaces_the_given_installation() {
        let script = FakeScript::new().build();
        let existing = Uuid::now_v7();
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::new(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
            unused_folders(),
        );

        let installation_id = client
            .install_script(InstallScriptRequest {
                installation_id: Some(existing),
                script_id: script.id(),
                label: "Marie".to_string(),
                parameters: HashMap::new(),
                folder_id: None,
            })
            .await
            .unwrap();

        assert_eq!(installation_id, existing);
        assert_eq!(
            sent_installation(&changelog, existing).installation_id(),
            existing
        );
    }

    /// Installs a script with `parameters` declared as `declared`, under a
    /// blank label, and returns the label it got.
    async fn default_label_of(
        declared: Vec<(&str, ScriptParameter)>,
        parameters: HashMap<String, Value>,
    ) -> String {
        let mut script = FakeScript::new().with_name("Ameli");
        for (name, parameter) in declared {
            script = script.with_parameter(name, parameter);
        }
        let script = script.build();
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::new(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
            unused_folders(),
        );

        let installation_id = client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: script.id(),
                label: " \t".to_string(),
                parameters,
                folder_id: None,
            })
            .await
            .unwrap();

        sent_installation(&changelog, installation_id)
            .label()
            .to_string()
    }

    fn secret_parameter() -> ScriptParameter {
        ScriptParameter {
            secret: true,
            ..required_parameter()
        }
    }

    #[tokio::test]
    async fn a_blank_label_defaults_to_the_username_first() {
        let label = default_label_of(
            vec![
                ("login_url", required_parameter()),
                ("password", secret_parameter()),
                ("username", required_parameter()),
            ],
            HashMap::from([
                ("login_url".to_string(), json!("https://acme.example.com")),
                ("password".to_string(), json!("hunter2")),
                ("username".to_string(), json!("  marie\n dupont ")),
            ]),
        )
        .await;

        assert_eq!(label, "marie dupont");
    }

    #[tokio::test]
    async fn a_blank_label_defaults_to_the_first_other_non_secret_text_parameter_by_name() {
        let label = default_label_of(
            vec![
                ("password", secret_parameter()),
                ("identifiant", required_parameter()),
                ("zone", required_parameter()),
            ],
            HashMap::from([
                ("password".to_string(), json!("hunter2")),
                ("identifiant".to_string(), json!("2840775112048")),
                ("zone".to_string(), json!("nord")),
            ]),
        )
        .await;

        assert_eq!(label, "2840775112048");
    }

    #[tokio::test]
    async fn a_blank_label_skips_blank_and_non_text_values() {
        let label = default_label_of(
            vec![
                ("a_blank", required_parameter()),
                (
                    "b_number",
                    ScriptParameter {
                        parameter_type: ScriptParameterType::Number,
                        ..required_parameter()
                    },
                ),
                ("c_username", required_parameter()),
            ],
            HashMap::from([
                ("a_blank".to_string(), json!("   ")),
                ("b_number".to_string(), json!(42)),
                ("c_username".to_string(), json!("marie")),
            ]),
        )
        .await;

        assert_eq!(label, "marie");
    }

    #[tokio::test]
    async fn a_blank_label_falls_back_to_the_script_name() {
        let label = default_label_of(
            vec![("password", secret_parameter())],
            HashMap::from([("password".to_string(), json!("hunter2"))]),
        )
        .await;

        assert_eq!(label, "Ameli");
    }

    #[tokio::test]
    async fn a_default_label_is_cut_to_the_maximum_length() {
        let label = default_label_of(
            vec![("username", required_parameter())],
            HashMap::from([("username".to_string(), json!("é".repeat(100)))]),
        )
        .await;

        assert_eq!(label, "é".repeat(MAX_INSTALLATION_LABEL_CHARS));
    }

    #[tokio::test]
    async fn install_script_rejects_an_invalid_label_without_publishing() {
        for label in [
            "x".repeat(MAX_INSTALLATION_LABEL_CHARS + 1),
            "Marie\u{7}".to_string(),
        ] {
            let script = FakeScript::new().build();
            let changelog = Arc::new(RecordingChangelog::default());
            let client = ScriptsClient::new(
                grpc_fetching(script.clone()),
                MockStorage::new(),
                changelog.clone(),
                unused_folders(),
            );

            let err = client
                .install_script(InstallScriptRequest {
                    installation_id: None,
                    script_id: script.id(),
                    label,
                    parameters: HashMap::new(),
                    folder_id: None,
                })
                .await
                .unwrap_err();

            assert_eq!(err.code(), crate::ErrorCode::InvalidInstallationLabel);
            assert!(changelog.sent.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn a_label_of_exactly_the_maximum_length_is_accepted() {
        let script = FakeScript::new().build();
        let client = ScriptsClient::new(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );

        client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: script.id(),
                label: "é".repeat(MAX_INSTALLATION_LABEL_CHARS),
                parameters: HashMap::new(),
                folder_id: None,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn uninstall_script_publishes_an_uninstall_event_for_the_installation() {
        let installation_id = Uuid::now_v7();
        let changelog = Arc::new(RecordingChangelog::default());
        // No storage or gRPC expectations: uninstalling only publishes.
        let client = ScriptsClient::new(
            MockFydeClient::new(),
            MockStorage::new(),
            changelog.clone(),
            unused_folders(),
        );

        client.uninstall_script(installation_id).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(
            *sent,
            vec![(EventType::UninstallScript, installation_id, None, None)]
        );
    }

    #[tokio::test]
    async fn install_script_publishes_the_folder_its_documents_go_to() {
        let script = FakeScript::new().build();
        let folder = FakeFolder::new().build();
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::new(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
            folders_listing(vec![FakeFolder::new().build(), folder.clone()]),
        );

        let installation_id = client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: script.id(),
                label: String::new(),
                parameters: HashMap::new(),
                folder_id: Some(folder.id),
            })
            .await
            .unwrap();

        assert_eq!(
            sent_installation(&changelog, installation_id).folder_id(),
            Some(folder.id)
        );
    }

    #[tokio::test]
    async fn install_script_rejects_a_folder_not_synced_here_without_fetching_or_publishing() {
        let folder_id = Uuid::now_v7();
        let changelog = Arc::new(RecordingChangelog::default());
        // Strict mock: fetching the script panics.
        let client = ScriptsClient::new(
            MockFydeClient::new(),
            MockStorage::new(),
            changelog.clone(),
            no_folders(),
        );

        let err = client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: Uuid::now_v7(),
                label: String::new(),
                parameters: HashMap::new(),
                folder_id: Some(folder_id),
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err.kind(), ErrorKind::FolderNotFound(id) if *id == folder_id),
            "unexpected error: {err}"
        );
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn install_script_rejects_a_missing_required_parameter_without_publishing() {
        let script = FakeScript::new()
            .with_parameter("username", required_parameter())
            .build();
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::new(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
            unused_folders(),
        );

        let err = client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: script.id(),
                label: String::new(),
                parameters: HashMap::new(),
                folder_id: None,
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err.kind(), ErrorKind::MissingScriptParameter { parameter, .. } if parameter == "username"),
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
        let client = ScriptsClient::new(
            grpc_fetching(script.clone()),
            MockStorage::new(),
            changelog.clone(),
            unused_folders(),
        );

        client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: script.id(),
                label: String::new(),
                parameters: HashMap::new(),
                folder_id: None,
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
            .returning(|_| Err(ErrorKind::InvalidResponse("boom".to_string()).into()));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            changelog.clone(),
            unused_folders(),
        );

        let err = client
            .install_script(InstallScriptRequest {
                installation_id: None,
                script_id: id,
                label: String::new(),
                parameters: HashMap::new(),
                folder_id: None,
            })
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidServerResponse);
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

        let client = ScriptsClient::new(
            MockFydeClient::new(),
            storage,
            unused_changelog(),
            unused_folders(),
        );

        assert_eq!(client.list_installed_scripts().await.unwrap(), expected);
    }

    #[tokio::test]
    async fn fetch_installed_script_returns_the_installation() {
        let wanted = FakeInstalledScript::new().build();
        let id = wanted.installation_id();
        let returned = wanted.clone();
        let mut storage = MockStorage::new();
        storage
            .expect_find_installed_script()
            .withf(move |requested| *requested == id)
            .times(1)
            .returning(move |_| Ok(Some(returned.clone())));

        let client = ScriptsClient::new(
            MockFydeClient::new(),
            storage,
            unused_changelog(),
            unused_folders(),
        );

        assert_eq!(client.fetch_installed_script(id).await.unwrap(), wanted);
    }

    #[tokio::test]
    async fn fetch_installed_script_fails_when_it_hasnt_synced_here() {
        let mut storage = MockStorage::new();
        storage
            .expect_find_installed_script()
            .times(1)
            .returning(|_| Ok(None));

        let client = ScriptsClient::new(
            MockFydeClient::new(),
            storage,
            unused_changelog(),
            unused_folders(),
        );

        let err = client
            .fetch_installed_script(Uuid::new_v4())
            .await
            .unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::ScriptNotInstalled);
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

        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );

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

        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );

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

        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );

        client.disable_script(id).await.unwrap();
    }

    #[tokio::test]
    async fn list_user_scripts_fetches_every_page() {
        let first = FakeScriptMetadata::new().build();
        let second = FakeScriptMetadata::new().build();
        let mut mock_grpc = MockFydeClient::new();
        let first_page = ScriptMetadataPage {
            scripts: vec![first.clone()],
            next_page_token: Some("next".to_string()),
        };
        mock_grpc
            .expect_list_user_scripts()
            .withf(|page_token| page_token.is_none())
            .times(1)
            .return_once(move |_| Ok(first_page));
        let last_page = ScriptMetadataPage {
            scripts: vec![second.clone()],
            next_page_token: None,
        };
        mock_grpc
            .expect_list_user_scripts()
            .withf(|page_token| page_token.as_deref() == Some("next"))
            .times(1)
            .return_once(move |_| Ok(last_page));

        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );

        assert_eq!(
            client.list_user_scripts().await.unwrap(),
            vec![first, second]
        );
    }

    #[tokio::test]
    async fn list_public_scripts_fetches_every_page() {
        let first = FakeScriptMetadata::new().build();
        let second = FakeScriptMetadata::new().build();
        let mut mock_grpc = MockFydeClient::new();
        let first_page = ScriptMetadataPage {
            scripts: vec![first.clone()],
            next_page_token: Some("next".to_string()),
        };
        mock_grpc
            .expect_list_public_scripts()
            .withf(|page_token| page_token.is_none())
            .times(1)
            .return_once(move |_| Ok(first_page));
        let last_page = ScriptMetadataPage {
            scripts: vec![second.clone()],
            next_page_token: None,
        };
        mock_grpc
            .expect_list_public_scripts()
            .withf(|page_token| page_token.as_deref() == Some("next"))
            .times(1)
            .return_once(move |_| Ok(last_page));

        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );

        assert_eq!(
            client.list_public_scripts().await.unwrap(),
            vec![first, second]
        );
    }

    #[tokio::test]
    async fn list_public_scripts_fails_if_a_page_fails() {
        let mut mock_grpc = MockFydeClient::new();
        let first_page = ScriptMetadataPage {
            scripts: vec![FakeScriptMetadata::new().build()],
            next_page_token: Some("next".to_string()),
        };
        mock_grpc
            .expect_list_public_scripts()
            .withf(|page_token| page_token.is_none())
            .times(1)
            .return_once(move |_| Ok(first_page));
        mock_grpc
            .expect_list_public_scripts()
            .withf(|page_token| page_token.is_some())
            .times(1)
            .return_once(|_| Err(ErrorKind::InvalidResponse("boom".to_string()).into()));

        let client = ScriptsClient::new(
            mock_grpc,
            MockStorage::new(),
            unused_changelog(),
            unused_folders(),
        );

        client.list_public_scripts().await.unwrap_err();
    }
}
