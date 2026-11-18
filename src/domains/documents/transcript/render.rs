//! Renders a PDF page to an RGB image with `hayro` (pure Rust), for OCR:
//! after the POC's `pdf-ocr-core`, at [`OCR_DPI`] and at most
//! [`OCR_MAX_SIDE`] pixels on its longest side, blank margins trimmed (they
//! only take resolution from the text detection, which resizes the whole
//! image). Only an adapter.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::page::Page;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

/// Plenty for PP-OCRv5's recognition, which reads crops at full resolution.
const OCR_DPI: f64 = 200.0;

/// Bounds a page's memory on a phone: an A4 page at [`OCR_DPI`] is
/// 1654 x 2339.
const OCR_MAX_SIDE: f64 = 2400.0;

/// "Ink" is clearly darker than paper: a looser threshold would count scan
/// noise and tinted paper, and nothing would ever be trimmed.
const INK_LUMINANCE: u32 = 190;

/// A row or column with fewer ink pixels than this is noise (dust, a scan
/// artifact), not content.
const MIN_INK_PIXELS: u32 = 2;

/// Kept around the inked area, in pixels.
const TRIM_PADDING: usize = 16;

/// A rendered page: tightly packed 8-bit RGB, row by row from the top.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PageImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgb: Vec<u8>,
}

impl std::fmt::Debug for PageImage {
    // Never the pixels: they're a document's content.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// `page` rendered for OCR, its blank margins trimmed; `None` for a page
/// with no ink at all (nothing to read) or no area.
pub(super) fn render_for_ocr(page: &Page<'_>) -> Option<PageImage> {
    let (width, height) = page.render_dimensions();
    let longest = f64::from(width.max(height));
    if longest <= 0.0 || !longest.is_finite() {
        return None;
    }
    let scale = (OCR_DPI / 72.0).min(OCR_MAX_SIDE / longest) as f32;

    let pixmap = render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (width, height) = (usize::from(pixmap.width()), usize::from(pixmap.height()));
    // Premultiplied, but over an opaque background: plain RGBA.
    let rgb: Vec<u8> = pixmap
        .data_as_u8_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[red, green, blue, _]| [red, green, blue])
        .collect();
    trim(width, height, &rgb)
}

/// The part of the `width` x `height` RGB image `rgb` holding ink, with
/// [`TRIM_PADDING`] around it; `None` for a blank image.
fn trim(width: usize, height: usize, rgb: &[u8]) -> Option<PageImage> {
    let mut rows = vec![0u32; height];
    let mut columns = vec![0u32; width];
    for (index, &[red, green, blue]) in rgb.as_chunks::<3>().0.iter().enumerate() {
        let luminance =
            (u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000;
        if luminance < INK_LUMINANCE {
            rows[index / width] += 1;
            columns[index % width] += 1;
        }
    }
    let inked = |counts: &[u32]| {
        let first = counts.iter().position(|&count| count >= MIN_INK_PIXELS)?;
        let last = counts.iter().rposition(|&count| count >= MIN_INK_PIXELS)?;
        Some((
            first.saturating_sub(TRIM_PADDING),
            (last + 1 + TRIM_PADDING).min(counts.len()),
        ))
    };
    let (top, bottom) = inked(&rows)?;
    let (left, right) = inked(&columns)?;

    let mut cropped = Vec::with_capacity((right - left) * (bottom - top) * 3);
    for row in top..bottom {
        cropped.extend_from_slice(&rgb[(row * width + left) * 3..(row * width + right) * 3]);
    }
    Some(PageImage {
        width: (right - left) as u32,
        height: (bottom - top) as u32,
        rgb: cropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white `width` x `height` image with black pixels at `ink`.
    fn image(width: usize, height: usize, ink: &[(usize, usize)]) -> Vec<u8> {
        let mut rgb = vec![255; width * height * 3];
        for &(x, y) in ink {
            rgb[(y * width + x) * 3..(y * width + x) * 3 + 3].fill(0);
        }
        rgb
    }

    #[test]
    fn trim_keeps_the_inked_area_and_some_padding() {
        let ink: Vec<(usize, usize)> = (50..60)
            .flat_map(|x| (100..105).map(move |y| (x, y)))
            .collect();

        let trimmed = trim(200, 300, &image(200, 300, &ink)).unwrap();

        assert_eq!(
            (trimmed.width, trimmed.height),
            (10 + 2 * TRIM_PADDING as u32, 5 + 2 * TRIM_PADDING as u32)
        );
        assert_eq!(
            trimmed.rgb.len(),
            (trimmed.width * trimmed.height * 3) as usize
        );
        // The ink's first pixel, past the padding.
        let first = (TRIM_PADDING * trimmed.width as usize + TRIM_PADDING) * 3;
        assert_eq!(&trimmed.rgb[first..first + 3], &[0, 0, 0]);
    }

    #[test]
    fn trim_ignores_stray_pixels() {
        let mut ink: Vec<(usize, usize)> = (50..60)
            .flat_map(|x| (100..105).map(move |y| (x, y)))
            .collect();
        // Dust in a corner.
        ink.push((2, 2));

        let trimmed = trim(200, 300, &image(200, 300, &ink)).unwrap();

        assert_eq!(trimmed.width, 10 + 2 * TRIM_PADDING as u32);
    }

    #[test]
    fn trim_of_a_blank_image_is_none() {
        assert!(trim(20, 30, &image(20, 30, &[])).is_none());
    }

    #[test]
    fn page_image_debug_leaves_the_pixels_out() {
        let image = PageImage {
            width: 1,
            height: 1,
            rgb: vec![1, 2, 3],
        };

        assert_eq!(
            format!("{image:?}"),
            "PageImage { width: 1, height: 1, .. }"
        );
    }
}
