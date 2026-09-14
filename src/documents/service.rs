use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Result;

use super::crypto;
use super::grpc_client::{DocumentMeta as ProtoDocumentMeta, GrpcClient};
use super::storage::Storage;

/// A file, as returned by [`DocumentsClient::fetch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: Uuid,
    pub name: String,
    pub content_type: String,
    pub content: Vec<u8>,
}

/// A document's metadata, without its file content, as returned by
/// [`DocumentsClient::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentMeta {
    pub id: Uuid,
    pub name: String,
    pub content_type: String,
    pub created_at: i64,
}

/// Cleartext metadata encrypted under the document's DEK before upload, and
/// decrypted back out of it on fetch/list.
#[derive(Serialize, Deserialize)]
struct DocumentEncryptedMetadata {
    name: String,
    content_type: String,
    created_at: i64,
    #[allow(dead_code)]
    size: u64,
}

/// A client for the fyde server's documents service, used to upload, fetch,
/// and list files over gRPC, as well as to save documents locally. Generic
/// over the [`Storage`] implementation used by [`Self::save`] to persist
/// documents.
pub struct DocumentsClient<S: Storage> {
    grpc: GrpcClient,
    storage: S,
}

impl<S: Storage> DocumentsClient<S> {
    /// Creates a client for the documents service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`), using `storage`
    /// to persist documents saved via [`Self::save`].
    pub async fn new(base_url: impl AsRef<str>, storage: S) -> Result<Self> {
        Ok(Self {
            grpc: GrpcClient::new(base_url).await?,
            storage,
        })
    }

    /// Saves `content` and its metadata as-is, unencrypted, in local
    /// storage, returning its generated id.
    pub async fn save(
        &mut self,
        name: impl Into<String>,
        content_type: impl Into<String>,
        content: impl Into<Vec<u8>>,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let name = name.into();
        let content_type = content_type.into();
        let content = content.into();

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        self.storage
            .save_document(id, &name, &content_type, &content, created_at)
            .await?;

        Ok(id)
    }

    /// Encrypts `content` and its metadata, then uploads them as a new
    /// document to the server, returning its generated id.
    ///
    /// This follows an envelope encryption scheme: a fresh, random data
    /// encryption key (DEK) is generated for the document, used to encrypt
    /// both `content` and its metadata (name, content type, creation time,
    /// size) with AES-256-GCM, and is itself wrapped under a
    /// key-encryption-key before being sent alongside the ciphertext. Only
    /// the wrapped DEK and ciphertexts ever leave this process.
    pub async fn upload(
        &mut self,
        name: impl Into<String>,
        content_type: impl Into<String>,
        content: impl Into<Vec<u8>>,
    ) -> Result<Uuid> {
        let name = name.into();
        let content_type = content_type.into();
        let content = content.into();

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let metadata = DocumentEncryptedMetadata {
            name,
            content_type,
            created_at,
            size: content.len() as u64,
        };
        let metadata_json = serde_json::to_vec(&metadata)?;

        let (encrypted_content, wrapped_dek, encrypted_metadata) =
            crypto::encrypt_document(&content, &metadata_json)?;

        self.grpc
            .save_document(encrypted_content, wrapped_dek, encrypted_metadata)
            .await
    }

    /// Fetches a document's content by id, or `None` if it doesn't exist.
    pub async fn fetch(&mut self, id: Uuid) -> Result<Option<Document>> {
        // The server stores documents as blind, opaque ciphertext: fetching
        // a document's content doesn't hand back its DEK or metadata, so
        // those are looked up (and decrypted) via `list` first.
        let Some(proto_meta) = self.find_meta(id).await? else {
            return Ok(None);
        };
        let Some(encrypted_content) = self.grpc.fetch_document(id).await? else {
            return Ok(None);
        };

        let dek = crypto::unwrap_dek(&proto_meta.dek)?;
        let metadata_json = crypto::decrypt_with_dek(&dek, &proto_meta.metadatas)?;
        let metadata: DocumentEncryptedMetadata = serde_json::from_slice(&metadata_json)?;

        let content = crypto::decrypt_with_dek(&dek, &encrypted_content)?;

        Ok(Some(Document {
            id,
            name: metadata.name,
            content_type: metadata.content_type,
            content,
        }))
    }

    /// Lists the metadata of every stored document, without file content.
    pub async fn list(&mut self) -> Result<Vec<DocumentMeta>> {
        let documents = self.grpc.list_documents().await?;

        documents
            .into_iter()
            .map(|proto_meta| {
                let id = Uuid::parse_str(&proto_meta.id)?;
                let dek = crypto::unwrap_dek(&proto_meta.dek)?;
                let metadata_json = crypto::decrypt_with_dek(&dek, &proto_meta.metadatas)?;
                let metadata: DocumentEncryptedMetadata = serde_json::from_slice(&metadata_json)?;

                Ok(DocumentMeta {
                    id,
                    name: metadata.name,
                    content_type: metadata.content_type,
                    created_at: metadata.created_at,
                })
            })
            .collect()
    }

    /// Finds a single document's still-encrypted metadata by id.
    async fn find_meta(&mut self, id: Uuid) -> Result<Option<ProtoDocumentMeta>> {
        let documents = self.grpc.list_documents().await?;
        Ok(documents
            .into_iter()
            .find(|proto_meta| proto_meta.id == id.to_string()))
    }
}
