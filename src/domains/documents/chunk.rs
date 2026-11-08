//! Cuts a document's text into retrieval-sized chunks along page, paragraph
//! and line boundaries, as byte ranges into it. Ported from `doc-ai-poc`'s
//! `chunk.rs`: like it, chunks never overlap.

use std::ops::Range;

/// What [`super::transcript::extract`] puts between two pages.
const PAGE_SEPARATOR: &str = "\n\n---\n\n";

/// A chunk's size, in characters of whitespace-collapsed text: layout
/// padding (grid-aligned text is mostly spaces) doesn't count.
fn weight(text: &str) -> usize {
    text.split_whitespace()
        .map(|word| word.chars().count() + 1)
        .sum()
}

/// Splits `text` into chunks of at most `max` [`weight`], never spanning two
/// pages: whole paragraphs while they fit, a paragraph too long on its own
/// (a dense table) line by line, a line too long on its own whole. Each
/// range starts and ends on non-whitespace; whitespace-only text has none.
pub(super) fn chunk(text: &str, max: usize) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();

    for page in text.split(PAGE_SEPARATOR) {
        let mut pending = Pending::default();
        for paragraph in page.split("\n\n") {
            let paragraph_weight = weight(paragraph);
            if paragraph_weight == 0 {
                continue;
            }
            if pending.weight + paragraph_weight > max {
                pending.flush(&mut chunks);
            }
            if paragraph_weight <= max {
                pending.push(text, paragraph, paragraph_weight);
                continue;
            }
            for line in paragraph.lines() {
                let line_weight = weight(line);
                if pending.weight + line_weight > max {
                    pending.flush(&mut chunks);
                }
                pending.push(text, line, line_weight);
            }
        }
        pending.flush(&mut chunks);
    }

    chunks
}

/// The chunk being built: the range from its first piece to its last.
#[derive(Default)]
struct Pending {
    range: Option<Range<usize>>,
    weight: usize,
}

impl Pending {
    /// Extends the chunk to `piece`, a slice of `text` weighing `weight`.
    fn push(&mut self, text: &str, piece: &str, weight: usize) {
        if weight == 0 {
            return;
        }
        let piece = piece.trim();
        let start = piece.as_ptr().addr() - text.as_ptr().addr();
        let end = start + piece.len();
        self.range = Some(match self.range.take() {
            Some(range) => range.start..end,
            None => start..end,
        });
        self.weight += weight;
    }

    fn flush(&mut self, chunks: &mut Vec<Range<usize>>) {
        chunks.extend(self.range.take());
        self.weight = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str, max: usize) -> Vec<&str> {
        chunk(text, max)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn groups_paragraphs_up_to_max_and_keeps_pages_apart() {
        let text = "aaa bbb\n\nccc\n\n\n\nddd eee fff\n\n---\n\nggg";

        assert_eq!(texts(text, 12), ["aaa bbb\n\nccc", "ddd eee fff", "ggg"]);
    }

    #[test]
    fn never_merges_short_pages_into_one_chunk() {
        assert_eq!(texts("aaa\n\n---\n\nbbb", 1000), ["aaa", "bbb"]);
    }

    #[test]
    fn splits_a_paragraph_too_long_on_its_own_by_line() {
        assert_eq!(
            texts("one two\nthree four\nfive six", 12),
            ["one two", "three four", "five six"]
        );
    }

    #[test]
    fn keeps_a_line_too_long_on_its_own_whole() {
        assert_eq!(
            texts("short\n\nfar too long a line\nend", 8),
            ["short", "far too long a line", "end"]
        );
    }

    #[test]
    fn layout_padding_does_not_count_towards_the_size() {
        assert_eq!(
            texts("aaa          bbb\n\nccc", 12),
            ["aaa          bbb\n\nccc"]
        );
    }

    #[test]
    fn trims_whitespace_around_each_chunk() {
        assert_eq!(
            texts("  \n  aaa bbb  \n\n  ccc \n ", 1000),
            ["aaa bbb  \n\n  ccc"]
        );
    }

    #[test]
    fn measures_multibyte_text_in_characters_with_ranges_on_char_boundaries() {
        // "café été" weighs 9 in characters (it's 11 bytes): it fits a
        // 9-character chunk, so "ñandú" starts the next one.
        let text = "café été\n\nñandú\n\n---\n\n  naïve ";

        let ranges = chunk(text, 9);

        assert!(ranges.iter().all(|range| text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)));
        assert_eq!(texts(text, 9), ["café été", "ñandú", "naïve"]);
    }

    #[test]
    fn chunks_are_ordered_disjoint_and_cover_every_word() {
        let text = "Facture n° 42\n\nMontant dû : 120,00 €\nÉchéance : 1er mars\n\n\n\n\
                    Désignation   Qté   Prix\nAbonnement    1     100,00\nTVA           1     20,00\n\n---\n\n\
                    Conditions générales de vente";

        let ranges = chunk(text, 24);

        assert!(ranges.len() > 3, "{ranges:?}");
        assert!(ranges.iter().all(|range| range.start < range.end));
        assert!(ranges.windows(2).all(|pair| pair[0].end <= pair[1].start));
        let words: Vec<&str> = ranges
            .iter()
            .flat_map(|range| text[range.clone()].split_whitespace())
            .collect();
        let expected: Vec<&str> = text.split_whitespace().filter(|w| *w != "---").collect();
        assert_eq!(words, expected);
    }

    #[test]
    fn empty_or_blank_text_has_no_chunks() {
        assert!(chunk("", 1000).is_empty());
        assert!(chunk(" \n\n \t\n\n---\n\n ", 1000).is_empty());
    }
}
