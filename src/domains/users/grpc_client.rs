use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, Result};

/// Generated protobuf/gRPC bindings for the `users` service, compiled from
/// `../api-protos/users.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("users");
}

use proto::{LogoutRequest, UserCredentials, users_client::UsersClient as GeneratedUsersClient};

/// A gRPC transport for talking to the fyde server's users service. Knows
/// nothing about users semantics beyond the raw proto types; just
/// sends/receives raw messages. Abstracted as a trait so callers can be
/// tested against [`MockFydeClient`] instead of a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Creates a new account and opens a session for `device_name`,
    /// returning its session token.
    async fn create_user(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
    ) -> Result<String>;

    /// Verifies `username`/`password` and opens a session for
    /// `device_name`, returning its session token.
    async fn login(&self, username: &str, password: &str, device_name: &str) -> Result<String>;

    /// Closes the session currently authenticating outgoing calls (see
    /// [`crate::domains::sessions::Service::authenticated_request`]).
    async fn logout(&self) -> Result<()>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::connect`]), so they all reuse the same underlying
/// connection instead of each opening one of their own. The channel is
/// typically opened lazily (via `Endpoint::connect_lazy`), so the SDK can be
/// used offline for anything that doesn't reach this client; a call that
/// does need the server surfaces a connection failure as
/// [`crate::Error::GrpcTransport`]. Cloning a [`Channel`] is cheap — it's
/// just a handle to the same underlying connection — so a fresh generated
/// client is created per call. Every call, including `logout` itself, is
/// authenticated by attaching the session token currently persisted in
/// settings (if any) as a bearer `authorization` header (see
/// [`crate::domains::sessions::Service::authenticated_request`]).
pub(super) struct GrpcClient {
    channel: Channel,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    /// Creates a client for the users service using the shared `channel`
    /// connection to the fyde server, authenticating every call via
    /// `sessions`.
    pub fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self { channel, sessions }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> GeneratedUsersClient<Channel> {
        GeneratedUsersClient::new(self.channel.clone())
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn create_user(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
    ) -> Result<String> {
        let request = self
            .sessions
            .authenticated_request(UserCredentials {
                username: username.to_string(),
                password: password.to_string(),
                device_name: device_name.to_string(),
            })
            .await?;

        let response = self
            .client()
            .create_user(request)
            .await
            .context("failed to create user")?
            .into_inner();

        Ok(response.session_token)
    }

    async fn login(&self, username: &str, password: &str, device_name: &str) -> Result<String> {
        let request = self
            .sessions
            .authenticated_request(UserCredentials {
                username: username.to_string(),
                password: password.to_string(),
                device_name: device_name.to_string(),
            })
            .await?;

        let response = self
            .client()
            .login(request)
            .await
            .context("failed to log in")?
            .into_inner();

        Ok(response.session_token)
    }

    async fn logout(&self) -> Result<()> {
        let request = self
            .sessions
            .authenticated_request(LogoutRequest {})
            .await?;

        self.client()
            .logout(request)
            .await
            .context("failed to log out")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;

    use super::*;
    use crate::Error;
    use crate::domains::settings::MockService as MockSettingsService;

    #[tokio::test]
    async fn new_does_not_connect_to_the_server() {
        // A syntactically valid but unreachable address, connected lazily:
        // if `client()` dialed eagerly at construction, this would fail
        // here rather than on first use below.
        let mut settings = MockSettingsService::new();
        settings.expect_get().returning(|_| Ok(None));
        let sessions = Arc::new(SessionsClient::new(Arc::new(settings)));
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel, sessions);

        let err = grpc.login("alice", "password", "device").await.unwrap_err();

        assert!(matches!(err, Error::Context { .. }));
    }
}
