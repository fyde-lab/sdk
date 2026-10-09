mod service;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;
use crate::domains::secrets::Service as SecretsService;

pub(crate) use service::SessionsClient;

/// Session-token attachment, shared by every service's gRPC transport so
/// authenticated calls attach it automatically. The token itself has no
/// in-memory copy: it is the one persisted by
/// `users::Service::create`/`login` in the secrets store (under
/// [`crate::domains::secrets::SESSION_TOKEN_SECRET`]), so a session survives
/// across process restarts without extra plumbing.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Builds a tonic request for `message`, attaching the session token
    /// currently persisted in the secrets store (if any) as a `Bearer`
    /// `authorization` header. Calls made before any session is opened
    /// (e.g. `CreateUser`/`Login` themselves) go out unauthenticated, since
    /// there is nothing to attach yet.
    async fn authenticated_request<T: Send + 'static>(
        &self,
        message: T,
    ) -> Result<tonic::Request<T>>;

    /// Persists `token` as the session token attached to subsequent
    /// outgoing calls by [`Service::authenticated_request`]. Called by
    /// `users::Service::create`/`login` once the server has opened a new
    /// session, overwriting whatever token (if any) was persisted before.
    async fn save_new_session(&self, token: &str) -> Result<()>;

    /// Returns whether a session token is currently persisted (i.e. a
    /// [`Service::save_new_session`] call happened since
    /// `users::Service::logout` last cleared it). Called by
    /// `users::Service::create`/`login`/`logout`.
    async fn is_connected(&self) -> Result<bool>;
}

/// Initializes the sessions service: uses `secrets` to read the session
/// token attached to outgoing gRPC requests. Returns the concrete
/// [`SessionsClient`] rather than `Arc<dyn Service>` because
/// [`Service::authenticated_request`] is generic, which makes the trait
/// object-unsafe (`dyn Service` cannot exist).
pub(crate) fn init(secrets: Arc<dyn SecretsService>) -> Arc<SessionsClient> {
    Arc::new(SessionsClient::new(secrets))
}
