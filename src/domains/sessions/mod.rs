mod service;

use std::sync::{Arc, Weak};

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;
use crate::domains::secrets::{SecretLocation, Service as SecretsService};

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
    /// (e.g. the registration/login RPCs themselves) go out
    /// unauthenticated, since there is nothing to attach yet.
    async fn authenticated_request<T: Send + 'static>(
        &self,
        message: T,
    ) -> Result<tonic::Request<T>>;

    /// Persists `token` at `location` as the session token attached to
    /// subsequent outgoing calls by [`Service::authenticated_request`].
    /// Called by `users::Service::create`/`login` once the server has opened
    /// a new session, overwriting whatever token (if any) was persisted
    /// before.
    async fn save_new_session(&self, token: &str, location: SecretLocation) -> Result<()>;

    /// Returns whether a session token is currently persisted (i.e. a
    /// [`Service::save_new_session`] call happened since
    /// `users::Service::logout` last cleared it). Called by
    /// `users::Service::create`/`login`/`logout`.
    async fn is_connected(&self) -> Result<bool>;

    /// Checks the outcome of a call sent with
    /// [`Service::authenticated_request`], passing a successful `response`
    /// through and converting a failed one into an [`crate::Error`]. A
    /// gRPC `UNAUTHENTICATED` rejection while a session is persisted means
    /// the server no longer accepts this device's token (expired, revoked,
    /// ...): the session is ended locally through the handler registered
    /// with [`Service::on_session_expired`] — awaited, but run on its own
    /// task, so it completes even if the caller is cancelled meanwhile
    /// (e.g. the changelog consume job, which that handler stops) — and
    /// this fails with [`crate::ErrorCode::SessionExpired`] — as does every
    /// later rejection until [`Service::save_new_session`] opens a new
    /// session, so calls racing the first one report it too, while the
    /// session is only ended once. Otherwise, without a persisted session,
    /// the rejection stays a plain [`crate::ErrorCode::Unauthenticated`]:
    /// there is nothing to end.
    /// Not used by the calls that open or close a session themselves
    /// (registration, login, logout).
    async fn check_response<R: Send + 'static>(
        &self,
        response: std::result::Result<R, tonic::Status>,
    ) -> Result<R>;

    /// Registers what [`Service::check_response`] runs to end a session
    /// the server rejected. Held weakly, since the handler (`users`) itself
    /// holds this service; once it's dropped, a rejected session is only
    /// reported, not ended. Only the first registration is kept.
    fn on_session_expired(&self, handler: Weak<dyn ExpiredSessionHandler>);
}

/// Ends a session the server rejected, clearing it from this device. Lets
/// `sessions` trigger a logout without depending on `users` (which depends
/// on `sessions`); see [`Service::on_session_expired`].
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait ExpiredSessionHandler: Send + Sync {
    /// Clears every trace of the rejected session from this device, the way
    /// `users::Service::logout` does, minus telling the server.
    async fn end_expired_session(&self) -> Result<()>;
}

/// Initializes the sessions service: uses `secrets` to read the session
/// token attached to outgoing gRPC requests. Returns the concrete
/// [`SessionsClient`] rather than `Arc<dyn Service>` because
/// [`Service::authenticated_request`] is generic, which makes the trait
/// object-unsafe (`dyn Service` cannot exist).
pub(crate) fn init(secrets: Arc<dyn SecretsService>) -> Arc<SessionsClient> {
    Arc::new(SessionsClient::new(secrets))
}
