use async_trait::async_trait;

use crate::{ErrorContext as _, Result};

use super::grpc_client::FydeClient;
use super::{Service, Usage};

/// The default [`Service`] implementation, over an injected
/// [`FydeClient`].
pub struct UsageClient {
    grpc: Box<dyn FydeClient>,
}

impl UsageClient {
    /// Creates a client fetching usage over `grpc`.
    pub(super) fn new(grpc: impl FydeClient + 'static) -> Self {
        Self {
            grpc: Box::new(grpc),
        }
    }
}

#[async_trait]
impl Service for UsageClient {
    async fn get_usage(&self) -> Result<Usage> {
        self.grpc
            .get_usage()
            .await
            .context("failed to fetch the account's plan usage")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use crate::domains::usage::FakeUsage;
    use crate::domains::usage::grpc_client::MockFydeClient;

    #[tokio::test]
    async fn returns_the_servers_usage() {
        // Init
        let mut grpc = MockFydeClient::new();

        // Variables
        let usage = FakeUsage::new().build();

        // Mocks
        let returned = usage.clone();
        grpc.expect_get_usage()
            .times(1)
            .return_once(move || Ok(returned));

        // Run
        let result = UsageClient::new(grpc).get_usage().await.unwrap();

        // Asserts
        assert_eq!(result, usage);
    }

    #[tokio::test]
    async fn wraps_a_transport_failure_with_context() {
        // Init
        let mut grpc = MockFydeClient::new();

        // Mocks
        grpc.expect_get_usage()
            .times(1)
            .return_once(|| Err(ErrorKind::InvalidResponse("boom".to_string()).into()));

        // Run
        let err = UsageClient::new(grpc).get_usage().await.unwrap_err();

        // Asserts
        assert!(err.has_context());
        assert!(matches!(err.kind(), ErrorKind::InvalidResponse(_)));
    }
}
