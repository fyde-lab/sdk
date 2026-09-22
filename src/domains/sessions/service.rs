use std::sync::Arc;

use async_trait::async_trait;

use crate::domains::settings::Service as SettingsService;
use crate::{Error, ErrorContext as _, Result};

use super::{SESSION_TOKEN_SETTING, Service};

/// The default [`Service`] implementation, reading the session token from
/// an injected [`SettingsService`].
pub(crate) struct SessionsClient {
    settings: Arc<dyn SettingsService>,
}

impl SessionsClient {
    pub(crate) fn new(settings: Arc<dyn SettingsService>) -> Self {
        Self { settings }
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
            .settings
            .get(SESSION_TOKEN_SETTING)
            .await
            .context("failed to read session token")?
        {
            let value = format!("Bearer {token}").parse().map_err(|_| {
                Error::InvalidResponse("session token is not a valid header value".to_string())
            })?;
            request.metadata_mut().insert("authorization", value);
        }

        Ok(request)
    }

    async fn save_new_session(&self, token: &str) -> Result<()> {
        self.settings
            .set(SESSION_TOKEN_SETTING, token)
            .await
            .context("failed to persist session token")
    }

    async fn remove_session(&self) -> Result<()> {
        self.settings
            .delete(SESSION_TOKEN_SETTING)
            .await
            .context("failed to remove persisted session token")
    }

    async fn is_connected(&self) -> Result<bool> {
        Ok(self
            .settings
            .get(SESSION_TOKEN_SETTING)
            .await
            .context("failed to read session token")?
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::settings::MockService as MockSettingsService;

    #[tokio::test]
    async fn leaves_the_request_unauthenticated_without_a_stored_token() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(None));
        let client = SessionsClient::new(Arc::new(settings));

        let request = client.authenticated_request(()).await.unwrap();

        assert!(request.metadata().get("authorization").is_none());
    }

    #[tokio::test]
    async fn attaches_the_stored_token_as_a_bearer_header() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(Some("a-token".to_string())));
        let client = SessionsClient::new(Arc::new(settings));

        let request = client.authenticated_request(()).await.unwrap();

        assert_eq!(
            request.metadata().get("authorization").unwrap(),
            "Bearer a-token"
        );
    }

    #[tokio::test]
    async fn save_new_session_persists_the_token_under_the_session_key() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_set()
            .withf(|key, value| key == SESSION_TOKEN_SETTING && value == "a-token")
            .times(1)
            .returning(|_, _| Ok(()));
        let client = SessionsClient::new(Arc::new(settings));

        client.save_new_session("a-token").await.unwrap();
    }

    #[tokio::test]
    async fn remove_session_deletes_the_session_key() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_delete()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .times(1)
            .returning(|_| Ok(()));
        let client = SessionsClient::new(Arc::new(settings));

        client.remove_session().await.unwrap();
    }

    #[tokio::test]
    async fn is_connected_returns_true_with_a_stored_token() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(Some("a-token".to_string())));
        let client = SessionsClient::new(Arc::new(settings));

        assert!(client.is_connected().await.unwrap());
    }

    #[tokio::test]
    async fn is_connected_returns_false_without_a_stored_token() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(None));
        let client = SessionsClient::new(Arc::new(settings));

        assert!(!client.is_connected().await.unwrap());
    }
}
