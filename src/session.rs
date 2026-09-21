//! Session-token attachment, shared by every service's gRPC transport so
//! authenticated calls attach it automatically. The token itself has no
//! in-memory copy: it is the one persisted by
//! `users::Service::create`/`login` in the settings store, so a session
//! survives across process restarts without extra plumbing.

use std::sync::Arc;

use crate::services::settings::Service as SettingsService;
use crate::{Error, ErrorContext as _, Result};

/// The settings key under which the session token opened by the most
/// recent `users::Service::create`/`login` call is persisted.
pub(crate) const SESSION_TOKEN_SETTING: &str = "session_token";

/// Builds a tonic request for `message`, attaching the session token
/// currently persisted in `settings` (if any) as a `Bearer`
/// `authorization` header. Calls made before any session is opened (e.g.
/// `CreateUser`/`Login` themselves) go out unauthenticated, since there is
/// nothing to attach yet.
pub(crate) async fn authenticated_request<T>(
    settings: &Arc<dyn SettingsService>,
    message: T,
) -> Result<tonic::Request<T>> {
    let mut request = tonic::Request::new(message);

    if let Some(token) = settings
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::settings::MockService as MockSettingsService;

    #[tokio::test]
    async fn leaves_the_request_unauthenticated_without_a_stored_token() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(None));
        let settings: Arc<dyn SettingsService> = Arc::new(settings);

        let request = authenticated_request(&settings, ()).await.unwrap();

        assert!(request.metadata().get("authorization").is_none());
    }

    #[tokio::test]
    async fn attaches_the_stored_token_as_a_bearer_header() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == SESSION_TOKEN_SETTING)
            .returning(|_| Ok(Some("a-token".to_string())));
        let settings: Arc<dyn SettingsService> = Arc::new(settings);

        let request = authenticated_request(&settings, ()).await.unwrap();

        assert_eq!(
            request.metadata().get("authorization").unwrap(),
            "Bearer a-token"
        );
    }
}
