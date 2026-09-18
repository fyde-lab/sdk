//! Foreign-function interface exposing the SDK to non-Rust callers (currently
//! the Kotlin Multiplatform `application`) via UniFFI-generated bindings.
//!
//! This is deliberately a thin wrapper: it only exposes enough to construct
//! and hold a [`crate::Client`] from Kotlin. Domain operations (documents,
//! changelog) are not exposed yet.

use std::sync::Arc;

use crate::{Client, Error};

/// Error type surfaced to FFI callers. UniFFI requires exported errors to be
/// their own type rather than the crate's own [`Error`], so every failure is
/// flattened to its display message here.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("{0}")]
    Failed(String),
}

impl From<Error> for FfiError {
    fn from(err: Error) -> Self {
        FfiError::Failed(err.to_string())
    }
}

/// A connection to a fyde server, exposed to Kotlin as `FydeClient`.
#[derive(uniffi::Object)]
pub struct FydeClient {
    // Not read yet: no domain operations (documents/changelog) are exposed
    // over FFI yet, only construction.
    #[allow(dead_code)]
    inner: Client,
}

#[uniffi::export]
impl FydeClient {
    /// Connects to a fyde server at `url`, backed by the default on-disk
    /// SQLite database. Mirrors [`Client::connect`].
    ///
    /// Needs `async_runtime = "tokio"`: `Client::connect` goes through
    /// sqlx's connection pool, which requires an active Tokio context to
    /// spawn its background tasks on — not just a generic future poller.
    #[uniffi::constructor(async_runtime = "tokio")]
    pub async fn connect(url: String) -> Result<Arc<Self>, FfiError> {
        let inner = Client::connect(url).await?;
        Ok(Arc::new(Self { inner }))
    }

    /// Connects to a fyde server backed by a private in-memory database.
    /// Mirrors [`Client::connect_memory`].
    #[uniffi::constructor(async_runtime = "tokio")]
    pub async fn connect_memory(url: String) -> Result<Arc<Self>, FfiError> {
        let inner = Client::connect_memory(url).await?;
        Ok(Arc::new(Self { inner }))
    }
}
