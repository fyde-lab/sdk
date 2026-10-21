use std::io::ErrorKind as IoErrorKind;
use std::path::PathBuf;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::{ErrorContext as _, Result};

/// Deletes the files a client keeps on this device outside its local
/// database, used to clear local state on logout alongside
/// [`crate::tools::sql::LocalDatabase::wipe`]. Trait methods take `&self`
/// so implementations can be shared behind `Arc<dyn LocalFiles>`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait LocalFiles: Send + Sync {
    async fn clear(&self) -> Result<()>;
}

/// A client's own directory (`<data_dir>/<client_name>/`, see
/// `crate::client_database_path`), or nothing for an in-memory client,
/// which keeps no files at all.
pub(crate) enum ClientDirectory {
    Disk {
        dir: PathBuf,
        /// Entries [`LocalFiles::clear`] leaves in place: files still open
        /// for the rest of the process's lifetime, which deleting would
        /// orphan rather than free — the database (emptied by its own
        /// `wipe` instead, see [`crate::tools::sql::SqliteClient::wipe`])
        /// and the changelog lock other processes may be holding.
        keep: Vec<PathBuf>,
    },
    Memory,
}

#[async_trait]
impl LocalFiles for ClientDirectory {
    /// Removes every entry of the directory but [`ClientDirectory::Disk`]'s
    /// `keep`, recursively. Goes through every entry even once one fails,
    /// so a single undeletable file never leaves the rest behind, and
    /// reports the first failure.
    async fn clear(&self) -> Result<()> {
        let ClientDirectory::Disk { dir, keep } = self else {
            return Ok(());
        };

        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == IoErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to list local directory {}", dir.display()));
            }
        };

        let mut result = Ok(());
        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("failed to list local directory {}", dir.display()))?
        {
            let path = entry.path();
            if keep.contains(&path) {
                continue;
            }
            let removed = remove_entry(&path)
                .await
                .with_context(|| format!("failed to delete local file {}", path.display()));
            if result.is_ok() {
                result = removed;
            }
        }
        result
    }
}

/// Removes `path`, recursively if it's a directory. A symlink is removed
/// itself, never followed. Already gone (e.g. deleted by another process
/// sharing the directory) counts as removed.
async fn remove_entry(path: &std::path::Path) -> std::io::Result<()> {
    let removed = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(err) => Err(err),
    };
    match removed {
        Err(err) if err.kind() == IoErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clear_deletes_every_entry_but_the_kept_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("desktop");
        std::fs::create_dir_all(dir.join("cache").join("nested")).unwrap();
        std::fs::write(dir.join("cache").join("nested").join("icon.png"), b"png").unwrap();
        std::fs::write(dir.join("stray.json"), b"{}").unwrap();
        std::fs::write(dir.join("fyde.db"), b"db").unwrap();
        std::fs::write(dir.join("fyde.db.consume.lock"), b"").unwrap();

        ClientDirectory::Disk {
            dir: dir.clone(),
            keep: vec![dir.join("fyde.db"), dir.join("fyde.db.consume.lock")],
        }
        .clear()
        .await
        .unwrap();

        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["fyde.db", "fyde.db.consume.lock"]);
        assert_eq!(std::fs::read(dir.join("fyde.db")).unwrap(), b"db");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn clear_removes_a_symlink_without_following_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("desktop");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();

        ClientDirectory::Disk {
            dir: dir.clone(),
            keep: vec![],
        }
        .clear()
        .await
        .unwrap();

        assert!(!dir.join("link").exists());
        assert!(outside.join("keep.txt").exists());
    }

    #[tokio::test]
    async fn clear_succeeds_on_a_missing_directory() {
        let tmp = tempfile::tempdir().unwrap();

        ClientDirectory::Disk {
            dir: tmp.path().join("missing"),
            keep: vec![],
        }
        .clear()
        .await
        .unwrap();
    }
}
