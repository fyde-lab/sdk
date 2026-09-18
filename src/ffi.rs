//! Foreign-function interface exposing the SDK to non-Rust callers (currently
//! the Kotlin Multiplatform `application`) via UniFFI-generated bindings.
//!
//! [`FydeClient`] mirrors [`Client`] plus its documents and changelog
//! operations; domain types are mirrored as `Ffi*` records/enums since
//! UniFFI has no native `Uuid` type and cannot export the crate's own
//! [`Document`]/[`ChangelogEvent`] types directly.

use std::path::Path;
use std::sync::Arc;

use uuid::Uuid;

use crate::services::changelog::EventType;
use crate::services::documents::Metadata;
use crate::{ChangelogEvent, Client, Document, Error};

/// Error type surfaced to FFI callers. UniFFI requires exported errors to be
/// their own type rather than the crate's own [`Error`], so every failure is
/// flattened to its display message here.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("{0}")]
    Failed(String),
}

impl From<Error> for FfiError {
    fn from(err: Error) -> Self {
        FfiError::Failed(err.to_string())
    }
}

impl From<uuid::Error> for FfiError {
    fn from(err: uuid::Error) -> Self {
        Error::from(err).into()
    }
}

fn parse_uuid(id: &str) -> Result<Uuid, FfiError> {
    Ok(Uuid::parse_str(id)?)
}

/// A document's cleartext metadata, mirroring [`Metadata`] across the FFI
/// boundary.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMetadata {
    pub name: String,
    pub content_type: String,
    pub created_at: i64,
    pub size: u64,
    pub checksum: String,
    pub transcript: String,
}

impl From<Metadata> for FfiMetadata {
    fn from(metadata: Metadata) -> Self {
        Self {
            name: metadata.name,
            content_type: metadata.content_type,
            created_at: metadata.created_at,
            size: metadata.size,
            checksum: metadata.checksum,
            transcript: metadata.transcript,
        }
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
            id: document.id.to_string(),
            content: document.content,
            metadata: document.metadata.into(),
        }
    }
}

/// The kind of write recorded by an [`FfiChangelogEvent`], mirroring
/// [`EventType`].
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiEventType {
    Created,
    Updated,
    Deleted,
}

impl From<EventType> for FfiEventType {
    fn from(event_type: EventType) -> Self {
        match event_type {
            EventType::Created => FfiEventType::Created,
            EventType::Updated => FfiEventType::Updated,
            EventType::Deleted => FfiEventType::Deleted,
        }
    }
}

/// A single recorded write against a document, mirroring [`ChangelogEvent`]
/// across the FFI boundary. `document_id` crosses as a string since UniFFI
/// has no native UUID type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiChangelogEvent {
    pub offset: i64,
    pub document_id: String,
    pub event_type: FfiEventType,
    pub created_at: i64,
}

impl From<ChangelogEvent> for FfiChangelogEvent {
    fn from(event: ChangelogEvent) -> Self {
        Self {
            offset: event.offset,
            document_id: event.document_id.to_string(),
            event_type: event.event_type.into(),
            created_at: event.created_at,
        }
    }
}

/// Callback interface for [`FydeClient::consume`], implemented by foreign
/// (Kotlin/Swift) callers and invoked once per [`FfiChangelogEvent`]
/// encountered during changelog catch-up and live streaming.
#[uniffi::export(callback_interface)]
pub trait ChangelogListener: Send + Sync {
    fn on_event(&self, event: FfiChangelogEvent);
}

/// A connection to a fyde server, exposed to Kotlin as `FydeClient`.
#[derive(uniffi::Object)]
pub struct FydeClient {
    inner: Client,
}

#[uniffi::export(async_runtime = "tokio")]
impl FydeClient {
    /// Connects to a fyde server at `url`, backed by the default on-disk
    /// SQLite database. Mirrors [`Client::connect`].
    ///
    /// Needs `async_runtime = "tokio"`: `Client::connect` goes through
    /// sqlx's connection pool, which requires an active Tokio context to
    /// spawn its background tasks on — not just a generic future poller.
    #[uniffi::constructor(async_runtime = "tokio")]
    pub async fn connect(url: String) -> Result<Arc<Self>, FfiError> {
        let inner = Client::connect(url).await?;
        Ok(Arc::new(Self { inner }))
    }

    /// Connects to a fyde server backed by a private in-memory database.
    /// Mirrors [`Client::connect_memory`].
    #[uniffi::constructor(async_runtime = "tokio")]
    pub async fn connect_memory(url: String) -> Result<Arc<Self>, FfiError> {
        let inner = Client::connect_memory(url).await?;
        Ok(Arc::new(Self { inner }))
    }

    /// Reads the local file at `path`, encrypts it and its metadata, then
    /// uploads it as a new document to the server, returning its generated
    /// id. Mirrors [`crate::DocumentsService::upload`].
    pub async fn upload_document(&self, path: String) -> Result<String, FfiError> {
        let id = self.inner.documents().upload(Path::new(&path)).await?;
        Ok(id.to_string())
    }

    /// Fetches a document's content by id from the server, or `None` if it
    /// doesn't exist. Mirrors [`crate::DocumentsService::download`].
    pub async fn download_document(&self, id: String) -> Result<Option<FfiDocument>, FfiError> {
        let id = parse_uuid(&id)?;
        let document = self.inner.documents().download(id).await?;
        Ok(document.map(FfiDocument::from))
    }

    /// Fetches a document previously cached locally by
    /// [`FydeClient::download_document`] or [`FydeClient::download_documents`],
    /// or `None` if it doesn't exist. Unlike [`FydeClient::download_document`],
    /// this does not talk to the server. Mirrors [`crate::DocumentsService::get`].
    pub async fn get_document(&self, id: String) -> Result<Option<FfiDocument>, FfiError> {
        let id = parse_uuid(&id)?;
        let document = self.inner.documents().get(id).await?;
        Ok(document.map(FfiDocument::from))
    }

    /// Fetches multiple documents' content by id in a single call. Ids that
    /// don't exist are omitted from the result. Mirrors
    /// [`crate::DocumentsService::download_many`].
    pub async fn download_documents(&self, ids: Vec<String>) -> Result<Vec<FfiDocument>, FfiError> {
        let ids = ids
            .iter()
            .map(|id| parse_uuid(id))
            .collect::<Result<Vec<_>, _>>()?;
        let documents = self.inner.documents().download_many(ids).await?;
        Ok(documents.into_iter().map(FfiDocument::from).collect())
    }

    /// Lists documents previously cached locally, oldest first, one page at
    /// a time. Like [`FydeClient::get_document`], this does not talk to the
    /// server. Mirrors [`crate::DocumentsService::list`].
    pub async fn list_documents(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<FfiDocument>, FfiError> {
        let documents = self.inner.documents().list(offset, limit).await?;
        Ok(documents.into_iter().map(FfiDocument::from).collect())
    }

    /// Subscribes to the server's changelog, invoking `listener` once for
    /// every event encountered during both catch-up and live streaming.
    /// Runs until the server closes the stream or an error occurs. Mirrors
    /// [`crate::ChangelogService::consume`].
    pub async fn consume(&self, listener: Box<dyn ChangelogListener>) -> Result<(), FfiError> {
        self.inner
            .changelog()
            .consume(Box::new(move |event| listener.on_event(event.into())))
            .await?;
        Ok(())
    }
}
