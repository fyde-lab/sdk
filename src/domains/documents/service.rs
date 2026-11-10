use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::{ErrorContext as _, ErrorKind, Result};

use crate::domains::changelog::{
    ChangelogEvent, EventType, MAX_EVENT_BYTES, Service as ChangelogService,
};

use super::attributes::{self, Service as AttributesService};
use super::classification::Service as ClassificationService;
use super::storage::{FacetField, Storage};
use super::transcript;
use super::{
    Category, Document, FacetCount, Indexer, Metadata, SearchFacets, SearchFilters, SearchQuery,
    Service, Subcategory, UploadRequest, UploadSource,
};

/// The only content type [`Service::upload`] currently accepts.
pub(super) const PDF_CONTENT_TYPE: &str = "application/pdf";

/// The default [`Service`] implementation: publishes new documents as
/// encrypted changelog events via an injected [`ChangelogService`], and
/// delegates local, unencrypted caching to an injected [`Storage`].
pub(super) struct DocumentsClient<S: Storage> {
    storage: S,
    changelog: Arc<dyn ChangelogService>,
    /// Invoked once per event by [`Service::start_sync`]'s consume job, when
    /// set — see [`crate::ClientConfig::on_document_change`].
    on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
    attributes: Arc<dyn AttributesService>,
    /// What guesses uploads' category, subcategory and type.
    classifier: Arc<dyn ClassificationService>,
    /// When classifying an upload last failed (see
    /// [`CLASSIFY_RETRY_AFTER`]).
    classify_failed_at: Mutex<Option<tokio::time::Instant>>,
}

/// How long uploads skip classification after it failed: failing usually
/// means the model couldn't be downloaded (a timeout, say), which every
/// upload would otherwise wait for again. The documents uploaded meanwhile
/// are published unclassified.
const CLASSIFY_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(5 * 60);

impl<S: Storage> DocumentsClient<S> {
    /// Creates a documents client using `storage` to read cached documents,
    /// `changelog` to publish new ones and drive [`Service::start_sync`],
    /// `on_document_change` as the callback [`Service::start_sync`] invokes
    /// per consumed event, `attributes` to check the attributes of every
    /// document before it's published, and `classifier` to classify uploads.
    pub(super) fn new(
        storage: S,
        changelog: Arc<dyn ChangelogService>,
        on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
        attributes: Arc<dyn AttributesService>,
        classifier: Arc<dyn ClassificationService>,
    ) -> Self {
        Self {
            storage,
            changelog,
            on_document_change,
            attributes,
            classifier,
            classify_failed_at: Mutex::new(None),
        }
    }
}

impl<S: Storage> DocumentsClient<S> {
    /// Rejects metadata whose fields contradict each other, or whose
    /// attributes aren't valid (see [`AttributesService::check`]).
    async fn check_metadata(&self, metadata: &Metadata) -> Result<()> {
        check_categories(metadata.category, metadata.subcategory)?;
        self.attributes
            .check(&metadata.attributes)
            .await
            .context("invalid document attributes")
    }

    /// Sets `metadata`'s category, subcategory and, unless it has one, type
    /// to what its text looks like (each left `None` where the classifier
    /// can't tell), or leaves them as they are if that fails, or failed less
    /// than [`CLASSIFY_RETRY_AFTER`] ago: the document is then published
    /// unclassified rather than not at all.
    async fn classify_upload(&self, metadata: &mut Metadata) {
        let failed_at = *self
            .classify_failed_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if failed_at.is_some_and(|at| at.elapsed() < CLASSIFY_RETRY_AFTER) {
            tracing::debug!(
                "not classifying document {}, classifying failed recently",
                metadata.id
            );
            return;
        }

        match self.classifier.classify(&metadata.transcript).await {
            Ok(classification) => {
                metadata.category = classification.category;
                metadata.subcategory = classification.subcategory;
                metadata.r#type = metadata.r#type.or(classification.r#type);
            }
            Err(err) => {
                *self
                    .classify_failed_at
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(tokio::time::Instant::now());
                tracing::warn!(
                    code = ?err.code(),
                    "failed to classify document {}, publishing it unclassified: {err}",
                    metadata.id
                );
            }
        }
    }

    /// Counts the documents matching `terms` and `filters` — minus
    /// `field`'s own filter — per value of `field`.
    async fn count_documents_by(
        &self,
        terms: &[String],
        filters: &SearchFilters,
        field: FacetField,
    ) -> Result<Vec<FacetCount<String>>> {
        let mut filters = filters.clone();
        match field {
            FacetField::Category => filters.categories.clear(),
            FacetField::Subcategory => filters.subcategories.clear(),
            FacetField::Type => filters.types.clear(),
            FacetField::ContentType => filters.content_types.clear(),
        }

        self.storage
            .count_documents_by(terms, &filters, field)
            .await
            .with_context(|| format!("failed to count matching documents by {field:?}"))
    }

    /// The creation date range of the documents matching `terms` and
    /// `filters` — minus the date filters themselves.
    async fn created_at_range(
        &self,
        terms: &[String],
        filters: &SearchFilters,
    ) -> Result<Option<(i64, i64)>> {
        let filters = SearchFilters {
            created_from: None,
            created_until: None,
            ..filters.clone()
        };

        self.storage
            .created_at_range(terms, &filters)
            .await
            .context("failed to find the creation date range of matching documents")
    }
}

#[async_trait]
impl<S: Storage> Service for DocumentsClient<S> {
    /// Reads the PDF file at `path`, encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its
    /// generated id.
    ///
    /// This follows an envelope encryption scheme (see
    /// `changelog::crypto::encrypt_event`): a fresh, random data encryption
    /// key (DEK) is generated for the event, used to encrypt it with
    /// AES-256-GCM, and is itself wrapped under a key-encryption-key before
    /// being sent alongside the ciphertext. Only the wrapped DEK and
    /// ciphertext ever leave this process.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn upload(&self, mut request: UploadRequest) -> Result<Uuid> {
        // Checked before anything else: publishing the event needs a
        // session, so a caller who never logged in gets this specific,
        // recognizable error before any time is spent reading and parsing
        // the document.
        self.changelog
            .ensure_master_key()
            .await
            .context("cannot upload a document before logging in")?;

        resolve_upload_request(&mut request)?;

        let (content, original_name) = match request.source {
            UploadSource::Path(path) => {
                check_extension(&path)?;
                let content = tokio::fs::read(&path)
                    .instrument(tracing::debug_span!("read_document"))
                    .await
                    .with_context(|| format!("failed to read document at {}", path.display()))?;
                let original_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                (content, original_name)
            }
            UploadSource::Raw { name, content } => {
                check_extension(std::path::Path::new(&name))?;
                (content, name)
            }
        };

        // The document is published whole in one event, so one larger than
        // the largest event can never be saved: refused before hashing and
        // parsing it. The event also carries the metadata (transcript
        // included), so `changelog::Service::send` checks the encrypted
        // event itself again.
        if content.len() > MAX_EVENT_BYTES {
            return Err(ErrorKind::DocumentTooLarge {
                what: format!("document {original_name:?}"),
                size: content.len(),
                max: MAX_EVENT_BYTES,
            }
            .into());
        }

        // Checked before extracting the transcript: no point paying for
        // that only to reject the document. This only
        // sees documents the sync job already cached locally, so the consume
        // job checks again before caching one (see `changelog::service`).
        let checksum = checksum(&content);
        if let Some(existing) = self
            .storage
            .find_document_by_checksum(&checksum)
            .instrument(tracing::debug_span!("find_document_by_checksum"))
            .await
            .context("failed to look up local documents with the same content")?
        {
            return Err(ErrorKind::DuplicateDocument(existing).into());
        }

        let mut metadata = parse_content(&content, &original_name, checksum)
            .with_context(|| format!("failed to parse document {original_name:?}"))?;
        if let Some(name) = request.name {
            metadata.name = name;
        }
        if let Some(r#type) = request.r#type {
            metadata.r#type = Some(r#type);
        }
        if let Some(category) = request.category {
            metadata.category = Some(category);
            metadata.subcategory = request.subcategory;
        }
        metadata.attributes.extend(request.attributes);

        self.check_metadata(&metadata)
            .instrument(tracing::debug_span!("check_metadata"))
            .await
            .with_context(|| format!("invalid metadata for document {original_name:?}"))?;

        // Never over a category the caller (a scraper, say) chose: the type
        // is then theirs too, set or not.
        if metadata.category.is_none() {
            self.classify_upload(&mut metadata)
                .instrument(tracing::debug_span!("classify_document"))
                .await;
        }

        let id = metadata.id;
        self.changelog
            .send(EventType::SaveDocument, id, Some(&content), Some(&metadata))
            .instrument(tracing::debug_span!("publish_event"))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        Ok(id)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Document>> {
        self.storage.get_document(id).await
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Metadata>> {
        self.storage.list_documents(offset, limit).await
    }

    async fn search(&self, query: SearchQuery) -> Result<Vec<Metadata>> {
        if query.offset < 0 {
            return Err(ErrorKind::InvalidSearchQuery(format!(
                "offset must not be negative, got {}",
                query.offset
            ))
            .into());
        }
        if query.limit <= 0 {
            return Err(ErrorKind::InvalidSearchQuery(format!(
                "limit must be positive, got {}",
                query.limit
            ))
            .into());
        }
        check_created_range(&query.filters)?;

        let terms = search_terms(&query.text);
        self.storage
            .search_documents(
                &terms,
                &query.filters,
                query.order,
                query.offset,
                query.limit,
            )
            .await
            .context("failed to search local documents")
    }

    async fn search_facets(&self, text: &str, filters: &SearchFilters) -> Result<SearchFacets> {
        check_created_range(filters)?;

        let terms = search_terms(text);

        // Each field is counted with its own filter lifted, so the values on
        // offer for it are every alternative compatible with the *rest* of
        // the search, not only the one(s) already picked — while every other
        // field narrows to what's left under the picked ones.
        let categories = self
            .count_documents_by(&terms, filters, FacetField::Category)
            .await?;
        let subcategories = self
            .count_documents_by(&terms, filters, FacetField::Subcategory)
            .await?;
        let types = self
            .count_documents_by(&terms, filters, FacetField::Type)
            .await?;
        let content_types = self
            .count_documents_by(&terms, filters, FacetField::ContentType)
            .await?;
        let created_at_range = self.created_at_range(&terms, filters).await?;

        Ok(SearchFacets {
            categories: parse_facet(categories, "category")?,
            subcategories: parse_facet(subcategories, "subcategory")?,
            types: parse_facet(types, "document type")?,
            content_types,
            earliest_created_at: created_at_range.map(|(earliest, _)| earliest),
            latest_created_at: created_at_range.map(|(_, latest)| latest),
        })
    }

    async fn update_metadata(&self, metadata: Metadata) -> Result<()> {
        let id = metadata.id;
        self.check_metadata(&metadata)
            .await
            .with_context(|| format!("invalid metadata for document {id}"))?;

        self.changelog
            .send(EventType::UpdateMetadata, id, None, Some(&metadata))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        self.changelog
            .send(EventType::Deleted, id, None, None)
            .await
            .with_context(|| format!("failed to publish the deletion of document {id}"))
    }

    async fn start_sync(&self) -> Result<()> {
        let on_document_change = self.on_document_change.clone();
        let callback: Box<dyn FnMut(ChangelogEvent) + Send> = Box::new(move |event| {
            if let Some(on_document_change) = &on_document_change {
                on_document_change(event);
            }
        });

        self.changelog.clone().start_consume_job(callback).await
    }

    fn stop_sync(&self) {
        self.changelog.stop_consume_job();
    }
}

/// The default [`Indexer`]: caches documents through an injected
/// [`Storage`] (whose triggers keep the full-text search index in step).
pub(crate) struct DocumentIndexer<S: Storage> {
    storage: S,
}

impl<S: Storage> DocumentIndexer<S> {
    pub(crate) fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: Storage> Indexer for DocumentIndexer<S> {
    async fn save_document(&self, document: &Document) -> Result<()> {
        self.storage
            .save_document(document)
            .await
            .with_context(|| format!("failed to cache document {} locally", document.id))
    }

    async fn update_metadata(&self, id: Uuid, metadata: &Metadata) -> Result<()> {
        self.storage
            .update_metadata(id, metadata)
            .await
            .with_context(|| format!("failed to update cached document {id} locally"))
    }

    async fn delete_document(&self, id: Uuid) -> Result<()> {
        self.storage
            .delete_document(id)
            .await
            .with_context(|| format!("failed to delete cached document {id} locally"))
    }
}

/// Completes `request` with what its fields imply — a subcategory alone
/// implies its category — and rejects one whose fields contradict each
/// other. Shared with the dev [`Service`] (see `super::dev`), so a scraper
/// run without a server fails on the same requests a real upload would.
pub(super) fn resolve_upload_request(request: &mut UploadRequest) -> Result<()> {
    if request.category.is_none() {
        request.category = request
            .subcategory
            .map(|subcategory| subcategory.category());
    }
    check_categories(request.category, request.subcategory)?;
    // Only what can be checked without a server: the dev `Service` has none
    // to fetch attribute definitions from. `upload` checks them in full once
    // the document is parsed.
    attributes::check_values(&request.attributes)
}

/// Rejects a subcategory that doesn't narrow down `category` (or that's
/// set without any category).
pub(super) fn check_categories(
    category: Option<Category>,
    subcategory: Option<Subcategory>,
) -> Result<()> {
    if let Some(subcategory) = subcategory
        && Some(subcategory.category()) != category
    {
        return Err(ErrorKind::MismatchedSubcategory(format!(
            "subcategory {subcategory} belongs to {}, not {}",
            subcategory.category(),
            category.map_or("no category".to_string(), |category| category.to_string())
        ))
        .into());
    }
    Ok(())
}

/// Rejects a `created_from` later than `created_until`, a range no
/// document can fall in.
fn check_created_range(filters: &SearchFilters) -> Result<()> {
    if let (Some(from), Some(until)) = (filters.created_from, filters.created_until)
        && from > until
    {
        return Err(ErrorKind::InvalidSearchQuery(format!(
            "created_from ({from}) is later than created_until ({until})"
        ))
        .into());
    }
    Ok(())
}

/// Parses the canonical string form [`Storage::count_documents_by`] returns
/// each value of a classification field in (e.g. `"real_estate"`) back into
/// its enum, `field` naming it in the error.
fn parse_facet<T: std::str::FromStr<Err = crate::Error>>(
    counts: Vec<FacetCount<String>>,
    field: &str,
) -> Result<Vec<FacetCount<T>>> {
    counts
        .into_iter()
        .map(|facet| {
            Ok(FacetCount {
                value: facet.value.parse().with_context(|| {
                    format!("invalid {field} {:?} in local database", facet.value)
                })?,
                count: facet.count,
            })
        })
        .collect()
}

/// Splits a search's free text into the words that must each match: runs
/// of letters/digits, everything else (spaces, punctuation, quotes...)
/// separating them — the same boundaries the index tokenizes documents on,
/// so "relevé-2024" searches for "relevé" and "2024".
fn search_terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

/// Validates that `name`'s extension is a supported document type; only
/// `.pdf` is currently accepted.
fn check_extension(name: &std::path::Path) -> Result<()> {
    let extension = name
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension != "pdf" {
        return Err(ErrorKind::UnsupportedDocumentExtension(extension).into());
    }
    Ok(())
}

/// The hex-encoded SHA-256 checksum of a document's `content`, as stored in
/// its [`Metadata::checksum`].
#[tracing::instrument(level = "debug", skip_all, fields(size = content.len()))]
fn checksum(content: &[u8]) -> String {
    crate::tools::hex::encode(&Sha256::digest(content))
}

/// Builds the [`Metadata`] of a freshly uploaded PDF `content`, originally
/// named `original_name`, whose [`checksum`] is `checksum`: a new id, its
/// name (the file name without extension), size and transcript. Its type,
/// classification and attributes are left empty, for the upload request to
/// fill in.
#[tracing::instrument(level = "debug", skip(content, checksum), fields(size = content.len()))]
fn parse_content(content: &[u8], original_name: &str, checksum: String) -> Result<Metadata> {
    let name = Path::new(original_name)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let transcript =
        transcript::extract(content).context("failed to extract document transcript")?;

    Ok(Metadata {
        id: Uuid::now_v7(),
        original_name: original_name.to_string(),
        name,
        content_type: PDF_CONTENT_TYPE.to_string(),
        created_at,
        size: content.len() as u64,
        checksum,
        transcript,
        r#type: None,
        category: None,
        subcategory: None,
        attributes: BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Mutex;

    use tempfile::NamedTempFile;

    use super::super::attributes::MockService as MockAttributesService;
    use super::super::classification::{
        Classification, FakeClassification, MockService as MockClassificationService,
    };
    use super::super::storage::MockStorage;
    use super::super::transcript::tests::build_pdf;
    use super::super::{Attribute, DocumentType};
    use super::super::{FakeDocument, FakeMetadata};
    use super::*;

    /// A classifier whose every call panics: for tests that never classify
    /// an upload.
    fn unused_classifier() -> Arc<dyn ClassificationService> {
        Arc::new(MockClassificationService::new())
    }

    /// A classifier expected to classify exactly `times` uploads, each as
    /// `classification`.
    fn classifier_returning(
        classification: Classification,
        times: usize,
    ) -> Arc<dyn ClassificationService> {
        let mut classifier = MockClassificationService::new();
        classifier
            .expect_classify()
            .times(times)
            .returning(move |_| Ok(classification));
        Arc::new(classifier)
    }

    fn accepting_attributes() -> Arc<dyn AttributesService> {
        let mut attributes = MockAttributesService::new();
        attributes.expect_check().returning(|_| Ok(()));
        Arc::new(attributes)
    }

    /// An [`AttributesService`] fake rejecting any attributes, as it would
    /// an invalid one.
    fn rejecting_attributes() -> Arc<dyn AttributesService> {
        let mut attributes = MockAttributesService::new();
        attributes
            .expect_check()
            .returning(|_| Err(crate::ErrorKind::InvalidAttribute("rejected".to_string()).into()));
        Arc::new(attributes)
    }

    #[tokio::test]
    async fn upload_fails_fast_with_no_master_key_before_ever_reading_the_document() {
        let file = write_temp_file("pdf", b"hello world");

        let changelog = Arc::new(RecordingChangelog {
            no_master_key: true,
            ..Default::default()
        });
        // `MockStorage` panics on any call, proving `upload` never reaches
        // the duplicate check (or anything after it) once this check fails.
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog,
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let err = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("no master key found"),
            "unexpected error: {err}"
        );
    }

    /// A single `send` call recorded by [`RecordingChangelog`].
    type SentEvent = (EventType, Uuid, Option<Vec<u8>>, Option<Metadata>);

    /// A [`ChangelogService`] fake recording every event passed to
    /// [`ChangelogService::send`], and replaying `to_consume` through
    /// [`ChangelogService::start_consume_job`]'s callback, for tests that
    /// don't need a live server.
    #[derive(Default)]
    struct RecordingChangelog {
        sent: Mutex<Vec<SentEvent>>,
        to_consume: Mutex<Vec<ChangelogEvent>>,
        stopped: std::sync::atomic::AtomicBool,
        /// When set, `ensure_master_key` fails the same way the real
        /// `ChangelogClient` does before any account has ever logged in —
        /// for tests exercising `Service::upload`'s fail-fast check.
        no_master_key: bool,
        /// When set, `send` records the event, then fails.
        fail_send: bool,
        /// When set, `send` records the event, notifies it, then never
        /// returns.
        hang_send: Option<Arc<tokio::sync::Notify>>,
    }

    #[async_trait]
    impl ChangelogService for RecordingChangelog {
        async fn ensure_master_key(&self) -> Result<()> {
            if self.no_master_key {
                return Err(ErrorKind::Encryption(
                    "no master key found in local settings; log in or create an account first"
                        .into(),
                )
                .into());
            }
            Ok(())
        }

        async fn send(
            &self,
            event_type: EventType,
            subject_id: Uuid,
            content: Option<&[u8]>,
            metadata: Option<&Metadata>,
        ) -> Result<()> {
            self.sent.lock().unwrap().push((
                event_type,
                subject_id,
                content.map(<[u8]>::to_vec),
                metadata.cloned(),
            ));
            if self.fail_send {
                return Err(ErrorKind::InvalidChangelogEvent("refused".to_string()).into());
            }
            if let Some(reached) = &self.hang_send {
                reached.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(())
        }

        async fn start_consume_job(
            self: Arc<Self>,
            mut callback: Box<dyn FnMut(ChangelogEvent) + Send>,
        ) -> Result<()> {
            for event in self.to_consume.lock().unwrap().drain(..) {
                callback(event);
            }
            Ok(())
        }

        fn stop_consume_job(&self) {
            self.stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Writes `content` to a temporary file with the given `extension`, for
    /// tests that need a real path to pass to [`Service::upload`].
    fn write_temp_file(extension: &str, content: &[u8]) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .unwrap();
        file.write_all(content).unwrap();
        file
    }

    #[tokio::test]
    async fn upload_publishes_a_save_document_event_for_the_document() {
        let pdf = build_pdf("hello world");
        let file = write_temp_file("pdf", &pdf);
        let file_name = file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let mut storage = MockStorage::new();
        let expected_checksum = checksum(&pdf);
        storage
            .expect_find_document_by_checksum()
            .withf(move |checksum| checksum == expected_checksum)
            .times(1)
            .returning(|_| Ok(None));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            classifier_returning(Classification::default(), 1),
        );

        let id = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, document_id, content, sent_metadata) = &sent[0];
        assert_eq!(*event_type, EventType::SaveDocument);
        assert_eq!(*document_id, id);
        assert_eq!(content.as_deref(), Some(pdf.as_slice()));
        let sent_metadata = sent_metadata.as_ref().unwrap();
        assert_eq!(sent_metadata.id, id);
        assert_eq!(sent_metadata.original_name, file_name);
        assert_eq!(sent_metadata.name, file_name.strip_suffix(".pdf").unwrap());
        assert_eq!(sent_metadata.content_type, PDF_CONTENT_TYPE);
        assert_eq!(sent_metadata.size, pdf.len() as u64);
        assert_eq!(sent_metadata.checksum, checksum(&pdf));
        assert!(
            sent_metadata.transcript.contains("hello world"),
            "unexpected transcript: {:?}",
            sent_metadata.transcript
        );
        assert_eq!(sent_metadata.r#type, None);
        assert_eq!(sent_metadata.category, None);
        assert_eq!(sent_metadata.subcategory, None);
        assert!(sent_metadata.attributes.is_empty());
    }

    #[tokio::test]
    async fn upload_rejects_a_document_already_saved_locally_before_parsing_or_publishing_it() {
        // Not a valid PDF: parsing it would fail, so this also proves the
        // duplicate is rejected before parsing.
        let pdf = b"hello world".to_vec();
        let file = write_temp_file("pdf", &pdf);
        let existing = Uuid::now_v7();

        let mut storage = MockStorage::new();
        let expected_checksum = checksum(&pdf);
        storage
            .expect_find_document_by_checksum()
            .withf(move |checksum| checksum == expected_checksum)
            .times(1)
            .returning(move |_| Ok(Some(existing)));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let err = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap_err();

        assert!(
            matches!(err.kind(), ErrorKind::DuplicateDocument(id) if *id == existing),
            "unexpected error: {err}"
        );
        assert_eq!(err.code(), crate::ErrorCode::DuplicateDocument);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upload_rejects_non_pdf_extensions() {
        let file = write_temp_file("txt", b"hello world");
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let result = client.upload(UploadRequest::from_path(file.path())).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn upload_refuses_a_document_larger_than_an_event_before_reading_it() {
        let changelog = Arc::new(RecordingChangelog::default());
        // No `expect_find_document_by_checksum()`: the mock panics if the
        // document gets as far as the duplicate check.
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let err = client
            .upload(UploadRequest::from_raw(
                "huge.pdf".to_string(),
                vec![0; MAX_EVENT_BYTES + 1],
            ))
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::DocumentTooLarge);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upload_applies_metadata_overrides_from_the_request() {
        let raw = build_pdf("hello world");

        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(1)
            .returning(|_| Ok(None));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let request = UploadRequest {
            source: super::super::UploadSource::Raw {
                name: "raw.pdf".to_string(),
                content: raw.clone(),
            },
            name: Some("overridden.pdf".to_string()),
            r#type: Some(DocumentType::Payslip),
            category: Some(super::super::Category::Work),
            subcategory: None,
            attributes: BTreeMap::new(),
        };

        client.upload(request).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (_, _, _, sent_metadata) = &sent[0];
        let sent_metadata = sent_metadata.as_ref().unwrap();
        assert_eq!(sent_metadata.original_name, "raw.pdf");
        assert_eq!(sent_metadata.name, "overridden.pdf");
        assert_eq!(sent_metadata.r#type, Some(DocumentType::Payslip));
        assert_eq!(sent_metadata.category, Some(super::super::Category::Work));
    }

    /// Uploads a PDF with `request`'s overrides, returning the metadata it
    /// was published with.
    async fn upload_with(mut request: UploadRequest) -> Metadata {
        // Classified only when the request leaves the category to it.
        let classified = request.category.is_none() && request.subcategory.is_none();
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(1)
            .returning(|_| Ok(None));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            classifier_returning(Classification::default(), usize::from(classified)),
        );

        request.source = super::super::UploadSource::Raw {
            name: "raw.pdf".to_string(),
            content: build_pdf("hello world"),
        };
        client.upload(request).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        sent[0].3.clone().unwrap()
    }

    #[tokio::test]
    async fn upload_infers_the_category_of_a_lone_subcategory() {
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.subcategory = Some(Subcategory::Energy);

        let metadata = upload_with(request).await;

        assert_eq!(metadata.category, Some(Category::Housing));
        assert_eq!(metadata.subcategory, Some(Subcategory::Energy));
    }

    #[tokio::test]
    async fn upload_rejects_a_subcategory_of_another_category_before_parsing() {
        // `MockStorage` panics on any call, proving the request is rejected
        // before the duplicate check and parsing.
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.category = Some(Category::Money);
        request.subcategory = Some(Subcategory::Energy);

        let err = client.upload(request).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::MismatchedSubcategory);
    }

    #[tokio::test]
    async fn upload_rejects_attributes_the_attributes_service_rejects() {
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            rejecting_attributes(),
            unused_classifier(),
        );
        let mut request = UploadRequest::from_raw("raw.pdf", build_pdf("hello world"));
        request.attributes = BTreeMap::from([("price".to_string(), Attribute::Integer(1))]);

        let err = client.upload(request).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upload_keeps_the_requested_attributes() {
        let attributes = BTreeMap::from([
            ("price".to_string(), Attribute::Float(32.3)),
            ("paid".to_string(), Attribute::Boolean(true)),
        ]);
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.attributes = attributes.clone();

        let metadata = upload_with(request).await;

        assert_eq!(metadata.attributes, attributes);
    }

    #[tokio::test]
    async fn upload_rejects_a_requested_non_finite_attribute_before_parsing() {
        // `MockStorage` panics on any call, proving the request is rejected
        // before the duplicate check and parsing.
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );
        let mut request = UploadRequest::from_raw("raw.pdf", Vec::new());
        request.attributes = BTreeMap::from([("price".to_string(), Attribute::Float(f64::NAN))]);

        let err = client.upload(request).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
    }

    #[tokio::test]
    async fn update_metadata_rejects_attributes_the_attributes_service_rejects() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            rejecting_attributes(),
            unused_classifier(),
        );
        let metadata = super::super::FakeMetadata::new()
            .with_attributes([("price".to_string(), Attribute::Integer(1))])
            .build();

        let err = client.update_metadata(metadata).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_metadata_publishes_the_given_attributes() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_classifier(),
        );
        let attributes = BTreeMap::from([("price".to_string(), Attribute::Float(32.3))]);
        let metadata = super::super::FakeMetadata::new()
            .with_attributes(attributes.clone())
            .build();

        client.update_metadata(metadata).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent[0].3.as_ref().unwrap().attributes, attributes);
    }

    #[tokio::test]
    async fn update_metadata_rejects_a_subcategory_of_another_category() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_classifier(),
        );
        let metadata = super::super::FakeMetadata::new()
            .with_category(Category::Money)
            .with_subcategory(Subcategory::Energy)
            .build();

        let err = client.update_metadata(metadata).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::MismatchedSubcategory);
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_returns_a_document_previously_cached_locally() {
        let id = Uuid::now_v7();
        let document = Document {
            id,
            content: b"hello".to_vec(),
            metadata: Metadata {
                id,
                name: "report.pdf".to_string(),
                original_name: "report.pdf".to_string(),
                content_type: PDF_CONTENT_TYPE.to_string(),
                created_at: 1_700_000_000,
                size: 5,
                checksum: "deadbeef".to_string(),
                transcript: String::new(),
                r#type: None,
                category: None,
                subcategory: None,
                attributes: BTreeMap::new(),
            },
        };

        let mut storage = MockStorage::new();
        let expected = document.clone();
        storage
            .expect_get_document()
            .withf(move |queried_id| *queried_id == id)
            .times(1)
            .returning(move |_| Ok(Some(expected.clone())));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        assert_eq!(client.get(id).await.unwrap(), Some(document));
    }

    #[tokio::test]
    async fn list_pages_through_documents_previously_cached_locally() {
        let page = vec![
            super::super::FakeMetadata::new().with_name("two").build(),
            super::super::FakeMetadata::new().with_name("three").build(),
        ];

        let mut storage = MockStorage::new();
        let expected_page = page.clone();
        storage
            .expect_list_documents()
            .withf(|offset, limit| *offset == 1 && *limit == 2)
            .times(1)
            .returning(move |_, _| Ok(expected_page.clone()));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let page = client.list(1, 2).await.unwrap();

        assert_eq!(
            page.into_iter()
                .map(|metadata| metadata.name)
                .collect::<Vec<_>>(),
            vec!["two", "three"]
        );
    }

    #[tokio::test]
    async fn search_splits_the_text_into_terms_and_forwards_the_query_to_storage() {
        let filters = super::super::SearchFilters {
            categories: vec![super::super::Category::Money],
            created_from: Some(1),
            created_until: Some(2),
            ..Default::default()
        };
        let found = vec![super::super::FakeMetadata::new().build()];

        let mut storage = MockStorage::new();
        let expected_filters = filters.clone();
        let returned = found.clone();
        storage
            .expect_search_documents()
            .withf(move |terms, filters, order, offset, limit| {
                terms == ["Relevé", "2024", "EDF"]
                    && *filters == expected_filters
                    && *order == super::super::SearchOrder::Oldest
                    && *offset == 10
                    && *limit == 5
            })
            .times(1)
            .returning(move |_, _, _, _, _| Ok(returned.clone()));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let documents = client
            .search(SearchQuery {
                text: "  Relevé-2024 \"EDF\" ".to_string(),
                filters,
                order: super::super::SearchOrder::Oldest,
                offset: 10,
                limit: 5,
            })
            .await
            .unwrap();

        assert_eq!(documents, found);
    }

    #[tokio::test]
    async fn search_forwards_no_terms_for_blank_text() {
        let mut storage = MockStorage::new();
        storage
            .expect_search_documents()
            .withf(|terms, _, _, _, _| terms.is_empty())
            .times(1)
            .returning(|_, _, _, _, _| Ok(Vec::new()));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        client.search(SearchQuery::new(" - ", 0, 10)).await.unwrap();
    }

    #[tokio::test]
    async fn search_rejects_invalid_queries_without_querying_storage() {
        let mut inverted_dates = SearchQuery::new("", 0, 10);
        inverted_dates.filters.created_from = Some(2);
        inverted_dates.filters.created_until = Some(1);

        for query in [
            SearchQuery::new("", -1, 10),
            SearchQuery::new("", 0, 0),
            inverted_dates,
        ] {
            // No `expect_search_documents()`: storage must never be reached.
            let client = DocumentsClient::new(
                MockStorage::new(),
                Arc::new(RecordingChangelog::default()),
                None,
                accepting_attributes(),
                unused_classifier(),
            );

            let err = client.search(query.clone()).await.unwrap_err();

            assert_eq!(
                err.code(),
                crate::ErrorCode::InvalidSearchQuery,
                "query {query:?}"
            );
        }
    }

    /// `filters` minus `field`'s own filter: what the service must pass to
    /// storage when counting `field`.
    fn without_own_filter(
        filters: &super::super::SearchFilters,
        field: FacetField,
    ) -> super::super::SearchFilters {
        let mut filters = filters.clone();
        match field {
            FacetField::Category => filters.categories.clear(),
            FacetField::Subcategory => filters.subcategories.clear(),
            FacetField::Type => filters.types.clear(),
            FacetField::ContentType => filters.content_types.clear(),
        }
        filters
    }

    #[tokio::test]
    async fn search_facets_counts_each_field_with_its_own_filter_lifted() {
        use super::super::{Category, SearchFilters, Subcategory};

        let filters = SearchFilters {
            categories: vec![Category::Money],
            subcategories: vec![Subcategory::Banking],
            types: vec![DocumentType::Statement],
            content_types: vec!["application/pdf".to_string()],
            created_from: Some(1),
            created_until: Some(2),
        };
        let expected = super::super::FakeSearchFacets::new()
            .with_created_at_range(Some((10, 20)))
            .build();
        let as_strings = |values: Vec<String>, counts: Vec<u64>| -> Vec<FacetCount<String>> {
            values
                .into_iter()
                .zip(counts)
                .map(|(value, count)| FacetCount { value, count })
                .collect()
        };
        let per_field = [
            (
                FacetField::Category,
                as_strings(
                    expected
                        .categories
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.categories.iter().map(|f| f.count).collect(),
                ),
            ),
            (
                FacetField::Subcategory,
                as_strings(
                    expected
                        .subcategories
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.subcategories.iter().map(|f| f.count).collect(),
                ),
            ),
            (
                FacetField::Type,
                as_strings(
                    expected
                        .types
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.types.iter().map(|f| f.count).collect(),
                ),
            ),
            (FacetField::ContentType, expected.content_types.clone()),
        ];

        let mut storage = MockStorage::new();
        for (field, returned) in per_field {
            let expected_filters = without_own_filter(&filters, field);
            storage
                .expect_count_documents_by()
                .withf(move |terms, filters, queried_field| {
                    terms == ["facture"] && *filters == expected_filters && *queried_field == field
                })
                .times(1)
                .returning(move |_, _, _| Ok(returned.clone()));
        }
        let date_filters = SearchFilters {
            created_from: None,
            created_until: None,
            ..filters.clone()
        };
        storage
            .expect_created_at_range()
            .withf(move |terms, filters| terms == ["facture"] && *filters == date_filters)
            .times(1)
            .returning(|_, _| Ok(Some((10, 20))));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let facets = client.search_facets(" facture ", &filters).await.unwrap();

        assert_eq!(facets, expected);
    }

    #[tokio::test]
    async fn search_facets_fails_on_an_unknown_classification_value() {
        let mut storage = MockStorage::new();
        storage
            .expect_count_documents_by()
            .returning(|_, _, field| {
                Ok(if field == FacetField::Category {
                    vec![FacetCount {
                        value: "not-a-category".to_string(),
                        count: 1,
                    }]
                } else {
                    Vec::new()
                })
            });
        storage.expect_created_at_range().returning(|_, _| Ok(None));

        let client = DocumentsClient::new(
            storage,
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let err = client
            .search_facets("", &super::super::SearchFilters::default())
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidCategory);
    }

    #[tokio::test]
    async fn search_facets_rejects_an_inverted_date_range_without_querying_storage() {
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        let err = client
            .search_facets(
                "",
                &super::super::SearchFilters {
                    created_from: Some(2),
                    created_until: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidSearchQuery);
    }

    #[tokio::test]
    async fn update_metadata_publishes_an_update_metadata_event_with_the_given_metadata() {
        let metadata = super::super::FakeMetadata::new()
            .with_name("new.pdf")
            .build();
        let id = metadata.id;

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        client.update_metadata(metadata).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, document_id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::UpdateMetadata);
        assert_eq!(*document_id, id);
        assert_eq!(*content, None);
        let metadata = metadata.as_ref().unwrap();
        assert_eq!(metadata.name, "new.pdf");
    }

    #[tokio::test]
    async fn start_sync_invokes_on_document_change_for_every_consumed_event() {
        let event = super::super::FakeChangelogEvent::new().build();

        let changelog = Arc::new(RecordingChangelog {
            to_consume: Mutex::new(vec![event.clone()]),
            ..Default::default()
        });

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let on_document_change: Arc<dyn Fn(ChangelogEvent) + Send + Sync> =
            Arc::new(move |event| received_in_callback.lock().unwrap().push(event));
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog,
            Some(on_document_change),
            accepting_attributes(),
            unused_classifier(),
        );

        client.start_sync().await.unwrap();

        assert_eq!(*received.lock().unwrap(), vec![event]);
    }

    #[tokio::test]
    async fn start_sync_does_not_require_an_on_document_change_callback() {
        let event = super::super::FakeChangelogEvent::new().build();

        let changelog = Arc::new(RecordingChangelog {
            to_consume: Mutex::new(vec![event]),
            ..Default::default()
        });
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog,
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        client.start_sync().await.unwrap();
    }

    #[tokio::test]
    async fn stop_sync_delegates_to_the_changelog_services_stop_consume_job() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            None,
            accepting_attributes(),
            unused_classifier(),
        );

        client.stop_sync();

        assert!(changelog.stopped.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// A documents client over `changelog` alone, for tests of calls that
    /// only publish an event.
    fn publishing_client(changelog: Arc<RecordingChangelog>) -> DocumentsClient<MockStorage> {
        DocumentsClient::new(
            MockStorage::new(),
            changelog,
            None,
            Arc::new(MockAttributesService::new()),
            unused_classifier(),
        )
    }

    #[tokio::test]
    async fn delete_publishes_a_deleted_event_for_the_document_alone() {
        let id = Uuid::now_v7();
        let changelog = Arc::new(RecordingChangelog::default());
        // `MockStorage` without expectations: deleting never touches the
        // local cache itself, nor checks the document is in it.
        let client = publishing_client(changelog.clone());

        client.delete(id).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(*sent, vec![(EventType::Deleted, id, None, None)]);
    }

    #[tokio::test]
    async fn delete_fails_with_context_when_publishing_fails() {
        let changelog = Arc::new(RecordingChangelog {
            fail_send: true,
            ..Default::default()
        });
        let client = publishing_client(changelog);

        let err = client.delete(Uuid::now_v7()).await.unwrap_err();

        assert!(err.has_context(), "missing context: {err}");
    }

    #[tokio::test]
    async fn indexer_save_document_caches_the_document() {
        let document = FakeDocument::new().build();
        let id = document.id;
        let mut storage = MockStorage::new();
        storage
            .expect_save_document()
            .withf(move |saved| saved.id == id)
            .times(1)
            .returning(|_| Ok(()));

        DocumentIndexer::new(storage)
            .save_document(&document)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn indexer_save_document_fails_with_context_when_storage_fails() {
        let document = FakeDocument::new().build();
        let mut storage = MockStorage::new();
        storage
            .expect_save_document()
            .returning(|_| Err(ErrorKind::InvalidChangelogEvent("refused".to_string()).into()));

        let err = DocumentIndexer::new(storage)
            .save_document(&document)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("failed to cache document"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn indexer_update_metadata_replaces_the_cached_metadata() {
        let metadata = FakeMetadata::new().build();
        let id = metadata.id;
        let expected = metadata.clone();
        let mut storage = MockStorage::new();
        storage
            .expect_update_metadata()
            .withf(move |updated, metadata| *updated == id && *metadata == expected)
            .times(1)
            .returning(|_, _| Ok(()));

        DocumentIndexer::new(storage)
            .update_metadata(id, &metadata)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn indexer_delete_document_deletes_the_cached_document() {
        let id = Uuid::now_v7();
        let mut storage = MockStorage::new();
        storage
            .expect_delete_document()
            .withf(move |deleted| *deleted == id)
            .times(1)
            .returning(|_| Ok(()));

        DocumentIndexer::new(storage)
            .delete_document(id)
            .await
            .unwrap();
    }

    fn storage_for_a_new_upload() -> MockStorage {
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(1)
            .returning(|_| Ok(None));
        storage
    }

    fn client_classifying_with(
        changelog: Arc<RecordingChangelog>,
        classifier: Arc<dyn ClassificationService>,
    ) -> DocumentsClient<MockStorage> {
        DocumentsClient::new(
            storage_for_a_new_upload(),
            changelog,
            None,
            accepting_attributes(),
            classifier,
        )
    }

    /// The metadata of the only document `changelog` published.
    fn published_metadata(changelog: &RecordingChangelog) -> Metadata {
        let sent = changelog.sent.lock().unwrap();
        let [(EventType::SaveDocument, _, _, Some(metadata))] = &sent[..] else {
            panic!("unexpected events: {sent:?}");
        };
        metadata.clone()
    }

    #[tokio::test]
    async fn upload_publishes_the_document_classified_from_its_transcript() {
        let classification = FakeClassification::new().build();
        let mut classifier = MockClassificationService::new();
        classifier
            .expect_classify()
            .withf(|text| text.contains("Montant : 42 €"))
            .times(1)
            .returning(move |_| Ok(classification));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = client_classifying_with(changelog.clone(), Arc::new(classifier));

        client
            .upload(UploadRequest::from_raw(
                "a.pdf",
                build_pdf("Montant : 42 €"),
            ))
            .await
            .unwrap();

        let metadata = published_metadata(&changelog);
        assert_eq!(
            (metadata.category, metadata.subcategory, metadata.r#type),
            (
                classification.category,
                classification.subcategory,
                classification.r#type
            )
        );
    }

    #[tokio::test]
    async fn upload_leaves_unclassified_what_the_classifier_can_t_tell() {
        let classification = FakeClassification::new()
            .with_subcategory(None)
            .with_type(None)
            .build();
        let changelog = Arc::new(RecordingChangelog::default());
        let client =
            client_classifying_with(changelog.clone(), classifier_returning(classification, 1));

        client
            .upload(UploadRequest::from_raw(
                "a.pdf",
                build_pdf("Montant : 42 €"),
            ))
            .await
            .unwrap();

        let metadata = published_metadata(&changelog);
        assert_eq!(metadata.category, classification.category);
        assert_eq!((metadata.subcategory, metadata.r#type), (None, None));
    }

    #[tokio::test]
    async fn upload_never_classifies_a_document_given_a_category() {
        let changelog = Arc::new(RecordingChangelog::default());
        // Strict: classifying panics.
        let client = client_classifying_with(changelog.clone(), unused_classifier());
        let mut request = UploadRequest::from_raw("a.pdf", build_pdf("Montant : 42 €"));
        request.category = Some(Category::Housing);

        client.upload(request).await.unwrap();

        let metadata = published_metadata(&changelog);
        assert_eq!(metadata.category, Some(Category::Housing));
        assert_eq!((metadata.subcategory, metadata.r#type), (None, None));
    }

    #[tokio::test]
    async fn upload_never_classifies_a_document_given_a_subcategory_alone() {
        let changelog = Arc::new(RecordingChangelog::default());
        // Strict: classifying panics.
        let client = client_classifying_with(changelog.clone(), unused_classifier());
        let mut request = UploadRequest::from_raw("a.pdf", build_pdf("Montant : 42 €"));
        request.subcategory = Some(Subcategory::Water);

        client.upload(request).await.unwrap();

        let metadata = published_metadata(&changelog);
        assert_eq!(
            (metadata.category, metadata.subcategory),
            (Some(Category::Housing), Some(Subcategory::Water))
        );
    }

    #[tokio::test]
    async fn upload_classifies_a_document_given_a_type_alone_but_keeps_its_type() {
        let classification = FakeClassification::new()
            .with_category(Category::Work)
            .with_subcategory(Subcategory::Payslips)
            .with_type(DocumentType::Payslip)
            .build();
        let changelog = Arc::new(RecordingChangelog::default());
        let client =
            client_classifying_with(changelog.clone(), classifier_returning(classification, 1));
        let mut request = UploadRequest::from_raw("a.pdf", build_pdf("Montant : 42 €"));
        request.r#type = Some(DocumentType::Contract);

        client.upload(request).await.unwrap();

        let metadata = published_metadata(&changelog);
        assert_eq!(
            (metadata.category, metadata.subcategory, metadata.r#type),
            (
                Some(Category::Work),
                Some(Subcategory::Payslips),
                Some(DocumentType::Contract)
            )
        );
    }

    #[tokio::test]
    async fn upload_publishes_the_document_unclassified_when_classifying_fails() {
        let mut classifier = MockClassificationService::new();
        classifier
            .expect_classify()
            .times(1)
            .returning(|_| Err(ErrorKind::ModelDownloadStatus(503).into()));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = client_classifying_with(changelog.clone(), Arc::new(classifier));

        client
            .upload(UploadRequest::from_raw(
                "a.pdf",
                build_pdf("Montant : 42 €"),
            ))
            .await
            .unwrap();

        let metadata = published_metadata(&changelog);
        assert_eq!(
            (metadata.category, metadata.subcategory, metadata.r#type),
            (None, None, None)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn upload_skips_classifying_for_a_while_after_classifying_failed() {
        let mut classifier = MockClassificationService::new();
        let mut sequence = mockall::Sequence::new();
        classifier
            .expect_classify()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(ErrorKind::ModelDownloadStatus(503).into()));
        let classification = FakeClassification::new().build();
        classifier
            .expect_classify()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| Ok(classification));
        let mut storage = MockStorage::new();
        storage
            .expect_find_document_by_checksum()
            .times(3)
            .returning(|_| Ok(None));
        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            storage,
            changelog.clone(),
            None,
            accepting_attributes(),
            Arc::new(classifier),
        );
        let upload = |text: &str| UploadRequest::from_raw("a.pdf", build_pdf(text));

        client.upload(upload("un")).await.unwrap();
        // Within the delay: published without asking the classifier.
        tokio::time::advance(CLASSIFY_RETRY_AFTER / 2).await;
        client.upload(upload("deux")).await.unwrap();
        tokio::time::advance(CLASSIFY_RETRY_AFTER).await;
        client.upload(upload("trois")).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        let categories: Vec<_> = sent
            .iter()
            .map(|(_, _, _, metadata)| metadata.as_ref().unwrap().category)
            .collect();
        assert_eq!(categories, vec![None, None, classification.category]);
    }
}
