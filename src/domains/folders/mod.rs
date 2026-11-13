#[cfg(test)]
mod layout;
mod models;
mod service;
mod storage;
mod storage_sqlite;

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use sqlx::SqlitePool;
use uuid::Uuid;

pub(crate) use models::DeleteFolderEvent;
#[cfg(any(test, feature = "testing"))]
pub use models::FakeFolder;
pub use models::{
    CreateFolderRequest, Folder, FolderAppearance, FolderColor, FolderContents, FolderIcon,
    MAX_FOLDER_NAME_CHARS, UpdateFolderRequest,
};

use crate::Result;
use crate::domains::changelog::Service as ChangelogService;

/// The vault's folders: each holds documents, subfolders, or both, nested
/// to any depth. Every change is checked against the folders synced to this
/// device so far, then published as an encrypted changelog event
/// (`SaveFolder`, `DeleteFolder`): like documents, it only shows up in
/// [`Service::list`] once the sync job consumes it back, on every device.
/// New documents are filed in them by what each already holds (see
/// [`crate::domains::documents::Service::upload`]), never by their names.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Every folder synced to this device, by name (case-insensitively).
    /// Never talks to the server.
    async fn list(&self) -> Result<Vec<Folder>>;

    /// Publishes a new folder and returns it. Its name is trimmed and must
    /// be 1 to [`MAX_FOLDER_NAME_CHARS`] characters, without control
    /// characters ([`crate::ErrorCode::InvalidFolderName`]), and free among
    /// its siblings, case-insensitively ([`crate::ErrorCode::FolderNameTaken`]).
    /// A top-level folder needs an appearance, a subfolder can't have one
    /// ([`crate::ErrorCode::InvalidFolderAppearance`]); the parent must
    /// exist ([`crate::ErrorCode::FolderNotFound`]).
    async fn create(&self, request: CreateFolderRequest) -> Result<Folder>;

    /// Renames or recolours a folder, with [`Self::create`]'s rules.
    async fn update(&self, request: UpdateFolderRequest) -> Result<()>;

    /// Moves a folder, with everything it holds, under `parent_id` (`None`
    /// for the top level). Fails with [`crate::ErrorCode::InvalidFolderMove`]
    /// into itself or one of its subfolders, and with
    /// [`crate::ErrorCode::FolderNameTaken`] if the target holds a folder
    /// with the same name. Under another folder it drops its appearance; at
    /// the top level it keeps the one it showed (its top-level folder's).
    /// Moving it where it already is publishes nothing.
    async fn move_folder(&self, id: Uuid, parent_id: Option<Uuid>) -> Result<()>;

    /// Deletes a folder, keeping what it holds (moved up to its parent) or
    /// deleting it too (see [`FolderContents`]).
    async fn delete(&self, id: Uuid, contents: FolderContents) -> Result<()>;
}

/// Materializes consumed folder events into local storage, for the
/// changelog's sync job. Resolves what concurrent changes from several
/// devices leave inconsistent the same way on every device, since they all
/// apply the same events in the same order: a folder whose parent is gone,
/// or whose move would close a cycle, lands at the top level; a name
/// clashing with a sibling's gets a `" (2)"` suffix (`" (3)"`...).
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Applier: Send + Sync {
    /// Saves `folder` as a `SaveFolder` event carries it.
    async fn apply_save(&self, folder: Folder) -> Result<()>;

    /// Deletes folder `id` as a `DeleteFolder` event says: moving its
    /// subfolders and documents up to its parent, or deleting every folder
    /// below it and every document in them, on this device only (every
    /// device applies the same event). A folder that isn't there is
    /// ignored.
    async fn apply_delete(&self, id: Uuid, contents: FolderContents) -> Result<()>;
}

/// The folders service and applier, over the local database `pool` the
/// sync job materializes folders into, publishing through `changelog`.
pub(crate) fn init(pool: SqlitePool, changelog: Arc<dyn ChangelogService>) -> Arc<dyn Service> {
    Arc::new(service::FoldersClient::new(
        storage_sqlite::SqliteStorage::new(pool),
        changelog,
    ))
}

/// The applier the changelog's sync job writes folders through, over the
/// local database `pool`. Publishes nothing, so it exists before the
/// changelog service it's given to.
pub(crate) fn init_applier(pool: SqlitePool) -> Arc<dyn Applier> {
    Arc::new(service::FolderApplier::new(
        storage_sqlite::SqliteStorage::new(pool.clone()),
        crate::domains::documents::SqliteStorage::new(pool),
    ))
}

/// A folders service holding no folder, for tests that don't file
/// documents.
#[cfg(test)]
pub(crate) fn no_folders() -> Arc<dyn Service> {
    folders_listing(Vec::new())
}

/// A folders service listing `folders`, and doing nothing else.
#[cfg(test)]
pub(crate) fn folders_listing(folders: Vec<Folder>) -> Arc<dyn Service> {
    let mut service = MockService::new();
    service.expect_list().returning(move || Ok(folders.clone()));
    Arc::new(service)
}

/// `layout.rs`'s sample layout in `language` as folders with fresh ids,
/// for tests filing documents in it.
#[cfg(test)]
pub(crate) fn proposed_layout_folders(language: &str) -> Vec<Folder> {
    fn add(
        proposed: &'static [layout::ProposedFolder],
        parent_id: Option<Uuid>,
        french: bool,
        into: &mut Vec<Folder>,
    ) {
        for folder in proposed {
            let name = if french { folder.fr } else { folder.en };
            let id = Uuid::now_v7();
            into.push(Folder {
                id,
                parent_id,
                name: name.to_string(),
                appearance: folder.appearance,
            });
            add(folder.children, Some(id), french, into);
        }
    }
    let mut folders = Vec::new();
    add(
        layout::PROPOSED_LAYOUT,
        None,
        crate::error::Language::parse(language) == crate::error::Language::French,
        &mut folders,
    );
    folders
}
