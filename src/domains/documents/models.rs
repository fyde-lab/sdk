use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::attributes::Attribute;
use crate::{Error, ErrorKind};

/// The part of life a document is about — where it would be filed — rather
/// than who issued it: a bank's tax form is [`Self::Taxes`], an internet
/// box's invoice [`Self::Housing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Housing,
    Telecom,
    Money,
    Taxes,
    Health,
    Work,
    Family,
    Transport,
    Shopping,
    Subscriptions,
    Donations,
}

impl Category {
    pub(crate) const ALL: &'static [Category] = &[
        Self::Housing,
        Self::Telecom,
        Self::Money,
        Self::Taxes,
        Self::Health,
        Self::Work,
        Self::Family,
        Self::Transport,
        Self::Shopping,
        Self::Subscriptions,
        Self::Donations,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Housing => "housing",
            Self::Telecom => "telecom",
            Self::Money => "money",
            Self::Taxes => "taxes",
            Self::Health => "health",
            Self::Work => "work",
            Self::Family => "family",
            Self::Transport => "transport",
            Self::Shopping => "shopping",
            Self::Subscriptions => "subscriptions",
            Self::Donations => "donations",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Category {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|value| value.as_str() == s)
            .ok_or_else(|| ErrorKind::InvalidCategory(s.to_string()).into())
    }
}

/// A finer-grained classification within one [`Category`] (see
/// [`Self::category`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subcategory {
    Energy,
    Water,
    Internet,
    HomeInsurance,
    HomeServices,
    Mobile,
    Banking,
    Savings,
    Gaming,
    IncomeTax,
    TaxCertificates,
    HealthInsurance,
    ComplementaryHealth,
    Care,
    Payslips,
    Employment,
    HouseholdEmployer,
    Benefits,
    ChildcareSchool,
    Train,
    Flight,
    Road,
    Ride,
    Travel,
    Online,
    Pets,
    Groceries,
    FoodDelivery,
    Media,
    CloudSoftware,
}

impl Subcategory {
    pub(crate) const ALL: &'static [Subcategory] = &[
        Self::Energy,
        Self::Water,
        Self::Internet,
        Self::HomeInsurance,
        Self::HomeServices,
        Self::Mobile,
        Self::Banking,
        Self::Savings,
        Self::Gaming,
        Self::IncomeTax,
        Self::TaxCertificates,
        Self::HealthInsurance,
        Self::ComplementaryHealth,
        Self::Care,
        Self::Payslips,
        Self::Employment,
        Self::HouseholdEmployer,
        Self::Benefits,
        Self::ChildcareSchool,
        Self::Train,
        Self::Flight,
        Self::Road,
        Self::Ride,
        Self::Travel,
        Self::Online,
        Self::Pets,
        Self::Groceries,
        Self::FoodDelivery,
        Self::Media,
        Self::CloudSoftware,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Energy => "energy",
            Self::Water => "water",
            Self::Internet => "internet",
            Self::HomeInsurance => "home_insurance",
            Self::HomeServices => "home_services",
            Self::Mobile => "mobile",
            Self::Banking => "banking",
            Self::Savings => "savings",
            Self::Gaming => "gaming",
            Self::IncomeTax => "income_tax",
            Self::TaxCertificates => "tax_certificates",
            Self::HealthInsurance => "health_insurance",
            Self::ComplementaryHealth => "complementary_health",
            Self::Care => "care",
            Self::Payslips => "payslips",
            Self::Employment => "employment",
            Self::HouseholdEmployer => "household_employer",
            Self::Benefits => "benefits",
            Self::ChildcareSchool => "childcare_school",
            Self::Train => "train",
            Self::Flight => "flight",
            Self::Road => "road",
            Self::Ride => "ride",
            Self::Travel => "travel",
            Self::Online => "online",
            Self::Pets => "pets",
            Self::Groceries => "groceries",
            Self::FoodDelivery => "food_delivery",
            Self::Media => "media",
            Self::CloudSoftware => "cloud_software",
        }
    }

    /// The [`Category`] this subcategory narrows down.
    pub fn category(&self) -> Category {
        match self {
            Self::Energy
            | Self::Water
            | Self::Internet
            | Self::HomeInsurance
            | Self::HomeServices => Category::Housing,
            Self::Mobile => Category::Telecom,
            Self::Banking | Self::Savings | Self::Gaming => Category::Money,
            Self::IncomeTax | Self::TaxCertificates => Category::Taxes,
            Self::HealthInsurance | Self::ComplementaryHealth | Self::Care => Category::Health,
            Self::Payslips | Self::Employment | Self::HouseholdEmployer => Category::Work,
            Self::Benefits | Self::ChildcareSchool => Category::Family,
            Self::Train | Self::Flight | Self::Road | Self::Ride | Self::Travel => {
                Category::Transport
            }
            Self::Online | Self::Pets | Self::Groceries | Self::FoodDelivery => Category::Shopping,
            Self::Media | Self::CloudSoftware => Category::Subscriptions,
        }
    }
}

impl fmt::Display for Subcategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Subcategory {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|value| value.as_str() == s)
            .ok_or_else(|| ErrorKind::InvalidSubcategory(s.to_string()).into())
    }
}

/// What kind of paper a document is, independent of who issued it or which
/// part of life it's about (an EDF bill and an Amazon order are both
/// [`Self::Invoice`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    Invoice,
    Receipt,
    Statement,
    ReimbursementStatement,
    Payslip,
    Contract,
    InsuranceCertificate,
    InsuranceCard,
    RightsCertificate,
    ProofOfResidence,
    Rib,
    TaxNotice,
    TaxReturn,
    TaxCertificate,
    EmploymentCertificate,
    FeeSummary,
    Booking,
    MedicalDocument,
    Other,
}

impl DocumentType {
    pub(crate) const ALL: &'static [DocumentType] = &[
        Self::Invoice,
        Self::Receipt,
        Self::Statement,
        Self::ReimbursementStatement,
        Self::Payslip,
        Self::Contract,
        Self::InsuranceCertificate,
        Self::InsuranceCard,
        Self::RightsCertificate,
        Self::ProofOfResidence,
        Self::Rib,
        Self::TaxNotice,
        Self::TaxReturn,
        Self::TaxCertificate,
        Self::EmploymentCertificate,
        Self::FeeSummary,
        Self::Booking,
        Self::MedicalDocument,
        Self::Other,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Invoice => "invoice",
            Self::Receipt => "receipt",
            Self::Statement => "statement",
            Self::ReimbursementStatement => "reimbursement_statement",
            Self::Payslip => "payslip",
            Self::Contract => "contract",
            Self::InsuranceCertificate => "insurance_certificate",
            Self::InsuranceCard => "insurance_card",
            Self::RightsCertificate => "rights_certificate",
            Self::ProofOfResidence => "proof_of_residence",
            Self::Rib => "rib",
            Self::TaxNotice => "tax_notice",
            Self::TaxReturn => "tax_return",
            Self::TaxCertificate => "tax_certificate",
            Self::EmploymentCertificate => "employment_certificate",
            Self::FeeSummary => "fee_summary",
            Self::Booking => "booking",
            Self::MedicalDocument => "medical_document",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for DocumentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DocumentType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|r#type| r#type.as_str() == s)
            .ok_or_else(|| ErrorKind::InvalidDocumentType(s.to_string()).into())
    }
}

/// Cleartext metadata encrypted under a document's DEK before upload, and
/// decrypted back out of it on download. Serialized through
/// [`MetadataWire`], which keeps the wire layout of fields since removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "MetadataWire", into = "MetadataWire")]
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
    /// `None` until a classification script, or the uploader, sets it.
    pub r#type: Option<DocumentType>,
    /// `None` until a classification script sets it — scripts never
    /// persist anything themselves, so a freshly parsed document has no
    /// category yet.
    pub category: Option<Category>,
    pub subcategory: Option<Subcategory>,
    /// Every unstructured piece of data a document carries beyond the
    /// fields above (e.g. `"reference"` → `Attribute::String("F-42")`),
    /// keyed by a `snake_case` name — one of the user's attribute
    /// definitions ([`super::attributes::Service::list`]) whenever one
    /// fits. A piece of data that needs to be filtered,
    /// faceted or sorted on across documents belongs in a field of its
    /// own instead, the way `category` is.
    pub attributes: BTreeMap<String, Attribute>,
}

/// [`Metadata`]'s serialized form. Changelog events are MessagePack-encoded
/// positionally (a struct is an array of its fields), so a removed field
/// keeps its slot: `subjects` and `purpose` are written empty and ignored
/// on read, so events recorded before they were removed still decode, and
/// devices still running a version that has them still decode new ones.
#[derive(Serialize, Deserialize)]
struct MetadataWire {
    id: Uuid,
    name: String,
    original_name: String,
    content_type: String,
    created_at: i64,
    size: u64,
    checksum: String,
    transcript: String,
    r#type: Option<DocumentType>,
    category: Option<Category>,
    subcategory: Option<Subcategory>,
    /// Removed: always written empty, ignored on read.
    subjects: Vec<String>,
    /// Removed: always written `None`, ignored on read.
    purpose: Option<String>,
    /// Defaults to empty so metadata encrypted before this field existed
    /// still decodes.
    #[serde(default)]
    attributes: BTreeMap<String, Attribute>,
}

impl From<MetadataWire> for Metadata {
    fn from(wire: MetadataWire) -> Self {
        Self {
            id: wire.id,
            name: wire.name,
            original_name: wire.original_name,
            content_type: wire.content_type,
            created_at: wire.created_at,
            size: wire.size,
            checksum: wire.checksum,
            transcript: wire.transcript,
            r#type: wire.r#type,
            category: wire.category,
            subcategory: wire.subcategory,
            attributes: wire.attributes,
        }
    }
}

impl From<Metadata> for MetadataWire {
    fn from(metadata: Metadata) -> Self {
        Self {
            id: metadata.id,
            name: metadata.name,
            original_name: metadata.original_name,
            content_type: metadata.content_type,
            created_at: metadata.created_at,
            size: metadata.size,
            checksum: metadata.checksum,
            transcript: metadata.transcript,
            r#type: metadata.r#type,
            category: metadata.category,
            subcategory: metadata.subcategory,
            subjects: Vec::new(),
            purpose: None,
            attributes: metadata.attributes,
        }
    }
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

    pub fn r#type(&self) -> Option<DocumentType> {
        self.r#type
    }

    pub fn category(&self) -> Option<Category> {
        self.category
    }

    pub fn subcategory(&self) -> Option<Subcategory> {
        self.subcategory
    }

    pub fn attributes(&self) -> &BTreeMap<String, Attribute> {
        &self.attributes
    }
}

/// A file, as returned by [`super::Service::get`]/[`super::Service::list`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Where [`super::Service::upload`] reads a document's content from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadSource {
    /// Reads the content from the file at this path on disk; the file's
    /// name (after stripping its path) becomes the document's
    /// `original_name`, and its extension is what's validated as a
    /// supported document type.
    Path(std::path::PathBuf),
    /// Uploads already-in-memory content directly, under `name` (used in
    /// place of a path's file name, including for the extension
    /// validation `Path` would otherwise get from it).
    Raw { name: String, content: Vec<u8> },
}

/// Parameters for [`super::Service::upload`]: where to read the document's
/// content from, plus any of the modifiable [`Metadata`] fields to set
/// up front. Every field besides `source` is optional — left `None`, it's
/// derived from the document itself and whatever scripts run against it;
/// set, it overrides that derived value. `attributes` is merged over the
/// ones derived instead, name by name: an attribute set here replaces a
/// script's attribute of the same name, and leaves the others in place.
#[derive(Debug, Clone, PartialEq)]
pub struct UploadRequest {
    pub source: UploadSource,
    pub name: Option<String>,
    pub r#type: Option<DocumentType>,
    pub category: Option<Category>,
    pub subcategory: Option<Subcategory>,
    pub attributes: BTreeMap<String, Attribute>,
}

impl UploadRequest {
    /// Builds a request to upload the file at `path`, with no metadata
    /// overrides.
    pub fn from_path(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            source: UploadSource::Path(path.into()),
            name: None,
            r#type: None,
            category: None,
            subcategory: None,
            attributes: BTreeMap::new(),
        }
    }

    /// Builds a request to upload `content` directly, named `name`, with no
    /// metadata overrides.
    pub fn from_raw(name: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        Self {
            source: UploadSource::Raw {
                name: name.into(),
                content: content.into(),
            },
            name: None,
            r#type: None,
            category: None,
            subcategory: None,
            attributes: BTreeMap::new(),
        }
    }
}

/// Restricts a [`super::Service::search`] to documents whose fields match.
///
/// Within a single list, a document matches if its field equals *any* of
/// the listed values; across fields, it must match *every* one that's set.
/// An empty list, or a `None` bound, leaves that field unconstrained — so
/// `SearchFilters::default()` matches every document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub categories: Vec<Category>,
    pub subcategories: Vec<Subcategory>,
    pub types: Vec<DocumentType>,
    /// Matched exactly against [`Metadata::content_type`].
    pub content_types: Vec<String>,
    /// Inclusive lower bound on [`Metadata::created_at`].
    pub created_from: Option<i64>,
    /// Inclusive upper bound on [`Metadata::created_at`].
    pub created_until: Option<i64>,
}

/// How many documents carry one value of a field, as listed in
/// [`SearchFacets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetCount<T> {
    pub value: T,
    pub count: u64,
}

/// The filter values still worth offering for a search, as returned by
/// [`super::Service::search_facets`]: for each field [`SearchFilters`] can
/// filter on, every value carried by at least one document the search would
/// match, most common first — so a UI only ever proposes choices that lead
/// somewhere. Unclassified documents (no category, type...) and empty
/// values are left out.
///
/// Each field's values ignore that field's *own* filter (but honor every
/// other one), so picking a category narrows the subcategories, types...
/// on offer to that category's, while every category still compatible with
/// the rest of the search stays available to switch to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFacets {
    pub categories: Vec<FacetCount<Category>>,
    pub subcategories: Vec<FacetCount<Subcategory>>,
    pub types: Vec<FacetCount<DocumentType>>,
    pub content_types: Vec<FacetCount<String>>,
    /// The oldest matching [`Metadata::created_at`], ignoring the
    /// `created_from`/`created_until` filters; `None` when nothing matches.
    pub earliest_created_at: Option<i64>,
    /// The newest matching [`Metadata::created_at`], likewise.
    pub latest_created_at: Option<i64>,
}

/// The order [`super::Service::search`] returns its matches in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchOrder {
    /// Best text match first, newest first among equally good matches.
    /// With no search text there's nothing to rank by, so this is the same
    /// as [`Self::Newest`].
    #[default]
    Relevance,
    Newest,
    Oldest,
}

/// Parameters for [`super::Service::search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Free text matched against a document's name, original name,
    /// transcript, type and category/subcategory. Every word must match one of those fields, as a
    /// case- and accent-insensitive prefix ("fact" matches "Facturé").
    /// Blank matches every document, leaving only `filters` to narrow it.
    pub text: String,
    pub filters: SearchFilters,
    pub order: SearchOrder,
    /// Paginate by repeatedly incrementing this by the returned page's
    /// length until it comes back shorter than `limit`.
    pub offset: i64,
    pub limit: i64,
}

impl SearchQuery {
    /// Builds a query for the page `offset..offset + limit` of documents
    /// matching `text`, unfiltered, ordered by relevance.
    pub fn new(text: impl Into<String>, offset: i64, limit: i64) -> Self {
        Self {
            text: text.into(),
            filters: SearchFilters::default(),
            order: SearchOrder::default(),
            offset,
            limit,
        }
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
        let name = format!("{}.pdf", crate::tools::testing::random_word());
        let subcategory = Subcategory::ALL
            [crate::tools::testing::random_u64(Subcategory::ALL.len() as u64) as usize];
        Self {
            metadata: Metadata {
                id: Uuid::now_v7(),
                original_name: name.clone(),
                name,
                content_type: "application/pdf".to_string(),
                created_at: crate::tools::testing::random_past_timestamp(),
                size: 1024 + crate::tools::testing::random_u64(10 * 1024 * 1024),
                checksum: crate::tools::testing::random_hex(32),
                transcript: format!(
                    "This is a fake transcript about {}.",
                    crate::tools::testing::random_word()
                ),
                r#type: Some(
                    DocumentType::ALL[crate::tools::testing::random_u64(
                        DocumentType::ALL.len() as u64
                    ) as usize],
                ),
                category: Some(subcategory.category()),
                subcategory: Some(subcategory),
                attributes: BTreeMap::from([(
                    crate::tools::testing::random_word().to_string(),
                    Attribute::Integer(crate::tools::testing::random_u64(1000) as i64),
                )]),
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

    pub(crate) fn with_type(mut self, r#type: impl Into<Option<DocumentType>>) -> Self {
        self.metadata.r#type = r#type.into();
        self
    }

    pub(crate) fn with_category(mut self, category: impl Into<Option<Category>>) -> Self {
        self.metadata.category = category.into();
        self
    }

    pub(crate) fn with_subcategory(mut self, subcategory: impl Into<Option<Subcategory>>) -> Self {
        self.metadata.subcategory = subcategory.into();
        self
    }

    pub(crate) fn with_attributes(
        mut self,
        attributes: impl Into<BTreeMap<String, Attribute>>,
    ) -> Self {
        self.metadata.attributes = attributes.into();
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
                content: crate::tools::testing::random_bytes(64),
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

/// Builds a [`SearchFacets`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeSearchFacets {
    facets: SearchFacets,
}

#[cfg(test)]
impl FakeSearchFacets {
    pub(crate) fn new() -> Self {
        let count = || 1 + crate::tools::testing::random_u64(20);
        let earliest = crate::tools::testing::random_past_timestamp();
        Self {
            facets: SearchFacets {
                categories: vec![FacetCount {
                    value: Category::ALL
                        [crate::tools::testing::random_u64(Category::ALL.len() as u64) as usize],
                    count: count(),
                }],
                subcategories: vec![FacetCount {
                    value: Subcategory::ALL
                        [crate::tools::testing::random_u64(Subcategory::ALL.len() as u64) as usize],
                    count: count(),
                }],
                types: vec![FacetCount {
                    value: DocumentType::ALL[crate::tools::testing::random_u64(
                        DocumentType::ALL.len() as u64,
                    ) as usize],
                    count: count(),
                }],
                content_types: vec![FacetCount {
                    value: "application/pdf".to_string(),
                    count: count(),
                }],
                earliest_created_at: Some(earliest),
                latest_created_at: Some(
                    earliest + crate::tools::testing::random_u64(86_400) as i64,
                ),
            },
        }
    }

    pub(crate) fn with_categories(mut self, categories: Vec<FacetCount<Category>>) -> Self {
        self.facets.categories = categories;
        self
    }

    pub(crate) fn with_subcategories(
        mut self,
        subcategories: Vec<FacetCount<Subcategory>>,
    ) -> Self {
        self.facets.subcategories = subcategories;
        self
    }

    pub(crate) fn with_types(mut self, types: Vec<FacetCount<DocumentType>>) -> Self {
        self.facets.types = types;
        self
    }

    pub(crate) fn with_content_types(mut self, content_types: Vec<FacetCount<String>>) -> Self {
        self.facets.content_types = content_types;
        self
    }

    /// Sets both `earliest_created_at` and `latest_created_at` at once,
    /// since they're only ever both set or both `None`.
    pub(crate) fn with_created_at_range(mut self, range: Option<(i64, i64)>) -> Self {
        self.facets.earliest_created_at = range.map(|(earliest, _)| earliest);
        self.facets.latest_created_at = range.map(|(_, latest)| latest);
        self
    }

    pub(crate) fn build(self) -> SearchFacets {
        self.facets
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
            .with_type(DocumentType::Invoice)
            .with_category(Category::Money)
            .with_subcategory(Subcategory::Banking)
            .with_attributes([("price".to_string(), Attribute::Float(32.3))])
            .build();

        assert_eq!(metadata.id, id);
        assert_eq!(metadata.name, "report.pdf");
        assert_eq!(metadata.original_name, "original-report.pdf");
        assert_eq!(metadata.content_type, "application/pdf");
        assert_eq!(metadata.created_at, 1_700_000_000);
        assert_eq!(metadata.size, 42);
        assert_eq!(metadata.checksum, "deadbeef");
        assert_eq!(metadata.transcript, "hello world");
        assert_eq!(metadata.r#type, Some(DocumentType::Invoice));
        assert_eq!(metadata.category, Some(Category::Money));
        assert_eq!(metadata.subcategory, Some(Subcategory::Banking));
        assert_eq!(
            metadata.attributes,
            BTreeMap::from([("price".to_string(), Attribute::Float(32.3))])
        );
    }

    #[test]
    fn metadata_with_every_attribute_type_round_trips_through_messagepack() {
        let metadata = FakeMetadata::new()
            .with_attributes([
                ("text".to_string(), Attribute::String("F-42".to_string())),
                ("count".to_string(), Attribute::Integer(2)),
                ("ratio".to_string(), Attribute::Float(0.5)),
                ("paid".to_string(), Attribute::Boolean(true)),
                ("on".to_string(), Attribute::Date("2026-10-15".to_string())),
                (
                    "price".to_string(),
                    Attribute::Amount {
                        minor_units: -3230,
                        currency: "EUR".to_string(),
                    },
                ),
                (
                    "status".to_string(),
                    Attribute::Select {
                        selected: "unpaid".to_string(),
                        options: vec!["paid".to_string(), "unpaid".to_string()],
                    },
                ),
            ])
            .build();

        let encoded = rmp_serde::to_vec(&metadata).unwrap();

        assert_eq!(
            rmp_serde::from_slice::<Metadata>(&encoded).unwrap(),
            metadata
        );
    }

    #[test]
    fn metadata_encoded_as_messagepack_before_attributes_existed_still_decodes() {
        // Changelog events are MessagePack-encoded positionally (a struct is
        // an array of its fields), so an event recorded before `attributes`
        // existed is the same array minus its last element: here, a
        // fixarray header one shorter and no trailing empty map.
        let metadata = FakeMetadata::new().with_attributes(BTreeMap::new()).build();
        let mut legacy = rmp_serde::to_vec(&metadata).unwrap();
        assert_eq!(legacy[0], 0x9e, "expected a 14-field fixarray");
        assert_eq!(legacy.pop(), Some(0x80), "expected a trailing empty map");
        legacy[0] = 0x9d;

        let decoded: Metadata = rmp_serde::from_slice(&legacy).unwrap();

        assert!(decoded.attributes.is_empty());
        assert_eq!(decoded.name, metadata.name);
    }

    #[test]
    fn metadata_encoded_with_subjects_and_purpose_still_decodes() {
        // The layout `Metadata` had before `subjects`/`purpose` were
        // removed, with both set.
        #[derive(Serialize)]
        struct Legacy {
            id: Uuid,
            name: String,
            original_name: String,
            content_type: String,
            created_at: i64,
            size: u64,
            checksum: String,
            transcript: String,
            r#type: Option<DocumentType>,
            category: Option<Category>,
            subcategory: Option<Subcategory>,
            subjects: Vec<String>,
            purpose: Option<String>,
            attributes: BTreeMap<String, Attribute>,
        }
        let metadata = FakeMetadata::new().build();
        let legacy = rmp_serde::to_vec(&Legacy {
            id: metadata.id,
            name: metadata.name.clone(),
            original_name: metadata.original_name.clone(),
            content_type: metadata.content_type.clone(),
            created_at: metadata.created_at,
            size: metadata.size,
            checksum: metadata.checksum.clone(),
            transcript: metadata.transcript.clone(),
            r#type: metadata.r#type,
            category: metadata.category,
            subcategory: metadata.subcategory,
            subjects: vec!["person:alice".to_string()],
            purpose: Some("invoice".to_string()),
            attributes: metadata.attributes.clone(),
        })
        .unwrap();

        let decoded: Metadata = rmp_serde::from_slice(&legacy).unwrap();

        assert_eq!(decoded, metadata);
    }

    #[test]
    fn metadata_without_attributes_still_deserializes() {
        let mut json = serde_json::to_value(FakeMetadata::new().build()).unwrap();
        json.as_object_mut().unwrap().remove("attributes");

        let metadata: Metadata = serde_json::from_value(json).unwrap();

        assert!(metadata.attributes.is_empty());
    }

    #[test]
    fn document_type_round_trips_through_its_string_form() {
        for r#type in DocumentType::ALL {
            assert_eq!(r#type.as_str().parse::<DocumentType>().unwrap(), *r#type);
        }
    }

    #[test]
    fn metadata_round_trips_through_its_encoding() {
        let metadata = FakeMetadata::new().build();

        let decoded: Metadata =
            rmp_serde::from_slice(&rmp_serde::to_vec(&metadata).unwrap()).unwrap();

        assert_eq!(decoded, metadata);
    }

    #[test]
    fn every_category_but_donations_has_a_subcategory() {
        for category in Category::ALL {
            let has_subcategory = Subcategory::ALL
                .iter()
                .any(|subcategory| subcategory.category() == *category);
            assert_eq!(
                has_subcategory,
                *category != Category::Donations,
                "{category}"
            );
        }
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

    #[test]
    fn fake_search_facets_builds_with_plausible_defaults() {
        let facets = FakeSearchFacets::new().build();

        assert!(!facets.categories.is_empty());
        assert!(facets.categories.iter().all(|facet| facet.count > 0));
        assert!(facets.earliest_created_at <= facets.latest_created_at);
    }

    #[test]
    fn fake_search_facets_with_methods_override_defaults() {
        let count = |value: &str| FacetCount {
            value: value.to_string(),
            count: 1,
        };
        let facets = FakeSearchFacets::new()
            .with_categories(vec![FacetCount {
                value: Category::Money,
                count: 2,
            }])
            .with_subcategories(Vec::new())
            .with_types(vec![FacetCount {
                value: DocumentType::Payslip,
                count: 1,
            }])
            .with_content_types(vec![count("application/pdf")])
            .with_created_at_range(Some((10, 20)))
            .build();

        assert_eq!(
            facets,
            SearchFacets {
                categories: vec![FacetCount {
                    value: Category::Money,
                    count: 2
                }],
                subcategories: Vec::new(),
                types: vec![FacetCount {
                    value: DocumentType::Payslip,
                    count: 1
                }],
                content_types: vec![count("application/pdf")],
                earliest_created_at: Some(10),
                latest_created_at: Some(20),
            }
        );
    }
}
