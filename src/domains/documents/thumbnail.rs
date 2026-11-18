//! A PDF's first page as a PNG, rendered with `hayro` (pure Rust) to fit a
//! box, its aspect ratio kept, on a white background. Only an adapter: the
//! box's bounds are the documents service's.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::util::TransformExt as _;
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings, render_into};
use image::ImageEncoder as _;
use image::codecs::png::PngEncoder;

use super::Thumbnail;
use super::transcript;
use crate::{ErrorContext as _, ErrorKind, Result};

/// The first page of the PDF in `content`, as large as fits within
/// `max_width` x `max_height` pixels (each at least 1, and small enough to
/// render: see `MAX_THUMBNAIL_SIDE`), never less than a pixel a side.
pub(super) fn render(content: &[u8], max_width: u32, max_height: u32) -> Result<Thumbnail> {
    let pdf = transcript::parse(content).context("failed to parse PDF document")?;
    let page = pdf
        .pages()
        .first()
        .ok_or(ErrorKind::Pdf("it has no page"))
        .context("failed to find its first page")?;

    let rgb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (page_width, page_height) = page.render_dimensions();
        let (page_width, page_height) = (f64::from(page_width), f64::from(page_height));
        if !(page_width > 0.0 && page_height > 0.0) {
            return Err(ErrorKind::Pdf("its first page has no area"));
        }
        let scale = (f64::from(max_width) / page_width).min(f64::from(max_height) / page_height);
        let side = |points: f64, max: u32| ((points * scale).round() as u32).clamp(1, max) as u16;
        let (width, height) = (side(page_width, max_width), side(page_height, max_height));

        let mut context = RenderContext::new(width, height);
        render_into(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &mut context,
            Affine::scale(scale) * page.initial_transform(true).to_kurbo(),
        );
        context.flush();
        let mut pixmap = Pixmap::new(width, height);
        context.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..Default::default()
            },
        );
        // Premultiplied, but over an opaque background: plain RGBA.
        let rgb: Vec<u8> = pixmap
            .data_as_u8_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[red, green, blue, _]| [red, green, blue])
            .collect();
        Ok((u32::from(width), u32::from(height), rgb))
    }))
    .map_err(|_| ErrorKind::Pdf("rendering it panicked"))
    .and_then(|rendered| rendered)
    .context("failed to render its first page")?;
    let (width, height, rgb) = rgb;

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(ErrorKind::ImageEncoding)
        .context("failed to encode the thumbnail as PNG")?;
    Ok(Thumbnail { png, width, height })
}

#[cfg(test)]
mod tests {
    use super::super::transcript::tests::{build_pdf, build_pdf_from, text_at};
    use super::*;
    use crate::ErrorCode;

    /// `thumbnail`'s pixels, decoded.
    fn decode(thumbnail: &Thumbnail) -> image::RgbImage {
        let image = image::load_from_memory_with_format(&thumbnail.png, image::ImageFormat::Png)
            .unwrap()
            .to_rgb8();
        assert_eq!(
            (image.width(), image.height()),
            (thumbnail.width, thumbnail.height)
        );
        image
    }

    #[test]
    fn render_fits_an_a4_page_in_the_box_keeping_its_ratio() {
        let thumbnail = render(&build_pdf("Hello World!"), 200, 200).unwrap();

        // 595 x 842 points.
        assert_eq!((thumbnail.width, thumbnail.height), (141, 200));
        let image = decode(&thumbnail);
        assert!(
            image.pixels().any(|pixel| pixel.0[0] < 100),
            "no text drawn"
        );
        assert_eq!(image.get_pixel(0, 0).0, [255, 255, 255]);
    }

    #[test]
    fn render_fits_a_wide_box_by_its_height() {
        let thumbnail = render(&build_pdf("Hello"), 1000, 100).unwrap();

        assert_eq!((thumbnail.width, thumbnail.height), (71, 100));
    }

    #[test]
    fn render_draws_a_rotated_page_as_shown() {
        let pdf = build_pdf_from(&[text_at(100, 700, 12, "Landscape")], 90);

        let thumbnail = render(&pdf, 200, 200).unwrap();

        assert_eq!((thumbnail.width, thumbnail.height), (200, 141));
    }

    #[test]
    fn render_draws_only_the_first_page() {
        let blank_first = build_pdf_from(&[Vec::new(), text_at(100, 700, 48, "Second")], 0);

        let image = decode(&render(&blank_first, 100, 100).unwrap());

        assert!(image.pixels().all(|pixel| pixel.0 == [255, 255, 255]));
    }

    #[test]
    fn render_fails_as_an_unreadable_pdf_on_other_content() {
        let err = render(b"not a pdf", 100, 100).unwrap_err();

        assert_eq!(err.code(), ErrorCode::UnreadablePdf);
    }
}
