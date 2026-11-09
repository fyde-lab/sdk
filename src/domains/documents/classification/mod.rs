//! A sub-domain of `documents` guessing a document's [`Category`],
//! [`Subcategory`] and [`DocumentType`] from its text, on device: zero-shot,
//! by comparing the text's embedding with embedded descriptions of each
//! label. Ported from `doc-ai-poc`'s `classify.rs`.

mod models;
mod service;

pub(crate) use models::Classification;
#[cfg(test)]
pub(crate) use models::FakeClassification;

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use super::embedder::Embedder;
#[cfg(doc)]
use super::{Category, DocumentType, Subcategory};
use crate::Result;

/// Classifies documents from their text.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Guesses the classification of a document whose transcript is
    /// `text`, leaving each field it can't tell apart from another choice
    /// `None`. Loads the embedding model if needed (see
    /// [`Embedder::embed_passages`]).
    async fn classify(&self, text: &str) -> Result<Classification>;
}

/// Every label description's embedding, computed once per process and
/// model, whichever `Client` asks first.
static DESCRIPTIONS: LazyLock<Arc<service::DescriptionVectors>> =
    LazyLock::new(|| Arc::new(service::DescriptionVectors::default()));

/// Initializes the classification [`Service`], embedding with `embedder`.
pub(crate) fn init(embedder: Arc<dyn Embedder>) -> Arc<dyn Service> {
    Arc::new(service::Classifier::new(embedder, DESCRIPTIONS.clone()))
}
