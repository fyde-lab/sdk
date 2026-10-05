use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Channel;

use crate::domains::documents::Service as DocumentsService;
#[cfg(test)]
use crate::domains::sessions::SESSION_TOKEN_SETTING;
use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::domains::settings::Service as SettingsService;
use crate::sql::LocalDatabase;
use crate::{Error, ErrorContext as _, Result};

use super::crypto::{
    DefaultOpaqueClient, OpaqueClient, generate_and_wrap_master_key, unwrap_master_key,
    wrap_master_key,
};
use super::grpc_client::{FydeClient, GrpcClient};
use super::{
    LANGUAGE_SETTING, MASTER_KEY_SETTING, ROLE_SETTING, Role, Service, decode_master_key,
    encode_master_key,
};

/// The device's preferred interface language as a short code (e.g. "en",
/// "fr"), read from the `LC_ALL`/`LC_MESSAGES`/`LANG` environment variables
/// (checked in that order, matching POSIX locale precedence) and falling
/// back to `"en"` if none is set or parses to something unexpected (e.g.
/// the POSIX default `"C"`/`"POSIX"`). Sent to the server by `create` so it
/// doesn't need to be asked for.
fn system_language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|var| std::env::var(var).ok())
        .and_then(|value| {
            let code = value.split(['_', '.']).next().unwrap_or("").to_lowercase();
            (code.len() == 2).then_some(code)
        })
        .unwrap_or_else(|| "en".to_string())
}

/// A client for the fyde server's users service. Persists the session
/// token opened by the most recent `create`/`login` call via
/// [`SessionsClient::save_new_session`] under
/// [`crate::domains::sessions::SESSION_TOKEN_SETTING`],
/// read back from there by every other service's gRPC transport (via
/// [`crate::domains::sessions::Service::authenticated_request`]) to
/// authenticate its own calls, so [`Service::logout`] doesn't need a token
/// passed in.
pub struct UsersClient {
    grpc: Box<dyn FydeClient>,
    opaque: Box<dyn OpaqueClient>,
    settings: Arc<dyn SettingsService>,
    sessions: Arc<SessionsClient>,
    local_db: Arc<dyn LocalDatabase>,
    documents: Arc<dyn DocumentsService>,
}

impl UsersClient {
    /// Creates a client for the users service using the shared `channel`
    /// connection to the fyde server, persisting newly created accounts'
    /// master keys in `settings`, the session token and authenticating
    /// outgoing calls via `sessions`, wiping `local_db` on `logout`, and
    /// starting/stopping changelog consumption on `documents` from
    /// `create`/`login`/`logout`.
    pub(super) async fn new(
        channel: Channel,
        settings: Arc<dyn SettingsService>,
        sessions: Arc<SessionsClient>,
        local_db: Arc<dyn LocalDatabase>,
        documents: Arc<dyn DocumentsService>,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(GrpcClient::new(channel, sessions.clone())),
            opaque: Box::new(DefaultOpaqueClient),
            settings,
            sessions,
            local_db,
            documents,
        })
    }

    /// Creates a client from already-constructed [`FydeClient`] and
    /// [`OpaqueClient`] implementations, for testing against
    /// [`super::grpc_client::MockFydeClient`]/[`super::crypto::MockOpaqueClient`]
    /// instead of a live server and real cryptography.
    #[cfg(test)]
    fn with_deps(
        grpc: impl FydeClient + 'static,
        opaque: impl OpaqueClient + 'static,
        settings: Arc<dyn SettingsService>,
        sessions: Arc<SessionsClient>,
        local_db: Arc<dyn LocalDatabase>,
        documents: Arc<dyn DocumentsService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            opaque: Box::new(opaque),
            settings,
            sessions,
            local_db,
            documents,
        }
    }
}

#[async_trait]
impl Service for UsersClient {
    async fn create(&self, username: &str, password: &str, device_name: &str) -> Result<String> {
        let (state, opaque_request) = self
            .opaque
            .start_registration(password)
            .context("failed to start OPAQUE registration")?;

        let opaque_response = self
            .grpc
            .start_registration(username, &opaque_request)
            .await
            .context("failed to start registration")?;

        let (opaque_upload, export_key) = self
            .opaque
            .finish_registration(state, password, &opaque_response)
            .context("failed to finish OPAQUE registration")?;

        let (raw_master_key, wrapped_master_key) =
            generate_and_wrap_master_key(&export_key).context("failed to generate master key")?;

        let (token, role) = self
            .grpc
            .finish_registration(
                username,
                &opaque_upload,
                device_name,
                wrapped_master_key.as_bytes(),
                &system_language(),
            )
            .await
            .context("failed to finish registration")?;

        self.sessions.save_new_session(&token).await?;

        self.settings
            .set(MASTER_KEY_SETTING, &encode_master_key(&raw_master_key)?)
            .await
            .context("failed to persist master key")?;

        self.settings
            .set(ROLE_SETTING, &role.to_string())
            .await
            .context("failed to persist role")?;

        self.documents
            .start_sync()
            .await
            .context("failed to start changelog sync")?;

        Ok(token)
    }

    async fn login(&self, username: &str, password: &str, device_name: &str) -> Result<String> {
        let (state, opaque_request) = self
            .opaque
            .start_login(password)
            .context("failed to start OPAQUE login")?;

        let (login_id, opaque_response) = self
            .grpc
            .start_login(username, &opaque_request)
            .await
            .context("failed to start login")?;

        let (opaque_upload, export_key) = self
            .opaque
            .finish_login(state, password, &opaque_response)
            .context("failed to finish OPAQUE login")?;

        let (token, encrypted_master_key, role) = self
            .grpc
            .finish_login(&login_id, &opaque_upload, device_name)
            .await
            .context("failed to log in")?;

        self.sessions.save_new_session(&token).await?;

        let wrapped_master_key = String::from_utf8(encrypted_master_key)
            .map_err(|_| Error::Encryption("server returned a non-UTF-8 master key".into()))
            .context("failed to decode master key returned by the server")?;
        let raw_master_key = unwrap_master_key(&export_key, &wrapped_master_key)
            .context("failed to unwrap master key returned by the server")?;

        self.settings
            .set(MASTER_KEY_SETTING, &encode_master_key(&raw_master_key)?)
            .await
            .context("failed to persist master key")?;

        self.settings
            .set(ROLE_SETTING, &role.to_string())
            .await
            .context("failed to persist role")?;

        self.documents
            .start_sync()
            .await
            .context("failed to start changelog sync")?;

        Ok(token)
    }

    async fn logout(&self) -> Result<()> {
        if !self.sessions.is_connected().await? {
            return Ok(());
        }

        self.documents.stop_sync();

        self.grpc.logout().await.context("failed to log out")?;

        self.local_db
            .wipe()
            .await
            .context("failed to wipe local database")?;

        Ok(())
    }

    async fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<()> {
        // Verifying `old_password` only needs the local half of an OPAQUE
        // login exchange: `finish_login` fails with
        // `Error::InvalidCredentials` if it can't open the account's
        // envelope, without a round trip to the server's `FinishLogin` —
        // so this never opens a session, unlike a real `login()` call. The
        // resulting export key isn't needed for anything else: the master
        // key is read back raw from local settings below, not decrypted
        // from a wrapped form that would need it.
        let (login_state, login_request) = self
            .opaque
            .start_login(old_password)
            .context("failed to start OPAQUE login")?;
        let (_login_id, login_response) = self
            .grpc
            .start_login(username, &login_request)
            .await
            .context("failed to start login")?;
        self.opaque
            .finish_login(login_state, old_password, &login_response)
            .context("failed to verify current password")?;

        let (state, opaque_request) = self
            .opaque
            .start_registration(new_password)
            .context("failed to start OPAQUE registration")?;

        let opaque_response = self
            .grpc
            .start_change_password(&opaque_request)
            .await
            .context("failed to start change password")?;

        let (opaque_upload, new_export_key) = self
            .opaque
            .finish_registration(state, new_password, &opaque_response)
            .context("failed to finish OPAQUE registration")?;

        let raw_master_key = self
            .settings
            .get(MASTER_KEY_SETTING)
            .await
            .context("failed to read current master key")?
            .ok_or_else(|| {
                Error::Encryption(
                    "no master key found in local settings; log in or create an account first"
                        .into(),
                )
            })
            .and_then(|encoded| decode_master_key(&encoded))?;

        let new_wrapped_master_key = wrap_master_key(&raw_master_key, &new_export_key)
            .context("failed to wrap master key under the new export key")?;

        self.grpc
            .finish_change_password(&opaque_upload, new_wrapped_master_key.as_bytes())
            .await
            .context("failed to finish change password")?;

        Ok(())
    }

    async fn set_language(&self, language: &str) -> Result<()> {
        self.settings
            .set(LANGUAGE_SETTING, language)
            .await
            .context("failed to persist language preference")
    }

    async fn get_language(&self) -> Result<Option<String>> {
        self.settings
            .get(LANGUAGE_SETTING)
            .await
            .context("failed to read language preference")
    }

    async fn role(&self) -> Result<Option<Role>> {
        let Some(role) = self
            .settings
            .get(ROLE_SETTING)
            .await
            .context("failed to read cached role")?
        else {
            return Ok(None);
        };

        role.parse()
            .map(Some)
            .map_err(|err| Error::InvalidResponse(format!("invalid cached role: {err}")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::crypto::{MockOpaqueClient, fake_login_state, fake_registration_state};
    use super::super::grpc_client::MockFydeClient;
    use super::*;
    use crate::domains::settings::MockService as MockSettingsService;
    use crate::sql::MockLocalDatabase;

    fn client_with_deps(
        grpc: impl FydeClient + 'static,
        opaque: impl OpaqueClient + 'static,
        settings: MockSettingsService,
    ) -> UsersClient {
        let settings: Arc<dyn SettingsService> = Arc::new(settings);
        let sessions = Arc::new(SessionsClient::new(settings.clone()));
        let mut local_db = MockLocalDatabase::new();
        local_db.expect_wipe().returning(|| Ok(()));
        UsersClient::with_deps(
            grpc,
            opaque,
            settings,
            sessions,
            Arc::new(local_db),
            Arc::new(RecordingDocuments::default()),
        )
    }

    /// A [`DocumentsService`] fake recording whether
    /// [`DocumentsService::start_sync`]/[`DocumentsService::stop_sync`] were
    /// called, for tests that don't need a live changelog. Every other
    /// method is unused by [`UsersClient`], so left unimplemented.
    #[derive(Default)]
    struct RecordingDocuments {
        start_sync_called: std::sync::atomic::AtomicBool,
        stop_sync_called: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl DocumentsService for RecordingDocuments {
        async fn upload(
            &self,
            _request: crate::domains::documents::UploadRequest,
        ) -> Result<uuid::Uuid> {
            unimplemented!()
        }

        async fn get(
            &self,
            _id: uuid::Uuid,
        ) -> Result<Option<crate::domains::documents::Document>> {
            unimplemented!()
        }

        async fn list(
            &self,
            _offset: i64,
            _limit: i64,
        ) -> Result<Vec<crate::domains::documents::Document>> {
            unimplemented!()
        }

        async fn update_metadata(
            &self,
            _metadata: crate::domains::documents::Metadata,
        ) -> Result<()> {
            unimplemented!()
        }

        async fn run_scripts(
            &self,
            _document: &crate::domains::documents::Document,
        ) -> Result<crate::domains::documents::Metadata> {
            unimplemented!()
        }

        async fn start_sync(&self) -> Result<()> {
            self.start_sync_called
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn stop_sync(&self) {
            self.stop_sync_called
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A [`MockOpaqueClient`] wired up for a successful `create()` call:
    /// `start_registration` returns a placeholder state plus `request`,
    /// and `finish_registration` returns `upload`/`export_key` regardless
    /// of what it's called with, so real OPAQUE/Argon2 never runs in these
    /// tests.
    fn opaque_for_create(
        request: &'static [u8],
        upload: &'static [u8],
        export_key: &'static [u8],
    ) -> MockOpaqueClient {
        let mut opaque = MockOpaqueClient::new();
        opaque
            .expect_start_registration()
            .times(1)
            .returning(move |_| Ok((fake_registration_state(), request.to_vec())));
        opaque
            .expect_finish_registration()
            .times(1)
            .returning(move |_, _, _| Ok((upload.to_vec(), export_key.to_vec())));
        opaque
    }

    /// The export key [`opaque_for_login`]'s `MockOpaqueClient` produces —
    /// shared with [`login_master_key`] so callers can build a server
    /// response the resulting `login()` call can actually unwrap.
    const LOGIN_EXPORT_KEY: &[u8] = b"the-login-export-key";

    fn opaque_for_login(request: &'static [u8], upload: &'static [u8]) -> MockOpaqueClient {
        let mut opaque = MockOpaqueClient::new();
        opaque
            .expect_start_login()
            .times(1)
            .returning(move |_| Ok((fake_login_state(), request.to_vec())));
        opaque
            .expect_finish_login()
            .times(1)
            .returning(move |_, _, _| Ok((upload.to_vec(), LOGIN_EXPORT_KEY.to_vec())));
        opaque
    }

    /// A master key wrapped under [`LOGIN_EXPORT_KEY`], for `MockFydeClient`
    /// `finish_login` expectations to return as `encrypted_master_key` —
    /// what a real server would send back, and what `login()` must be able
    /// to unwrap with the export key `opaque_for_login`'s mock produces.
    fn login_master_key() -> Vec<u8> {
        super::super::crypto::generate_and_wrap_master_key(LOGIN_EXPORT_KEY)
            .unwrap()
            .1
            .into_bytes()
    }

    #[tokio::test]
    async fn create_returns_and_stores_the_session_token() {
        let opaque = opaque_for_create(b"the-request", b"the-upload", b"the-export-key");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_registration()
            .withf(|username, request| username == "alice" && request == b"the-request")
            .returning(|_, _| Ok(b"the-response".to_vec()));
        mock_grpc
            .expect_finish_registration()
            .withf(|username, upload, device_name, _, _| {
                username == "alice" && upload == b"the-upload" && device_name == "Pierre's iPhone"
            })
            .returning(|_, _, _, _, _| Ok(("a-token".to_string(), Role::User)));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(mock_grpc, opaque, settings);

        let token = client
            .create("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(token, "a-token");
    }

    #[tokio::test]
    async fn create_persists_the_same_raw_master_key_it_sends_wrapped_to_the_server() {
        let opaque = opaque_for_create(b"the-request", b"the-upload", b"the-export-key");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_registration()
            .returning(|_, _| Ok(b"the-response".to_vec()));

        // Captures what `create` sends the server as `encrypted_master_key`
        // so it can be compared against what it persists locally below.
        let sent_master_key: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let capture = sent_master_key.clone();
        mock_grpc.expect_finish_registration().times(1).returning(
            move |_, _, _, encrypted_master_key, _| {
                *capture.lock().unwrap() = Some(encrypted_master_key.to_vec());
                Ok(("a-token".to_string(), Role::User))
            },
        );

        let persisted_master_key: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        let set_master_key = persisted_master_key.clone();
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(move |_, value| {
                *set_master_key.lock().unwrap() = Some(value.to_string());
                Ok(())
            });
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(mock_grpc, opaque, settings);
        client
            .create("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        // What's sent to the server is the *wrapped* form (opaque to it);
        // what's persisted locally is the *raw* key underneath — never the
        // wrapped form itself (see `MASTER_KEY_SETTING`'s doc). Unwrapping
        // the sent bytes under the export key `opaque_for_create` produced
        // must yield exactly what was persisted.
        let sent = sent_master_key.lock().unwrap().clone().unwrap();
        let sent_wrapped = String::from_utf8(sent).unwrap();
        let unwrapped_from_sent =
            super::super::crypto::unwrap_master_key(b"the-export-key", &sent_wrapped).unwrap();

        let persisted = persisted_master_key.lock().unwrap().clone().unwrap();
        let persisted_raw = decode_master_key(&persisted).unwrap();

        assert_eq!(unwrapped_from_sent, persisted_raw);
    }

    #[tokio::test]
    async fn create_persists_an_encrypted_master_key_in_settings() {
        let opaque = opaque_for_create(b"the-request", b"the-upload", b"the-export-key");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_registration()
            .returning(|_, _| Ok(b"the-response".to_vec()));
        mock_grpc
            .expect_finish_registration()
            .returning(|_, _, _, _, _| Ok(("a-token".to_string(), Role::User)));

        // Captures what `create` persists so it can be read back and
        // asserted on below, without a generic key/value store.
        let persisted_master_key: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));
        let set_master_key = persisted_master_key.clone();
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(move |_, value| {
                *set_master_key.lock().unwrap() = Some(value.to_string());
                Ok(())
            });
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(mock_grpc, opaque, settings);

        client
            .create("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        let stored = persisted_master_key
            .lock()
            .unwrap()
            .clone()
            .expect("create must persist a master key");
        assert_ne!(stored, "correct horse battery staple");
        assert!(!stored.is_empty());
    }

    #[tokio::test]
    async fn create_persists_the_session_token_in_settings() {
        let opaque = opaque_for_create(b"the-request", b"the-upload", b"the-export-key");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_registration()
            .returning(|_, _| Ok(b"the-response".to_vec()));
        mock_grpc
            .expect_finish_registration()
            .returning(|_, _, _, _, _| Ok(("a-token".to_string(), Role::User)));

        // Captures what `create` persists under `SESSION_TOKEN_SETTING` so
        // it can be read back and asserted on below.
        let persisted_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let mut settings = MockSettingsService::new();
        let set_token = persisted_token.clone();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(move |_, value| {
                *set_token.lock().unwrap() = Some(value.to_string());
                Ok(())
            });
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(mock_grpc, opaque, settings);

        client
            .create("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(
            persisted_token.lock().unwrap().clone(),
            Some("a-token".to_string())
        );
    }

    #[tokio::test]
    async fn login_returns_and_stores_the_session_token() {
        let opaque = opaque_for_login(b"the-request", b"the-upload");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .withf(|username, request| username == "alice" && request == b"the-request")
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-response".to_vec())));
        mock_grpc
            .expect_finish_login()
            .withf(|login_id, upload, device_name| {
                login_id == "a-login-id"
                    && upload == b"the-upload"
                    && device_name == "Pierre's iPhone"
            })
            .returning(|_, _, _| Ok(("a-token".to_string(), login_master_key(), Role::User)));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(mock_grpc, opaque, settings);

        let token = client
            .login("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(token, "a-token");
    }

    #[tokio::test]
    async fn login_persists_the_session_token_in_settings() {
        let opaque = opaque_for_login(b"the-request", b"the-upload");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-response".to_vec())));
        mock_grpc
            .expect_finish_login()
            .returning(|_, _, _| Ok(("a-token".to_string(), login_master_key(), Role::User)));

        let persisted_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let mut settings = MockSettingsService::new();
        let set_token = persisted_token.clone();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(move |_, value| {
                *set_token.lock().unwrap() = Some(value.to_string());
                Ok(())
            });
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(mock_grpc, opaque, settings);

        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        assert_eq!(
            persisted_token.lock().unwrap().clone(),
            Some("a-token".to_string())
        );
    }

    #[tokio::test]
    async fn login_persists_the_raw_master_key_it_unwraps_from_the_server() {
        let opaque = opaque_for_login(b"the-request", b"the-upload");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-response".to_vec())));
        let wrapped_master_key = login_master_key();
        let returned_master_key = wrapped_master_key.clone();
        mock_grpc.expect_finish_login().returning(move |_, _, _| {
            Ok((
                "a-token".to_string(),
                returned_master_key.clone(),
                Role::User,
            ))
        });

        let persisted_master_key: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        let set_master_key = persisted_master_key.clone();
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(move |_, value| {
                *set_master_key.lock().unwrap() = Some(value.to_string());
                Ok(())
            });
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(mock_grpc, opaque, settings);

        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        // What the server returned is the *wrapped* form; what must be
        // persisted locally is the raw key underneath it, unwrapped under
        // the login's export key — never the wrapped form itself (see
        // `MASTER_KEY_SETTING`'s doc).
        let expected_raw = super::super::crypto::unwrap_master_key(
            LOGIN_EXPORT_KEY,
            &String::from_utf8(wrapped_master_key).unwrap(),
        )
        .unwrap();
        let persisted = persisted_master_key.lock().unwrap().clone().unwrap();
        assert_eq!(decode_master_key(&persisted).unwrap(), expected_raw);
    }

    #[tokio::test]
    async fn logout_closes_the_session_opened_by_login() {
        let opaque = opaque_for_login(b"the-request", b"the-upload");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-response".to_vec())));
        mock_grpc
            .expect_finish_login()
            .returning(|_, _, _| Ok(("a-token".to_string(), login_master_key(), Role::User)));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(Some("a-token".to_string())));

        let client = client_with_deps(mock_grpc, opaque, settings);
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_is_a_no_op_without_an_open_session() {
        // No `expect_logout()` set up: the mock panics if it's called.
        let mock_grpc = MockFydeClient::new();
        let opaque = MockOpaqueClient::new();

        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(None));

        let client = client_with_deps(mock_grpc, opaque, settings);

        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_forgets_the_session_token_once_used() {
        let opaque = opaque_for_login(b"the-request", b"the-upload");
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-response".to_vec())));
        mock_grpc
            .expect_finish_login()
            .returning(|_, _, _| Ok(("a-token".to_string(), login_master_key(), Role::User)));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .returning(|_, _| Ok(()));
        // Mockall checks the most-recently-defined expectation first, so
        // this "already gone" expectation (defined first, checked last)
        // matches the second `logout()` call, once the "still open" one
        // below has been used up by the first.
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(None));
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(Some("a-token".to_string())));

        let client = client_with_deps(mock_grpc, opaque, settings);
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();
        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_wipes_the_local_database() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-response".to_vec())));
        mock_grpc
            .expect_finish_login()
            .returning(|_, _, _| Ok(("a-token".to_string(), login_master_key(), Role::User)));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(Some("a-token".to_string())));
        let settings: Arc<dyn SettingsService> = Arc::new(settings);
        let sessions = Arc::new(SessionsClient::new(settings.clone()));

        let mut local_db = MockLocalDatabase::new();
        local_db.expect_wipe().times(1).returning(|| Ok(()));

        let client = UsersClient::with_deps(
            mock_grpc,
            opaque_for_login(b"the-request", b"the-upload"),
            settings,
            sessions,
            Arc::new(local_db),
            Arc::new(RecordingDocuments::default()),
        );
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_stops_a_running_sync() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-response".to_vec())));
        mock_grpc
            .expect_finish_login()
            .returning(|_, _, _| Ok(("a-token".to_string(), login_master_key(), Role::User)));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == MASTER_KEY_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_set()
            .withf(|key, _| key == ROLE_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(Some("a-token".to_string())));
        let settings: Arc<dyn SettingsService> = Arc::new(settings);
        let sessions = Arc::new(SessionsClient::new(settings.clone()));

        let mut local_db = MockLocalDatabase::new();
        local_db.expect_wipe().returning(|| Ok(()));

        let documents = Arc::new(RecordingDocuments::default());

        let client = UsersClient::with_deps(
            mock_grpc,
            opaque_for_login(b"the-request", b"the-upload"),
            settings,
            sessions,
            Arc::new(local_db),
            documents.clone(),
        );
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();

        assert!(
            documents
                .stop_sync_called
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn logout_does_not_stop_sync_without_an_open_session() {
        let mock_grpc = MockFydeClient::new();
        let opaque = MockOpaqueClient::new();

        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(None));
        let settings: Arc<dyn SettingsService> = Arc::new(settings);
        let sessions = Arc::new(SessionsClient::new(settings.clone()));
        let mut local_db = MockLocalDatabase::new();
        local_db.expect_wipe().returning(|| Ok(()));
        let documents = Arc::new(RecordingDocuments::default());

        let client = UsersClient::with_deps(
            mock_grpc,
            opaque,
            settings,
            sessions,
            Arc::new(local_db),
            documents.clone(),
        );

        client.logout().await.unwrap();

        assert!(
            !documents
                .stop_sync_called
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    const OLD_EXPORT_KEY: &[u8] = b"the-old-export-key";
    const NEW_EXPORT_KEY: &[u8] = b"the-new-export-key";

    /// A [`MockOpaqueClient`] wired up for a successful `change_password()`
    /// call: verifying the old password locally (`start_login`/
    /// `finish_login`, never reaching the server's `FinishLogin`) and
    /// registering the new one (`start_registration`/`finish_registration`),
    /// regardless of what either is called with, so real OPAQUE/Argon2
    /// never runs in these tests.
    fn opaque_for_change_password() -> MockOpaqueClient {
        let mut opaque = MockOpaqueClient::new();
        opaque
            .expect_start_login()
            .times(1)
            .returning(|_| Ok((fake_login_state(), b"the-login-request".to_vec())));
        opaque
            .expect_finish_login()
            .times(1)
            .returning(|_, _, _| Ok((b"the-login-upload".to_vec(), OLD_EXPORT_KEY.to_vec())));
        opaque
            .expect_start_registration()
            .times(1)
            .returning(|_| Ok((fake_registration_state(), b"the-new-request".to_vec())));
        opaque
            .expect_finish_registration()
            .times(1)
            .returning(|_, _, _| Ok((b"the-new-upload".to_vec(), NEW_EXPORT_KEY.to_vec())));
        opaque
    }

    #[tokio::test]
    async fn change_password_sends_the_raw_master_key_wrapped_under_the_new_export_key() {
        let opaque = opaque_for_change_password();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .withf(|username, request| username == "alice" && request == b"the-login-request")
            .times(1)
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-login-response".to_vec())));
        mock_grpc
            .expect_start_change_password()
            .withf(|request| request == b"the-new-request")
            .times(1)
            .returning(|_| Ok(b"the-registration-response".to_vec()));

        // Captures what `change_password` sends the server as the new
        // `encrypted_master_key`, so it can be checked against the raw key
        // read back from local settings below.
        let sent_master_key: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let capture = sent_master_key.clone();
        mock_grpc
            .expect_finish_change_password()
            .withf(|upload, key| upload == b"the-new-upload" && !key.is_empty())
            .times(1)
            .returning(move |_, key| {
                *capture.lock().unwrap() = Some(key.to_vec());
                Ok(())
            });

        let raw_master_key = b"the-raw-master-key--------------".to_vec();
        let encoded_master_key = encode_master_key(&raw_master_key).unwrap();

        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(move |_| Ok(Some(encoded_master_key.clone())));
        // No `expect_set()` set up: the mock panics if it's called, proving
        // `change_password` never touches local settings — the raw key it
        // already cached stays untouched, since only what protects it
        // server-side changes.

        let client = client_with_deps(mock_grpc, opaque, settings);

        client
            .change_password("alice", "old password", "new password")
            .await
            .unwrap();

        let sent = sent_master_key.lock().unwrap().clone().unwrap();
        let sent_wrapped = String::from_utf8(sent).unwrap();
        let unwrapped_from_sent =
            super::super::crypto::unwrap_master_key(NEW_EXPORT_KEY, &sent_wrapped).unwrap();
        assert_eq!(unwrapped_from_sent, raw_master_key);
    }

    #[tokio::test]
    async fn change_password_fails_on_the_wrong_old_password_without_registering_a_new_one() {
        let mut opaque = MockOpaqueClient::new();
        opaque
            .expect_start_login()
            .times(1)
            .returning(|_| Ok((fake_login_state(), b"the-login-request".to_vec())));
        opaque
            .expect_finish_login()
            .times(1)
            .returning(|_, _, _| Err(Error::InvalidCredentials));
        // No `expect_start_registration()`/`expect_finish_registration()`
        // set up: the mock panics if either is called, proving a wrong old
        // password never reaches the new-password registration exchange.

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .times(1)
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-login-response".to_vec())));
        // No `expect_start_change_password()`/`expect_finish_change_password()`
        // set up either: the mock panics if either is called.

        let settings = MockSettingsService::new();
        let client = client_with_deps(mock_grpc, opaque, settings);

        let err = client
            .change_password("alice", "wrong password", "new password")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Context { .. }));
    }

    #[tokio::test]
    async fn change_password_fails_without_a_master_key_in_settings() {
        let opaque = opaque_for_change_password();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_start_login()
            .times(1)
            .returning(|_, _| Ok(("a-login-id".to_string(), b"the-login-response".to_vec())));
        mock_grpc
            .expect_start_change_password()
            .times(1)
            .returning(|_| Ok(b"the-registration-response".to_vec()));
        // No `expect_finish_change_password()` set up: the mock panics if
        // it's called, proving a missing local master key is caught before
        // ever sending the new registration record to the server.

        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == MASTER_KEY_SETTING)
            .times(1)
            .returning(|_| Ok(None));

        let client = client_with_deps(mock_grpc, opaque, settings);

        let err = client
            .change_password("alice", "old password", "new password")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Encryption(_)));
    }

    #[tokio::test]
    async fn set_language_persists_it_under_language_setting() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, value| key == LANGUAGE_SETTING && value == "fr")
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_deps(MockFydeClient::new(), MockOpaqueClient::new(), settings);

        client.set_language("fr").await.unwrap();
    }

    #[tokio::test]
    async fn get_language_returns_the_persisted_value() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == LANGUAGE_SETTING)
            .times(1)
            .returning(|_| Ok(Some("fr".to_string())));

        let client = client_with_deps(MockFydeClient::new(), MockOpaqueClient::new(), settings);

        assert_eq!(client.get_language().await.unwrap(), Some("fr".to_string()));
    }

    #[tokio::test]
    async fn get_language_returns_none_when_never_set() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == LANGUAGE_SETTING)
            .times(1)
            .returning(|_| Ok(None));

        let client = client_with_deps(MockFydeClient::new(), MockOpaqueClient::new(), settings);

        assert_eq!(client.get_language().await.unwrap(), None);
    }

    #[tokio::test]
    async fn role_returns_the_cached_value() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == ROLE_SETTING)
            .times(1)
            .returning(|_| Ok(Some("admin".to_string())));

        let client = client_with_deps(MockFydeClient::new(), MockOpaqueClient::new(), settings);

        assert_eq!(client.role().await.unwrap(), Some(Role::Admin));
    }

    #[tokio::test]
    async fn role_returns_none_when_never_set() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == ROLE_SETTING)
            .times(1)
            .returning(|_| Ok(None));

        let client = client_with_deps(MockFydeClient::new(), MockOpaqueClient::new(), settings);

        assert_eq!(client.role().await.unwrap(), None);
    }
}
