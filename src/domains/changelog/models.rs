use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::domains::documents::{Attribute, DocumentType, Metadata};
use crate::domains::folders::{FolderAppearance, FolderContents};
use crate::domains::scripts::Script;

/// The kind of write recorded by a [`ChangelogEvent`]: one per action a
/// user (or a scraper run) takes. What each carries is its `Change`'s
/// variant of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// A folder was deleted (see `folders::Service::delete`), saying
    /// whether what it held was kept or deleted too.
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

/// What a [`ChangelogEvent`] records, encrypted in it: one variant per
/// [`EventType`], each carrying only the fields its action changes (the
/// event's `subject_id` says which document, folder or installation), so
/// applying it patches what's saved locally. MessagePack-encoded,
/// positionally: never reorder a variant's fields.
///
/// Its `Debug` output is only its event type: the fields carry a
/// document's bytes and transcript, names, and an installation's
/// parameters (credentials included).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum Change {
    /// The document's content and metadata.
    DocumentUploaded {
        #[serde(with = "serde_bytes")]
        content: Vec<u8>,
        metadata: Metadata,
    },
    DocumentRenamed {
        name: String,
    },
    /// Its type and every attribute it now has: one left out is removed.
    DocumentAttributesUpdated {
        r#type: Option<DocumentType>,
        attributes: BTreeMap<String, Attribute>,
    },
    /// `None` for À classer.
    DocumentMoved {
        folder_id: Option<Uuid>,
    },
    DocumentDeleted,
    FolderCreated {
        parent_id: Option<Uuid>,
        name: String,
        appearance: Option<FolderAppearance>,
    },
    FolderRenamed {
        name: String,
    },
    FolderAppearanceChanged {
        appearance: FolderAppearance,
    },
    /// `appearance` is the one it has once moved: none under another
    /// folder, at the top level the one it showed.
    FolderMoved {
        parent_id: Option<Uuid>,
        appearance: Option<FolderAppearance>,
    },
    FolderDeleted {
        contents: FolderContents,
    },
    ScriptInstalled {
        label: String,
        script: Script,
        parameters: HashMap<String, Value>,
        folder_id: Option<Uuid>,
    },
    InstallationCredentialsUpdated {
        parameters: HashMap<String, Value>,
    },
    InstallationRenamed {
        label: String,
    },
    /// `None` to let its documents be filed automatically.
    InstallationFolderChanged {
        folder_id: Option<Uuid>,
    },
    InstallationUpgraded {
        script: Script,
    },
    ScriptUninstalled,
}

impl Change {
    pub(crate) fn event_type(&self) -> EventType {
        match self {
            Self::DocumentUploaded { .. } => EventType::DocumentUploaded,
            Self::DocumentRenamed { .. } => EventType::DocumentRenamed,
            Self::DocumentAttributesUpdated { .. } => EventType::DocumentAttributesUpdated,
            Self::DocumentMoved { .. } => EventType::DocumentMoved,
            Self::DocumentDeleted => EventType::DocumentDeleted,
            Self::FolderCreated { .. } => EventType::FolderCreated,
            Self::FolderRenamed { .. } => EventType::FolderRenamed,
            Self::FolderAppearanceChanged { .. } => EventType::FolderAppearanceChanged,
            Self::FolderMoved { .. } => EventType::FolderMoved,
            Self::FolderDeleted { .. } => EventType::FolderDeleted,
            Self::ScriptInstalled { .. } => EventType::ScriptInstalled,
            Self::InstallationCredentialsUpdated { .. } => {
                EventType::InstallationCredentialsUpdated
            }
            Self::InstallationRenamed { .. } => EventType::InstallationRenamed,
            Self::InstallationFolderChanged { .. } => EventType::InstallationFolderChanged,
            Self::InstallationUpgraded { .. } => EventType::InstallationUpgraded,
            Self::ScriptUninstalled => EventType::ScriptUninstalled,
        }
    }
}

impl std::fmt::Debug for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} {{ [redacted] }}", self.event_type())
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
/// `Folder*` ones, a script installation for the others. Only a
/// `DocumentUploaded` event has a [`Self::content`] and [`Self::metadata`]:
/// every other one carries just what its action changed.
///
/// Its `Debug` output shows none of what the event carries (see
/// `Change`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChangelogEvent {
    pub(super) id: Uuid,
    pub(super) subject_id: Uuid,
    pub(super) change: Change,
}

impl ChangelogEvent {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn event_type(&self) -> EventType {
        self.change.event_type()
    }

    pub fn subject_id(&self) -> Uuid {
        self.subject_id
    }

    /// The uploaded document's content, for a `DocumentUploaded` event.
    pub fn content(&self) -> Option<&[u8]> {
        match &self.change {
            Change::DocumentUploaded { content, .. } => Some(content),
            _ => None,
        }
    }

    /// The uploaded document's metadata, for a `DocumentUploaded` event.
    pub fn metadata(&self) -> Option<&Metadata> {
        match &self.change {
            Change::DocumentUploaded { metadata, .. } => Some(metadata),
            _ => None,
        }
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
    /// A `DocumentUploaded` event.
    pub fn new() -> Self {
        Self {
            event: ChangelogEvent {
                id: Uuid::now_v7(),
                subject_id: Uuid::now_v7(),
                change: Change::DocumentUploaded {
                    content: crate::tools::testing::random_bytes(64),
                    metadata: crate::domains::documents::FakeMetadata::new().build(),
                },
            },
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.event.id = id;
        self
    }

    /// Makes it a `DocumentDeleted` event, which carries nothing.
    pub fn deleting(mut self) -> Self {
        self.event.change = Change::DocumentDeleted;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_change(mut self, change: Change) -> Self {
        self.event.change = change;
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

    /// Sets the uploaded document's content (making it a
    /// `DocumentUploaded` event if it wasn't).
    pub fn with_content(mut self, content: Vec<u8>) -> Self {
        let metadata = self.uploaded_metadata();
        self.event.change = Change::DocumentUploaded { content, metadata };
        self
    }

    /// Sets the uploaded document's metadata (making it a
    /// `DocumentUploaded` event if it wasn't).
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        let content = match &self.event.change {
            Change::DocumentUploaded { content, .. } => content.clone(),
            _ => crate::tools::testing::random_bytes(64),
        };
        self.event.change = Change::DocumentUploaded { content, metadata };
        self
    }

    fn uploaded_metadata(&self) -> Metadata {
        match &self.event.change {
            Change::DocumentUploaded { metadata, .. } => metadata.clone(),
            _ => crate::domains::documents::FakeMetadata::new().build(),
        }
    }

    pub fn build(self) -> ChangelogEvent {
        self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::{FakeDocument, FakeMetadata};
    use crate::domains::folders::{FolderColor, FolderIcon};
    use crate::domains::scripts::FakeScript;

    const APPEARANCE: FolderAppearance = FolderAppearance {
        color: FolderColor::Teal,
        icon: FolderIcon::Briefcase,
    };

    /// One change of each event type.
    fn every_change() -> Vec<Change> {
        let parameters = HashMap::from([("username".to_string(), Value::from("marie"))]);
        vec![
            Change::DocumentUploaded {
                content: b"%PDF".to_vec(),
                metadata: FakeMetadata::new().build(),
            },
            Change::DocumentRenamed {
                name: "Facture.pdf".to_string(),
            },
            Change::DocumentAttributesUpdated {
                r#type: Some(DocumentType::Invoice),
                attributes: BTreeMap::from([("amount".to_string(), Attribute::Integer(42))]),
            },
            Change::DocumentMoved {
                folder_id: Some(Uuid::now_v7()),
            },
            Change::DocumentDeleted,
            Change::FolderCreated {
                parent_id: None,
                name: "Travail".to_string(),
                appearance: Some(APPEARANCE),
            },
            Change::FolderRenamed {
                name: "Emploi".to_string(),
            },
            Change::FolderAppearanceChanged {
                appearance: APPEARANCE,
            },
            Change::FolderMoved {
                parent_id: Some(Uuid::now_v7()),
                appearance: None,
            },
            Change::FolderDeleted {
                contents: FolderContents::Keep,
            },
            Change::ScriptInstalled {
                label: "Marie".to_string(),
                script: FakeScript::new().build(),
                parameters: parameters.clone(),
                folder_id: None,
            },
            Change::InstallationCredentialsUpdated { parameters },
            Change::InstallationRenamed {
                label: "Compte pro".to_string(),
            },
            Change::InstallationFolderChanged {
                folder_id: Some(Uuid::now_v7()),
            },
            Change::InstallationUpgraded {
                script: FakeScript::new().build(),
            },
            Change::ScriptUninstalled,
        ]
    }

    #[test]
    fn every_change_roundtrips_through_messagepack() {
        for change in every_change() {
            let bytes = rmp_serde::to_vec(&change).unwrap();

            assert_eq!(rmp_serde::from_slice::<Change>(&bytes).unwrap(), change);
        }
    }

    #[test]
    fn every_event_type_has_its_own_change() {
        let event_types: Vec<EventType> = every_change().iter().map(Change::event_type).collect();

        assert_eq!(
            event_types,
            vec![
                EventType::DocumentUploaded,
                EventType::DocumentRenamed,
                EventType::DocumentAttributesUpdated,
                EventType::DocumentMoved,
                EventType::DocumentDeleted,
                EventType::FolderCreated,
                EventType::FolderRenamed,
                EventType::FolderAppearanceChanged,
                EventType::FolderMoved,
                EventType::FolderDeleted,
                EventType::ScriptInstalled,
                EventType::InstallationCredentialsUpdated,
                EventType::InstallationRenamed,
                EventType::InstallationFolderChanged,
                EventType::InstallationUpgraded,
                EventType::ScriptUninstalled,
            ]
        );
    }

    #[test]
    fn a_rename_carries_only_the_new_name() {
        let rename = Change::DocumentRenamed {
            name: "Facture.pdf".to_string(),
        };

        // The variant's name, then the one field.
        assert!(rmp_serde::to_vec(&rename).unwrap().len() < 40);
    }

    #[test]
    fn fake_changelog_event_builds_an_upload_by_default() {
        let event = FakeChangelogEvent::new().build();

        assert_eq!(event.event_type(), EventType::DocumentUploaded);
        assert!(event.content().is_some());
        assert!(event.metadata().is_some());
    }

    #[test]
    fn debug_never_shows_what_the_event_carries() {
        let event = FakeChangelogEvent::new()
            .with_change(Change::InstallationCredentialsUpdated {
                parameters: HashMap::from([("password".to_string(), Value::from("hunter2"))]),
            })
            .build();

        let debug = format!("{event:?}");

        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(!debug.contains("password"), "{debug}");
        assert!(debug.contains("InstallationCredentialsUpdated"), "{debug}");
        assert!(debug.contains(&event.subject_id.to_string()), "{debug}");
    }

    #[test]
    fn debug_never_shows_an_uploaded_documents_content_or_metadata() {
        let metadata = FakeMetadata::new().with_transcript("secret text").build();
        let event = FakeChangelogEvent::new()
            .with_content(b"secret bytes".to_vec())
            .with_metadata(metadata)
            .build();

        let debug = format!("{event:?}");

        assert!(!debug.contains("secret"), "{debug}");
    }

    #[test]
    fn for_document_sets_the_subject_id() {
        let document = FakeDocument::new().build();

        let event = FakeChangelogEvent::new().for_document(&document).build();

        assert_eq!(event.subject_id, document.id());
    }

    #[test]
    fn with_methods_override_every_field() {
        let id = Uuid::now_v7();
        let subject_id = Uuid::now_v7();
        let metadata = FakeMetadata::new().build();

        let event = FakeChangelogEvent::new()
            .with_id(id)
            .with_subject_id(subject_id)
            .with_content(vec![1, 2, 3])
            .with_metadata(metadata.clone())
            .build();

        assert_eq!(event.id(), id);
        assert_eq!(event.subject_id(), subject_id);
        assert_eq!(event.content(), Some(&[1, 2, 3][..]));
        assert_eq!(event.metadata(), Some(&metadata));
    }

    #[test]
    fn only_an_upload_has_content_and_metadata() {
        let event = FakeChangelogEvent::new().deleting().build();

        assert_eq!(event.event_type(), EventType::DocumentDeleted);
        assert_eq!(event.content(), None);
        assert_eq!(event.metadata(), None);
    }
}
