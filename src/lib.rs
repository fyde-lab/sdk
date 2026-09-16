mod services;
#[path = "lib/sql/mod.rs"]
mod sql;

use std::sync::Arc;

pub use services::changelog::{
    ChangelogClient, Service as ChangelogService, SqliteStorage as ChangelogSqliteStorage,
    Storage as ChangelogStorage,
};
pub use services::documents::{
    Document, DocumentsClient, NewDocument, Service as DocumentsService,
    SqliteStorage as DocumentsSqliteStorage, Storage as DocumentsStorage,
};
pub use sql::SqliteClient;

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
    #[error("encryption error: {0}")]
    Encryption(String),
    #[error("invalid changelog event: {0}")]
    InvalidChangelogEvent(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A connection to a fyde server.
pub struct Client {
    changelog: ChangelogClient<ChangelogSqliteStorage>,
    documents: Arc<DocumentsClient<DocumentsSqliteStorage>>,
}

impl Client {
    /// Connects to a fyde server at the given `http://` or `https://` base
    /// URL (e.g. `http://127.0.0.1:8080`).
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        let url = url.as_ref();

        let sqlite = SqliteClient::connect().await?;
        let changelog_storage = ChangelogSqliteStorage::new(sqlite.pool().clone());
        let documents_storage = DocumentsSqliteStorage::new(sqlite.pool().clone());

        let documents = Arc::new(DocumentsClient::new(url, documents_storage).await?);
        let changelog = ChangelogClient::new(url, changelog_storage, documents.clone()).await?;

        Ok(Self {
            changelog,
            documents,
        })
    }

    /// Returns a reference to the client's documents service.
    pub fn documents(&self) -> &DocumentsClient<impl services::documents::Storage> {
        &self.documents
    }

    /// Returns a reference to the client's changelog service.
    pub fn changelog(&self) -> &ChangelogClient<impl services::changelog::Storage> {
        &self.changelog
    }
}
