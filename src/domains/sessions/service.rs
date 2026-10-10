use std::sync::Arc;

use async_trait::async_trait;

use crate::domains::secrets::{SESSION_TOKEN_SECRET, Service as SecretsService};
use crate::{ErrorContext as _, ErrorKind, Result};

use super::Service;

/// The default [`Service`] implementation, reading the session token from
/// an injected [`SecretsService`].
pub(crate) struct SessionsClient {
    secrets: Arc<dyn SecretsService>,
}

impl SessionsClient {
    pub(crate) fn new(secrets: Arc<dyn SecretsService>) -> Self {
        Self { secrets }
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

    async fn save_new_session(&self, token: &str) -> Result<()> {
        self.secrets
            .set(SESSION_TOKEN_SECRET, token)
            .await
            .context("failed to persist session token")
    }

    async fn is_connected(&self) -> Result<bool> {
        Ok(self
            .secrets
            .get(SESSION_TOKEN_SECRET)
            .await
            .context("failed to read session token")?
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::secrets::MockService as MockSecretsService;

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
            .withf(|key, value| key == SESSION_TOKEN_SECRET && value == "a-token")
            .times(1)
            .returning(|_, _| Ok(()));
        let client = SessionsClient::new(Arc::new(secrets));

        client.save_new_session("a-token").await.unwrap();
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
}
