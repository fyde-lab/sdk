use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{Error, ErrorContext as _, Result};

use super::changelog::{ChangelogEvent, EventType, Service as ChangelogService};
use super::parser::Service as ParserService;
use super::storage::Storage;
use super::{Document, Metadata, Service};

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
    async fn upload(&self, path: &Path) -> Result<Uuid> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension != "pdf" {
            return Err(Error::UnsupportedDocumentExtension(extension));
        }

        let content = tokio::fs::read(path)
            .await
            .with_context(|| format!("failed to read document at {}", path.display()))?;

        let original_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let metadata = self.parser.parse_content(&content, &original_name).await?;
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
    }

    #[async_trait]
    impl ChangelogService for RecordingChangelog {
        async fn send(
            &self,
            event_type: EventType,
            document_id: Uuid,
            content: Option<&[u8]>,
            metadata: Option<&Metadata>,
        ) -> Result<()> {
            self.sent.lock().unwrap().push((
                event_type,
                document_id,
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

        let id = client.upload(file.path()).await.unwrap();

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

        let result = client.upload(file.path()).await;

        assert!(result.is_err());
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
                source_category: String::new(),
                source_sub_category: None,
                subject: String::new(),
                qualification: String::new(),
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
                    source_category: String::new(),
                    source_sub_category: None,
                    subject: String::new(),
                    qualification: String::new(),
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
                    source_category: String::new(),
                    source_sub_category: None,
                    subject: String::new(),
                    qualification: String::new(),
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
    async fn update_metadata_publishes_an_update_metadata_event_with_the_given_metadata() {
        let metadata = super::super::FakeMetadata::new()
            .with_name("new.pdf")
            .with_subject("new-subject")
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
        assert_eq!(metadata.subject, "new-subject");
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
