use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::domains::documents::Metadata;
use crate::domains::settings::Service as SettingsService;
use crate::domains::users::MASTER_KEY_SETTING;
use crate::{Error, ErrorContext as _, Result};

use super::models::EventType;

/// Length in bytes of an AES-256 key (DEK or KEK).
const KEY_LEN: usize = 32;
/// Length in bytes of an AES-GCM nonce (96 bits, as recommended by NIST
/// SP 800-38D / OWASP for AES-GCM).
const NONCE_LEN: usize = 12;
/// Length in bytes of the AES-GCM authentication tag AEAD appends to every
/// ciphertext.
const TAG_LEN: usize = 16;
/// Length in bytes of a wrapped DEK: always exactly this, since a DEK is
/// always [`KEY_LEN`] bytes. Used to split a changelog event's opaque
/// `encrypted_content` back into its wrapped DEK and ciphertext.
const WRAPPED_KEY_LEN: usize = NONCE_LEN + KEY_LEN + TAG_LEN;

/// The plaintext shape of a changelog event, MessagePack-serialized then
/// encrypted as a whole. Mirrors [`super::ChangelogEvent`] minus `id`
/// (assigned by the server, sent alongside the encrypted blob in the
/// clear).
#[derive(Serialize, Deserialize)]
struct EventPayload {
    event_type: EventType,
    document_id: Uuid,
    #[serde(with = "serde_bytes")]
    content: Option<Vec<u8>>,
    metadata: Option<Metadata>,
}

/// Generates a fresh random 256-bit data encryption key (DEK) using the OS
/// CSPRNG, as recommended by OWASP for symmetric key generation.
fn generate_dek() -> [u8; KEY_LEN] {
    let mut dek = [0u8; KEY_LEN];
    rand::rng().fill_bytes(&mut dek);
    dek
}

/// Derives the key-encryption-key (KEK) that wraps every changelog event's
/// DEK from the account's master key, persisted locally under
/// [`MASTER_KEY_SETTING`] by `users::Service::create`/`login`. Fails with
/// [`Error::Encryption`] if no master key is on hand yet — i.e. before any
/// account has been created or logged into on this device.
async fn derive_kek(settings: &dyn SettingsService) -> Result<[u8; KEY_LEN]> {
    let master_key = settings
        .get(MASTER_KEY_SETTING)
        .await
        .context("failed to read master key from local settings")?
        .ok_or_else(|| {
            Error::Encryption(
                "no master key found in local settings; log in or create an account first".into(),
            )
        })?;

    let mut hasher = Sha256::new();
    hasher.update(master_key.as_bytes());
    Ok(hasher.finalize().into())
}

/// Encrypts `plaintext` under `key` with AES-256-GCM using a random nonce,
/// and returns `nonce || ciphertext_with_tag`.
fn aead_encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| Error::Encryption("failed to encrypt data".into()))?;

    let mut output = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

/// Decrypts a `nonce || ciphertext_with_tag` blob produced by
/// [`aead_encrypt`] under `key`.
fn aead_decrypt(key: &[u8; KEY_LEN], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < NONCE_LEN {
        return Err(Error::Encryption("ciphertext too short".into()));
    }
    let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
    let nonce_bytes: [u8; NONCE_LEN] = nonce_bytes
        .try_into()
        .expect("split_at(NONCE_LEN) guarantees this length");
    let nonce = Nonce::from(nonce_bytes);

    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| Error::Encryption("failed to decrypt data".into()))
}

/// Encrypts a changelog event using envelope encryption, the same technique
/// used for document uploads before the documents service was folded into
/// the changelog:
///
/// 1. MessagePack-serializes `{event_type, document_id, content, metadata}`.
/// 2. Generates a fresh, random DEK and encrypts the serialized bytes under
///    it (AES-256-GCM, random nonce).
/// 3. Wraps the DEK itself under a KEK (AES-256-GCM), so only the wrapped
///    DEK ever leaves this process.
///
/// Returns `wrapped_dek || ciphertext` — a single opaque blob, since the
/// server's `changelog` table has no separate column for the DEK.
pub(super) async fn encrypt_event(
    settings: &dyn SettingsService,
    event_type: EventType,
    document_id: Uuid,
    content: Option<&[u8]>,
    metadata: Option<&Metadata>,
) -> Result<Vec<u8>> {
    let payload = EventPayload {
        event_type,
        document_id,
        content: content.map(<[u8]>::to_vec),
        metadata: metadata.cloned(),
    };
    let bytes = rmp_serde::to_vec(&payload).context("failed to serialize changelog event")?;

    let dek = generate_dek();
    let encrypted_payload =
        aead_encrypt(&dek, &bytes).context("failed to encrypt changelog event")?;

    let kek = derive_kek(settings).await?;
    let wrapped_dek = aead_encrypt(&kek, &dek).context("failed to wrap changelog event DEK")?;

    let mut encrypted_content = Vec::with_capacity(wrapped_dek.len() + encrypted_payload.len());
    encrypted_content.extend_from_slice(&wrapped_dek);
    encrypted_content.extend_from_slice(&encrypted_payload);
    Ok(encrypted_content)
}

/// The plaintext fields of a decrypted changelog event: `event_type`,
/// `document_id`, `content`, `metadata`.
pub(super) type DecryptedEvent = (EventType, Uuid, Option<Vec<u8>>, Option<Metadata>);

/// Decrypts an `encrypted_content` blob produced by [`encrypt_event`].
pub(super) async fn decrypt_event(
    settings: &dyn SettingsService,
    encrypted_content: &[u8],
) -> Result<DecryptedEvent> {
    if encrypted_content.len() < WRAPPED_KEY_LEN {
        return Err(Error::Encryption("changelog event too short".into()));
    }
    let (wrapped_dek, encrypted_payload) = encrypted_content.split_at(WRAPPED_KEY_LEN);

    let kek = derive_kek(settings).await?;
    let dek = aead_decrypt(&kek, wrapped_dek).context("failed to unwrap changelog event DEK")?;
    let dek: [u8; KEY_LEN] = dek
        .try_into()
        .map_err(|_| Error::Encryption("unwrapped DEK has an invalid length".into()))?;

    let bytes =
        aead_decrypt(&dek, encrypted_payload).context("failed to decrypt changelog event")?;
    let payload: EventPayload =
        rmp_serde::from_slice(&bytes).context("failed to deserialize changelog event")?;

    Ok((
        payload.event_type,
        payload.document_id,
        payload.content,
        payload.metadata,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::FakeMetadata;
    use crate::domains::settings::MockService as MockSettingsService;

    /// A [`SettingsService`] fixture with a fixed master key set under
    /// [`MASTER_KEY_SETTING`], for exercising real KEK derivation without
    /// depending on `users::Service::create`/`login`.
    fn settings_with_master_key() -> MockSettingsService {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == MASTER_KEY_SETTING)
            .returning(|_| Ok(Some("the-account-master-key".to_string())));
        settings
    }

    #[tokio::test]
    async fn encrypt_event_roundtrips_a_created_event() {
        let settings = settings_with_master_key();
        let document_id = Uuid::now_v7();
        let metadata = FakeMetadata::new().build();

        let encrypted = encrypt_event(
            &settings,
            EventType::Created,
            document_id,
            Some(b"body"),
            Some(&metadata),
        )
        .await
        .unwrap();

        let (event_type, decrypted_id, content, decrypted_metadata) =
            decrypt_event(&settings, &encrypted).await.unwrap();

        assert_eq!(event_type, EventType::Created);
        assert_eq!(decrypted_id, document_id);
        assert_eq!(content, Some(b"body".to_vec()));
        assert_eq!(decrypted_metadata, Some(metadata));
    }

    #[tokio::test]
    async fn encrypt_event_roundtrips_a_deleted_event_with_no_content_or_metadata() {
        let settings = settings_with_master_key();
        let document_id = Uuid::now_v7();

        let encrypted = encrypt_event(&settings, EventType::Deleted, document_id, None, None)
            .await
            .unwrap();

        let (event_type, decrypted_id, content, metadata) =
            decrypt_event(&settings, &encrypted).await.unwrap();

        assert_eq!(event_type, EventType::Deleted);
        assert_eq!(decrypted_id, document_id);
        assert_eq!(content, None);
        assert_eq!(metadata, None);
    }

    #[tokio::test]
    async fn decrypt_event_rejects_content_tampered_with_after_encryption() {
        let settings = settings_with_master_key();
        let document_id = Uuid::now_v7();
        let mut encrypted = encrypt_event(
            &settings,
            EventType::Created,
            document_id,
            Some(b"body"),
            None,
        )
        .await
        .unwrap();
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0xff;

        assert!(decrypt_event(&settings, &encrypted).await.is_err());
    }

    #[tokio::test]
    async fn decrypt_event_rejects_a_blob_shorter_than_a_wrapped_key() {
        let settings = settings_with_master_key();

        assert!(decrypt_event(&settings, b"too short").await.is_err());
    }

    #[tokio::test]
    async fn encrypt_event_fails_without_a_master_key_in_settings() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .withf(|key| key == MASTER_KEY_SETTING)
            .returning(|_| Ok(None));
        let document_id = Uuid::now_v7();

        let err = encrypt_event(
            &settings,
            EventType::Created,
            document_id,
            Some(b"body"),
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, Error::Encryption(_)));
    }
}
