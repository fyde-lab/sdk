//! A PDF page's own text layer, laid out like the page, through `hayro`'s
//! interpreter (pure Rust): a [`Device`] records every glyph it's asked to
//! draw (its Unicode text, position, size and advance) instead of drawing it,
//! invisible text included (a scan's OCR layer), glyphs drawn off the page
//! left out. Rebuilt after `pdf-ocr-core`'s `text_layer.rs`
//! (`paddleocr-vl-cli`, the POC `doc-ai-poc` used):
//!
//! 1. Reading frames: each glyph's baseline direction is snapped to a quarter
//!    turn and laid out in its own frame, so a rotated page reads normally and
//!    vertical margin text (legal mentions) comes out after the page's body.
//! 2. Lines: glyphs sharing a baseline, anchored on the line's largest glyph,
//!    superscripts and subscripts (n°, m²) attached to their line.
//! 3. Chunks: runs of glyphs separated by at most a word space
//!    ("Prix unitaire HT", "15,62 €").
//! 4. Grid: chunks are placed on a character grid (a column is the median
//!    glyph width); a chunk whose right edge lines up with chunks on other
//!    lines (and whose left edge doesn't) is right-aligned, like amount
//!    columns, the others left-aligned. Vertical gaps become blank lines (at
//!    most 2), which `chunk.rs` reads as paragraph breaks.
//!
//! [`layout_boxes`] lays OCR'd lines out with the same grid, so a scan keeps
//! its layout too. Only an adapter: [`PageText::is_usable`] measures the text
//! layer, deciding what to do with a page is up to the caller.

use std::cmp::Ordering;
use std::f64::consts::FRAC_PI_2;

use hayro::hayro_interpret::font::{Glyph as PdfGlyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::kurbo::{Affine, BezPath, Point, Rect};
use hayro::hayro_interpret::util::TransformExt as _;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};
use hayro::hayro_syntax::page::Page;

/// Glyph space is 1000 units per em in `hayro`, whatever the font.
const UNITS_PER_EM: f64 = 1000.0;

/// A glyph without an advance width (a Type 1 font with neither widths nor
/// standard metrics) is taken to be this wide, in ems.
const FALLBACK_ADVANCE: f64 = 0.5;

/// Glyphs smaller than this (in points) are noise: hidden text, or a
/// degenerate text matrix.
const MIN_GLYPH_SIZE: f64 = 0.5;

/// A gap between two glyphs wider than this (in ems of the line) is a word
/// space, even without a space glyph.
const WORD_GAP: f64 = 0.15;

/// A gap wider than this (in ems) splits a line into two chunks, each placed
/// on the grid on its own: wider than any justified word space.
const CHUNK_GAP: f64 = 1.2;

/// A glyph drawn again within this distance (in ems) of the same glyph is a
/// duplicate (fake bold, a shadow) and dropped.
const DUPLICATE_DISTANCE: f64 = 0.15;

/// The grid's column is at least this wide (in ems of the frame's letters):
/// narrower than any real font's letters, so it only matters for a font whose
/// widths are wrong, which would otherwise pad a line with millions of spaces.
const MIN_COLUMN_WIDTH: f64 = 0.2;

/// Most blank lines a vertical gap becomes.
const MAX_BLANK_LINES: usize = 2;

/// A text layer with fewer letters and digits than this has no real content
/// (a page number over a scan).
const MIN_USABLE_LETTERS: usize = 20;

/// A text layer with at least one glyph in this many mapping to no readable
/// character is garbage (a font without a Unicode mapping).
const MAX_UNREADABLE_RATIO: usize = 20;

/// A page's text layer, and how much of it could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageText {
    pub(crate) text: String,
    /// Letters and digits in [`Self::text`].
    pub(crate) letters: usize,
    /// Glyphs drawn, spaces aside, readable or not.
    pub(crate) glyphs: usize,
    /// Glyphs mapping to no readable character, left out of the text.
    pub(crate) unreadable: usize,
}

impl PageText {
    /// Whether the text layer is the page's text: it has real content
    /// (at least [`MIN_USABLE_LETTERS`] letters or digits) and fewer than
    /// one glyph in [`MAX_UNREADABLE_RATIO`] is unreadable. A scan, or a
    /// page whose fonts map to nothing, isn't.
    pub(crate) fn is_usable(&self) -> bool {
        self.letters >= MIN_USABLE_LETTERS && self.unreadable * MAX_UNREADABLE_RATIO < self.glyphs
    }
}

/// The text layer of `page`, laid out as described in this module's docs.
pub(super) fn page_text(page: &Page<'_>) -> PageText {
    let collector = collect_glyphs(page);
    let text = frames_text(collector.glyphs);
    PageText {
        letters: text.chars().filter(|c| c.is_alphanumeric()).count(),
        text,
        glyphs: collector.drawn,
        unreadable: collector.unreadable,
    }
}

/// A line of text an OCR engine found on a page image, in pixels from its
/// top-left corner.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextBox {
    pub(crate) text: String,
    pub(crate) left: f64,
    pub(crate) top: f64,
    pub(crate) right: f64,
    pub(crate) bottom: f64,
}

/// OCR'd lines laid out with the same grid as a text layer: each box is
/// taken as one run of glyphs as tall as the box, its baseline at its
/// bottom.
pub(crate) fn layout_boxes(boxes: Vec<TextBox>) -> String {
    let glyphs = boxes
        .into_iter()
        .filter_map(|text_box| {
            let text = text_box.text.trim();
            let size = text_box.bottom - text_box.top;
            (!text.is_empty() && size > 0.0 && text_box.right > text_box.left).then(|| Glyph {
                text: text.to_string(),
                turn: 0,
                x: text_box.left,
                end: text_box.right,
                baseline: text_box.bottom,
                size,
            })
        })
        .collect();
    layout(glyphs)
}

/// `glyphs`' text, each reading frame laid out on its own.
fn frames_text(glyphs: Vec<Glyph>) -> String {
    let mut frames: [Vec<Glyph>; 4] = Default::default();
    for glyph in glyphs {
        frames[usize::from(glyph.turn)].push(glyph);
    }

    // The frame most glyphs read in is the page's body; the others (margin
    // text, rotated labels) follow it.
    let mut order: Vec<usize> = (0..frames.len()).collect();
    order.sort_by_key(|&turn| std::cmp::Reverse(frames[turn].len()));

    order
        .into_iter()
        .map(|turn| layout(std::mem::take(&mut frames[turn])))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One glyph, in its reading frame: `x` grows along the baseline, `baseline`
/// grows towards the next line. Points throughout.
#[derive(Debug, Clone)]
struct Glyph {
    /// Never empty. Whitespace-only for a space glyph.
    text: String,
    /// How many quarter turns (clockwise on the page) its baseline is from
    /// left-to-right.
    turn: u8,
    x: f64,
    end: f64,
    baseline: f64,
    /// Its em, in points.
    size: f64,
}

impl Glyph {
    fn is_space(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }
}

/// Runs `page` through `hayro`'s interpreter, recording its glyphs.
fn collect_glyphs(page: &Page<'_>) -> Collector {
    let (width, height) = page.render_dimensions();
    let bounds = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        // Top-left origin, `y` down, the page's `/Rotate` applied.
        page.initial_transform(true).to_kurbo(),
        bounds,
        &cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    let mut collector = Collector {
        bounds,
        glyphs: Vec::new(),
        drawn: 0,
        unreadable: 0,
    };
    interpret_page(page, &mut context, &mut collector);
    collector
}

struct Collector {
    /// The page, in the space glyphs are drawn in.
    bounds: Rect,
    glyphs: Vec<Glyph>,
    /// Glyphs drawn, spaces aside, readable or not.
    drawn: usize,
    /// Glyphs left out for mapping to no readable character.
    unreadable: usize,
}

impl<'a> Device<'a> for Collector {
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for positioned in run.glyphs() {
            let text = match readable_text(positioned.as_unicode()) {
                Readable::Text(text) => text,
                Readable::Dropped => continue,
                Readable::Unreadable => {
                    self.drawn += 1;
                    self.unreadable += 1;
                    continue;
                }
            };
            if !text.chars().all(char::is_whitespace) {
                self.drawn += 1;
            }
            // Both in glyph space (`hayro` scales a Type 3 font's widths by
            // its font matrix).
            let advance = match &**positioned {
                PdfGlyph::Outline(outline) => outline.advance_width(),
                PdfGlyph::Type3(glyph) => glyph.advance_width(),
            }
            .map(f64::from)
            .unwrap_or(FALLBACK_ADVANCE * UNITS_PER_EM);
            if let Some(glyph) = place(
                text,
                props.transform * positioned.transform(),
                advance,
                self.bounds,
            ) {
                self.glyphs.push(glyph);
            }
        }
    }

    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

/// What a glyph's Unicode mapping gives the transcript.
#[derive(Debug, PartialEq, Eq)]
enum Readable {
    Text(String),
    /// Nothing, on purpose (a soft hyphen).
    Dropped,
    /// No readable character: no Unicode mapping, a private-use code point,
    /// a replacement or control character. Left out rather than printed as
    /// garbage.
    Unreadable,
}

/// A glyph's text as it goes in the transcript. Ligatures are spelled out,
/// soft hyphens dropped.
fn readable_text(unicode: Option<BfString>) -> Readable {
    let raw = match unicode {
        Some(BfString::Char(c)) => c.to_string(),
        Some(BfString::String(s)) => s,
        None => return Readable::Unreadable,
    };
    let mut text = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\u{00AD}' => {}
            '\u{FB00}' => text.push_str("ff"),
            '\u{FB01}' => text.push_str("fi"),
            '\u{FB02}' => text.push_str("fl"),
            '\u{FB03}' => text.push_str("ffi"),
            '\u{FB04}' => text.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => text.push_str("st"),
            c if c.is_whitespace() => text.push(' '),
            '\u{FFFD}' | '\u{E000}'..='\u{F8FF}' => return Readable::Unreadable,
            c if c.is_control() => return Readable::Unreadable,
            c => text.push(c),
        }
    }
    if text.is_empty() {
        Readable::Dropped
    } else {
        Readable::Text(text)
    }
}

/// Places a glyph drawn with `transform` (glyph space to page space) and
/// `advance` (glyph space units) in its reading frame; `None` for a glyph too
/// small to read or whose origin is more than an em off the page `bounds`
/// (it isn't shown, and laying it out would pad its line up to it).
fn place(text: String, transform: Affine, advance: f64, bounds: Rect) -> Option<Glyph> {
    let origin = transform * Point::ZERO;
    let along = transform * Point::new(UNITS_PER_EM, 0.0) - origin;
    let size = (transform * Point::new(0.0, UNITS_PER_EM) - origin).hypot();
    if size < MIN_GLYPH_SIZE
        || !size.is_finite()
        || along.hypot() == 0.0
        || !bounds.inflate(size, size).contains(origin)
    {
        return None;
    }
    let turn = (along.y.atan2(along.x) / FRAC_PI_2).round().rem_euclid(4.0) as u8;
    let start = to_frame(origin, turn);
    let end = to_frame(transform * Point::new(advance, 0.0), turn);
    Some(Glyph {
        text,
        turn,
        x: start.x,
        end: end.x.max(start.x),
        baseline: start.y,
        size,
    })
}

/// `point` (page space, `y` down) in the frame `turn` quarter turns from
/// upright.
fn to_frame(point: Point, turn: u8) -> Point {
    match turn {
        0 => point,
        1 => Point::new(point.y, -point.x),
        2 => Point::new(-point.x, -point.y),
        _ => Point::new(-point.y, point.x),
    }
}

struct Line {
    /// Its largest glyph's.
    baseline: f64,
    size: f64,
    glyphs: Vec<Glyph>,
}

struct Chunk {
    text: String,
    x: f64,
    end: f64,
}

/// One reading frame's glyphs as text.
fn layout(mut glyphs: Vec<Glyph>) -> String {
    let lines = lines(&mut glyphs);
    let chunks: Vec<Vec<Chunk>> = lines.iter().map(chunks).collect();
    let Some(left) = chunks
        .iter()
        .flatten()
        .map(|chunk| chunk.x)
        .min_by(f64::total_cmp)
    else {
        return String::new();
    };
    let column = column_width(&glyphs);
    let tolerance = column / 2.0;

    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for (index, line) in chunks.iter().enumerate() {
        for chunk in line {
            starts.push((chunk.x, index));
            ends.push((chunk.end, index));
        }
    }
    starts.sort_by(|a, b| a.0.total_cmp(&b.0));
    ends.sort_by(|a, b| a.0.total_cmp(&b.0));

    let pitch = median(
        lines
            .windows(2)
            .map(|pair| pair[1].baseline - pair[0].baseline),
    );
    let mut text = String::new();
    for (index, line_chunks) in chunks.iter().enumerate() {
        if index > 0 {
            text.push('\n');
            let gap = lines[index].baseline - lines[index - 1].baseline;
            let size = lines[index].size.max(lines[index - 1].size);
            text.push_str(&"\n".repeat(blank_lines(gap, pitch, size)));
        }
        let mut row = String::new();
        let mut width = 0;
        for chunk in line_chunks {
            let length = chunk.text.chars().count();
            let right_aligned = lines_up(&ends, chunk.end, index, tolerance)
                && !lines_up(&starts, chunk.x, index, tolerance);
            let column_of = |x: f64| ((x - left) / column).round().max(0.0) as usize;
            let wanted = if right_aligned {
                column_of(chunk.end).saturating_sub(length)
            } else {
                column_of(chunk.x)
            };
            let at = if width == 0 {
                wanted
            } else {
                wanted.max(width + 1)
            };
            row.extend(std::iter::repeat_n(' ', at - width));
            row.push_str(&chunk.text);
            width = at + length;
        }
        text.push_str(row.trim_end());
    }
    text.trim_matches('\n').to_string()
}

/// Groups `glyphs` into lines, top to bottom, each sorted along its baseline
/// with duplicates dropped. Glyphs are taken largest first, so each line is
/// anchored on its largest glyph; a smaller one joins the nearest line its
/// baseline is close enough to (a superscript is raised about a third of
/// an em).
fn lines(glyphs: &mut [Glyph]) -> Vec<Line> {
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    order.sort_by(|&a, &b| glyphs[b].size.total_cmp(&glyphs[a].size));

    let mut lines: Vec<Line> = Vec::new();
    for index in order {
        let glyph = &glyphs[index];
        let nearest = lines
            .iter_mut()
            .filter(|line| {
                let tolerance = (0.45 * line.size).min(0.7 * glyph.size);
                (glyph.baseline - line.baseline).abs() <= tolerance
            })
            .min_by(|a, b| {
                let distance = |line: &Line| (glyph.baseline - line.baseline).abs();
                distance(a).total_cmp(&distance(b))
            });
        match nearest {
            Some(line) => line.glyphs.push(glyph.clone()),
            None => lines.push(Line {
                baseline: glyph.baseline,
                size: glyph.size,
                glyphs: vec![glyph.clone()],
            }),
        }
    }

    lines.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    for line in &mut lines {
        line.glyphs.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut kept: Vec<Glyph> = Vec::with_capacity(line.glyphs.len());
        for glyph in line.glyphs.drain(..) {
            let duplicate = kept.iter().rev().take(4).any(|previous| {
                previous.text == glyph.text
                    && (previous.x - glyph.x).abs() < DUPLICATE_DISTANCE * glyph.size
                    && (previous.baseline - glyph.baseline).abs() < DUPLICATE_DISTANCE * glyph.size
            });
            if !duplicate {
                kept.push(glyph);
            }
        }
        line.glyphs = kept;
    }
    lines
}

/// `line`'s glyphs as chunks: words joined by single spaces, split where the
/// gap is wider than [`CHUNK_GAP`].
fn chunks(line: &Line) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut previous_end: Option<f64> = None;
    let mut space_pending = false;
    for glyph in &line.glyphs {
        if glyph.is_space() {
            space_pending = previous_end.is_some();
            continue;
        }
        match (previous_end, chunks.last_mut()) {
            (Some(end), Some(chunk)) if glyph.x - end <= CHUNK_GAP * line.size => {
                if space_pending || glyph.x - end > WORD_GAP * line.size {
                    chunk.text.push(' ');
                }
                chunk.text.push_str(&glyph.text);
                chunk.end = chunk.end.max(glyph.end);
            }
            _ => chunks.push(Chunk {
                text: glyph.text.clone(),
                x: glyph.x,
                end: glyph.end,
            }),
        }
        previous_end = Some(previous_end.map_or(glyph.end, |end| end.max(glyph.end)));
        space_pending = false;
    }
    chunks
}

/// The grid's column width: the median width of the frame's letters and
/// digits (punctuation is narrower, and a form's dot leaders would make the
/// grid far too wide), but at least [`MIN_COLUMN_WIDTH`] of their median
/// size; half that size if none has a width. An OCR'd line counts as one
/// glyph, its width shared by its characters.
fn column_width(glyphs: &[Glyph]) -> f64 {
    let letters = || {
        glyphs
            .iter()
            .filter(|glyph| glyph.text.chars().any(char::is_alphanumeric))
    };
    let size = median(letters().map(|glyph| glyph.size));
    let width = median(
        letters()
            .map(|glyph| (glyph.end - glyph.x) / glyph.text.chars().count() as f64)
            .filter(|width| *width > 0.0),
    );
    if width > 0.0 {
        width.max(MIN_COLUMN_WIDTH * size)
    } else {
        (size / 2.0).max(1.0)
    }
}

/// Whether one of `edges` (sorted `(position, line)` pairs) from a line
/// other than `line` is within `tolerance` of `position`.
fn lines_up(edges: &[(f64, usize)], position: f64, line: usize, tolerance: f64) -> bool {
    let first = edges.partition_point(|(edge, _)| *edge < position - tolerance);
    edges[first..]
        .iter()
        .take_while(|(edge, _)| *edge <= position + tolerance)
        .any(|(_, other)| *other != line)
}

/// How many blank lines a `gap` between two baselines becomes, given the
/// frame's usual line `pitch` and the lines' `size`: one per missing line
/// (a gap of at least one and a half lines), if the gap is also wider than
/// a line of that size would be.
fn blank_lines(gap: f64, pitch: f64, size: f64) -> usize {
    if pitch <= 0.0 || gap <= 1.5 * size {
        return 0;
    }
    ((gap / pitch).round() as usize)
        .saturating_sub(1)
        .min(MAX_BLANK_LINES)
}

/// The median of `values`, 0 for none.
fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(text: &str, x: f64, baseline: f64, size: f64) -> Glyph {
        let width = 0.6 * size * text.chars().count() as f64;
        Glyph {
            text: text.to_string(),
            turn: 0,
            x,
            end: x + width,
            baseline,
            size,
        }
    }

    /// One glyph per character of `text`, monospaced (0.6 em), from `x`.
    fn word(text: &str, x: f64, baseline: f64, size: f64) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| glyph(&c.to_string(), x + 0.6 * size * i as f64, baseline, size))
            .collect()
    }

    #[test]
    fn readable_text_spells_out_ligatures_and_drops_unreadable_glyphs() {
        let text = |c: char| readable_text(Some(BfString::Char(c)));

        assert_eq!(text('\u{FB01}'), Readable::Text("fi".into()));
        assert_eq!(text('\u{00A0}'), Readable::Text(" ".into()));
        assert_eq!(text('\u{00AD}'), Readable::Dropped);
        assert_eq!(text('\u{FFFD}'), Readable::Unreadable);
        assert_eq!(text('\u{E001}'), Readable::Unreadable);
        assert_eq!(text('\u{0007}'), Readable::Unreadable);
        assert_eq!(readable_text(None), Readable::Unreadable);
        assert_eq!(
            readable_text(Some(BfString::String("é".into()))),
            Readable::Text("é".into())
        );
    }

    fn page_text_of(text: &str, unreadable: usize) -> PageText {
        PageText {
            text: text.to_string(),
            letters: text.chars().filter(|c| c.is_alphanumeric()).count(),
            glyphs: text.chars().filter(|c| !c.is_whitespace()).count() + unreadable,
            unreadable,
        }
    }

    #[test]
    fn a_text_layer_is_usable_with_enough_readable_letters() {
        assert!(page_text_of("Facture du 06/08/2022, montant 42 euros", 0).is_usable());
        // A page number over a scan.
        assert!(!page_text_of("Page 1 / 2", 0).is_usable());
        // Mostly glyphs without a Unicode mapping.
        assert!(!page_text_of("Facture du 06/08/2022, montant 42 euros", 20).is_usable());
        assert!(page_text_of("Facture du 06/08/2022, montant 42 euros", 1).is_usable());
    }

    fn text_box(text: &str, left: f64, top: f64, right: f64, bottom: f64) -> TextBox {
        TextBox {
            text: text.to_string(),
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn layout_boxes_lays_ocr_lines_out_like_a_text_layer() {
        let boxes = vec![
            // Out of order, as detection may return them.
            text_box("Total", 100.0, 150.0, 150.0, 170.0),
            text_box("Facture n 42", 100.0, 100.0, 220.0, 120.0),
            text_box("125,00", 400.0, 151.0, 460.0, 171.0),
            text_box("  ", 300.0, 100.0, 310.0, 120.0),
        ];

        let text = layout_boxes(boxes);
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 2, "{text:?}");
        assert_eq!(lines[0], "Facture n 42");
        assert!(lines[1].starts_with("Total  ") && lines[1].ends_with("125,00"));
    }

    #[test]
    fn layout_boxes_of_nothing_is_empty() {
        assert_eq!(layout_boxes(Vec::new()), "");
    }

    /// An A4 page, in points.
    const PAGE: Rect = Rect::new(0.0, 0.0, 595.0, 842.0);

    #[test]
    fn place_snaps_the_reading_direction_to_a_quarter_turn() {
        let at = Affine::translate((100.0, 100.0));
        let upright = place("a".into(), at * Affine::scale(0.01), 600.0, PAGE).unwrap();
        // Reading top to bottom: the baseline points down the page.
        let down = place(
            "a".into(),
            at * Affine::rotate(FRAC_PI_2) * Affine::scale(0.01),
            600.0,
            PAGE,
        );

        assert_eq!(upright.turn, 0);
        assert!((upright.size - 10.0).abs() < 1e-9);
        assert!((upright.end - upright.x - 6.0).abs() < 1e-9);
        assert_eq!(down.unwrap().turn, 1);
    }

    #[test]
    fn place_drops_degenerate_glyphs() {
        let at = Affine::translate((100.0, 100.0));

        assert!(place("a".into(), at * Affine::scale(0.0001), 600.0, PAGE).is_none());
    }

    #[test]
    fn place_drops_glyphs_off_the_page() {
        let place_at = |x: f64, y: f64| {
            place(
                "a".into(),
                Affine::translate((x, y)) * Affine::scale(0.01),
                600.0,
                PAGE,
            )
        };

        assert!(place_at(1e9, 100.0).is_none());
        assert!(place_at(-50.0, 100.0).is_none());
        assert!(place_at(100.0, 900.0).is_none());
        // Within an em of the edge: a glyph the crop box cuts.
        assert!(place_at(-5.0, 100.0).is_some());
        assert!(place_at(590.0, 845.0).is_some());
    }

    #[test]
    fn layout_joins_glyphs_into_words_on_their_lines() {
        let mut glyphs = word("Hello", 100.0, 100.0, 10.0);
        // No space glyph, but a word-sized gap.
        glyphs.extend(word("World", 134.0, 100.0, 10.0));
        glyphs.extend(word("Next", 100.0, 114.0, 10.0));

        assert_eq!(layout(glyphs), "Hello World\nNext");
    }

    #[test]
    fn layout_keeps_a_space_glyph_between_words() {
        let mut glyphs = word("a", 100.0, 100.0, 10.0);
        glyphs.push(glyph(" ", 106.0, 100.0, 10.0));
        glyphs.extend(word("b", 107.0, 100.0, 10.0));

        assert_eq!(layout(glyphs), "a b");
    }

    #[test]
    fn layout_keeps_columns_apart() {
        let mut glyphs = word("Name", 100.0, 100.0, 10.0);
        glyphs.extend(word("Value", 400.0, 100.0, 10.0));

        let text = layout(glyphs);

        assert!(text.starts_with("Name   "), "{text:?}");
        assert!(text.ends_with("Value"), "{text:?}");
        assert_eq!(text.find("Value"), Some(50));
    }

    #[test]
    fn layout_right_aligns_amount_columns() {
        let mut glyphs = word("Prix", 100.0, 100.0, 10.0);
        glyphs.extend(word("5,00", 376.0, 100.0, 10.0));
        glyphs.extend(word("Total TTC", 100.0, 114.0, 10.0));
        glyphs.extend(word("125,00", 364.0, 114.0, 10.0));

        let text = layout(glyphs);
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 2, "{text:?}");
        assert_eq!(lines[0].len(), lines[1].len(), "{text:?}");
        assert!(lines[0].ends_with(" 5,00") && lines[1].ends_with(" 125,00"));
    }

    #[test]
    fn layout_attaches_superscripts_to_their_line() {
        let mut glyphs = word("12 m", 100.0, 100.0, 10.0);
        glyphs.push(glyph("2", 124.0, 96.0, 6.0));
        glyphs.extend(word("Next", 100.0, 114.0, 10.0));

        assert_eq!(layout(glyphs), "12 m2\nNext");
    }

    #[test]
    fn layout_does_not_merge_small_text_under_a_title() {
        let mut glyphs = word("TITLE", 100.0, 100.0, 24.0);
        glyphs.extend(word("body", 100.0, 109.0, 10.0));

        assert_eq!(layout(glyphs), "TITLE\nbody");
    }

    #[test]
    fn layout_turns_vertical_gaps_into_blank_lines() {
        let mut glyphs = word("one", 100.0, 100.0, 10.0);
        glyphs.extend(word("two", 100.0, 114.0, 10.0));
        glyphs.extend(word("three", 100.0, 128.0, 10.0));
        glyphs.extend(word("far", 100.0, 300.0, 10.0));

        assert_eq!(layout(glyphs), "one\ntwo\nthree\n\n\nfar");
    }

    #[test]
    fn layout_drops_duplicated_glyphs() {
        let mut glyphs = word("Bold", 100.0, 100.0, 10.0);
        glyphs.extend(word("Bold", 100.3, 100.0, 10.0));

        assert_eq!(layout(glyphs), "Bold");
    }

    #[test]
    fn layout_does_not_pad_lines_out_to_a_font_with_wrong_widths() {
        // Glyphs a thousandth of their real width, as from a font's widths
        // in the wrong unit.
        let mut glyphs = Vec::new();
        for (x, text) in [(100.0, "Name"), (400.0, "Value")] {
            for (i, c) in text.chars().enumerate() {
                // Placed by those widths: piled up.
                let mut glyph = glyph(&c.to_string(), x + 0.006 * i as f64, 100.0, 10.0);
                glyph.end = glyph.x + 0.006;
                glyphs.push(glyph);
            }
        }

        let text = layout(glyphs);

        assert!(
            text.starts_with("Name ") && text.ends_with("Value"),
            "{text:?}"
        );
        assert!(text.len() < 200, "{} characters", text.len());
    }

    #[test]
    fn layout_drops_the_left_margin() {
        assert_eq!(layout(word("x", 300.0, 100.0, 10.0)), "x");
    }

    #[test]
    fn layout_of_no_glyph_is_empty() {
        assert_eq!(layout(Vec::new()), "");
    }
}
