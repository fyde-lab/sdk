use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A folder of the vault: it holds documents, subfolders, or both. Folders
/// are the user's own (created, renamed, moved and deleted through
/// [`super::Service`]), synced across devices as encrypted changelog
/// events, so the server never sees their names. New documents are filed
/// by what the folders already hold (see
/// [`crate::domains::documents::Service::upload`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: Uuid,
    /// The folder holding this one, `None` for a top-level folder.
    pub parent_id: Option<Uuid>,
    /// 1 to [`MAX_FOLDER_NAME_CHARS`] characters, unique among its siblings
    /// (case-insensitively).
    pub name: String,
    /// Set on top-level folders only: a subfolder shows its top-level
    /// folder's.
    pub appearance: Option<FolderAppearance>,
}

/// The longest folder name, in characters.
pub const MAX_FOLDER_NAME_CHARS: usize = 60;

/// The colour and icon a top-level folder is shown with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FolderAppearance {
    pub color: FolderColor,
    pub icon: FolderIcon,
}

/// A top-level folder's colour, one of the design system's folder colours
/// (each a fill and an ink, defined by the application). Serialized by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderColor {
    Sand,
    Green,
    Teal,
    Brick,
    Rose,
    Blue,
    Indigo,
    Amber,
    Ochre,
    Cyan,
    Slate,
    Leaf,
    Rust,
    Plum,
    Violet,
    Moss,
}

/// A top-level folder's icon, one of the application's folder glyphs.
/// Serialized by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderIcon {
    Folder,
    Home,
    Wallet,
    Briefcase,
    Landmark,
    Heart,
    Phone,
    Train,
    Bag,
    Users,
    IdCard,
    Scale,
    GraduationCap,
    Paw,
    Ticket,
    Repeat,
    Gift,
    Car,
    Key,
    Calendar,
}

/// Creates a folder (see [`super::Service::create`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateFolderRequest {
    /// `None` to create a top-level folder.
    pub parent_id: Option<Uuid>,
    pub name: String,
    /// Required for a top-level folder, refused for a subfolder.
    pub appearance: Option<FolderAppearance>,
}

/// Changes a folder (see [`super::Service::update`]): each field left
/// `None` keeps its current value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateFolderRequest {
    pub id: Uuid,
    pub name: Option<String>,
    /// Only for a top-level folder.
    pub appearance: Option<FolderAppearance>,
}

/// What [`super::Service::delete`] does with what a folder holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FolderContents {
    /// Its subfolders and documents move up to its parent (to the root,
    /// À classer for documents, for a top-level folder).
    Keep,
    /// Its subfolders, and every document below it, are deleted too.
    Delete,
}

/// The content of a `DeleteFolder` changelog event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeleteFolderEvent {
    pub(crate) contents: FolderContents,
}

/// Builds a [`Folder`] with random-but-plausible data, overridable field by
/// field, for use in tests: a top-level folder unless given a parent.
#[cfg(any(test, feature = "testing"))]
pub struct FakeFolder {
    folder: Folder,
}

#[cfg(any(test, feature = "testing"))]
impl Default for FakeFolder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl FakeFolder {
    pub fn new() -> Self {
        Self {
            folder: Folder {
                id: Uuid::now_v7(),
                parent_id: None,
                name: format!(
                    "{} {}",
                    crate::tools::testing::random_word(),
                    crate::tools::testing::random_hex(4)
                ),
                appearance: Some(FolderAppearance {
                    color: FolderColor::Sand,
                    icon: FolderIcon::Home,
                }),
            },
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.folder.id = id;
        self
    }

    /// Makes it a subfolder of `parent_id`, without an appearance.
    pub fn with_parent(mut self, parent_id: Uuid) -> Self {
        self.folder.parent_id = Some(parent_id);
        self.folder.appearance = None;
        self
    }

    pub fn with_name(mut self, name: &str) -> Self {
        self.folder.name = name.to_string();
        self
    }

    pub fn with_appearance(mut self, appearance: Option<FolderAppearance>) -> Self {
        self.folder.appearance = appearance;
        self
    }

    pub fn build(self) -> Folder {
        self.folder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_saved_with_a_description_still_deserializes() {
        // How `SaveFolder` events carried a folder while folders had a
        // description: those events stay in the changelog.
        let id = Uuid::now_v7();
        let parent_id = Uuid::now_v7();
        let json = format!(
            r#"{{"id":"{id}","parent_id":"{parent_id}","name":"Énergie","description":"Factures d'électricité","appearance":null}}"#
        );

        let folder: Folder = serde_json::from_str(&json).unwrap();

        assert_eq!(
            folder,
            Folder {
                id,
                parent_id: Some(parent_id),
                name: "Énergie".to_string(),
                appearance: None,
            }
        );
    }
}
