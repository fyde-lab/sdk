use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, Result};

use super::Role;

/// Generated protobuf/gRPC bindings for the `users` service, compiled by
/// `build.rs` from `users/v1/users.proto` in the `buf.build/fyde-lab/api`
/// BSR module (sourced from `../api-protos`).
mod proto {
    tonic::include_proto!("users.v1");
}

use proto::{
    FinishChangePasswordRequest, FinishLoginRequest, FinishRegistrationRequest, LogoutRequest,
    StartChangePasswordRequest, StartLoginRequest, StartRegistrationRequest,
    users_service_client::UsersServiceClient as GeneratedUsersClient,
};

/// Converts the raw `role` field of a `FinishRegistrationResponse`/
/// `FinishLoginResponse` into [`Role`]. Defaults to [`Role::User`] for
/// `ROLE_UNSPECIFIED` or any value this client doesn't recognize (e.g. an
/// older client talking to a server that added a new role), rather than
/// failing the call over a field that's informational only.
fn to_role(value: i32) -> Role {
    match proto::Role::try_from(value) {
        Ok(proto::Role::Admin) => Role::Admin,
        Ok(proto::Role::User) | Ok(proto::Role::Unspecified) | Err(_) => Role::User,
    }
}

/// A gRPC transport for talking to the fyde server's users service. Knows
/// nothing about OPAQUE or users semantics beyond the raw proto types —
/// every method here just carries opaque protocol bytes back and forth;
/// [`super::crypto`] is what actually drives the OPAQUE exchange. Abstracted
/// as a trait so callers can be tested against [`MockFydeClient`] instead of
/// a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// First step of registration: forwards a serialized OPAQUE
    /// `RegistrationRequest` for `username` and returns the server's
    /// serialized `RegistrationResponse`.
    async fn start_registration(&self, username: &str, opaque_request: &[u8]) -> Result<Vec<u8>>;

    /// Second and final step of registration: sends the serialized OPAQUE
    /// `RegistrationUpload` for `username`, the master key encrypted
    /// client-side under a key derived from the OPAQUE export key, and the
    /// account's preferred interface language, and opens a session for
    /// `device_name`, returning its session token alongside the newly
    /// created account's role.
    async fn finish_registration(
        &self,
        username: &str,
        opaque_upload: &[u8],
        device_name: &str,
        encrypted_master_key: &[u8],
        language: &str,
    ) -> Result<(String, Role)>;

    /// First step of logging in: forwards a serialized OPAQUE
    /// `CredentialRequest` for `username` and returns the server's
    /// serialized `CredentialResponse` alongside the `login_id`
    /// correlating this call with the matching `finish_login` call.
    async fn start_login(&self, username: &str, opaque_request: &[u8])
    -> Result<(String, Vec<u8>)>;

    /// Second and final step of logging in: sends the serialized OPAQUE
    /// `CredentialFinalization` for the exchange identified by `login_id`
    /// and opens a session for `device_name`, returning its session token
    /// alongside the account's master key, still encrypted client-side
    /// under a key derived from the OPAQUE export key (see
    /// [`super::crypto::unwrap_master_key`]), and the account's role.
    async fn finish_login(
        &self,
        login_id: &str,
        opaque_upload: &[u8],
        device_name: &str,
    ) -> Result<(String, Vec<u8>, Role)>;

    /// Closes the session currently authenticating outgoing calls (see
    /// [`crate::domains::sessions::Service::authenticated_request`]).
    async fn logout(&self) -> Result<()>;

    /// First step of changing the password of the account behind the
    /// currently authenticated session: forwards a serialized OPAQUE
    /// `RegistrationRequest` derived from the new password and returns the
    /// server's serialized `RegistrationResponse`. The account to change is
    /// derived server-side from the session, not a username field.
    async fn start_change_password(&self, opaque_request: &[u8]) -> Result<Vec<u8>>;

    /// Second and final step of changing password: sends the serialized
    /// OPAQUE `RegistrationUpload` derived from the new password, plus the
    /// master key re-encrypted client-side under a key derived from the new
    /// password's export key, replacing the account's previous
    /// registration record and encrypted master key.
    async fn finish_change_password(
        &self,
        opaque_upload: &[u8],
        encrypted_master_key: &[u8],
    ) -> Result<()>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::init`]), so they all reuse the same underlying
/// connection instead of each opening one of their own. The channel is
/// typically opened lazily (via `Endpoint::connect_lazy`), so the SDK can be
/// used offline for anything that doesn't reach this client; a call that
/// does need the server surfaces a connection failure as
/// [`crate::Error::GrpcTransport`]. Cloning a [`Channel`] is cheap — it's
/// just a handle to the same underlying connection — so a fresh generated
/// client is created per call. Every call, including `logout` itself, is
/// authenticated by attaching the session token currently persisted in
/// the secrets store (if any) as a bearer `authorization` header (see
/// [`crate::domains::sessions::Service::authenticated_request`]) — harmless
/// for the four registration/login steps, which the server doesn't require
/// one for.
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
    async fn start_registration(&self, username: &str, opaque_request: &[u8]) -> Result<Vec<u8>> {
        let request = self
            .sessions
            .authenticated_request(StartRegistrationRequest {
                username: username.to_string(),
                opaque_request: opaque_request.to_vec(),
            })
            .await?;

        let response = self
            .client()
            .start_registration(request)
            .await
            .context("failed to start registration")?
            .into_inner();

        Ok(response.opaque_response)
    }

    async fn finish_registration(
        &self,
        username: &str,
        opaque_upload: &[u8],
        device_name: &str,
        encrypted_master_key: &[u8],
        language: &str,
    ) -> Result<(String, Role)> {
        let request = self
            .sessions
            .authenticated_request(FinishRegistrationRequest {
                username: username.to_string(),
                opaque_upload: opaque_upload.to_vec(),
                device_name: device_name.to_string(),
                encrypted_master_key: encrypted_master_key.to_vec(),
                language: language.to_string(),
            })
            .await?;

        let response = self
            .client()
            .finish_registration(request)
            .await
            .context("failed to finish registration")?
            .into_inner();

        Ok((response.session_token, to_role(response.role)))
    }

    async fn start_login(
        &self,
        username: &str,
        opaque_request: &[u8],
    ) -> Result<(String, Vec<u8>)> {
        let request = self
            .sessions
            .authenticated_request(StartLoginRequest {
                username: username.to_string(),
                opaque_request: opaque_request.to_vec(),
            })
            .await?;

        let response = self
            .client()
            .start_login(request)
            .await
            .context("failed to start login")?
            .into_inner();

        Ok((response.login_id, response.opaque_response))
    }

    async fn finish_login(
        &self,
        login_id: &str,
        opaque_upload: &[u8],
        device_name: &str,
    ) -> Result<(String, Vec<u8>, Role)> {
        let request = self
            .sessions
            .authenticated_request(FinishLoginRequest {
                login_id: login_id.to_string(),
                opaque_upload: opaque_upload.to_vec(),
                device_name: device_name.to_string(),
            })
            .await?;

        let response = self
            .client()
            .finish_login(request)
            .await
            .context("failed to finish login")?
            .into_inner();

        Ok((
            response.session_token,
            response.encrypted_master_key,
            to_role(response.role),
        ))
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

    async fn start_change_password(&self, opaque_request: &[u8]) -> Result<Vec<u8>> {
        let request = self
            .sessions
            .authenticated_request(StartChangePasswordRequest {
                opaque_request: opaque_request.to_vec(),
            })
            .await?;

        let response = self
            .client()
            .start_change_password(request)
            .await
            .context("failed to start change password")?
            .into_inner();

        Ok(response.opaque_response)
    }

    async fn finish_change_password(
        &self,
        opaque_upload: &[u8],
        encrypted_master_key: &[u8],
    ) -> Result<()> {
        let request = self
            .sessions
            .authenticated_request(FinishChangePasswordRequest {
                opaque_upload: opaque_upload.to_vec(),
                encrypted_master_key: encrypted_master_key.to_vec(),
            })
            .await?;

        self.client()
            .finish_change_password(request)
            .await
            .context("failed to finish change password")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;

    use super::*;
    use crate::Error;
    use crate::domains::secrets::MockService as MockSecretsService;

    #[tokio::test]
    async fn new_does_not_connect_to_the_server() {
        // A syntactically valid but unreachable address, connected lazily:
        // if `client()` dialed eagerly at construction, this would fail
        // here rather than on first use below.
        let mut secrets = MockSecretsService::new();
        secrets.expect_get().returning(|_| Ok(None));
        let sessions = Arc::new(SessionsClient::new(Arc::new(secrets)));
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel, sessions);

        let err = grpc.start_login("alice", b"the-request").await.unwrap_err();

        assert!(matches!(err, Error::Context { .. }));
    }
}
