use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::documents::Metadata;

/// The kind of write recorded by a [`ChangelogEvent`]: one per action a
/// user (or a scraper run) takes. Variants are serialized by name inside
/// each encrypted event.
///
/// Every event carries the full state of its subject once the action is
/// done (not just what changed), so applying it replaces whatever was
/// there: a document's events carry its [`Metadata`] (and its content for
/// an upload), a folder's the JSON-serialized `folders::Folder`, an
/// installation's the JSON-serialized `scripts::InstalledScript` (no
/// `metadata`). `subject_id` is the document's, folder's or installation's
/// id. A service call that does several actions at once (renaming and
/// recolouring a folder, editing an installation's credentials and label)
/// publishes one event per action, in turn, each carrying the state after
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    /// A document was uploaded, by the user or a scraper run (see
    /// `documents::Service::upload`). Carries its content and metadata.
    DocumentUploaded,
    /// A document's name changed (see `documents::Service::update_metadata`).
    DocumentRenamed,
    /// A document's attributes changed (see
    /// `documents::Service::update_metadata`).
    DocumentAttributesUpdated,
    /// A document moved to another folder, or to À classer (see
    /// `documents::Service::move_document`).
    DocumentMoved,
    /// A document was deleted (see `documents::Service::delete`). Carries
    /// only the document's id as `subject_id`.
    DocumentDeleted,
    /// A folder was created (see `folders::Service::create`).
    FolderCreated,
    /// A folder's name changed (see `folders::Service::update`).
    FolderRenamed,
    /// A top-level folder's colour or icon changed (see
    /// `folders::Service::update`).
    FolderAppearanceChanged,
    /// A folder moved under another one, or to the top level (see
    /// `folders::Service::move_folder`).
    FolderMoved,
    /// A folder was deleted (see `folders::Service::delete`). Its `content`
    /// is the JSON-serialized `folders::DeleteFolderEvent`, saying whether
    /// what it held was kept or deleted too.
    FolderDeleted,
    /// A script was installed for the account: a new installation (see
    /// `scripts::Service::install_script`).
    ScriptInstalled,
    /// An installation's parameters (its credentials) changed.
    InstallationCredentialsUpdated,
    /// An installation's label changed.
    InstallationRenamed,
    /// An installation now files its documents into another folder (or
    /// lets them be filed automatically).
    InstallationFolderChanged,
    /// An installation moved to another version of its script.
    InstallationUpgraded,
    /// An installation was removed (see
    /// `scripts::Service::uninstall_script`). Carries only the
    /// installation's id as `subject_id`.
    ScriptUninstalled,
}

impl EventType {
    /// Whether this event caches or changes a document (and so needs the
    /// indexing model to be applied).
    pub(crate) fn writes_document(self) -> bool {
        matches!(
            self,
            Self::DocumentUploaded
                | Self::DocumentRenamed
                | Self::DocumentAttributesUpdated
                | Self::DocumentMoved
        )
    }
}

/// How far the running sync job has got through the changelog since it
/// started (at login, or at `Client::init` with a session already open):
/// what a new device shows while it replays the account's history.
/// Counts changelog events, not documents: the server can't tell a
/// document's event from a folder's or a script installation's, and a
/// document can have several.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncProgress {
    /// Events the job has gone through so far, applied or skipped as
    /// replays of one already applied.
    pub consumed: u64,
    /// How many events the job has to go through to catch up: those
    /// consumed before its latest pass plus those the server counted after
    /// that pass's cursor. `None` until the server has counted (or if it
    /// can't). Never below `consumed`: an event recorded after the count
    /// raises it.
    pub total: Option<u64>,
}

impl SyncProgress {
    /// Whether the job has gone through every event the server counted:
    /// whatever arrives from then on is the live tail.
    pub fn is_caught_up(&self) -> bool {
        self.total.is_some_and(|total| self.consumed >= total)
    }
}

/// A single recorded write, decrypted from the server's changelog.
/// `subject_id` is the id of whatever the write is about, which depends on
/// [`EventType`]: a document for the `Document*` events, a folder for the
/// `Folder*` ones, a script installation for the others. Inlines a document's fields rather than
/// wrapping a [`crate::domains::documents::Document`], since not every
/// event type carries all of them: `content` is `None` for a metadata-only
/// update, and both `content` and `metadata` are `None` for a deletion
/// (only `subject_id` is meaningful then).
///
/// Its `Debug` output shows only the content's size and whether metadata is
/// present: the content is a document's bytes or an installation's JSON
/// (its label and parameters' values, credentials included), and the
/// metadata carries the document's transcript.
#[derive(Clone, PartialEq)]
pub struct ChangelogEvent {
    pub(super) id: Uuid,
    pub(super) event_type: EventType,
    pub(super) subject_id: Uuid,
    pub(super) content: Option<Vec<u8>>,
    pub(super) metadata: Option<Metadata>,
}

impl std::fmt::Debug for ChangelogEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangelogEvent")
            .field("id", &self.id)
            .field("event_type", &self.event_type)
            .field("subject_id", &self.subject_id)
            .field(
                "content",
                &self
                    .content
                    .as_ref()
                    .map(|content| format!("[{} bytes redacted]", content.len())),
            )
            .field("metadata", &self.metadata.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl ChangelogEvent {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn event_type(&self) -> EventType {
        self.event_type
    }

    pub fn subject_id(&self) -> Uuid {
        self.subject_id
    }

    pub fn content(&self) -> Option<&[u8]> {
        self.content.as_deref()
    }

    pub fn metadata(&self) -> Option<&Metadata> {
        self.metadata.as_ref()
    }
}

/// Builds a [`ChangelogEvent`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(any(test, feature = "testing"))]
pub struct FakeChangelogEvent {
    event: ChangelogEvent,
}

#[cfg(any(test, feature = "testing"))]
impl Default for FakeChangelogEvent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl FakeChangelogEvent {
    pub fn new() -> Self {
        Self {
            event: ChangelogEvent {
                id: Uuid::now_v7(),
                event_type: EventType::DocumentUploaded,
                subject_id: Uuid::now_v7(),
                content: Some(crate::tools::testing::random_bytes(64)),
                metadata: Some(crate::domains::documents::FakeMetadata::new().build()),
            },
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.event.id = id;
        self
    }

    pub fn with_event_type(mut self, event_type: EventType) -> Self {
        self.event.event_type = event_type;
        self
    }

    /// Sets `subject_id` to `document`'s id, so the event refers to a
    /// document that actually exists in the test's fixtures.
    pub fn for_document(mut self, document: &crate::domains::documents::Document) -> Self {
        self.event.subject_id = document.id();
        self
    }

    pub fn with_subject_id(mut self, subject_id: Uuid) -> Self {
        self.event.subject_id = subject_id;
        self
    }

    pub fn with_content(mut self, content: Vec<u8>) -> Self {
        self.event.content = Some(content);
        self
    }

    pub fn without_content(mut self) -> Self {
        self.event.content = None;
        self
    }

    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.event.metadata = Some(metadata);
        self
    }

    pub fn without_metadata(mut self) -> Self {
        self.event.metadata = None;
        self
    }

    pub fn build(self) -> ChangelogEvent {
        self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::FakeDocument;

    #[test]
    fn fake_changelog_event_builds_with_plausible_defaults() {
        let event = FakeChangelogEvent::new().build();

        assert_eq!(event.event_type, EventType::DocumentUploaded);
        assert!(event.content.is_some());
        assert!(event.metadata.is_some());
    }

    #[test]
    fn debug_never_shows_the_content_or_metadata() {
        let event = FakeChangelogEvent::new()
            .with_event_type(EventType::ScriptInstalled)
            .with_content(br#"{"parameters":{"password":"hunter2"}}"#.to_vec())
            .build();
        let metadata = format!("{:?}", event.metadata);

        let debug = format!("{event:?}");

        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(!debug.contains(&format!("{:?}", event.content)), "{debug}");
        assert!(!debug.contains(&metadata), "{debug}");
        assert!(debug.contains("[37 bytes redacted]"), "{debug}");
        assert!(debug.contains(&event.subject_id.to_string()), "{debug}");
    }

    #[test]
    fn for_document_sets_the_subject_id() {
        let document = FakeDocument::new().build();

        let event = FakeChangelogEvent::new().for_document(&document).build();

        assert_eq!(event.subject_id, document.id());
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
        let id = Uuid::now_v7();
        let subject_id = Uuid::now_v7();
        let metadata = crate::domains::documents::FakeMetadata::new().build();

        let event = FakeChangelogEvent::new()
            .with_id(id)
            .with_event_type(EventType::DocumentDeleted)
            .with_subject_id(subject_id)
            .with_metadata(metadata.clone())
            .build();

        assert_eq!(event.id, id);
        assert_eq!(event.event_type, EventType::DocumentDeleted);
        assert_eq!(event.subject_id, subject_id);
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
