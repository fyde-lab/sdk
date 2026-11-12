//! Zero-shot entity spans from GLiNER-X small (`tools::models`), run on
//! device through ONNX Runtime. Ported from `doc-ai-poc`'s `gliner.rs`. Only
//! finds the spans of a text matching labels given in plain words ("net
//! pay", "invoice number"): which labels to look for, and what to make of the
//! spans, is up to the caller.
//!
//! The model reads `<<ENT>> label… <<ENT>> label… <<SEP>> word word … </s>`
//! and scores every span of up to [`MAX_WIDTH`] words against every label.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::Tokenizer;

use crate::tools::models::{GLINER_X_SMALL, GLINER_X_SMALL_TOKENIZER, ModelStore};
use crate::tools::onnx;
use crate::{ErrorContext as _, ErrorKind, Result};

/// `<<ENT>>` and `<<SEP>>`, from the model's `added_tokens.json`: reused mT5
/// sentinel ids, so its `tokenizer.json` doesn't know them by name.
const ENT: i64 = 250100;
const SEP: i64 = 250101;
/// `gliner_config.json`'s `max_width`: the longest span, in words.
const MAX_WIDTH: usize = 12;
/// Words per model call. Windows overlap by [`OVERLAP`] words, so a span on a
/// boundary is still seen whole by one of them.
const WINDOW: usize = 320;
const OVERLAP: usize = MAX_WIDTH;
/// At most this many threads run the model, so it never takes over a
/// device's every core.
const MAX_THREADS: usize = 4;
const OUTPUT: &str = "logits";

/// A span of text matching one of the labels looked for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Span {
    /// The label's index among those looked for.
    pub(crate) label: usize,
    /// Its byte range in the text.
    pub(crate) bytes: Range<usize>,
    /// How sure the model is, from 0 to 1.
    pub(crate) score: f32,
}

/// Finds the spans of a text matching labels given in plain words.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait SpanFinder: Send + Sync {
    /// Fetches the model (see [`ModelStore::ensure`]) and loads it, unless
    /// that's already done: a failure is retried on the next call.
    async fn load(&self) -> Result<()>;

    /// Every span of `text` matching one of `labels` with a score of at
    /// least `threshold`, best first. Overlapping spans are all kept, and a
    /// span on the boundary between two windows of the text can come twice.
    /// Only the first `max_words` words of `text` are read. Fetches and
    /// loads the model first if needed (a failure is retried on the next
    /// call), unless there is nothing to look for.
    async fn spans(
        &self,
        text: String,
        labels: Vec<String>,
        threshold: f32,
        max_words: usize,
    ) -> Result<Vec<Span>>;
}

/// The [`SpanFinder`] running GLiNER-X small. Its session is loaded on the
/// first call that needs it, then kept for the finder's lifetime.
pub(crate) struct OnnxGliner {
    models: Arc<ModelStore>,
    threads: usize,
    loaded: tokio::sync::OnceCell<Arc<Loaded>>,
}

impl OnnxGliner {
    pub(crate) fn new(models: Arc<ModelStore>) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get());

        Self {
            models,
            threads: (cores / 2).clamp(1, MAX_THREADS),
            loaded: tokio::sync::OnceCell::new(),
        }
    }

    async fn loaded(&self) -> Result<Arc<Loaded>> {
        self.loaded
            .get_or_try_init(|| async {
                let model = self
                    .models
                    .ensure(&GLINER_X_SMALL)
                    .await
                    .context("failed to fetch the GLiNER model")?;
                let tokenizer = self
                    .models
                    .ensure(&GLINER_X_SMALL_TOKENIZER)
                    .await
                    .context("failed to fetch the GLiNER model's tokenizer")?;

                self.open(model, tokenizer).await
            })
            .await
            .cloned()
    }

    async fn open(&self, model: PathBuf, tokenizer: PathBuf) -> Result<Arc<Loaded>> {
        let threads = self.threads;

        tokio::task::spawn_blocking(move || Loaded::open(&model, &tokenizer, threads))
            .await
            .context("the GLiNER model loading task failed")?
            .map(Arc::new)
    }
}

#[async_trait]
impl SpanFinder for OnnxGliner {
    async fn load(&self) -> Result<()> {
        self.loaded().await.map(|_| ())
    }

    async fn spans(
        &self,
        text: String,
        labels: Vec<String>,
        threshold: f32,
        max_words: usize,
    ) -> Result<Vec<Span>> {
        // `split_words` only skips whitespace: a blank text has no word.
        if labels.is_empty() || max_words == 0 || text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let loaded = self.loaded().await?;

        tokio::task::spawn_blocking(move || loaded.spans(&text, &labels, threshold, max_words))
            .await
            .context("the GLiNER task failed")?
            .context("failed to find spans with GLiNER")
    }
}

struct Loaded {
    /// `Session::run` takes `&mut self`.
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    eos: i64,
    inputs: Vec<String>,
}

impl Loaded {
    fn open(model: &Path, tokenizer: &Path, threads: usize) -> Result<Self> {
        let session = onnx::session(model, threads).context("failed to load the GLiNER model")?;
        let tokenizer =
            onnx::tokenizer(tokenizer).context("failed to load the GLiNER model's tokenizer")?;
        let eos = tokenizer.token_to_id("</s>").ok_or_else(|| {
            ErrorKind::InvalidModel("the GLiNER tokenizer has no </s> token".to_string())
        })?;
        let inputs = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect();

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            eos: i64::from(eos),
            inputs,
        })
    }

    fn spans(
        &self,
        text: &str,
        labels: &[String],
        threshold: f32,
        max_words: usize,
    ) -> Result<Vec<Span>> {
        let mut label_ids = Vec::with_capacity(labels.len());
        for label in labels {
            let encoding = self
                .tokenizer
                .encode(label.as_str(), false)
                .map_err(ErrorKind::Tokenizer)
                .with_context(|| format!("failed to tokenize the label {label:?}"))?;
            label_ids.push(encoding.get_ids().to_vec());
        }
        let prompt = prompt(&label_ids);

        find_spans(text, labels.len(), threshold, max_words, |words| {
            self.run(&prompt, words)
        })
    }

    /// Runs the model on one window's `words`, returning the logits' shape
    /// and values.
    fn run(&self, prompt: &[i64], words: &[&str]) -> Result<(Vec<i64>, Vec<f32>)> {
        let encoding = self
            .tokenizer
            .encode(words.to_vec(), false)
            .map_err(ErrorKind::Tokenizer)
            .context("failed to tokenize the text")?;
        let inputs = build_inputs(
            prompt,
            encoding.get_ids(),
            encoding.get_word_ids(),
            self.eos,
            words.len(),
        );

        let n_tokens = inputs.input_ids.len();
        let n_spans = inputs.span_mask.len();
        let int = |name: &'static str, shape: Vec<usize>, values: Vec<i64>| {
            Tensor::from_array((shape, values))
                .map(SessionInputValue::from)
                .with_context(|| format!("failed to build the GLiNER model's {name} input"))
                .map(|value| (name, value))
        };
        let built = vec![
            int("input_ids", vec![1, n_tokens], inputs.input_ids)?,
            int("attention_mask", vec![1, n_tokens], vec![1; n_tokens])?,
            int("words_mask", vec![1, n_tokens], inputs.words_mask)?,
            int("text_lengths", vec![1, 1], vec![words.len() as i64])?,
            int("span_idx", vec![1, n_spans, 2], inputs.span_idx)?,
            (
                "span_mask",
                Tensor::from_array(([1, n_spans], inputs.span_mask))
                    .map(SessionInputValue::from)
                    .context("failed to build the GLiNER model's span_mask input")?,
            ),
        ];
        let inputs = select_inputs(&self.inputs, built)?;

        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let outputs = session
            .run(inputs)
            .context("failed to run the GLiNER model")?;
        let output = outputs.get(OUTPUT).ok_or_else(|| {
            ErrorKind::InvalidModel(format!("the GLiNER model has no {OUTPUT} output"))
        })?;
        let (shape, logits) = output
            .try_extract_tensor::<f32>()
            .context("failed to read the GLiNER model's logits")?;

        Ok((shape.to_vec(), logits.to_vec()))
    }
}

/// GLiNER's word splitter (`\w+(?:[-_]\w+)*|\S`), as byte ranges: runs of
/// letters, digits and underscores, joined across single hyphens between
/// two of them; any other non-whitespace character is a word of its own.
fn split_words(text: &str) -> Vec<Range<usize>> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut words = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let mut end = start + c.len_utf8();
        if is_word(c) {
            while let Some(&(i, next)) = chars.peek() {
                let joins = is_word(next)
                    || (next == '-' && text[i + 1..].chars().next().is_some_and(is_word));
                if !joins {
                    break;
                }
                chars.next();
                end = i + next.len_utf8();
            }
        }
        words.push(start..end);
    }
    words
}

/// The ranges of word indices each model call reads, out of `n_words`: at
/// most [`WINDOW`] words each, each starting [`OVERLAP`] words before the
/// previous one ends.
fn windows(n_words: usize) -> Vec<Range<usize>> {
    let mut windows = Vec::new();
    let mut start = 0;
    while start < n_words {
        let end = (start + WINDOW).min(n_words);
        windows.push(start..end);
        if end == n_words {
            break;
        }
        start = end - OVERLAP;
    }
    windows
}

/// The prompt preceding every window: `<<ENT>>` then each label's tokens,
/// for each label, then `<<SEP>>`.
fn prompt(labels: &[Vec<u32>]) -> Vec<i64> {
    let mut prompt = Vec::new();
    for label in labels {
        prompt.push(ENT);
        prompt.extend(label.iter().map(|&id| i64::from(id)));
    }
    prompt.push(SEP);
    prompt
}

/// The model's inputs for one window, but the attention mask (all ones) and
/// the text length (`n_words`).
#[derive(Debug, PartialEq)]
struct Inputs {
    /// The prompt, the window's tokens, then `</s>`.
    input_ids: Vec<i64>,
    /// For each of `input_ids`, the 1-based number of the word it's the
    /// first token of, else 0.
    words_mask: Vec<i64>,
    /// `[first word, last word]` of every span, [`MAX_WIDTH`] per word.
    span_idx: Vec<i64>,
    /// Whether each span ends within the window.
    span_mask: Vec<bool>,
}

/// Builds a window's [`Inputs`] from its `tokens` and the word each comes
/// from (`word_ids`, as the tokenizer returns them for pre-split words).
fn build_inputs(
    prompt: &[i64],
    tokens: &[u32],
    word_ids: &[Option<u32>],
    eos: i64,
    n_words: usize,
) -> Inputs {
    let mut input_ids = prompt.to_vec();
    let mut words_mask = vec![0; prompt.len()];
    let mut previous = None;
    for (&id, &word) in tokens.iter().zip(word_ids) {
        input_ids.push(i64::from(id));
        words_mask.push(match word {
            Some(word) if Some(word) != previous => {
                previous = Some(word);
                i64::from(word) + 1
            }
            _ => 0,
        });
    }
    input_ids.push(eos);
    words_mask.push(0);

    let mut span_idx = Vec::with_capacity(n_words * MAX_WIDTH * 2);
    let mut span_mask = Vec::with_capacity(n_words * MAX_WIDTH);
    for first in 0..n_words {
        for width in 0..MAX_WIDTH {
            span_idx.extend([first as i64, (first + width) as i64]);
            span_mask.push(first + width < n_words);
        }
    }

    Inputs {
        input_ids,
        words_mask,
        span_idx,
        span_mask,
    }
}

/// Keeps the `built` inputs the model declares, failing if it declares one
/// that isn't built.
fn select_inputs<T>(
    declared: &[String],
    mut built: Vec<(&'static str, T)>,
) -> Result<Vec<(&'static str, T)>> {
    if let Some(unknown) = declared
        .iter()
        .find(|name| !built.iter().any(|(built, _)| built == name))
    {
        return Err(ErrorKind::InvalidModel(format!(
            "the GLiNER model expects an unknown input {unknown}"
        ))
        .into());
    }
    built.retain(|(name, _)| declared.iter().any(|declared| declared == name));
    Ok(built)
}

/// Splits `text` into words, keeps the first `max_words`, and reads them in
/// [`windows`] through `run` (which returns a window's logits shape and
/// values), returning the spans scoring at least `threshold`, best first.
fn find_spans(
    text: &str,
    n_labels: usize,
    threshold: f32,
    max_words: usize,
    mut run: impl FnMut(&[&str]) -> Result<(Vec<i64>, Vec<f32>)>,
) -> Result<Vec<Span>> {
    if n_labels == 0 {
        return Ok(Vec::new());
    }
    let mut words = split_words(text);
    words.truncate(max_words);

    let mut spans = Vec::new();
    for window in windows(words.len()) {
        let words = &words[window.clone()];
        let texts: Vec<&str> = words.iter().map(|word| &text[word.clone()]).collect();
        let found = run(&texts)
            .and_then(|(shape, logits)| decode(&shape, &logits, words, n_labels, threshold))
            .with_context(|| format!("failed to read words {}..{}", window.start, window.end))?;
        spans.extend(found);
    }
    spans.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(spans)
}

/// Reads one window's `[1, words, MAX_WIDTH, n_labels]` logits: each is the
/// score of the span of `width + 1` words from `first`, for one label. Keeps
/// the spans scoring at least `threshold` that end within the window, as
/// byte ranges (`words` being the window's words' ones).
fn decode(
    shape: &[i64],
    logits: &[f32],
    words: &[Range<usize>],
    n_labels: usize,
    threshold: f32,
) -> Result<Vec<Span>> {
    let expected = [1, words.len() as i64, MAX_WIDTH as i64, n_labels as i64];
    if shape != expected || logits.len() != words.len() * MAX_WIDTH * n_labels {
        return Err(ErrorKind::InvalidModel(format!(
            "the GLiNER model returned logits of shape {shape:?}, not {expected:?}"
        ))
        .into());
    }

    let mut spans = Vec::new();
    for (i, logit) in logits.iter().enumerate() {
        let score = 1.0 / (1.0 + (-logit).exp());
        let (label, rest) = (i % n_labels, i / n_labels);
        let (width, first) = (rest % MAX_WIDTH, rest / MAX_WIDTH);
        if score >= threshold && first + width < words.len() {
            spans.push(Span {
                label,
                bytes: words[first].start..words[first + width].end,
                score,
            });
        }
    }
    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn words(text: &str) -> Vec<&str> {
        split_words(text)
            .into_iter()
            .map(|word| &text[word])
            .collect()
    }

    #[test]
    fn split_words_splits_like_gliner() {
        assert_eq!(
            words("Net à payer : 1 234,56 € (jean-pierre_x) -"),
            [
                "Net",
                "à",
                "payer",
                ":",
                "1",
                "234",
                ",",
                "56",
                "€",
                "(",
                "jean-pierre_x",
                ")",
                "-"
            ]
        );
    }

    #[test]
    fn split_words_returns_byte_ranges_in_multibyte_text() {
        let text = "Échéance\u{a0}: 15 février — 日本語テキスト";

        assert_eq!(
            split_words(text),
            [0..10, 12..13, 14..16, 17..25, 26..29, 30..51]
        );
        assert_eq!(
            words(text),
            ["Échéance", ":", "15", "février", "—", "日本語テキスト"]
        );
    }

    #[test]
    fn split_words_joins_words_across_hyphens_and_underscores() {
        assert_eq!(
            words("Saint-Étienne-du-Rouvray n_facture __init__ a_-b _"),
            [
                "Saint-Étienne-du-Rouvray",
                "n_facture",
                "__init__",
                "a_-b",
                "_"
            ]
        );
    }

    #[test]
    fn split_words_keeps_hyphens_not_between_two_words_apart() {
        assert_eq!(
            words("-a a- a--b a-€ a - b"),
            [
                "-", "a", "a", "-", "a", "-", "-", "b", "a", "-", "€", "a", "-", "b"
            ]
        );
    }

    #[test]
    fn split_words_makes_every_punctuation_mark_a_word() {
        assert_eq!(
            words("TVA(20%):«ok»!?..."),
            [
                "TVA", "(", "20", "%", ")", ":", "«", "ok", "»", "!", "?", ".", ".", "."
            ]
        );
    }

    #[test]
    fn split_words_skips_every_kind_of_whitespace() {
        assert_eq!(words(" \t\n\r\u{a0}\u{2003}a\n\nb "), ["a", "b"]);
        assert!(split_words(" \n\t").is_empty());
        assert!(split_words("").is_empty());
    }

    #[test]
    fn windows_overlap_by_max_width_words() {
        assert_eq!(windows(700), [0..320, 308..628, 616..700]);
        assert_eq!(OVERLAP, MAX_WIDTH);
    }

    #[test]
    fn windows_fit_a_short_text_in_one() {
        assert_eq!(windows(1), [Range { start: 0, end: 1 }]);
        assert_eq!(
            windows(WINDOW),
            [Range {
                start: 0,
                end: WINDOW
            }]
        );
        assert_eq!(windows(WINDOW + 1), [0..320, 308..321]);
    }

    #[test]
    fn windows_of_no_word_are_none() {
        assert!(windows(0).is_empty());
    }

    #[test]
    fn prompt_puts_ent_before_each_label_and_sep_last() {
        assert_eq!(prompt(&[vec![7, 8], vec![9]]), [ENT, 7, 8, ENT, 9, SEP]);
        assert_eq!(prompt(&[]), [SEP]);
    }

    #[test]
    fn build_inputs_numbers_each_words_first_token() {
        // Three words: the second is split into three tokens.
        let inputs = build_inputs(
            &[ENT, 5, SEP],
            &[10, 11, 12, 13, 14],
            &[Some(0), Some(1), Some(1), Some(1), Some(2)],
            1,
            3,
        );

        assert_eq!(inputs.input_ids, [ENT, 5, SEP, 10, 11, 12, 13, 14, 1]);
        assert_eq!(inputs.words_mask, [0, 0, 0, 1, 2, 0, 0, 3, 0]);
    }

    #[test]
    fn build_inputs_leaves_tokens_of_no_word_unnumbered() {
        let inputs = build_inputs(&[SEP], &[10, 11, 12], &[Some(0), None, Some(1)], 1, 2);

        assert_eq!(inputs.words_mask, [0, 1, 0, 2, 0]);
    }

    #[test]
    fn build_inputs_lists_max_width_spans_per_word_masking_those_past_the_end() {
        let inputs = build_inputs(&[SEP], &[10, 11], &[Some(0), Some(1)], 1, 2);

        assert_eq!(inputs.span_mask.len(), 2 * MAX_WIDTH);
        assert_eq!(inputs.span_idx.len(), 2 * MAX_WIDTH * 2);
        assert_eq!(&inputs.span_idx[..6], [0, 0, 0, 1, 0, 2]);
        assert_eq!(
            &inputs.span_idx[MAX_WIDTH * 2..MAX_WIDTH * 2 + 4],
            [1, 1, 1, 2]
        );
        assert_eq!(
            inputs.span_mask.iter().filter(|&&kept| kept).count(),
            3,
            "[0, 0], [0, 1] and [1, 1]"
        );
        assert!(inputs.span_mask[0] && inputs.span_mask[1] && !inputs.span_mask[2]);
        assert!(inputs.span_mask[MAX_WIDTH] && !inputs.span_mask[MAX_WIDTH + 1]);
    }

    fn declared(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn select_inputs_passes_only_the_declared_inputs() {
        let built = vec![("input_ids", 1), ("attention_mask", 2), ("span_mask", 3)];

        let selected = select_inputs(&declared(&["span_mask", "input_ids"]), built).unwrap();

        assert_eq!(selected, [("input_ids", 1), ("span_mask", 3)]);
    }

    #[test]
    fn select_inputs_fails_on_an_input_it_doesnt_build() {
        let built = vec![("input_ids", 1)];

        let err = select_inputs(&declared(&["input_ids", "token_type_ids"]), built).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.to_string().contains("token_type_ids"), "{err}");
    }

    /// The logit whose sigmoid is `score`.
    fn logit(score: f32) -> f32 {
        (score / (1.0 - score)).ln()
    }

    /// Logits for `n_words` words and `n_labels` labels, all scoring 0.01
    /// but `scores`' `((first, width, label), score)`.
    fn logits(
        n_words: usize,
        n_labels: usize,
        scores: &[((usize, usize, usize), f32)],
    ) -> (Vec<i64>, Vec<f32>) {
        let mut values = vec![logit(0.01); n_words * MAX_WIDTH * n_labels];
        for &((first, width, label), score) in scores {
            values[(first * MAX_WIDTH + width) * n_labels + label] = logit(score);
        }
        let shape = vec![1, n_words as i64, MAX_WIDTH as i64, n_labels as i64];
        (shape, values)
    }

    fn assert_spans(actual: &[Span], expected: &[(usize, Range<usize>, f32)]) {
        assert_eq!(actual.len(), expected.len(), "{actual:?}");
        for (span, (label, bytes, score)) in actual.iter().zip(expected) {
            assert_eq!((span.label, &span.bytes), (*label, bytes), "{actual:?}");
            assert!((span.score - score).abs() < 1e-4, "{actual:?}");
        }
    }

    #[test]
    fn decode_reads_each_logit_as_first_word_width_and_label() {
        let words = [0..3, 4..7, 8..11, 12..15];
        let (shape, values) = logits(
            4,
            3,
            &[((0, 0, 2), 0.9), ((1, 2, 1), 0.8), ((2, 1, 0), 0.7)],
        );

        let spans = decode(&shape, &values, &words, 3, 0.5).unwrap();

        assert_spans(&spans, &[(2, 0..3, 0.9), (1, 4..15, 0.8), (0, 8..15, 0.7)]);
    }

    #[test]
    fn decode_keeps_scores_at_or_above_the_threshold() {
        let words = [0..1, 2..3];
        let (shape, values) = logits(
            2,
            1,
            &[((0, 0, 0), 0.6), ((0, 1, 0), 0.5), ((1, 0, 0), 0.4)],
        );

        let spans = decode(&shape, &values, &words, 1, 0.5).unwrap();

        assert_spans(&spans, &[(0, 0..1, 0.6), (0, 0..3, 0.5)]);
    }

    #[test]
    fn decode_drops_spans_past_the_end_of_the_window() {
        let words = [0..1, 2..3];
        // Word 1 spanning 2 words, and word 0 spanning MAX_WIDTH, both end
        // past the last word.
        let (shape, values) = logits(
            2,
            1,
            &[
                ((1, 1, 0), 0.99),
                ((0, MAX_WIDTH - 1, 0), 0.99),
                ((1, 0, 0), 0.9),
            ],
        );

        let spans = decode(&shape, &values, &words, 1, 0.5).unwrap();

        assert_spans(&spans, &[(0, 2..3, 0.9)]);
    }

    #[test]
    fn decode_rejects_logits_of_the_wrong_shape() {
        let words = [0..1, 2..3];
        let (_, values) = logits(2, 2, &[]);

        for shape in [
            vec![1, 3, MAX_WIDTH as i64, 2],
            vec![1, 2, 8, 2],
            vec![1, 2, MAX_WIDTH as i64, 3],
            vec![2, 2, MAX_WIDTH as i64, 2],
            vec![1, 2 * MAX_WIDTH as i64, 2],
        ] {
            let err = decode(&shape, &values, &words, 2, 0.5).unwrap_err();

            assert_eq!(err.code(), ErrorCode::ModelLoadFailed, "{shape:?}");
        }
    }

    #[test]
    fn decode_rejects_values_that_dont_match_the_shape() {
        let words = [0..1, 2..3];
        let (shape, mut values) = logits(2, 2, &[]);
        values.pop();

        let err = decode(&shape, &values, &words, 2, 0.5).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
    }

    /// `n` words `w0 w1 …`.
    fn numbered_words(n: usize) -> String {
        (0..n)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn find_spans_reads_overlapping_windows_and_maps_spans_back_to_the_text() {
        let text = numbered_words(700);
        let mut calls = Vec::new();

        let spans = find_spans(&text, 2, 0.5, usize::MAX, |words| {
            calls.push(
                words
                    .iter()
                    .map(|word| word.to_string())
                    .collect::<Vec<_>>(),
            );
            // The first word of each window, alone, for label 1; the second
            // window also gets its last two words for label 0.
            let scores: &[_] = match calls.len() {
                1 => &[((0, 0, 1), 0.6)],
                2 => &[((0, 0, 1), 0.7), ((318, 1, 0), 0.9)],
                _ => &[((0, 0, 1), 0.8)],
            };
            Ok(logits(words.len(), 2, scores))
        })
        .unwrap();

        let firsts: Vec<_> = calls.iter().map(|call| call[0].as_str()).collect();
        let lens: Vec<_> = calls.iter().map(Vec::len).collect();
        assert_eq!(firsts, ["w0", "w308", "w616"]);
        assert_eq!(lens, [320, 320, 84]);
        let texts: Vec<_> = spans
            .iter()
            .map(|span| (span.label, &text[span.bytes.clone()]))
            .collect();
        assert_eq!(
            texts,
            [(0, "w626 w627"), (1, "w616"), (1, "w308"), (1, "w0")]
        );
    }

    #[test]
    fn find_spans_reads_only_the_first_max_words() {
        let text = numbered_words(400);
        let mut read = Vec::new();

        find_spans(&text, 1, 0.5, 330, |words| {
            read.push((words[0].to_string(), words.len()));
            Ok(logits(words.len(), 1, &[]))
        })
        .unwrap();

        assert_eq!(read, [("w0".to_string(), 320), ("w308".to_string(), 22)]);
    }

    #[test]
    fn find_spans_returns_the_best_first_across_windows() {
        let text = numbered_words(330);
        let mut call = 0;

        let spans = find_spans(&text, 1, 0.1, usize::MAX, |words| {
            call += 1;
            let scores: &[_] = if call == 1 {
                &[((0, 0, 0), 0.3), ((5, 0, 0), 0.9)]
            } else {
                &[((0, 0, 0), 0.6)]
            };
            Ok(logits(words.len(), 1, scores))
        })
        .unwrap();

        let scores: Vec<_> = spans
            .iter()
            .map(|span| (span.score * 10.0).round())
            .collect();
        assert_eq!(scores, [9.0, 6.0, 3.0]);
    }

    #[test]
    fn find_spans_never_runs_the_model_without_a_word_or_a_label() {
        let never = |_: &[&str]| -> Result<(Vec<i64>, Vec<f32>)> { panic!("ran the model") };

        assert!(
            find_spans(" \n ", 1, 0.5, usize::MAX, never)
                .unwrap()
                .is_empty()
        );
        assert!(
            find_spans("text", 0, 0.5, usize::MAX, never)
                .unwrap()
                .is_empty()
        );
        assert!(find_spans("text", 1, 0.5, 0, never).unwrap().is_empty());
    }

    #[test]
    fn find_spans_fails_with_context_on_logits_of_the_wrong_shape() {
        let err = find_spans("a b", 1, 0.5, usize::MAX, |words| {
            Ok(logits(words.len() + 1, 1, &[]))
        })
        .unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.has_context());
        assert!(err.to_string().contains("0..2"), "{err}");
    }

    #[test]
    fn find_spans_fails_with_context_on_an_unknown_model_input() {
        let err = find_spans("a b", 1, 0.5, usize::MAX, |_| {
            select_inputs(&declared(&["token_type_ids"]), vec![("input_ids", ())])?;
            unreachable!("token_type_ids is never built")
        })
        .unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.has_context());
        assert!(err.to_string().contains("token_type_ids"), "{err}");
    }

    #[test]
    fn new_runs_on_at_most_four_threads() {
        let gliner = OnnxGliner::new(Arc::new(ModelStore::new("/models".into())));

        assert!((1..=MAX_THREADS).contains(&gliner.threads));
    }

    /// A store whose models directory is a file: `ensure` can't create it,
    /// so fails without touching the network.
    fn unusable_models() -> (tempfile::NamedTempFile, Arc<ModelStore>) {
        let file = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(ModelStore::new(file.path().to_path_buf()));
        (file, store)
    }

    #[tokio::test]
    async fn spans_fails_and_can_be_retried_when_the_model_cant_be_fetched() {
        let (_file, models) = unusable_models();
        let gliner = OnnxGliner::new(models);
        let spans = || gliner.spans("Total : 12 €".into(), vec!["total".into()], 0.5, 100);

        let err = spans().await.unwrap_err();
        assert!(err.has_context());
        assert!(gliner.loaded.get().is_none());

        let err = spans().await.unwrap_err();
        assert!(err.has_context());
    }

    #[tokio::test]
    async fn load_fails_and_can_be_retried_when_the_model_cant_be_fetched() {
        let (_file, models) = unusable_models();
        let gliner = OnnxGliner::new(models);

        let err = gliner.load().await.unwrap_err();
        assert!(err.has_context());
        assert!(gliner.loaded.get().is_none());

        let err = gliner.load().await.unwrap_err();
        assert!(err.has_context());
    }

    #[tokio::test]
    async fn spans_never_fetches_the_model_with_nothing_to_look_for() {
        let (_file, models) = unusable_models();
        let gliner = OnnxGliner::new(models);
        let label = || vec!["total".to_string()];

        for (text, labels, max_words) in [
            ("Total : 12 €", vec![], 100),
            (" \n\t", label(), 100),
            ("Total : 12 €", label(), 0),
        ] {
            let spans = gliner.spans(text.into(), labels, 0.5, max_words).await;

            assert!(spans.unwrap().is_empty(), "{text:?}");
        }
    }

    /// The real model's store: `FYDE_TEST_MODELS_DIR` when set, to reuse a
    /// download across runs, else the returned temporary directory.
    fn real_models() -> (tempfile::TempDir, Arc<ModelStore>) {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        (temp, Arc::new(ModelStore::new(dir)))
    }

    /// A synthetic invoice, every name and number made up.
    const INVOICE: &str = "FACTURE N° FA-2024-0117\n\
        Date de facture : 14 mars 2024\n\
        Atelier Exemple SARL, 12 rue des Lilas, 69000 Lyon\n\
        Client : Camille Martin\n\
        Désignation : Réparation de vélo, main d'œuvre 2 h\n\
        Total HT : 150,00 €\n\
        TVA 20 % : 30,00 €\n\
        Montant total TTC : 180,00 €\n\
        Payable à réception par virement.";

    /// Downloads GLiNER-X small (see [`real_models`]) and looks for the
    /// total of a synthetic invoice.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 173 MB model from Hugging Face"]
    async fn gliner_x_small_finds_the_total_of_an_invoice() {
        let (_temp, models) = real_models();
        let gliner = OnnxGliner::new(models);

        let spans = gliner
            .spans(
                INVOICE.to_string(),
                vec![
                    "montant total TTC".to_string(),
                    "date de facture".to_string(),
                ],
                0.3,
                usize::MAX,
            )
            .await
            .unwrap();

        let found: Vec<_> = spans
            .iter()
            .map(|span| (span.label, &INVOICE[span.bytes.clone()], span.score))
            .collect();
        eprintln!("{found:?}");
        let best = |label| {
            found
                .iter()
                .find(|(l, ..)| *l == label)
                .map(|(_, text, _)| *text)
        };
        assert_eq!(best(0), Some("180,00 €"));
        assert_eq!(best(1), Some("14 mars 2024"));
        assert!(found.windows(2).all(|pair| pair[0].2 >= pair[1].2));
    }

    /// Like [`gliner_x_small_finds_the_total_of_an_invoice`], with the total
    /// pushed past the first window by a long preamble.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 173 MB model from Hugging Face"]
    async fn gliner_x_small_finds_the_total_of_an_invoice_past_the_first_window() {
        let (_temp, models) = real_models();
        let gliner = OnnxGliner::new(models);
        let preamble = "Conditions générales : les marchandises voyagent aux risques du \
            destinataire et restent la propriété du vendeur jusqu'au paiement complet. "
            .repeat(30);
        let text = format!("{preamble}\n{INVOICE}");
        assert!(split_words(&preamble).len() > WINDOW);

        let spans = gliner
            .spans(
                text.clone(),
                vec!["montant total TTC".to_string()],
                0.3,
                usize::MAX,
            )
            .await
            .unwrap();

        let found: Vec<_> = spans
            .iter()
            .map(|span| (&text[span.bytes.clone()], span.score))
            .collect();
        eprintln!("{found:?}");
        assert_eq!(found.first().map(|(text, _)| *text), Some("180,00 €"));
        assert!(spans[0].bytes.start > preamble.len());
    }
}
