mod crypto;
mod grpc_client;
mod service;

pub use service::UsersClient;

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::Result;
use crate::services::settings::Service as SettingsService;
use crate::session::SessionTokenStore;

/// A user account, as returned by [`Service::create`]/[`Service::login`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub created_at: i64,
}

/// Manages account creation and session lifecycle against the fyde
/// server's users service. Trait methods take `&self` (not `&mut self`) so
/// implementations can be shared behind `Arc<dyn Service>`; the session
/// token opened by `create`/`login` is tracked in the [`SessionTokenStore`]
/// shared with every other service's gRPC transport (see
/// [`crate::session::AuthInterceptor`]), which attaches it to authenticate
/// subsequent calls, rather than it being threaded through every call
/// here — so `logout` takes no argument.
#[async_trait]
pub trait Service: Send + Sync {
    /// Creates a new account and opens a session for the device named
    /// `device_name`, returning the created user. Also generates a random
    /// master key, encrypts it under a key derived from `password`, and
    /// persists it in the settings store under `master_key`. Fails with
    /// [`crate::Error::Grpc`] (`ALREADY_EXISTS`) if the username is
    /// already taken.
    async fn create(&self, username: &str, password: &str, device_name: &str) -> Result<User>;

    /// Verifies `username`/`password` and opens a session for the device
    /// named `device_name`, returning the authenticated user. Fails with
    /// [`crate::Error::Grpc`] (`UNAUTHENTICATED`) if the credentials are
    /// invalid.
    async fn login(&self, username: &str, password: &str, device_name: &str) -> Result<User>;

    /// Closes the session opened by the most recent `create`/`login` call.
    /// A no-op if there is no open session.
    async fn logout(&self) -> Result<()>;
}

/// Initializes the users service: connects to the fyde server's users
/// service at `base_url`. `tokens` is written to on a successful
/// `create`/`login` and shared with every other service's gRPC transport
/// so they can authenticate their own calls. `settings` is where `create`
/// persists the master key it generates for a new account.
pub(crate) async fn init(
    base_url: impl AsRef<str>,
    tokens: SessionTokenStore,
    settings: Arc<dyn SettingsService>,
) -> Result<Arc<dyn Service>> {
    Ok(Arc::new(
        UsersClient::new(base_url, tokens, settings).await?,
    ))
}
