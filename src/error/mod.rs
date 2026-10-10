//! The SDK's single error type, [`Error`], returned by every public method.
//!
//! An [`Error`] carries three things:
//!
//! - a stable [`ErrorCode`], unique per kind of failure, for callers to
//!   branch on (and for users to quote to support) — see [`Error::code`];
//! - the original, technical error it was raised from, with every context
//!   message attached on its way up — see [`Error::original`] and its
//!   `Display`, meant for logs, never for end users;
//! - a user-friendly wording of the failure, in the user's selected
//!   language — see [`Error::user_message`].
//!
//! Inside the crate, failures are raised as an [`ErrorKind`] (converted
//! into an [`Error`] by `?`/`.into()`) and annotated on their way up with
//! [`ErrorContext`].

mod messages;

use std::fmt;

/// Expands `$callback!` with every [`ErrorCode`] variant, its doc comment
/// and its numeric value, so the FFI layer's error type (`crate::ffi`) is
/// generated from this one list instead of duplicating it.
macro_rules! for_each_error_code {
    ($callback:ident) => {
        $callback! {
            /// A local file couldn't be read or written.
            Io = 1001,
            /// The local database couldn't be read or written.
            Database = 1002,
            /// The local database couldn't be migrated to the current schema.
            DatabaseMigration = 1003,
            /// The local data directory couldn't be located.
            DataDirectory = 1004,
            /// Some locally stored or exchanged JSON was malformed.
            Json = 1005,
            /// A value couldn't be serialized to MessagePack.
            MessagePackEncode = 1006,
            /// Some locally stored or exchanged MessagePack was malformed.
            MessagePackDecode = 1007,
            /// An identifier wasn't a valid UUID.
            InvalidUuid = 1008,
            /// A value wasn't a valid integer.
            InvalidInteger = 1009,
            /// A background task crashed or was cancelled.
            TaskFailed = 1010,
            /// Encrypting or decrypting data failed.
            Encryption = 1101,
            /// The OS credential store (Keychain, Secret Service, ...) failed.
            CredentialStore = 1102,
            /// The configured server URL is malformed.
            InvalidServerUrl = 2001,
            /// No connection could be established with the server.
            ServerUnreachable = 2002,
            /// The server answered with something the SDK can't make sense of.
            InvalidServerResponse = 2003,
            /// The server is temporarily unavailable (gRPC `UNAVAILABLE`).
            ServerUnavailable = 2004,
            /// The server didn't answer in time (gRPC `DEADLINE_EXCEEDED`).
            ServerTimeout = 2005,
            /// The request was cancelled before completing (gRPC `CANCELLED`).
            RequestCancelled = 2006,
            /// The session is missing, expired or revoked (gRPC `UNAUTHENTICATED`).
            Unauthenticated = 2007,
            /// The account isn't allowed to do this (gRPC `PERMISSION_DENIED`).
            PermissionDenied = 2008,
            /// The server has no such resource (gRPC `NOT_FOUND`).
            NotFoundOnServer = 2009,
            /// The resource already exists on the server, e.g. a taken
            /// username (gRPC `ALREADY_EXISTS`).
            AlreadyExists = 2010,
            /// The server rejected the request as invalid (gRPC
            /// `INVALID_ARGUMENT`/`OUT_OF_RANGE`).
            InvalidRequest = 2011,
            /// The server refused the request in its current state (gRPC
            /// `FAILED_PRECONDITION`/`ABORTED`).
            FailedPrecondition = 2012,
            /// Too many requests (gRPC `RESOURCE_EXHAUSTED`).
            RateLimited = 2013,
            /// The server doesn't support this operation (gRPC `UNIMPLEMENTED`).
            NotSupportedByServer = 2014,
            /// The server failed internally (gRPC `INTERNAL`/`UNKNOWN`/`DATA_LOSS`).
            ServerError = 2015,
            /// Wrong username or password.
            InvalidCredentials = 3001,
            /// A session is already open on this device.
            AlreadyLoggedIn = 3002,
            /// The document's file type isn't supported.
            UnsupportedDocumentExtension = 4001,
            /// No such document in the local cache.
            DocumentNotFound = 4002,
            /// A PDF document couldn't be read.
            UnreadablePdf = 4003,
            /// A changelog event was malformed.
            InvalidChangelogEvent = 4004,
            /// A document's source category is unknown.
            InvalidSourceCategory = 4005,
            /// A document's source sub-category is unknown.
            InvalidSourceSubCategory = 4006,
            /// A document's purpose is unknown.
            InvalidPurpose = 4007,
            /// No such script.
            ScriptNotFound = 5001,
            /// A script was run without one of its required parameters.
            MissingScriptParameter = 5002,
            /// A script's type is unknown.
            InvalidScriptType = 5003,
            /// A script failed while running.
            ScriptFailed = 5004,
            /// The embedded browser a scraper script drives failed.
            BrowserFailed = 5005,
        }
    };
}
pub(crate) use for_each_error_code;

macro_rules! define_error_code {
    ($($(#[$meta:meta])* $name:ident = $value:literal,)*) => {
        /// A stable identifier for each kind of failure an [`Error`] can
        /// report. Each variant has its own numeric value (see
        /// [`ErrorCode::as_u32`]) that never changes meaning once
        /// released — the compiler rejects two variants sharing one — so
        /// callers can safely persist or match on it. Values are grouped
        /// by area: `1xxx` local device, `2xxx` server/network, `3xxx`
        /// account, `4xxx` documents, `5xxx` scripts.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        #[repr(u32)]
        pub enum ErrorCode {
            $($(#[$meta])* $name = $value,)*
        }

        impl ErrorCode {
            /// Every error code, in declaration order.
            pub const ALL: &[ErrorCode] = &[$(ErrorCode::$name,)*];
        }
    };
}
for_each_error_code!(define_error_code);

impl ErrorCode {
    /// The code's stable numeric value, e.g. `3001` for
    /// [`ErrorCode::InvalidCredentials`].
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    /// The wording to show an end user for this code, in `language` (a
    /// short code such as `"en"` or `"fr"`, or a locale such as `"fr_FR"`),
    /// falling back to English for any language the SDK has no
    /// translation for.
    pub fn user_message(self, language: &str) -> &'static str {
        messages::user_message(self, messages::Language::parse(language))
    }
}

/// Formats as `FYDE-<value>`, e.g. `FYDE-3001`.
impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FYDE-{}", self.as_u32())
    }
}

/// The technical cause of an [`Error`] — what went wrong, as raised inside
/// the SDK. Crate-private: callers branch on [`ErrorCode`] instead, so
/// this enum (and the third-party error types it wraps) can change without
/// breaking them.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ErrorKind {
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
    Pdf(#[from] pdf_oxide::Error),
    #[error("lua error: {0}")]
    Lua(#[from] mlua::Error),
    #[error("unsupported document extension {0:?}: only .pdf is supported")]
    UnsupportedDocumentExtension(String),
    #[error("document {0} not found in local cache")]
    DocumentNotFound(uuid::Uuid),
    #[error("script {0} not found")]
    ScriptNotFound(uuid::Uuid),
    #[error("missing value for required parameter {parameter:?} of script {script_id}")]
    MissingScriptParameter {
        script_id: uuid::Uuid,
        parameter: String,
    },
    #[error("invalid changelog event: {0}")]
    InvalidChangelogEvent(String),
    #[error("invalid source category: {0:?}")]
    InvalidSourceCategory(String),
    #[error("invalid source sub-category: {0:?}")]
    InvalidSourceSubCategory(String),
    #[error("invalid purpose: {0:?}")]
    InvalidPurpose(String),
    #[error("invalid script type: {0:?}")]
    InvalidScriptType(String),
    #[error("invalid server response: {0}")]
    InvalidResponse(String),
    #[error("encryption error: {0}")]
    Encryption(String),
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("a session is already open on this device; log out first")]
    AlreadyLoggedIn,
    #[error("credential store error: {0}")]
    Keystore(#[from] keyring_core::Error),
    #[error("messagepack encode error: {0}")]
    MessagePackEncode(#[from] rmp_serde::encode::Error),
    #[error("messagepack decode error: {0}")]
    MessagePackDecode(#[from] rmp_serde::decode::Error),
    #[error("scraper task failed: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
    #[error("browser error: {0}")]
    Browser(String),
}

impl ErrorKind {
    fn code(&self) -> ErrorCode {
        match self {
            ErrorKind::GrpcTransport(_) => ErrorCode::ServerUnreachable,
            ErrorKind::Grpc(status) => grpc_code(status.code()),
            ErrorKind::InvalidEndpoint(_) => ErrorCode::InvalidServerUrl,
            ErrorKind::InvalidUuid(_) => ErrorCode::InvalidUuid,
            ErrorKind::InvalidInteger(_) => ErrorCode::InvalidInteger,
            ErrorKind::Io(_) => ErrorCode::Io,
            ErrorKind::Database(_) => ErrorCode::Database,
            ErrorKind::Migrate(_) => ErrorCode::DatabaseMigration,
            ErrorKind::Xdg(_) => ErrorCode::DataDirectory,
            ErrorKind::Json(_) => ErrorCode::Json,
            ErrorKind::Pdf(_) => ErrorCode::UnreadablePdf,
            ErrorKind::Lua(_) => ErrorCode::ScriptFailed,
            ErrorKind::UnsupportedDocumentExtension(_) => ErrorCode::UnsupportedDocumentExtension,
            ErrorKind::DocumentNotFound(_) => ErrorCode::DocumentNotFound,
            ErrorKind::ScriptNotFound(_) => ErrorCode::ScriptNotFound,
            ErrorKind::MissingScriptParameter { .. } => ErrorCode::MissingScriptParameter,
            ErrorKind::InvalidChangelogEvent(_) => ErrorCode::InvalidChangelogEvent,
            ErrorKind::InvalidSourceCategory(_) => ErrorCode::InvalidSourceCategory,
            ErrorKind::InvalidSourceSubCategory(_) => ErrorCode::InvalidSourceSubCategory,
            ErrorKind::InvalidPurpose(_) => ErrorCode::InvalidPurpose,
            ErrorKind::InvalidScriptType(_) => ErrorCode::InvalidScriptType,
            ErrorKind::InvalidResponse(_) => ErrorCode::InvalidServerResponse,
            ErrorKind::Encryption(_) => ErrorCode::Encryption,
            ErrorKind::InvalidCredentials => ErrorCode::InvalidCredentials,
            ErrorKind::AlreadyLoggedIn => ErrorCode::AlreadyLoggedIn,
            ErrorKind::Keystore(_) => ErrorCode::CredentialStore,
            ErrorKind::MessagePackEncode(_) => ErrorCode::MessagePackEncode,
            ErrorKind::MessagePackDecode(_) => ErrorCode::MessagePackDecode,
            ErrorKind::TaskJoin(_) => ErrorCode::TaskFailed,
            ErrorKind::Browser(_) => ErrorCode::BrowserFailed,
        }
    }
}

fn grpc_code(code: tonic::Code) -> ErrorCode {
    use tonic::Code;

    match code {
        Code::Unavailable => ErrorCode::ServerUnavailable,
        Code::DeadlineExceeded => ErrorCode::ServerTimeout,
        Code::Cancelled => ErrorCode::RequestCancelled,
        Code::Unauthenticated => ErrorCode::Unauthenticated,
        Code::PermissionDenied => ErrorCode::PermissionDenied,
        Code::NotFound => ErrorCode::NotFoundOnServer,
        Code::AlreadyExists => ErrorCode::AlreadyExists,
        Code::InvalidArgument | Code::OutOfRange => ErrorCode::InvalidRequest,
        Code::FailedPrecondition | Code::Aborted => ErrorCode::FailedPrecondition,
        Code::ResourceExhausted => ErrorCode::RateLimited,
        Code::Unimplemented => ErrorCode::NotSupportedByServer,
        Code::Ok | Code::Unknown | Code::Internal | Code::DataLoss => ErrorCode::ServerError,
    }
}

/// The error returned by every fallible SDK method. See the module docs.
///
/// Its `Display` is the technical description — every context message
/// attached on the way up, then the original error, e.g. `failed to open
/// local database: io error: permission denied` — meant for logs and bug
/// reports. Show [`Error::user_message`] to end users instead.
pub struct Error {
    /// Boxed to keep `Result<T>` small, since it's returned everywhere.
    kind: Box<ErrorKind>,
    /// Messages attached via [`ErrorContext`], outermost first.
    context: Vec<String>,
}

impl Error {
    /// The stable code identifying this kind of failure.
    pub fn code(&self) -> ErrorCode {
        self.kind.code()
    }

    /// The original error this one was raised from, without the context
    /// messages attached on its way up. Its own `source()` chain, when
    /// any, leads to the underlying third-party error (I/O, SQLite,
    /// gRPC, ...).
    pub fn original(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self.kind.as_ref()
    }

    /// A short, non-technical wording of this failure, fit to show an end
    /// user, in `language` (see [`ErrorCode::user_message`]). To use the
    /// language the user picked via `UsersService::set_language`, see
    /// [`crate::Client::user_message`].
    pub fn user_message(&self, language: &str) -> &'static str {
        self.code().user_message(language)
    }

    #[cfg(test)]
    pub(crate) fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// Whether at least one [`ErrorContext`] message was attached — lets
    /// tests assert an error was annotated without depending on its exact
    /// wording.
    #[cfg(test)]
    pub(crate) fn has_context(&self) -> bool {
        !self.context.is_empty()
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("code", &self.code())
            .field("context", &self.context)
            .field("kind", &self.kind)
            .finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for message in &self.context {
            write!(f, "{message}: ")?;
        }
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // With context attached, `Display` is "<context>: <original>" and
        // the original is the next link in the chain. Without, `Display`
        // already *is* the original's, so skip straight past it rather
        // than have chain printers (e.g. anyhow's `{:#}`) repeat it.
        if self.context.is_empty() {
            self.kind.source()
        } else {
            Some(self.kind.as_ref())
        }
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self {
            kind: Box::new(kind),
            context: Vec::new(),
        }
    }
}

/// Lets `?` convert each third-party error [`ErrorKind`] wraps straight
/// into an [`Error`], as it did when `Error` was itself that enum.
macro_rules! impl_from_source {
    ($($source:ty),* $(,)?) => {
        $(
            impl From<$source> for Error {
                fn from(err: $source) -> Self {
                    ErrorKind::from(err).into()
                }
            }
        )*
    };
}
impl_from_source!(
    tonic::transport::Error,
    tonic::Status,
    uuid::Error,
    std::num::ParseIntError,
    std::io::Error,
    sqlx::Error,
    sqlx::migrate::MigrateError,
    xdg::BaseDirectoriesError,
    serde_json::Error,
    pdf_oxide::Error,
    mlua::Error,
    keyring_core::Error,
    rmp_serde::encode::Error,
    rmp_serde::decode::Error,
    tokio::task::JoinError,
);

pub type Result<T> = std::result::Result<T, Error>;

/// Attaches a human-readable message to a fallible operation's error,
/// preserving the original error as its source. Used throughout the crate
/// so every `?` site reports what it was trying to do, not just the bare
/// underlying error. Never changes the error's [`ErrorCode`].
pub(crate) trait ErrorContext<T> {
    fn context(self, message: &str) -> Result<T>;
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T>;
}

impl<T, E> ErrorContext<T> for std::result::Result<T, E>
where
    E: Into<Error>,
{
    fn context(self, message: &str) -> Result<T> {
        self.with_context(|| message.to_string())
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|err| {
            let mut err = err.into();
            err.context.insert(0, f());
            err
        })
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

        assert!(err.has_context());
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
    fn nested_context_reads_outermost_first() {
        let err = io_error().context("inner").context("outer").unwrap_err();

        assert_eq!(err.to_string(), "outer: inner: io error: missing");
    }

    #[test]
    fn context_preserves_the_original_error_as_the_source() {
        use std::error::Error as _;

        let err = io_error().context("failed to do the thing").unwrap_err();

        let source = err.source().expect("context error must carry a source");
        assert_eq!(source.to_string(), "io error: missing");
        assert_eq!(err.original().to_string(), "io error: missing");
    }

    #[test]
    fn source_skips_the_original_when_there_is_no_context() {
        use std::error::Error as _;

        let err: Error = io_error().unwrap_err().into();

        let source = err.source().expect("io error must carry a source");
        assert_eq!(source.to_string(), "missing");
    }

    #[test]
    fn context_is_a_no_op_on_success() {
        let ok: std::io::Result<u32> = Ok(42);

        assert_eq!(ok.context("unused").unwrap(), 42);
    }

    #[test]
    fn context_never_changes_the_code() {
        let err = Err::<(), _>(ErrorKind::InvalidCredentials)
            .context("failed to log in")
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::InvalidCredentials);
    }

    #[test]
    fn grpc_statuses_map_to_their_own_codes() {
        let unavailable: Error = tonic::Status::unavailable("down").into();
        let unauthenticated: Error = tonic::Status::unauthenticated("expired").into();
        let already_exists: Error = tonic::Status::already_exists("taken").into();

        assert_eq!(unavailable.code(), ErrorCode::ServerUnavailable);
        assert_eq!(unauthenticated.code(), ErrorCode::Unauthenticated);
        assert_eq!(already_exists.code(), ErrorCode::AlreadyExists);
    }

    #[test]
    fn codes_display_with_their_numeric_value() {
        assert_eq!(ErrorCode::InvalidCredentials.to_string(), "FYDE-3001");
        assert_eq!(ErrorCode::InvalidCredentials.as_u32(), 3001);
    }

    #[test]
    fn user_message_is_localized() {
        let err: Error = ErrorKind::InvalidCredentials.into();

        assert_ne!(err.user_message("en"), err.user_message("fr"));
        assert_eq!(err.user_message("fr"), err.user_message("fr_FR.UTF-8"));
    }

    #[test]
    fn user_message_falls_back_to_english() {
        let err: Error = ErrorKind::InvalidCredentials.into();

        assert_eq!(err.user_message("xx"), err.user_message("en"));
        assert_eq!(err.user_message(""), err.user_message("en"));
    }

    #[test]
    fn user_message_never_leaks_technical_details() {
        let err: Error = std::io::Error::other("/home/alice/secret.pdf").into();

        assert!(!err.user_message("en").contains("secret"));
    }

    #[test]
    fn every_code_has_a_distinct_wording_in_every_language() {
        for language in ["en", "fr"] {
            let mut seen = std::collections::HashSet::new();
            for code in ErrorCode::ALL {
                let message = code.user_message(language);
                assert!(!message.is_empty(), "{code:?} has no {language} wording");
                assert!(
                    seen.insert(message),
                    "{code:?} shares its {language} wording with another code"
                );
            }
        }
    }
}
