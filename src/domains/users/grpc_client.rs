use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{Error, ErrorContext as _, Result};

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

/// The production [`FydeClient`] implementation, backed by a real tonic
/// connection. Every call, including `logout` itself, is authenticated by
/// attaching the session token currently persisted in settings (if any) as
/// a bearer `authorization` header (see
/// [`crate::domains::sessions::Service::authenticated_request`]).
pub(super) struct GrpcClient {
    client: GeneratedUsersClient<Channel>,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    /// Connects to the users service at the given `http://` or `https://`
    /// base URL (e.g. `http://127.0.0.1:8080`), authenticating every call
    /// via `sessions`.
    pub async fn new(base_url: impl AsRef<str>, sessions: Arc<SessionsClient>) -> Result<Self> {
        let endpoint = Channel::from_shared(base_url.as_ref().to_string())
            .map_err(|err| Error::InvalidEndpoint(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .context("failed to connect to users grpc endpoint")?;

        Ok(Self {
            client: GeneratedUsersClient::new(channel),
            sessions,
        })
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

        // The generated client's RPC methods take `&mut self`, but the
        // underlying `Channel` is cheap to clone and safe to use
        // concurrently, so we clone it per call to expose `&self` here.
        let response = self
            .client
            .clone()
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
            .client
            .clone()
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

        self.client
            .clone()
            .logout(request)
            .await
            .context("failed to log out")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::settings::MockService as MockSettingsService;

    #[tokio::test]
    async fn new_rejects_a_malformed_base_url() {
        let sessions = Arc::new(SessionsClient::new(Arc::new(MockSettingsService::new())));
        let result = GrpcClient::new("not a valid uri", sessions).await;

        let err = match result {
            Ok(_) => panic!("a malformed base url must be rejected"),
            Err(err) => err,
        };
        assert!(matches!(err, Error::InvalidEndpoint(_)));
    }
}
