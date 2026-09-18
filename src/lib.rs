mod ffi;
mod services;
#[path = "lib/sql/mod.rs"]
mod sql;

uniffi::setup_scaffolding!();

pub use ffi::{FfiError, FydeClient};

use std::sync::Arc;

pub use services::changelog::{ChangelogEvent, Service as ChangelogService};
pub use services::documents::{Document, Service as DocumentsService};

use sql::SqliteClient;

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
    #[error("unsupported document extension {0:?}: only .pdf is supported")]
    UnsupportedDocumentExtension(String),
    #[error("encryption error: {0}")]
    Encryption(String),
    #[error("invalid changelog event: {0}")]
    InvalidChangelogEvent(String),
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

/// A connection to a fyde server.
pub struct Client {
    changelog: Arc<dyn ChangelogService>,
    documents: Arc<dyn DocumentsService>,
}

impl Client {
    /// Connects to a fyde server at the given `http://` or `https://` base
    /// URL (e.g. `http://127.0.0.1:8080`), using the default local SQLite
    /// database location (see [`sql::SqliteClient::connect`]).
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        let sqlite = SqliteClient::connect()
            .await
            .context("failed to open local database")?;

        Self::connect_with_sqlite(url, sqlite).await
    }

    /// Connects to a fyde server, like [`Client::connect`], but backed by a
    /// private in-memory SQLite database that only lives for the process's
    /// lifetime, instead of the default XDG data directory location.
    pub async fn connect_memory(url: impl AsRef<str>) -> Result<Self> {
        let sqlite = SqliteClient::connect_with(sql::IN_MEMORY_DB)
            .await
            .context("failed to open local database")?;

        Self::connect_with_sqlite(url, sqlite).await
    }

    async fn connect_with_sqlite(url: impl AsRef<str>, sqlite: SqliteClient) -> Result<Self> {
        let url = url.as_ref();

        let documents = services::documents::init(url, sqlite.pool().clone())
            .await
            .context("failed to initialize documents service")?;
        let changelog = services::changelog::init(url, sqlite.pool().clone(), documents.clone())
            .await
            .context("failed to initialize changelog service")?;

        Ok(Self {
            changelog,
            documents,
        })
    }

    /// Returns a reference to the client's documents service.
    pub fn documents(&self) -> &dyn DocumentsService {
        self.documents.as_ref()
    }

    /// Returns a reference to the client's changelog service.
    pub fn changelog(&self) -> &dyn ChangelogService {
        self.changelog.as_ref()
    }
}
