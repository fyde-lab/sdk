use tonic::transport::Channel;
use uuid::Uuid;

use crate::{Error, Result};

/// Generated protobuf/gRPC bindings for the `documents` service, compiled
/// from `../api-protos/documents.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("documents");
}

pub(super) use proto::DocumentMeta;

use proto::documents_client::DocumentsClient;
use proto::{FetchDocumentRequest, ListDocumentsRequest, SaveDocumentRequest};

/// A thin gRPC transport for talking to the fyde server's documents
/// service. Knows nothing about documents themselves; just sends requests
/// and hands back raw responses.
pub(super) struct GrpcClient {
    client: DocumentsClient<Channel>,
}

impl GrpcClient {
    /// Connects to the documents service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`).
    pub async fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let endpoint = Channel::from_shared(base_url.as_ref().to_string())
            .map_err(|err| Error::InvalidEndpoint(err.to_string()))?;
        let channel = endpoint.connect().await?;

        Ok(Self {
            client: DocumentsClient::new(channel),
        })
    }

    /// Uploads an encrypted document, sending its ciphertext `content`,
    /// wrapped data encryption key (`dek`), and encrypted `metadatas`.
    /// Returns the generated document id.
    pub async fn save_document(
        &mut self,
        content: Vec<u8>,
        dek: Vec<u8>,
        metadatas: Vec<u8>,
    ) -> Result<Uuid> {
        let response = self
            .client
            .save_document(SaveDocumentRequest {
                content,
                dek,
                metadatas,
            })
            .await?;

        Ok(Uuid::parse_str(&response.into_inner().id)?)
    }

    /// Fetches a document's encrypted content by id, or `None` if it
    /// doesn't exist.
    pub async fn fetch_document(&mut self, id: Uuid) -> Result<Option<Vec<u8>>> {
        let request = FetchDocumentRequest { id: id.to_string() };

        match self.client.fetch_document(request).await {
            Ok(response) => Ok(Some(response.into_inner().content)),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
            Err(status) => Err(status.into()),
        }
    }

    /// Lists the metadata of every stored document, still encrypted, along
    /// with each document's wrapped DEK.
    pub async fn list_documents(&mut self) -> Result<Vec<DocumentMeta>> {
        let response = self
            .client
            .list_documents(ListDocumentsRequest {})
            .await?;

        Ok(response.into_inner().documents)
    }
}
