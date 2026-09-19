use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::services::documents::Metadata;
use crate::{Error, ErrorContext as _, Result};

use super::service::EventType;

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

/// TEMPORARY placeholder secret used to derive the key-encryption-key (KEK)
/// that wraps each changelog event's DEK.
///
/// Hard-coding a cryptographic secret in source is a known anti-pattern
/// (CWE-798, OWASP A02:2021 Cryptographic Failures): anyone with the binary
/// or repo history can recover it and decrypt every wrapped DEK. This must
/// be replaced with a key sourced from a proper KMS/HSM (or at minimum an
/// operator-provided secret loaded from a secrets manager or environment)
/// before this code handles real data.
const TEMP_HARDCODED_KEK_SECRET: &[u8] = b"CHANGE-ME-temporary-development-only-secret";

/// The plaintext shape of a changelog event, MessagePack-serialized then
/// encrypted as a whole. Mirrors [`super::ChangelogEvent`] minus `offset`
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

/// Derives the temporary KEK from the hard-coded placeholder secret.
fn derive_temporary_kek() -> [u8; KEY_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(TEMP_HARDCODED_KEK_SECRET);
    hasher.finalize().into()
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
pub(super) fn encrypt_event(
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

    let kek = derive_temporary_kek();
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
pub(super) fn decrypt_event(encrypted_content: &[u8]) -> Result<DecryptedEvent> {
    if encrypted_content.len() < WRAPPED_KEY_LEN {
        return Err(Error::Encryption("changelog event too short".into()));
    }
    let (wrapped_dek, encrypted_payload) = encrypted_content.split_at(WRAPPED_KEY_LEN);

    let kek = derive_temporary_kek();
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

    fn metadata() -> Metadata {
        Metadata {
            name: "report.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            created_at: 1_700_000_000,
            size: 4,
            checksum: "checksum-value".to_string(),
            transcript: "some transcript text".to_string(),
        }
    }

    #[test]
    fn encrypt_event_roundtrips_a_created_event() {
        let document_id = Uuid::new_v4();
        let metadata = metadata();

        let encrypted = encrypt_event(
            EventType::Created,
            document_id,
            Some(b"body"),
            Some(&metadata),
        )
        .unwrap();

        let (event_type, decrypted_id, content, decrypted_metadata) =
            decrypt_event(&encrypted).unwrap();

        assert_eq!(event_type, EventType::Created);
        assert_eq!(decrypted_id, document_id);
        assert_eq!(content, Some(b"body".to_vec()));
        assert_eq!(decrypted_metadata, Some(metadata));
    }

    #[test]
    fn encrypt_event_roundtrips_a_deleted_event_with_no_content_or_metadata() {
        let document_id = Uuid::new_v4();

        let encrypted = encrypt_event(EventType::Deleted, document_id, None, None).unwrap();

        let (event_type, decrypted_id, content, metadata) = decrypt_event(&encrypted).unwrap();

        assert_eq!(event_type, EventType::Deleted);
        assert_eq!(decrypted_id, document_id);
        assert_eq!(content, None);
        assert_eq!(metadata, None);
    }

    #[test]
    fn decrypt_event_rejects_content_tampered_with_after_encryption() {
        let document_id = Uuid::new_v4();
        let mut encrypted =
            encrypt_event(EventType::Created, document_id, Some(b"body"), None).unwrap();
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0xff;

        assert!(decrypt_event(&encrypted).is_err());
    }
}
