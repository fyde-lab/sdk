use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use crate::{ErrorContext as _, Result};

use super::storage::Storage;

/// A [`Storage`] backed by one JSON file per scraper under `dir`, the way
/// `demo-rust-fyde` persisted session data (as `<sessions_dir>/
/// <scraper_name>.json`) before this sub-domain existed. Meant for
/// `../scripts`' standalone CLI runner, which has no local SQLite database
/// of its own — see [`crate::init_dev_scrapers`].
pub(crate) struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, scraper_name: &str) -> PathBuf {
        self.dir.join(format!("{scraper_name}.json"))
    }
}

#[async_trait]
impl Storage for FileStorage {
    async fn get(&self, scraper_name: &str) -> Result<Option<Value>> {
        let path = self.path(scraper_name);

        if !tokio::fs::try_exists(&path).await.with_context(|| {
            format!(
                "failed to check for a saved session file at {}",
                path.display()
            )
        })? {
            return Ok(None);
        }

        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read session file {}", path.display()))?;

        let value = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse session file {} as JSON", path.display()))?;

        Ok(Some(value))
    }

    async fn set(&self, scraper_name: &str, data: Value) -> Result<()> {
        let path = self.path(scraper_name);

        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| {
                format!("failed to create sessions directory {}", self.dir.display())
            })?;

        let contents = serde_json::to_string_pretty(&data).with_context(|| {
            format!("failed to serialize session data for scraper {scraper_name:?}")
        })?;

        tokio::fs::write(&path, contents)
            .await
            .with_context(|| format!("failed to write session file {}", path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn setup() -> (tempfile::TempDir, FileStorage) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FileStorage::new(tmp.path().to_path_buf());
        (tmp, storage)
    }

    #[tokio::test]
    async fn get_returns_none_when_never_saved() {
        let (_tmp, storage) = setup();

        assert_eq!(storage.get("didaxis").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_returns_the_saved_data() {
        let (_tmp, storage) = setup();

        storage
            .set("didaxis", json!({"cursor": "abc"}))
            .await
            .unwrap();

        assert_eq!(
            storage.get("didaxis").await.unwrap(),
            Some(json!({"cursor": "abc"}))
        );
    }

    #[tokio::test]
    async fn set_overwrites_previously_saved_data() {
        let (_tmp, storage) = setup();

        storage
            .set("didaxis", json!({"cursor": "old"}))
            .await
            .unwrap();
        storage
            .set("didaxis", json!({"cursor": "new"}))
            .await
            .unwrap();

        assert_eq!(
            storage.get("didaxis").await.unwrap(),
            Some(json!({"cursor": "new"}))
        );
    }

    #[tokio::test]
    async fn distinct_scrapers_are_stored_independently() {
        let (_tmp, storage) = setup();

        storage
            .set("didaxis", json!({"cursor": "a"}))
            .await
            .unwrap();
        storage.set("impots", json!({"cursor": "b"})).await.unwrap();

        assert_eq!(
            storage.get("didaxis").await.unwrap(),
            Some(json!({"cursor": "a"}))
        );
        assert_eq!(
            storage.get("impots").await.unwrap(),
            Some(json!({"cursor": "b"}))
        );
    }

    #[tokio::test]
    async fn set_creates_the_directory_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("sessions");
        let storage = FileStorage::new(dir.clone());

        storage.set("didaxis", json!({})).await.unwrap();

        assert!(dir.join("didaxis.json").exists());
    }
}
