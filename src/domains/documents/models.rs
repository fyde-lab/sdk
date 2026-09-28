use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Cleartext metadata encrypted under a document's DEK before upload, and
/// decrypted back out of it on download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub(super) id: Uuid,
    pub(super) name: String,
    /// The file name as originally uploaded. Unlike `name`, this never
    /// changes after creation.
    pub(super) original_name: String,
    pub(super) content_type: String,
    pub(super) created_at: i64,
    pub(super) size: u64,
    pub(super) checksum: String,
    /// Plaintext transcript of the document's PDF text content, extracted
    /// on upload.
    pub(super) transcript: String,
    pub(super) r#type: String,
    pub(super) source_category: String,
    pub(super) source_sub_category: Option<String>,
    pub(super) subject: String,
    pub(super) qualification: String,
}

impl Metadata {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn original_name(&self) -> &str {
        &self.original_name
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    pub fn transcript(&self) -> &str {
        &self.transcript
    }

    pub fn r#type(&self) -> &str {
        &self.r#type
    }

    pub fn source_category(&self) -> &str {
        &self.source_category
    }

    pub fn source_sub_category(&self) -> Option<&str> {
        self.source_sub_category.as_deref()
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn qualification(&self) -> &str {
        &self.qualification
    }
}

/// A file, as returned by [`super::Service::get`]/[`super::Service::list`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub(super) id: Uuid,
    #[serde(with = "serde_bytes")]
    pub(super) content: Vec<u8>,
    pub(super) metadata: Metadata,
}

impl Document {
    /// Constructs a document from already-known parts. Used by other
    /// domains (e.g. `changelog`, materializing a document from a decrypted
    /// event) that need to build one without going through `documents`'s
    /// own storage/service layer.
    pub(crate) fn new(id: Uuid, content: Vec<u8>, metadata: Metadata) -> Self {
        Self {
            id,
            content,
            metadata,
        }
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn content(&self) -> &[u8] {
        &self.content
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
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
        let name = format!("{}.pdf", crate::testing::random_word());
        Self {
            metadata: Metadata {
                id: Uuid::now_v7(),
                original_name: name.clone(),
                name,
                content_type: "application/pdf".to_string(),
                created_at: crate::testing::random_past_timestamp(),
                size: 1024 + crate::testing::random_u64(10 * 1024 * 1024),
                checksum: crate::testing::random_hex(32),
                transcript: format!(
                    "This is a fake transcript about {}.",
                    crate::testing::random_word()
                ),
                r#type: crate::testing::random_word().to_string(),
                source_category: crate::testing::random_word().to_string(),
                source_sub_category: Some(crate::testing::random_word().to_string()),
                subject: crate::testing::random_word().to_string(),
                qualification: crate::testing::random_word().to_string(),
            },
        }
    }

    pub(crate) fn with_id(mut self, id: Uuid) -> Self {
        self.metadata.id = id;
        self
    }

    pub(crate) fn with_name(mut self, name: impl Into<String>) -> Self {
        self.metadata.name = name.into();
        self
    }

    pub(crate) fn with_original_name(mut self, original_name: impl Into<String>) -> Self {
        self.metadata.original_name = original_name.into();
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

    pub(crate) fn with_type(mut self, r#type: impl Into<String>) -> Self {
        self.metadata.r#type = r#type.into();
        self
    }

    pub(crate) fn with_source_category(mut self, source_category: impl Into<String>) -> Self {
        self.metadata.source_category = source_category.into();
        self
    }

    pub(crate) fn with_source_sub_category(
        mut self,
        source_sub_category: impl Into<Option<String>>,
    ) -> Self {
        self.metadata.source_sub_category = source_sub_category.into();
        self
    }

    pub(crate) fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.metadata.subject = subject.into();
        self
    }

    pub(crate) fn with_qualification(mut self, qualification: impl Into<String>) -> Self {
        self.metadata.qualification = qualification.into();
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
        let id = Uuid::now_v7();
        Self {
            document: Document {
                id,
                content: crate::testing::random_bytes(64),
                metadata: FakeMetadata::new().with_id(id).build(),
            },
        }
    }

    pub(crate) fn with_id(mut self, id: Uuid) -> Self {
        self.document.id = id;
        self.document.metadata.id = id;
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
        let id = Uuid::now_v7();
        let metadata = FakeMetadata::new()
            .with_id(id)
            .with_name("report.pdf")
            .with_original_name("original-report.pdf")
            .with_content_type("application/pdf")
            .with_created_at(1_700_000_000)
            .with_size(42)
            .with_checksum("deadbeef")
            .with_transcript("hello world")
            .with_type("invoice")
            .with_source_category("finance")
            .with_source_sub_category(Some("billing".to_string()))
            .with_subject("Q1 report")
            .with_qualification("verified")
            .build();

        assert_eq!(metadata.id, id);
        assert_eq!(metadata.name, "report.pdf");
        assert_eq!(metadata.original_name, "original-report.pdf");
        assert_eq!(metadata.content_type, "application/pdf");
        assert_eq!(metadata.created_at, 1_700_000_000);
        assert_eq!(metadata.size, 42);
        assert_eq!(metadata.checksum, "deadbeef");
        assert_eq!(metadata.transcript, "hello world");
        assert_eq!(metadata.r#type, "invoice");
        assert_eq!(metadata.source_category, "finance");
        assert_eq!(metadata.source_sub_category.as_deref(), Some("billing"));
        assert_eq!(metadata.subject, "Q1 report");
        assert_eq!(metadata.qualification, "verified");
    }

    #[test]
    fn fake_document_builds_with_plausible_defaults() {
        let document = FakeDocument::new().build();

        assert!(!document.content.is_empty());
        assert!(document.metadata.name.ends_with(".pdf"));
    }

    #[test]
    fn fake_document_with_methods_override_defaults() {
        let id = Uuid::now_v7();
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
