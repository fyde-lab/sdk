mod domains;
mod error;
mod ffi;
#[path = "tools/log/mod.rs"]
mod log;
#[path = "tools/sandbox/mod.rs"]
mod sandbox;
#[path = "tools/sql/mod.rs"]
mod sql;
#[cfg(test)]
#[path = "tools/testing/mod.rs"]
mod testing;

uniffi::setup_scaffolding!();

pub use error::{Error, ErrorCode, Result};
pub use ffi::{FfiError, FydeClient};

pub(crate) use error::{ErrorContext, ErrorKind};

use std::sync::Arc;

pub use domains::documents::parser::{Service as ParserService, init as init_parser};
pub use domains::documents::{
    ChangelogEvent, Document, Metadata, Service as DocumentsService, UploadRequest,
};
pub use domains::scrapers::{ProgressEvent, Service as ScrapersService};
pub use domains::scripts::{
    InMemoryScriptStorage, InstallScriptRequest, InstalledScript, Script, ScriptType,
    Service as ScriptsService,
};
pub use domains::server_state::Service as ServerStateService;
pub use domains::settings::Service as SettingsService;
pub use domains::users::Service as UsersService;
pub use log::LogLevel;

use domains::sessions::{Service as SessionsService, SessionsClient};
use log::CallbackLayer;
use sql::SqliteClient;
use tonic::transport::Endpoint;

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
    /// to (e.g. `http://127.0.0.1:40051`).
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
    /// Called once for every `fyde.progress.step`/`fyde.progress.update`
    /// call made by a scraper script running via `ScrapersService::run`, so
    /// the caller can render its own progress bar. `None` if the caller
    /// doesn't need to observe scraper progress.
    pub on_scraper_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    /// Called once for every `fyde.input.ask(question)` call made by a
    /// running scraper script, and must return the answer to show for it —
    /// the call blocks until it does, so the caller should ask directly on
    /// screen rather than return immediately with a placeholder. `None`
    /// means a scraper script calling `fyde.input.ask` fails immediately
    /// instead of blocking forever waiting for an answer that can never
    /// come.
    pub on_scraper_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
}

/// A connection to a fyde server.
pub struct Client {
    documents: Arc<dyn DocumentsService>,
    scrapers: Arc<dyn ScrapersService>,
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
        // consuming binary or by a previous `Client::init` call, and a
        // library must not panic over it.
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
            match &config.storage {
                Storage::Disk(path) => SqliteClient::connect_at(path.clone()).await,
                Storage::Memory => SqliteClient::connect_with(sql::IN_MEMORY_DB).await,
            }
            .context("failed to open local database")?,
        );

        // Secrets (master key, session token) live in the OS credential
        // store next to an on-disk database, never in the database itself;
        // an in-memory client keeps them in memory, like everything else.
        let secrets = domains::secrets::init(match &config.storage {
            Storage::Disk(path) => domains::secrets::StorageConfig::Keystore {
                database_path: path,
            },
            Storage::Memory => domains::secrets::StorageConfig::Memory,
        })
        .context("failed to initialize secrets store")?;

        // Opened lazily: `connect_lazy` doesn't dial the server here, only
        // once some call actually needs it (see each domain's
        // `grpc_client.rs`). The resulting `Channel` is cheap to clone and
        // shared by every domain's gRPC client, so they all reuse the same
        // underlying HTTP/2 connection instead of opening one each.
        let channel = Endpoint::from_shared(config.url)
            .map_err(|err| ErrorKind::InvalidEndpoint(err.to_string()))?
            .connect_lazy();

        let settings = domains::settings::init(sqlite.pool().clone());
        let sessions = domains::sessions::init(secrets.clone());
        let server_state = domains::server_state::init(channel.clone());
        // Shared by `scripts` (to publish script installations) and
        // `documents` (to publish documents and drive the sync job that
        // consumes every event, scripts' included).
        let changelog = domains::changelog::init(
            channel.clone(),
            sqlite.pool().clone(),
            settings.clone(),
            secrets.clone(),
            sessions.clone(),
            server_state.clone(),
        )
        .await
        .context("failed to initialize changelog service")?;
        let scripts = domains::scripts::init(
            channel.clone(),
            sessions.clone(),
            sqlite.pool().clone(),
            changelog.clone(),
        );
        let documents = domains::documents::init(
            sqlite.pool().clone(),
            changelog,
            scripts.clone(),
            config.on_document_change,
        );
        let users = domains::users::init(
            channel.clone(),
            settings.clone(),
            secrets,
            sessions.clone(),
            sqlite.clone(),
            documents.clone(),
        )
        .await
        .context("failed to initialize users service")?;
        let scrapers = domains::scrapers::init(
            domains::scrapers::StorageConfig::Sqlite(sqlite.pool().clone()),
            documents.clone(),
            config.on_scraper_progress,
            config.on_scraper_question,
        );

        // A session may already be open from a previous run (the token is
        // persisted in `secrets`, not just held in memory — see
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
            scrapers,
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

    /// Returns a reference to the client's scrapers service.
    pub fn scrapers(&self) -> &dyn ScrapersService {
        self.scrapers.as_ref()
    }

    /// Returns `err`'s user-facing wording (see [`Error::user_message`]) in
    /// the language the user picked via [`UsersService::set_language`],
    /// falling back to the device's system language if they never picked
    /// one — or if reading their preference fails, since this is meant to
    /// be called while already reporting a failure.
    pub async fn user_message(&self, err: &Error) -> &'static str {
        let language = match self.users.get_language().await {
            Ok(Some(language)) => language,
            Ok(None) | Err(_) => domains::users::system_language(),
        };
        err.user_message(&language)
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

/// Testing hook, not part of the supported API: makes every [`Client`]
/// created afterwards in this process with [`Storage::Disk`] keep its
/// secrets (master key, session token) in an in-process store instead of
/// the OS credential store — they survive dropping and re-`init`ing a
/// `Client`, but not the process, and the machine's real keychain is never
/// touched. For end-to-end tests that must not depend on (or leave entries
/// behind in) a desktop keychain, which a headless runner may not even have.
#[doc(hidden)]
pub fn use_in_process_keystore_for_tests() -> Result<()> {
    domains::secrets::use_in_process_store()
}

/// Builds a standalone [`ScrapersService`] for local/dev use — no
/// [`Client`], no fyde server connection, no OPAQUE login, no changelog
/// sync. Meant for a CLI runner (ported from `demo-rust-fyde`) that just
/// runs Lua scraper scripts and keeps their output on disk.
///
/// Cookies, session data and per-run debug reports persist as JSON files
/// under `storage_dir` (one subdirectory per sub-domain, created if missing
/// — see [`domains::scrapers::StorageConfig::File`]) rather than in a SQLite
/// database: a standalone CLI runner has no reason to pull in a local
/// database just for this, unlike [`Client`], which already has one for
/// everything else it persists. Documents a script saves via
/// `fyde.save_document` are written as plain files under
/// `documents_dir/scraper_name` instead of being encrypted and uploaded —
/// see [`domains::documents::init_dev`] — so documents from different
/// scrapers never collide or mix together under the same `documents_dir`.
/// `on_progress`/`on_question` are the same callbacks as
/// [`ClientConfig::on_scraper_progress`]/[`ClientConfig::on_scraper_question`].
pub async fn init_dev_scrapers(
    storage_dir: std::path::PathBuf,
    documents_dir: std::path::PathBuf,
    scraper_name: impl Into<String>,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
) -> Result<Arc<dyn ScrapersService>> {
    let documents = domains::documents::init_dev(documents_dir, scraper_name);

    Ok(domains::scrapers::init(
        domains::scrapers::StorageConfig::File(storage_dir),
        documents,
        on_progress,
        on_question,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn init_rejects_a_malformed_url() {
        let config = ClientConfig {
            url: "not a valid uri".to_string(),
            storage: Storage::Memory,
            log_level: LogLevel::Off,
            on_log: None,
            on_document_change: None,
            on_scraper_progress: None,
            on_scraper_question: None,
        };

        let err = match Client::init(config).await {
            Ok(_) => panic!("a malformed url must be rejected"),
            Err(err) => err,
        };

        assert_eq!(err.code(), ErrorCode::InvalidServerUrl);
    }
}
