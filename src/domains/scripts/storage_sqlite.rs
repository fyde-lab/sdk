use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

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
        let script_id = installed.script.id;
        let script = serde_json::to_string(&installed.script)
            .with_context(|| format!("failed to serialize installed script {script_id}"))?;
        let parameters = serde_json::to_string(&installed.parameters).with_context(|| {
            format!("failed to serialize parameters of installed script {script_id}")
        })?;

        sqlx::query(
            "INSERT INTO installed_scripts (script_id, script, parameters) VALUES (?1, ?2, ?3)
             ON CONFLICT(script_id) DO UPDATE SET
                 script = excluded.script,
                 parameters = excluded.parameters,
                 installed_at = excluded.installed_at",
        )
        .bind(script_id.to_string())
        .bind(script)
        .bind(parameters)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!("failed to save installed script {script_id} to local database")
        })?;

        Ok(())
    }

    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>> {
        let rows = sqlx::query(
            "SELECT script_id, script, parameters FROM installed_scripts
             ORDER BY installed_at, rowid",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list installed scripts from local database")?;

        rows.into_iter()
            .map(|row| {
                let script_id: String = row.get("script_id");
                let script: String = row.get("script");
                let parameters: String = row.get("parameters");

                Ok(InstalledScript {
                    script: serde_json::from_str(&script).with_context(|| {
                        format!("invalid script json in local database for script {script_id}")
                    })?,
                    parameters: serde_json::from_str(&parameters).with_context(|| {
                        format!("invalid parameters json in local database for script {script_id}")
                    })?,
                })
            })
            .collect()
    }
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
    async fn save_installed_script_replaces_a_previous_installation_of_the_same_script() {
        let storage = setup().await;
        let script = FakeScript::new().build();
        let first = FakeInstalledScript::new()
            .with_script(script.clone())
            .with_parameters(HashMap::from([("username".to_string(), json!("alice"))]))
            .build();
        let second = FakeInstalledScript::new()
            .with_script(script)
            .with_parameters(HashMap::from([("username".to_string(), json!("bob"))]))
            .build();

        storage.save_installed_script(&first).await.unwrap();
        storage.save_installed_script(&second).await.unwrap();

        assert_eq!(
            storage.list_installed_scripts().await.unwrap(),
            vec![second]
        );
    }
}
