use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Error;

/// The business domain a document's issuer belongs to (e.g. a bank, an
/// insurer, a retailer), as filled in by a user's classification script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCategory {
    Bank,
    Insurance,
    Retail,
    Telecom,
    Energy,
    Water,
    Health,
    Gov,
    Association,
    Education,
    Employer,
    Transport,
    Goods,
    Alimentation,
    Building,
    RealEstate,
    Web,
    Individual,
    Shopping,
}

impl SourceCategory {
    #[cfg(test)]
    pub(crate) const ALL: &'static [SourceCategory] = &[
        Self::Bank,
        Self::Insurance,
        Self::Retail,
        Self::Telecom,
        Self::Energy,
        Self::Water,
        Self::Health,
        Self::Gov,
        Self::Association,
        Self::Education,
        Self::Employer,
        Self::Transport,
        Self::Goods,
        Self::Alimentation,
        Self::Building,
        Self::RealEstate,
        Self::Web,
        Self::Individual,
        Self::Shopping,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bank => "bank",
            Self::Insurance => "insurance",
            Self::Retail => "retail",
            Self::Telecom => "telecom",
            Self::Energy => "energy",
            Self::Water => "water",
            Self::Health => "health",
            Self::Gov => "gov",
            Self::Association => "association",
            Self::Education => "education",
            Self::Employer => "employer",
            Self::Transport => "transport",
            Self::Goods => "goods",
            Self::Alimentation => "alimentation",
            Self::Building => "building",
            Self::RealEstate => "real_estate",
            Self::Web => "web",
            Self::Individual => "individual",
            Self::Shopping => "shopping",
        }
    }
}

impl fmt::Display for SourceCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SourceCategory {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "bank" => Ok(Self::Bank),
            "insurance" => Ok(Self::Insurance),
            "retail" => Ok(Self::Retail),
            "telecom" => Ok(Self::Telecom),
            "energy" => Ok(Self::Energy),
            "water" => Ok(Self::Water),
            "health" => Ok(Self::Health),
            "gov" => Ok(Self::Gov),
            "association" => Ok(Self::Association),
            "education" => Ok(Self::Education),
            "employer" => Ok(Self::Employer),
            "transport" => Ok(Self::Transport),
            "goods" => Ok(Self::Goods),
            "alimentation" => Ok(Self::Alimentation),
            "building" => Ok(Self::Building),
            "real_estate" => Ok(Self::RealEstate),
            "web" => Ok(Self::Web),
            "individual" => Ok(Self::Individual),
            "shopping" => Ok(Self::Shopping),
            other => Err(Error::InvalidSourceCategory(other.to_string())),
        }
    }
}

/// A finer-grained classification within a document's [`SourceCategory`],
/// as filled in by a user's classification script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceSubCategory {
    CivilRegistration,
    Immigration,
    Transport,
    Family,
    Tax,
    Health,
    RealEstate,
    Mobile,
    Internet,
    Citizen,
    Sport,
}

impl SourceSubCategory {
    #[cfg(test)]
    pub(crate) const ALL: &'static [SourceSubCategory] = &[
        Self::CivilRegistration,
        Self::Immigration,
        Self::Transport,
        Self::Family,
        Self::Tax,
        Self::Health,
        Self::RealEstate,
        Self::Mobile,
        Self::Internet,
        Self::Citizen,
        Self::Sport,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CivilRegistration => "civil_registration",
            Self::Immigration => "immigration",
            Self::Transport => "transport",
            Self::Family => "family",
            Self::Tax => "tax",
            Self::Health => "health",
            Self::RealEstate => "real_estate",
            Self::Mobile => "mobile",
            Self::Internet => "internet",
            Self::Citizen => "citizen",
            Self::Sport => "sport",
        }
    }
}

impl fmt::Display for SourceSubCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SourceSubCategory {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "civil_registration" => Ok(Self::CivilRegistration),
            "immigration" => Ok(Self::Immigration),
            "transport" => Ok(Self::Transport),
            "family" => Ok(Self::Family),
            "tax" => Ok(Self::Tax),
            "health" => Ok(Self::Health),
            "real_estate" => Ok(Self::RealEstate),
            "mobile" => Ok(Self::Mobile),
            "internet" => Ok(Self::Internet),
            "citizen" => Ok(Self::Citizen),
            "sport" => Ok(Self::Sport),
            other => Err(Error::InvalidSourceSubCategory(other.to_string())),
        }
    }
}

/// Cleartext metadata encrypted under a document's DEK before upload, and
/// decrypted back out of it on download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub id: Uuid,
    pub name: String,
    /// The file name as originally uploaded. Unlike `name`, this never
    /// changes after creation.
    pub original_name: String,
    pub content_type: String,
    pub created_at: i64,
    pub size: u64,
    pub checksum: String,
    /// Plaintext transcript of the document's PDF text content, extracted
    /// on upload.
    pub transcript: String,
    pub r#type: String,
    /// `None` until a classification script sets it — scripts never
    /// persist anything themselves, so a freshly parsed document has no
    /// category yet.
    pub source_category: Option<SourceCategory>,
    pub source_sub_category: Option<SourceSubCategory>,
    pub subject: String,
    pub qualification: String,
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

    pub fn source_category(&self) -> Option<SourceCategory> {
        self.source_category
    }

    pub fn source_sub_category(&self) -> Option<SourceSubCategory> {
        self.source_sub_category
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
    /// own storage/service layer, and by external callers of the public
    /// `parser` service (e.g. `fyde-scripts`) that need a placeholder
    /// document to run a script against without a real upload.
    pub fn new(id: Uuid, content: Vec<u8>, metadata: Metadata) -> Self {
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
                source_category: Some(
                    SourceCategory::ALL
                        [crate::testing::random_u64(SourceCategory::ALL.len() as u64) as usize],
                ),
                source_sub_category: Some(
                    SourceSubCategory::ALL
                        [crate::testing::random_u64(SourceSubCategory::ALL.len() as u64) as usize],
                ),
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

    pub(crate) fn with_source_category(
        mut self,
        source_category: impl Into<Option<SourceCategory>>,
    ) -> Self {
        self.metadata.source_category = source_category.into();
        self
    }

    pub(crate) fn with_source_sub_category(
        mut self,
        source_sub_category: impl Into<Option<SourceSubCategory>>,
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
            .with_source_category(SourceCategory::Bank)
            .with_source_sub_category(SourceSubCategory::Tax)
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
        assert_eq!(metadata.source_category, Some(SourceCategory::Bank));
        assert_eq!(metadata.source_sub_category, Some(SourceSubCategory::Tax));
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
