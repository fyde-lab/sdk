//! Shared session-token state, threaded into every service's gRPC
//! transport so authenticated calls attach it automatically instead of
//! each transport managing its own copy.

use std::sync::{Arc, Mutex};

use tonic::Status;
use tonic::service::Interceptor;

/// Thread-safe storage for the token of the session opened by the most
/// recent `UsersService::create`/`login` call. Cheap to clone: every clone
/// shares the same underlying token, so the same store can be handed to
/// the users service (which writes it) and every other service's gRPC
/// transport (which reads it to authenticate outgoing calls).
#[derive(Clone, Default)]
pub(crate) struct SessionTokenStore(Arc<Mutex<Option<String>>>);

impl SessionTokenStore {
    pub(crate) fn get(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }

    pub(crate) fn set(&self, token: String) {
        *self.0.lock().unwrap() = Some(token);
    }

    /// Clears the stored token, returning the previous value (if any).
    pub(crate) fn take(&self) -> Option<String> {
        self.0.lock().unwrap().take()
    }
}

/// A tonic interceptor that attaches the token tracked by a
/// [`SessionTokenStore`] as a `Bearer` `authorization` header on every
/// outgoing gRPC call, when one is available. Calls made before any
/// session is opened (e.g. `CreateUser`/`Login` themselves) go out
/// unauthenticated, since there is nothing to attach yet.
#[derive(Clone)]
pub(crate) struct AuthInterceptor {
    tokens: SessionTokenStore,
}

impl AuthInterceptor {
    pub(crate) fn new(tokens: SessionTokenStore) -> Self {
        Self { tokens }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        if let Some(token) = self.tokens.get() {
            let value = format!("Bearer {token}")
                .parse()
                .map_err(|_| Status::internal("session token is not a valid header value"))?;
            request.metadata_mut().insert("authorization", value);
        }

        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_none_before_any_token_is_set() {
        let store = SessionTokenStore::default();

        assert_eq!(store.get(), None);
    }

    #[test]
    fn set_then_get_returns_the_stored_token() {
        let store = SessionTokenStore::default();

        store.set("a-token".to_string());

        assert_eq!(store.get(), Some("a-token".to_string()));
    }

    #[test]
    fn take_clears_the_token_and_returns_the_previous_value() {
        let store = SessionTokenStore::default();
        store.set("a-token".to_string());

        assert_eq!(store.take(), Some("a-token".to_string()));
        assert_eq!(store.get(), None);
    }

    #[test]
    fn clones_share_the_same_underlying_token() {
        let store = SessionTokenStore::default();
        let clone = store.clone();

        store.set("a-token".to_string());

        assert_eq!(clone.get(), Some("a-token".to_string()));
    }

    #[test]
    fn interceptor_leaves_the_request_untouched_without_a_stored_token() {
        let mut interceptor = AuthInterceptor::new(SessionTokenStore::default());

        let request = interceptor.call(tonic::Request::new(())).unwrap();

        assert!(request.metadata().get("authorization").is_none());
    }

    #[test]
    fn interceptor_attaches_the_stored_token_as_a_bearer_header() {
        let tokens = SessionTokenStore::default();
        tokens.set("a-token".to_string());
        let mut interceptor = AuthInterceptor::new(tokens);

        let request = interceptor.call(tonic::Request::new(())).unwrap();

        assert_eq!(
            request.metadata().get("authorization").unwrap(),
            "Bearer a-token"
        );
    }
}
