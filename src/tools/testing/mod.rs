//! Test-only primitives for building random-but-plausible fixture data,
//! used by the `Fake*` builders in each domain's `models.rs`. Draws
//! randomness from repeated [`Uuid::new_v4`] (backed by the OS's CSPRNG)
//! rather than `rand` (even though it's already a regular dependency), to
//! keep every `Fake*` builder's randomness source consistent with
//! `../server`'s.

use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

const WORDS: &[&str] = &[
    "quarterly",
    "invoice",
    "roadmap",
    "proposal",
    "budget",
    "summary",
    "agenda",
    "memo",
    "brief",
    "checklist",
];

/// `len` random bytes from the OS's CSPRNG.
pub(crate) fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    bytes.truncate(len);
    bytes
}

/// A random lowercase hex string encoding `len` random bytes.
pub(crate) fn random_hex(len: usize) -> String {
    random_bytes(len)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A random non-negative integer in `0..max`.
pub(crate) fn random_u64(max: u64) -> u64 {
    (Uuid::new_v4().as_u128() % u128::from(max)) as u64
}

fn pick(words: &'static [&'static str]) -> &'static str {
    words[random_u64(words.len() as u64) as usize]
}

pub(crate) fn random_word() -> &'static str {
    pick(WORDS)
}

/// A random Unix timestamp within the last 30 days, so fixtures never
/// carry a future `created_at`.
pub(crate) fn random_past_timestamp() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64;
    now - random_u64(30 * 24 * 60 * 60) as i64
}
