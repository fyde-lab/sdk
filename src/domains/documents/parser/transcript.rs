use pdf_oxide::PdfDocument;
use pdf_oxide::converters::ConversionOptions;

use crate::{ErrorContext as _, Result};

/// Extracts a raw text transcript of a PDF's text for storage alongside a
/// document's other encrypted metadata, so its contents can later be
/// searched without re-decrypting and re-parsing the document itself.
///
/// Callers are expected to have already verified `content` is a PDF (see
/// [`super::PDF_CONTENT_TYPE`]) — this always attempts to parse it as one.
pub(super) fn extract(content: &[u8]) -> Result<String> {
    let pdf = PdfDocument::from_bytes(content.to_vec()).context("failed to parse PDF document")?;
    let transcript = pdf
        .to_plain_text_all(&ConversionOptions::default())
        .context("failed to extract text from PDF document")?;

    Ok(transcript)
}

#[cfg(test)]
pub(super) mod tests {
    use lopdf::content::{Content, Operation};
    use lopdf::{Object, Stream, dictionary};

    use super::*;

    /// Builds a minimal, single-page PDF containing `text`, for use as
    /// fixture content in tests that need real (rather than hand-rolled)
    /// PDF bytes.
    pub(in super::super) fn build_pdf(text: &str) -> Vec<u8> {
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
}
