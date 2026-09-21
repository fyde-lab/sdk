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

/// Builds a [`Metadata`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeMetadata {
    metadata: Metadata,
}

#[cfg(test)]
impl FakeMetadata {
    pub(crate) fn new() -> Self {
        Self {
            metadata: Metadata {
                name: format!("{}.pdf", crate::testing::random_word()),
                content_type: "application/pdf".to_string(),
                created_at: crate::testing::random_past_timestamp(),
                size: 1024 + crate::testing::random_u64(10 * 1024 * 1024),
                checksum: crate::testing::random_hex(32),
                transcript: format!(
                    "This is a fake transcript about {}.",
                    crate::testing::random_word()
                ),
            },
        }
    }

    pub(crate) fn with_name(mut self, name: impl Into<String>) -> Self {
        self.metadata.name = name.into();
        self
    }

    pub(crate) fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.metadata.content_type = content_type.into();
        self
    }

    pub(crate) fn with_created_at(mut self, created_at: i64) -> Self {
        self.metadata.created_at = created_at;
        self
    }

    pub(crate) fn with_size(mut self, size: u64) -> Self {
        self.metadata.size = size;
        self
    }

    pub(crate) fn with_checksum(mut self, checksum: impl Into<String>) -> Self {
        self.metadata.checksum = checksum.into();
        self
    }

    pub(crate) fn with_transcript(mut self, transcript: impl Into<String>) -> Self {
        self.metadata.transcript = transcript.into();
        self
    }

    pub(crate) fn build(self) -> Metadata {
        self.metadata
    }
}

/// Builds a [`Document`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeDocument {
    document: Document,
}

#[cfg(test)]
impl FakeDocument {
    pub(crate) fn new() -> Self {
        Self {
            document: Document {
                id: Uuid::new_v4(),
                content: crate::testing::random_bytes(64),
                metadata: FakeMetadata::new().build(),
            },
        }
    }

    pub(crate) fn with_id(mut self, id: Uuid) -> Self {
        self.document.id = id;
        self
    }

    pub(crate) fn with_content(mut self, content: Vec<u8>) -> Self {
        self.document.content = content;
        self
    }

    pub(crate) fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.document.metadata = metadata;
        self
    }

    pub(crate) fn build(self) -> Document {
        self.document
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_metadata_builds_with_plausible_defaults() {
        let metadata = FakeMetadata::new().build();

        assert!(metadata.name.ends_with(".pdf"));
        assert_eq!(metadata.content_type, "application/pdf");
        assert_eq!(metadata.checksum.len(), 64);
    }

    #[test]
    fn fake_metadata_with_methods_override_defaults() {
        let metadata = FakeMetadata::new()
            .with_name("report.pdf")
            .with_content_type("application/pdf")
            .with_created_at(1_700_000_000)
            .with_size(42)
            .with_checksum("deadbeef")
            .with_transcript("hello world")
            .build();

        assert_eq!(metadata.name, "report.pdf");
        assert_eq!(metadata.content_type, "application/pdf");
        assert_eq!(metadata.created_at, 1_700_000_000);
        assert_eq!(metadata.size, 42);
        assert_eq!(metadata.checksum, "deadbeef");
        assert_eq!(metadata.transcript, "hello world");
    }

    #[test]
    fn fake_document_builds_with_plausible_defaults() {
        let document = FakeDocument::new().build();

        assert!(!document.content.is_empty());
        assert!(document.metadata.name.ends_with(".pdf"));
    }

    #[test]
    fn fake_document_with_methods_override_defaults() {
        let id = Uuid::new_v4();
        let metadata = FakeMetadata::new().with_name("report.pdf").build();

        let document = FakeDocument::new()
            .with_id(id)
            .with_content(vec![1, 2, 3])
            .with_metadata(metadata.clone())
            .build();

        assert_eq!(document.id, id);
        assert_eq!(document.content, vec![1, 2, 3]);
        assert_eq!(document.metadata, metadata);
    }
}
