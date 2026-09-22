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

use crate::domains::documents::{EventType, Metadata};
use crate::domains::sessions::Service as SessionsService;
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
    pub id: String,
    pub name: String,
    pub original_name: String,
    pub content_type: String,
    pub created_at: i64,
    pub size: u64,
    pub checksum: String,
    pub transcript: String,
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
            id: document.id().to_string(),
            content: document.content().to_vec(),
            metadata: document.metadata().clone().into(),
        }
    }
}

/// The kind of write recorded by an [`FfiChangelogEvent`], mirroring
/// [`EventType`].
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiEventType {
    Created,
    UpdateMetadata,
    Deleted,
}

impl From<EventType> for FfiEventType {
    fn from(event_type: EventType) -> Self {
        match event_type {
            EventType::Created => FfiEventType::Created,
            EventType::UpdateMetadata => FfiEventType::UpdateMetadata,
            EventType::Deleted => FfiEventType::Deleted,
        }
    }
}

/// A single recorded write against a document, mirroring [`ChangelogEvent`]
/// across the FFI boundary. `id`/`document_id` cross as strings since
/// UniFFI has no native UUID type. `content`/`metadata` are `None` for
/// event types that don't carry them.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiChangelogEvent {
    pub id: String,
    pub event_type: FfiEventType,
    pub document_id: String,
    pub content: Option<Vec<u8>>,
    pub metadata: Option<FfiMetadata>,
}

impl From<ChangelogEvent> for FfiChangelogEvent {
    fn from(event: ChangelogEvent) -> Self {
        Self {
            id: event.id().to_string(),
            event_type: event.event_type().into(),
            document_id: event.document_id().to_string(),
            content: event.content().map(<[u8]>::to_vec),
            metadata: event.metadata().cloned().map(FfiMetadata::from),
        }
    }
}

/// Callback interface for [`FydeClient::sync`], implemented by foreign
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

    /// Creates a new account and opens a session for the device named
    /// `device_name`, returning its session token. Mirrors
    /// [`crate::UsersService::create`].
    pub async fn create_user(
        &self,
        username: String,
        password: String,
        device_name: String,
    ) -> Result<String, FfiError> {
        let token = self
            .inner
            .users()
            .create(&username, &password, &device_name)
            .await?;
        Ok(token)
    }

    /// Verifies `username`/`password` and opens a session for the device
    /// named `device_name`, returning its session token. Mirrors
    /// [`crate::UsersService::login`].
    pub async fn login(
        &self,
        username: String,
        password: String,
        device_name: String,
    ) -> Result<String, FfiError> {
        let token = self
            .inner
            .users()
            .login(&username, &password, &device_name)
            .await?;
        Ok(token)
    }

    /// Closes the session opened by the most recent `create_user`/`login`
    /// call. Mirrors [`crate::UsersService::logout`].
    pub async fn logout(&self) -> Result<(), FfiError> {
        self.inner.users().logout().await?;
        Ok(())
    }

    /// Persists `token` as the session token attached to every subsequent
    /// authenticated request, overwriting any token already stored.
    /// `create_user`/`login` already do this as part of opening a session;
    /// this is for restoring a previously-issued token (e.g. one a caller
    /// kept in secure storage across app restarts) without a network round
    /// trip. Mirrors `sessions::Service::save_new_session`.
    pub async fn save_new_session(&self, token: String) -> Result<(), FfiError> {
        self.inner.sessions().save_new_session(&token).await?;
        Ok(())
    }

    /// Removes the currently persisted session token, if any, so subsequent
    /// requests go out unauthenticated. Unlike `logout`, this never talks to
    /// the server — it only clears the local token. Mirrors
    /// `sessions::Service::remove_session`.
    pub async fn remove_session(&self) -> Result<(), FfiError> {
        self.inner.sessions().remove_session().await?;
        Ok(())
    }

    /// Returns whether a session token is currently persisted, i.e. a
    /// `create_user`/`login`/`save_new_session` call succeeded and neither
    /// `logout` nor `remove_session` has been called since. Mirrors
    /// `sessions::Service::is_connected`.
    pub async fn is_connected(&self) -> Result<bool, FfiError> {
        let connected = self.inner.sessions().is_connected().await?;
        Ok(connected)
    }

    /// Reads the local file at `path`, encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its generated
    /// id. Mirrors [`crate::DocumentsService::upload`].
    pub async fn upload_document(&self, path: String) -> Result<String, FfiError> {
        let id = self.inner.documents().upload(Path::new(&path)).await?;
        Ok(id.to_string())
    }

    /// Fetches a document previously cached locally by
    /// [`FydeClient::consume`], or `None` if it doesn't exist. Mirrors
    /// [`crate::DocumentsService::get`].
    pub async fn get_document(&self, id: String) -> Result<Option<FfiDocument>, FfiError> {
        let id = parse_uuid(&id)?;
        let document = self.inner.documents().get(id).await?;
        Ok(document.map(FfiDocument::from))
    }

    /// Lists documents previously cached locally, oldest first, one page at
    /// a time. Mirrors [`crate::DocumentsService::list`].
    pub async fn list_documents(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<FfiDocument>, FfiError> {
        let documents = self.inner.documents().list(offset, limit).await?;
        Ok(documents.into_iter().map(FfiDocument::from).collect())
    }

    /// Streams and decrypts every changelog event since the last consumed
    /// id, invoking `listener` once for each (replaying history, then
    /// continuing with the live tail). Resumes from the cursor persisted
    /// locally by a previous call, or from the beginning of the changelog
    /// if there is none. Runs until the server closes the stream or an
    /// error occurs. Mirrors [`crate::DocumentsService::sync`].
    pub async fn sync(&self, listener: Box<dyn ChangelogListener>) -> Result<(), FfiError> {
        self.inner
            .documents()
            .sync(Box::new(move |event| listener.on_event(event.into())))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::{EventType, FakeChangelogEvent, FakeDocument, FakeMetadata};

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
    fn event_type_conversion_maps_every_variant() {
        assert!(matches!(
            FfiEventType::from(EventType::Created),
            FfiEventType::Created
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
    fn changelog_event_conversion_stringifies_the_document_id_and_preserves_payload() {
        let id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let event = FakeChangelogEvent::new()
            .with_id(id)
            .with_event_type(EventType::Created)
            .with_document_id(document_id)
            .with_content(b"body".to_vec())
            .with_metadata(FakeMetadata::new().build())
            .build();

        let ffi: FfiChangelogEvent = event.into();

        assert_eq!(ffi.id, id.to_string());
        assert!(matches!(ffi.event_type, FfiEventType::Created));
        assert_eq!(ffi.document_id, document_id.to_string());
        assert_eq!(ffi.content, Some(b"body".to_vec()));
        assert!(ffi.metadata.is_some());
    }

    #[test]
    fn changelog_event_conversion_handles_events_with_no_content_or_metadata() {
        let event = FakeChangelogEvent::new()
            .with_event_type(EventType::Deleted)
            .with_document_id(Uuid::now_v7())
            .without_content()
            .without_metadata()
            .build();

        let ffi: FfiChangelogEvent = event.into();

        assert_eq!(ffi.content, None);
        assert!(ffi.metadata.is_none());
    }

    #[test]
    fn ffi_error_flattens_the_source_error_to_its_display_message() {
        let err = Error::UnsupportedDocumentExtension("txt".to_string());
        let message = err.to_string();

        let ffi_err: FfiError = err.into();

        assert_eq!(ffi_err.to_string(), message);
    }

    #[test]
    fn ffi_error_from_uuid_error_carries_a_message() {
        let uuid_err = Uuid::parse_str("not-a-uuid").unwrap_err();

        let ffi_err: FfiError = uuid_err.into();

        assert!(!ffi_err.to_string().is_empty());
    }
}
