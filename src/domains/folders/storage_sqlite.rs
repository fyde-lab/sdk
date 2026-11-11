use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::storage::Storage;
use super::{Folder, FolderAppearance};

/// A [`Storage`] backed by the SDK's local SQLite database (the `folders`
/// table).
pub(crate) struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn list_folders(&self) -> Result<Vec<Folder>> {
        let rows = sqlx::query("SELECT id, parent_id, name, description, color, icon FROM folders")
            .fetch_all(&self.pool)
            .await
            .context("failed to list folders from local database")?;

        rows.iter().map(folder_from_row).collect()
    }

    async fn apply(&self, save: Vec<Folder>, delete: Vec<Uuid>) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start a folders transaction")?;

        for folder in &save {
            let (color, icon) = match folder.appearance {
                Some(appearance) => (
                    Some(name_of(&appearance.color)?),
                    Some(name_of(&appearance.icon)?),
                ),
                None => (None, None),
            };
            sqlx::query(
                "INSERT INTO folders (id, parent_id, name, description, color, icon)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                     parent_id = excluded.parent_id,
                     name = excluded.name,
                     description = excluded.description,
                     color = excluded.color,
                     icon = excluded.icon",
            )
            .bind(folder.id.to_string())
            .bind(folder.parent_id.map(|id| id.to_string()))
            .bind(&folder.name)
            .bind(&folder.description)
            .bind(color)
            .bind(icon)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("failed to save folder {} to local database", folder.id))?;
        }

        for id in &delete {
            sqlx::query("DELETE FROM folders WHERE id = ?1")
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .with_context(|| format!("failed to delete folder {id} from local database"))?;
        }

        tx.commit()
            .await
            .context("failed to commit a folders transaction")
    }
}

/// The name `value` (a unit enum variant) serializes to.
fn name_of<T: Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value).context("failed to serialize a folder field")?;
    Ok(value.as_str().unwrap_or_default().to_string())
}

fn parse_name<T: DeserializeOwned>(name: &str, id: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(name.to_string()))
        .with_context(|| format!("invalid appearance {name:?} in local database for folder {id}"))
}

fn folder_from_row(row: &SqliteRow) -> Result<Folder> {
    let id: String = row.get("id");
    let parent_id: Option<String> = row.get("parent_id");
    let color: Option<String> = row.get("color");
    let icon: Option<String> = row.get("icon");

    let appearance = match (color, icon) {
        (Some(color), Some(icon)) => Some(FolderAppearance {
            color: parse_name(&color, &id)?,
            icon: parse_name(&icon, &id)?,
        }),
        _ => None,
    };

    Ok(Folder {
        id: Uuid::parse_str(&id)
            .with_context(|| format!("invalid folder id in local database: {id}"))?,
        parent_id: parent_id
            .map(|parent| {
                Uuid::parse_str(&parent).with_context(|| {
                    format!("invalid parent id in local database for folder {id}: {parent}")
                })
            })
            .transpose()?,
        name: row.get("name"),
        description: row.get("description"),
        appearance,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::domains::folders::{FakeFolder, FolderColor, FolderIcon};

    async fn setup() -> SqliteStorage {
        // A single connection, so all queries in a test hit the same
        // in-memory database rather than each getting its own.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteStorage::new(pool)
    }

    fn sorted(mut folders: Vec<Folder>) -> Vec<Folder> {
        folders.sort_by_key(|folder| folder.id);
        folders
    }

    #[tokio::test]
    async fn apply_saves_top_level_folders_and_subfolders() {
        let storage = setup().await;
        let parent = FakeFolder::new()
            .with_appearance(Some(FolderAppearance {
                color: FolderColor::Teal,
                icon: FolderIcon::Briefcase,
            }))
            .build();
        let child = FakeFolder::new().with_parent(parent.id).build();

        storage
            .apply(vec![parent.clone(), child.clone()], Vec::new())
            .await
            .unwrap();

        assert_eq!(
            sorted(storage.list_folders().await.unwrap()),
            sorted(vec![parent, child])
        );
    }

    #[tokio::test]
    async fn apply_replaces_a_saved_folder() {
        let storage = setup().await;
        let folder = FakeFolder::new().build();
        storage
            .apply(vec![folder.clone()], Vec::new())
            .await
            .unwrap();
        let renamed = FakeFolder::new()
            .with_id(folder.id)
            .with_name("Renamed")
            .with_description("Other")
            .build();

        storage
            .apply(vec![renamed.clone()], Vec::new())
            .await
            .unwrap();

        assert_eq!(storage.list_folders().await.unwrap(), vec![renamed]);
    }

    #[tokio::test]
    async fn apply_deletes_folders_and_ignores_unknown_ids() {
        let storage = setup().await;
        let kept = FakeFolder::new().build();
        let deleted = FakeFolder::new().build();
        storage
            .apply(vec![kept.clone(), deleted.clone()], Vec::new())
            .await
            .unwrap();

        storage
            .apply(Vec::new(), vec![deleted.id, Uuid::now_v7()])
            .await
            .unwrap();

        assert_eq!(storage.list_folders().await.unwrap(), vec![kept]);
    }
}
