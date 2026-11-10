//! A sub-domain of `documents` guessing a document's [`Category`],
//! [`Subcategory`] and [`DocumentType`] from its text, on device: zero-shot,
//! by asking GLiNER (see `super::gliner`) for the spans of the text matching
//! short cues phrased like what such documents contain ("consommation
//! d'électricité", "numéro de facture"), each label scoring its best span.

mod models;
mod service;

pub(crate) use models::Classification;
#[cfg(test)]
pub(crate) use models::FakeClassification;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use super::gliner::SpanFinder;
#[cfg(doc)]
use super::{Category, DocumentType, Subcategory};
use crate::Result;

/// Classifies documents from their text.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// Guesses the classification of a document whose transcript is
    /// `text`, leaving each field it can't tell apart from another choice
    /// (or finds no cue for) `None`. Fetches and loads the GLiNER model if
    /// needed (see [`SpanFinder::spans`]), unless `text` is blank.
    async fn classify(&self, text: &str) -> Result<Classification>;
}

/// Initializes the classification [`Service`], finding cues with `spans`.
pub(crate) fn init(spans: Arc<dyn SpanFinder>) -> Arc<dyn Service> {
    Arc::new(service::Classifier::new(spans))
}
