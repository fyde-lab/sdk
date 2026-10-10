use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
#[cfg(test)]
use mockall::automock;
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::{Error, ErrorContext as _, Result};

/// Length in bytes of an AES-256 key: the master key, and the key derived
/// from the OPAQUE export key to wrap it.
const KEY_LEN: usize = 32;
/// Length in bytes of an AES-GCM nonce (96 bits, as recommended by NIST
/// SP 800-38D / OWASP for AES-GCM).
const NONCE_LEN: usize = 12;

/// The OPAQUE ciphersuite used across the fyde protocol: the ristretto255
/// group for both the OPRF and the key exchange, SHA-512 as the
/// key-exchange hash (3DH), and Argon2id — configured via
/// [`high_effort_ksf`] — as the key-stretching function (KSF). Must match
/// what the server declares (`../../../../server/CLAUDE.md`'s `tools::opaque`
/// section) for registration and login to compute the same record on both
/// sides, though the `Ksf` type specifically only ever matters here: key
/// stretching in OPAQUE is a client-only operation.
struct FydeCipherSuite;

impl CipherSuite for FydeCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2_opaque::Sha512>;
    type Ksf = Argon2<'static>;
}

/// RFC 9106 §4's "low-memory" recommended Argon2id parameters (64 MiB, 3
/// iterations, 4 lanes) — safe on the mobile clients this SDK is meant to
/// eventually back (see the top-level `CLAUDE.md`'s `application/` entry),
/// unlike the RFC's other "high-memory" option (2 GiB), which risks getting
/// OOM-killed there. Applied as OPAQUE's key-stretching function on every
/// registration and login, on top of (not instead of) the security its
/// oblivious PRF already provides: the KSF is what protects against a
/// compromised server using a stolen OPRF key to brute-force weak
/// passwords offline.
const KSF_M_COST_KIB: u32 = 64 * 1024;
const KSF_T_COST: u32 = 3;
const KSF_P_COST: u32 = 4;

fn high_effort_ksf() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(KSF_M_COST_KIB, KSF_T_COST, KSF_P_COST, None)
            .expect("hardcoded Argon2 parameters are valid"),
    )
}

/// Client-side state produced by `start_registration`, to be round-tripped
/// into `finish_registration` once the server's response arrives. Opaque
/// outside this module: wraps `opaque-ke`'s generic ciphersuite machinery
/// so callers never need to name [`FydeCipherSuite`] themselves.
pub(super) struct RegistrationState(ClientRegistration<FydeCipherSuite>);

/// Client-side state produced by `start_login`, to be round-tripped into
/// `finish_login` once the server's response arrives.
pub(super) struct LoginState(ClientLogin<FydeCipherSuite>);

/// Drives the client side of the OPAQUE registration/login exchange.
/// Abstracted as a trait — even though, unlike [`super::grpc_client::FydeClient`],
/// there's only ever one real implementation — so [`super::service`]'s tests
/// can substitute [`MockOpaqueClient`] instead of paying the cost of the
/// real (deliberately expensive, see [`high_effort_ksf`]) key-stretching
/// function on every test, and without needing to hand-construct
/// protocol-valid response bytes for every scenario.
#[cfg_attr(test, automock)]
pub(super) trait OpaqueClient: Send + Sync {
    /// First step of OPAQUE registration: derives an oblivious request
    /// from `password`, to send to the server's `StartRegistration` RPC.
    fn start_registration(&self, password: &str) -> Result<(RegistrationState, Vec<u8>)>;

    /// Second and final step of OPAQUE registration: turns the server's
    /// `StartRegistration` response into the registration record to send
    /// to `FinishRegistration`, alongside the export key the server never
    /// sees — used by [`generate_and_wrap_master_key`] to protect the
    /// account's master key.
    fn finish_registration(
        &self,
        state: RegistrationState,
        password: &str,
        response: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)>;

    /// First step of OPAQUE login: derives an oblivious request from
    /// `password`, to send to the server's `StartLogin` RPC.
    fn start_login(&self, password: &str) -> Result<(LoginState, Vec<u8>)>;

    /// Second and final step of OPAQUE login: turns the server's
    /// `StartLogin` response into the finalization message to send to
    /// `FinishLogin`, alongside the export key — deterministic across every
    /// successful login for the same account/password, so it's the same
    /// value [`generate_and_wrap_master_key`] used at registration time —
    /// used by [`unwrap_master_key`] to recover the account's master key
    /// from what `FinishLogin` returns. A wrong password is detected right
    /// here, without a round trip to the server: OPAQUE's envelope can't be
    /// opened with the wrong password-derived key, so there's no
    /// finalization message to produce. Fails with
    /// [`crate::Error::InvalidCredentials`] in that case — the same error a
    /// wrong password produces if it isn't caught until the server rejects
    /// `FinishLogin`.
    fn finish_login(
        &self,
        state: LoginState,
        password: &str,
        response: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)>;
}

/// The default [`OpaqueClient`] implementation, backed by real `opaque-ke`
/// cryptographic operations.
pub(super) struct DefaultOpaqueClient;

impl OpaqueClient for DefaultOpaqueClient {
    fn start_registration(&self, password: &str) -> Result<(RegistrationState, Vec<u8>)> {
        let mut rng = OsRng;
        let result = ClientRegistration::<FydeCipherSuite>::start(&mut rng, password.as_bytes())
            .map_err(|err| {
                Error::Encryption(format!("failed to start OPAQUE registration: {err}"))
            })?;

        Ok((
            RegistrationState(result.state),
            result.message.serialize().to_vec(),
        ))
    }

    fn finish_registration(
        &self,
        state: RegistrationState,
        password: &str,
        response: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let response = RegistrationResponse::deserialize(response).map_err(|err| {
            Error::Encryption(format!(
                "failed to parse OPAQUE registration response: {err}"
            ))
        })?;

        let mut rng = OsRng;
        let ksf = high_effort_ksf();
        let result = state
            .0
            .finish(
                &mut rng,
                password.as_bytes(),
                response,
                ClientRegistrationFinishParameters::new(Default::default(), Some(&ksf)),
            )
            .map_err(|err| {
                Error::Encryption(format!("failed to finish OPAQUE registration: {err}"))
            })?;

        Ok((
            result.message.serialize().to_vec(),
            result.export_key.to_vec(),
        ))
    }

    fn start_login(&self, password: &str) -> Result<(LoginState, Vec<u8>)> {
        let mut rng = OsRng;
        let result = ClientLogin::<FydeCipherSuite>::start(&mut rng, password.as_bytes())
            .map_err(|err| Error::Encryption(format!("failed to start OPAQUE login: {err}")))?;

        Ok((
            LoginState(result.state),
            result.message.serialize().to_vec(),
        ))
    }

    fn finish_login(
        &self,
        state: LoginState,
        password: &str,
        response: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let response = CredentialResponse::deserialize(response).map_err(|err| {
            Error::Encryption(format!("failed to parse OPAQUE login response: {err}"))
        })?;

        let mut rng = OsRng;
        let ksf = high_effort_ksf();
        let result = state
            .0
            .finish(
                &mut rng,
                password.as_bytes(),
                response,
                ClientLoginFinishParameters::new(None, Default::default(), Some(&ksf)),
            )
            .map_err(|_| Error::InvalidCredentials)?;

        Ok((
            result.message.serialize().to_vec(),
            result.export_key.to_vec(),
        ))
    }
}

/// Produces a placeholder [`RegistrationState`]/[`LoginState`] for tests
/// outside this module (`super::service`'s, mocking [`OpaqueClient`]) that
/// need *a* value satisfying `finish_registration`/`finish_login`'s
/// signature but don't care about its content — [`FydeCipherSuite`] is
/// private to this module, so nothing outside it can construct one
/// directly. Cheap: only exercises OPAQUE's first message (blinding), never
/// the key-stretching function.
#[cfg(test)]
pub(super) fn fake_registration_state() -> RegistrationState {
    RegistrationState(
        ClientRegistration::<FydeCipherSuite>::start(&mut OsRng, b"placeholder")
            .unwrap()
            .state,
    )
}

#[cfg(test)]
pub(super) fn fake_login_state() -> LoginState {
    LoginState(
        ClientLogin::<FydeCipherSuite>::start(&mut OsRng, b"placeholder")
            .unwrap()
            .state,
    )
}

/// A random master key wrapped under a key derived from the OPAQUE export
/// key (see [`finish_registration`]), JSON-serialized into the single
/// `encrypted_master_key` value the server stores. `nonce` isn't secret —
/// it's required to decrypt, so it travels alongside the ciphertext.
#[derive(Serialize, Deserialize)]
struct WrappedMasterKey {
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// Generates a fresh random 256-bit master key using the OS CSPRNG, as
/// recommended by OWASP for symmetric key generation.
fn generate_master_key() -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    rand::rng().fill_bytes(&mut key);
    key
}

/// Derives a 256-bit key-wrapping key from an OPAQUE export key via
/// HKDF-SHA256. No further stretching happens here: the export key is
/// already the output of OPAQUE's own key-stretched, oblivious-PRF-hardened
/// exchange (see [`high_effort_ksf`]) — running another slow hash over it
/// would add cost without adding security.
fn derive_wrapping_key(export_key: &[u8]) -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    Hkdf::<sha2_opaque::Sha256>::new(None, export_key)
        .expand(b"fyde/master-key-wrap-key-v1", &mut key)
        .map_err(|err| Error::Encryption(format!("failed to derive key from export key: {err}")))?;
    Ok(key)
}

/// Encrypts `master_key` under a key derived from `export_key`
/// (AES-256-GCM, random nonce), returning the result JSON-serialized and
/// ready to send to the server as `encrypted_master_key`. The server is the
/// only place this wrapped form is ever persisted — see
/// [`crate::domains::secrets::MASTER_KEY_SECRET`] for why the SDK itself
/// caches only the raw key locally, never this wrapped form. Used both by
/// [`generate_and_wrap_master_key`] and directly by `change_password`,
/// which already has the raw key on hand (read back from the credential
/// store) and only needs to wrap it fresh under the new password's export
/// key.
pub(super) fn wrap_master_key(master_key: &[u8], export_key: &[u8]) -> Result<String> {
    let master_key: &[u8; KEY_LEN] = master_key
        .try_into()
        .map_err(|_| Error::Encryption("master key has an invalid length".into()))?;

    let wrapping_key =
        derive_wrapping_key(export_key).context("failed to derive master key wrapping key")?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);

    let cipher = Aes256Gcm::new(&wrapping_key.into());
    let ciphertext = cipher
        .encrypt(&nonce, master_key.as_slice())
        .map_err(|_| Error::Encryption("failed to encrypt master key".into()))?;

    let wrapped = WrappedMasterKey {
        nonce: nonce_bytes.to_vec(),
        ciphertext,
    };

    serde_json::to_string(&wrapped).context("failed to serialize wrapped master key")
}

/// Generates a fresh random master key and wraps it under a key derived
/// from `export_key` (see [`wrap_master_key`]), returning both the raw key
/// — the only form the SDK persists locally, under
/// [`crate::domains::secrets::MASTER_KEY_SECRET`] — and the wrapped form
/// sent to the server, which is the only place it's ever stored.
pub(super) fn generate_and_wrap_master_key(export_key: &[u8]) -> Result<(Vec<u8>, String)> {
    let master_key = generate_master_key();
    let wrapped = wrap_master_key(&master_key, export_key)?;
    Ok((master_key.to_vec(), wrapped))
}

/// Decrypts a [`generate_and_wrap_master_key`]-produced wrapped master key
/// back to the raw master key, using a wrapping key derived from
/// `export_key` the same way registration derived the one that wrapped it.
/// Used by `login` to recover the account's master key from what
/// `FinishLogin` returns, on a device that doesn't already have it cached
/// locally.
pub(super) fn unwrap_master_key(export_key: &[u8], wrapped_json: &str) -> Result<Vec<u8>> {
    let wrapped: WrappedMasterKey =
        serde_json::from_str(wrapped_json).context("failed to parse wrapped master key")?;

    let wrapping_key =
        derive_wrapping_key(export_key).context("failed to derive master key wrapping key")?;

    let nonce_bytes: [u8; NONCE_LEN] = wrapped
        .nonce
        .try_into()
        .map_err(|_| Error::Encryption("wrapped master key nonce has the wrong length".into()))?;

    let cipher = Aes256Gcm::new(&wrapping_key.into());
    cipher
        .decrypt(&Nonce::from(nonce_bytes), wrapped.ciphertext.as_slice())
        .map_err(|_| Error::Encryption("failed to decrypt master key".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a full OPAQUE registration for `password` against an in-process
    /// server (real `opaque-ke`, no mocks, since this module's whole job is
    /// to drive that crate correctly) sharing `server_setup` — the same
    /// setup a subsequent login must use, since the client's envelope
    /// embeds and cross-checks the server's static public key from
    /// registration time — returning `(registration record, export key)`.
    /// Exercises the real (expensive) high-effort KSF, so tests share this
    /// helper rather than each registering from scratch.
    fn register(
        server_setup: &opaque_ke::ServerSetup<FydeCipherSuite>,
        password: &str,
    ) -> (Vec<u8>, Vec<u8>) {
        use opaque_ke::{RegistrationRequest, RegistrationUpload, ServerRegistration};

        let client = DefaultOpaqueClient;

        let (state, request_bytes) = client.start_registration(password).unwrap();
        let server_response = ServerRegistration::<FydeCipherSuite>::start(
            server_setup,
            RegistrationRequest::deserialize(&request_bytes).unwrap(),
            b"alice",
        )
        .unwrap();

        let (upload_bytes, export_key) = client
            .finish_registration(state, password, &server_response.message.serialize())
            .unwrap();

        let record = ServerRegistration::<FydeCipherSuite>::finish(
            RegistrationUpload::deserialize(&upload_bytes).unwrap(),
        )
        .serialize()
        .to_vec();

        (record, export_key)
    }

    #[test]
    fn finish_login_succeeds_with_the_correct_password_and_the_server_accepts_it() {
        use opaque_ke::rand::rngs::OsRng as ServerOsRng;
        use opaque_ke::{
            CredentialRequest, ServerLogin, ServerLoginParameters, ServerRegistration, ServerSetup,
        };

        let mut server_rng = ServerOsRng;
        let server_setup = ServerSetup::<FydeCipherSuite>::new(&mut server_rng);
        let client = DefaultOpaqueClient;
        let password = "correct horse battery staple";
        let (record, export_key) = register(&server_setup, password);
        assert!(!export_key.is_empty());

        let (login_state, login_request_bytes) = client.start_login(password).unwrap();
        let server_login = ServerLogin::start(
            &mut server_rng,
            &server_setup,
            Some(ServerRegistration::<FydeCipherSuite>::deserialize(&record).unwrap()),
            CredentialRequest::deserialize(&login_request_bytes).unwrap(),
            b"alice",
            ServerLoginParameters::default(),
        )
        .unwrap();

        let (finalization_bytes, login_export_key) = client
            .finish_login(login_state, password, &server_login.message.serialize())
            .unwrap();
        // The export key `finish_login` returns must match the one
        // `finish_registration` produced for the same account/password,
        // since `login()` uses it to unwrap the master key
        // `finish_registration`'s caller wrapped.
        assert_eq!(login_export_key, export_key);

        // Proves the finalization this module produced is one the server
        // actually accepts, not just that `finish_login` returned `Ok`.
        server_login
            .state
            .finish(
                opaque_ke::CredentialFinalization::deserialize(&finalization_bytes).unwrap(),
                ServerLoginParameters::default(),
            )
            .unwrap();
    }

    #[test]
    fn finish_login_rejects_the_wrong_password() {
        use opaque_ke::rand::rngs::OsRng as ServerOsRng;
        use opaque_ke::{
            CredentialRequest, ServerLogin, ServerLoginParameters, ServerRegistration, ServerSetup,
        };

        let mut server_rng = ServerOsRng;
        let server_setup = ServerSetup::<FydeCipherSuite>::new(&mut server_rng);
        let client = DefaultOpaqueClient;
        let (record, _export_key) = register(&server_setup, "correct horse battery staple");

        let (login_state, login_request_bytes) = client.start_login("wrong password").unwrap();
        let server_login = ServerLogin::start(
            &mut server_rng,
            &server_setup,
            Some(ServerRegistration::<FydeCipherSuite>::deserialize(&record).unwrap()),
            CredentialRequest::deserialize(&login_request_bytes).unwrap(),
            b"alice",
            ServerLoginParameters::default(),
        )
        .unwrap();

        let err = client
            .finish_login(
                login_state,
                "wrong password",
                &server_login.message.serialize(),
            )
            .unwrap_err();

        assert!(matches!(err, Error::InvalidCredentials));
    }

    #[test]
    fn generate_and_wrap_master_key_roundtrips_under_the_same_export_key() {
        let export_key = b"a-fake-64-byte-export-key-------------------------------------";

        let (raw_master_key, wrapped) = generate_and_wrap_master_key(export_key).unwrap();

        let master_key = unwrap_master_key(export_key, &wrapped).unwrap();

        assert_eq!(master_key.len(), KEY_LEN);
        assert_eq!(master_key, raw_master_key);
    }

    #[test]
    fn generate_and_wrap_master_key_produces_a_fresh_key_and_nonce_each_call() {
        let export_key = b"a-fake-64-byte-export-key-------------------------------------";

        let (first_raw, first) = generate_and_wrap_master_key(export_key).unwrap();
        let (second_raw, second) = generate_and_wrap_master_key(export_key).unwrap();

        assert_ne!(first, second);
        assert_ne!(first_raw, second_raw);
        let first_key = unwrap_master_key(export_key, &first).unwrap();
        let second_key = unwrap_master_key(export_key, &second).unwrap();
        assert_ne!(first_key, second_key);
    }

    #[test]
    fn wrapped_master_key_cannot_be_unwrapped_with_the_wrong_export_key() {
        let export_key = b"a-fake-64-byte-export-key-------------------------------------";
        let (_, wrapped) = generate_and_wrap_master_key(export_key).unwrap();

        let err = unwrap_master_key(b"a-different-export-key", &wrapped).unwrap_err();

        assert!(matches!(err, Error::Encryption(_)));
    }

    #[test]
    fn wrap_master_key_lets_a_raw_key_be_rewrapped_under_a_different_export_key() {
        let old_export_key = b"a-fake-64-byte-export-key-------------------------------------";
        let new_export_key = b"a-different-64-byte-export-key--------------------------------";
        let (raw_master_key, wrapped) = generate_and_wrap_master_key(old_export_key).unwrap();

        let rewrapped = wrap_master_key(&raw_master_key, new_export_key).unwrap();

        assert_ne!(rewrapped, wrapped);
        let unwrapped_under_new_key = unwrap_master_key(new_export_key, &rewrapped).unwrap();
        assert_eq!(unwrapped_under_new_key, raw_master_key);
    }

    #[test]
    fn wrap_master_key_rejects_a_key_of_the_wrong_length() {
        let export_key = b"a-fake-64-byte-export-key-------------------------------------";

        let err = wrap_master_key(b"too-short", export_key).unwrap_err();

        assert!(matches!(err, Error::Encryption(_)));
    }
}
