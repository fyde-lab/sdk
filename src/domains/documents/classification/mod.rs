//! A sub-domain of `documents` guessing a document's [`DocumentType`] and
//! folder, on device. The type is zero-shot, from its embedding (its chunks'
//! mean, as embedded at upload) compared with embedded descriptions of each
//! type (ported from
//! `doc-ai-poc`'s `classify.rs`). The folder is the one whose documents are
//! the closest to it, from how similar the caller found each document
//! already filed (see [`FiledDocument`]); a folder's name never counts.

mod models;
mod service;

#[cfg(test)]
pub(crate) use models::FakeClassification;
pub(crate) use models::{Classification, FiledDocument};

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

#[cfg(doc)]
use super::DocumentType;
use super::embedder::Embedder;
use crate::Result;

/// Classifies documents from their text.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Guesses the type of a document whose embedding (the mean of its
    /// chunks' passage embeddings, by `model`; normalized here) is
    /// `document`, and the folder it belongs in among those holding one of
    /// `filed` (the documents already filed in the folders it may go to,
    /// each with its similarity to this one), leaving each field it can't
    /// tell apart from another choice `None`. No `filed` guesses the type
    /// alone: a folder holding no document is never picked. Never embeds
    /// the document: only the type descriptions, once per model (see
    /// [`Embedder::embed_query`]), failing with
    /// [`crate::ErrorCode::ModelLoadFailed`] if `model` isn't the
    /// embedder's.
    async fn classify(
        &self,
        document: &[f32],
        model: &str,
        filed: &[FiledDocument],
    ) -> Result<Classification>;
}

/// Every type description's embedding, computed once per process and
/// model, whichever `Client` asks first.
static DESCRIPTIONS: LazyLock<Arc<service::DescriptionVectors>> =
    LazyLock::new(|| Arc::new(service::DescriptionVectors::default()));

/// Initializes the classification [`Service`], embedding with `embedder`.
pub(crate) fn init(embedder: Arc<dyn Embedder>) -> Arc<dyn Service> {
    Arc::new(service::Classifier::new(embedder, DESCRIPTIONS.clone()))
}
