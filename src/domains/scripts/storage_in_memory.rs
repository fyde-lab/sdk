use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{ErrorKind, Result};

use super::Script;
use super::ScriptMetadata;
use super::Service;
use super::{InstallScriptRequest, InstalledScript};

/// A [`Service`] implementation holding scripts purely in memory, for
/// running scripts (e.g. via `init_dev_scrapers`) without a live scripts
/// server. State doesn't survive past the process's lifetime.
/// With no changelog to publish through, [`Service::install_script`] saves
/// the installation directly, so it shows up in
/// [`Service::list_installed_scripts`] immediately. [`Service`] has no way to
/// author scripts, so they're added with [`InMemoryScriptStorage::insert`].
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

    /// Adds `script`, or replaces the one with the same id, as the server
    /// would serve it (public or not, as `script` says).
    pub fn insert(&self, script: Script) {
        self.scripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(script.id, script);
    }
}

#[async_trait]
impl Service for InMemoryScriptStorage {
    async fn fetch_script(&self, id: Uuid) -> Result<Script> {
        self.scripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
            .ok_or(ErrorKind::ScriptNotFound(id).into())
    }

    async fn enable_script(&self, script_id: Uuid) -> Result<()> {
        self.enabled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(script_id);
        Ok(())
    }

    async fn disable_script(&self, script_id: Uuid) -> Result<()> {
        self.enabled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&script_id);
        Ok(())
    }

    async fn list_user_scripts(&self) -> Result<Vec<ScriptMetadata>> {
        let scripts = self.scripts.lock().unwrap_or_else(PoisonError::into_inner);
        let enabled = self.enabled.lock().unwrap_or_else(PoisonError::into_inner);

        Ok(enabled
            .iter()
            .filter_map(|id| scripts.get(id).cloned().map(ScriptMetadata::from))
            .collect())
    }

    async fn list_public_scripts(&self) -> Result<Vec<ScriptMetadata>> {
        Ok(self
            .scripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|script| script.is_public)
            .cloned()
            .map(ScriptMetadata::from)
            .collect())
    }

    async fn install_script(&self, request: InstallScriptRequest) -> Result<()> {
        let script = self.fetch_script(request.script_id).await?;
        let mut installed = self
            .installed
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        installed.retain(|existing| existing.script.id != script.id);
        installed.push(InstalledScript {
            script,
            parameters: request.parameters,
        });

        Ok(())
    }

    async fn list_installed_scripts(&self) -> Result<Vec<InstalledScript>> {
        Ok(self
            .installed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    async fn fetch_installed_script(&self, script_id: Uuid) -> Result<InstalledScript> {
        self.installed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|installed| installed.script.id == script_id)
            .cloned()
            .ok_or_else(|| ErrorKind::ScriptNotInstalled(script_id).into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::domains::scripts::FakeScript;

    #[tokio::test]
    async fn fetches_an_inserted_script() {
        let storage = InMemoryScriptStorage::new();
        let script = FakeScript::new().build();
        storage.insert(script.clone());

        let fetched = storage.fetch_script(script.id).await.unwrap();

        assert_eq!(fetched, script);
    }

    #[tokio::test]
    async fn inserting_a_script_again_replaces_it() {
        let storage = InMemoryScriptStorage::new();
        let script = FakeScript::new().build();
        let id = script.id;
        storage.insert(script);
        let updated = FakeScript::new()
            .with_id(id)
            .with_version(2)
            .with_script("return 2")
            .build();
        storage.insert(updated.clone());

        assert_eq!(storage.fetch_script(id).await.unwrap(), updated);
    }

    #[tokio::test]
    async fn fetching_an_unknown_script_fails_with_not_found() {
        let storage = InMemoryScriptStorage::new();

        let err = storage.fetch_script(Uuid::now_v7()).await.unwrap_err();

        assert!(matches!(err.kind(), ErrorKind::ScriptNotFound(_)));
    }

    #[tokio::test]
    async fn lists_only_enabled_scripts() {
        let storage = InMemoryScriptStorage::new();
        let enabled = FakeScript::new().build();
        let disabled = FakeScript::new().build();
        storage.insert(enabled.clone());
        storage.insert(disabled.clone());

        storage.enable_script(enabled.id).await.unwrap();

        let listed = storage.list_user_scripts().await.unwrap();

        assert_eq!(listed, vec![ScriptMetadata::from(enabled)]);
        assert!(!listed.contains(&ScriptMetadata::from(disabled)));
    }

    #[tokio::test]
    async fn lists_only_public_scripts() {
        let storage = InMemoryScriptStorage::new();
        let public = FakeScript::new().with_public(true).build();
        let private = FakeScript::new().with_public(false).build();
        storage.insert(public.clone());
        storage.insert(private.clone());

        let listed = storage.list_public_scripts().await.unwrap();

        assert_eq!(listed, vec![ScriptMetadata::from(public)]);
        assert!(!listed.contains(&ScriptMetadata::from(private)));
    }

    #[tokio::test]
    async fn installing_a_script_lists_it_immediately_replacing_a_previous_install() {
        let storage = InMemoryScriptStorage::new();
        let script = FakeScript::new().build();
        storage.insert(script.clone());

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
        let script = FakeScript::new().build();
        storage.insert(script.clone());

        storage.enable_script(script.id).await.unwrap();
        storage.disable_script(script.id).await.unwrap();

        assert_eq!(storage.list_user_scripts().await.unwrap(), Vec::new());
    }
}
