use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::InstalledScript;
use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `installed_scripts` table).
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
    async fn save_installed_script(&self, installed: &InstalledScript) -> Result<()> {
        let installation_id = installed.installation_id;
        let script = serde_json::to_string(&installed.script).with_context(|| {
            format!("failed to serialize the script of installation {installation_id}")
        })?;
        let parameters = serde_json::to_string(&installed.parameters).with_context(|| {
            format!("failed to serialize parameters of installation {installation_id}")
        })?;

        sqlx::query(
            "INSERT INTO installed_scripts
                 (installation_id, script_id, label, script, parameters, folder_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(installation_id) DO UPDATE SET
                 script_id = excluded.script_id,
                 label = excluded.label,
                 script = excluded.script,
                 parameters = excluded.parameters,
                 folder_id = excluded.folder_id",
        )
        .bind(installation_id.to_string())
        .bind(installed.script.id.to_string())
        .bind(&installed.label)
        .bind(script)
        .bind(parameters)
        .bind(installed.folder_id.map(|id| id.to_string()))
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!("failed to save installation {installation_id} to local database")
        })?;

        Ok(())
    }

    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>> {
        let rows = sqlx::query(
            "SELECT installation_id, label, script, parameters, folder_id FROM installed_scripts
             ORDER BY installed_at, rowid",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list installed scripts from local database")?;

        rows.iter().map(installed_script_from_row).collect()
    }

    async fn find_installed_script(
        &self,
        installation_id: Uuid,
    ) -> Result<Option<InstalledScript>> {
        let row = sqlx::query(
            "SELECT installation_id, label, script, parameters, folder_id FROM installed_scripts
             WHERE installation_id = ?1",
        )
        .bind(installation_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .with_context(|| {
            format!("failed to fetch installation {installation_id} from local database")
        })?;

        row.as_ref().map(installed_script_from_row).transpose()
    }

    async fn delete_installed_script(&self, installation_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM installed_scripts WHERE installation_id = ?1")
            .bind(installation_id.to_string())
            .execute(&self.pool)
            .await
            .with_context(|| {
                format!("failed to delete installation {installation_id} from local database")
            })?;

        Ok(())
    }
}

fn installed_script_from_row(row: &SqliteRow) -> Result<InstalledScript> {
    let installation_id: String = row.get("installation_id");
    let script: String = row.get("script");
    let parameters: String = row.get("parameters");
    let folder_id: Option<String> = row.get("folder_id");

    Ok(InstalledScript {
        installation_id: installation_id.parse().with_context(|| {
            format!("invalid installation id {installation_id:?} in local database")
        })?,
        label: row.get("label"),
        script: serde_json::from_str(&script).with_context(|| {
            format!("invalid script json in local database for installation {installation_id}")
        })?,
        parameters: serde_json::from_str(&parameters).with_context(|| {
            format!("invalid parameters json in local database for installation {installation_id}")
        })?,
        folder_id: folder_id
            .map(|id| Uuid::parse_str(&id))
            .transpose()
            .with_context(|| {
                format!("invalid folder id in local database for installation {installation_id}")
            })?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::domains::scripts::{FakeInstalledScript, FakeScript};

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

    #[tokio::test]
    async fn list_installed_scripts_is_empty_when_nothing_was_saved() {
        let storage = setup().await;

        assert_eq!(storage.list_installed_scripts().await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn find_installed_script_returns_only_that_installation() {
        let storage = setup().await;
        let wanted = FakeInstalledScript::new().build();
        storage.save_installed_script(&wanted).await.unwrap();
        storage
            .save_installed_script(&FakeInstalledScript::new().build())
            .await
            .unwrap();

        assert_eq!(
            storage
                .find_installed_script(wanted.installation_id())
                .await
                .unwrap(),
            Some(wanted)
        );
        assert_eq!(
            storage.find_installed_script(Uuid::new_v4()).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn find_installed_script_returns_its_folder() {
        let storage = setup().await;
        let installed = FakeInstalledScript::new()
            .with_folder_id(Uuid::now_v7())
            .build();
        storage.save_installed_script(&installed).await.unwrap();

        assert_eq!(
            storage
                .find_installed_script(installed.installation_id())
                .await
                .unwrap(),
            Some(installed)
        );
    }

    #[tokio::test]
    async fn list_installed_scripts_returns_saved_scripts_oldest_first() {
        let storage = setup().await;
        let first = FakeInstalledScript::new().build();
        let second = FakeInstalledScript::new().build();

        storage.save_installed_script(&first).await.unwrap();
        storage.save_installed_script(&second).await.unwrap();

        assert_eq!(
            storage.list_installed_scripts().await.unwrap(),
            vec![first, second]
        );
    }

    #[tokio::test]
    async fn save_installed_script_keeps_every_installation_of_the_same_script() {
        let storage = setup().await;
        let script = FakeScript::new().build();
        let alice = FakeInstalledScript::new()
            .with_script(script.clone())
            .with_label("alice")
            .build();
        let bob = FakeInstalledScript::new()
            .with_script(script)
            .with_label("bob")
            .build();

        storage.save_installed_script(&alice).await.unwrap();
        storage.save_installed_script(&bob).await.unwrap();

        assert_eq!(
            storage.list_installed_scripts().await.unwrap(),
            vec![alice, bob]
        );
    }

    #[tokio::test]
    async fn save_installed_script_replaces_the_same_installation_in_place() {
        let storage = setup().await;
        let first = FakeInstalledScript::new()
            .with_parameters(HashMap::from([("username".to_string(), json!("alice"))]))
            .build();
        let other = FakeInstalledScript::new().build();
        let replacement = FakeInstalledScript::new()
            .with_installation_id(first.installation_id())
            .with_script(
                FakeScript::new()
                    .with_id(first.script().id())
                    .with_version(2)
                    .build(),
            )
            .with_label("renamed")
            .with_parameters(HashMap::from([("username".to_string(), json!("bob"))]))
            .with_folder_id(Uuid::now_v7())
            .build();

        storage.save_installed_script(&first).await.unwrap();
        storage.save_installed_script(&other).await.unwrap();
        storage.save_installed_script(&replacement).await.unwrap();

        assert_eq!(
            storage.list_installed_scripts().await.unwrap(),
            vec![replacement, other]
        );
    }

    #[tokio::test]
    async fn delete_installed_script_deletes_only_that_installation() {
        let storage = setup().await;
        let script = FakeScript::new().build();
        let deleted = FakeInstalledScript::new()
            .with_script(script.clone())
            .build();
        let kept = FakeInstalledScript::new().with_script(script).build();
        storage.save_installed_script(&deleted).await.unwrap();
        storage.save_installed_script(&kept).await.unwrap();

        storage
            .delete_installed_script(deleted.installation_id())
            .await
            .unwrap();
        storage
            .delete_installed_script(Uuid::now_v7())
            .await
            .unwrap();

        assert_eq!(storage.list_installed_scripts().await.unwrap(), vec![kept]);
    }
}
