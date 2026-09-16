use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::{Error, ErrorContext as _, Result};

/// Generated protobuf/gRPC bindings for the `documents` service, compiled
/// from `../api-protos/documents.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("documents");
}

pub(super) use proto::EncryptedDocument;

use proto::documents_client::DocumentsClient;
use proto::{FetchDocumentRequest, FetchDocumentsRequest, UploadDocumentRequest};

/// A gRPC transport for talking to the fyde server's documents service.
/// Knows nothing about documents themselves; just sends requests and hands
/// back raw responses. Abstracted as a trait so callers can be tested
/// against [`MockFydeClient`] instead of a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Uploads an encrypted document, sending its ciphertext `content`,
    /// wrapped data encryption key (`dek`), and encrypted `metadatas`.
    /// Returns the generated document id.
    async fn upload_document(
        &self,
        content: Vec<u8>,
        dek: Vec<u8>,
        metadatas: Vec<u8>,
    ) -> Result<Uuid>;

    /// Fetches a document's encrypted content, wrapped DEK, and encrypted
    /// metadata by id, or `None` if it doesn't exist.
    async fn fetch_document(&self, id: Uuid) -> Result<Option<EncryptedDocument>>;

    /// Fetches multiple documents' encrypted content, wrapped DEKs, and
    /// encrypted metadata by id in a single call. Ids that don't exist are
    /// omitted from the result.
    async fn fetch_documents(&self, ids: &[Uuid]) -> Result<Vec<EncryptedDocument>>;
}

/// The production [`FydeClient`] implementation, backed by a real tonic connection.
pub(super) struct GrpcClient {
    client: DocumentsClient<Channel>,
}

impl GrpcClient {
    /// Connects to the documents service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`).
    pub async fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let endpoint = Channel::from_shared(base_url.as_ref().to_string())
            .map_err(|err| Error::InvalidEndpoint(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .context("failed to connect to documents grpc endpoint")?;

        Ok(Self {
            client: DocumentsClient::new(channel),
        })
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn upload_document(
        &self,
        content: Vec<u8>,
        dek: Vec<u8>,
        metadatas: Vec<u8>,
    ) -> Result<Uuid> {
        // The generated client's RPC methods take `&mut self`, but the
        // underlying `Channel` is cheap to clone and safe to use
        // concurrently, so we clone it per call to expose `&self` here.
        let response = self
            .client
            .clone()
            .upload_document(UploadDocumentRequest {
                content,
                dek,
                metadatas,
            })
            .await
            .context("failed to upload document")?;

        Uuid::parse_str(&response.into_inner().id).context("invalid document id returned by server")
    }

    async fn fetch_document(&self, id: Uuid) -> Result<Option<EncryptedDocument>> {
        let request = FetchDocumentRequest { id: id.to_string() };

        match self.client.clone().fetch_document(request).await {
            Ok(response) => Ok(response.into_inner().document),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
            Err(status) => Err(status.into()),
        }
    }

    async fn fetch_documents(&self, ids: &[Uuid]) -> Result<Vec<EncryptedDocument>> {
        let request = FetchDocumentsRequest {
            ids: ids.iter().map(Uuid::to_string).collect(),
        };

        let response = self
            .client
            .clone()
            .fetch_documents(request)
            .await
            .context("failed to fetch documents")?;

        Ok(response.into_inner().documents)
    }
}
