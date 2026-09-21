use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Cleartext metadata encrypted under a document's DEK before upload, and
/// decrypted back out of it on download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub content_type: String,
    pub created_at: i64,
    pub size: u64,
    pub checksum: String,
    /// Plaintext transcript of the document's PDF text content, extracted
    /// on upload.
    pub transcript: String,
}

/// A file, as returned by [`super::Service::get`]/[`super::Service::list`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
    pub metadata: Metadata,
}
