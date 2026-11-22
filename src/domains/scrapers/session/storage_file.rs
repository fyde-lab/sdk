use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncWriteExt as _;

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

        write_owner_only(&path, contents.as_bytes())
            .await
            .with_context(|| format!("failed to write session file {}", path.display()))?;

        Ok(())
    }

    async fn delete(&self, scraper_name: &str) -> Result<()> {
        let path = self.path(scraper_name);

        match tokio::fs::remove_file(&path).await {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err)
                .with_context(|| format!("failed to delete session file {}", path.display())),
            _ => Ok(()),
        }
    }
}

/// Writes `contents` to `path` through a sibling temporary file created
/// readable and writable by its owner only, then renamed into place. A run
/// interrupted mid-write then leaves the previous file whole: a truncated
/// one would fail to parse in [`FileStorage::get`], failing every later run
/// of that scraper until it's deleted by hand. The session data (saved
/// document URLs, a script's cursors...) also mustn't inherit a
/// umask-derived, typically world-readable mode (a file a previous version
/// wrote that way is replaced by the rename). Off Unix only the atomic
/// replace applies.
async fn write_owner_only(path: &Path, contents: &[u8]) -> Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);

    match tokio::fs::remove_file(&tmp).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to remove stale temporary file {}", tmp.display())
            });
        }
    }

    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&tmp)
        .await
        .with_context(|| format!("failed to create temporary file {}", tmp.display()))?;
    file.write_all(contents)
        .await
        .with_context(|| format!("failed to write temporary file {}", tmp.display()))?;
    file.sync_all()
        .await
        .with_context(|| format!("failed to flush temporary file {}", tmp.display()))?;
    drop(file);

    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("failed to move temporary file {} into place", tmp.display()))
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
    async fn delete_forgets_only_that_scrapers_data() {
        let (_tmp, storage) = setup();
        storage.set("didaxis", json!({"a": 1})).await.unwrap();
        storage.set("impots", json!({"b": 2})).await.unwrap();

        storage.delete("didaxis").await.unwrap();
        storage.delete("never-saved").await.unwrap();

        assert_eq!(storage.get("didaxis").await.unwrap(), None);
        assert_eq!(storage.get("impots").await.unwrap(), Some(json!({"b": 2})));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn set_writes_a_file_only_its_owner_can_read() {
        use std::os::unix::fs::PermissionsExt as _;

        let (tmp, storage) = setup();
        let path = tmp.path().join("didaxis.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        storage
            .set("didaxis", json!({"cursor": "abc"}))
            .await
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn set_leaves_no_temporary_file_behind() {
        let (tmp, storage) = setup();
        std::fs::write(tmp.path().join("didaxis.json.tmp"), "stale").unwrap();

        storage
            .set("didaxis", json!({"cursor": "abc"}))
            .await
            .unwrap();

        let names: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("didaxis.json")]);
        assert_eq!(
            storage.get("didaxis").await.unwrap(),
            Some(json!({"cursor": "abc"}))
        );
    }
}
