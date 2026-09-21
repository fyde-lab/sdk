use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::services::documents::Metadata;

/// The kind of write recorded by a [`ChangelogEvent`]. Only [`Self::Created`]
/// is producible today (there is no update/delete flow yet), but the shape
/// is forward-compatible with the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    Created,
    UpdateMetadata,
    Deleted,
}

/// A single recorded write against a document, decrypted from the server's
/// changelog. Inlines the document's fields rather than wrapping a
/// [`crate::services::documents::Document`], since not every event type
/// carries all of them: `content` is `None` for a metadata-only update, and
/// both `content` and `metadata` are `None` for a deletion (only
/// `document_id` is meaningful then).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEvent {
    pub offset: i64,
    pub event_type: EventType,
    pub document_id: Uuid,
    pub content: Option<Vec<u8>>,
    pub metadata: Option<Metadata>,
}
