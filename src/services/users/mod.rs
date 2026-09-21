mod crypto;
mod grpc_client;
mod service;

pub use service::UsersClient;

use std::sync::Arc;

use async_trait::async_trait;

use crate::Result;
use crate::services::sessions::SessionsClient;
use crate::services::settings::Service as SettingsService;

/// Manages account creation and session lifecycle against the fyde
/// server's users service. Trait methods take `&self` (not `&mut self`) so
/// implementations can be shared behind `Arc<dyn Service>`; the session
/// token opened by `create`/`login` is persisted in the settings store
/// under `session_token` (see
/// [`crate::services::sessions::SESSION_TOKEN_SETTING`]), shared with every
/// other service's gRPC transport, which reads it from there to
/// authenticate outgoing calls (see
/// [`crate::services::sessions::Service::authenticated_request`]) rather than it being
/// threaded through every call here — so `logout` takes no argument.
/// Persisting it in settings, rather than only holding it in memory, means
/// a session survives across process restarts; `logout` removes it again.
#[async_trait]
pub trait Service: Send + Sync {
    /// Creates a new account and opens a session for the device named
    /// `device_name`, returning its session token. Also generates a random
    /// master key, encrypts it under a key derived from `password`, and
    /// persists it in the settings store under `master_key`. The server's
    /// response carries only the new session's token (see
    /// `../api-protos/users.proto`), so there is no user payload to return
    /// here. Fails with [`crate::Error::Grpc`] (`ALREADY_EXISTS`) if the
    /// username is already taken.
    async fn create(&self, username: &str, password: &str, device_name: &str) -> Result<String>;

    /// Verifies `username`/`password` and opens a session for the device
    /// named `device_name`, returning its session token. Fails with
    /// [`crate::Error::Grpc`] (`UNAUTHENTICATED`) if the credentials are
    /// invalid.
    async fn login(&self, username: &str, password: &str, device_name: &str) -> Result<String>;

    /// Closes the session opened by the most recent `create`/`login` call.
    /// A no-op if there is no open session.
    async fn logout(&self) -> Result<()>;

    /// Returns whether a session token is currently persisted in the
    /// settings store (i.e. a `create`/`login` call succeeded and
    /// `logout` hasn't been called since).
    async fn is_connected(&self) -> Result<bool>;
}

/// Initializes the users service: connects to the fyde server's users
/// service at `base_url`. `settings` is where `create`/`login` persist the
/// session token (read back by every other service's gRPC transport, via
/// `sessions`, to authenticate their own calls) and where `create` also
/// persists the master key it generates for a new account.
pub(crate) async fn init(
    base_url: impl AsRef<str>,
    settings: Arc<dyn SettingsService>,
    sessions: Arc<SessionsClient>,
) -> Result<Arc<dyn Service>> {
    Ok(Arc::new(
        UsersClient::new(base_url, settings, sessions).await?,
    ))
}
