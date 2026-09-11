use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Length in bytes of an AES-256 key (DEK or KEK).
pub(super) const KEY_LEN: usize = 32;
/// Length in bytes of an AES-GCM nonce (96 bits, as recommended by NIST
/// SP 800-38D / OWASP for AES-GCM).
const NONCE_LEN: usize = 12;

/// TEMPORARY placeholder secret used to derive the key-encryption-key (KEK)
/// that wraps each document's DEK.
///
/// Hard-coding a cryptographic secret in source is a known anti-pattern
/// (CWE-798, OWASP A02:2021 Cryptographic Failures): anyone with the binary
/// or repo history can recover it and decrypt every wrapped DEK. This must
/// be replaced with a key sourced from a proper KMS/HSM (or at minimum an
/// operator-provided secret loaded from a secrets manager or environment)
/// before this code handles real data.
const TEMP_HARDCODED_KEK_SECRET: &[u8] = b"CHANGE-ME-temporary-development-only-secret";

/// Generates a fresh random 256-bit data encryption key (DEK) using the OS
/// CSPRNG, as recommended by OWASP for symmetric key generation.
pub(super) fn generate_dek() -> [u8; KEY_LEN] {
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

/// Encrypts a document for upload using envelope encryption:
///
/// 1. Generates a fresh, random DEK.
/// 2. Encrypts `content` and `metadata` under that DEK (AES-256-GCM, one
///    random nonce per encryption).
/// 3. Wraps the DEK itself under a KEK (AES-256-GCM), so only the wrapped
///    DEK ever leaves this process.
///
/// Returns `(encrypted_content, wrapped_dek, encrypted_metadata)`.
pub(super) fn encrypt_document(
    content: &[u8],
    metadata: &[u8],
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let dek = generate_dek();

    let encrypted_content = aead_encrypt(&dek, content)?;
    let encrypted_metadata = aead_encrypt(&dek, metadata)?;

    let kek = derive_temporary_kek();
    let wrapped_dek = aead_encrypt(&kek, &dek)?;

    Ok((encrypted_content, wrapped_dek, encrypted_metadata))
}
