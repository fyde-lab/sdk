use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::Request;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;

use crate::session::{AuthInterceptor, SessionTokenStore};
use crate::{Error, ErrorContext as _, Result};

/// Generated protobuf/gRPC bindings for the `users` service, compiled from
/// `../api-protos/users.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("users");
}

pub(super) use proto::User as ProtoUser;

use proto::{LogoutRequest, UserCredentials, users_client::UsersClient as GeneratedUsersClient};

/// A gRPC transport for talking to the fyde server's users service. Knows
/// nothing about users semantics beyond the raw proto types; just
/// sends/receives raw messages. Abstracted as a trait so callers can be
/// tested against [`MockFydeClient`] instead of a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Creates a new account and opens a session for `device_name`,
    /// returning the created user and its session token.
    async fn create_user(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
    ) -> Result<(ProtoUser, String)>;

    /// Verifies `username`/`password` and opens a session for
    /// `device_name`, returning the authenticated user and its session
    /// token.
    async fn login(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
    ) -> Result<(ProtoUser, String)>;

    /// Closes the session currently authenticating outgoing calls (see
    /// [`AuthInterceptor`]).
    async fn logout(&self) -> Result<()>;
}

/// The production [`FydeClient`] implementation, backed by a real tonic
/// connection. Every call, including `logout` itself, is authenticated:
/// the underlying client is wrapped with an [`AuthInterceptor`] that
/// attaches the current session token (if any) as a bearer `authorization`
/// header.
pub(super) struct GrpcClient {
    client: GeneratedUsersClient<InterceptedService<Channel, AuthInterceptor>>,
}

impl GrpcClient {
    /// Connects to the users service at the given `http://` or `https://`
    /// base URL (e.g. `http://127.0.0.1:8080`), authenticating every call
    /// with the session token tracked by `tokens`.
    pub async fn new(base_url: impl AsRef<str>, tokens: SessionTokenStore) -> Result<Self> {
        let endpoint = Channel::from_shared(base_url.as_ref().to_string())
            .map_err(|err| Error::InvalidEndpoint(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .context("failed to connect to users grpc endpoint")?;

        Ok(Self {
            client: GeneratedUsersClient::with_interceptor(channel, AuthInterceptor::new(tokens)),
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
    ) -> Result<(ProtoUser, String)> {
        // The generated client's RPC methods take `&mut self`, but the
        // underlying `Channel` is cheap to clone and safe to use
        // concurrently, so we clone it per call to expose `&self` here.
        let response = self
            .client
            .clone()
            .create_user(UserCredentials {
                username: username.to_string(),
                password: password.to_string(),
                device_name: device_name.to_string(),
            })
            .await
            .context("failed to create user")?
            .into_inner();

        let user = response
            .user
            .ok_or_else(|| Error::InvalidResponse("create_user response missing user".into()))?;

        Ok((user, response.session_token))
    }

    async fn login(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
    ) -> Result<(ProtoUser, String)> {
        let response = self
            .client
            .clone()
            .login(UserCredentials {
                username: username.to_string(),
                password: password.to_string(),
                device_name: device_name.to_string(),
            })
            .await
            .context("failed to log in")?
            .into_inner();

        let user = response
            .user
            .ok_or_else(|| Error::InvalidResponse("login response missing user".into()))?;

        Ok((user, response.session_token))
    }

    async fn logout(&self) -> Result<()> {
        self.client
            .clone()
            .logout(Request::new(LogoutRequest {}))
            .await
            .context("failed to log out")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_rejects_a_malformed_base_url() {
        let result = GrpcClient::new("not a valid uri", SessionTokenStore::default()).await;

        let err = match result {
            Ok(_) => panic!("a malformed base url must be rejected"),
            Err(err) => err,
        };
        assert!(matches!(err, Error::InvalidEndpoint(_)));
    }
}
