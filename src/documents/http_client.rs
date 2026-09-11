use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Result;

/// A thin HTTP transport for talking to the fyde server's documents
/// endpoints. Knows nothing about documents themselves; just sends requests
/// and hands back raw responses.
pub(super) struct HttpClient {
    http: reqwest::Client,
    base_url: url::Url,
}

/// The MessagePack body expected by the server's save-document endpoint.
#[derive(Serialize)]
struct SaveDocumentRequest {
    #[serde(with = "serde_bytes")]
    content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    dek: Vec<u8>,
    #[serde(with = "serde_bytes")]
    metadatas: Vec<u8>,
}

/// The server's response to a successful document upload.
#[derive(Deserialize)]
pub(super) struct SaveResponse {
    pub id: Uuid,
}

impl HttpClient {
    /// Creates a client for the given `http://` or `https://` base URL
    /// (e.g. `http://127.0.0.1:8080`).
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let base_url = url::Url::parse(base_url.as_ref())?;

        Ok(Self {
            http: reqwest::Client::new(),
            base_url,
        })
    }

    pub async fn get(&self, path: &str) -> Result<reqwest::Response> {
        Ok(self.http.get(self.base_url.join(path)?).send().await?)
    }

    pub async fn post(
        &self,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<reqwest::Response> {
        let mut request = self.http.post(self.base_url.join(path)?);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        Ok(request.body(body).send().await?)
    }

    /// Uploads an encrypted document, sending `content`, its wrapped data
    /// encryption key (`dek`), and its encrypted `metadatas` as a
    /// MessagePack-encoded body. Returns the generated document id.
    pub async fn upload_document(
        &self,
        content: Vec<u8>,
        dek: Vec<u8>,
        metadatas: Vec<u8>,
    ) -> Result<SaveResponse> {
        let body = rmp_serde::to_vec_named(&SaveDocumentRequest {
            content,
            dek,
            metadatas,
        })?;

        let response = self
            .post(
                "documents",
                &[(
                    reqwest::header::CONTENT_TYPE.as_str(),
                    "application/msgpack",
                )],
                body,
            )
            .await?
            .error_for_status()?;

        let body = response.bytes().await?;
        Ok(rmp_serde::from_slice(&body)?)
    }
}
