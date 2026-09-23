use async_trait::async_trait;
use tonic::transport::Channel;
use tonic_health::pb::health_check_response::ServingStatus;

use crate::Result;

use super::Service;
use super::grpc_client::{FydeClient, GrpcClient};

/// The default [`Service`] implementation, checking reachability over an
/// injected [`FydeClient`].
pub struct ServerStateClient {
    grpc: Box<dyn FydeClient>,
}

impl ServerStateClient {
    /// Creates a client for the server_state service using the shared
    /// `channel` connection to the fyde server.
    pub(super) fn new(channel: Channel) -> Self {
        Self {
            grpc: Box::new(GrpcClient::new(channel)),
        }
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of
    /// a live server.
    #[cfg(test)]
    fn with_grpc(grpc: impl FydeClient + 'static) -> Self {
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
    use crate::Error;

    #[tokio::test]
    async fn is_server_reachable_returns_true_when_the_server_reports_serving() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_check()
            .times(1)
            .returning(|| Ok(ServingStatus::Serving));

        let client = ServerStateClient::with_grpc(mock_grpc);

        assert!(client.is_server_reachable().await.unwrap());
    }

    #[tokio::test]
    async fn is_server_reachable_returns_false_when_the_server_reports_not_serving() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_check()
            .times(1)
            .returning(|| Ok(ServingStatus::NotServing));

        let client = ServerStateClient::with_grpc(mock_grpc);

        assert!(!client.is_server_reachable().await.unwrap());
    }

    #[tokio::test]
    async fn is_server_reachable_returns_false_rather_than_erroring_on_a_transport_failure() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_check()
            .times(1)
            .returning(|| Err(Error::InvalidResponse("connection refused".to_string())));

        let client = ServerStateClient::with_grpc(mock_grpc);

        assert!(!client.is_server_reachable().await.unwrap());
    }
}
