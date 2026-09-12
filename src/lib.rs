mod changelog;
mod documents;
mod sqlite;

pub use changelog::{ChangelogClient, SqliteStorage, Storage};
pub use documents::{Document, DocumentMeta, DocumentsClient};
pub use sqlite::SqliteClient;

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
    #[error("messagepack encoding error: {0}")]
    MessagePackEncode(#[from] rmp_serde::encode::Error),
    #[error("messagepack decoding error: {0}")]
    MessagePackDecode(#[from] rmp_serde::decode::Error),
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
    changelog: ChangelogClient<SqliteStorage>,
    documents: DocumentsClient,
}

impl Client {
    /// Connects to a fyde server at the given `http://` or `https://` base
    /// URL (e.g. `http://127.0.0.1:8080`).
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        let url = url.as_ref();

        let sqlite = SqliteClient::connect().await?;
        let storage = SqliteStorage::new(sqlite.pool().clone());

        let changelog = ChangelogClient::new(url, storage).await?;
        let documents = DocumentsClient::new(url).await?;

        Ok(Self {
            changelog,
            documents,
        })
    }

    /// Returns a mutable reference to the client's documents service.
    pub fn documents(&mut self) -> &mut DocumentsClient {
        &mut self.documents
    }

    /// Returns a mutable reference to the client's changelog service.
    pub fn changelog(&mut self) -> &mut ChangelogClient<SqliteStorage> {
        &mut self.changelog
    }
}
