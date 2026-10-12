use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;

use crate::domains::secrets::{SESSION_TOKEN_SECRET, SecretLocation, Service as SecretsService};
use crate::{ErrorContext as _, ErrorKind, Result};

use super::{ExpiredSessionHandler, Service};

/// The default [`Service`] implementation, reading the session token from
/// an injected [`SecretsService`].
pub(crate) struct SessionsClient {
    secrets: Arc<dyn SecretsService>,
    /// Set once by [`Service::on_session_expired`], after construction:
    /// the handler (`users`) is built from this client.
    expired_session_handler: OnceLock<Weak<dyn ExpiredSessionHandler>>,
    /// Whether the last session was ended by [`Service::check_response`]
    /// rather than closed by the user, until [`Service::save_new_session`]
    /// opens a new one — so every call the server rejects meanwhile reports
    /// [`crate::ErrorCode::SessionExpired`], including those that raced the
    /// first rejection or were sent once its token was already gone. Held
    /// across the whole check-and-end, so concurrent rejections end the
    /// session only once.
    expired: tokio::sync::Mutex<bool>,
}

impl SessionsClient {
    pub(crate) fn new(secrets: Arc<dyn SecretsService>) -> Self {
        Self {
            secrets,
            expired_session_handler: OnceLock::new(),
            expired: tokio::sync::Mutex::new(false),
        }
    }

    /// Runs the registered [`ExpiredSessionHandler`], if it's still alive,
    /// on its own task (see [`Service::check_response`] for why). A failure
    /// is only logged: the caller reports the expired session either way.
    async fn end_expired_session(&self) {
        let Some(handler) = self.expired_session_handler.get().and_then(Weak::upgrade) else {
            tracing::warn!("no handler registered to end the rejected session");
            return;
        };

        match tokio::spawn(async move { handler.end_expired_session().await }).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::error!(error = %err, "failed to end the rejected session"),
            Err(err) => tracing::error!(error = %err, "task ending the rejected session failed"),
        }
    }
}

#[async_trait]
impl Service for SessionsClient {
    async fn authenticated_request<T: Send + 'static>(
        &self,
        message: T,
    ) -> Result<tonic::Request<T>> {
        let mut request = tonic::Request::new(message);

        if let Some(token) = self
            .secrets
            .get(SESSION_TOKEN_SECRET)
            .await
            .context("failed to read session token")?
        {
            let value = format!("Bearer {token}").parse().map_err(|_| {
                ErrorKind::InvalidResponse("session token is not a valid header value".to_string())
            })?;
            request.metadata_mut().insert("authorization", value);
        }

        Ok(request)
    }

    async fn save_new_session(&self, token: &str, location: SecretLocation) -> Result<()> {
        self.secrets
            .set(SESSION_TOKEN_SECRET, token, location)
            .await
            .context("failed to persist session token")?;
        *self.expired.lock().await = false;
        Ok(())
    }

    async fn is_connected(&self) -> Result<bool> {
        Ok(self
            .secrets
            .get(SESSION_TOKEN_SECRET)
            .await
            .context("failed to read session token")?
            .is_some())
    }

    async fn check_response<R: Send + 'static>(
        &self,
        response: std::result::Result<R, tonic::Status>,
    ) -> Result<R> {
        let status = match response {
            Ok(response) => return Ok(response),
            Err(status) if status.code() != tonic::Code::Unauthenticated => {
                return Err(status.into());
            }
            Err(status) => status,
        };

        let mut expired = self.expired.lock().await;
        if !*expired {
            match self.is_connected().await {
                Ok(true) => {}
                Ok(false) => return Err(status.into()),
                Err(err) => {
                    tracing::warn!(error = %err, "failed to check for an open session after a rejected call");
                    return Err(status.into());
                }
            }

            tracing::warn!(status = %status, "server rejected the session token, logging out");
            // Set first: the handler still runs to completion on its own
            // task if this call is cancelled while awaiting it.
            *expired = true;
            self.end_expired_session().await;
        }

        Err(ErrorKind::SessionExpired(status).into())
    }

    fn on_session_expired(&self, handler: Weak<dyn ExpiredSessionHandler>) {
        if self.expired_session_handler.set(handler).is_err() {
            tracing::warn!("an expired session handler is already registered, ignoring another");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use crate::domains::secrets::MockService as MockSecretsService;
    use crate::domains::sessions::MockExpiredSessionHandler;

    fn rejected() -> std::result::Result<(), tonic::Status> {
        Err(tonic::Status::unauthenticated(
            "invalid authorization token",
        ))
    }

    /// Registers `handler` on `client`, returning the `Arc` that keeps it
    /// alive, since `client` only holds it weakly.
    fn register(
        client: &SessionsClient,
        handler: MockExpiredSessionHandler,
    ) -> Arc<dyn ExpiredSessionHandler> {
        let handler: Arc<dyn ExpiredSessionHandler> = Arc::new(handler);
        client.on_session_expired(Arc::downgrade(&handler));
        handler
    }

    #[tokio::test]
    async fn leaves_the_request_unauthenticated_without_a_stored_token() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SECRET)
            .returning(|_| Ok(None));
        let client = SessionsClient::new(Arc::new(secrets));

        let request = client.authenticated_request(()).await.unwrap();

        assert!(request.metadata().get("authorization").is_none());
    }

    #[tokio::test]
    async fn attaches_the_stored_token_as_a_bearer_header() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SECRET)
            .returning(|_| Ok(Some("a-token".to_string())));
        let client = SessionsClient::new(Arc::new(secrets));

        let request = client.authenticated_request(()).await.unwrap();

        assert_eq!(
            request.metadata().get("authorization").unwrap(),
            "Bearer a-token"
        );
    }

    #[tokio::test]
    async fn save_new_session_persists_the_token_under_the_session_key() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_set()
            .withf(|key, value, location| {
                key == SESSION_TOKEN_SECRET
                    && value == "a-token"
                    && *location == SecretLocation::Database
            })
            .times(1)
            .returning(|_, _, _| Ok(()));
        let client = SessionsClient::new(Arc::new(secrets));

        client
            .save_new_session("a-token", SecretLocation::Database)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn is_connected_returns_true_with_a_stored_token() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SECRET)
            .returning(|_| Ok(Some("a-token".to_string())));
        let client = SessionsClient::new(Arc::new(secrets));

        assert!(client.is_connected().await.unwrap());
    }

    #[tokio::test]
    async fn is_connected_returns_false_without_a_stored_token() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SECRET)
            .returning(|_| Ok(None));
        let client = SessionsClient::new(Arc::new(secrets));

        assert!(!client.is_connected().await.unwrap());
    }

    #[tokio::test]
    async fn check_response_passes_a_successful_response_through() {
        let client = SessionsClient::new(Arc::new(MockSecretsService::new()));

        assert_eq!(client.check_response(Ok(42)).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn check_response_keeps_other_rejections_as_they_are() {
        // No expectation on `secrets` or the handler: anything but
        // `UNAUTHENTICATED` must not even look at the session.
        let client = SessionsClient::new(Arc::new(MockSecretsService::new()));
        let _handler = register(&client, MockExpiredSessionHandler::new());

        let err = client
            .check_response::<()>(Err(tonic::Status::permission_denied("nope")))
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn check_response_keeps_a_rejection_without_an_open_session_unauthenticated() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SECRET)
            .returning(|_| Ok(None));
        let client = SessionsClient::new(Arc::new(secrets));
        let _handler = register(&client, MockExpiredSessionHandler::new());

        let err = client.check_response(rejected()).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::Unauthenticated);
    }

    #[tokio::test]
    async fn check_response_ends_a_rejected_session_and_reports_it_expired() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SECRET)
            .returning(|_| Ok(Some("a-token".to_string())));
        let client = SessionsClient::new(Arc::new(secrets));
        let mut handler = MockExpiredSessionHandler::new();
        handler
            .expect_end_expired_session()
            .times(1)
            .returning(|| Ok(()));
        let _handler = register(&client, handler);

        let err = client.check_response(rejected()).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::SessionExpired);
    }

    #[tokio::test]
    async fn check_response_reports_the_session_expired_even_if_ending_it_fails() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .returning(|_| Ok(Some("a-token".to_string())));
        let client = SessionsClient::new(Arc::new(secrets));
        let mut handler = MockExpiredSessionHandler::new();
        handler
            .expect_end_expired_session()
            .times(1)
            .returning(|| Err(ErrorKind::Io(std::io::Error::other("wipe failed")).into()));
        let _handler = register(&client, handler);

        let err = client.check_response(rejected()).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::SessionExpired);
    }

    #[tokio::test]
    async fn check_response_reports_the_session_expired_without_a_live_handler() {
        let mut secrets = MockSecretsService::new();
        secrets
            .expect_get()
            .returning(|_| Ok(Some("a-token".to_string())));
        let client = SessionsClient::new(Arc::new(secrets));
        drop(register(&client, MockExpiredSessionHandler::new()));

        let err = client.check_response(rejected()).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::SessionExpired);
    }

    #[tokio::test]
    async fn check_response_ends_the_session_once_and_reports_it_expired_until_a_new_one_opens() {
        // The token is there for the first rejection only: ending the
        // session clears it, like the real handler does.
        let token = Arc::new(std::sync::Mutex::new(Some("a-token".to_string())));
        let mut secrets = MockSecretsService::new();
        let stored = token.clone();
        secrets
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SECRET)
            .returning(move |_| Ok(stored.lock().unwrap().clone()));
        secrets.expect_set().times(1).returning(|_, _, _| Ok(()));
        let client = SessionsClient::new(Arc::new(secrets));
        let mut handler = MockExpiredSessionHandler::new();
        let cleared = token.clone();
        handler
            .expect_end_expired_session()
            .times(1)
            .returning(move || {
                *cleared.lock().unwrap() = None;
                Ok(())
            });
        let _handler = register(&client, handler);

        let first = client.check_response(rejected()).await.unwrap_err();
        let second = client.check_response(rejected()).await.unwrap_err();
        client
            .save_new_session("another-token", SecretLocation::SecureStorage)
            .await
            .unwrap();
        let after_new_session = client.check_response(rejected()).await.unwrap_err();

        assert_eq!(first.code(), ErrorCode::SessionExpired);
        assert_eq!(second.code(), ErrorCode::SessionExpired);
        // The mock's `set` doesn't store the new token, so this rejection
        // finds no session: what matters is it's no longer reported expired.
        assert_eq!(after_new_session.code(), ErrorCode::Unauthenticated);
    }
}
