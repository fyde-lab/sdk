mod domains;
mod ffi;
#[path = "tools/log/mod.rs"]
mod log;
#[path = "tools/sql/mod.rs"]
mod sql;
#[cfg(test)]
#[path = "tools/testing/mod.rs"]
mod testing;

uniffi::setup_scaffolding!();

pub use ffi::{FfiError, FydeClient};

use std::sync::Arc;

pub use domains::documents::{ChangelogEvent, Document, Metadata, Service as DocumentsService};
pub use domains::scripts::Service as ScriptsService;
pub use domains::server_state::Service as ServerStateService;
pub use domains::settings::Service as SettingsService;
pub use domains::users::Service as UsersService;
pub use log::LogLevel;

use domains::sessions::{Service as SessionsService, SessionsClient};
use log::CallbackLayer;
use sql::SqliteClient;
use tonic::transport::Endpoint;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("grpc transport error: {0}")]
    GrpcTransport(#[from] tonic::transport::Error),
    #[error("grpc error: {0}")]
    Grpc(#[from] tonic::Status),
    #[error("invalid grpc endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("invalid uuid: {0}")]
    InvalidUuid(#[from] uuid::Error),
    #[error("invalid integer: {0}")]
    InvalidInteger(#[from] std::num::ParseIntError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("database migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("xdg base directories error: {0}")]
    Xdg(#[from] xdg::BaseDirectoriesError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pdf error: {0}")]
    Pdf(#[from] lopdf::Error),
    #[error("lua error: {0}")]
    Lua(#[from] mlua::Error),
    #[error("unsupported document extension {0:?}: only .pdf is supported")]
    UnsupportedDocumentExtension(String),
    #[error("document {0} not found in local cache")]
    DocumentNotFound(uuid::Uuid),
    #[error("invalid changelog event: {0}")]
    InvalidChangelogEvent(String),
    #[error("invalid server response: {0}")]
    InvalidResponse(String),
    #[error("encryption error: {0}")]
    Encryption(String),
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("messagepack encode error: {0}")]
    MessagePackEncode(#[from] rmp_serde::encode::Error),
    #[error("messagepack decode error: {0}")]
    MessagePackDecode(#[from] rmp_serde::decode::Error),
    #[error("{message}: {source}")]
    Context {
        message: String,
        #[source]
        source: Box<Error>,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Attaches a human-readable message to a fallible operation's error,
/// preserving the original error as its source. Used throughout the crate
/// so every `?` site reports what it was trying to do, not just the bare
/// underlying error.
pub(crate) trait ErrorContext<T> {
    fn context(self, message: &str) -> Result<T>;
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T>;
}

impl<T, E> ErrorContext<T> for std::result::Result<T, E>
where
    E: Into<Error>,
{
    fn context(self, message: &str) -> Result<T> {
        self.map_err(|err| Error::Context {
            message: message.to_string(),
            source: Box::new(err.into()),
        })
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|err| Error::Context {
            message: f(),
            source: Box::new(err.into()),
        })
    }
}

/// Selects which SQLite backend a [`Client`] persists local state to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// An on-disk database at the given path (see
    /// [`sql::SqliteClient::connect_with`]).
    Disk(std::path::PathBuf),
    /// A private in-memory database that only lives for the process's
    /// lifetime (see [`sql::SqliteClient::connect_with`]).
    Memory,
}

/// Configuration for [`Client::init`].
pub struct ClientConfig {
    /// The `http://` or `https://` base URL of the fyde server to connect
    /// to (e.g. `http://127.0.0.1:8080`).
    pub url: String,
    /// Which SQLite backend to persist local state to.
    pub storage: Storage,
    /// The verbosity of the SDK's internal logs.
    pub log_level: LogLevel,
    /// Called once for every log line emitted by the SDK, in addition to the
    /// usual terminal output, when set. Useful for callers with no
    /// terminal/stderr to inspect (in particular FFI callers like the Kotlin
    /// Multiplatform app).
    pub on_log: Option<Arc<dyn Fn(LogLevel, String) + Send + Sync>>,
    /// Called once for every changelog event consumed by the background
    /// sync job that `UsersService::create`/`login` start automatically
    /// (replaying persisted history, then continuing with the live tail —
    /// see `DocumentsService::start_sync`). Each `Created`/`UpdateMetadata`
    /// event is already reflected in local storage (readable via
    /// `DocumentsService::get`/`list`) by the time this is called; use it
    /// to react to changes rather than to populate the cache. `None` if the
    /// caller doesn't need to observe individual events.
    pub on_document_change: Option<Arc<dyn Fn(ChangelogEvent) + Send + Sync>>,
}

/// A connection to a fyde server.
pub struct Client {
    documents: Arc<dyn DocumentsService>,
    scripts: Arc<dyn ScriptsService>,
    server_state: Arc<dyn ServerStateService>,
    sessions: Arc<SessionsClient>,
    settings: Arc<dyn SettingsService>,
    users: Arc<dyn UsersService>,
}

impl Client {
    /// Connects to a fyde server per `config`.
    pub async fn init(config: ClientConfig) -> Result<Self> {
        // Ignore the result: a subscriber may already be installed by the
        // consuming binary (e.g. `cli` sets one up via `RUST_LOG`), or by a
        // previous `Client::init` call, and a library must not panic over it.
        use tracing_subscriber::prelude::*;
        let callback_layer = config
            .on_log
            .clone()
            .map(|callback| CallbackLayer { callback });
        let _ = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(callback_layer)
            .with(tracing_subscriber::filter::LevelFilter::from(
                config.log_level,
            ))
            .try_init();

        let sqlite: Arc<SqliteClient> = Arc::new(
            match config.storage {
                Storage::Disk(path) => SqliteClient::connect_at(path).await,
                Storage::Memory => SqliteClient::connect_with(sql::IN_MEMORY_DB).await,
            }
            .context("failed to open local database")?,
        );

        // Opened lazily: `connect_lazy` doesn't dial the server here, only
        // once some call actually needs it (see each domain's
        // `grpc_client.rs`). The resulting `Channel` is cheap to clone and
        // shared by every domain's gRPC client, so they all reuse the same
        // underlying HTTP/2 connection instead of opening one each.
        let channel = Endpoint::from_shared(config.url)
            .map_err(|err| Error::InvalidEndpoint(err.to_string()))?
            .connect_lazy();

        let settings = domains::settings::init(sqlite.pool().clone());
        let sessions = domains::sessions::init(settings.clone());
        let server_state = domains::server_state::init(channel.clone());
        let documents = domains::documents::init(
            channel.clone(),
            sqlite.pool().clone(),
            settings.clone(),
            sessions.clone(),
            server_state.clone(),
            config.on_document_change,
        )
        .await
        .context("failed to initialize documents service")?;
        let users = domains::users::init(
            channel.clone(),
            settings.clone(),
            sessions.clone(),
            sqlite.clone(),
            documents.clone(),
        )
        .await
        .context("failed to initialize users service")?;
        let scripts = domains::scripts::init(channel.clone(), sessions.clone(), documents.clone());

        // A session may already be open from a previous run (the token is
        // persisted in `settings`, not just held in memory — see
        // `users::mod`'s doc comment) — in that case `create`/`login` won't
        // run again to start it, so start it here instead.
        if sessions.is_connected().await? {
            documents
                .start_sync()
                .await
                .context("failed to start changelog sync")?;
        }

        Ok(Self {
            documents,
            scripts,
            server_state,
            sessions,
            settings,
            users,
        })
    }

    /// Returns a reference to the client's documents service.
    pub fn documents(&self) -> &dyn DocumentsService {
        self.documents.as_ref()
    }

    /// Returns a reference to the client's users service.
    pub fn users(&self) -> &dyn UsersService {
        self.users.as_ref()
    }

    /// Returns a reference to the client's settings service.
    pub fn settings(&self) -> &dyn SettingsService {
        self.settings.as_ref()
    }

    /// Returns a reference to the client's server_state service.
    pub fn server_state(&self) -> &dyn ServerStateService {
        self.server_state.as_ref()
    }

    /// Returns a reference to the client's scripts service.
    pub fn scripts(&self) -> &dyn ScriptsService {
        self.scripts.as_ref()
    }

    /// Returns a reference to the client's sessions service. Returns the
    /// concrete `SessionsClient` rather than a trait object, like
    /// [`domains::sessions::init`], since `sessions::Service` is not
    /// object-safe. `pub(crate)` rather than `pub` since `sessions::Service`
    /// itself is internal plumbing, not part of the SDK's public API.
    pub(crate) fn sessions(&self) -> &SessionsClient {
        self.sessions.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_error() -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
    }

    #[test]
    fn context_wraps_the_error_with_a_message() {
        let err = io_error().context("failed to do the thing").unwrap_err();

        assert!(matches!(err, Error::Context { .. }));
        assert_eq!(err.to_string(), "failed to do the thing: io error: missing");
    }

    #[test]
    fn with_context_lazily_builds_the_message() {
        let err = io_error()
            .with_context(|| format!("failed at offset {}", 42))
            .unwrap_err();

        assert_eq!(err.to_string(), "failed at offset 42: io error: missing");
    }

    #[test]
    fn context_preserves_the_original_error_as_the_source() {
        use std::error::Error as _;

        let err = io_error().context("failed to do the thing").unwrap_err();

        let source = err.source().expect("context error must carry a source");
        assert_eq!(source.to_string(), "io error: missing");
    }

    #[test]
    fn context_is_a_no_op_on_success() {
        let ok: std::io::Result<u32> = Ok(42);

        assert_eq!(ok.context("unused").unwrap(), 42);
    }

    #[tokio::test]
    async fn init_rejects_a_malformed_url() {
        let config = ClientConfig {
            url: "not a valid uri".to_string(),
            storage: Storage::Memory,
            log_level: LogLevel::Off,
            on_log: None,
            on_document_change: None,
        };

        let err = match Client::init(config).await {
            Ok(_) => panic!("a malformed url must be rejected"),
            Err(err) => err,
        };

        assert!(matches!(err, Error::InvalidEndpoint(_)));
    }
}
