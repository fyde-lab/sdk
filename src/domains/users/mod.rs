mod crypto;
mod grpc_client;
mod service;

pub use service::UsersClient;

use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Channel;

use crate::domains::documents::Service as DocumentsService;
use crate::domains::sessions::SessionsClient;
use crate::domains::settings::Service as SettingsService;
use crate::sql::LocalDatabase;
use crate::{ErrorContext as _, Result};

/// The settings key under which the account master key's *raw, decrypted*
/// bytes are cached locally (JSON-encoded, see [`encode_master_key`]/
/// [`decode_master_key`]), once `create`/`login` have decrypted it (and
/// re-read as-is by `change_password`, which never changes the underlying
/// key, only what protects it on the server). Deliberately never the
/// wrapped/encrypted form the server stores and returns: the SDK only ever
/// needs that ciphertext transiently, to send over the wire, and never has
/// a reason to persist it locally — see
/// [`crate::domains::documents::changelog::crypto`]'s `derive_kek`, which
/// reads this key to derive the KEK protecting every changelog event, and
/// would otherwise desync every time the wrapped form's nonce/ciphertext
/// changed (e.g. on a `change_password`) even though the raw key underneath
/// it didn't.
pub(crate) const MASTER_KEY_SETTING: &str = "master_key";

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
/// `../api-protos`.
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

/// JSON-encodes `master_key`'s raw bytes for storage under
/// [`MASTER_KEY_SETTING`].
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
/// `create`/`login` is persisted in the settings store under
/// `session_token` (see
/// [`crate::domains::sessions::SESSION_TOKEN_SETTING`]), shared with every
/// other service's gRPC transport, which reads it from there to
/// authenticate outgoing calls (see
/// [`crate::domains::sessions::Service::authenticated_request`]) rather than it being
/// threaded through every call here — so `logout` takes no argument.
/// Persisting it in settings, rather than only holding it in memory, means
/// a session survives across process restarts; `logout` removes it again.
#[async_trait]
pub trait Service: Send + Sync {
    /// Creates a new account and opens a session for the device named
    /// `device_name`, returning its session token, via a two-step OPAQUE
    /// registration exchange (`StartRegistration`/`FinishRegistration`).
    /// Also generates a random master key, encrypts it under a key derived
    /// from the OPAQUE export key produced by that exchange (never
    /// `password` directly), and persists it in the settings store under
    /// `master_key`. The server's response carries only the new session's
    /// token (see `../api-protos/users.proto`), so there is no user payload
    /// to return here. Fails with [`crate::Error::Grpc`] (`ALREADY_EXISTS`)
    /// if the username is already taken. Once the session and master key are
    /// persisted, starts changelog consumption via
    /// [`DocumentsService::start_sync`].
    async fn create(&self, username: &str, password: &str, device_name: &str) -> Result<String>;

    /// Verifies `username`/`password` and opens a session for the device
    /// named `device_name`, returning its session token, via a two-step
    /// OPAQUE login exchange (`StartLogin`/`FinishLogin`). An incorrect
    /// password is often detected locally, without a round trip to the
    /// server, and fails with [`crate::Error::InvalidCredentials`] in that
    /// case; failure detected by the server instead surfaces the usual way,
    /// as [`crate::Error::Grpc`] (`UNAUTHENTICATED`). Once the session and
    /// master key are persisted, starts changelog consumption via
    /// [`DocumentsService::start_sync`].
    async fn login(&self, username: &str, password: &str, device_name: &str) -> Result<String>;

    /// Closes the session opened by the most recent `create`/`login` call
    /// and wipes the SDK's local database, clearing all locally cached
    /// state (documents, settings — including the session token itself —
    /// and the changelog offset). A no-op if there is no open session.
    async fn logout(&self) -> Result<()>;

    /// Changes the password of the account behind the current session,
    /// identified by `username` (needed to verify `old_password`; the
    /// session itself is what authenticates the change server-side — see
    /// below). Verifies `old_password` the same way `login` verifies a
    /// password: the local half of an OPAQUE login exchange, which fails
    /// with [`crate::Error::InvalidCredentials`] on a mismatch without ever
    /// calling the server's `FinishLogin` (so, unlike a real `login()`
    /// call, this never opens an extra session). Then drives a two-step
    /// OPAQUE registration exchange for `new_password`
    /// (`StartChangePassword`/`FinishChangePassword`) — the same shape as
    /// `create`'s, but authenticated against the existing session rather
    /// than opening a new one. The master key itself never changes, only
    /// what protects it: this reads the raw key already cached locally
    /// under [`MASTER_KEY_SETTING`] (never the wrapped form — see that
    /// constant's doc) and wraps that same key fresh under the new
    /// password's export key, to send the server in place of its previous
    /// wrapped copy — so every document encrypted under the old password
    /// stays decryptable, and there is nothing to update in local settings
    /// once the server confirms the change.
    async fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<()>;

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
/// service over the shared `channel`. `settings` is where `create`/`login`
/// persist the session token (read back by every other service's gRPC
/// transport, via `sessions`, to authenticate their own calls) and where
/// `create` also persists the master key it generates for a new account.
/// `local_db` is wiped by `logout` to clear local state. `documents`'
/// [`DocumentsService::start_sync`] is called by `create`/`login` once a
/// session is open, and [`DocumentsService::stop_sync`] is called by
/// `logout` to stop that consumption again.
pub(crate) async fn init(
    channel: Channel,
    settings: Arc<dyn SettingsService>,
    sessions: Arc<SessionsClient>,
    local_db: Arc<dyn LocalDatabase>,
    documents: Arc<dyn DocumentsService>,
) -> Result<Arc<dyn Service>> {
    Ok(Arc::new(
        UsersClient::new(channel, settings, sessions, local_db, documents).await?,
    ))
}
