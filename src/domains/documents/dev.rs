use std::path::PathBuf;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::{Document, Metadata, Service, UploadRequest, UploadSource};

/// A [`Service`] implementation for standalone/dev use (see
/// [`crate::init_dev_scrapers`]): instead of encrypting and uploading
/// through the changelog, [`Self::upload`] just writes the content as plain
/// bytes under `out_dir/scraper_name`, the way `demo-rust-fyde` wrote
/// scraped documents straight to `--out-dir` — scoped to a subfolder per
/// scraper here so documents from different scrapers never collide or mix
/// together under the same `out_dir`. There is no local cache and no server
/// connection, so every other [`Service`] method is a no-op — nothing in
/// the scraper flow (`fyde.save_document` → [`Service::upload`] only) calls
/// them.
pub(crate) struct DevDocumentsClient {
    out_dir: PathBuf,
    scraper_name: String,
}

impl DevDocumentsClient {
    pub(crate) fn new(out_dir: PathBuf, scraper_name: impl Into<String>) -> Self {
        Self {
            out_dir,
            scraper_name: scraper_name.into(),
        }
    }
}

#[async_trait]
impl Service for DevDocumentsClient {
    async fn upload(&self, request: UploadRequest) -> Result<Uuid> {
        let id = Uuid::new_v4();

        let (name, content) = match request.source {
            UploadSource::Path(path) => {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                let content = tokio::fs::read(&path)
                    .await
                    .with_context(|| format!("reading scraped document at {}", path.display()))?;
                (name, content)
            }
            UploadSource::Raw { name, content } => (name, content),
        };
        let dir = self.out_dir.join(&self.scraper_name);
        let dest = dir.join(name);

        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating documents directory {}", dir.display()))?;
        tokio::fs::write(&dest, &content)
            .await
            .with_context(|| format!("writing scraped document to {}", dest.display()))?;

        tracing::debug!(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upload_writes_raw_content_under_out_dir_scraper_name_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("out");
        let client = DevDocumentsClient::new(out_dir.clone(), "didaxis");

        client
            .upload(UploadRequest::from_raw(
                "payslip.pdf",
                b"the pdf content".to_vec(),
            ))
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read(out_dir.join("didaxis").join("payslip.pdf"))
                .await
                .unwrap(),
            b"the pdf content"
        );
    }

    #[tokio::test]
    async fn upload_copies_the_file_at_path_under_out_dir_scraper_name_by_its_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("out");
        let client = DevDocumentsClient::new(out_dir.clone(), "impots");

        let src_dir = tmp.path().join("src");
        tokio::fs::create_dir_all(&src_dir).await.unwrap();
        let src = src_dir.join("payslip.pdf");
        tokio::fs::write(&src, b"the pdf content").await.unwrap();

        client.upload(UploadRequest::from_path(&src)).await.unwrap();

        assert_eq!(
            tokio::fs::read(out_dir.join("impots").join("payslip.pdf"))
                .await
                .unwrap(),
            b"the pdf content"
        );
    }
}
