use std::sync::Arc;

use async_trait::async_trait;

use crate::services::sessions::{
    SESSION_TOKEN_SETTING, Service as SessionsService, SessionsClient,
};
use crate::services::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

use super::Service;
use super::crypto::generate_and_wrap_master_key;
use super::grpc_client::{FydeClient, GrpcClient};

/// The settings key under which a newly created account's encrypted
/// master key is persisted.
const MASTER_KEY_SETTING: &str = "master_key";

/// A client for the fyde server's users service. Persists the session
/// token opened by the most recent `create`/`login` call via
/// [`SessionsClient::save_new_session`] under [`SESSION_TOKEN_SETTING`],
/// read back from there by every other service's gRPC transport (via
/// [`crate::services::sessions::Service::authenticated_request`]) to
/// authenticate its own calls, so [`Service::logout`] doesn't need a token
/// passed in.
pub struct UsersClient {
    grpc: Box<dyn FydeClient>,
    settings: Arc<dyn SettingsService>,
    sessions: Arc<SessionsClient>,
}

impl UsersClient {
    /// Creates a client for the users service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`), persisting
    /// newly created accounts' master keys in `settings`, and the session
    /// token and authenticating outgoing calls via `sessions`.
    pub(super) async fn new(
        base_url: impl AsRef<str>,
        settings: Arc<dyn SettingsService>,
        sessions: Arc<SessionsClient>,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(
                GrpcClient::new(base_url, sessions.clone())
                    .await
                    .context("failed to create users grpc client")?,
            ),
            settings,
            sessions,
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of
    /// a live server.
    #[cfg(test)]
    fn with_grpc(
        grpc: impl FydeClient + 'static,
        settings: Arc<dyn SettingsService>,
        sessions: Arc<SessionsClient>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            settings,
            sessions,
        }
    }
}

#[async_trait]
impl Service for UsersClient {
    async fn create(&self, username: &str, password: &str, device_name: &str) -> Result<String> {
        let token = self
            .grpc
            .create_user(username, password, device_name)
            .await
            .context("failed to create user")?;

        self.sessions.save_new_session(&token).await?;

        let wrapped_master_key =
            generate_and_wrap_master_key(password).context("failed to generate master key")?;
        self.settings
            .set(MASTER_KEY_SETTING, &wrapped_master_key)
            .await
            .context("failed to persist master key")?;

        Ok(token)
    }

    async fn login(&self, username: &str, password: &str, device_name: &str) -> Result<String> {
        let token = self
            .grpc
            .login(username, password, device_name)
            .await
            .context("failed to log in")?;

        self.sessions.save_new_session(&token).await?;

        Ok(token)
    }

    async fn logout(&self) -> Result<()> {
        let has_open_session = self
            .settings
            .get(SESSION_TOKEN_SETTING)
            .await
            .context("failed to read session token")?
            .is_some();
        if !has_open_session {
            return Ok(());
        }

        self.grpc.logout().await.context("failed to log out")?;

        self.sessions.remove_session().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::grpc_client::MockFydeClient;
    use super::*;
    use crate::services::settings::MockService as MockSettingsService;

    fn client_with_grpc(
        grpc: impl FydeClient + 'static,
        settings: MockSettingsService,
    ) -> UsersClient {
        let settings: Arc<dyn SettingsService> = Arc::new(settings);
        let sessions = Arc::new(SessionsClient::new(settings.clone()));
        UsersClient::with_grpc(grpc, settings, sessions)
    }

    #[tokio::test]
    async fn create_returns_and_stores_the_session_token() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_create_user()
            .withf(|username, password, device_name| {
                username == "alice"
                    && password == "correct horse battery staple"
                    && device_name == "Pierre's iPhone"
            })
            .returning(|_, _, _| Ok("a-token".to_string()));

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

        let client = client_with_grpc(mock_grpc, settings);

        let token = client
            .create("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(token, "a-token");
    }

    #[tokio::test]
    async fn create_persists_an_encrypted_master_key_in_settings() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_create_user()
            .returning(|_, _, _| Ok("a-token".to_string()));

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

        let client = client_with_grpc(mock_grpc, settings);

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
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_create_user()
            .returning(|_, _, _| Ok("a-token".to_string()));

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

        let client = client_with_grpc(mock_grpc, settings);

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
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(|_, _, _| Ok("a-token".to_string()));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client_with_grpc(mock_grpc, settings);

        let token = client
            .login("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(token, "a-token");
    }

    #[tokio::test]
    async fn login_persists_the_session_token_in_settings() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(|_, _, _| Ok("a-token".to_string()));

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

        let client = client_with_grpc(mock_grpc, settings);

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
    async fn login_does_not_persist_a_master_key() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(|_, _, _| Ok("a-token".to_string()));

        // `login` persists the session token but never a master key: no
        // `expect_set()` for `MASTER_KEY_SETTING` is set up, so the mock
        // panics if it's called with that key, which is how this test
        // proves it isn't. `expect_get()` is only for the direct
        // post-check below.
        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        settings.expect_get().returning(|_| Ok(None));

        let client = client_with_grpc(mock_grpc, settings);

        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        assert_eq!(client.settings.get(MASTER_KEY_SETTING).await.unwrap(), None);
    }

    #[tokio::test]
    async fn logout_closes_the_session_opened_by_login() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(|_, _, _| Ok("a-token".to_string()));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
            .returning(|_, _| Ok(()));
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(Some("a-token".to_string())));
        settings
            .expect_delete()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(()));

        let client = client_with_grpc(mock_grpc, settings);
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

        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(None));
        // No `expect_delete()` set up: the early return means settings is
        // never told to remove anything.

        let client = client_with_grpc(mock_grpc, settings);

        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_forgets_the_session_token_once_used() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(|_, _, _| Ok("a-token".to_string()));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, _| key == SESSION_TOKEN_SETTING)
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
        settings
            .expect_delete()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(()));

        let client = client_with_grpc(mock_grpc, settings);
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();
        client.logout().await.unwrap();
    }
}
