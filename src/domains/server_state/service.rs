use async_trait::async_trait;
use tonic_health::pb::health_check_response::ServingStatus;

use crate::Result;

use super::Service;
use super::grpc_client::FydeClient;

/// The default [`Service`] implementation, checking reachability over an
/// injected [`FydeClient`].
pub struct ServerStateClient {
    grpc: Box<dyn FydeClient>,
}

impl ServerStateClient {
    /// Creates a client checking reachability over `grpc`.
    pub(super) fn new(grpc: impl FydeClient + 'static) -> Self {
        Self {
            grpc: Box::new(grpc),
        }
    }
}

#[async_trait]
impl Service for ServerStateClient {
    async fn is_server_reachable(&self) -> Result<bool> {
        Ok(matches!(
            self.grpc.check().await,
            Ok(ServingStatus::Serving)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::grpc_client::MockFydeClient;
    use super::*;
    use crate::ErrorKind;

    #[tokio::test]
    async fn is_server_reachable_returns_true_when_the_server_reports_serving() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_check()
            .times(1)
            .returning(|| Ok(ServingStatus::Serving));

        let client = ServerStateClient::new(mock_grpc);

        assert!(client.is_server_reachable().await.unwrap());
    }

    #[tokio::test]
    async fn is_server_reachable_returns_false_when_the_server_reports_not_serving() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_check()
            .times(1)
            .returning(|| Ok(ServingStatus::NotServing));

        let client = ServerStateClient::new(mock_grpc);

        assert!(!client.is_server_reachable().await.unwrap());
    }

    #[tokio::test]
    async fn is_server_reachable_returns_false_rather_than_erroring_on_a_transport_failure() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_check()
            .times(1)
            .returning(|| Err(ErrorKind::InvalidResponse("connection refused".to_string()).into()));

        let client = ServerStateClient::new(mock_grpc);

        assert!(!client.is_server_reachable().await.unwrap());
    }
}
