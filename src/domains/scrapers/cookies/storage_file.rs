use std::path::PathBuf;

use async_trait::async_trait;

use crate::{ErrorContext as _, Result};

use super::models::Cookie;
use super::storage::Storage;

/// A [`Storage`] backed by one JSON file per scraper under `dir`, the way
/// `demo-rust-fyde` persisted cookies (bundled with session data) before
/// this sub-domain existed. Meant for `../scripts`' standalone CLI runner,
/// which has no local SQLite database of its own — see
/// [`crate::init_dev_scrapers`].
pub(crate) struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, scraper_name: &str) -> PathBuf {
        self.dir.join(format!("{scraper_name}.cookies.json"))
    }
}

#[async_trait]
impl Storage for FileStorage {
    async fn list(&self, scraper_name: &str) -> Result<Vec<Cookie>> {
        let path = self.path(scraper_name);

        if !tokio::fs::try_exists(&path).await.with_context(|| {
            format!(
                "failed to check for a saved cookie file at {}",
                path.display()
            )
        })? {
            return Ok(Vec::new());
        }

        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read cookie file {}", path.display()))?;

        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse cookie file {} as JSON", path.display()))
    }

    async fn replace_all(&self, scraper_name: &str, cookies: Vec<Cookie>) -> Result<()> {
        let path = self.path(scraper_name);

        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| {
                format!("failed to create cookies directory {}", self.dir.display())
            })?;

        let contents = serde_json::to_string_pretty(&cookies)
            .with_context(|| format!("failed to serialize cookies for scraper {scraper_name:?}"))?;

        tokio::fs::write(&path, contents)
            .await
            .with_context(|| format!("failed to write cookie file {}", path.display()))?;

        Ok(())
    }

    async fn delete_all(&self, scraper_name: &str) -> Result<()> {
        let path = self.path(scraper_name);

        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => {
                Err(err).with_context(|| format!("failed to delete cookie file {}", path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::FakeCookie;
    use super::*;

    fn setup() -> (tempfile::TempDir, FileStorage) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FileStorage::new(tmp.path().to_path_buf());
        (tmp, storage)
    }

    #[tokio::test]
    async fn list_returns_empty_when_never_saved() {
        let (_tmp, storage) = setup();

        assert_eq!(storage.list("didaxis").await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn replace_all_then_list_returns_the_saved_cookies() {
        let (_tmp, storage) = setup();
        let cookies = vec![
            FakeCookie::new()
                .with_origin("https://a.example.com")
                .build(),
            FakeCookie::new()
                .with_origin("https://b.example.com")
                .build(),
        ];

        storage
            .replace_all("didaxis", cookies.clone())
            .await
            .unwrap();

        let mut saved = storage.list("didaxis").await.unwrap();
        saved.sort_by(|a, b| a.origin.cmp(&b.origin));
        let mut expected = cookies;
        expected.sort_by(|a, b| a.origin.cmp(&b.origin));
        assert_eq!(saved, expected);
    }

    #[tokio::test]
    async fn replace_all_overwrites_previously_saved_cookies() {
        let (_tmp, storage) = setup();
        storage
            .replace_all(
                "didaxis",
                vec![
                    FakeCookie::new()
                        .with_origin("https://old.example.com")
                        .build(),
                ],
            )
            .await
            .unwrap();

        let new_cookie = FakeCookie::new()
            .with_origin("https://new.example.com")
            .build();
        storage
            .replace_all("didaxis", vec![new_cookie.clone()])
            .await
            .unwrap();

        assert_eq!(storage.list("didaxis").await.unwrap(), vec![new_cookie]);
    }

    #[tokio::test]
    async fn distinct_scrapers_are_stored_independently() {
        let (_tmp, storage) = setup();
        let didaxis_cookie = FakeCookie::new()
            .with_origin("https://didaxis.example.com")
            .build();
        let impots_cookie = FakeCookie::new()
            .with_origin("https://impots.example.com")
            .build();

        storage
            .replace_all("didaxis", vec![didaxis_cookie.clone()])
            .await
            .unwrap();
        storage
            .replace_all("impots", vec![impots_cookie.clone()])
            .await
            .unwrap();

        assert_eq!(storage.list("didaxis").await.unwrap(), vec![didaxis_cookie]);
        assert_eq!(storage.list("impots").await.unwrap(), vec![impots_cookie]);
    }

    #[tokio::test]
    async fn replace_all_creates_the_directory_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("cookies");
        let storage = FileStorage::new(dir.clone());

        storage
            .replace_all("didaxis", vec![FakeCookie::new().build()])
            .await
            .unwrap();

        assert!(dir.join("didaxis.cookies.json").exists());
    }

    #[tokio::test]
    async fn delete_all_removes_only_that_scrapers_cookies() {
        let (_tmp, storage) = setup();
        let kept = FakeCookie::new().build();
        storage
            .replace_all("didaxis", vec![FakeCookie::new().build()])
            .await
            .unwrap();
        storage
            .replace_all("impots", vec![kept.clone()])
            .await
            .unwrap();

        storage.delete_all("didaxis").await.unwrap();

        assert!(storage.list("didaxis").await.unwrap().is_empty());
        assert_eq!(storage.list("impots").await.unwrap(), vec![kept]);
    }

    #[tokio::test]
    async fn delete_all_succeeds_when_nothing_was_saved() {
        let (_tmp, storage) = setup();
        storage.delete_all("didaxis").await.unwrap();
    }
}
