use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::services::settings::Service as SettingsService;
use crate::session::SessionTokenStore;
use crate::{Error, ErrorContext as _, Result};

use super::Service;
use super::crypto::generate_and_wrap_master_key;
use super::grpc_client::{FydeClient, GrpcClient, ProtoUser};

/// The settings key under which a newly created account's encrypted
/// master key is persisted.
const MASTER_KEY_SETTING: &str = "master_key";

impl TryFrom<ProtoUser> for super::User {
    type Error = Error;

    fn try_from(proto: ProtoUser) -> Result<Self> {
        Ok(Self {
            id: Uuid::parse_str(&proto.id)?,
            username: proto.username,
            created_at: proto.created_at,
        })
    }
}

/// A client for the fyde server's users service. Writes the session token
/// opened by the most recent `create`/`login` call into the shared
/// [`SessionTokenStore`] also held by every other service's gRPC
/// transport, so [`Service::logout`] doesn't need one passed in and other
/// services' calls authenticate automatically (see
/// [`crate::session::AuthInterceptor`]).
pub struct UsersClient {
    grpc: Box<dyn FydeClient>,
    tokens: SessionTokenStore,
    settings: Arc<dyn SettingsService>,
}

impl UsersClient {
    /// Creates a client for the users service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`), sharing
    /// `tokens` with every other service's gRPC transport and persisting
    /// newly created accounts' master keys in `settings`.
    pub(super) async fn new(
        base_url: impl AsRef<str>,
        tokens: SessionTokenStore,
        settings: Arc<dyn SettingsService>,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(
                GrpcClient::new(base_url, tokens.clone())
                    .await
                    .context("failed to create users grpc client")?,
            ),
            tokens,
            settings,
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of
    /// a live server.
    #[cfg(test)]
    fn with_grpc(grpc: impl FydeClient + 'static, settings: Arc<dyn SettingsService>) -> Self {
        Self {
            grpc: Box::new(grpc),
            tokens: SessionTokenStore::default(),
            settings,
        }
    }
}

#[async_trait]
impl Service for UsersClient {
    async fn create(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
    ) -> Result<super::User> {
        let (proto_user, token) = self
            .grpc
            .create_user(username, password, device_name)
            .await
            .context("failed to create user")?;
        let user = super::User::try_from(proto_user).context("failed to parse created user")?;

        self.tokens.set(token);

        let wrapped_master_key =
            generate_and_wrap_master_key(password).context("failed to generate master key")?;
        self.settings
            .set(MASTER_KEY_SETTING, &wrapped_master_key)
            .await
            .context("failed to persist master key")?;

        Ok(user)
    }

    async fn login(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
    ) -> Result<super::User> {
        let (proto_user, token) = self
            .grpc
            .login(username, password, device_name)
            .await
            .context("failed to log in")?;
        let user = super::User::try_from(proto_user).context("failed to parse logged-in user")?;

        self.tokens.set(token);

        Ok(user)
    }

    async fn logout(&self) -> Result<()> {
        if self.tokens.get().is_none() {
            return Ok(());
        }

        self.grpc.logout().await.context("failed to log out")?;

        self.tokens.take();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::super::grpc_client::MockFydeClient;
    use super::*;

    /// An in-memory [`SettingsService`] fake, so tests can assert on what
    /// `create` persisted without touching SQLite.
    #[derive(Default)]
    struct FakeSettings {
        values: Mutex<HashMap<String, String>>,
    }

    #[async_trait]
    impl SettingsService for FakeSettings {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            Ok(self.values.lock().unwrap().get(key).cloned())
        }

        async fn set(&self, key: &str, value: &str) -> Result<()> {
            self.values
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        async fn delete(&self, key: &str) -> Result<()> {
            self.values.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn proto_user() -> ProtoUser {
        ProtoUser {
            id: Uuid::new_v4().to_string(),
            username: "alice".to_string(),
            created_at: 1_700_000_000,
        }
    }

    fn client_with_grpc(grpc: impl FydeClient + 'static) -> UsersClient {
        UsersClient::with_grpc(grpc, Arc::new(FakeSettings::default()))
    }

    #[tokio::test]
    async fn create_returns_the_created_user() {
        let user = proto_user();
        let username = user.username.clone();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_create_user()
            .withf(|username, password, device_name| {
                username == "alice"
                    && password == "correct horse battery staple"
                    && device_name == "Pierre's iPhone"
            })
            .returning(move |_, _, _| Ok((user.clone(), "a-token".to_string())));

        let client = client_with_grpc(mock_grpc);

        let created = client
            .create("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(created.username, username);
        assert_eq!(client.tokens.get(), Some("a-token".to_string()));
    }

    #[tokio::test]
    async fn create_persists_an_encrypted_master_key_in_settings() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_create_user()
            .returning(move |_, _, _| Ok((proto_user(), "a-token".to_string())));

        let client = client_with_grpc(mock_grpc);

        client
            .create("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        let stored = client
            .settings
            .get(MASTER_KEY_SETTING)
            .await
            .unwrap()
            .expect("create must persist a master key");
        assert_ne!(stored, "correct horse battery staple");
        assert!(!stored.is_empty());
    }

    #[tokio::test]
    async fn login_returns_the_authenticated_user() {
        let user = proto_user();
        let username = user.username.clone();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(move |_, _, _| Ok((user.clone(), "a-token".to_string())));

        let client = client_with_grpc(mock_grpc);

        let logged_in = client
            .login("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(logged_in.username, username);
        assert_eq!(client.tokens.get(), Some("a-token".to_string()));
    }

    #[tokio::test]
    async fn login_does_not_persist_a_master_key() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(move |_, _, _| Ok((proto_user(), "a-token".to_string())));

        let client = client_with_grpc(mock_grpc);

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
            .returning(move |_, _, _| Ok((proto_user(), "a-token".to_string())));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let client = client_with_grpc(mock_grpc);
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

        let client = client_with_grpc(mock_grpc);

        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_forgets_the_session_token_once_used() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(move |_, _, _| Ok((proto_user(), "a-token".to_string())));
        mock_grpc.expect_logout().times(1).returning(|| Ok(()));

        let client = client_with_grpc(mock_grpc);
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();
        client.logout().await.unwrap();
    }
}
