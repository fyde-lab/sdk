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
use crate::domains::scripts::Script;
use crate::domains::sessions::Service as SessionsService;
use crate::{ChangelogEvent, Client, ClientConfig, Document, Error, LogLevel, Storage};

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
    pub r#type: String,
    pub source_category: String,
    pub source_sub_category: Option<String>,
    pub subject: String,
    pub qualification: String,
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
            r#type: metadata.r#type().to_string(),
            source_category: metadata.source_category().to_string(),
            source_sub_category: metadata.source_sub_category().map(str::to_string),
            subject: metadata.subject().to_string(),
            qualification: metadata.qualification().to_string(),
        }
    }
}

impl TryFrom<FfiMetadata> for Metadata {
    type Error = FfiError;

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
            r#type: metadata.r#type,
            source_category: metadata.source_category,
            source_sub_category: metadata.source_sub_category,
            subject: metadata.subject,
            qualification: metadata.qualification,
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

/// A user-authored script, mirroring [`Script`] across the FFI boundary.
/// `id` crosses as a string since UniFFI has no native UUID type.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiScript {
    pub id: String,
    pub name: String,
    pub is_public: bool,
    pub icon: Vec<u8>,
    pub version: u64,
    pub script: String,
    pub last_updated: i64,
}

impl From<Script> for FfiScript {
    fn from(script: Script) -> Self {
        Self {
            id: script.id().to_string(),
            name: script.name().to_string(),
            is_public: script.is_public(),
            icon: script.icon().to_vec(),
            version: script.version(),
            script: script.script().to_string(),
            last_updated: script.last_updated(),
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
    pub storage: FfiStorage,
    pub log_level: FfiLogLevel,
}

impl From<FfiClientConfig> for ClientConfig {
    fn from(config: FfiClientConfig) -> Self {
        Self {
            url: config.url,
            storage: config.storage.into(),
            log_level: config.log_level.into(),
            on_log: None,
            on_document_change: None,
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

/// A connection to a fyde server, exposed to Kotlin as `FydeClient`.
#[derive(uniffi::Object)]
pub struct FydeClient {
    inner: Client,
}

#[uniffi::export(async_runtime = "tokio")]
impl FydeClient {
    /// Connects to a fyde server per `config`. Mirrors [`Client::init`].
    ///
    /// Needs `async_runtime = "tokio"`: `Client::init` goes through sqlx's
    /// connection pool, which requires an active Tokio context to spawn its
    /// background tasks on — not just a generic future poller.
    #[uniffi::constructor(async_runtime = "tokio")]
    pub async fn init(
        config: FfiClientConfig,
        on_log: Option<Box<dyn LogListener>>,
        on_document_change: Option<Box<dyn DocumentChangeListener>>,
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

        let inner = Client::init(config).await?;
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

    /// Returns whether a session token is currently persisted, i.e. a
    /// `create_user`/`login`/`save_new_session` call succeeded and `logout`
    /// hasn't been called since. Mirrors `sessions::Service::is_connected`.
    pub async fn is_connected(&self) -> Result<bool, FfiError> {
        let connected = self.inner.sessions().is_connected().await?;
        Ok(connected)
    }

    /// Returns whether the fyde server currently responds as reachable.
    /// Mirrors [`crate::ServerStateService::is_server_reachable`].
    pub async fn is_server_reachable(&self) -> Result<bool, FfiError> {
        let reachable = self.inner.server_state().is_server_reachable().await?;
        Ok(reachable)
    }

    /// Reads the local file at `path`, encrypts it and its metadata, then
    /// publishes it as a "created" changelog event, returning its generated
    /// id. Mirrors [`crate::DocumentsService::upload`].
    pub async fn upload_document(&self, path: String) -> Result<String, FfiError> {
        let id = self.inner.documents().upload(Path::new(&path)).await?;
        Ok(id.to_string())
    }

    /// Fetches a document previously cached locally by the SDK's automatic
    /// changelog sync (see [`FydeClient::init`]'s `on_document_change`), or
    /// `None` if it doesn't exist. Mirrors [`crate::DocumentsService::get`].
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

    /// Encrypts `metadata` as given and publishes it as an "update metadata"
    /// changelog event, replacing whatever metadata was previously
    /// associated with `metadata.id`. Mirrors
    /// [`crate::DocumentsService::update_metadata`].
    pub async fn update_document_metadata(&self, metadata: FfiMetadata) -> Result<(), FfiError> {
        let metadata = Metadata::try_from(metadata)?;
        self.inner.documents().update_metadata(metadata).await?;
        Ok(())
    }

    /// Persists `language` as this device's UI language preference. Mirrors
    /// [`crate::UsersService::set_language`].
    pub async fn set_language(&self, language: String) -> Result<(), FfiError> {
        self.inner.users().set_language(&language).await?;
        Ok(())
    }

    /// Returns this device's UI language preference, or `None` if never
    /// set. Mirrors [`crate::UsersService::get_language`].
    pub async fn get_language(&self) -> Result<Option<String>, FfiError> {
        let language = self.inner.users().get_language().await?;
        Ok(language)
    }

    /// Creates a new script owned by the authenticated user, at version 1.
    /// Mirrors [`crate::ScriptsService::create_script`].
    pub async fn create_script(
        &self,
        name: String,
        is_public: bool,
        icon: Vec<u8>,
        script: String,
    ) -> Result<FfiScript, FfiError> {
        let script = self
            .inner
            .scripts()
            .create_script(&name, is_public, icon, &script)
            .await?;
        Ok(script.into())
    }

    /// Fetches the script matching `id`. Mirrors
    /// [`crate::ScriptsService::fetch_script`].
    pub async fn fetch_script(&self, id: String) -> Result<FfiScript, FfiError> {
        let id = parse_uuid(&id)?;
        let script = self.inner.scripts().fetch_script(id).await?;
        Ok(script.into())
    }

    /// Updates a script owned by the authenticated user, incrementing its
    /// version. Mirrors [`crate::ScriptsService::update_script`].
    pub async fn update_script(
        &self,
        id: String,
        name: String,
        is_public: bool,
        icon: Vec<u8>,
        script: String,
    ) -> Result<FfiScript, FfiError> {
        let id = parse_uuid(&id)?;
        let script = self
            .inner
            .scripts()
            .update_script(id, &name, is_public, icon, &script)
            .await?;
        Ok(script.into())
    }

    /// Enables `script_id` for the authenticated user. Mirrors
    /// [`crate::ScriptsService::enable_script`].
    pub async fn enable_script(&self, script_id: String) -> Result<(), FfiError> {
        let script_id = parse_uuid(&script_id)?;
        self.inner.scripts().enable_script(script_id).await?;
        Ok(())
    }

    /// Disables `script_id` for the authenticated user. Mirrors
    /// [`crate::ScriptsService::disable_script`].
    pub async fn disable_script(&self, script_id: String) -> Result<(), FfiError> {
        let script_id = parse_uuid(&script_id)?;
        self.inner.scripts().disable_script(script_id).await?;
        Ok(())
    }

    /// Lists the scripts currently enabled for the authenticated user.
    /// Mirrors [`crate::ScriptsService::list_user_scripts`].
    pub async fn list_user_scripts(&self) -> Result<Vec<FfiScript>, FfiError> {
        let scripts = self.inner.scripts().list_user_scripts().await?;
        Ok(scripts.into_iter().map(FfiScript::from).collect())
    }

    /// Runs every script currently enabled for the authenticated user
    /// against the document matching `document_id`. Mirrors
    /// [`crate::ScriptsService::run_for_document`].
    pub async fn run_scripts_for_document(&self, document_id: String) -> Result<(), FfiError> {
        let id = parse_uuid(&document_id)?;
        let document = self
            .inner
            .documents()
            .get(id)
            .await?
            .ok_or(Error::DocumentNotFound(id))?;
        self.inner.scripts().run_for_document(&document).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::{EventType, FakeChangelogEvent, FakeDocument, FakeMetadata};
    use crate::domains::scripts::FakeScript;

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
        assert_eq!(ffi.r#type, metadata.r#type());
        assert_eq!(ffi.source_category, metadata.source_category());
        assert_eq!(
            ffi.source_sub_category.as_deref(),
            metadata.source_sub_category()
        );
        assert_eq!(ffi.subject, metadata.subject());
        assert_eq!(ffi.qualification, metadata.qualification());
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
        assert_eq!(ffi.is_public, script.is_public());
        assert_eq!(ffi.icon, script.icon());
        assert_eq!(ffi.version, script.version());
        assert_eq!(ffi.script, script.script());
        assert_eq!(ffi.last_updated, script.last_updated());
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
