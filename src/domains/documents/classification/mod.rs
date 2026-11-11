//! A sub-domain of `documents` guessing a document's [`DocumentType`] and
//! folder from its text, on device: zero-shot, by comparing the text's
//! embedding with embedded descriptions of each type and of each described
//! folder. Ported from `doc-ai-poc`'s `classify.rs`.

mod models;
mod service;

pub(crate) use models::Classification;
#[cfg(test)]
pub(crate) use models::FakeClassification;

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

#[cfg(doc)]
use super::DocumentType;
use super::embedder::Embedder;
use crate::Result;
use crate::domains::folders::Folder;

/// Classifies documents from their text.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Guesses the type of a document whose transcript is `text`, and the
    /// one of `folders` it belongs in by their descriptions (a folder
    /// without one is never picked), leaving each field it can't tell apart
    /// from another choice `None`. Loads the embedding model if needed (see
    /// [`Embedder::embed_passages`]).
    async fn classify(&self, text: &str, folders: &[Folder]) -> Result<Classification>;
}

/// Every type description's embedding, computed once per process and
/// model, whichever `Client` asks first, and the folder texts embedded so
/// far.
static DESCRIPTIONS: LazyLock<Arc<service::DescriptionVectors>> =
    LazyLock::new(|| Arc::new(service::DescriptionVectors::default()));

/// Initializes the classification [`Service`], embedding with `embedder`.
pub(crate) fn init(embedder: Arc<dyn Embedder>) -> Arc<dyn Service> {
    Arc::new(service::Classifier::new(embedder, DESCRIPTIONS.clone()))
}
