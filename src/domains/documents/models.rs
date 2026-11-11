use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::attributes::Attribute;
use crate::{Error, ErrorKind};

/// The part of life a document is about — where it would be filed — rather
/// than who issued it: a bank's tax form is [`Self::Taxes`], an internet
/// box's invoice [`Self::Telecom`]. No longer part of a document's metadata
/// (folders replaced it): only names the classifier's label descriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    Housing,
    Telecom,
    Money,
    Taxes,
    Health,
    Work,
    Family,
    Identity,
    Legal,
    Transport,
    Pets,
    Shopping,
    Leisure,
    Education,
    Subscriptions,
    Donations,
}

#[cfg(test)]
impl Category {
    pub(crate) const ALL: &'static [Category] = &[
        Self::Housing,
        Self::Telecom,
        Self::Money,
        Self::Taxes,
        Self::Health,
        Self::Work,
        Self::Family,
        Self::Identity,
        Self::Legal,
        Self::Transport,
        Self::Pets,
        Self::Shopping,
        Self::Leisure,
        Self::Education,
        Self::Subscriptions,
        Self::Donations,
    ];
}

/// A finer-grained classification within one [`Category`] (see
/// [`Self::category`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subcategory {
    Energy,
    Water,
    HomeInsurance,
    HomeServices,
    Rent,
    Property,
    Works,
    Mobile,
    Internet,
    Banking,
    Savings,
    Investments,
    Loans,
    Gaming,
    IncomeTax,
    TaxCertificates,
    PropertyTax,
    HousingTax,
    HealthInsurance,
    ComplementaryHealth,
    Care,
    DeathDisabilityCover,
    Payslips,
    Employment,
    HouseholdEmployer,
    Freelance,
    Unemployment,
    Retirement,
    Benefits,
    Childcare,
    IdDocuments,
    CivilStatus,
    InheritanceGifts,
    Company,
    Justice,
    Train,
    Flight,
    Road,
    Ride,
    Travel,
    Vehicle,
    VehicleInsurance,
    Veterinary,
    PetCare,
    PetInsurance,
    PetSupplies,
    Online,
    Groceries,
    FoodDelivery,
    Electronics,
    HomeEquipment,
    Clothing,
    Secondhand,
    Sports,
    Events,
    Associations,
    School,
    HigherEducation,
    Training,
    Media,
    CloudSoftware,
    Press,
}

#[cfg(test)]
impl Subcategory {
    pub(crate) const ALL: &'static [Subcategory] = &[
        Self::Energy,
        Self::Water,
        Self::HomeInsurance,
        Self::HomeServices,
        Self::Rent,
        Self::Property,
        Self::Works,
        Self::Mobile,
        Self::Internet,
        Self::Banking,
        Self::Savings,
        Self::Investments,
        Self::Loans,
        Self::Gaming,
        Self::IncomeTax,
        Self::TaxCertificates,
        Self::PropertyTax,
        Self::HousingTax,
        Self::HealthInsurance,
        Self::ComplementaryHealth,
        Self::Care,
        Self::DeathDisabilityCover,
        Self::Payslips,
        Self::Employment,
        Self::HouseholdEmployer,
        Self::Freelance,
        Self::Unemployment,
        Self::Retirement,
        Self::Benefits,
        Self::Childcare,
        Self::IdDocuments,
        Self::CivilStatus,
        Self::InheritanceGifts,
        Self::Company,
        Self::Justice,
        Self::Train,
        Self::Flight,
        Self::Road,
        Self::Ride,
        Self::Travel,
        Self::Vehicle,
        Self::VehicleInsurance,
        Self::Veterinary,
        Self::PetCare,
        Self::PetInsurance,
        Self::PetSupplies,
        Self::Online,
        Self::Groceries,
        Self::FoodDelivery,
        Self::Electronics,
        Self::HomeEquipment,
        Self::Clothing,
        Self::Secondhand,
        Self::Sports,
        Self::Events,
        Self::Associations,
        Self::School,
        Self::HigherEducation,
        Self::Training,
        Self::Media,
        Self::CloudSoftware,
        Self::Press,
    ];

    /// The [`Category`] this subcategory narrows down.
    pub(crate) fn category(&self) -> Category {
        match self {
            Self::Energy
            | Self::Water
            | Self::HomeInsurance
            | Self::HomeServices
            | Self::Rent
            | Self::Property
            | Self::Works => Category::Housing,
            Self::Mobile | Self::Internet => Category::Telecom,
            Self::Banking | Self::Savings | Self::Investments | Self::Loans | Self::Gaming => {
                Category::Money
            }
            Self::IncomeTax | Self::TaxCertificates | Self::PropertyTax | Self::HousingTax => {
                Category::Taxes
            }
            Self::HealthInsurance
            | Self::ComplementaryHealth
            | Self::Care
            | Self::DeathDisabilityCover => Category::Health,
            Self::Payslips
            | Self::Employment
            | Self::HouseholdEmployer
            | Self::Freelance
            | Self::Unemployment
            | Self::Retirement => Category::Work,
            Self::Benefits | Self::Childcare => Category::Family,
            Self::IdDocuments | Self::CivilStatus => Category::Identity,
            Self::InheritanceGifts | Self::Company | Self::Justice => Category::Legal,
            Self::Train
            | Self::Flight
            | Self::Road
            | Self::Ride
            | Self::Travel
            | Self::Vehicle
            | Self::VehicleInsurance => Category::Transport,
            Self::Veterinary | Self::PetCare | Self::PetInsurance | Self::PetSupplies => {
                Category::Pets
            }
            Self::Online
            | Self::Groceries
            | Self::FoodDelivery
            | Self::Electronics
            | Self::HomeEquipment
            | Self::Clothing
            | Self::Secondhand => Category::Shopping,
            Self::Sports | Self::Events | Self::Associations => Category::Leisure,
            Self::School | Self::HigherEducation | Self::Training => Category::Education,
            Self::Media | Self::CloudSoftware | Self::Press => Category::Subscriptions,
        }
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
    Quote,
    IdDocument,
    CivilStatusRecord,
    Letter,
    Acknowledgement,
    Form,
    PaymentSchedule,
    Mandate,
    TermsAndConditions,
    Brochure,
    Timesheet,
    InspectionReport,
    NotarialDeed,
    Attestation,
    PaymentReminder,
    Fine,
    Warranty,
    Resume,
    Minutes,
    Diploma,
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
        Self::Quote,
        Self::IdDocument,
        Self::CivilStatusRecord,
        Self::Letter,
        Self::Acknowledgement,
        Self::Form,
        Self::PaymentSchedule,
        Self::Mandate,
        Self::TermsAndConditions,
        Self::Brochure,
        Self::Timesheet,
        Self::InspectionReport,
        Self::NotarialDeed,
        Self::Attestation,
        Self::PaymentReminder,
        Self::Fine,
        Self::Warranty,
        Self::Resume,
        Self::Minutes,
        Self::Diploma,
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
            Self::Quote => "quote",
            Self::IdDocument => "id_document",
            Self::CivilStatusRecord => "civil_status_record",
            Self::Letter => "letter",
            Self::Acknowledgement => "acknowledgement",
            Self::Form => "form",
            Self::PaymentSchedule => "payment_schedule",
            Self::Mandate => "mandate",
            Self::TermsAndConditions => "terms_and_conditions",
            Self::Brochure => "brochure",
            Self::Timesheet => "timesheet",
            Self::InspectionReport => "inspection_report",
            Self::NotarialDeed => "notarial_deed",
            Self::Attestation => "attestation",
            Self::PaymentReminder => "payment_reminder",
            Self::Fine => "fine",
            Self::Warranty => "warranty",
            Self::Resume => "resume",
            Self::Minutes => "minutes",
            Self::Diploma => "diploma",
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
    /// Guessed from the text at upload (`None` when the classifier can't
    /// tell), or set by a later metadata update.
    pub r#type: Option<DocumentType>,
    /// The folder holding the document (see `folders`), `None` for one at
    /// the root of the vault, in À classer. A document whose folder was
    /// deleted on another device meanwhile also shows there.
    pub folder_id: Option<Uuid>,
    /// Every unstructured piece of data a document carries beyond the
    /// fields above (e.g. `"reference"` → `Attribute::String("F-42")`),
    /// keyed by a `snake_case` name — one of the user's attribute
    /// definitions ([`super::attributes::Service::list`]) whenever one
    /// fits. A piece of data that needs to be filtered,
    /// faceted or sorted on across documents belongs in a field of its
    /// own instead, the way `folder_id` is.
    pub attributes: BTreeMap<String, Attribute>,
}

/// [`Metadata`]'s serialized form: changelog events are MessagePack-encoded
/// positionally (a struct is an array of its fields), so fields are only
/// ever added at the end, with `#[serde(default)]`.
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
    folder_id: Option<Uuid>,
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
            folder_id: wire.folder_id,
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
            folder_id: metadata.folder_id,
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

    pub fn folder_id(&self) -> Option<Uuid> {
        self.folder_id
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
/// content from (its file name becomes the document's `original_name`),
/// and the name to give it, if not that one. Everything else (type,
/// folder) is derived from the document itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRequest {
    pub source: UploadSource,
    pub name: Option<String>,
}

impl UploadRequest {
    /// Builds a request to upload the file at `path`, under its own name.
    pub fn from_path(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            source: UploadSource::Path(path.into()),
            name: None,
        }
    }

    /// Builds a request to upload `content` directly, its original name
    /// being `name`.
    pub fn from_raw(name: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        Self {
            source: UploadSource::Raw {
                name: name.into(),
                content: content.into(),
            },
            name: None,
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
    /// Restricts the search to a folder (subfolders included) or to À
    /// classer.
    pub folder: Option<FolderFilter>,
    pub types: Vec<DocumentType>,
    /// Matched exactly against [`Metadata::content_type`].
    pub content_types: Vec<String>,
    /// Inclusive lower bound on [`Metadata::created_at`].
    pub created_from: Option<i64>,
    /// Inclusive upper bound on [`Metadata::created_at`].
    pub created_until: Option<i64>,
}

/// Where [`SearchFilters::folder`] restricts a search to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderFilter {
    /// The documents at the root of the vault (no folder, or one deleted
    /// meanwhile): À classer.
    Unfiled,
    /// The documents of this folder and every folder below it.
    Within(Uuid),
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
/// somewhere. Untyped documents and empty values are left out.
///
/// Each field's values ignore that field's *own* filter (but honor every
/// other one), so picking a type narrows the folders on offer to those
/// holding that type, while every type still compatible with the rest of
/// the search stays available to switch to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFacets {
    /// Every folder holding at least one matching document, itself or in a
    /// subfolder, with that count (its subfolders' documents included),
    /// most first.
    pub folders: Vec<FacetCount<Uuid>>,
    /// How many matching documents are in À classer.
    pub unfiled: u64,
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
    /// Best match first. Besides the documents matching every word (see
    /// [`SearchQuery::text`]), finds those whose text is close in meaning
    /// to the search text, even sharing no word with it, and ranks both
    /// together; with no indexing model on the device yet, only the former.
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
    /// transcript and type. Every word must match one of those fields, as a
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
#[cfg(any(test, feature = "testing"))]
pub struct FakeMetadata {
    metadata: Metadata,
}

#[cfg(any(test, feature = "testing"))]
impl Default for FakeMetadata {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl FakeMetadata {
    pub fn new() -> Self {
        let name = format!("{}.pdf", crate::tools::testing::random_word());
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
                folder_id: None,
                attributes: BTreeMap::from([(
                    crate::tools::testing::random_word().to_string(),
                    Attribute::Integer(crate::tools::testing::random_u64(1000) as i64),
                )]),
            },
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.metadata.id = id;
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.metadata.name = name.into();
        self
    }

    pub fn with_original_name(mut self, original_name: impl Into<String>) -> Self {
        self.metadata.original_name = original_name.into();
        self
    }

    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.metadata.content_type = content_type.into();
        self
    }

    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.metadata.created_at = created_at;
        self
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.metadata.size = size;
        self
    }

    pub fn with_checksum(mut self, checksum: impl Into<String>) -> Self {
        self.metadata.checksum = checksum.into();
        self
    }

    pub fn with_transcript(mut self, transcript: impl Into<String>) -> Self {
        self.metadata.transcript = transcript.into();
        self
    }

    pub fn with_type(mut self, r#type: impl Into<Option<DocumentType>>) -> Self {
        self.metadata.r#type = r#type.into();
        self
    }

    pub fn with_folder(mut self, folder_id: impl Into<Option<Uuid>>) -> Self {
        self.metadata.folder_id = folder_id.into();
        self
    }

    pub fn with_attributes(mut self, attributes: impl Into<BTreeMap<String, Attribute>>) -> Self {
        self.metadata.attributes = attributes.into();
        self
    }

    pub fn build(self) -> Metadata {
        self.metadata
    }
}

/// Builds a [`Document`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(any(test, feature = "testing"))]
pub struct FakeDocument {
    document: Document,
}

#[cfg(any(test, feature = "testing"))]
impl Default for FakeDocument {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl FakeDocument {
    pub fn new() -> Self {
        let id = Uuid::now_v7();
        Self {
            document: Document {
                id,
                content: crate::tools::testing::random_bytes(64),
                metadata: FakeMetadata::new().with_id(id).build(),
            },
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.document.id = id;
        self.document.metadata.id = id;
        self
    }

    pub fn with_content(mut self, content: Vec<u8>) -> Self {
        self.document.content = content;
        self
    }

    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.document.metadata = metadata;
        self
    }

    pub fn build(self) -> Document {
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
                folders: vec![FacetCount {
                    value: Uuid::now_v7(),
                    count: count(),
                }],
                unfiled: count(),
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

    pub(crate) fn with_folders(mut self, folders: Vec<FacetCount<Uuid>>, unfiled: u64) -> Self {
        self.facets.folders = folders;
        self.facets.unfiled = unfiled;
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

/// The embeddings of a document's text, one per chunk, as stored beside
/// the document (see [`super::Indexer`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DocumentEmbeddings {
    /// The model the vectors come from (see `Embedder::model`).
    pub(crate) model: String,
    pub(crate) chunks: Vec<ChunkEmbedding>,
}

/// The embedding of one chunk of a document's text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChunkEmbedding {
    /// The chunk's byte range in the document's transcript.
    pub(crate) range: std::ops::Range<usize>,
    pub(crate) vector: Vec<f32>,
}

/// Builds a [`DocumentEmbeddings`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeDocumentEmbeddings {
    embeddings: DocumentEmbeddings,
}

#[cfg(test)]
impl FakeDocumentEmbeddings {
    pub(crate) fn new() -> Self {
        let count = 1 + crate::tools::testing::random_u64(3) as usize;
        Self {
            embeddings: DocumentEmbeddings {
                model: format!("{}.onnx", crate::tools::testing::random_word()),
                chunks: (0..count)
                    .map(|i| {
                        FakeChunkEmbedding::new()
                            .with_range(i * 100..i * 100 + 90)
                            .build()
                    })
                    .collect(),
            },
        }
    }

    pub(crate) fn with_model(mut self, model: impl Into<String>) -> Self {
        self.embeddings.model = model.into();
        self
    }

    pub(crate) fn with_chunks(mut self, chunks: Vec<ChunkEmbedding>) -> Self {
        self.embeddings.chunks = chunks;
        self
    }

    pub(crate) fn build(self) -> DocumentEmbeddings {
        self.embeddings
    }
}

/// Builds a [`ChunkEmbedding`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeChunkEmbedding {
    chunk: ChunkEmbedding,
}

#[cfg(test)]
impl FakeChunkEmbedding {
    pub(crate) fn new() -> Self {
        let start = crate::tools::testing::random_u64(1000) as usize;
        Self {
            chunk: ChunkEmbedding {
                range: start..start + 1 + crate::tools::testing::random_u64(1000) as usize,
                vector: (0..super::embedder::DIMENSIONS)
                    .map(|_| crate::tools::testing::random_u64(2001) as f32 / 1000.0 - 1.0)
                    .collect(),
            },
        }
    }

    pub(crate) fn with_range(mut self, range: std::ops::Range<usize>) -> Self {
        self.chunk.range = range;
        self
    }

    pub(crate) fn with_vector(mut self, vector: Vec<f32>) -> Self {
        self.chunk.vector = vector;
        self
    }

    pub(crate) fn build(self) -> ChunkEmbedding {
        self.chunk
    }
}

/// Identifies what a document's embeddings were computed from: they can
/// only stand in for the ones computed from the same key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddingKey {
    pub(crate) document_id: Uuid,
    /// See `Embedder::model`.
    pub(crate) model: String,
    pub(crate) chunker_version: u32,
    /// The SHA-256 of the passages embedded, never the text itself.
    pub(crate) text_sha256: [u8; 32],
}

/// A document's embeddings computed at upload, kept for the changelog
/// consumer to reuse (see [`super::EmbeddingCache`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CachedEmbeddings {
    pub(crate) key: EmbeddingKey,
    pub(crate) embeddings: DocumentEmbeddings,
}

/// Builds a [`CachedEmbeddings`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeCachedEmbeddings {
    cached: CachedEmbeddings,
}

#[cfg(test)]
impl FakeCachedEmbeddings {
    pub(crate) fn new() -> Self {
        let embeddings = FakeDocumentEmbeddings::new().build();
        Self {
            cached: CachedEmbeddings {
                key: EmbeddingKey {
                    document_id: Uuid::now_v7(),
                    model: embeddings.model.clone(),
                    chunker_version: 1,
                    text_sha256: std::array::from_fn(|_| {
                        crate::tools::testing::random_u64(256) as u8
                    }),
                },
                embeddings,
            },
        }
    }

    /// Keys the entry as the upload of `metadata` embedded with `model`
    /// would.
    pub(crate) fn for_metadata(mut self, metadata: &Metadata, model: &str) -> Self {
        use super::service::{CHUNKER_VERSION, embedding_key, passages};

        self.cached.key = embedding_key(
            metadata.id,
            model,
            CHUNKER_VERSION,
            &passages(metadata).texts,
        );
        self.cached.embeddings.model = model.to_string();
        self
    }

    pub(crate) fn with_key(mut self, key: EmbeddingKey) -> Self {
        self.cached.key = key;
        self
    }

    pub(crate) fn with_document_id(mut self, id: Uuid) -> Self {
        self.cached.key.document_id = id;
        self
    }

    pub(crate) fn with_embeddings(mut self, embeddings: DocumentEmbeddings) -> Self {
        self.cached.embeddings = embeddings;
        self
    }

    pub(crate) fn build(self) -> CachedEmbeddings {
        self.cached
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
        let folder_id = Uuid::now_v7();
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
            .with_folder(folder_id)
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
        assert_eq!(metadata.folder_id, Some(folder_id));
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

        assert!(!facets.folders.is_empty());
        assert!(facets.folders.iter().all(|facet| facet.count > 0));
        assert!(facets.earliest_created_at <= facets.latest_created_at);
    }

    #[test]
    fn fake_search_facets_with_methods_override_defaults() {
        let count = |value: &str| FacetCount {
            value: value.to_string(),
            count: 1,
        };
        let folder_id = Uuid::now_v7();
        let facets = FakeSearchFacets::new()
            .with_folders(
                vec![FacetCount {
                    value: folder_id,
                    count: 2,
                }],
                3,
            )
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
                folders: vec![FacetCount {
                    value: folder_id,
                    count: 2
                }],
                unfiled: 3,
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
