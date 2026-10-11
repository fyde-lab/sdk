use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{ErrorContext as _, ErrorKind, Result};

use crate::domains::changelog::{ChangelogEvent, EventType, Service as ChangelogService};

use super::parser::Service as ParserService;
use super::storage::{FacetField, Storage};
use super::{
    Document, FacetCount, Metadata, SearchFacets, SearchFilters, SearchQuery, Service,
    UploadRequest, UploadSource,
};

/// The default [`Service`] implementation: publishes new documents as
/// encrypted changelog events via an injected [`ChangelogService`], and
/// delegates local, unencrypted caching to an injected [`Storage`].
pub(super) struct DocumentsClient<S: Storage> {
    storage: S,
    changelog: Arc<dyn ChangelogService>,
    parser: Arc<dyn ParserService>,
    /// Invoked once per event by [`Service::start_sync`]'s consume job, when
    /// set — see [`crate::ClientConfig::on_document_change`].
    on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
}

impl<S: Storage> DocumentsClient<S> {
    /// Creates a documents client using `storage` to cache documents
    /// locally, `changelog` to publish new ones and drive
    /// [`Service::start_sync`], `parser` to derive a newly uploaded
    /// document's metadata and to fill in its classification metadata (see
    /// [`Service::run_scripts`]), and `on_document_change` as the callback
    /// [`Service::start_sync`] invokes per consumed event.
    pub(super) fn new(
        storage: S,
        changelog: Arc<dyn ChangelogService>,
        parser: Arc<dyn ParserService>,
        on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
    ) -> Self {
        Self {
            storage,
            changelog,
            parser,
            on_document_change,
        }
    }
}

impl<S: Storage> DocumentsClient<S> {
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
            FacetField::SourceCategory => filters.source_categories.clear(),
            FacetField::SourceSubCategory => filters.source_sub_categories.clear(),
            FacetField::Purpose => filters.purposes.clear(),
            FacetField::Type => filters.types.clear(),
            FacetField::ContentType => filters.content_types.clear(),
            FacetField::Subject => filters.subjects.clear(),
        }

        self.storage
            .count_documents_by(terms, &filters, field)
            .await
            .with_context(|| format!("failed to count matching documents by {field:?}"))
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
    async fn upload(&self, request: UploadRequest) -> Result<Uuid> {
        // Checked before anything else — parsing below also lists and runs
        // the user's enabled scripts against the document (see
        // `Self::run_scripts`), which needs a session just as much as
        // actually publishing the event does. Failing fast here means a
        // caller who never logged in gets this specific, recognizable
        // error instead of whatever that unrelated, also-authenticated
        // work happens to fail with first.
        self.changelog
            .ensure_master_key()
            .await
            .context("cannot upload a document before logging in")?;

        let (content, original_name) = match request.source {
            UploadSource::Path(path) => {
                check_extension(&path)?;
                let content = tokio::fs::read(&path)
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

        let mut metadata = self
            .parser
            .parse_content(&content, &original_name)
            .await
            .with_context(|| format!("failed to parse document {original_name:?}"))?;
        if let Some(name) = request.name {
            metadata.name = name;
        }
        if let Some(r#type) = request.r#type {
            metadata.r#type = r#type;
        }
        if let Some(source_category) = request.source_category {
            metadata.source_category = Some(source_category);
        }
        if let Some(source_sub_category) = request.source_sub_category {
            metadata.source_sub_category = Some(source_sub_category);
        }
        if let Some(subjects) = request.subjects {
            metadata.subjects = subjects;
        }
        if let Some(purpose) = request.purpose {
            metadata.purpose = Some(purpose);
        }

        let id = metadata.id;

        self.changelog
            .send(EventType::Created, id, Some(&content), Some(&metadata))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        Ok(id)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Document>> {
        self.storage.get_document(id).await
    }

    async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Document>> {
        self.storage.list_documents(offset, limit).await
    }

    async fn search(&self, query: SearchQuery) -> Result<Vec<Document>> {
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
        let source_categories = self
            .count_documents_by(&terms, filters, FacetField::SourceCategory)
            .await?;
        let source_sub_categories = self
            .count_documents_by(&terms, filters, FacetField::SourceSubCategory)
            .await?;
        let purposes = self
            .count_documents_by(&terms, filters, FacetField::Purpose)
            .await?;
        let created_at_range = self
            .storage
            .created_at_range(
                &terms,
                &SearchFilters {
                    created_from: None,
                    created_until: None,
                    ..filters.clone()
                },
            )
            .await
            .context("failed to find the creation date range of matching documents")?;

        Ok(SearchFacets {
            source_categories: parse_facet(source_categories, "source category")?,
            source_sub_categories: parse_facet(source_sub_categories, "source sub-category")?,
            purposes: parse_facet(purposes, "purpose")?,
            types: self
                .count_documents_by(&terms, filters, FacetField::Type)
                .await?,
            content_types: self
                .count_documents_by(&terms, filters, FacetField::ContentType)
                .await?,
            subjects: self
                .count_documents_by(&terms, filters, FacetField::Subject)
                .await?,
            earliest_created_at: created_at_range.map(|(earliest, _)| earliest),
            latest_created_at: created_at_range.map(|(_, latest)| latest),
        })
    }

    async fn run_scripts(&self, document: &Document) -> Result<Metadata> {
        self.parser.run_scripts(document).await
    }

    async fn update_metadata(&self, metadata: Metadata) -> Result<()> {
        let id = metadata.id;

        self.changelog
            .send(EventType::UpdateMetadata, id, None, Some(&metadata))
            .await
            .with_context(|| format!("failed to publish changelog event for document {id}"))?;

        Ok(())
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Mutex;

    use tempfile::NamedTempFile;

    use super::super::parser::MockService as MockParserService;
    use super::super::storage::MockStorage;
    use super::*;

    /// A [`ParserService`] fake that's never expected to be called, for
    /// tests that don't exercise document parsing/script running.
    fn no_op_parser() -> Arc<dyn ParserService> {
        Arc::new(MockParserService::new())
    }

    #[tokio::test]
    async fn upload_fails_fast_with_no_master_key_before_ever_parsing_the_document() {
        let file = write_temp_file("pdf", b"hello world");

        let changelog = Arc::new(RecordingChangelog {
            no_master_key: true,
            ..Default::default()
        });
        // `no_op_parser` panics on any call, proving `upload` never reaches
        // parsing (and so never lists/runs scripts) once this check fails.
        let client = DocumentsClient::new(MockStorage::new(), changelog, no_op_parser(), None);

        let err = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("no master key found"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn run_scripts_delegates_to_the_parser_service() {
        let document = super::super::FakeDocument::new().build();
        let expected = super::super::FakeMetadata::new().build();

        let mut parser = MockParserService::new();
        let returned = expected.clone();
        parser
            .expect_run_scripts()
            .withf({
                let expected_id = document.id();
                move |queried_document| queried_document.id() == expected_id
            })
            .times(1)
            .returning(move |_| Ok(returned.clone()));

        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            Arc::new(parser),
            None,
        );

        let metadata = client.run_scripts(&document).await.unwrap();

        assert_eq!(metadata, expected);
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
    async fn upload_publishes_a_created_event_for_the_document() {
        let pdf = b"hello world".to_vec();
        let file = write_temp_file("pdf", &pdf);
        let file_name = file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let metadata = super::super::FakeMetadata::new().build();
        let expected_content = pdf.clone();
        let expected_name = file_name.clone();
        let returned_metadata = metadata.clone();
        let mut parser = MockParserService::new();
        parser
            .expect_parse_content()
            .withf(move |content, original_name| {
                content == expected_content.as_slice() && original_name == expected_name
            })
            .times(1)
            .returning(move |_, _| Ok(returned_metadata.clone()));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            Arc::new(parser),
            None,
        );

        let id = client
            .upload(UploadRequest::from_path(file.path()))
            .await
            .unwrap();

        assert_eq!(id, metadata.id);
        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, document_id, content, sent_metadata) = &sent[0];
        assert_eq!(*event_type, EventType::Created);
        assert_eq!(*document_id, metadata.id);
        assert_eq!(content.as_deref(), Some(pdf.as_slice()));
        assert_eq!(sent_metadata.as_ref(), Some(&metadata));
    }

    #[tokio::test]
    async fn upload_rejects_non_pdf_extensions() {
        let file = write_temp_file("txt", b"hello world");
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            no_op_parser(),
            None,
        );

        let result = client.upload(UploadRequest::from_path(file.path())).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn upload_applies_metadata_overrides_from_the_request() {
        let raw = b"hello world".to_vec();
        let parsed = super::super::FakeMetadata::new()
            .with_name("original.pdf")
            .build();

        let expected_content = raw.clone();
        let returned_metadata = parsed.clone();
        let mut parser = MockParserService::new();
        parser
            .expect_parse_content()
            .withf(move |content, original_name| {
                content == expected_content.as_slice() && original_name == "raw.pdf"
            })
            .times(1)
            .returning(move |_, _| Ok(returned_metadata.clone()));

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(
            MockStorage::new(),
            changelog.clone(),
            Arc::new(parser),
            None,
        );

        let request = UploadRequest {
            source: super::super::UploadSource::Raw {
                name: "raw.pdf".to_string(),
                content: raw.clone(),
            },
            name: Some("overridden.pdf".to_string()),
            r#type: Some("payslip".to_string()),
            source_category: Some(super::super::SourceCategory::Employer),
            source_sub_category: None,
            subjects: Some(vec!["salary".to_string()]),
            purpose: Some(super::super::Purpose::Employment),
        };

        client.upload(request).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (_, _, _, sent_metadata) = &sent[0];
        let sent_metadata = sent_metadata.as_ref().unwrap();
        assert_eq!(sent_metadata.name, "overridden.pdf");
        assert_eq!(sent_metadata.r#type, "payslip");
        assert_eq!(
            sent_metadata.source_category,
            Some(super::super::SourceCategory::Employer)
        );
        assert_eq!(sent_metadata.subjects, vec!["salary".to_string()]);
        assert_eq!(
            sent_metadata.purpose,
            Some(super::super::Purpose::Employment)
        );
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
                content_type: super::super::parser::PDF_CONTENT_TYPE.to_string(),
                created_at: 1_700_000_000,
                size: 5,
                checksum: "deadbeef".to_string(),
                transcript: String::new(),
                r#type: String::new(),
                source_category: None,
                source_sub_category: None,
                subjects: Vec::new(),
                purpose: None,
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
            no_op_parser(),
            None,
        );

        assert_eq!(client.get(id).await.unwrap(), Some(document));
    }

    #[tokio::test]
    async fn list_pages_through_documents_previously_cached_locally() {
        let (id_two, id_three) = (Uuid::now_v7(), Uuid::now_v7());
        let page = vec![
            Document {
                id: id_two,
                content: Vec::new(),
                metadata: Metadata {
                    id: id_two,
                    name: "two".to_string(),
                    original_name: "two".to_string(),
                    content_type: super::super::parser::PDF_CONTENT_TYPE.to_string(),
                    created_at: 1_700_000_001,
                    size: 0,
                    checksum: String::new(),
                    transcript: String::new(),
                    r#type: String::new(),
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            },
            Document {
                id: id_three,
                content: Vec::new(),
                metadata: Metadata {
                    id: id_three,
                    name: "three".to_string(),
                    original_name: "three".to_string(),
                    content_type: super::super::parser::PDF_CONTENT_TYPE.to_string(),
                    created_at: 1_700_000_002,
                    size: 0,
                    checksum: String::new(),
                    transcript: String::new(),
                    r#type: String::new(),
                    source_category: None,
                    source_sub_category: None,
                    subjects: Vec::new(),
                    purpose: None,
                },
            },
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
            no_op_parser(),
            None,
        );

        let page = client.list(1, 2).await.unwrap();

        assert_eq!(
            page.into_iter()
                .map(|document| document.metadata.name)
                .collect::<Vec<_>>(),
            vec!["two", "three"]
        );
    }

    #[tokio::test]
    async fn search_splits_the_text_into_terms_and_forwards_the_query_to_storage() {
        let filters = super::super::SearchFilters {
            source_categories: vec![super::super::SourceCategory::Bank],
            created_from: Some(1),
            created_until: Some(2),
            ..Default::default()
        };
        let found = vec![super::super::FakeDocument::new().build()];

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
            no_op_parser(),
            None,
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
            no_op_parser(),
            None,
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
                no_op_parser(),
                None,
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
            FacetField::SourceCategory => filters.source_categories.clear(),
            FacetField::SourceSubCategory => filters.source_sub_categories.clear(),
            FacetField::Purpose => filters.purposes.clear(),
            FacetField::Type => filters.types.clear(),
            FacetField::ContentType => filters.content_types.clear(),
            FacetField::Subject => filters.subjects.clear(),
        }
        filters
    }

    #[tokio::test]
    async fn search_facets_counts_each_field_without_its_own_filter_but_with_every_other() {
        use super::super::{Purpose, SearchFilters, SourceCategory, SourceSubCategory};

        let filters = SearchFilters {
            source_categories: vec![SourceCategory::Bank],
            source_sub_categories: vec![SourceSubCategory::Tax],
            purposes: vec![Purpose::Invoice],
            types: vec!["statement".to_string()],
            subjects: vec!["rent".to_string()],
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
                FacetField::SourceCategory,
                as_strings(
                    expected
                        .source_categories
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.source_categories.iter().map(|f| f.count).collect(),
                ),
            ),
            (
                FacetField::SourceSubCategory,
                as_strings(
                    expected
                        .source_sub_categories
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected
                        .source_sub_categories
                        .iter()
                        .map(|f| f.count)
                        .collect(),
                ),
            ),
            (
                FacetField::Purpose,
                as_strings(
                    expected
                        .purposes
                        .iter()
                        .map(|f| f.value.as_str().to_string())
                        .collect(),
                    expected.purposes.iter().map(|f| f.count).collect(),
                ),
            ),
            (FacetField::Type, expected.types.clone()),
            (FacetField::ContentType, expected.content_types.clone()),
            (FacetField::Subject, expected.subjects.clone()),
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
            no_op_parser(),
            None,
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
                Ok(if field == FacetField::SourceCategory {
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
            no_op_parser(),
            None,
        );

        let err = client
            .search_facets("", &super::super::SearchFilters::default())
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidSourceCategory);
    }

    #[tokio::test]
    async fn search_facets_rejects_an_inverted_date_range_without_querying_storage() {
        let client = DocumentsClient::new(
            MockStorage::new(),
            Arc::new(RecordingChangelog::default()),
            no_op_parser(),
            None,
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
            .with_subjects(vec!["new-subject".to_string()])
            .build();
        let id = metadata.id;

        let changelog = Arc::new(RecordingChangelog::default());
        let client =
            DocumentsClient::new(MockStorage::new(), changelog.clone(), no_op_parser(), None);

        client.update_metadata(metadata).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, document_id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::UpdateMetadata);
        assert_eq!(*document_id, id);
        assert_eq!(*content, None);
        let metadata = metadata.as_ref().unwrap();
        assert_eq!(metadata.name, "new.pdf");
        assert_eq!(metadata.subjects, vec!["new-subject".to_string()]);
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
            no_op_parser(),
            Some(on_document_change),
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
        let client = DocumentsClient::new(MockStorage::new(), changelog, no_op_parser(), None);

        client.start_sync().await.unwrap();
    }

    #[tokio::test]
    async fn stop_sync_delegates_to_the_changelog_services_stop_consume_job() {
        let changelog = Arc::new(RecordingChangelog::default());
        let client =
            DocumentsClient::new(MockStorage::new(), changelog.clone(), no_op_parser(), None);

        client.stop_sync();

        assert!(changelog.stopped.load(std::sync::atomic::Ordering::SeqCst));
    }
}
