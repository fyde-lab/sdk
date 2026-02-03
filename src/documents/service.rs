use super::AUTHORIZED_MIME_TYPES;
use super::storage;
use super::{Document, Metadata};
use chrono::Utc;
use file_type::FileType;
use mockall::predicate::*;
use mockall::*;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{RenderSettings, render};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;
use vello_cpu::color::palette::css::WHITE;

const MAX_LIMIT: u64 = 100;
const DEFAULTLIMIT: u64 = 20;

const SCALE: f32 = 1.0;

#[automock]
pub trait Service: Send + Sync {
    fn save_file_from_path(&self, path: &Path) -> Result<Document, Error>;
    fn get_all<'a>(&self, cmd: &GetAllCmd<'a>) -> Result<Vec<Metadata>, Error>;
    fn get_by_id(&self, doc_id: &Uuid) -> Result<Metadata, Error>;
    fn get_preview(&self, meta: &Metadata) -> Result<Box<[u8]>, Error>;
    fn get_content(&self, meta: &Metadata) -> Result<Box<[u8]>, Error>;
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to access a file: {0}")]
    FileAccessError(#[from] io::Error),

    #[error("invalid file format detected: {file_type}")]
    InvalidFileFormat { file_type: String },

    #[error("storage failure: {0}")]
    StorageError(#[from] storage::Error),

    #[error("the saved size is different than the one fetched")]
    InvalidSize(),

    #[error("the given path doesn't point to a file")]
    PathIsDir(),

    #[error("failed to extract the text from the pdf: {0}")]
    ExtractTextError(#[source] lopdf::Error),
}

pub struct GetAllCmd<'a> {
    pub after: Option<&'a Metadata>,
    pub limit: Option<u64>,
}

pub struct Svc {
    storage: Arc<Box<dyn storage::Storage>>,
}

impl Svc {
    pub fn new(storage: Box<dyn storage::Storage>) -> Self {
        Self {
            storage: Arc::new(storage),
        }
    }
}

impl Service for Svc {
    fn save_file_from_path(&self, path: &Path) -> Result<Document, Error> {
        tracing::debug!("Start saving file form path: {0}", path.to_string_lossy());

        if path.is_dir() {
            return Err(Error::PathIsDir());
        }

        let mut file = File::open(path).map_err(Error::FileAccessError)?;
        let file_metadatas = file.metadata().map_err(Error::FileAccessError)?;

        let mut file_content = Vec::with_capacity(file_metadatas.size() as usize);
        let _ = file.read_to_end(&mut file_content)?;

        let hash = Sha256::digest(&file_content);

        let raw_preview: Vec<u8>;
        {
            let pdf = Pdf::new(Arc::new(file_content.clone())).unwrap();

            let interpreter_settings = InterpreterSettings::default();

            let render_settings = RenderSettings {
                x_scale: SCALE,
                y_scale: SCALE,
                bg_color: WHITE,
                ..Default::default()
            };

            let page = pdf.pages().first().unwrap();
            let pixmap = render(page, &interpreter_settings, &render_settings);
            raw_preview = pixmap.into_png().unwrap();
        }

        let transcript: String;
        {
            let doc = lopdf::Document::load_mem(&file_content.clone())
                .map_err(Error::ExtractTextError)?;

            let pages = doc.get_pages();
            let page_numbers: Vec<u32> = pages.keys().cloned().collect();
            transcript = doc
                .extract_text(&page_numbers)
                .map_err(Error::ExtractTextError)?;
        }

        let file_type = FileType::from_bytes(&file_content)
            .media_types()
            .first()
            .ok_or(Error::InvalidFileFormat {
                file_type: String::from("unknown"),
            })?;

        if !AUTHORIZED_MIME_TYPES.contains(file_type) {
            Error::InvalidFileFormat {
                file_type: file_type.to_string(),
            };
        }

        if file_content.len() != file_metadatas.size() as usize {
            return Err(Error::InvalidSize());
        }

        let doc = Document {
            metadata: Metadata {
                id: Uuid::now_v7(),
                name: path
                    .file_name()
                    .ok_or(Error::PathIsDir())?
                    .to_string_lossy()
                    .to_string(),
                checksum: format!("{:x}", hash),
                detected_type: file_type.to_lowercase(),
                size: file_metadatas.size(),
                created_at: Utc::now(),
                transcript: Some(transcript),
            },
            file_content: file_content.into_boxed_slice(),
            file_preview: raw_preview.into_boxed_slice(),
        };

        self.storage.as_ref().save(&doc)?;

        Ok(doc)
    }

    fn get_all(&self, cmd: &GetAllCmd) -> Result<Vec<Metadata>, Error> {
        self.storage
            .get_all(&storage::GetAllCmd {
                after_id: cmd.after.map(|v| &v.id),
                limit: cmd.limit.unwrap_or(DEFAULTLIMIT).min(MAX_LIMIT),
            })
            .map_err(Error::StorageError)
    }

    fn get_preview(&self, meta: &Metadata) -> Result<Box<[u8]>, Error> {
        Ok(self.storage.get_preview(&meta.id)?)
    }

    fn get_content(&self, meta: &Metadata) -> Result<Box<[u8]>, Error> {
        Ok(self.storage.get_content(&meta.id)?)
    }

    fn get_by_id(&self, doc_id: &Uuid) -> Result<Metadata, Error> {
        Ok(self.storage.get_by_id(doc_id)?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::storage::MockStorage;
    use super::*;
    use std::path::Path;

    #[test]
    fn save_file_from_path_success() {
        let mut storage = MockStorage::new();

        storage.expect_save().returning(|_| Ok(())).once();

        let svc = Svc::new(Box::new(storage));

        let _ = svc
            .save_file_from_path(Path::new("./fixtures/basic-text.pdf"))
            .unwrap();
    }

    #[test]
    fn save_file_from_path_with_file_not_found_should_failed() {
        let storage = MockStorage::new();

        let svc = Svc::new(Box::new(storage));

        let res = svc.save_file_from_path(Path::new("./some-invalid-path"));

        assert!(
            res.unwrap_err()
                .to_string()
                .contains("failed to access a file")
        );
    }
}
