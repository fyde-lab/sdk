//! A sub-domain of `documents` reading attribute values out of a document's
//! text, on device (ported from `doc-ai-poc`'s `attributes.rs`): which
//! attributes to look for depends on the document's [`DocumentType`], each
//! is found by GLiNER (see [`SpanFinder`]) from a short French label, and
//! the span it finds is parsed into a typed [`Attribute`] (amounts in minor
//! units, ISO dates).

mod service;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use super::DocumentType;
use super::attributes::Attribute;
use super::gliner::SpanFinder;
use crate::Result;

/// Reads attribute values out of documents' text.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Service: Send + Sync {
    /// The attributes found in `transcript`, a document of type `r#type`
    /// (`None` when it couldn't be classified: then only what any document
    /// carries, an issuer, a date and an amount, is looked for). At most one
    /// value per attribute, its best match; nothing is returned for an
    /// attribute GLiNER finds no span for, or none that reads as its type.
    /// Values are valid on their own, but not checked against the user's
    /// attribute definitions. Fetches and loads the model first if needed;
    /// for [`EXTRACTION_RETRY_AFTER`] after that fails, finds nothing
    /// without trying again.
    async fn extract(
        &self,
        transcript: &str,
        r#type: Option<DocumentType>,
    ) -> Result<BTreeMap<String, Attribute>>;
}

/// How long extraction finds nothing after failing to run the model:
/// failing usually means waiting for its download (a timeout, say), which
/// every upload would otherwise wait for again.
pub(crate) const EXTRACTION_RETRY_AFTER: std::time::Duration =
    std::time::Duration::from_secs(5 * 60);

/// Initializes the extraction [`Service`], finding spans with
/// `span_finder`.
pub(crate) fn init(span_finder: Arc<dyn SpanFinder>) -> Arc<dyn Service> {
    Arc::new(service::Extractor::new(span_finder))
}
