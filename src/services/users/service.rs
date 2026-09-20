use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{Error, ErrorContext as _, Result};

use super::Service;
use super::grpc_client::{FydeClient, GrpcClient, ProtoUser};

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

/// A client for the fyde server's users service. Tracks the session token
/// opened by the most recent `create`/`login` call, so [`Service::logout`]
/// doesn't need one passed in.
pub struct UsersClient {
    grpc: Box<dyn FydeClient>,
    session_token: Mutex<Option<String>>,
}

impl UsersClient {
    /// Creates a client for the users service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`).
    pub(super) async fn new(base_url: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(
                GrpcClient::new(base_url)
                    .await
                    .context("failed to create users grpc client")?,
            ),
            session_token: Mutex::new(None),
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of
    /// a live server.
    #[cfg(test)]
    fn with_grpc(grpc: impl FydeClient + 'static) -> Self {
        Self {
            grpc: Box::new(grpc),
            session_token: Mutex::new(None),
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

        *self.session_token.lock().unwrap() = Some(token);

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

        *self.session_token.lock().unwrap() = Some(token);

        Ok(user)
    }

    async fn logout(&self) -> Result<()> {
        let token = self.session_token.lock().unwrap().take();
        let Some(token) = token else {
            return Ok(());
        };

        self.grpc
            .logout(&token)
            .await
            .context("failed to log out")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::grpc_client::MockFydeClient;
    use super::*;

    fn proto_user() -> ProtoUser {
        ProtoUser {
            id: Uuid::new_v4().to_string(),
            username: "alice".to_string(),
            created_at: 1_700_000_000,
        }
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

        let client = UsersClient::with_grpc(mock_grpc);

        let created = client
            .create("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(created.username, username);
    }

    #[tokio::test]
    async fn login_returns_the_authenticated_user() {
        let user = proto_user();
        let username = user.username.clone();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(move |_, _, _| Ok((user.clone(), "a-token".to_string())));

        let client = UsersClient::with_grpc(mock_grpc);

        let logged_in = client
            .login("alice", "correct horse battery staple", "Pierre's iPhone")
            .await
            .unwrap();

        assert_eq!(logged_in.username, username);
    }

    #[tokio::test]
    async fn logout_closes_the_session_opened_by_login() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(move |_, _, _| Ok((proto_user(), "a-token".to_string())));
        mock_grpc
            .expect_logout()
            .withf(|token| token == "a-token")
            .returning(|_| Ok(()));

        let client = UsersClient::with_grpc(mock_grpc);
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_is_a_no_op_without_an_open_session() {
        let mock_grpc = MockFydeClient::new();

        let client = UsersClient::with_grpc(mock_grpc);

        client.logout().await.unwrap();
    }

    #[tokio::test]
    async fn logout_forgets_the_session_token_once_used() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_login()
            .returning(move |_, _, _| Ok((proto_user(), "a-token".to_string())));
        mock_grpc.expect_logout().times(1).returning(|_| Ok(()));

        let client = UsersClient::with_grpc(mock_grpc);
        client
            .login("alice", "correct horse battery staple", "device")
            .await
            .unwrap();

        client.logout().await.unwrap();
        client.logout().await.unwrap();
    }
}
