mod crypto;
mod grpc_client;
mod service;

pub use service::UsersClient;
pub use service::system_language;

use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Channel;

use crate::domains::documents::Service as DocumentsService;
use crate::domains::secrets::Service as SecretsService;
use crate::domains::sessions::{ExpiredSessionHandler, Service as _, SessionsClient};
use crate::domains::settings::Service as SettingsService;
use crate::tools::sql::LocalDatabase;
use crate::{ErrorContext as _, Result};

/// The settings key under which this device's UI language preference (see
/// [`Service::set_language`]) is persisted. Distinct from the `language`
/// sent once to the server by `create` (via `system_language`): that one
/// tells the server what language to use for account-related
/// communication, while this is a local-only preference this device's own
/// UI reads to pick which resource bundle to render — never sent to the
/// server.
pub(crate) const LANGUAGE_SETTING: &str = "language";

/// The settings key under which the account's role (see [`Role`]), as
/// reported by the server's `FinishRegistration`/`FinishLogin` response, is
/// cached locally by `create`/`login` so [`Service::role`] can report it
/// without a server round trip.
pub(crate) const ROLE_SETTING: &str = "role";

/// A user account's privilege level, mirroring `users.v1.Role` in
/// `../server/protos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "admin" => Ok(Role::Admin),
            "user" => Ok(Role::User),
            other => Err(format!("unknown role: {other}")),
        }
    }
}

/// The outcome of a [`Service::login`] whose credentials checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// A session was opened; carries its token.
    LoggedIn { session_token: String },
    /// The account already has as many open sessions as its plan allows
    /// (`max_sessions`), so none was opened — and nothing was persisted
    /// locally. `sessions` lists them, oldest first: call
    /// [`Service::login`] again with one's [`OpenSession::id`] as
    /// `replace_session_id` to close it in favor of the new session.
    SessionLimitReached {
        max_sessions: u32,
        sessions: Vec<OpenSession>,
    },
}

/// One of an account's open sessions (a device logged into it), as listed
/// by [`LoginOutcome::SessionLimitReached`] or [`Service::list_sessions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSession {
    pub(super) id: String,
    pub(super) device_name: String,
    /// Unix timestamp, in seconds, of when the session was opened.
    pub(super) created_at: i64,
    /// Whether this is the session this device is signed in with. Always
    /// `false` in [`LoginOutcome::SessionLimitReached`], since no session is
    /// open on this device yet then.
    pub(super) is_current: bool,
}

impl OpenSession {
    /// The id to pass as [`Service::login`]'s `replace_session_id`, or to
    /// [`Service::revoke_session`].
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name of the device, as given when the session was opened.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    pub fn is_current(&self) -> bool {
        self.is_current
    }
}

/// Builds an [`OpenSession`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(any(test, feature = "testing"))]
pub struct FakeOpenSession {
    session: OpenSession,
}

#[cfg(any(test, feature = "testing"))]
impl Default for FakeOpenSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl FakeOpenSession {
    pub fn new() -> Self {
        Self {
            session: OpenSession {
                id: uuid::Uuid::now_v7().to_string(),
                device_name: crate::tools::testing::random_word().to_string(),
                created_at: crate::tools::testing::random_past_timestamp(),
                is_current: false,
            },
        }
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self.session.id = id.to_string();
        self
    }

    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.session.created_at = created_at;
        self
    }

    pub fn with_is_current(mut self, is_current: bool) -> Self {
        self.session.is_current = is_current;
        self
    }

    pub fn with_device_name(mut self, device_name: &str) -> Self {
        self.session.device_name = device_name.to_string();
        self
    }

    pub fn build(self) -> OpenSession {
        self.session
    }
}

/// JSON-encodes `master_key`'s raw bytes for storage under
/// [`crate::domains::secrets::MASTER_KEY_SECRET`].
pub(crate) fn encode_master_key(master_key: &[u8]) -> Result<String> {
    serde_json::to_string(master_key).context("failed to serialize master key")
}

/// Decodes a value previously stored by [`encode_master_key`].
pub(crate) fn decode_master_key(encoded: &str) -> Result<Vec<u8>> {
    serde_json::from_str(encoded).context("failed to parse master key")
}

/// Manages account creation and session lifecycle against the fyde
/// server's users service, authenticating via the OPAQUE
/// asymmetric password-authenticated key exchange protocol (RFC 9807) —
/// `password` never leaves this SDK, not even hashed; see
/// [`crate::domains::users::crypto`] for the client side of the exchange
/// and `../server/CLAUDE.md`'s `tools::opaque` section for the server's.
/// Trait methods take `&self` (not `&mut self`) so implementations can be
/// shared behind `Arc<dyn Service>`; the session token opened by
/// `create`/`login` is persisted in the OS credential store (see
/// [`crate::domains::secrets::SESSION_TOKEN_SECRET`]), shared with every
/// other service's gRPC transport, which reads it from there to
/// authenticate outgoing calls (see
/// [`crate::domains::sessions::Service::authenticated_request`]) rather than it being
/// threaded through every call here — so `logout` takes no argument.
/// Persisting it, rather than only holding it in memory, means a session
/// survives across process restarts; `logout` removes it again. Only one
/// account can be signed in on a device (i.e. per local database) at a time:
/// `create`/`login` refuse while a session is open, since the local cache —
/// documents, changelog cursor, scraper cookies — isn't partitioned per
/// account and would otherwise leak the previous account's data into the
/// next one. Should the server reject the session token on any
/// authenticated call (expired, revoked, ...), the SDK logs out locally on
/// its own — clearing the same state `logout` does — and that call fails
/// with [`crate::ErrorCode::SessionExpired`], as does every call the
/// server rejects until `create`/`login` open a new session.
#[async_trait]
pub trait Service: Send + Sync {
    /// Creates a new account and opens a session for the device named
    /// `device_name`, returning its session token, via a two-step OPAQUE
    /// registration exchange (`StartRegistration`/`FinishRegistration`).
    /// Also generates a random master key, encrypts it under a key derived
    /// from the OPAQUE export key produced by that exchange (never
    /// `password` directly), and persists the raw key in the OS credential
    /// store (see [`crate::domains::secrets::MASTER_KEY_SECRET`]). On a
    /// device with no usable credential store (see
    /// [`Service::is_secure_storage_available`]), the raw master key and the
    /// session token are kept in plaintext in the local database instead if
    /// `allow_insecure_storage` is set — the user must have explicitly
    /// accepted that — and this fails with
    /// [`crate::ErrorCode::SecureStorageUnavailable`], before contacting the
    /// server, if it isn't; the flag is ignored whenever a credential store
    /// is usable. The server's response carries only the new session's
    /// token (see `../server/protos/users.proto`), so there is no user payload
    /// to return here. Fails with [`crate::ErrorCode::AlreadyExists`]
    /// if the username is already taken, and with
    /// [`crate::ErrorCode::AlreadyLoggedIn`] — before contacting the server — if a
    /// session is already open on this device. Once the session and master
    /// key are persisted, starts changelog consumption via
    /// [`DocumentsService::start_sync`].
    async fn create(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
        allow_insecure_storage: bool,
    ) -> Result<String>;

    /// Verifies `username`/`password` and opens a session for the device
    /// named `device_name`, returning its session token
    /// ([`LoginOutcome::LoggedIn`]), via a two-step OPAQUE login exchange
    /// (`StartLogin`/`FinishLogin`). If the account's plan doesn't allow
    /// another open session, opens none and returns
    /// [`LoginOutcome::SessionLimitReached`] instead, listing the account's
    /// sessions so the user can pick one to close: calling this again with
    /// that session's id as `replace_session_id` closes it and opens the new
    /// one in its place, atomically server-side. A `replace_session_id` that
    /// isn't one of the account's sessions fails with
    /// [`crate::ErrorCode::NotFoundOnServer`]. An incorrect
    /// password is often detected locally, without a round trip to the
    /// server, and fails with [`crate::ErrorCode::InvalidCredentials`] in that
    /// case; failure detected by the server instead surfaces the usual way,
    /// as [`crate::ErrorCode::Unauthenticated`]. Fails with
    /// [`crate::ErrorCode::AlreadyLoggedIn`] — before contacting the server — if a
    /// session is already open on this device. On a device with no usable
    /// OS credential store, the session's secrets are kept in plaintext in
    /// the local database instead if `allow_insecure_storage` is set — the
    /// same explicit opt-in as `create`'s — and this fails with
    /// [`crate::ErrorCode::SecureStorageUnavailable`] (also before contacting
    /// the server) if it isn't; the flag is ignored whenever a credential
    /// store is usable. Once the session and master key are persisted,
    /// starts changelog consumption via [`DocumentsService::start_sync`]. A
    /// session restored on a later start reads its secrets from the
    /// credential store first, then from the local database
    /// `create`/`login` may have put them in.
    async fn login(
        &self,
        username: &str,
        password: &str,
        device_name: &str,
        allow_insecure_storage: bool,
        replace_session_id: Option<&str>,
    ) -> Result<LoginOutcome>;

    /// Closes the session opened by the most recent `create`/`login` call
    /// and clears every trace of the account from this device: its secrets
    /// (master key, session token) from the OS credential store, and every
    /// locally cached row (documents, settings, the changelog offset,
    /// scraper state) from the local database. Local state is cleared even
    /// if the server can't be reached or rejects the logout — a device
    /// that's offline, or a server that's down, must never be what leaves
    /// the account's keys behind — in which case the server-side session is
    /// left to expire on its own and the failure is only logged. A no-op if
    /// there is no open session.
    async fn logout(&self) -> Result<()>;

    /// Changes the password of the account behind the current session,
    /// identified by `username` (needed to verify `old_password`; the
    /// session itself is what authenticates the change server-side — see
    /// below). Verifies `old_password` the same way `login` verifies a
    /// password: the local half of an OPAQUE login exchange, which fails
    /// with [`crate::ErrorCode::InvalidCredentials`] on a mismatch without ever
    /// calling the server's `FinishLogin` (so, unlike a real `login()`
    /// call, this never opens an extra session). Then drives a two-step
    /// OPAQUE registration exchange for `new_password`
    /// (`StartChangePassword`/`FinishChangePassword`) — the same shape as
    /// `create`'s, but authenticated against the existing session rather
    /// than opening a new one. The master key itself never changes, only
    /// what protects it: this reads the raw key already cached locally
    /// under [`crate::domains::secrets::MASTER_KEY_SECRET`] (never the
    /// wrapped form — see that constant's doc) and wraps that same key
    /// fresh under the new password's export key, to send the server in
    /// place of its previous wrapped copy — so every document encrypted
    /// under the old password stays decryptable, and there is nothing to
    /// update locally once the server confirms the change.
    async fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<()>;

    /// Lists every session open on the signed-in account, across all of its
    /// users, oldest first — the sessions its plan's `max_sessions` counts
    /// (see [`crate::Usage`]). The one this device is signed in with is
    /// flagged [`OpenSession::is_current`]. Never cached locally: other
    /// devices open and close sessions, so it needs the server.
    async fn list_sessions(&self) -> Result<Vec<OpenSession>>;

    /// Closes `session_id`, one of the sessions [`Service::list_sessions`]
    /// lists, logging its device out: it finds out on its next
    /// authenticated call ([`crate::ErrorCode::SessionExpired`]). Fails with
    /// [`crate::ErrorCode::NotFoundOnServer`] if it isn't one of the
    /// account's open sessions. Naming this device's own session closes it
    /// server-side like any other, so the SDK logs out locally on the next
    /// authenticated call — prefer [`Service::logout`] for that, which
    /// clears local state right away.
    async fn revoke_session(&self, session_id: &str) -> Result<()>;

    /// Returns whether this device has a usable OS credential store
    /// (Keychain, Secret Service, ...) to keep the account's secrets in —
    /// when it doesn't, `create` needs `allow_insecure_storage` to keep them
    /// in the local database instead, which a UI should ask the user to
    /// accept explicitly. Never fails: an unreachable store reports `false`.
    async fn is_secure_storage_available(&self) -> bool;

    /// Persists `language` (a short code, e.g. "en", "fr") as this device's
    /// UI language preference, read back by [`Service::get_language`].
    /// Purely local: unlike the `language` `create` sends the server once
    /// at registration (see [`LANGUAGE_SETTING`]'s doc), this is never
    /// synced there.
    async fn set_language(&self, language: &str) -> Result<()>;

    /// Returns this device's UI language preference previously persisted by
    /// [`Service::set_language`], or `None` if never set — callers should
    /// fall back to the platform's own locale in that case rather than
    /// assuming a language.
    async fn get_language(&self) -> Result<Option<String>>;

    /// Returns the role of the account behind the most recent `create`/
    /// `login` call, cached locally under [`ROLE_SETTING`], or `None` if
    /// neither has ever been called on this device.
    async fn role(&self) -> Result<Option<Role>>;
}

/// Initializes the users service: connects to the fyde server's users
/// service over the shared `channel`. `secrets` is where `create`/`login`
/// persist the account's master key, and `sessions` the session token (read
/// back by every other service's gRPC transport to authenticate their own
/// calls); `settings` holds the cached role and language preference.
/// `secrets` is cleared and `local_db` wiped by `logout`. `documents`'
/// [`DocumentsService::start_sync`] is called by `create`/`login` once a
/// session is open, and [`DocumentsService::stop_sync`] is called by
/// `logout` to stop that consumption again. The returned service is also
/// registered with `sessions` to log out locally once the server rejects
/// the session (see [`crate::domains::sessions::Service::check_response`]).
pub(crate) async fn init(
    channel: Channel,
    settings: Arc<dyn SettingsService>,
    secrets: Arc<dyn SecretsService>,
    sessions: Arc<SessionsClient>,
    local_db: Arc<dyn LocalDatabase>,
    documents: Arc<dyn DocumentsService>,
) -> Result<Arc<dyn Service>> {
    let users = Arc::new(
        UsersClient::new(
            channel,
            settings,
            secrets,
            sessions.clone(),
            local_db,
            documents,
        )
        .await?,
    );
    let handler: Arc<dyn ExpiredSessionHandler> = users.clone();
    sessions.on_session_expired(Arc::downgrade(&handler));
    Ok(users)
}
