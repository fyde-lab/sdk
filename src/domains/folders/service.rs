use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::domains::changelog::{EventType, Service as ChangelogService};
use crate::domains::documents::Storage as DocumentStorage;
use crate::{ErrorContext as _, ErrorKind, Result};

use super::storage::Storage;
use super::{
    Applier, CreateFolderRequest, DeleteFolderEvent, Folder, FolderAppearance, FolderColor,
    FolderContents, FolderIcon, MAX_FOLDER_NAME_CHARS, Service, UpdateFolderRequest,
};

/// The appearance a folder gets when it ends up at the top level without
/// one of its own to keep.
const DEFAULT_APPEARANCE: FolderAppearance = FolderAppearance {
    color: FolderColor::Sand,
    icon: FolderIcon::Folder,
};

/// Implements [`Service`]: validates each change against the folders synced
/// so far in `storage`, then publishes it as a changelog event.
pub(super) struct FoldersClient<S: Storage> {
    storage: S,
    changelog: Arc<dyn ChangelogService>,
}

impl<S: Storage> FoldersClient<S> {
    pub(super) fn new(storage: S, changelog: Arc<dyn ChangelogService>) -> Self {
        Self { storage, changelog }
    }

    async fn folders(&self) -> Result<Vec<Folder>> {
        self.storage
            .list_folders()
            .await
            .context("failed to list local folders")
    }

    async fn publish(&self, folder: &Folder) -> Result<()> {
        let content = serde_json::to_vec(folder)
            .with_context(|| format!("failed to serialize folder {}", folder.id))?;
        self.changelog
            .send(EventType::SaveFolder, folder.id, Some(&content), None)
            .await
            .with_context(|| format!("failed to publish folder {}", folder.id))
    }
}

#[async_trait]
impl<S: Storage> Service for FoldersClient<S> {
    async fn list(&self) -> Result<Vec<Folder>> {
        let mut folders = self.folders().await?;
        folders.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(folders)
    }

    async fn create(&self, request: CreateFolderRequest) -> Result<Folder> {
        let name = valid_name(&request.name)?;
        let folders = self.folders().await?;
        if let Some(parent_id) = request.parent_id {
            find(&folders, parent_id)?;
        }
        check_appearance(request.parent_id, request.appearance)?;
        check_name_free(&folders, request.parent_id, &name, None)?;

        let folder = Folder {
            id: Uuid::now_v7(),
            parent_id: request.parent_id,
            name,
            appearance: request.appearance,
        };
        self.publish(&folder).await?;
        Ok(folder)
    }

    async fn update(&self, request: UpdateFolderRequest) -> Result<()> {
        let folders = self.folders().await?;
        let mut folder = find(&folders, request.id)?.clone();

        if let Some(name) = &request.name {
            let name = valid_name(name)?;
            check_name_free(&folders, folder.parent_id, &name, Some(folder.id))?;
            folder.name = name;
        }
        if let Some(appearance) = request.appearance {
            check_appearance(folder.parent_id, Some(appearance))?;
            folder.appearance = Some(appearance);
        }

        self.publish(&folder).await
    }

    async fn move_folder(&self, id: Uuid, parent_id: Option<Uuid>) -> Result<()> {
        let folders = self.folders().await?;
        let mut folder = find(&folders, id)?.clone();
        if folder.parent_id == parent_id {
            return Ok(());
        }
        if let Some(parent_id) = parent_id {
            find(&folders, parent_id)?;
            if parent_id == id || is_descendant(&folders, parent_id, id) {
                return Err(ErrorKind::InvalidFolderMove(format!(
                    "folder {id} can't move into itself or one of its subfolders"
                ))
                .into());
            }
        }
        check_name_free(&folders, parent_id, &folder.name, Some(id))?;

        folder.appearance = match parent_id {
            Some(_) => None,
            None => Some(top_level_appearance(&folders, &folder)),
        };
        folder.parent_id = parent_id;
        self.publish(&folder).await
    }

    async fn delete(&self, id: Uuid, contents: FolderContents) -> Result<()> {
        let folders = self.folders().await?;
        find(&folders, id)?;

        let content = serde_json::to_vec(&DeleteFolderEvent { contents })
            .with_context(|| format!("failed to serialize deletion of folder {id}"))?;
        self.changelog
            .send(EventType::DeleteFolder, id, Some(&content), None)
            .await
            .with_context(|| format!("failed to publish deletion of folder {id}"))
    }
}

/// Implements [`Applier`]: materializes consumed folder events into
/// `storage`, and their consequences for the documents they held into
/// `documents`.
pub(super) struct FolderApplier<S: Storage, D: DocumentStorage> {
    storage: S,
    documents: D,
}

impl<S: Storage, D: DocumentStorage> FolderApplier<S, D> {
    pub(super) fn new(storage: S, documents: D) -> Self {
        Self { storage, documents }
    }

    async fn folders(&self) -> Result<Vec<Folder>> {
        self.storage
            .list_folders()
            .await
            .context("failed to list local folders")
    }
}

#[async_trait]
impl<S: Storage, D: DocumentStorage> Applier for FolderApplier<S, D> {
    async fn apply_save(&self, folder: Folder) -> Result<()> {
        let folders = self.folders().await?;
        let mut folder = folder;

        // Concurrent changes from other devices may have deleted the parent
        // or made this move a cycle: the folder then lands at the top level.
        if let Some(parent_id) = folder.parent_id
            && (parent_id == folder.id
                || !folders.iter().any(|other| other.id == parent_id)
                || is_descendant(&folders, parent_id, folder.id))
        {
            folder.parent_id = None;
        }
        folder.appearance = match folder.parent_id {
            Some(_) => None,
            None => Some(folder.appearance.unwrap_or(DEFAULT_APPEARANCE)),
        };
        folder.name = unique_name(&folders, folder.parent_id, &folder.name, folder.id);

        self.storage
            .apply(vec![folder.clone()], Vec::new())
            .await
            .with_context(|| format!("failed to save folder {} locally", folder.id))
    }

    async fn apply_delete(&self, id: Uuid, contents: FolderContents) -> Result<()> {
        let folders = self.folders().await?;
        let Some(folder) = folders.iter().find(|folder| folder.id == id) else {
            return Ok(());
        };

        let (save, delete) = match contents {
            FolderContents::Delete => {
                let mut delete = descendants(&folders, id);
                delete.push(id);
                self.documents
                    .delete_folder_documents(&delete)
                    .await
                    .with_context(|| format!("failed to delete the documents of folder {id}"))?;
                (Vec::new(), delete)
            }
            FolderContents::Keep => {
                self.documents
                    .move_folder_documents(&[id], folder.parent_id)
                    .await
                    .with_context(|| format!("failed to move the documents of folder {id} up"))?;

                // Name clashes are checked against the folders that stay,
                // in order, so two children can't take the same new name.
                let mut remaining: Vec<Folder> = folders
                    .iter()
                    .filter(|other| other.id != id)
                    .cloned()
                    .collect();
                let appearance = top_level_appearance(&folders, folder);
                let mut save = Vec::new();
                let mut children: Vec<Folder> = folders
                    .iter()
                    .filter(|child| child.parent_id == Some(id))
                    .cloned()
                    .collect();
                children.sort_by_key(|child| child.id);
                for mut child in children {
                    child.parent_id = folder.parent_id;
                    child.appearance = folder.parent_id.is_none().then_some(appearance);
                    child.name = unique_name(&remaining, child.parent_id, &child.name, child.id);
                    if let Some(slot) = remaining.iter_mut().find(|other| other.id == child.id) {
                        *slot = child.clone();
                    }
                    save.push(child);
                }
                (save, vec![id])
            }
        };

        self.storage
            .apply(save, delete)
            .await
            .with_context(|| format!("failed to delete folder {id} locally"))
    }
}

fn find(folders: &[Folder], id: Uuid) -> Result<&Folder> {
    folders
        .iter()
        .find(|folder| folder.id == id)
        .ok_or_else(|| ErrorKind::FolderNotFound(id).into())
}

/// `name` trimmed, if it's 1 to [`MAX_FOLDER_NAME_CHARS`] characters with no
/// control character.
fn valid_name(name: &str) -> Result<String> {
    let name = name.trim();
    let length = name.chars().count();
    if length == 0 || length > MAX_FOLDER_NAME_CHARS {
        return Err(ErrorKind::InvalidFolderName(format!(
            "a folder name must be 1 to {MAX_FOLDER_NAME_CHARS} characters, got {length}"
        ))
        .into());
    }
    if name.chars().any(char::is_control) {
        return Err(ErrorKind::InvalidFolderName(
            "a folder name can't contain control characters".to_string(),
        )
        .into());
    }
    Ok(name.to_string())
}

/// A top-level folder needs an appearance; a subfolder takes its top-level
/// folder's, so can't have one.
fn check_appearance(parent_id: Option<Uuid>, appearance: Option<FolderAppearance>) -> Result<()> {
    match (parent_id, appearance) {
        (None, None) => Err(ErrorKind::InvalidFolderAppearance(
            "a top-level folder needs a colour and an icon".to_string(),
        )
        .into()),
        (Some(_), Some(_)) => Err(ErrorKind::InvalidFolderAppearance(
            "a subfolder takes its top-level folder's colour and icon".to_string(),
        )
        .into()),
        _ => Ok(()),
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Fails if another folder than `except` under `parent_id` is already
/// named `name` (case-insensitively).
fn check_name_free(
    folders: &[Folder],
    parent_id: Option<Uuid>,
    name: &str,
    except: Option<Uuid>,
) -> Result<()> {
    let taken = folders.iter().any(|folder| {
        folder.parent_id == parent_id && Some(folder.id) != except && same_name(&folder.name, name)
    });
    if taken {
        return Err(ErrorKind::FolderNameTaken(name.to_string()).into());
    }
    Ok(())
}

/// `name`, or, if a sibling under `parent_id` other than `id` already has
/// it, `"name (2)"`, `"name (3)"`... whichever is free first.
fn unique_name(folders: &[Folder], parent_id: Option<Uuid>, name: &str, id: Uuid) -> String {
    let taken: HashSet<String> = folders
        .iter()
        .filter(|folder| folder.parent_id == parent_id && folder.id != id)
        .map(|folder| folder.name.to_lowercase())
        .collect();
    if !taken.contains(&name.to_lowercase()) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} ({n})"))
        .find(|candidate| !taken.contains(&candidate.to_lowercase()))
        .expect("an unbounded range always yields a free name")
}

/// Whether `id` sits somewhere below `ancestor`.
fn is_descendant(folders: &[Folder], id: Uuid, ancestor: Uuid) -> bool {
    let parents: HashMap<Uuid, Option<Uuid>> = folders
        .iter()
        .map(|folder| (folder.id, folder.parent_id))
        .collect();
    let mut seen = HashSet::new();
    let mut current = parents.get(&id).copied().flatten();
    while let Some(parent) = current {
        if parent == ancestor {
            return true;
        }
        if !seen.insert(parent) {
            return false;
        }
        current = parents.get(&parent).copied().flatten();
    }
    false
}

/// Every folder below `id`, at any depth.
fn descendants(folders: &[Folder], id: Uuid) -> Vec<Uuid> {
    folders
        .iter()
        .filter(|folder| is_descendant(folders, folder.id, id))
        .map(|folder| folder.id)
        .collect()
}

/// The appearance `folder` shows: its own, else its top-level ancestor's,
/// else the default.
fn top_level_appearance(folders: &[Folder], folder: &Folder) -> FolderAppearance {
    let mut current = Some(folder);
    let mut seen = HashSet::new();
    while let Some(folder) = current {
        if let Some(appearance) = folder.appearance {
            return appearance;
        }
        if !seen.insert(folder.id) {
            break;
        }
        current = folder
            .parent_id
            .and_then(|parent_id| folders.iter().find(|other| other.id == parent_id));
    }
    DEFAULT_APPEARANCE
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mockall::predicate::eq;

    use super::*;
    use crate::ErrorCode;
    use crate::domains::changelog::ChangelogEvent;
    use crate::domains::documents::Metadata;
    use crate::domains::documents::MockStorage as MockDocumentStorage;
    use crate::domains::folders::FakeFolder;
    use crate::domains::folders::layout::{PROPOSED_LAYOUT, ProposedFolder};
    use crate::domains::folders::storage::MockStorage;

    /// An event a [`RecordingChangelog`] was sent:
    /// `(event_type, subject_id, content)`.
    type SentEvent = (EventType, Uuid, Option<Vec<u8>>);

    /// A changelog fake recording every sent event.
    #[derive(Default)]
    struct RecordingChangelog {
        sent: Mutex<Vec<SentEvent>>,
    }

    #[async_trait]
    impl ChangelogService for RecordingChangelog {
        async fn ensure_master_key(&self) -> Result<()> {
            Ok(())
        }

        async fn send(
            &self,
            event_type: EventType,
            subject_id: Uuid,
            content: Option<&[u8]>,
            _metadata: Option<&Metadata>,
        ) -> Result<()> {
            self.sent
                .lock()
                .unwrap()
                .push((event_type, subject_id, content.map(<[u8]>::to_vec)));
            Ok(())
        }

        async fn start_consume_job(
            self: Arc<Self>,
            _callback: Box<dyn FnMut(ChangelogEvent) + Send>,
        ) -> Result<()> {
            Ok(())
        }

        fn stop_consume_job(&self) {}
    }

    fn storage_with(folders: Vec<Folder>) -> MockStorage {
        let mut storage = MockStorage::new();
        storage
            .expect_list_folders()
            .returning(move || Ok(folders.clone()));
        storage
    }

    fn client(storage: MockStorage) -> (FoldersClient<MockStorage>, Arc<RecordingChangelog>) {
        let changelog = Arc::new(RecordingChangelog::default());
        (FoldersClient::new(storage, changelog.clone()), changelog)
    }

    fn published_folder(changelog: &RecordingChangelog) -> Folder {
        let sent = changelog.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (event_type, subject_id, content) = &sent[0];
        assert_eq!(*event_type, EventType::SaveFolder);
        let folder: Folder = serde_json::from_slice(content.as_deref().unwrap()).unwrap();
        assert_eq!(*subject_id, folder.id);
        folder
    }

    const APPEARANCE: FolderAppearance = FolderAppearance {
        color: FolderColor::Teal,
        icon: FolderIcon::Briefcase,
    };

    fn top_level_request(name: &str) -> CreateFolderRequest {
        CreateFolderRequest {
            parent_id: None,
            name: name.to_string(),
            appearance: Some(APPEARANCE),
        }
    }

    #[tokio::test]
    async fn list_sorts_folders_by_name_case_insensitively() {
        let b = FakeFolder::new().with_name("banque").build();
        let a = FakeFolder::new().with_name("Achats").build();
        let c = FakeFolder::new().with_name("Courses").build();
        let (client, _) = client(storage_with(vec![b.clone(), c.clone(), a.clone()]));

        assert_eq!(client.list().await.unwrap(), vec![a, b, c]);
    }

    #[tokio::test]
    async fn create_publishes_a_trimmed_top_level_folder() {
        let (client, changelog) = client(storage_with(Vec::new()));

        let created = client
            .create(CreateFolderRequest {
                name: "  Travail ".to_string(),
                ..top_level_request("")
            })
            .await
            .unwrap();

        let published = published_folder(&changelog);
        assert_eq!(published, created);
        assert_eq!(published.name, "Travail");
        assert_eq!(published.parent_id, None);
        assert_eq!(published.appearance, Some(APPEARANCE));
    }

    #[tokio::test]
    async fn create_publishes_a_subfolder_of_an_existing_folder() {
        let parent = FakeFolder::new().build();
        let (client, changelog) = client(storage_with(vec![parent.clone()]));

        client
            .create(CreateFolderRequest {
                parent_id: Some(parent.id),
                name: "Bulletins de paie".to_string(),
                appearance: None,
            })
            .await
            .unwrap();

        assert_eq!(published_folder(&changelog).parent_id, Some(parent.id));
    }

    #[tokio::test]
    async fn create_rejects_an_invalid_name() {
        let (client, changelog) = client(storage_with(Vec::new()));

        for name in ["", "   ", &"a".repeat(MAX_FOLDER_NAME_CHARS + 1), "a\nb"] {
            let err = client.create(top_level_request(name)).await.unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidFolderName, "{name:?}");
        }
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_accepts_a_name_of_the_maximum_length_in_characters() {
        let (client, _) = client(storage_with(Vec::new()));

        client
            .create(top_level_request(&"é".repeat(MAX_FOLDER_NAME_CHARS)))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_rejects_a_name_a_sibling_already_has_whatever_its_case() {
        let existing = FakeFolder::new().with_name("Travail").build();
        let (client, _) = client(storage_with(vec![existing]));

        let err = client
            .create(top_level_request("TRAVAIL"))
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::FolderNameTaken);
    }

    #[tokio::test]
    async fn create_allows_a_name_used_elsewhere_in_the_tree() {
        let parent = FakeFolder::new().build();
        let nested = FakeFolder::new()
            .with_parent(parent.id)
            .with_name("Travail")
            .build();
        let (client, _) = client(storage_with(vec![parent, nested]));

        client.create(top_level_request("Travail")).await.unwrap();
    }

    #[tokio::test]
    async fn create_rejects_an_unknown_parent() {
        let (client, _) = client(storage_with(Vec::new()));

        let err = client
            .create(CreateFolderRequest {
                parent_id: Some(Uuid::now_v7()),
                name: "Eau".to_string(),
                appearance: None,
            })
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::FolderNotFound);
    }

    #[tokio::test]
    async fn create_requires_an_appearance_at_the_top_level_only() {
        let parent = FakeFolder::new().build();
        let (client, _) = client(storage_with(vec![parent.clone()]));

        let missing = client
            .create(CreateFolderRequest {
                appearance: None,
                ..top_level_request("Travail")
            })
            .await
            .unwrap_err();
        let refused = client
            .create(CreateFolderRequest {
                parent_id: Some(parent.id),
                ..top_level_request("Eau")
            })
            .await
            .unwrap_err();

        assert_eq!(missing.code(), ErrorCode::InvalidFolderAppearance);
        assert_eq!(refused.code(), ErrorCode::InvalidFolderAppearance);
    }

    #[tokio::test]
    async fn update_publishes_only_the_changed_fields() {
        let folder = FakeFolder::new().with_name("Travail").build();
        let (client, changelog) = client(storage_with(vec![folder.clone()]));

        client
            .update(UpdateFolderRequest {
                id: folder.id,
                name: Some("Emploi".to_string()),
                ..UpdateFolderRequest::default()
            })
            .await
            .unwrap();

        let published = published_folder(&changelog);
        assert_eq!(
            published,
            Folder {
                name: "Emploi".to_string(),
                ..folder
            }
        );
    }

    #[tokio::test]
    async fn update_lets_a_folder_change_the_case_of_its_own_name() {
        let folder = FakeFolder::new().with_name("travail").build();
        let (client, changelog) = client(storage_with(vec![folder.clone()]));

        client
            .update(UpdateFolderRequest {
                id: folder.id,
                name: Some("Travail".to_string()),
                ..UpdateFolderRequest::default()
            })
            .await
            .unwrap();

        assert_eq!(published_folder(&changelog).name, "Travail");
    }

    #[tokio::test]
    async fn update_rejects_an_appearance_on_a_subfolder() {
        let parent = FakeFolder::new().build();
        let child = FakeFolder::new().with_parent(parent.id).build();
        let (client, _) = client(storage_with(vec![parent, child.clone()]));

        let err = client
            .update(UpdateFolderRequest {
                id: child.id,
                appearance: Some(APPEARANCE),
                ..UpdateFolderRequest::default()
            })
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::InvalidFolderAppearance);
    }

    #[tokio::test]
    async fn update_rejects_an_unknown_folder() {
        let (client, _) = client(storage_with(Vec::new()));

        let err = client
            .update(UpdateFolderRequest {
                id: Uuid::now_v7(),
                ..UpdateFolderRequest::default()
            })
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::FolderNotFound);
    }

    #[tokio::test]
    async fn move_folder_under_another_drops_its_appearance() {
        let target = FakeFolder::new().build();
        let moved = FakeFolder::new().build();
        let (client, changelog) = client(storage_with(vec![target.clone(), moved.clone()]));

        client.move_folder(moved.id, Some(target.id)).await.unwrap();

        let published = published_folder(&changelog);
        assert_eq!(published.parent_id, Some(target.id));
        assert_eq!(published.appearance, None);
    }

    #[tokio::test]
    async fn move_folder_to_the_root_keeps_the_appearance_it_showed() {
        let top = FakeFolder::new().with_appearance(Some(APPEARANCE)).build();
        let middle = FakeFolder::new().with_parent(top.id).build();
        let moved = FakeFolder::new().with_parent(middle.id).build();
        let (client, changelog) = client(storage_with(vec![top, middle, moved.clone()]));

        client.move_folder(moved.id, None).await.unwrap();

        let published = published_folder(&changelog);
        assert_eq!(published.parent_id, None);
        assert_eq!(published.appearance, Some(APPEARANCE));
    }

    #[tokio::test]
    async fn move_folder_rejects_moving_into_itself_or_a_descendant() {
        let folder = FakeFolder::new().build();
        let child = FakeFolder::new().with_parent(folder.id).build();
        let grandchild = FakeFolder::new().with_parent(child.id).build();
        let (client, changelog) = client(storage_with(vec![
            folder.clone(),
            child,
            grandchild.clone(),
        ]));

        for target in [folder.id, grandchild.id] {
            let err = client
                .move_folder(folder.id, Some(target))
                .await
                .unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidFolderMove);
        }
        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn move_folder_rejects_a_target_holding_a_folder_with_the_same_name() {
        let target = FakeFolder::new().build();
        let clash = FakeFolder::new()
            .with_parent(target.id)
            .with_name("Internet")
            .build();
        let moved = FakeFolder::new().with_name("internet").build();
        let (client, _) = client(storage_with(vec![target.clone(), clash, moved.clone()]));

        let err = client
            .move_folder(moved.id, Some(target.id))
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::FolderNameTaken);
    }

    #[tokio::test]
    async fn move_folder_to_its_current_parent_publishes_nothing() {
        let folder = FakeFolder::new().build();
        let (client, changelog) = client(storage_with(vec![folder.clone()]));

        client.move_folder(folder.id, None).await.unwrap();

        assert!(changelog.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_publishes_what_to_do_with_the_contents() {
        let folder = FakeFolder::new().build();
        let (client, changelog) = client(storage_with(vec![folder.clone()]));

        client
            .delete(folder.id, FolderContents::Delete)
            .await
            .unwrap();

        let sent = changelog.sent.lock().unwrap();
        let (event_type, subject_id, content) = &sent[0];
        assert_eq!(*event_type, EventType::DeleteFolder);
        assert_eq!(*subject_id, folder.id);
        let event: DeleteFolderEvent = serde_json::from_slice(content.as_deref().unwrap()).unwrap();
        assert_eq!(event.contents, FolderContents::Delete);
    }

    /// Every folder of the proposed layout, depth first.
    fn proposed_folders() -> Vec<&'static ProposedFolder> {
        fn walk(folders: &'static [ProposedFolder], into: &mut Vec<&'static ProposedFolder>) {
            for folder in folders {
                into.push(folder);
                walk(folder.children, into);
            }
        }
        let mut all = Vec::new();
        walk(PROPOSED_LAYOUT, &mut all);
        all
    }

    #[test]
    fn the_proposed_layout_passes_the_rules_a_user_s_folders_do() {
        for proposed in proposed_folders() {
            for name in [proposed.fr, proposed.en] {
                assert_eq!(valid_name(name).unwrap(), name);
            }
        }
        for top in PROPOSED_LAYOUT {
            check_appearance(None, top.appearance).unwrap();
            for child in top.children {
                check_appearance(Some(Uuid::nil()), child.appearance).unwrap();
                assert!(
                    child.children.is_empty(),
                    "{}: two levels at most",
                    child.fr
                );
            }
        }
    }

    #[test]
    fn the_proposed_layout_has_no_sibling_names_clashing() {
        let check = |folders: &[ProposedFolder]| {
            for pick in [|f: &ProposedFolder| f.fr, |f: &ProposedFolder| f.en] {
                let names: HashSet<String> = folders
                    .iter()
                    .map(|folder| pick(folder).to_lowercase())
                    .collect();
                assert_eq!(names.len(), folders.len());
            }
        };
        check(PROPOSED_LAYOUT);
        for top in PROPOSED_LAYOUT {
            check(top.children);
        }
    }

    #[tokio::test]
    async fn delete_rejects_an_unknown_folder() {
        let (client, _) = client(storage_with(Vec::new()));

        let err = client
            .delete(Uuid::now_v7(), FolderContents::Keep)
            .await
            .unwrap_err();

        assert_eq!(err.code(), ErrorCode::FolderNotFound);
    }

    fn applier(storage: MockStorage) -> FolderApplier<MockStorage, MockDocumentStorage> {
        applier_with_documents(storage, MockDocumentStorage::new())
    }

    fn applier_with_documents(
        storage: MockStorage,
        documents: MockDocumentStorage,
    ) -> FolderApplier<MockStorage, MockDocumentStorage> {
        FolderApplier::new(storage, documents)
    }

    /// Documents storage expecting the documents of `folder` to move to
    /// `to`.
    fn documents_moving(folder: Uuid, to: Option<Uuid>) -> MockDocumentStorage {
        let mut documents = MockDocumentStorage::new();
        documents
            .expect_move_folder_documents()
            .withf(move |folders, target| folders == [folder] && *target == to)
            .times(1)
            .returning(|_, _| Ok(()));
        documents
    }

    fn storage_expecting(
        folders: Vec<Folder>,
        save: Vec<Folder>,
        delete: Vec<Uuid>,
    ) -> MockStorage {
        let mut storage = storage_with(folders);
        storage
            .expect_apply()
            .with(eq(save), eq(delete))
            .times(1)
            .returning(|_, _| Ok(()));
        storage
    }

    #[tokio::test]
    async fn apply_save_stores_the_folder_as_published() {
        let parent = FakeFolder::new().build();
        let folder = FakeFolder::new().with_parent(parent.id).build();
        let client = applier(storage_expecting(
            vec![parent],
            vec![folder.clone()],
            Vec::new(),
        ));

        client.apply_save(folder).await.unwrap();
    }

    #[tokio::test]
    async fn apply_save_moves_a_folder_whose_parent_is_gone_to_the_top_level() {
        let folder = FakeFolder::new().with_parent(Uuid::now_v7()).build();
        let expected = Folder {
            parent_id: None,
            appearance: Some(DEFAULT_APPEARANCE),
            ..folder.clone()
        };
        let client = applier(storage_expecting(Vec::new(), vec![expected], Vec::new()));

        client.apply_save(folder).await.unwrap();
    }

    #[tokio::test]
    async fn apply_save_breaks_a_cycle_two_devices_made_by_moving_to_the_top_level() {
        // Device 1 moved A into B while device 2 moved B into A: B is
        // already under A here when A's move into B arrives.
        let a = FakeFolder::new().build();
        let b = FakeFolder::new().with_parent(a.id).build();
        let moved_a = Folder {
            parent_id: Some(b.id),
            appearance: None,
            ..a.clone()
        };
        let expected = Folder {
            parent_id: None,
            appearance: Some(DEFAULT_APPEARANCE),
            ..a.clone()
        };
        let client = applier(storage_expecting(vec![a, b], vec![expected], Vec::new()));

        client.apply_save(moved_a).await.unwrap();
    }

    #[tokio::test]
    async fn apply_save_renames_a_folder_clashing_with_a_sibling_made_on_another_device() {
        let existing = FakeFolder::new().with_name("Voiture").build();
        let folder = FakeFolder::new().with_name("voiture").build();
        let expected = Folder {
            name: "voiture (2)".to_string(),
            ..folder.clone()
        };
        let client = applier(storage_expecting(
            vec![existing],
            vec![expected],
            Vec::new(),
        ));

        client.apply_save(folder).await.unwrap();
    }

    #[tokio::test]
    async fn apply_delete_keeping_contents_moves_subfolders_up() {
        let top = FakeFolder::new().with_appearance(Some(APPEARANCE)).build();
        let deleted = FakeFolder::new().with_parent(top.id).build();
        let child = FakeFolder::new().with_parent(deleted.id).build();
        let expected = Folder {
            parent_id: Some(top.id),
            ..child.clone()
        };
        let documents = documents_moving(deleted.id, Some(top.id));
        let client = applier_with_documents(
            storage_expecting(
                vec![top, deleted.clone(), child],
                vec![expected],
                vec![deleted.id],
            ),
            documents,
        );

        client
            .apply_delete(deleted.id, FolderContents::Keep)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn apply_delete_keeping_contents_of_a_top_level_folder_gives_its_children_its_appearance()
    {
        let deleted = FakeFolder::new().with_appearance(Some(APPEARANCE)).build();
        let child = FakeFolder::new().with_parent(deleted.id).build();
        let expected = Folder {
            parent_id: None,
            appearance: Some(APPEARANCE),
            ..child.clone()
        };
        let documents = documents_moving(deleted.id, None);
        let client = applier_with_documents(
            storage_expecting(
                vec![deleted.clone(), child],
                vec![expected],
                vec![deleted.id],
            ),
            documents,
        );

        client
            .apply_delete(deleted.id, FolderContents::Keep)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn apply_delete_keeping_contents_renames_a_child_clashing_with_its_new_siblings() {
        let parent = FakeFolder::new().build();
        let sibling = FakeFolder::new()
            .with_parent(parent.id)
            .with_name("Eau")
            .build();
        let deleted = FakeFolder::new().with_parent(parent.id).build();
        let child = FakeFolder::new()
            .with_parent(deleted.id)
            .with_name("Eau")
            .build();
        let expected = Folder {
            parent_id: Some(parent.id),
            name: "Eau (2)".to_string(),
            ..child.clone()
        };
        let documents = documents_moving(deleted.id, Some(parent.id));
        let client = applier_with_documents(
            storage_expecting(
                vec![parent, sibling, deleted.clone(), child],
                vec![expected],
                vec![deleted.id],
            ),
            documents,
        );

        client
            .apply_delete(deleted.id, FolderContents::Keep)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn apply_delete_with_contents_removes_every_descendant() {
        let deleted = FakeFolder::new().build();
        let child = FakeFolder::new().with_parent(deleted.id).build();
        let grandchild = FakeFolder::new().with_parent(child.id).build();
        let other = FakeFolder::new().build();

        let mut storage = storage_with(vec![
            deleted.clone(),
            child.clone(),
            grandchild.clone(),
            other,
        ]);
        let expected: HashSet<Uuid> = [deleted.id, child.id, grandchild.id].into();
        let mut documents = MockDocumentStorage::new();
        let expected_documents = expected.clone();
        documents
            .expect_delete_folder_documents()
            .withf(move |folders| {
                folders.iter().copied().collect::<HashSet<_>>() == expected_documents
            })
            .times(1)
            .returning(|_| Ok(()));
        storage
            .expect_apply()
            .withf(move |save, delete| {
                save.is_empty() && delete.iter().copied().collect::<HashSet<_>>() == expected
            })
            .times(1)
            .returning(|_, _| Ok(()));
        let client = applier_with_documents(storage, documents);

        client
            .apply_delete(deleted.id, FolderContents::Delete)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn apply_delete_of_an_unknown_folder_changes_nothing() {
        let client = applier(storage_with(Vec::new()));

        client
            .apply_delete(Uuid::now_v7(), FolderContents::Keep)
            .await
            .unwrap();
    }
}
