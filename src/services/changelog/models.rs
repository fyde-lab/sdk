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

/// Builds a [`ChangelogEvent`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeChangelogEvent {
    event: ChangelogEvent,
}

#[cfg(test)]
impl FakeChangelogEvent {
    pub(crate) fn new() -> Self {
        Self {
            event: ChangelogEvent {
                offset: crate::testing::random_u64(10_000) as i64,
                event_type: EventType::Created,
                document_id: Uuid::new_v4(),
                content: Some(crate::testing::random_bytes(64)),
                metadata: Some(crate::services::documents::FakeMetadata::new().build()),
            },
        }
    }

    pub(crate) fn with_offset(mut self, offset: i64) -> Self {
        self.event.offset = offset;
        self
    }

    pub(crate) fn with_event_type(mut self, event_type: EventType) -> Self {
        self.event.event_type = event_type;
        self
    }

    /// Sets `document_id` to `document`'s id, so the event refers to a
    /// document that actually exists in the test's fixtures.
    pub(crate) fn for_document(mut self, document: &crate::services::documents::Document) -> Self {
        self.event.document_id = document.id;
        self
    }

    pub(crate) fn with_document_id(mut self, document_id: Uuid) -> Self {
        self.event.document_id = document_id;
        self
    }

    pub(crate) fn with_content(mut self, content: Vec<u8>) -> Self {
        self.event.content = Some(content);
        self
    }

    pub(crate) fn without_content(mut self) -> Self {
        self.event.content = None;
        self
    }

    pub(crate) fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.event.metadata = Some(metadata);
        self
    }

    pub(crate) fn without_metadata(mut self) -> Self {
        self.event.metadata = None;
        self
    }

    pub(crate) fn build(self) -> ChangelogEvent {
        self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::documents::FakeDocument;

    #[test]
    fn fake_changelog_event_builds_with_plausible_defaults() {
        let event = FakeChangelogEvent::new().build();

        assert_eq!(event.event_type, EventType::Created);
        assert!(event.content.is_some());
        assert!(event.metadata.is_some());
    }

    #[test]
    fn for_document_sets_the_document_id() {
        let document = FakeDocument::new().build();

        let event = FakeChangelogEvent::new().for_document(&document).build();

        assert_eq!(event.document_id, document.id);
    }

    #[test]
    fn with_content_overrides_the_default_content() {
        let event = FakeChangelogEvent::new()
            .with_content(vec![1, 2, 3])
            .build();

        assert_eq!(event.content, Some(vec![1, 2, 3]));
    }

    #[test]
    fn with_methods_override_every_other_field() {
        let document_id = Uuid::new_v4();
        let metadata = crate::services::documents::FakeMetadata::new().build();

        let event = FakeChangelogEvent::new()
            .with_offset(42)
            .with_event_type(EventType::Deleted)
            .with_document_id(document_id)
            .with_metadata(metadata.clone())
            .build();

        assert_eq!(event.offset, 42);
        assert_eq!(event.event_type, EventType::Deleted);
        assert_eq!(event.document_id, document_id);
        assert_eq!(event.metadata, Some(metadata));
    }

    #[test]
    fn without_content_and_without_metadata_clear_those_fields() {
        let event = FakeChangelogEvent::new()
            .without_content()
            .without_metadata()
            .build();

        assert_eq!(event.content, None);
        assert_eq!(event.metadata, None);
    }
}
