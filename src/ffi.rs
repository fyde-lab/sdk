//! Foreign-function interface exposing the SDK to non-Rust callers (currently
//! the Kotlin Multiplatform `application`) via UniFFI-generated bindings.
//!
//! [`FydeClient`] mirrors [`Client`] plus its users, sessions, server
//! state, documents and scripts operations; domain types are mirrored as
//! `Ffi*` records/enums since UniFFI has no native `Uuid` type and cannot
//! export the crate's own [`Document`]/[`ChangelogEvent`] types directly.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};

use uuid::Uuid;

use crate::domains::documents::{
    Attribute, AttributeDefinition, AttributeType, Category, DocumentType, EventType, FacetCount,
    Metadata, SearchFacets, SearchFilters, SearchOrder, SearchQuery, SetAttributeRequest,
    Subcategory, UploadRequest,
};
use crate::domains::scripts::{
    InstallScriptRequest, InstalledScript, Script, ScriptMetadata, ScriptParameter,
    ScriptParameterType, ScriptType,
};
use crate::domains::secrets::SecretLocation;
use crate::domains::sessions::Service as SessionsService;
use crate::domains::users::system_language;
use crate::{
    ChangelogEvent, Client, ClientConfig, Document, Error, ErrorCode, ErrorContext as _, LogLevel,
    LoginOutcome, OpenSession, ProgressEvent, ProgressStep, Storage, Usage,
};

macro_rules! define_ffi_error {
    ($($(#[$meta:meta])* $name:ident = $value:literal,)*) => {
        /// Error type surfaced to FFI callers, mirroring [`Error`]: one
        /// variant per [`ErrorCode`] (so Kotlin can catch e.g.
        /// `FfiException.InvalidCredentials` specifically), each carrying
        /// the failure's user-facing wording in the user's selected
        /// language (see [`Client::user_message`]) as its message. A flat
        /// error, so Kotlin's `Exception.message` is exactly that wording —
        /// a record-style error would instead get a generated
        /// `"field=value, ..."` message. The technical description never
        /// crosses the boundary; it's logged (and so reaches
        /// [`FfiClientConfig`]'s log listener) instead.
        #[derive(Debug, thiserror::Error, uniffi::Error)]
        #[uniffi(flat_error)]
        pub enum FfiError {
            $($(#[$meta])* #[error("{0}")] $name(String),)*
        }

        impl FfiError {
            fn new(code: ErrorCode, user_message: &str) -> Self {
                match code {
                    $(ErrorCode::$name => FfiError::$name(user_message.to_string()),)*
                }
            }

            #[cfg(test)]
            fn code(&self) -> ErrorCode {
                match self {
                    $(FfiError::$name(_) => ErrorCode::$name,)*
                }
            }
        }
    };
}
crate::error::for_each_error_code!(define_ffi_error);

impl FfiError {
    /// Converts `err` for an FFI caller, worded in `language`, logging its
    /// technical description first since that's otherwise lost here.
    fn from_error(err: &Error, language: &str) -> Self {
        tracing::warn!(code = %err.code(), "{err}");
        Self::new(err.code(), err.user_message(language))
    }
}

fn parse_uuid(id: &str) -> crate::Result<Uuid> {
    Uuid::parse_str(id).with_context(|| format!("invalid id {id:?}"))
}

/// The part of life a document is about, mirroring [`Category`] across the
/// FFI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCategory {
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

impl From<Category> for FfiCategory {
    fn from(category: Category) -> Self {
        match category {
            Category::Housing => Self::Housing,
            Category::Telecom => Self::Telecom,
            Category::Money => Self::Money,
            Category::Taxes => Self::Taxes,
            Category::Health => Self::Health,
            Category::Work => Self::Work,
            Category::Family => Self::Family,
            Category::Transport => Self::Transport,
            Category::Shopping => Self::Shopping,
            Category::Subscriptions => Self::Subscriptions,
            Category::Donations => Self::Donations,
        }
    }
}

impl From<FfiCategory> for Category {
    fn from(category: FfiCategory) -> Self {
        match category {
            FfiCategory::Housing => Self::Housing,
            FfiCategory::Telecom => Self::Telecom,
            FfiCategory::Money => Self::Money,
            FfiCategory::Taxes => Self::Taxes,
            FfiCategory::Health => Self::Health,
            FfiCategory::Work => Self::Work,
            FfiCategory::Family => Self::Family,
            FfiCategory::Transport => Self::Transport,
            FfiCategory::Shopping => Self::Shopping,
            FfiCategory::Subscriptions => Self::Subscriptions,
            FfiCategory::Donations => Self::Donations,
        }
    }
}

/// A finer-grained classification within a document's [`FfiCategory`],
/// mirroring [`Subcategory`] across the FFI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSubcategory {
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

impl From<Subcategory> for FfiSubcategory {
    fn from(subcategory: Subcategory) -> Self {
        match subcategory {
            Subcategory::Energy => Self::Energy,
            Subcategory::Water => Self::Water,
            Subcategory::Internet => Self::Internet,
            Subcategory::HomeInsurance => Self::HomeInsurance,
            Subcategory::HomeServices => Self::HomeServices,
            Subcategory::Mobile => Self::Mobile,
            Subcategory::Banking => Self::Banking,
            Subcategory::Savings => Self::Savings,
            Subcategory::Gaming => Self::Gaming,
            Subcategory::IncomeTax => Self::IncomeTax,
            Subcategory::TaxCertificates => Self::TaxCertificates,
            Subcategory::HealthInsurance => Self::HealthInsurance,
            Subcategory::ComplementaryHealth => Self::ComplementaryHealth,
            Subcategory::Care => Self::Care,
            Subcategory::Payslips => Self::Payslips,
            Subcategory::Employment => Self::Employment,
            Subcategory::HouseholdEmployer => Self::HouseholdEmployer,
            Subcategory::Benefits => Self::Benefits,
            Subcategory::ChildcareSchool => Self::ChildcareSchool,
            Subcategory::Train => Self::Train,
            Subcategory::Flight => Self::Flight,
            Subcategory::Road => Self::Road,
            Subcategory::Ride => Self::Ride,
            Subcategory::Travel => Self::Travel,
            Subcategory::Online => Self::Online,
            Subcategory::Pets => Self::Pets,
            Subcategory::Groceries => Self::Groceries,
            Subcategory::FoodDelivery => Self::FoodDelivery,
            Subcategory::Media => Self::Media,
            Subcategory::CloudSoftware => Self::CloudSoftware,
        }
    }
}

impl From<FfiSubcategory> for Subcategory {
    fn from(subcategory: FfiSubcategory) -> Self {
        match subcategory {
            FfiSubcategory::Energy => Self::Energy,
            FfiSubcategory::Water => Self::Water,
            FfiSubcategory::Internet => Self::Internet,
            FfiSubcategory::HomeInsurance => Self::HomeInsurance,
            FfiSubcategory::HomeServices => Self::HomeServices,
            FfiSubcategory::Mobile => Self::Mobile,
            FfiSubcategory::Banking => Self::Banking,
            FfiSubcategory::Savings => Self::Savings,
            FfiSubcategory::Gaming => Self::Gaming,
            FfiSubcategory::IncomeTax => Self::IncomeTax,
            FfiSubcategory::TaxCertificates => Self::TaxCertificates,
            FfiSubcategory::HealthInsurance => Self::HealthInsurance,
            FfiSubcategory::ComplementaryHealth => Self::ComplementaryHealth,
            FfiSubcategory::Care => Self::Care,
            FfiSubcategory::Payslips => Self::Payslips,
            FfiSubcategory::Employment => Self::Employment,
            FfiSubcategory::HouseholdEmployer => Self::HouseholdEmployer,
            FfiSubcategory::Benefits => Self::Benefits,
            FfiSubcategory::ChildcareSchool => Self::ChildcareSchool,
            FfiSubcategory::Train => Self::Train,
            FfiSubcategory::Flight => Self::Flight,
            FfiSubcategory::Road => Self::Road,
            FfiSubcategory::Ride => Self::Ride,
            FfiSubcategory::Travel => Self::Travel,
            FfiSubcategory::Online => Self::Online,
            FfiSubcategory::Pets => Self::Pets,
            FfiSubcategory::Groceries => Self::Groceries,
            FfiSubcategory::FoodDelivery => Self::FoodDelivery,
            FfiSubcategory::Media => Self::Media,
            FfiSubcategory::CloudSoftware => Self::CloudSoftware,
        }
    }
}

/// What kind of paper a document is, mirroring [`DocumentType`] across the
/// FFI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDocumentType {
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

impl From<DocumentType> for FfiDocumentType {
    fn from(r#type: DocumentType) -> Self {
        match r#type {
            DocumentType::Invoice => Self::Invoice,
            DocumentType::Receipt => Self::Receipt,
            DocumentType::Statement => Self::Statement,
            DocumentType::ReimbursementStatement => Self::ReimbursementStatement,
            DocumentType::Payslip => Self::Payslip,
            DocumentType::Contract => Self::Contract,
            DocumentType::InsuranceCertificate => Self::InsuranceCertificate,
            DocumentType::InsuranceCard => Self::InsuranceCard,
            DocumentType::RightsCertificate => Self::RightsCertificate,
            DocumentType::ProofOfResidence => Self::ProofOfResidence,
            DocumentType::Rib => Self::Rib,
            DocumentType::TaxNotice => Self::TaxNotice,
            DocumentType::TaxReturn => Self::TaxReturn,
            DocumentType::TaxCertificate => Self::TaxCertificate,
            DocumentType::EmploymentCertificate => Self::EmploymentCertificate,
            DocumentType::FeeSummary => Self::FeeSummary,
            DocumentType::Booking => Self::Booking,
            DocumentType::MedicalDocument => Self::MedicalDocument,
            DocumentType::Other => Self::Other,
        }
    }
}

impl From<FfiDocumentType> for DocumentType {
    fn from(r#type: FfiDocumentType) -> Self {
        match r#type {
            FfiDocumentType::Invoice => Self::Invoice,
            FfiDocumentType::Receipt => Self::Receipt,
            FfiDocumentType::Statement => Self::Statement,
            FfiDocumentType::ReimbursementStatement => Self::ReimbursementStatement,
            FfiDocumentType::Payslip => Self::Payslip,
            FfiDocumentType::Contract => Self::Contract,
            FfiDocumentType::InsuranceCertificate => Self::InsuranceCertificate,
            FfiDocumentType::InsuranceCard => Self::InsuranceCard,
            FfiDocumentType::RightsCertificate => Self::RightsCertificate,
            FfiDocumentType::ProofOfResidence => Self::ProofOfResidence,
            FfiDocumentType::Rib => Self::Rib,
            FfiDocumentType::TaxNotice => Self::TaxNotice,
            FfiDocumentType::TaxReturn => Self::TaxReturn,
            FfiDocumentType::TaxCertificate => Self::TaxCertificate,
            FfiDocumentType::EmploymentCertificate => Self::EmploymentCertificate,
            FfiDocumentType::FeeSummary => Self::FeeSummary,
            FfiDocumentType::Booking => Self::Booking,
            FfiDocumentType::MedicalDocument => Self::MedicalDocument,
            FfiDocumentType::Other => Self::Other,
        }
    }
}

/// A document's cleartext metadata, mirroring [`Metadata`] across the FFI
/// boundary.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMetadata {
    pub id: String,
    pub name: String,
    pub original_name: String,
    pub content_type: String,
    pub created_at: i64,
    pub size: u64,
    pub checksum: String,
    pub transcript: String,
    pub r#type: Option<FfiDocumentType>,
    pub category: Option<FfiCategory>,
    pub subcategory: Option<FfiSubcategory>,
    pub attributes: HashMap<String, FfiAttribute>,
}

/// One typed value of a document's attributes, mirroring [`Attribute`]
/// across the FFI boundary.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiAttribute {
    String {
        value: String,
    },
    Integer {
        value: i64,
    },
    Float {
        value: f64,
    },
    Boolean {
        value: bool,
    },
    /// An ISO 8601 `YYYY-MM-DD` calendar date.
    Date {
        value: String,
    },
    /// `minor_units` of `currency`'s ISO 4217 minor unit (cents for `"EUR"`).
    Amount {
        minor_units: i64,
        currency: String,
    },
    /// `selected`, always one of `options`.
    Select {
        selected: String,
        options: Vec<String>,
    },
}

impl From<Attribute> for FfiAttribute {
    fn from(attribute: Attribute) -> Self {
        match attribute {
            Attribute::String(value) => Self::String { value },
            Attribute::Integer(value) => Self::Integer { value },
            Attribute::Float(value) => Self::Float { value },
            Attribute::Boolean(value) => Self::Boolean { value },
            Attribute::Date(value) => Self::Date { value },
            Attribute::Amount {
                minor_units,
                currency,
            } => Self::Amount {
                minor_units,
                currency,
            },
            Attribute::Select { selected, options } => Self::Select { selected, options },
        }
    }
}

impl From<FfiAttribute> for Attribute {
    fn from(attribute: FfiAttribute) -> Self {
        match attribute {
            FfiAttribute::String { value } => Self::String(value),
            FfiAttribute::Integer { value } => Self::Integer(value),
            FfiAttribute::Float { value } => Self::Float(value),
            FfiAttribute::Boolean { value } => Self::Boolean(value),
            FfiAttribute::Date { value } => Self::Date(value),
            FfiAttribute::Amount {
                minor_units,
                currency,
            } => Self::Amount {
                minor_units,
                currency,
            },
            FfiAttribute::Select { selected, options } => Self::Select { selected, options },
        }
    }
}

/// The type an attribute's values must have, mirroring [`AttributeType`]
/// across the FFI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiAttributeType {
    String,
    Integer,
    Float,
    Boolean,
    Date,
    Amount,
    Select,
}

impl From<AttributeType> for FfiAttributeType {
    fn from(r#type: AttributeType) -> Self {
        match r#type {
            AttributeType::String => Self::String,
            AttributeType::Integer => Self::Integer,
            AttributeType::Float => Self::Float,
            AttributeType::Boolean => Self::Boolean,
            AttributeType::Date => Self::Date,
            AttributeType::Amount => Self::Amount,
            AttributeType::Select => Self::Select,
        }
    }
}

impl From<FfiAttributeType> for AttributeType {
    fn from(r#type: FfiAttributeType) -> Self {
        match r#type {
            FfiAttributeType::String => Self::String,
            FfiAttributeType::Integer => Self::Integer,
            FfiAttributeType::Float => Self::Float,
            FfiAttributeType::Boolean => Self::Boolean,
            FfiAttributeType::Date => Self::Date,
            FfiAttributeType::Amount => Self::Amount,
            FfiAttributeType::Select => Self::Select,
        }
    }
}

/// An attribute definition, mirroring [`AttributeDefinition`] across the
/// FFI boundary. `id` crosses as a string since UniFFI has no native UUID
/// type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAttributeDefinition {
    pub id: String,
    pub name: String,
    pub r#type: FfiAttributeType,
    pub description: String,
    pub is_default: bool,
}

impl From<AttributeDefinition> for FfiAttributeDefinition {
    fn from(definition: AttributeDefinition) -> Self {
        Self {
            id: definition.id().to_string(),
            name: definition.name().to_string(),
            r#type: definition.r#type().into(),
            description: definition.description().to_string(),
            is_default: definition.is_default(),
        }
    }
}

impl From<Metadata> for FfiMetadata {
    fn from(metadata: Metadata) -> Self {
        Self {
            id: metadata.id().to_string(),
            name: metadata.name().to_string(),
            original_name: metadata.original_name().to_string(),
            content_type: metadata.content_type().to_string(),
            created_at: metadata.created_at(),
            size: metadata.size(),
            checksum: metadata.checksum().to_string(),
            transcript: metadata.transcript().to_string(),
            r#type: metadata.r#type().map(FfiDocumentType::from),
            category: metadata.category().map(FfiCategory::from),
            subcategory: metadata.subcategory().map(FfiSubcategory::from),
            attributes: metadata
                .attributes()
                .iter()
                .map(|(name, attribute)| (name.clone(), attribute.clone().into()))
                .collect(),
        }
    }
}

impl TryFrom<FfiMetadata> for Metadata {
    type Error = Error;

    fn try_from(metadata: FfiMetadata) -> Result<Self, Self::Error> {
        Ok(Self {
            id: parse_uuid(&metadata.id)?,
            name: metadata.name,
            original_name: metadata.original_name,
            content_type: metadata.content_type,
            created_at: metadata.created_at,
            size: metadata.size,
            checksum: metadata.checksum,
            transcript: metadata.transcript,
            r#type: metadata.r#type.map(DocumentType::from),
            category: metadata.category.map(Category::from),
            subcategory: metadata.subcategory.map(Subcategory::from),
            attributes: metadata
                .attributes
                .into_iter()
                .map(|(name, attribute)| (name, attribute.into()))
                .collect(),
        })
    }
}

/// A document, mirroring [`Document`] across the FFI boundary. `id` crosses
/// as a string since UniFFI has no native UUID type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDocument {
    pub id: String,
    pub content: Vec<u8>,
    pub metadata: FfiMetadata,
}

impl From<Document> for FfiDocument {
    fn from(document: Document) -> Self {
        Self {
            id: document.id().to_string(),
            content: document.content().to_vec(),
            metadata: document.metadata().clone().into(),
        }
    }
}

/// Which documents a search keeps, mirroring [`SearchFilters`] across the
/// FFI boundary: any of the values within one list, every list that's
/// non-empty, and every bound that's set.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchFilters {
    pub categories: Vec<FfiCategory>,
    pub subcategories: Vec<FfiSubcategory>,
    pub types: Vec<FfiDocumentType>,
    pub content_types: Vec<String>,
    pub created_from: Option<i64>,
    pub created_until: Option<i64>,
}

impl From<FfiSearchFilters> for SearchFilters {
    fn from(filters: FfiSearchFilters) -> Self {
        Self {
            categories: filters.categories.into_iter().map(Category::from).collect(),
            subcategories: filters
                .subcategories
                .into_iter()
                .map(Subcategory::from)
                .collect(),
            types: filters.types.into_iter().map(DocumentType::from).collect(),
            content_types: filters.content_types,
            created_from: filters.created_from,
            created_until: filters.created_until,
        }
    }
}

/// The order search results come back in, mirroring [`SearchOrder`].
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiSearchOrder {
    Relevance,
    Newest,
    Oldest,
}

impl From<FfiSearchOrder> for SearchOrder {
    fn from(order: FfiSearchOrder) -> Self {
        match order {
            FfiSearchOrder::Relevance => Self::Relevance,
            FfiSearchOrder::Newest => Self::Newest,
            FfiSearchOrder::Oldest => Self::Oldest,
        }
    }
}

/// The signed-in account's plan and current usage, mirroring [`Usage`]
/// across the FFI boundary.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiUsage {
    pub plan_id: String,
    pub max_scripts: u32,
    pub max_sessions: u32,
    pub max_size: u64,
    pub enabled_scripts: u32,
    pub open_sessions: u32,
    pub total_size: u64,
}

impl From<Usage> for FfiUsage {
    fn from(usage: Usage) -> Self {
        Self {
            plan_id: usage.plan_id().to_string(),
            max_scripts: usage.max_scripts(),
            max_sessions: usage.max_sessions(),
            max_size: usage.max_size(),
            enabled_scripts: usage.enabled_scripts(),
            open_sessions: usage.open_sessions(),
            total_size: usage.total_size(),
        }
    }
}

/// How many documents carry one category, mirroring
/// `FacetCount<Category>` (UniFFI has no generic records).
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCategoryCount {
    pub value: FfiCategory,
    pub count: u64,
}

/// How many documents carry one subcategory, mirroring
/// `FacetCount<Subcategory>`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSubcategoryCount {
    pub value: FfiSubcategory,
    pub count: u64,
}

/// How many documents carry one type, mirroring `FacetCount<DocumentType>`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDocumentTypeCount {
    pub value: FfiDocumentType,
    pub count: u64,
}

/// How many documents carry one free-form value (a content type),
/// mirroring `FacetCount<String>`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiStringCount {
    pub value: String,
    pub count: u64,
}

/// The filter values still worth offering for a search, mirroring
/// [`SearchFacets`] across the FFI boundary.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchFacets {
    pub categories: Vec<FfiCategoryCount>,
    pub subcategories: Vec<FfiSubcategoryCount>,
    pub types: Vec<FfiDocumentTypeCount>,
    pub content_types: Vec<FfiStringCount>,
    pub earliest_created_at: Option<i64>,
    pub latest_created_at: Option<i64>,
}

impl From<SearchFacets> for FfiSearchFacets {
    fn from(facets: SearchFacets) -> Self {
        fn strings(counts: Vec<FacetCount<String>>) -> Vec<FfiStringCount> {
            counts
                .into_iter()
                .map(|facet| FfiStringCount {
                    value: facet.value,
                    count: facet.count,
                })
                .collect()
        }

        Self {
            categories: facets
                .categories
                .into_iter()
                .map(|facet| FfiCategoryCount {
                    value: facet.value.into(),
                    count: facet.count,
                })
                .collect(),
            subcategories: facets
                .subcategories
                .into_iter()
                .map(|facet| FfiSubcategoryCount {
                    value: facet.value.into(),
                    count: facet.count,
                })
                .collect(),
            types: facets
                .types
                .into_iter()
                .map(|facet| FfiDocumentTypeCount {
                    value: facet.value.into(),
                    count: facet.count,
                })
                .collect(),
            content_types: strings(facets.content_types),
            earliest_created_at: facets.earliest_created_at,
            latest_created_at: facets.latest_created_at,
        }
    }
}

/// What kind of script this is, mirroring [`ScriptType`].
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiScriptType {
    Scraper,
    Parser,
}

impl From<ScriptType> for FfiScriptType {
    fn from(script_type: ScriptType) -> Self {
        match script_type {
            ScriptType::Scraper => FfiScriptType::Scraper,
            ScriptType::Parser => FfiScriptType::Parser,
        }
    }
}

impl From<FfiScriptType> for ScriptType {
    fn from(script_type: FfiScriptType) -> Self {
        match script_type {
            FfiScriptType::Scraper => ScriptType::Scraper,
            FfiScriptType::Parser => ScriptType::Parser,
        }
    }
}

/// What kind of value a [`FfiScriptParameter`] expects, mirroring
/// [`ScriptParameterType`].
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiScriptParameterType {
    String,
    Number,
    Boolean,
}

impl From<ScriptParameterType> for FfiScriptParameterType {
    fn from(parameter_type: ScriptParameterType) -> Self {
        match parameter_type {
            ScriptParameterType::String => FfiScriptParameterType::String,
            ScriptParameterType::Number => FfiScriptParameterType::Number,
            ScriptParameterType::Boolean => FfiScriptParameterType::Boolean,
        }
    }
}

impl From<FfiScriptParameterType> for ScriptParameterType {
    fn from(parameter_type: FfiScriptParameterType) -> Self {
        match parameter_type {
            FfiScriptParameterType::String => ScriptParameterType::String,
            FfiScriptParameterType::Number => ScriptParameterType::Number,
            FfiScriptParameterType::Boolean => ScriptParameterType::Boolean,
        }
    }
}

/// A single configurable parameter a script declares it needs from the
/// user, mirroring [`ScriptParameter`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiScriptParameter {
    pub label: String,
    pub placeholder: String,
    pub parameter_type: FfiScriptParameterType,
    pub required: bool,
    pub secret: bool,
}

impl From<ScriptParameter> for FfiScriptParameter {
    fn from(parameter: ScriptParameter) -> Self {
        Self {
            label: parameter.label,
            placeholder: parameter.placeholder,
            parameter_type: parameter.parameter_type.into(),
            required: parameter.required,
            secret: parameter.secret,
        }
    }
}

impl From<FfiScriptParameter> for ScriptParameter {
    fn from(parameter: FfiScriptParameter) -> Self {
        Self {
            label: parameter.label,
            placeholder: parameter.placeholder,
            parameter_type: parameter.parameter_type.into(),
            required: parameter.required,
            secret: parameter.secret,
        }
    }
}

/// A user-authored script, mirroring [`Script`] across the FFI boundary.
/// `id` crosses as a string since UniFFI has no native UUID type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiScript {
    pub id: String,
    pub name: String,
    pub description: String,
    pub short_description: String,
    pub is_public: bool,
    pub icon: Vec<u8>,
    pub allowed_domains: Vec<String>,
    pub version: u64,
    pub script: String,
    pub last_updated: i64,
    pub script_type: FfiScriptType,
    pub parameters: HashMap<String, FfiScriptParameter>,
}

impl From<Script> for FfiScript {
    fn from(script: Script) -> Self {
        Self {
            id: script.id().to_string(),
            name: script.name().to_string(),
            description: script.description().to_string(),
            short_description: script.short_description().to_string(),
            is_public: script.is_public(),
            icon: script.icon().to_vec(),
            allowed_domains: script.allowed_domains().to_vec(),
            version: script.version(),
            script: script.script().to_string(),
            last_updated: script.last_updated(),
            script_type: script.script_type().into(),
            parameters: script
                .parameters()
                .iter()
                .map(|(key, parameter)| (key.clone(), parameter.clone().into()))
                .collect(),
        }
    }
}

/// A script's metadata — everything but its content — mirroring
/// [`ScriptMetadata`] across the FFI boundary. `id` crosses as a string
/// since UniFFI has no native UUID type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiScriptMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub short_description: String,
    pub is_public: bool,
    pub icon: Vec<u8>,
    pub allowed_domains: Vec<String>,
    pub version: u64,
    pub last_updated: i64,
    pub script_type: FfiScriptType,
    pub parameters: HashMap<String, FfiScriptParameter>,
}

impl From<ScriptMetadata> for FfiScriptMetadata {
    fn from(metadata: ScriptMetadata) -> Self {
        Self {
            id: metadata.id().to_string(),
            name: metadata.name().to_string(),
            description: metadata.description().to_string(),
            short_description: metadata.short_description().to_string(),
            is_public: metadata.is_public(),
            icon: metadata.icon().to_vec(),
            allowed_domains: metadata.allowed_domains().to_vec(),
            version: metadata.version(),
            last_updated: metadata.last_updated(),
            script_type: metadata.script_type().into(),
            parameters: metadata
                .parameters()
                .iter()
                .map(|(key, parameter)| (key.clone(), parameter.clone().into()))
                .collect(),
        }
    }
}

/// A script installed for the authenticated user, mirroring
/// [`InstalledScript`] across the FFI boundary. `parameters_json` is the
/// JSON object of values the script was installed with (e.g.
/// `{"username": "alice"}`), since UniFFI has no native JSON value type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiInstalledScript {
    pub script: FfiScript,
    pub parameters_json: String,
}

impl TryFrom<InstalledScript> for FfiInstalledScript {
    type Error = Error;

    fn try_from(installed: InstalledScript) -> Result<Self, Self::Error> {
        let parameters_json = serde_json::to_string(installed.parameters()).with_context(|| {
            format!(
                "failed to serialize parameters of installed script {}",
                installed.script().id()
            )
        })?;

        Ok(Self {
            script: installed.script().clone().into(),
            parameters_json,
        })
    }
}

/// The kind of write recorded by an [`FfiChangelogEvent`], mirroring
/// [`EventType`].
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiEventType {
    SaveDocument,
    UpdateMetadata,
    Deleted,
    InstallScript,
}

impl From<EventType> for FfiEventType {
    fn from(event_type: EventType) -> Self {
        match event_type {
            EventType::SaveDocument => FfiEventType::SaveDocument,
            EventType::UpdateMetadata => FfiEventType::UpdateMetadata,
            EventType::Deleted => FfiEventType::Deleted,
            EventType::InstallScript => FfiEventType::InstallScript,
        }
    }
}

/// A single recorded write, mirroring [`ChangelogEvent`] across the FFI
/// boundary. `subject_id` is a document or script id depending on
/// `event_type` (see [`ChangelogEvent`]). `id`/`subject_id` cross as strings since
/// UniFFI has no native UUID type. `content`/`metadata` are `None` for
/// event types that don't carry them.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiChangelogEvent {
    pub id: String,
    pub event_type: FfiEventType,
    pub subject_id: String,
    pub content: Option<Vec<u8>>,
    pub metadata: Option<FfiMetadata>,
}

impl From<ChangelogEvent> for FfiChangelogEvent {
    fn from(event: ChangelogEvent) -> Self {
        Self {
            id: event.id().to_string(),
            event_type: event.event_type().into(),
            subject_id: event.subject_id().to_string(),
            content: event.content().map(<[u8]>::to_vec),
            metadata: event.metadata().cloned().map(FfiMetadata::from),
        }
    }
}

/// One of an account's open sessions, mirroring [`OpenSession`] across the
/// FFI boundary.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiOpenSession {
    /// The id to pass back as `login`'s `replace_session_id`, or to
    /// `revoke_session`.
    pub id: String,
    pub device_name: String,
    /// Unix timestamp, in seconds, of when the session was opened.
    pub created_at: i64,
    /// Whether this device is signed in with this session. Always `false`
    /// in a `SessionLimitReached` login outcome.
    pub is_current: bool,
}

impl From<OpenSession> for FfiOpenSession {
    fn from(session: OpenSession) -> Self {
        Self {
            id: session.id().to_string(),
            device_name: session.device_name().to_string(),
            created_at: session.created_at(),
            is_current: session.is_current(),
        }
    }
}

/// The outcome of [`FydeClient::login`], mirroring [`LoginOutcome`] across
/// the FFI boundary.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiLoginOutcome {
    /// A session was opened.
    LoggedIn { session_token: String },
    /// The account's plan allows no more open sessions, so none was opened:
    /// show `sessions` and let the user pick one to close, then call
    /// `login` again with its id as `replace_session_id`.
    SessionLimitReached {
        max_sessions: u32,
        sessions: Vec<FfiOpenSession>,
    },
}

impl From<LoginOutcome> for FfiLoginOutcome {
    fn from(outcome: LoginOutcome) -> Self {
        match outcome {
            LoginOutcome::LoggedIn { session_token } => Self::LoggedIn { session_token },
            LoginOutcome::SessionLimitReached {
                max_sessions,
                sessions,
            } => Self::SessionLimitReached {
                max_sessions,
                sessions: sessions.into_iter().map(Into::into).collect(),
            },
        }
    }
}

/// Which SQLite backend a [`FydeClient`] persists local state to, mirroring
/// [`Storage`].
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiStorage {
    Disk { path: String },
    Memory,
}

impl From<FfiStorage> for Storage {
    fn from(storage: FfiStorage) -> Self {
        match storage {
            FfiStorage::Disk { path } => Storage::Disk(path.into()),
            FfiStorage::Memory => Storage::Memory,
        }
    }
}

/// The verbosity of the SDK's internal logs, mirroring [`LogLevel`].
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiLogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<FfiLogLevel> for LogLevel {
    fn from(level: FfiLogLevel) -> Self {
        match level {
            FfiLogLevel::Off => LogLevel::Off,
            FfiLogLevel::Error => LogLevel::Error,
            FfiLogLevel::Warn => LogLevel::Warn,
            FfiLogLevel::Info => LogLevel::Info,
            FfiLogLevel::Debug => LogLevel::Debug,
            FfiLogLevel::Trace => LogLevel::Trace,
        }
    }
}

impl From<LogLevel> for FfiLogLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Off => FfiLogLevel::Off,
            LogLevel::Error => FfiLogLevel::Error,
            LogLevel::Warn => FfiLogLevel::Warn,
            LogLevel::Info => FfiLogLevel::Info,
            LogLevel::Debug => FfiLogLevel::Debug,
            LogLevel::Trace => FfiLogLevel::Trace,
        }
    }
}

/// Callback interface for [`FfiClientConfig::on_log`], implemented by
/// foreign (Kotlin/Swift) callers and invoked once per log line emitted by
/// the SDK.
#[uniffi::export(callback_interface)]
pub trait LogListener: Send + Sync {
    fn on_log(&self, level: FfiLogLevel, line: String);
}

/// Configuration for [`FydeClient::init`], mirroring [`ClientConfig`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiClientConfig {
    pub url: String,
    pub client_name: String,
    pub storage: FfiStorage,
    pub log_level: FfiLogLevel,
}

impl From<FfiClientConfig> for ClientConfig {
    fn from(config: FfiClientConfig) -> Self {
        Self {
            url: config.url,
            client_name: config.client_name,
            storage: config.storage.into(),
            log_level: config.log_level.into(),
            on_log: None,
            on_document_change: None,
            // Set by `FydeClient::init` from its `scraper_listener`.
            on_scraper_progress: None,
            on_scraper_question: None,
        }
    }
}

/// Callback interface for [`FydeClient::init`]'s `on_document_change`
/// parameter, implemented by foreign (Kotlin/Swift) callers and invoked once
/// per [`FfiChangelogEvent`] the SDK's automatic changelog sync consumes
/// (started internally by `create_user`/`login`, stopped by `logout`).
#[uniffi::export(callback_interface)]
pub trait DocumentChangeListener: Send + Sync {
    fn on_event(&self, event: FfiChangelogEvent);
}

/// A high-level stage of a scraper run, mirroring [`ProgressStep`] across
/// the FFI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProgressStep {
    LoggingIn,
    CheckingSession,
    WaitingForVerificationCode,
    WaitingForCaptcha,
    ListingDocuments,
    DownloadingDocuments,
    Done,
}

impl From<ProgressStep> for FfiProgressStep {
    fn from(step: ProgressStep) -> Self {
        match step {
            ProgressStep::LoggingIn => Self::LoggingIn,
            ProgressStep::CheckingSession => Self::CheckingSession,
            ProgressStep::WaitingForVerificationCode => Self::WaitingForVerificationCode,
            ProgressStep::WaitingForCaptcha => Self::WaitingForCaptcha,
            ProgressStep::ListingDocuments => Self::ListingDocuments,
            ProgressStep::DownloadingDocuments => Self::DownloadingDocuments,
            ProgressStep::Done => Self::Done,
        }
    }
}

/// A scraper run's progress, mirroring [`ProgressEvent`]: a step starting
/// — with `message`, its wording in the user's language (see
/// [`ProgressStep::user_message`]) — or how far the current step has got.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProgressEvent {
    Step {
        step: FfiProgressStep,
        message: String,
    },
    Update {
        current: i64,
        total: i64,
    },
}

impl FfiProgressEvent {
    /// Converts `event`, wording its step (if any) in `language`.
    fn localized(event: ProgressEvent, language: &str) -> Self {
        match event {
            ProgressEvent::Step { step } => Self::Step {
                step: step.into(),
                message: step.user_message(language).to_string(),
            },
            ProgressEvent::Update { current, total } => Self::Update { current, total },
        }
    }
}

/// Callback interface for [`FydeClient::init`]'s `scraper_listener`,
/// implemented by foreign (Kotlin/Swift) callers so a scraper run started by
/// [`FydeClient::run_installed_script`] can show its progress and ask the
/// user questions (a one-time code, ...) on screen. Both are invoked from
/// the run's own blocking thread, never the caller's: `on_question` may
/// block until the user answers, and returns their answer to the script.
#[uniffi::export(callback_interface)]
pub trait ScraperListener: Send + Sync {
    fn on_progress(&self, event: FfiProgressEvent);
    fn on_question(&self, question: String) -> String;
}

/// A connection to a fyde server, exposed to Kotlin as `FydeClient`.
#[derive(uniffi::Object)]
pub struct FydeClient {
    inner: Client,
    /// The language scraper progress steps are worded in for
    /// `init`'s `scraper_listener`, refreshed from [`Client::language`]
    /// at the start of each [`Self::run_installed_script`]. The progress
    /// callback runs synchronously on the run's blocking thread, so it
    /// can't await the user's preference itself.
    scraper_language: Arc<RwLock<String>>,
}

impl FydeClient {
    /// Awaits `operation`, converting its error (if any) to an [`FfiError`]
    /// worded in the user's selected language. Every exported method's body
    /// runs through this, so none can surface an error untranslated.
    async fn localized<T>(
        &self,
        operation: impl Future<Output = crate::Result<T>>,
    ) -> Result<T, FfiError> {
        match operation.await {
            Ok(value) => Ok(value),
            Err(err) => {
                let message = self.inner.user_message(&err).await;
                tracing::warn!(code = %err.code(), "{err}");
                Err(FfiError::new(err.code(), message))
            }
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl FydeClient {
    /// Connects to a fyde server per `config`. Mirrors [`Client::init`].
    /// Fails worded in the device's system language, since the user's own
    /// preference lives in the local database this is what opens.
    ///
    /// Needs `async_runtime = "tokio"`: `Client::init` goes through sqlx's
    /// connection pool, which requires an active Tokio context to spawn its
    /// background tasks on — not just a generic future poller.
    #[uniffi::constructor(async_runtime = "tokio")]
    pub async fn init(
        config: FfiClientConfig,
        on_log: Option<Box<dyn LogListener>>,
        on_document_change: Option<Box<dyn DocumentChangeListener>>,
        scraper_listener: Option<Box<dyn ScraperListener>>,
    ) -> Result<Arc<Self>, FfiError> {
        let mut config: ClientConfig = config.into();
        config.on_log = on_log.map(|listener| -> Arc<dyn Fn(LogLevel, String) + Send + Sync> {
            let listener: Arc<dyn LogListener> = Arc::from(listener);
            Arc::new(move |level, line| listener.on_log(level.into(), line))
        });
        config.on_document_change =
            on_document_change.map(|listener| -> Arc<dyn Fn(ChangelogEvent) + Send + Sync> {
                let listener: Arc<dyn DocumentChangeListener> = Arc::from(listener);
                Arc::new(move |event| listener.on_event(event.into()))
            });
        let scraper_language = Arc::new(RwLock::new(system_language()));
        if let Some(listener) = scraper_listener {
            let listener: Arc<dyn ScraperListener> = Arc::from(listener);
            let for_progress = listener.clone();
            let language = scraper_language.clone();
            config.on_scraper_progress = Some(Arc::new(move |event: ProgressEvent| {
                let language = language.read().unwrap_or_else(PoisonError::into_inner);
                for_progress.on_progress(FfiProgressEvent::localized(event, &language))
            }));
            config.on_scraper_question = Some(Arc::new(move |question: String| {
                listener.on_question(question)
            }));
        }

        match Client::init(config).await {
            Ok(inner) => Ok(Arc::new(Self {
                inner,
                scraper_language,
            })),
            Err(err) => Err(FfiError::from_error(&err, &system_language())),
        }
    }

    /// Creates a new account and opens a session for the device named
    /// `device_name`, returning its session token. `allow_insecure_storage`
    /// lets the account's secrets fall back to the local database on a
    /// device without secure storage (see `is_secure_storage_available`).
    /// Mirrors [`crate::UsersService::create`].
    pub async fn create_user(
        &self,
        username: String,
        password: String,
        device_name: String,
        allow_insecure_storage: bool,
    ) -> Result<String, FfiError> {
        self.localized(self.inner.users().create(
            &username,
            &password,
            &device_name,
            allow_insecure_storage,
        ))
        .await
    }

    /// Returns whether this device has a usable OS credential store for the
    /// account's secrets; when it doesn't, `create_user` needs
    /// `allow_insecure_storage`. Mirrors
    /// [`crate::UsersService::is_secure_storage_available`].
    pub async fn is_secure_storage_available(&self) -> bool {
        self.inner.users().is_secure_storage_available().await
    }

    /// Verifies `username`/`password` and opens a session for the device
    /// named `device_name`, returning its session token — or, if the
    /// account's plan allows no more open sessions, the sessions to pick
    /// one from to close, passed back here as `replace_session_id`.
    /// `allow_insecure_storage` lets the session's secrets fall back to the
    /// local database on a device without secure storage, as for
    /// `create_user`. Mirrors [`crate::UsersService::login`].
    pub async fn login(
        &self,
        username: String,
        password: String,
        device_name: String,
        allow_insecure_storage: bool,
        replace_session_id: Option<String>,
    ) -> Result<FfiLoginOutcome, FfiError> {
        self.localized(self.inner.users().login(
            &username,
            &password,
            &device_name,
            allow_insecure_storage,
            replace_session_id.as_deref(),
        ))
        .await
        .map(Into::into)
    }

    /// Closes the session opened by the most recent `create_user`/`login`
    /// call. Mirrors [`crate::UsersService::logout`].
    pub async fn logout(&self) -> Result<(), FfiError> {
        self.localized(self.inner.users().logout()).await
    }

    /// Lists every session open on the signed-in account, oldest first,
    /// flagging the one this device uses. Mirrors
    /// [`crate::UsersService::list_sessions`].
    pub async fn list_sessions(&self) -> Result<Vec<FfiOpenSession>, FfiError> {
        self.localized(async {
            let sessions = self.inner.users().list_sessions().await?;
            Ok(sessions.into_iter().map(Into::into).collect())
        })
        .await
    }

    /// Closes one of the account's sessions, logging its device out.
    /// Mirrors [`crate::UsersService::revoke_session`].
    pub async fn revoke_session(&self, session_id: String) -> Result<(), FfiError> {
        self.localized(self.inner.users().revoke_session(&session_id))
            .await
    }

    /// Persists `token` as the session token attached to every subsequent
    /// authenticated request, overwriting any token already stored.
    /// `create_user`/`login` already do this as part of opening a session;
    /// this is for restoring a previously-issued token (e.g. one a caller
    /// kept in secure storage across app restarts) without a network round
    /// trip. Mirrors `sessions::Service::save_new_session`.
    pub async fn save_new_session(&self, token: String) -> Result<(), FfiError> {
        self.localized(
            self.inner
                .sessions()
                .save_new_session(&token, SecretLocation::SecureStorage),
        )
        .await
    }

    /// Returns whether a session token is currently persisted, i.e. a
    /// `create_user`/`login`/`save_new_session` call succeeded and `logout`
    /// hasn't been called since. Mirrors `sessions::Service::is_connected`.
    pub async fn is_connected(&self) -> Result<bool, FfiError> {
        self.localized(self.inner.sessions().is_connected()).await
    }

    /// Fetches the signed-in account's plan, limits and current usage.
    /// Mirrors [`crate::UsageService::get_usage`].
    pub async fn get_usage(&self) -> Result<FfiUsage, FfiError> {
        self.localized(async { Ok(FfiUsage::from(self.inner.usage().get_usage().await?)) })
            .await
    }

    /// Returns whether the fyde server currently responds as reachable.
    /// Mirrors [`crate::ServerStateService::is_server_reachable`].
    pub async fn is_server_reachable(&self) -> Result<bool, FfiError> {
        self.localized(self.inner.server_state().is_server_reachable())
            .await
    }

    /// Reads the local file at `path`, encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its generated
    /// id. Mirrors [`crate::DocumentsService::upload`].
    pub async fn upload_document(&self, path: String) -> Result<String, FfiError> {
        self.localized(async {
            let id = self
                .inner
                .documents()
                .upload(UploadRequest::from_path(Path::new(&path)))
                .await?;
            Ok(id.to_string())
        })
        .await
    }

    /// Like [`Self::upload_document`], also setting `attributes` on the
    /// document. Mirrors
    /// [`crate::DocumentsService::upload`] with
    /// [`UploadRequest::attributes`].
    pub async fn upload_document_with_attributes(
        &self,
        path: String,
        attributes: HashMap<String, FfiAttribute>,
    ) -> Result<String, FfiError> {
        self.localized(async {
            let mut request = UploadRequest::from_path(Path::new(&path));
            request.attributes = attributes
                .into_iter()
                .map(|(name, attribute)| (name, attribute.into()))
                .collect();
            let id = self.inner.documents().upload(request).await?;
            Ok(id.to_string())
        })
        .await
    }

    /// Lists every attribute definition the signed-in user can use: the
    /// default ones, then its own. Mirrors [`crate::AttributesService::list`].
    pub async fn list_attributes(&self) -> Result<Vec<FfiAttributeDefinition>, FfiError> {
        self.localized(async {
            let definitions = self.inner.attributes().list().await?;
            Ok(definitions.into_iter().map(Into::into).collect())
        })
        .await
    }

    /// Defines (or redefines) an attribute for the signed-in user. Mirrors
    /// [`crate::AttributesService::set`].
    pub async fn set_attribute(
        &self,
        name: String,
        r#type: FfiAttributeType,
        description: String,
    ) -> Result<FfiAttributeDefinition, FfiError> {
        self.localized(async {
            let definition = self
                .inner
                .attributes()
                .set(SetAttributeRequest {
                    name,
                    r#type: r#type.into(),
                    description,
                })
                .await?;
            Ok(definition.into())
        })
        .await
    }

    /// Fetches a document previously cached locally by the SDK's automatic
    /// changelog sync (see [`FydeClient::init`]'s `on_document_change`), or
    /// `None` if it doesn't exist. Mirrors [`crate::DocumentsService::get`].
    pub async fn get_document(&self, id: String) -> Result<Option<FfiDocument>, FfiError> {
        self.localized(async {
            let id = parse_uuid(&id)?;
            let document = self.inner.documents().get(id).await?;
            Ok(document.map(FfiDocument::from))
        })
        .await
    }

    /// Lists documents previously cached locally, oldest first, one page at
    /// a time. Mirrors [`crate::DocumentsService::list`].
    pub async fn list_documents(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<FfiDocument>, FfiError> {
        self.localized(async {
            let documents = self.inner.documents().list(offset, limit).await?;
            Ok(documents.into_iter().map(FfiDocument::from).collect())
        })
        .await
    }

    /// Searches documents previously cached locally for `text`, keeping
    /// only those matching `filters`, one page at a time in `order`.
    /// Mirrors [`crate::DocumentsService::search`].
    pub async fn search_documents(
        &self,
        text: String,
        filters: FfiSearchFilters,
        order: FfiSearchOrder,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<FfiDocument>, FfiError> {
        self.localized(async {
            let documents = self
                .inner
                .documents()
                .search(SearchQuery {
                    text,
                    filters: filters.into(),
                    order: order.into(),
                    offset,
                    limit,
                })
                .await?;
            Ok(documents.into_iter().map(FfiDocument::from).collect())
        })
        .await
    }

    /// Lists the filter values still worth offering for a search of `text`
    /// narrowed by `filters` — each field's values ignoring that field's own
    /// filter. Mirrors [`crate::DocumentsService::search_facets`].
    pub async fn search_document_facets(
        &self,
        text: String,
        filters: FfiSearchFilters,
    ) -> Result<FfiSearchFacets, FfiError> {
        self.localized(async {
            let facets = self
                .inner
                .documents()
                .search_facets(&text, &filters.into())
                .await?;
            Ok(facets.into())
        })
        .await
    }

    /// Encrypts `metadata` as given and publishes it as an "update metadata"
    /// changelog event, replacing whatever metadata was previously
    /// associated with `metadata.id`. Mirrors
    /// [`crate::DocumentsService::update_metadata`].
    pub async fn update_document_metadata(&self, metadata: FfiMetadata) -> Result<(), FfiError> {
        self.localized(async {
            let metadata = Metadata::try_from(metadata)?;
            self.inner.documents().update_metadata(metadata).await
        })
        .await
    }

    /// Persists `language` as this device's UI language preference — also
    /// the language every later [`FfiError`] is worded in. Mirrors
    /// [`crate::UsersService::set_language`].
    pub async fn set_language(&self, language: String) -> Result<(), FfiError> {
        self.localized(self.inner.users().set_language(&language))
            .await
    }

    /// Returns this device's UI language preference, or `None` if never
    /// set. Mirrors [`crate::UsersService::get_language`].
    pub async fn get_language(&self) -> Result<Option<String>, FfiError> {
        self.localized(self.inner.users().get_language()).await
    }

    /// Creates a new script owned by the authenticated user, at version 1.
    /// Mirrors [`crate::ScriptsService::create_script`].
    #[allow(clippy::too_many_arguments)]
    pub async fn create_script(
        &self,
        name: String,
        is_public: bool,
        icon: Vec<u8>,
        script: String,
        description: String,
        short_description: String,
        allowed_domains: Vec<String>,
        script_type: FfiScriptType,
        parameters: HashMap<String, FfiScriptParameter>,
    ) -> Result<FfiScript, FfiError> {
        self.localized(async {
            let script = self
                .inner
                .scripts()
                .create_script(
                    &name,
                    is_public,
                    icon,
                    &script,
                    &description,
                    &short_description,
                    allowed_domains,
                    script_type.into(),
                    parameters
                        .into_iter()
                        .map(|(key, parameter)| (key, parameter.into()))
                        .collect(),
                )
                .await?;
            Ok(script.into())
        })
        .await
    }

    /// Fetches the script matching `id`. Mirrors
    /// [`crate::ScriptsService::fetch_script`].
    pub async fn fetch_script(&self, id: String) -> Result<FfiScript, FfiError> {
        self.localized(async {
            let id = parse_uuid(&id)?;
            let script = self.inner.scripts().fetch_script(id).await?;
            Ok(script.into())
        })
        .await
    }

    /// Updates a script owned by the authenticated user, incrementing its
    /// version. Mirrors [`crate::ScriptsService::update_script`].
    #[allow(clippy::too_many_arguments)]
    pub async fn update_script(
        &self,
        id: String,
        name: String,
        is_public: bool,
        icon: Vec<u8>,
        script: String,
        description: String,
        short_description: String,
        allowed_domains: Vec<String>,
        script_type: FfiScriptType,
        parameters: HashMap<String, FfiScriptParameter>,
    ) -> Result<FfiScript, FfiError> {
        self.localized(async {
            let id = parse_uuid(&id)?;
            let script = self
                .inner
                .scripts()
                .update_script(
                    id,
                    &name,
                    is_public,
                    icon,
                    &script,
                    &description,
                    &short_description,
                    allowed_domains,
                    script_type.into(),
                    parameters
                        .into_iter()
                        .map(|(key, parameter)| (key, parameter.into()))
                        .collect(),
                )
                .await?;
            Ok(script.into())
        })
        .await
    }

    /// Enables `script_id` for the authenticated user. Mirrors
    /// [`crate::ScriptsService::enable_script`].
    pub async fn enable_script(&self, script_id: String) -> Result<(), FfiError> {
        self.localized(async {
            let script_id = parse_uuid(&script_id)?;
            self.inner.scripts().enable_script(script_id).await?;
            Ok(())
        })
        .await
    }

    /// Disables `script_id` for the authenticated user. Mirrors
    /// [`crate::ScriptsService::disable_script`].
    pub async fn disable_script(&self, script_id: String) -> Result<(), FfiError> {
        self.localized(async {
            let script_id = parse_uuid(&script_id)?;
            self.inner.scripts().disable_script(script_id).await?;
            Ok(())
        })
        .await
    }

    /// Lists the metadata of the scripts currently enabled for the
    /// authenticated user, without their content. Mirrors
    /// [`crate::ScriptsService::list_user_scripts`].
    pub async fn list_user_scripts(&self) -> Result<Vec<FfiScriptMetadata>, FfiError> {
        self.localized(async {
            let scripts = self.inner.scripts().list_user_scripts().await?;
            Ok(scripts.into_iter().map(FfiScriptMetadata::from).collect())
        })
        .await
    }

    /// Lists the metadata of every script marked public, regardless of who
    /// owns it or whether the authenticated user has it enabled, without
    /// their content. Mirrors [`crate::ScriptsService::list_public_scripts`].
    pub async fn list_public_scripts(&self) -> Result<Vec<FfiScriptMetadata>, FfiError> {
        self.localized(async {
            let scripts = self.inner.scripts().list_public_scripts().await?;
            Ok(scripts.into_iter().map(FfiScriptMetadata::from).collect())
        })
        .await
    }

    /// Installs `script_id` for the authenticated user, to be run with
    /// `parameters_json` — a JSON object keyed by the script's parameter
    /// names (e.g. `{"username": "alice"}`). Mirrors
    /// [`crate::ScriptsService::install_script`]: the installation only
    /// shows up in [`Self::list_installed_scripts`] once the background
    /// sync job has consumed it back.
    pub async fn install_script(
        &self,
        script_id: String,
        parameters_json: String,
    ) -> Result<(), FfiError> {
        self.localized(async {
            let script_id = parse_uuid(&script_id)?;
            let parameters = serde_json::from_str(&parameters_json).with_context(|| {
                format!("invalid parameters json for script {script_id}: expected a JSON object")
            })?;

            self.inner
                .scripts()
                .install_script(InstallScriptRequest {
                    script_id,
                    parameters,
                })
                .await?;
            Ok(())
        })
        .await
    }

    /// Lists every script installed for the authenticated user that's been
    /// synced to this device so far. Mirrors
    /// [`crate::ScriptsService::list_installed_scripts`].
    pub async fn list_installed_scripts(&self) -> Result<Vec<FfiInstalledScript>, FfiError> {
        self.localized(async {
            let installed = self.inner.scripts().list_installed_scripts().await?;
            installed
                .into_iter()
                .map(FfiInstalledScript::try_from)
                .collect()
        })
        .await
    }

    /// Runs the installed script `script_id` — with the parameter values it
    /// was installed with — until it finishes, reporting progress and
    /// questions through `init`'s `scraper_listener`; the documents it
    /// saves are uploaded like any other. Fails if the installation hasn't
    /// synced to this device yet. Mirrors
    /// [`crate::ScriptsService::fetch_installed_script`] then
    /// [`crate::ScrapersService::run_installed`].
    pub async fn run_installed_script(&self, script_id: String) -> Result<(), FfiError> {
        self.localized(async {
            let script_id = parse_uuid(&script_id)?;
            let installed = self
                .inner
                .scripts()
                .fetch_installed_script(script_id)
                .await?;
            *self
                .scraper_language
                .write()
                .unwrap_or_else(PoisonError::into_inner) = self.inner.language().await;
            self.inner.scrapers().run_installed(&installed).await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use crate::domains::documents::{EventType, FakeChangelogEvent, FakeDocument, FakeMetadata};
    use crate::domains::scripts::FakeScript;
    use crate::domains::users::FakeOpenSession;

    #[test]
    fn parse_uuid_accepts_a_canonical_uuid() {
        let id = Uuid::now_v7();

        assert_eq!(parse_uuid(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn parse_uuid_rejects_malformed_input() {
        assert!(parse_uuid("not-a-uuid").is_err());
    }

    #[test]
    fn metadata_conversion_preserves_every_field() {
        let metadata = FakeMetadata::new().build();

        let ffi: FfiMetadata = metadata.clone().into();

        assert_eq!(ffi.id, metadata.id().to_string());
        assert_eq!(ffi.name, metadata.name());
        assert_eq!(ffi.content_type, metadata.content_type());
        assert_eq!(ffi.created_at, metadata.created_at());
        assert_eq!(ffi.size, metadata.size());
        assert_eq!(ffi.checksum, metadata.checksum());
        assert_eq!(ffi.transcript, metadata.transcript());
        assert_eq!(ffi.r#type, metadata.r#type().map(FfiDocumentType::from));
        assert_eq!(ffi.category, metadata.category().map(FfiCategory::from));
        assert_eq!(
            ffi.subcategory,
            metadata.subcategory().map(FfiSubcategory::from)
        );
    }

    #[test]
    fn metadata_conversion_roundtrips_through_ffi() {
        let metadata = FakeMetadata::new().build();

        let ffi: FfiMetadata = metadata.clone().into();
        let roundtripped = Metadata::try_from(ffi).unwrap();

        assert_eq!(roundtripped, metadata);
    }

    #[test]
    fn metadata_conversion_rejects_a_malformed_id() {
        let mut ffi: FfiMetadata = FakeMetadata::new().build().into();
        ffi.id = "not-a-uuid".to_string();

        assert!(Metadata::try_from(ffi).is_err());
    }

    #[test]
    fn document_conversion_stringifies_the_id() {
        let document = FakeDocument::new()
            .with_content(b"hello".to_vec())
            .with_metadata(FakeMetadata::new().build())
            .build();

        let ffi: FfiDocument = document.clone().into();

        assert_eq!(ffi.id, document.id().to_string());
        assert_eq!(ffi.content, document.content());
        assert_eq!(ffi.metadata.name, document.metadata().name());
    }

    #[test]
    fn script_conversion_preserves_every_field() {
        let script = FakeScript::new().build();

        let ffi: FfiScript = script.clone().into();

        assert_eq!(ffi.id, script.id().to_string());
        assert_eq!(ffi.name, script.name());
        assert_eq!(ffi.description, script.description());
        assert_eq!(ffi.short_description, script.short_description());
        assert_eq!(ffi.is_public, script.is_public());
        assert_eq!(ffi.icon, script.icon());
        assert_eq!(ffi.allowed_domains, script.allowed_domains());
        assert_eq!(ffi.version, script.version());
        assert_eq!(ffi.script, script.script());
        assert_eq!(ffi.last_updated, script.last_updated());
    }

    #[test]
    fn login_outcome_conversion_preserves_the_open_sessions() {
        let session = FakeOpenSession::new().build();

        let ffi = FfiLoginOutcome::from(LoginOutcome::SessionLimitReached {
            max_sessions: 3,
            sessions: vec![session.clone()],
        });

        let FfiLoginOutcome::SessionLimitReached {
            max_sessions,
            sessions,
        } = ffi
        else {
            panic!("expected SessionLimitReached, got {ffi:?}");
        };
        assert_eq!(max_sessions, 3);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.id());
        assert_eq!(sessions[0].device_name, session.device_name());
        assert_eq!(sessions[0].created_at, session.created_at());
    }

    #[test]
    fn login_outcome_conversion_keeps_the_session_token() {
        let ffi = FfiLoginOutcome::from(LoginOutcome::LoggedIn {
            session_token: "a-token".to_string(),
        });

        assert!(matches!(
            ffi,
            FfiLoginOutcome::LoggedIn { session_token } if session_token == "a-token"
        ));
    }

    #[test]
    fn event_type_conversion_maps_every_variant() {
        assert!(matches!(
            FfiEventType::from(EventType::SaveDocument),
            FfiEventType::SaveDocument
        ));
        assert!(matches!(
            FfiEventType::from(EventType::UpdateMetadata),
            FfiEventType::UpdateMetadata
        ));
        assert!(matches!(
            FfiEventType::from(EventType::Deleted),
            FfiEventType::Deleted
        ));
    }

    #[test]
    fn changelog_event_conversion_stringifies_the_subject_id_and_preserves_payload() {
        let id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let event = FakeChangelogEvent::new()
            .with_id(id)
            .with_event_type(EventType::SaveDocument)
            .with_subject_id(document_id)
            .with_content(b"body".to_vec())
            .with_metadata(FakeMetadata::new().build())
            .build();

        let ffi: FfiChangelogEvent = event.into();

        assert_eq!(ffi.id, id.to_string());
        assert!(matches!(ffi.event_type, FfiEventType::SaveDocument));
        assert_eq!(ffi.subject_id, document_id.to_string());
        assert_eq!(ffi.content, Some(b"body".to_vec()));
        assert!(ffi.metadata.is_some());
    }

    #[test]
    fn changelog_event_conversion_handles_events_with_no_content_or_metadata() {
        let event = FakeChangelogEvent::new()
            .with_event_type(EventType::Deleted)
            .with_subject_id(Uuid::now_v7())
            .without_content()
            .without_metadata()
            .build();

        let ffi: FfiChangelogEvent = event.into();

        assert_eq!(ffi.content, None);
        assert!(ffi.metadata.is_none());
    }

    #[test]
    fn ffi_error_carries_the_code_and_the_localized_wording() {
        let err: Error = ErrorKind::InvalidCredentials.into();

        let ffi_err = FfiError::from_error(&err, "fr");

        assert_eq!(ffi_err.code(), ErrorCode::InvalidCredentials);
        assert_eq!(ffi_err.to_string(), err.user_message("fr"));
    }

    #[test]
    fn ffi_error_never_carries_the_technical_description() {
        let err: Error = ErrorKind::UnsupportedDocumentExtension("txt".to_string()).into();

        let ffi_err = FfiError::from_error(&err, "en");

        assert!(!ffi_err.to_string().contains("txt"));
    }

    #[test]
    fn parse_uuid_failure_maps_to_invalid_uuid() {
        let err = parse_uuid("not-a-uuid").unwrap_err();

        assert_eq!(err.code(), ErrorCode::InvalidUuid);
    }
}
