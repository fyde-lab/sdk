use serde::Deserialize;
use uuid::Uuid;

use crate::Result;

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

#[derive(Deserialize)]
struct SaveResponse {
    id: Uuid,
}

/// A client for the fyde server's documents service, used to save, fetch,
/// and list files over HTTP.
pub struct DocumentsClient {
    http: reqwest::Client,
    base_url: url::Url,
}

impl DocumentsClient {
    /// Creates a client for the documents service at the given `http://` or
    /// `https://` base URL (e.g. `http://127.0.0.1:8080`).
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let mut base_url = url::Url::parse(base_url.as_ref())?;
        // Ensure the base has a trailing slash so `Url::join` appends rather
        // than replaces the last path segment.
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }

        Ok(Self {
            http: reqwest::Client::new(),
            base_url,
        })
    }

    /// Saves `content` as a new document and returns its generated id.
    pub async fn save(
        &self,
        name: impl Into<String>,
        content_type: impl Into<String>,
        content: impl Into<Vec<u8>>,
    ) -> Result<Uuid> {
        let response = self
            .http
            .post(self.base_url.join("documents")?)
            .header("x-file-name", name.into())
            .header(reqwest::header::CONTENT_TYPE, content_type.into())
            .body(content.into())
            .send()
            .await?
            .error_for_status()?;

        let response: SaveResponse = response.json().await?;
        Ok(response.id)
    }

    /// Fetches a document's content by id, or `None` if it doesn't exist.
    pub async fn fetch(&self, id: Uuid) -> Result<Option<Document>> {
        let response = self
            .http
            .get(self.base_url.join(&format!("documents/{id}"))?)
            .send()
            .await?;

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
        let response = self
            .http
            .get(self.base_url.join("documents")?)
            .send()
            .await?
            .error_for_status()?;

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
