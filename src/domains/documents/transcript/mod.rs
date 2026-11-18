//! A PDF's text transcript, stored in its encrypted metadata so it can be
//! searched and embedded without re-parsing the document. Each page's own
//! text layer, laid out like the page ([`text_layer`]), pages separated by
//! [`PAGE_SEPARATOR`].

mod text_layer;

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::hayro_syntax::{LoadPdfError, Pdf};

use crate::{ErrorContext as _, ErrorKind, Result};

/// What [`extract`] puts between two pages.
pub(super) const PAGE_SEPARATOR: &str = "\n\n---\n\n";

/// The transcript of the PDF in `content`: every page's text layer, in
/// order, joined by [`PAGE_SEPARATOR`]. A page without one (a scan) is
/// empty.
///
/// Callers are expected to have already verified `content` is a PDF (see
/// [`super::service::PDF_CONTENT_TYPE`]) — this always attempts to parse it as one.
#[tracing::instrument(level = "debug", skip_all, fields(size = content.len()))]
pub(crate) fn extract(content: &[u8]) -> Result<String> {
    let pdf = tracing::debug_span!("parse_pdf")
        .in_scope(|| parse(content))
        .context("failed to parse PDF document")?;
    tracing::debug_span!("extract_text", pages = pdf.pages().len())
        .in_scope(|| {
            // `hayro` reads untrusted bytes: a bug of its must fail this
            // document's upload, not unwind through the caller.
            catch_unwind(AssertUnwindSafe(|| {
                pdf.pages()
                    .iter()
                    .map(text_layer::page_text)
                    .collect::<Vec<_>>()
                    .join(PAGE_SEPARATOR)
            }))
            .map_err(|_| ErrorKind::Pdf("reading its text layer panicked"))
        })
        .context("failed to extract text from PDF document")
}

/// Parses `content` (decrypting it if it's only protected by an owner
/// password, as bank statements often are).
fn parse(content: &[u8]) -> Result<Pdf> {
    catch_unwind(|| Pdf::new(content.to_vec()))
        .map_err(|_| ErrorKind::Pdf("parsing it panicked"))?
        .map_err(|err| {
            ErrorKind::Pdf(match err {
                LoadPdfError::Decryption(_) => "it's encrypted with a password",
                LoadPdfError::Invalid => "it's malformed",
            })
            .into()
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use lopdf::content::{Content, Operation};
    use lopdf::{Object, Stream, dictionary};

    use super::*;
    use crate::ErrorCode;

    /// Builds a minimal, single-page PDF containing `text`, for use as
    /// fixture content in tests that need real (rather than hand-rolled)
    /// PDF bytes.
    pub(crate) fn build_pdf(text: &str) -> Vec<u8> {
        build_pdf_pages(&[text])
    }

    /// Builds a minimal PDF with one page per element of `pages`, each
    /// containing that text.
    pub(crate) fn build_pdf_pages(pages: &[&str]) -> Vec<u8> {
        let pages: Vec<Vec<Operation>> = pages
            .iter()
            .map(|text| text_at(100, 600, 48, text))
            .collect();
        build_pdf_from(&pages, 0)
    }

    /// The operations writing `text` at (`x`, `y`) (PDF points, from the
    /// bottom-left corner) in `size`-point Courier.
    fn text_at(x: i64, y: i64, size: i64, text: &str) -> Vec<Operation> {
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), size.into()]),
            Operation::new("Td", vec![x.into(), y.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ]
    }

    /// Builds an A4 PDF with one page per element of `pages`, each drawn by
    /// those operations (`F1` is Courier) and rotated by `rotate` degrees.
    fn build_pdf_from(pages: &[Vec<Operation>], rotate: i64) -> Vec<u8> {
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

        let mut kids = Vec::new();
        for operations in pages {
            let content = Content {
                operations: operations.clone(),
            };
            let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Rotate" => rotate,
            });
            kids.push(page_id.into());
        }

        let pages = dictionary! {
            "Type" => "Pages",
            "Count" => kids.len() as i64,
            "Kids" => kids,
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
    fn extract_fails_as_an_unreadable_pdf_on_malformed_content() {
        let err = extract(b"not a real pdf").unwrap_err();

        assert_eq!(err.code(), ErrorCode::UnreadablePdf);
    }

    #[test]
    fn extract_returns_the_text_content_of_a_pdf() {
        let pdf = build_pdf("Hello World!");

        let transcript = extract(&pdf).unwrap();

        assert_eq!(transcript, "Hello World!");
    }

    #[test]
    fn extract_separates_pages() {
        let pdf = build_pdf_pages(&["one", "two", "three"]);

        let transcript = extract(&pdf).unwrap();

        assert_eq!(transcript, "one\n\n---\n\ntwo\n\n---\n\nthree");
    }

    #[test]
    fn extract_lays_out_lines_top_to_bottom() {
        let mut operations = text_at(100, 700, 12, "Facture n 42");
        operations.extend(text_at(100, 680, 12, "Total"));
        operations.extend(text_at(400, 680, 12, "15,62"));

        let transcript = extract(&build_pdf_from(&[operations], 0)).unwrap();

        let lines: Vec<&str> = transcript.lines().collect();
        assert_eq!(lines.len(), 2, "{transcript:?}");
        assert_eq!(lines[0], "Facture n 42");
        assert!(lines[1].starts_with("Total  ") && lines[1].ends_with("15,62"));
    }

    #[test]
    fn extract_reads_a_rotated_page_upright() {
        let mut operations = text_at(100, 700, 12, "first line");
        operations.extend(text_at(100, 680, 12, "second line"));

        let transcript = extract(&build_pdf_from(&[operations], 90)).unwrap();

        assert_eq!(transcript, "first line\nsecond line");
    }

    #[test]
    fn extract_puts_vertical_margin_text_after_the_body() {
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 8.into()]),
            // Rotated a quarter turn: reads bottom to top, along the margin.
            Operation::new(
                "Tm",
                vec![
                    0.into(),
                    1.into(),
                    (-1).into(),
                    0.into(),
                    30.into(),
                    100.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("SAS au capital")]),
            Operation::new("ET", vec![]),
        ];
        operations.extend(text_at(100, 700, 12, "The body of the page"));

        let transcript = extract(&build_pdf_from(&[operations], 0)).unwrap();

        assert_eq!(transcript, "The body of the page\n\nSAS au capital");
    }

    #[test]
    fn extract_reads_invisible_text() {
        let operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            // A scan's OCR layer is drawn invisible.
            Operation::new("Tr", vec![3.into()]),
            Operation::new("Td", vec![100.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal("hidden words")]),
            Operation::new("ET", vec![]),
        ];

        let transcript = extract(&build_pdf_from(&[operations], 0)).unwrap();

        assert_eq!(transcript, "hidden words");
    }

    #[test]
    fn extract_of_a_page_without_text_is_empty() {
        let transcript = extract(&build_pdf_from(&[Vec::new()], 0)).unwrap();

        assert_eq!(transcript, "");
    }
}
