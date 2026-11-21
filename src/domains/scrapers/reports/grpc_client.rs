use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, Result};

use super::models::{DeviceType, ReportUpload};

/// Generated protobuf/gRPC bindings for the `reports` service, compiled by
/// `build.rs` from `reports/v1/reports.proto` in the `buf.build/fyde-lab/api`
/// BSR module (sourced from `../server/protos`).
mod proto {
    tonic::include_proto!("reports.v1");
}

use proto::{
    DeviceType as ProtoDeviceType, UploadReportRequest,
    reports_service_client::ReportsServiceClient as GeneratedReportsClient,
};

/// A gRPC transport for the fyde server's reports service. Knows nothing
/// beyond translating a [`ReportUpload`] into its proto request.
/// Abstracted as a trait so callers can be tested against
/// [`MockFydeClient`] instead of a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Stores `upload` on the server, which links it to no user.
    async fn upload_report(&self, upload: ReportUpload) -> Result<()>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::init`]). Every call is authenticated by attaching the
/// persisted session token (see
/// [`crate::domains::sessions::Service::authenticated_request`]), and its
/// outcome checked for a session the server rejected (see
/// [`crate::domains::sessions::Service::check_response`]).
pub(super) struct GrpcClient {
    channel: Channel,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    /// Creates a client for the reports service using the shared `channel`
    /// connection to the fyde server, authenticating every call via
    /// `sessions`.
    pub(super) fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self { channel, sessions }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> GeneratedReportsClient<Channel> {
        GeneratedReportsClient::new(self.channel.clone())
    }
}

fn proto_device_type(device_type: DeviceType) -> ProtoDeviceType {
    match device_type {
        DeviceType::Android => ProtoDeviceType::Android,
        DeviceType::Ios => ProtoDeviceType::Ios,
        DeviceType::Desktop => ProtoDeviceType::Desktop,
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn upload_report(&self, upload: ReportUpload) -> Result<()> {
        let request = self
            .sessions
            .authenticated_request(UploadReportRequest {
                report: upload.report,
                user_note: None,
                device_type: proto_device_type(upload.device_type).into(),
                client_id: upload.client_id,
                script_id: upload.script_id.to_string(),
                script_version: upload.script_version,
                duration_ms: upload.duration_ms,
            })
            .await
            .context("failed to authenticate the report upload")?;

        self.sessions
            .check_response(self.client().upload_report(request).await)
            .await
            .context("failed to upload a scraper report")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;
    use uuid::Uuid;

    use super::*;
    use crate::domains::secrets::MockService as MockSecretsService;

    #[tokio::test]
    async fn an_unreachable_server_fails_the_upload_with_context() {
        let mut secrets = MockSecretsService::new();
        secrets.expect_get().returning(|_| Ok(None));
        let sessions = Arc::new(SessionsClient::new(Arc::new(secrets)));
        // Connected lazily: nothing is dialed until the upload.
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel, sessions);

        let err = grpc
            .upload_report(ReportUpload {
                report: "{}".to_string(),
                device_type: DeviceType::Desktop,
                client_id: "desktop".to_string(),
                script_id: Uuid::new_v4(),
                script_version: 1,
                duration_ms: 1_000,
            })
            .await
            .unwrap_err();

        assert!(err.has_context());
    }

    #[test]
    fn every_device_type_has_its_own_proto_value() {
        assert_eq!(
            proto_device_type(DeviceType::Android),
            ProtoDeviceType::Android
        );
        assert_eq!(proto_device_type(DeviceType::Ios), ProtoDeviceType::Ios);
        assert_eq!(
            proto_device_type(DeviceType::Desktop),
            ProtoDeviceType::Desktop
        );
    }
}
