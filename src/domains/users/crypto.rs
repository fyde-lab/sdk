use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::{Error, ErrorContext as _, Result};

/// Length in bytes of an AES-256 key: the master key, and the key derived
/// from the user's password to wrap it.
const KEY_LEN: usize = 32;
/// Length in bytes of an AES-GCM nonce (96 bits, as recommended by NIST
/// SP 800-38D / OWASP for AES-GCM).
const NONCE_LEN: usize = 12;
/// Length in bytes of the random salt used to derive the password-wrapping
/// key.
const SALT_LEN: usize = 16;

/// A random master key wrapped under a key derived from the user's
/// password, JSON-serialized for storage as a single settings value under
/// `master_key`. `salt` and `nonce` aren't secret — both are required to
/// re-derive the wrapping key and decrypt, so they travel alongside the
/// ciphertext.
#[derive(Serialize, Deserialize)]
struct WrappedMasterKey {
    salt: Vec<u8>,
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

/// Derives a 256-bit key-wrapping key from `password` and `salt` using
/// Argon2id (OWASP's recommended memory-hard password KDF), the same
/// algorithm the server uses to hash passwords for authentication.
fn derive_wrapping_key(password: &str, salt: &[u8]) -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|err| Error::Encryption(format!("failed to derive key from password: {err}")))?;
    Ok(key)
}

/// Generates a fresh random master key and encrypts it under a key derived
/// from `password` (AES-256-GCM, random nonce), returning the result
/// JSON-serialized and ready to persist as a settings value.
pub(super) fn generate_and_wrap_master_key(password: &str) -> Result<String> {
    let master_key = generate_master_key();

    let mut salt = [0u8; SALT_LEN];
    rand::rng().fill_bytes(&mut salt);
    let wrapping_key =
        derive_wrapping_key(password, &salt).context("failed to derive master key wrapping key")?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);

    let cipher = Aes256Gcm::new(&wrapping_key.into());
    let ciphertext = cipher
        .encrypt(&nonce, master_key.as_slice())
        .map_err(|_| Error::Encryption("failed to encrypt master key".into()))?;

    let wrapped = WrappedMasterKey {
        salt: salt.to_vec(),
        nonce: nonce_bytes.to_vec(),
        ciphertext,
    };

    serde_json::to_string(&wrapped).context("failed to serialize wrapped master key")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decrypts a [`generate_and_wrap_master_key`] result back to the raw
    /// master key, for asserting on it in tests.
    fn unwrap_master_key(password: &str, wrapped_json: &str) -> Vec<u8> {
        let wrapped: WrappedMasterKey = serde_json::from_str(wrapped_json).unwrap();
        let wrapping_key = derive_wrapping_key(password, &wrapped.salt).unwrap();
        let nonce_bytes: [u8; NONCE_LEN] = wrapped.nonce.try_into().unwrap();
        let cipher = Aes256Gcm::new(&wrapping_key.into());
        cipher
            .decrypt(&Nonce::from(nonce_bytes), wrapped.ciphertext.as_slice())
            .unwrap()
    }

    #[test]
    fn generate_and_wrap_master_key_roundtrips_under_the_same_password() {
        let wrapped = generate_and_wrap_master_key("correct horse battery staple").unwrap();

        let master_key = unwrap_master_key("correct horse battery staple", &wrapped);

        assert_eq!(master_key.len(), KEY_LEN);
    }

    #[test]
    fn generate_and_wrap_master_key_produces_a_fresh_key_and_salt_each_call() {
        let first = generate_and_wrap_master_key("correct horse battery staple").unwrap();
        let second = generate_and_wrap_master_key("correct horse battery staple").unwrap();

        assert_ne!(first, second);
        let first_key = unwrap_master_key("correct horse battery staple", &first);
        let second_key = unwrap_master_key("correct horse battery staple", &second);
        assert_ne!(first_key, second_key);
    }

    #[test]
    fn wrapped_master_key_cannot_be_unwrapped_with_the_wrong_password() {
        let wrapped = generate_and_wrap_master_key("correct horse battery staple").unwrap();
        let wrapped_data: WrappedMasterKey = serde_json::from_str(&wrapped).unwrap();

        let wrapping_key = derive_wrapping_key("wrong password", &wrapped_data.salt).unwrap();
        let nonce_bytes: [u8; NONCE_LEN] = wrapped_data.nonce.try_into().unwrap();
        let cipher = Aes256Gcm::new(&wrapping_key.into());

        assert!(
            cipher
                .decrypt(
                    &Nonce::from(nonce_bytes),
                    wrapped_data.ciphertext.as_slice()
                )
                .is_err()
        );
    }
}
