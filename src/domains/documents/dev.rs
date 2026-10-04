use std::path::{Path, PathBuf};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{Error, ErrorContext as _, Result};

use super::{Document, Metadata, Service};

/// A [`Service`] implementation for standalone/dev use (see
/// [`crate::init_dev_scrapers`]): instead of encrypting and uploading
/// through the changelog, [`Self::upload`] just copies the file as plain
/// bytes under `out_dir`, the way `demo-rust-fyde` wrote scraped documents
/// straight to `--out-dir`. There is no local cache and no server
/// connection, so every other [`Service`] method is a no-op — nothing in
/// the scraper flow (`fyde.save_document` → [`Service::upload`] only) calls
/// them.
pub(crate) struct DevDocumentsClient {
    out_dir: PathBuf,
}

impl DevDocumentsClient {
    pub(crate) fn new(out_dir: PathBuf) -> Self {
        Self { out_dir }
    }
}

#[async_trait]
impl Service for DevDocumentsClient {
    async fn upload(&self, path: &Path) -> Result<Uuid> {
        let id = Uuid::new_v4();

        let file_name = path
            .file_name()
            .ok_or_else(|| Error::Context {
                message: "saving a scraped document locally".to_string(),
                source: Box::new(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{path:?} has no file name"),
                ))),
            })?
            .to_string_lossy();
        let dest = self.out_dir.join(strip_temp_uuid_prefix(&file_name));

        tokio::fs::create_dir_all(&self.out_dir)
            .await
            .with_context(|| format!("creating documents directory {}", self.out_dir.display()))?;
        tokio::fs::copy(path, &dest)
            .await
            .with_context(|| format!("copying scraped document to {}", dest.display()))?;

        tracing::info!(
            target: "fyde::scrapers",
            document = %id,
            path = %dest.display(),
            "document saved locally (dev)"
        );

        Ok(id)
    }

    async fn get(&self, _id: Uuid) -> Result<Option<Document>> {
        Ok(None)
    }

    async fn list(&self, _offset: i64, _limit: i64) -> Result<Vec<Document>> {
        Ok(Vec::new())
    }

    async fn update_metadata(&self, _metadata: Metadata) -> Result<()> {
        Ok(())
    }

    async fn run_scripts(&self, _document: &Document) -> Result<Metadata> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "run_scripts is not supported by the dev documents service",
        )
        .into())
    }

    async fn start_sync(&self) -> Result<()> {
        Ok(())
    }

    fn stop_sync(&self) {}
}

/// `fyde.save_document` (`host/documents.rs`) always hands [`Self::upload`]
/// a temp file named `"{uuid}-{leaf}"` — strips that prefix back off so
/// files land under `out_dir` under their original, human-readable name
/// instead of a random one.
fn strip_temp_uuid_prefix(file_name: &str) -> &str {
    if file_name.len() > 37
        && file_name.as_bytes()[36] == b'-'
        && Uuid::parse_str(&file_name[..36]).is_ok()
    {
        &file_name[37..]
    } else {
        file_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_temp_uuid_prefix_strips_a_valid_uuid_prefix() {
        let name = format!("{}-payslip.pdf", Uuid::new_v4());
        assert_eq!(strip_temp_uuid_prefix(&name), "payslip.pdf");
    }

    #[test]
    fn strip_temp_uuid_prefix_leaves_other_names_untouched() {
        assert_eq!(strip_temp_uuid_prefix("payslip.pdf"), "payslip.pdf");
        assert_eq!(
            strip_temp_uuid_prefix("not-a-uuid-prefix.pdf"),
            "not-a-uuid-prefix.pdf"
        );
    }

    #[tokio::test]
    async fn upload_copies_the_file_under_out_dir_stripping_the_temp_uuid_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("out");
        let client = DevDocumentsClient::new(out_dir.clone());

        let src_dir = tmp.path().join("src");
        tokio::fs::create_dir_all(&src_dir).await.unwrap();
        let src = src_dir.join(format!("{}-payslip.pdf", Uuid::new_v4()));
        tokio::fs::write(&src, b"the pdf content").await.unwrap();

        client.upload(&src).await.unwrap();

        assert_eq!(
            tokio::fs::read(out_dir.join("payslip.pdf")).await.unwrap(),
            b"the pdf content"
        );
    }
}
