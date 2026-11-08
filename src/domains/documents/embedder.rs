//! Text embeddings from multilingual-e5-small (`tools::models`), run on
//! device through ONNX Runtime. Ported from `doc-ai-poc`'s `embed.rs`. Only
//! turns text into vectors: what to embed is up to
//! [`super::service::DocumentIndexer`].

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationParams};

use crate::tools::models::{
    self, MULTILINGUAL_E5_SMALL, MULTILINGUAL_E5_SMALL_TOKENIZER, ModelStore,
};
use crate::tools::onnx;
use crate::{ErrorContext as _, ErrorKind, Result};

/// The length of an embedding.
pub(crate) const DIMENSIONS: usize = 384;
/// The model's maximum input length: longer text is truncated.
const MAX_TOKENS: usize = 512;
/// At most this many threads run the model, so indexing never takes over a
/// device's every core.
const MAX_THREADS: usize = 4;
const OUTPUT: &str = "last_hidden_state";

/// Embeds text as [`DIMENSIONS`]-long, L2-normalized vectors.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait Embedder: Send + Sync {
    /// Identifies the model the vectors come from, stored beside them.
    fn model(&self) -> String;

    /// Fetches the model (see [`ModelStore::ensure`]) and loads it, unless
    /// that's already done: a failure is retried on the next call.
    async fn load(&self) -> Result<()>;

    /// Embeds each of `passages` as a passage to retrieve, in order,
    /// [`Self::load`]ing the model first if needed.
    async fn embed_passages(&self, passages: Vec<String>) -> Result<Vec<Vec<f32>>>;
}

/// The [`Embedder`] running multilingual-e5-small. Its session is loaded on
/// the first call that needs it, then kept for the embedder's lifetime.
pub(crate) struct OnnxEmbedder {
    models: Arc<ModelStore>,
    threads: usize,
    loaded: tokio::sync::OnceCell<Arc<Loaded>>,
}

impl OnnxEmbedder {
    pub(crate) fn new(models: Arc<ModelStore>) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get());

        Self {
            models,
            threads: (cores / 2).clamp(1, MAX_THREADS),
            loaded: tokio::sync::OnceCell::new(),
        }
    }

    /// Embeds `query` as a search query, to compare against passages.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "semantic search over the embeddings is to come")
    )]
    pub(crate) async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let loaded = self.loaded().await?;
        let input = query_input(query);

        tokio::task::spawn_blocking(move || loaded.embed(&input))
            .await
            .context("the query embedding task failed")?
    }

    async fn loaded(&self) -> Result<Arc<Loaded>> {
        self.loaded
            .get_or_try_init(|| async {
                let model = self
                    .models
                    .ensure(&MULTILINGUAL_E5_SMALL)
                    .await
                    .context("failed to fetch the embedding model")?;
                let tokenizer = self
                    .models
                    .ensure(&MULTILINGUAL_E5_SMALL_TOKENIZER)
                    .await
                    .context("failed to fetch the embedding model's tokenizer")?;
                let threads = self.threads;

                tokio::task::spawn_blocking(move || Loaded::open(&model, &tokenizer, threads))
                    .await
                    .context("the embedding model loading task failed")?
                    .map(Arc::new)
            })
            .await
            .cloned()
    }
}

#[async_trait]
impl Embedder for OnnxEmbedder {
    fn model(&self) -> String {
        models::file_name(&MULTILINGUAL_E5_SMALL)
    }

    async fn load(&self) -> Result<()> {
        self.loaded().await.map(|_| ())
    }

    async fn embed_passages(&self, passages: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let loaded = self.loaded().await?;

        tokio::task::spawn_blocking(move || {
            passages
                .iter()
                .enumerate()
                .map(|(i, passage)| {
                    loaded
                        .embed(&passage_input(passage))
                        .with_context(|| format!("failed to embed passage {i}"))
                })
                .collect()
        })
        .await
        .context("the passage embedding task failed")?
    }
}

struct Loaded {
    /// `Session::run` takes `&mut self`.
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    inputs: Vec<String>,
}

impl Loaded {
    fn open(model: &Path, tokenizer: &Path, threads: usize) -> Result<Self> {
        let session =
            onnx::session(model, threads).context("failed to load the embedding model")?;
        let mut tokenizer =
            onnx::tokenizer(tokenizer).context("failed to load the embedding model's tokenizer")?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..TruncationParams::default()
            }))
            .map_err(ErrorKind::Tokenizer)
            .context("failed to configure the embedding model's tokenizer")?
            .with_padding(None);
        let inputs = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect();

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            inputs,
        })
    }

    /// Embeds `input`, already prefixed (see [`passage_input`]/[`query_input`]).
    fn embed(&self, input: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(input, true)
            .map_err(ErrorKind::Tokenizer)
            .context("failed to tokenize the text to embed")?;
        let len = encoding.get_ids().len();
        let as_i64 = |values: &[u32]| values.iter().map(|&v| i64::from(v)).collect::<Vec<_>>();

        let mut inputs = Vec::with_capacity(self.inputs.len());
        for name in &self.inputs {
            let values = match name.as_str() {
                "input_ids" => as_i64(encoding.get_ids()),
                "attention_mask" => as_i64(encoding.get_attention_mask()),
                "token_type_ids" => as_i64(encoding.get_type_ids()),
                other => {
                    return Err(ErrorKind::InvalidModel(format!(
                        "the embedding model expects an unknown input {other}"
                    ))
                    .into());
                }
            };
            let tensor = Tensor::from_array(([1usize, len], values))
                .with_context(|| format!("failed to build the embedding model's {name} input"))?;
            inputs.push((name.clone(), tensor));
        }

        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let outputs = session
            .run(inputs)
            .context("failed to run the embedding model")?;
        let output = outputs.get(OUTPUT).ok_or_else(|| {
            ErrorKind::InvalidModel(format!("the embedding model has no {OUTPUT} output"))
        })?;
        let (shape, values) = output
            .try_extract_tensor::<f32>()
            .context("failed to read the embedding model's output")?;

        mean_pool(shape, values, encoding.get_attention_mask())
    }
}

/// What e5 expects for a passage to retrieve. Whitespace is collapsed: PDF
/// text laid out on a grid is mostly spaces, each costing a token.
fn passage_input(text: &str) -> String {
    format!(
        "passage: {}",
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// What e5 expects for a search query.
fn query_input(text: &str) -> String {
    format!("query: {text}")
}

/// Averages the token vectors of a `[1, tokens, DIMENSIONS]` output over the
/// tokens `mask` keeps, then scales the result to unit length.
fn mean_pool(shape: &[i64], values: &[f32], mask: &[u32]) -> Result<Vec<f32>> {
    let expected = [1, mask.len() as i64, DIMENSIONS as i64];
    if shape != expected || values.len() != mask.len() * DIMENSIONS {
        return Err(ErrorKind::InvalidModel(format!(
            "the embedding model returned a {shape:?} output, not {expected:?}"
        ))
        .into());
    }

    let mut pooled = vec![0.0_f32; DIMENSIONS];
    let mut tokens = 0_usize;
    for (token, _) in values
        .as_chunks::<DIMENSIONS>()
        .0
        .iter()
        .zip(mask)
        .filter(|(_, keep)| **keep != 0)
    {
        tokens += 1;
        for (sum, value) in pooled.iter_mut().zip(token) {
            *sum += value;
        }
    }
    if tokens == 0 {
        return Err(ErrorKind::InvalidModel("no token to embed".to_string()).into());
    }

    let norm = pooled
        .iter()
        .map(|sum| (sum / tokens as f32).powi(2))
        .sum::<f32>()
        .sqrt()
        .max(1e-12);
    Ok(pooled
        .into_iter()
        .map(|sum| sum / tokens as f32 / norm)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    /// A `[1, tokens.len(), DIMENSIONS]` output whose token `i` is
    /// `tokens[i]` followed by zeros.
    fn output(tokens: &[&[f32]]) -> (Vec<i64>, Vec<f32>) {
        let values = tokens
            .iter()
            .flat_map(|token| {
                let mut vector = token.to_vec();
                vector.resize(DIMENSIONS, 0.0);
                vector
            })
            .collect();
        (vec![1, tokens.len() as i64, DIMENSIONS as i64], values)
    }

    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, e)| (a - e).abs() < 1e-6),
            "{:?} != {expected:?}",
            &actual[..expected.len()]
        );
        assert!(actual[expected.len()..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn mean_pool_averages_the_tokens_then_normalizes() {
        let (shape, values) = output(&[&[1.0, 0.0], &[3.0, 4.0], &[2.0, 4.0]]);

        let pooled = mean_pool(&shape, &values, &[1, 1, 1]).unwrap();

        // The mean is (2, 8/3); its length is 10/3.
        assert_close(&pooled, &[0.6, 0.8]);
        let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_skips_padding_tokens() {
        let (shape, values) = output(&[&[3.0, 0.0], &[100.0, 100.0], &[0.0, 4.0]]);

        let pooled = mean_pool(&shape, &values, &[1, 0, 1]).unwrap();

        assert_close(&pooled, &[0.6, 0.8]);
    }

    #[test]
    fn mean_pool_leaves_a_zero_vector_at_zero() {
        let (shape, values) = output(&[&[0.0]]);

        let pooled = mean_pool(&shape, &values, &[1]).unwrap();

        assert!(pooled.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn mean_pool_rejects_a_wrong_output_shape() {
        let (_, values) = output(&[&[1.0], &[2.0]]);

        for shape in [
            vec![2, 1, DIMENSIONS as i64],
            vec![1, 3, DIMENSIONS as i64],
            vec![1, 2, 768],
            vec![1, 2 * DIMENSIONS as i64],
        ] {
            let err = mean_pool(&shape, &values, &[1, 1]).unwrap_err();

            assert_eq!(err.code(), ErrorCode::ModelLoadFailed, "{shape:?}");
        }
    }

    #[test]
    fn mean_pool_rejects_values_that_dont_match_the_shape() {
        let (shape, mut values) = output(&[&[1.0], &[2.0]]);
        values.pop();

        let err = mean_pool(&shape, &values, &[1, 1]).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
    }

    #[test]
    fn mean_pool_rejects_an_output_with_no_token_kept() {
        let (shape, values) = output(&[&[1.0]]);

        let err = mean_pool(&shape, &values, &[0]).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
    }

    #[test]
    fn passage_input_prefixes_and_collapses_whitespace() {
        assert_eq!(
            passage_input("Facture\n  Montant   120 €\n\n"),
            "passage: Facture Montant 120 €"
        );
    }

    #[test]
    fn query_input_prefixes_the_query_as_is() {
        assert_eq!(query_input("ma  facture"), "query: ma  facture");
    }

    #[test]
    fn model_names_the_pinned_model_file() {
        let embedder = OnnxEmbedder::new(Arc::new(ModelStore::new("/models".into())));

        assert_eq!(
            embedder.model(),
            "f80102d3f2a1229f-multilingual-e5-small.onnx"
        );
    }

    #[test]
    fn new_runs_on_at_most_four_threads() {
        let embedder = OnnxEmbedder::new(Arc::new(ModelStore::new("/models".into())));

        assert!((1..=MAX_THREADS).contains(&embedder.threads));
    }

    #[tokio::test]
    async fn load_fails_and_can_be_retried_when_the_model_cant_be_fetched() {
        // A file where the models directory should be: `ensure` can't
        // create it, so fails without touching the network.
        let file = tempfile::NamedTempFile::new().unwrap();
        let embedder = OnnxEmbedder::new(Arc::new(ModelStore::new(file.path().to_path_buf())));

        let err = embedder.load().await.unwrap_err();
        assert!(err.has_context());
        assert!(embedder.loaded.get().is_none());

        embedder.load().await.unwrap_err();
        let err = embedder
            .embed_passages(vec!["text".to_string()])
            .await
            .unwrap_err();
        assert!(err.has_context());
    }

    /// Downloads multilingual-e5-small (into `FYDE_TEST_MODELS_DIR` when set,
    /// to reuse it across runs) and checks it ranks an invoice above a
    /// payslip for a question about an invoice.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 135 MB model from Hugging Face"]
    async fn multilingual_e5_ranks_an_invoice_above_a_payslip_for_an_invoice_query() {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        let embedder = OnnxEmbedder::new(Arc::new(ModelStore::new(dir)));

        let passages = embedder
            .embed_passages(vec![
                "facture-electricite-mars\nFacture d'électricité\nPériode : mars\n\
                 Consommation : 320 kWh\nMontant TTC à payer : 84,20 €\nÉchéance : 15 avril"
                    .to_string(),
                "bulletin-de-paie-mars\nBulletin de paie\nPériode : mars\n\
                 Salaire de base : 2 400,00 €\nCotisations salariales : 520,00 €\n\
                 Net à payer avant impôt : 1 880,00 €"
                    .to_string(),
            ])
            .await
            .unwrap();
        let query = embedder
            .embed_query("combien dois-je payer pour ma facture d'électricité ?")
            .await
            .unwrap();

        let score = |passage: &[f32]| passage.iter().zip(&query).map(|(p, q)| p * q).sum::<f32>();
        let (invoice, payslip) = (score(&passages[0]), score(&passages[1]));
        eprintln!("invoice: {invoice:.4}, payslip: {payslip:.4}");
        assert!(passages.iter().all(|passage| passage.len() == DIMENSIONS));
        assert!(invoice > payslip, "invoice {invoice} <= payslip {payslip}");
    }
}
