use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::crypto;
use super::grpc_client::{FydeClient, GrpcClient};
use super::storage::Storage;
use super::{Document, Metadata, NewDocument, Service};

/// The default [`Service`] implementation: talks to the fyde server's
/// documents service over gRPC, and delegates local, unencrypted persistence
/// to an injected [`Storage`]. Generic over the [`Storage`] implementation
/// used to cache documents fetched via [`Service::download`] and
/// [`Service::download_many`].
pub(super) struct DocumentsClient<S: Storage> {
    grpc: Box<dyn FydeClient>,
    storage: S,
}

impl<S: Storage> DocumentsClient<S> {
    /// Creates a client for the documents service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`), using `storage`
    /// to cache documents fetched via [`Service::download`] and
    /// [`Service::download_many`].
    pub(super) async fn new(base_url: impl AsRef<str>, storage: S) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(
                GrpcClient::new(base_url)
                    .await
                    .context("failed to create documents grpc client")?,
            ),
            storage,
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of a
    /// live server.
    #[cfg(test)]
    fn with_grpc(grpc: impl FydeClient + 'static, storage: S) -> Self {
        Self {
            grpc: Box::new(grpc),
            storage,
        }
    }
}

#[async_trait]
impl<S: Storage> Service for DocumentsClient<S> {
    /// Encrypts `document` and its metadata, then uploads them as a new
    /// document to the server, returning its generated id.
    ///
    /// This follows an envelope encryption scheme: a fresh, random data
    /// encryption key (DEK) is generated for the document, used to encrypt
    /// both its content and metadata (name, content type, creation time,
    /// size) with AES-256-GCM, and is itself wrapped under a
    /// key-encryption-key before being sent alongside the ciphertext. Only
    /// the wrapped DEK and ciphertexts ever leave this process.
    async fn upload(&self, document: NewDocument) -> Result<Uuid> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut hasher = Sha256::new();
        hasher.update(&document.content);

        let metadata = Metadata {
            name: document.name,
            content_type: document.content_type,
            created_at,
            size: document.content.len() as u64,
            checksum: hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        };
        let metadata_json =
            serde_json::to_vec(&metadata).context("failed to serialize document metadata")?;

        let (encrypted_content, wrapped_dek, encrypted_metadata) =
            crypto::encrypt_document(&document.content, &metadata_json)
                .context("failed to encrypt document")?;

        self.grpc
            .upload_document(encrypted_content, wrapped_dek, encrypted_metadata)
            .await
    }

    async fn download(&self, id: Uuid) -> Result<Option<Document>> {
        let Some(encrypted) = self
            .grpc
            .fetch_document(id)
            .await
            .with_context(|| format!("failed to fetch document {id}"))?
        else {
            return Ok(None);
        };

        let dek = crypto::unwrap_dek(&encrypted.dek)
            .with_context(|| format!("failed to unwrap DEK for document {id}"))?;
        let metadata_json = crypto::decrypt_with_dek(&dek, &encrypted.metadatas)
            .with_context(|| format!("failed to decrypt metadata for document {id}"))?;
        let metadata: Metadata = serde_json::from_slice(&metadata_json)
            .with_context(|| format!("failed to deserialize metadata for document {id}"))?;

        let content = crypto::decrypt_with_dek(&dek, &encrypted.content)
            .with_context(|| format!("failed to decrypt content for document {id}"))?;

        let document = Document {
            id,
            content,
            metadata,
        };

        self.storage
            .save_document(&document)
            .await
            .with_context(|| format!("failed to cache document {id} locally"))?;

        Ok(Some(document))
    }

    async fn get(&self, id: Uuid) -> Result<Option<Document>> {
        self.storage.get_document(id).await
    }

    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Document>> {
        self.storage.list_documents(offset, limit).await
    }

    /// Fetches and decrypts multiple documents by id in a single round
    /// trip. Ids that don't exist are omitted from the result.
    async fn download_many(&self, ids: Vec<Uuid>) -> Result<Vec<Document>> {
        let encrypted_documents = self
            .grpc
            .fetch_documents(&ids)
            .await
            .context("failed to fetch documents")?;

        let mut documents = Vec::with_capacity(encrypted_documents.len());

        for encrypted in encrypted_documents {
            let id = Uuid::parse_str(&encrypted.id).with_context(|| {
                format!("invalid document id returned by server: {}", encrypted.id)
            })?;

            let dek = crypto::unwrap_dek(&encrypted.dek)
                .with_context(|| format!("failed to unwrap DEK for document {id}"))?;
            let metadata_json = crypto::decrypt_with_dek(&dek, &encrypted.metadatas)
                .with_context(|| format!("failed to decrypt metadata for document {id}"))?;
            let metadata: Metadata = serde_json::from_slice(&metadata_json)
                .with_context(|| format!("failed to deserialize metadata for document {id}"))?;

            let content = crypto::decrypt_with_dek(&dek, &encrypted.content)
                .with_context(|| format!("failed to decrypt content for document {id}"))?;

            let document = Document {
                id,
                content,
                metadata,
            };

            self.storage
                .save_document(&document)
                .await
                .with_context(|| format!("failed to cache document {id} locally"))?;

            documents.push(document);
        }

        Ok(documents)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::super::grpc_client::{EncryptedDocument, MockFydeClient};
    use super::super::storage_sqlite::SqliteStorage;
    use super::*;

    #[test]
    fn metadata_roundtrips_through_json_and_crypto() {
        let metadata = Metadata {
            name: "report.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            created_at: 1_700_000_000,
            size: 4,
            checksum: "checksum-value".to_string(),
        };
        let metadata_json = serde_json::to_vec(&metadata).unwrap();

        let (_, wrapped_dek, encrypted_metadata) =
            crypto::encrypt_document(b"body", &metadata_json).unwrap();

        let dek = crypto::unwrap_dek(&wrapped_dek).unwrap();
        let decrypted_json = crypto::decrypt_with_dek(&dek, &encrypted_metadata).unwrap();
        let decrypted: Metadata = serde_json::from_slice(&decrypted_json).unwrap();

        assert_eq!(decrypted.name, metadata.name);
        assert_eq!(decrypted.content_type, metadata.content_type);
        assert_eq!(decrypted.created_at, metadata.created_at);
        assert_eq!(decrypted.size, metadata.size);
        assert_eq!(decrypted.checksum, metadata.checksum);
    }

    async fn setup_storage() -> SqliteStorage {
        // A single connection, so all queries in a test hit the same
        // in-memory database rather than each getting its own.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteStorage::new(pool)
    }

    #[tokio::test]
    async fn download_decrypts_and_caches_the_document() {
        let content = b"hello world".to_vec();
        let metadata = Metadata {
            name: "report.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            created_at: 1_700_000_000,
            size: content.len() as u64,
            checksum: "checksum-value".to_string(),
        };
        let metadata_json = serde_json::to_vec(&metadata).unwrap();

        let (encrypted_content, wrapped_dek, encrypted_metadata) =
            crypto::encrypt_document(&content, &metadata_json).unwrap();

        let id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_fetch_document()
            .withf(move |fetched_id| *fetched_id == id)
            .returning(move |_| {
                Ok(Some(EncryptedDocument {
                    id: id.to_string(),
                    content: encrypted_content.clone(),
                    dek: wrapped_dek.clone(),
                    metadatas: encrypted_metadata.clone(),
                }))
            });

        let client = DocumentsClient::with_grpc(mock_grpc, setup_storage().await);

        let document = client.download(id).await.unwrap().unwrap();

        assert_eq!(document.id, id);
        assert_eq!(document.metadata.name, "report.pdf");
        assert_eq!(document.metadata.content_type, "application/pdf");
        assert_eq!(document.content, content);
        assert_eq!(document.metadata.checksum, "checksum-value");

        // download() should have cached the document locally.
        let cached = client.get(id).await.unwrap().unwrap();
        assert_eq!(cached, document);
    }

    #[tokio::test]
    async fn upload_sends_encrypted_content_to_grpc() {
        let expected_id = Uuid::new_v4();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_upload_document()
            // The plaintext content must never reach the transport layer.
            .withf(|content, _dek, _metadatas| content != b"hello world")
            .returning(move |_, _, _| Ok(expected_id));

        let client = DocumentsClient::with_grpc(mock_grpc, setup_storage().await);

        let id = client
            .upload(NewDocument {
                name: "report.pdf".to_string(),
                content_type: "application/pdf".to_string(),
                content: b"hello world".to_vec(),
            })
            .await
            .unwrap();

        assert_eq!(id, expected_id);
    }
}
