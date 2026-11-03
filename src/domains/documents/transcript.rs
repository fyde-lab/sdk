use pdf_oxide::PdfDocument;
use pdf_oxide::converters::ConversionOptions;
pub(crate) use pdf_oxide::geometry::Rect;
use pdf_oxide::layout::RectFilterMode;

use crate::{ErrorContext as _, Result};

/// Extracts a raw text transcript of a PDF's text for storage alongside a
/// document's other encrypted metadata, so its contents can later be
/// searched without re-decrypting and re-parsing the document itself.
///
/// Callers are expected to have already verified `content` is a PDF (see
/// [`super::service::PDF_CONTENT_TYPE`]) — this always attempts to parse it as one.
#[tracing::instrument(level = "debug", skip_all, fields(size = content.len()))]
pub(crate) fn extract(content: &[u8]) -> Result<String> {
    let pdf = tracing::debug_span!("parse_pdf").in_scope(|| {
        PdfDocument::from_bytes(content.to_vec()).context("failed to parse PDF document")
    })?;
    let transcript = tracing::debug_span!("extract_text").in_scope(|| {
        pdf.to_plain_text_all(&ConversionOptions::default())
            .context("failed to extract text from PDF document")
    })?;

    Ok(transcript)
}

/// Extracts the text of the 0-based `page_index` page of a PDF that falls,
/// even partly, inside `region`: in PDF points (1/72 inch), from the page's
/// bottom-left corner, `y` growing upwards (`pdf_oxide`'s `Rect` docs say
/// "top-left", but its spans are in PDF user space: `y` 720-842 is the top
/// of an A4 page).
#[tracing::instrument(level = "debug", skip(content), fields(size = content.len()))]
pub(crate) fn extract_in_rect(content: &[u8], page_index: usize, region: Rect) -> Result<String> {
    let pdf = PdfDocument::from_bytes(content.to_vec()).context("failed to parse PDF document")?;
    pdf.extract_text_in_rect(page_index, region, RectFilterMode::Intersects)
        .with_context(|| format!("failed to extract text from page {page_index} of PDF document"))
}

#[cfg(test)]
pub(crate) mod tests {
    use lopdf::content::{Content, Operation};
    use lopdf::{Object, Stream, dictionary};

    use super::*;

    /// Builds a minimal, single-page PDF containing `text`, for use as
    /// fixture content in tests that need real (rather than hand-rolled)
    /// PDF bytes.
    pub(crate) fn build_pdf(text: &str) -> Vec<u8> {
        let mut doc = lopdf::Document::with_version("1.5");

        let pages_id = doc.new_object_id();

        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });

        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 48.into()]),
                Operation::new("Td", vec![100.into(), 600.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });

        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));

        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn extract_returns_an_error_for_malformed_pdf_content() {
        let result = extract(b"not a real pdf");

        assert!(result.is_err());
    }

    #[test]
    fn extract_returns_the_text_content_of_a_pdf() {
        let pdf = build_pdf("Hello World!");

        let transcript = extract(&pdf).unwrap();

        assert!(transcript.contains("Hello World!"));
    }

    // `build_pdf` writes its text at (100, 600), in 48pt.
    #[test]
    fn extract_in_rect_returns_the_text_inside_the_region() {
        let pdf = build_pdf("Hello World!");

        let text = extract_in_rect(&pdf, 0, Rect::new(90.0, 590.0, 500.0, 70.0)).unwrap();

        assert!(text.contains("Hello World!"));
    }

    #[test]
    fn extract_in_rect_leaves_out_the_text_outside_the_region() {
        let pdf = build_pdf("Hello World!");

        let text = extract_in_rect(&pdf, 0, Rect::new(0.0, 0.0, 595.0, 100.0)).unwrap();

        assert!(!text.contains("Hello"));
    }

    #[test]
    fn extract_in_rect_returns_an_error_for_a_page_the_pdf_does_not_have() {
        let pdf = build_pdf("Hello World!");

        let result = extract_in_rect(&pdf, 1, Rect::new(0.0, 0.0, 595.0, 842.0));

        assert!(result.is_err());
    }
}
