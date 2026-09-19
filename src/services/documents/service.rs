use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::services::changelog::{EventType, Service as ChangelogService};
use crate::{Error, ErrorContext as _, Result};

use super::storage::Storage;
use super::{Document, Metadata, Service, transcript};

/// The only content type [`Service::upload`] currently accepts.
pub(super) const PDF_CONTENT_TYPE: &str = "application/pdf";

/// The default [`Service`] implementation: publishes new documents as
/// encrypted changelog events via an injected [`ChangelogService`], and
/// delegates local, unencrypted caching to an injected [`Storage`].
pub(super) struct DocumentsClient<S: Storage> {
    storage: S,
    changelog: Arc<dyn ChangelogService>,
}

impl<S: Storage> DocumentsClient<S> {
    /// Creates a documents client using `storage` to cache documents
    /// locally, and `changelog` to publish new ones.
    pub(super) fn new(storage: S, changelog: Arc<dyn ChangelogService>) -> Self {
        Self { storage, changelog }
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

        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut hasher = Sha256::new();
        hasher.update(&content);

        let doc_transcript =
            transcript::extract(&content).context("failed to extract document transcript")?;

        let metadata = Metadata {
            name,
            content_type: PDF_CONTENT_TYPE.to_string(),
            created_at,
            size: content.len() as u64,
            checksum: hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            transcript: doc_transcript,
        };

        let id = Uuid::new_v4();

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
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Mutex;

    use sqlx::sqlite::SqlitePoolOptions;
    use tempfile::NamedTempFile;

    use crate::services::changelog::ChangelogEvent;

    use super::super::storage_sqlite::SqliteStorage;
    use super::*;

    /// A single `send` call recorded by [`RecordingChangelog`].
    type SentEvent = (EventType, Uuid, Option<Vec<u8>>, Option<Metadata>);

    /// A [`ChangelogService`] fake recording every event passed to
    /// [`ChangelogService::send`], for tests that don't need a live server.
    /// `consume` is never exercised through `DocumentsClient`.
    #[derive(Default)]
    struct RecordingChangelog {
        sent: Mutex<Vec<SentEvent>>,
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

        async fn consume(&self, _callback: Box<dyn FnMut(ChangelogEvent) + Send>) -> Result<()> {
            unimplemented!("not exercised by these tests")
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

    async fn setup_storage() -> SqliteStorage {
        // A single connection, so all queries in a test hit the same
        // in-memory database rather than each getting its own.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteStorage::new(pool)
    }

    #[tokio::test]
    async fn upload_publishes_a_created_event_for_the_document() {
        let pdf = super::super::transcript::tests::build_pdf("hello world");
        let file = write_temp_file("pdf", &pdf);

        let changelog = Arc::new(RecordingChangelog::default());
        let client = DocumentsClient::new(setup_storage().await, changelog.clone());

        client.upload(file.path()).await.unwrap();

        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, _id, content, metadata) = &sent[0];
        assert_eq!(*event_type, EventType::Created);
        assert_eq!(content.as_deref(), Some(pdf.as_slice()));
        assert_eq!(
            metadata
                .as_ref()
                .map(|metadata| metadata.transcript.as_str()),
            Some("hello world\n")
        );
    }

    #[tokio::test]
    async fn upload_rejects_non_pdf_extensions() {
        let file = write_temp_file("txt", b"hello world");
        let client = DocumentsClient::new(
            setup_storage().await,
            Arc::new(RecordingChangelog::default()),
        );

        let result = client.upload(file.path()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_returns_a_document_previously_cached_locally() {
        let storage = setup_storage().await;
        let id = Uuid::new_v4();
        let document = Document {
            id,
            content: b"hello".to_vec(),
            metadata: Metadata {
                name: "report.pdf".to_string(),
                content_type: PDF_CONTENT_TYPE.to_string(),
                created_at: 1_700_000_000,
                size: 5,
                checksum: "deadbeef".to_string(),
                transcript: String::new(),
            },
        };
        storage.save_document(&document).await.unwrap();

        let client = DocumentsClient::new(storage, Arc::new(RecordingChangelog::default()));

        assert_eq!(client.get(id).await.unwrap(), Some(document));
    }

    #[tokio::test]
    async fn list_pages_through_documents_previously_cached_locally() {
        let storage = setup_storage().await;
        for (i, name) in ["one", "two", "three"].iter().enumerate() {
            storage
                .save_document(&Document {
                    id: Uuid::new_v4(),
                    content: Vec::new(),
                    metadata: Metadata {
                        name: name.to_string(),
                        content_type: PDF_CONTENT_TYPE.to_string(),
                        created_at: 1_700_000_000 + i as i64,
                        size: 0,
                        checksum: String::new(),
                        transcript: String::new(),
                    },
                })
                .await
                .unwrap();
        }
        let client = DocumentsClient::new(storage, Arc::new(RecordingChangelog::default()));

        let page = client.list(1, 2).await.unwrap();

        assert_eq!(
            page.into_iter()
                .map(|document| document.metadata.name)
                .collect::<Vec<_>>(),
            vec!["two", "three"]
        );
    }
}
