use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{ErrorKind, Result};

use super::Script;
use super::ScriptParameter;
use super::ScriptType;
use super::Service;
use super::{InstallScriptRequest, InstalledScript};

/// A [`Service`] implementation holding scripts purely in memory, for
/// running scripts (e.g. via `documents::Service`'s `run_scripts`) without a
/// live scripts server. State doesn't survive past the process's lifetime.
/// With no changelog to publish through, [`Service::install_script`] saves
/// the installation directly, so it shows up in
/// [`Service::list_installed_scripts`] immediately.
#[derive(Default)]
pub struct InMemoryScriptStorage {
    scripts: Mutex<HashMap<Uuid, Script>>,
    enabled: Mutex<HashSet<Uuid>>,
    installed: Mutex<Vec<InstalledScript>>,
}

impl InMemoryScriptStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[async_trait]
impl Service for InMemoryScriptStorage {
    async fn create_script(
        &self,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script> {
        let created = Script {
            id: Uuid::now_v7(),
            name: name.to_string(),
            description: description.to_string(),
            short_description: short_description.to_string(),
            is_public,
            icon,
            allowed_domains,
            version: 1,
            script: script.to_string(),
            last_updated: now(),
            script_type,
            parameters,
        };

        self.scripts
            .lock()
            .unwrap()
            .insert(created.id, created.clone());

        Ok(created)
    }

    async fn fetch_script(&self, id: Uuid) -> Result<Script> {
        self.scripts
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(ErrorKind::ScriptNotFound(id).into())
    }

    async fn update_script(
        &self,
        id: Uuid,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script> {
        let mut scripts = self.scripts.lock().unwrap();

        let existing = scripts.get(&id).ok_or(ErrorKind::ScriptNotFound(id))?;

        let updated = Script {
            id,
            name: name.to_string(),
            description: description.to_string(),
            short_description: short_description.to_string(),
            is_public,
            icon,
            allowed_domains,
            version: existing.version + 1,
            script: script.to_string(),
            last_updated: now(),
            script_type,
            parameters,
        };

        scripts.insert(id, updated.clone());

        Ok(updated)
    }

    async fn enable_script(&self, script_id: Uuid) -> Result<()> {
        self.enabled.lock().unwrap().insert(script_id);
        Ok(())
    }

    async fn disable_script(&self, script_id: Uuid) -> Result<()> {
        self.enabled.lock().unwrap().remove(&script_id);
        Ok(())
    }

    async fn list_user_scripts(&self) -> Result<Vec<Script>> {
        let scripts = self.scripts.lock().unwrap();
        let enabled = self.enabled.lock().unwrap();

        Ok(enabled
            .iter()
            .filter_map(|id| scripts.get(id).cloned())
            .collect())
    }

    async fn list_public_scripts(&self) -> Result<Vec<Script>> {
        Ok(self
            .scripts
            .lock()
            .unwrap()
            .values()
            .filter(|script| script.is_public)
            .cloned()
            .collect())
    }

    async fn install_script(&self, request: InstallScriptRequest) -> Result<()> {
        let script = self.fetch_script(request.script_id).await?;
        let mut installed = self.installed.lock().unwrap();

        installed.retain(|existing| existing.script.id != script.id);
        installed.push(InstalledScript {
            script,
            parameters: request.parameters,
        });

        Ok(())
    }

    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>> {
        Ok(self.installed.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_a_script_at_version_one() {
        let storage = InMemoryScriptStorage::new();

        let created = storage
            .create_script(
                "example",
                true,
                vec![1, 2, 3],
                "return 1 + 1",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        assert_eq!(created.name, "example");
        assert_eq!(created.version, 1);
        assert_eq!(created.script, "return 1 + 1");
    }

    #[tokio::test]
    async fn fetches_a_previously_created_script() {
        let storage = InMemoryScriptStorage::new();
        let created = storage
            .create_script(
                "example",
                true,
                Vec::new(),
                "return 1",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        let fetched = storage.fetch_script(created.id).await.unwrap();

        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn fetching_an_unknown_script_fails_with_not_found() {
        let storage = InMemoryScriptStorage::new();

        let err = storage.fetch_script(Uuid::now_v7()).await.unwrap_err();

        assert!(matches!(err.kind(), ErrorKind::ScriptNotFound(_)));
    }

    #[tokio::test]
    async fn updating_a_script_increments_its_version() {
        let storage = InMemoryScriptStorage::new();
        let created = storage
            .create_script(
                "example",
                false,
                Vec::new(),
                "return 1",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        let updated = storage
            .update_script(
                created.id,
                "renamed",
                true,
                vec![9],
                "return 2",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        assert_eq!(updated.version, 2);
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.script, "return 2");
    }

    #[tokio::test]
    async fn updating_an_unknown_script_fails_with_not_found() {
        let storage = InMemoryScriptStorage::new();

        let err = storage
            .update_script(
                Uuid::now_v7(),
                "renamed",
                true,
                Vec::new(),
                "return 2",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap_err();

        assert!(matches!(err.kind(), ErrorKind::ScriptNotFound(_)));
    }

    #[tokio::test]
    async fn lists_only_enabled_scripts() {
        let storage = InMemoryScriptStorage::new();
        let enabled = storage
            .create_script(
                "enabled",
                true,
                Vec::new(),
                "return 1",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();
        let disabled = storage
            .create_script(
                "disabled",
                true,
                Vec::new(),
                "return 2",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        storage.enable_script(enabled.id).await.unwrap();

        let listed = storage.list_user_scripts().await.unwrap();

        assert_eq!(listed, vec![enabled]);
        assert!(!listed.contains(&disabled));
    }

    #[tokio::test]
    async fn lists_only_public_scripts() {
        let storage = InMemoryScriptStorage::new();
        let public = storage
            .create_script(
                "public",
                true,
                Vec::new(),
                "return 1",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();
        let private = storage
            .create_script(
                "private",
                false,
                Vec::new(),
                "return 2",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        let listed = storage.list_public_scripts().await.unwrap();

        assert_eq!(listed, vec![public]);
        assert!(!listed.contains(&private));
    }

    #[tokio::test]
    async fn installing_a_script_lists_it_immediately_replacing_a_previous_install() {
        let storage = InMemoryScriptStorage::new();
        let script = storage
            .create_script(
                "example",
                false,
                Vec::new(),
                "return 1",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        for username in ["alice", "bob"] {
            storage
                .install_script(InstallScriptRequest {
                    script_id: script.id,
                    parameters: HashMap::from([(
                        "username".to_string(),
                        serde_json::json!(username),
                    )]),
                })
                .await
                .unwrap();
        }

        let installed = storage.list_installed_scripts().await.unwrap();

        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].script, script);
        assert_eq!(
            installed[0].parameters["username"],
            serde_json::json!("bob")
        );
    }

    #[tokio::test]
    async fn disabling_a_script_removes_it_from_the_listing() {
        let storage = InMemoryScriptStorage::new();
        let script = storage
            .create_script(
                "example",
                true,
                Vec::new(),
                "return 1",
                "",
                "",
                Vec::new(),
                ScriptType::Scraper,
                HashMap::new(),
            )
            .await
            .unwrap();

        storage.enable_script(script.id).await.unwrap();
        storage.disable_script(script.id).await.unwrap();

        assert_eq!(storage.list_user_scripts().await.unwrap(), Vec::new());
    }
}
