use std::path::PathBuf;

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::{
    Document, Metadata, Purpose, Service, SourceCategory, SourceSubCategory, UploadRequest,
    UploadSource,
};

/// A [`Service`] implementation for standalone/dev use (see
/// [`crate::init_dev_scrapers`]): instead of encrypting and uploading
/// through the changelog, [`Self::upload`] just writes the content as plain
/// bytes under `out_dir/scraper_name` (alongside a [`DevMetadata`] JSON file
/// with the same name, `.json` extension), the way `demo-rust-fyde` wrote
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

/// The metadata [`DevDocumentsClient::upload`] writes as JSON next to each
/// saved document: only what the caller set on the [`UploadRequest`], since
/// there is no parser (and so no transcript or script-derived fields) here.
#[derive(Debug, Serialize)]
struct DevMetadata<'a> {
    id: Uuid,
    original_name: &'a str,
    name: Option<&'a str>,
    r#type: Option<&'a str>,
    source_category: Option<SourceCategory>,
    source_sub_category: Option<SourceSubCategory>,
    subjects: Option<&'a [String]>,
    purpose: Option<Purpose>,
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
        let metadata = DevMetadata {
            id,
            original_name: &name,
            name: request.name.as_deref(),
            r#type: request.r#type.as_deref(),
            source_category: request.source_category,
            source_sub_category: request.source_sub_category,
            subjects: request.subjects.as_deref(),
            purpose: request.purpose,
        };
        let metadata = serde_json::to_vec_pretty(&metadata)
            .with_context(|| format!("serializing metadata for scraped document {name}"))?;

        let dir = self.out_dir.join(&self.scraper_name);
        let dest = dir.join(&name);
        let metadata_dest = dest.with_extension("json");

        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating documents directory {}", dir.display()))?;
        tokio::fs::write(&dest, &content)
            .await
            .with_context(|| format!("writing scraped document to {}", dest.display()))?;
        tokio::fs::write(&metadata_dest, &metadata)
            .await
            .with_context(|| {
                format!(
                    "writing scraped document metadata to {}",
                    metadata_dest.display()
                )
            })?;

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

    #[tokio::test]
    async fn upload_writes_the_request_metadata_as_json_next_to_the_document() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("out");
        let client = DevDocumentsClient::new(out_dir.clone(), "didaxis");

        let mut request = UploadRequest::from_raw("payslip.pdf", b"the pdf content".to_vec());
        request.name = Some("Payslip January".to_string());
        request.source_category = Some(SourceCategory::Employer);
        request.subjects = Some(vec!["alice".to_string()]);
        let id = client.upload(request).await.unwrap();

        let json: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(out_dir.join("didaxis").join("payslip.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": id,
                "original_name": "payslip.pdf",
                "name": "Payslip January",
                "type": null,
                "source_category": "employer",
                "source_sub_category": null,
                "subjects": ["alice"],
                "purpose": null,
            })
        );
    }
}
