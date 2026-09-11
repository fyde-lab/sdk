use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Result;

use super::crypto;
use super::http_client::HttpClient;

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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DocumentMeta {
    pub id: Uuid,
    pub name: String,
    pub content_type: String,
    pub created_at: i64,
}

/// Cleartext metadata encrypted under the document's DEK before upload.
#[derive(Serialize)]
struct DocumentMetadata {
    name: String,
    content_type: String,
    created_at: i64,
    size: u64,
}

/// A client for the fyde server's documents service, used to save, fetch,
/// and list files over HTTP.
pub struct DocumentsClient {
    http: HttpClient,
}

impl DocumentsClient {
    /// Creates a client for the documents service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`).
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            http: HttpClient::new(base_url)?,
        })
    }

    /// Encrypts `content` and its metadata, then uploads them as a new
    /// document, returning its generated id.
    ///
    /// This follows an envelope encryption scheme: a fresh, random data
    /// encryption key (DEK) is generated for the document, used to encrypt
    /// both `content` and its metadata (name, content type, creation time,
    /// size) with AES-256-GCM, and is itself wrapped under a
    /// key-encryption-key before being sent alongside the ciphertext. Only
    /// the wrapped DEK and ciphertexts ever leave this process.
    pub async fn save(
        &self,
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

        let metadata = DocumentMetadata {
            name,
            content_type,
            created_at,
            size: content.len() as u64,
        };
        let metadata_json = serde_json::to_vec(&metadata)?;

        let (encrypted_content, wrapped_dek, encrypted_metadata) =
            crypto::encrypt_document(&content, &metadata_json)?;

        let response = self
            .http
            .upload_document(encrypted_content, wrapped_dek, encrypted_metadata)
            .await?;
        Ok(response.id)
    }

    /// Fetches a document's content by id, or `None` if it doesn't exist.
    pub async fn fetch(&self, id: Uuid) -> Result<Option<Document>> {
        let response = self.http.get(&format!("documents/{id}")).await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response.error_for_status()?;

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let name = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_filename)
            .unwrap_or_default();

        let content = response.bytes().await?.to_vec();

        Ok(Some(Document {
            id,
            name,
            content_type,
            content,
        }))
    }

    /// Lists the metadata of every stored document, without file content.
    pub async fn list(&self) -> Result<Vec<DocumentMeta>> {
        let response = self.http.get("documents").await?.error_for_status()?;

        let documents = response.json().await?;
        Ok(documents)
    }
}

/// Extracts the quoted `filename` parameter from a `Content-Disposition`
/// header value, e.g. `attachment; filename="hello.txt"` -> `hello.txt`.
fn parse_filename(content_disposition: &str) -> Option<String> {
    content_disposition
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("filename="))
        .map(|name| name.trim_matches('"').to_string())
}
