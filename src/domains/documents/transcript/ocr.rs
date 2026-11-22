//! Reads the text of a page image with PP-OCRv5 mobile (text detection +
//! Latin recognition, Apache-2.0, ONNX) through `oar-ocr`, on the SDK's
//! ONNX Runtime: the [`Ocr`] adapter. Ported from the POC's
//! `pdf-ocr-core` (`fast_ocr.rs`), whose settings it keeps. Recognized lines
//! are laid out with the text layer's grid
//! ([`super::text_layer::layout_boxes`]). What to OCR is up to the caller.

use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use mockall::automock;

use async_trait::async_trait;
use oar_ocr::core::OCRError;
use oar_ocr::core::config::OrtSessionConfig;
use oar_ocr::domain::tasks::{TextDetectionConfig, TextRecognitionConfig};
use oar_ocr::oarocr::{OAROCR, OAROCRBuilder};
use oar_ocr::processors::LimitType;

use super::render::PageImage;
use super::text_layer::{TextBox, layout_boxes};
use crate::error::{ErrorContext, ErrorKind, Result};
use crate::tools::models::{LATIN_PP_OCRV5_MOBILE_REC, ModelStore, PP_OCRV5_MOBILE_DET};
use crate::tools::onnx;

/// The Latin recognition model's characters, one per line, from its
/// `inference.yml` at the pinned revision (`oar-ocr` adds the space).
const LATIN_DICT: &str = include_str!("ppocrv5_latin_dict.txt");

/// Text detection reads the page resized to at most this many pixels on its
/// longest side; recognition reads crops from the full-resolution page.
const DETECTION_MAX_SIDE: u32 = 1280;

/// Lines recognized with a lower confidence are dropped.
const MIN_CONFIDENCE: f32 = 0.5;

/// At most this many ONNX Runtime threads, like the other models.
const MAX_THREADS: usize = 4;

/// Reads the text of rendered pages.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Ocr: Send + Sync {
    /// Fetches the models (see [`ModelStore::ensure`]) and loads them,
    /// unless that's already done: a failure is retried on the next call.
    async fn load(&self) -> Result<()>;

    /// The text of `page`, laid out like it. Loads the models first if
    /// needed (see [`Ocr::load`]).
    async fn read(&self, page: PageImage) -> Result<String>;
}

/// The [`Ocr`] running PP-OCRv5 mobile. Its pipeline is built on the first
/// call that needs it, then kept for the reader's lifetime.
pub(crate) struct OnnxOcr {
    models: Arc<ModelStore>,
    threads: usize,
    loaded: tokio::sync::OnceCell<Arc<OAROCR>>,
}

impl OnnxOcr {
    pub(crate) fn new(models: Arc<ModelStore>) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get());

        Self {
            models,
            threads: (cores / 2).clamp(1, MAX_THREADS),
            loaded: tokio::sync::OnceCell::new(),
        }
    }

    async fn loaded(&self) -> Result<Arc<OAROCR>> {
        self.loaded
            .get_or_try_init(|| async {
                let detection = self
                    .models
                    .ensure(&PP_OCRV5_MOBILE_DET)
                    .await
                    .context("failed to fetch the OCR text detection model")?;
                let recognition = self
                    .models
                    .ensure(&LATIN_PP_OCRV5_MOBILE_REC)
                    .await
                    .context("failed to fetch the OCR text recognition model")?;
                let threads = self.threads;

                tokio::task::spawn_blocking(move || build(&detection, &recognition, threads))
                    .await
                    .context("the OCR model loading task failed")?
                    .map(Arc::new)
            })
            .await
            .cloned()
    }
}

#[async_trait]
impl Ocr for OnnxOcr {
    async fn load(&self) -> Result<()> {
        self.loaded().await.map(|_| ())
    }

    async fn read(&self, page: PageImage) -> Result<String> {
        let ocr = self.loaded().await?;

        tokio::task::spawn_blocking(move || read(&ocr, page))
            .await
            .context("the OCR task failed")?
            .context("failed to read a page with OCR")
    }
}

/// The OCR pipeline over the models at `detection` and `recognition`.
fn build(detection: &Path, recognition: &Path, threads: usize) -> Result<OAROCR> {
    onnx::load_runtime().context("failed to prepare ONNX Runtime for OCR")?;

    OAROCRBuilder::new(detection, recognition, "")
        .character_dict_content(LATIN_DICT)
        .text_detection_config(TextDetectionConfig {
            limit_side_len: Some(DETECTION_MAX_SIDE),
            limit_type: Some(LimitType::Max),
            ..TextDetectionConfig::default()
        })
        .text_recognition_config(TextRecognitionConfig {
            score_threshold: MIN_CONFIDENCE,
        })
        .ort_session(OrtSessionConfig::default().with_intra_threads(threads))
        .build()
        .map_err(ErrorKind::Ocr)
        .context("failed to load the OCR models")
}

/// `page`'s recognized lines, laid out.
fn read(ocr: &OAROCR, page: PageImage) -> Result<String> {
    let image = image::RgbImage::from_raw(page.width, page.height, page.rgb).ok_or_else(|| {
        ErrorKind::Ocr(OCRError::invalid_input(
            "the page image's size doesn't match its pixels",
        ))
    })?;
    let results = ocr
        .predict(vec![image])
        .map_err(ErrorKind::Ocr)
        .context("failed to run OCR")?;

    let boxes = results
        .into_iter()
        .flat_map(|result| result.text_regions)
        .filter_map(|region| {
            let text = region.text?.to_string();
            let points = &region.bounding_box.points;
            let xs = points.iter().map(|point| f64::from(point.x));
            let ys = points.iter().map(|point| f64::from(point.y));
            Some(TextBox {
                text,
                left: xs.clone().fold(f64::INFINITY, f64::min),
                right: xs.fold(f64::NEG_INFINITY, f64::max),
                top: ys.clone().fold(f64::INFINITY, f64::min),
                bottom: ys.fold(f64::NEG_INFINITY, f64::max),
            })
        })
        .collect();
    Ok(layout_boxes(boxes))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::render_page;
    use super::super::tests::{build_pdf_from, text_at};
    use super::*;

    /// The real models' store: `FYDE_TEST_MODELS_DIR` when set, to reuse a
    /// download across runs, else the returned temporary directory.
    fn real_models() -> (tempfile::TempDir, Arc<ModelStore>) {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        (temp, Arc::new(ModelStore::new(dir)))
    }

    #[tokio::test]
    #[ignore = "downloads 13 MB of models from Hugging Face"]
    async fn pp_ocrv5_reads_a_rendered_invoice_with_its_layout() {
        let (_temp, models) = real_models();
        let ocr = OnnxOcr::new(models);
        // Made up.
        let mut operations = text_at(80, 760, 16, "FACTURE N 2024-0193");
        operations.extend(text_at(
            80,
            720,
            11,
            "Electricite, periode du 1er au 31 mars",
        ));
        operations.extend(text_at(80, 690, 11, "Montant TTC"));
        operations.extend(text_at(420, 690, 11, "84,20 EUR"));
        let pdf = build_pdf_from(&[operations], 0);
        let page = render_page(&pdf, 0).unwrap().unwrap();

        let text = ocr.read(page).await.unwrap();

        let lines: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(
            lines[0].contains("FACTURE") && lines[0].contains("2024-0193"),
            "{text}"
        );
        // Courier's 1 and l look alike: only the words that don't.
        assert!(lines[1].starts_with("Electricite, periode du"), "{text}");
        assert!(lines[1].ends_with("mars"), "{text}");
        assert!(lines[2].starts_with("Montant TTC  "), "{text}");
        assert!(lines[2].trim_end().ends_with("84,20 EUR"), "{text}");
    }
}
