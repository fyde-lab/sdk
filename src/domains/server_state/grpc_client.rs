use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::{HealthCheckRequest, health_client::HealthClient as GeneratedHealthClient};

use crate::{ErrorContext as _, Result};

/// How long [`GrpcClient::check`] waits for the server's answer before
/// giving up. The shared channel has no deadline of its own, so without one
/// a check over a connection that silently went dead (a sleeping laptop, a
/// changed network) would hang until the OS gives up on the TCP connection,
/// many minutes later, rather than report the server unreachable.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A gRPC transport for the fyde server's standard gRPC health check
/// protocol (`grpc.health.v1.Health`). Knows nothing about server_state
/// semantics beyond the raw proto types; just sends/receives raw messages.
/// Abstracted as a trait so callers can be tested against
/// [`MockFydeClient`] instead of a live server. Generated bindings come
/// from the `tonic-health` crate (the standard, cross-language health
/// checking protocol), not from `build.rs`/`../server/protos`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Performs a `Check` call against the overall server status (the
    /// well-known empty service name), returning the serving status
    /// reported by the server. Fails if it hasn't answered within
    /// [`CHECK_TIMEOUT`].
    async fn check(&self) -> Result<ServingStatus>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::init`]). Cloning a [`Channel`] is cheap — it's just
/// a handle to the same underlying connection — so a fresh generated
/// client is created per call.
pub(super) struct GrpcClient {
    channel: Channel,
}

impl GrpcClient {
    /// Creates a client for the server's health service using the shared
    /// `channel` connection to the fyde server.
    pub fn new(channel: Channel) -> Self {
        Self { channel }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> GeneratedHealthClient<Channel> {
        GeneratedHealthClient::new(self.channel.clone())
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn check(&self) -> Result<ServingStatus> {
        let mut client = self.client();
        let call = client.check(HealthCheckRequest {
            service: String::new(),
        });
        let response = tokio::time::timeout(CHECK_TIMEOUT, call)
            .await
            .unwrap_or_else(|_| {
                Err(tonic::Status::deadline_exceeded(format!(
                    "no answer within {CHECK_TIMEOUT:?}"
                )))
            })
            .context("failed to check server health")?
            .into_inner();

        Ok(ServingStatus::try_from(response.status).unwrap_or(ServingStatus::Unknown))
    }
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;

    use super::*;
    use crate::ErrorKind;

    #[tokio::test]
    async fn new_does_not_connect_to_the_server() {
        // A syntactically valid but unreachable address, connected lazily:
        // if `client()` dialed eagerly at construction, this would fail
        // here rather than on first use below.
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel);

        let err = grpc.check().await.unwrap_err();

        assert!(err.has_context());
    }

    #[tokio::test(start_paused = true)]
    async fn check_gives_up_on_a_server_that_never_answers() {
        // Accepted by the kernel's backlog but never served: the HTTP/2
        // handshake never completes, as over a connection gone silent.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let channel = Endpoint::from_shared(url).unwrap().connect_lazy();
        let grpc = GrpcClient::new(channel);

        let err = grpc.check().await.unwrap_err();

        assert!(err.has_context());
        assert!(matches!(
            err.kind(),
            ErrorKind::Grpc(status) if status.code() == tonic::Code::DeadlineExceeded
        ));
    }
}
