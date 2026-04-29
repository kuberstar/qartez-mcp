// Rust guideline compliant 2026-04-15

//! ONNX-based embedding model for semantic code search.
//!
//! Wraps a Jina Code v2 ONNX model and its tokenizer behind a single
//! [`EmbeddingModel`] struct. The model is loaded once and reused across
//! queries. All vectors are L2-normalized so cosine similarity reduces to
//! a dot product.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

/// Dimensionality of the Jina Code v2 embedding vectors.
pub const EMBEDDING_DIM: usize = 768;

/// Default batch size for encoding. Balances memory use vs. throughput
/// on CPU. Each batch is a single ONNX `run` call; larger batches amortize
/// per-call overhead but increase peak RSS.
const DEFAULT_BATCH_SIZE: usize = 64;

/// Expected ONNX model filename inside the model directory.
pub const MODEL_FILENAME: &str = "model.onnx";

/// Expected tokenizer filename inside the model directory.
pub const TOKENIZER_FILENAME: &str = "tokenizer.json";

/// Maximum token count the model supports per input sequence.
const MAX_TOKENS: usize = 8192;

/// Pre-loaded ONNX embedding model and tokenizer.
///
/// Wraps the ONNX Runtime session and HuggingFace fast tokenizer. Created
/// once via [`EmbeddingModel::load`] and shared behind an `Arc` for the
/// lifetime of the server.
#[derive(Clone)]
pub struct EmbeddingModel {
    // ONNX `Session` is not `Send`/`Sync`; the `Mutex` serializes all
    // inference calls through a single lock. This limits throughput but
    // is acceptable: the `semantic` feature is opt-in and typical query
    // workloads are sequential. A session pool would allow parallelism.
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<tokenizers::Tokenizer>,
    // Some recent re-exports of `jina-embeddings-v2-base-code` drop the
    // optional `token_type_ids` input from the ONNX graph. Feeding an input
    // the graph does not declare aborts inference with `Invalid input name`,
    // so we capture the declared input set at load time and only feed
    // `token_type_ids` when the model actually accepts it.
    accepts_token_type_ids: bool,
}

impl std::fmt::Debug for EmbeddingModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingModel")
            .field("dim", &EMBEDDING_DIM)
            .finish_non_exhaustive()
    }
}

impl EmbeddingModel {
    /// Load the ONNX model and tokenizer from `model_dir`.
    ///
    /// The directory must contain `model.onnx` and `tokenizer.json` as
    /// downloaded by `qartez-setup`.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model_path = model_dir.join(MODEL_FILENAME);
        let tokenizer_path = model_dir.join(TOKENIZER_FILENAME);

        if !model_path.exists() {
            bail!(
                "ONNX model not found at {}. Run `qartez-setup` to download it.",
                model_path.display()
            );
        }
        if !tokenizer_path.exists() {
            bail!(
                "tokenizer not found at {}. Run `qartez-setup` to download it.",
                tokenizer_path.display()
            );
        }

        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("ONNX session builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("ONNX optimization level: {e}"))?
            .with_intra_threads(4)
            .map_err(|e| anyhow::anyhow!("ONNX intra threads: {e}"))?
            .commit_from_file(&model_path)
            .map_err(|e| anyhow::anyhow!("failed to load ONNX model: {e}"))?;

        let accepts_token_type_ids = session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {e}"))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
            accepts_token_type_ids,
        })
    }

    /// Encode a batch of text inputs into L2-normalized f32 vectors.
    ///
    /// Returns one `Vec<f32>` of length [`EMBEDDING_DIM`] per input. Inputs
    /// longer than the model's context window are silently truncated by the
    /// tokenizer.
    pub fn encode_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());

        for chunk in texts.chunks(DEFAULT_BATCH_SIZE) {
            let batch = self.encode_chunk(chunk)?;
            all_embeddings.extend(batch);
        }

        Ok(all_embeddings)
    }

    /// Encode a single text string.
    pub fn encode_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut batch = self.encode_batch(&[text])?;
        batch
            .pop()
            .context("encode_batch returned empty result for single input")
    }

    /// Internal: encode a single chunk of up to `DEFAULT_BATCH_SIZE` texts.
    fn encode_chunk(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?;

        let batch_size = encodings.len();

        // Determine the maximum sequence length in this batch, capped at
        // the model's context window.
        let max_len = encodings
            .iter()
            .map(|enc| enc.get_ids().len().min(MAX_TOKENS))
            .max()
            .unwrap_or(0);

        // Build padded input tensors. `token_type_ids` is only allocated and
        // built when the loaded ONNX graph actually declares that input.
        let mut input_ids = vec![0i64; batch_size * max_len];
        let mut attention_mask = vec![0i64; batch_size * max_len];
        let mut token_type_ids = if self.accepts_token_type_ids {
            vec![0i64; batch_size * max_len]
        } else {
            Vec::new()
        };

        for (i, enc) in encodings.iter().enumerate() {
            let ids = enc.get_ids();
            let mask = enc.get_attention_mask();
            let type_ids = enc.get_type_ids();
            let seq_len = ids.len().min(max_len);

            for j in 0..seq_len {
                input_ids[i * max_len + j] = ids[j] as i64;
                attention_mask[i * max_len + j] = mask[j] as i64;
                if self.accepts_token_type_ids {
                    token_type_ids[i * max_len + j] = type_ids[j] as i64;
                }
            }
        }

        let shape = vec![batch_size as i64, max_len as i64];

        // Keep a copy of attention_mask for mean-pooling after inference
        // (the original is consumed by tensor creation).
        let mask_copy = attention_mask.clone();

        let ids_tensor = ort::value::Tensor::from_array((shape.clone(), input_ids))
            .map_err(|e| anyhow::anyhow!("tensor creation: {e}"))?;
        let mask_tensor = ort::value::Tensor::from_array((shape.clone(), attention_mask))
            .map_err(|e| anyhow::anyhow!("tensor creation: {e}"))?;

        let mut inputs = ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
        ];
        if self.accepts_token_type_ids {
            let type_tensor = ort::value::Tensor::from_array((shape, token_type_ids))
                .map_err(|e| anyhow::anyhow!("tensor creation: {e}"))?;
            inputs.push((
                std::borrow::Cow::Borrowed("token_type_ids"),
                ort::session::SessionInputValue::from(type_tensor),
            ));
        }

        let mut session = self
            .session
            .lock()
            .map_err(|e| anyhow::anyhow!("session lock poisoned: {e}"))?;
        let outputs = session
            .run(inputs)
            .map_err(|e| anyhow::anyhow!("ONNX inference: {e}"))?;

        // The model outputs a tensor of shape [batch, seq_len, hidden_dim].
        // Mean-pool over the sequence dimension using the attention mask.
        let (out_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("tensor extraction: {e}"))?;
        let hidden_dim = hidden_dim_from_shape(out_shape)?;
        // `data` is a flat slice of length batch_size * max_len * hidden_dim.
        // Index as `data[i * max_len * hidden_dim + j * hidden_dim + k]`.
        let stride_batch = max_len * hidden_dim;
        let stride_seq = hidden_dim;

        let mut results = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            let mut pooled = vec![0.0f32; hidden_dim];
            let mut token_count = 0.0f32;

            for j in 0..max_len {
                if mask_copy[i * max_len + j] == 1 {
                    token_count += 1.0;
                    let offset = i * stride_batch + j * stride_seq;
                    for k in 0..hidden_dim {
                        pooled[k] += data[offset + k];
                    }
                }
            }

            if token_count > 0.0 {
                for val in &mut pooled {
                    *val /= token_count;
                }
            }

            l2_normalize(&mut pooled);
            results.push(pooled);
        }

        Ok(results)
    }
}

/// Extract the hidden dimension from an ONNX output tensor shape. The model
/// is expected to return `[batch, seq_len, hidden_dim]`; the hidden dim is
/// the last axis. Returns an error if the shape is empty or if the last axis
/// is zero, both of which would otherwise silently produce zero-length
/// embeddings and poison downstream similarity scores.
fn hidden_dim_from_shape(out_shape: &[i64]) -> Result<usize> {
    out_shape
        .last()
        .copied()
        .filter(|d| *d > 0)
        .map(|d| d as usize)
        .ok_or_else(|| {
            anyhow::anyhow!("ONNX model returned empty/zero output shape; cannot derive hidden_dim")
        })
}

/// L2-normalize a vector in place. Returns a zero vector for zero-norm inputs.
fn l2_normalize(vec: &mut [f32]) {
    let norm_sq: f32 = vec.iter().map(|x| x * x).sum();
    if norm_sq > 0.0 {
        let inv_norm = 1.0 / norm_sq.sqrt();
        for v in vec.iter_mut() {
            *v *= inv_norm;
        }
    }
}

/// Cosine similarity between two L2-normalized vectors (reduces to dot product).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vector dimension mismatch");
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Serialize an f32 vector to raw little-endian bytes for SQLite BLOB storage.
/// 768 dims * 4 bytes = 3072 bytes per vector.
pub fn vec_to_blob(vec: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for &v in vec {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// Deserialize raw little-endian bytes from a SQLite BLOB back to an f32 vector.
pub fn blob_to_vec(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Reciprocal Rank Fusion (RRF) merge of two ranked result lists.
///
/// Each input is an ordered list of `(id, payload)` pairs. The function
/// returns the top `limit` results sorted by descending RRF score. The
/// `k` parameter controls how much lower ranks contribute (standard
/// default: 60).
///
/// RRF score for an item appearing at rank `r_i` in list `i`:
///   `score = sum(1.0 / (k + r_i))` over all lists containing the item.
pub fn rrf_merge<T: Clone>(lists: &[&[(i64, T)]], k: f64, limit: usize) -> Vec<(i64, T, f64)> {
    use std::collections::HashMap;

    // Accumulate RRF scores. Keep the payload from the first list that
    // contains the item (both lists carry equivalent data for the same id).
    let mut scores: HashMap<i64, (f64, T)> = HashMap::new();

    for list in lists {
        for (rank, (id, payload)) in list.iter().enumerate() {
            let rrf_score = 1.0 / (k + (rank + 1) as f64);
            scores
                .entry(*id)
                .and_modify(|(s, _)| *s += rrf_score)
                .or_insert_with(|| (rrf_score, payload.clone()));
        }
    }

    let mut merged: Vec<(i64, T, f64)> = scores
        .into_iter()
        .map(|(id, (score, payload))| (id, payload, score))
        .collect();

    merged.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(limit);
    merged
}

/// Returns the default model directory path: `~/.qartez/models/jina-embeddings-v2-base-code/`.
pub fn default_model_dir() -> Option<std::path::PathBuf> {
    dirs_next().map(|home| {
        home.join(".qartez")
            .join("models")
            .join("jina-embeddings-v2-base-code")
    })
}

/// Platform-independent home directory.
fn dirs_next() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector() {
        let mut v = vec![1.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert!((v[0] - 1.0).abs() < 1e-6);
        assert!((v[1]).abs() < 1e-6);
        assert!((v[2]).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_produces_unit_norm() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn cosine_similarity_identical() {
        let mut a = vec![1.0, 2.0, 3.0];
        l2_normalize(&mut a);
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn hidden_dim_from_valid_shape() {
        assert_eq!(hidden_dim_from_shape(&[1, 10, 768]).unwrap(), 768);
        assert_eq!(hidden_dim_from_shape(&[4, 2, 1024]).unwrap(), 1024);
        assert_eq!(hidden_dim_from_shape(&[42]).unwrap(), 42);
    }

    #[test]
    fn hidden_dim_from_empty_shape_errors() {
        let err = hidden_dim_from_shape(&[]).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("empty/zero output shape"),
            "message must name the failure mode, got: {msg}"
        );
    }

    #[test]
    fn hidden_dim_from_zero_last_axis_errors() {
        let err = hidden_dim_from_shape(&[1, 10, 0]).unwrap_err();
        assert!(format!("{err}").contains("empty/zero output shape"));
    }

    #[test]
    fn hidden_dim_rejects_negative_last_axis() {
        // i64 can be negative via sign-extension of untrusted model outputs.
        // Pre-fix, casting -1i64 to usize would wrap to usize::MAX and panic
        // on the next allocation. The > 0 filter catches it cleanly.
        let err = hidden_dim_from_shape(&[1, 10, -1]).unwrap_err();
        assert!(format!("{err}").contains("empty/zero output shape"));
    }

    #[test]
    fn blob_round_trip() {
        let original = vec![1.0f32, -2.5, 3.25, 0.0, f32::MAX, f32::MIN];
        let blob = vec_to_blob(&original);
        assert_eq!(blob.len(), original.len() * 4);
        let restored = blob_to_vec(&blob);
        assert_eq!(original, restored);
    }

    #[test]
    fn rrf_merge_basic() {
        let list_a: Vec<(i64, String)> = vec![(1, "a".into()), (2, "b".into()), (3, "c".into())];
        let list_b: Vec<(i64, String)> = vec![(3, "c".into()), (1, "a".into()), (4, "d".into())];
        let merged = rrf_merge(&[&list_a, &list_b], 60.0, 10);

        // Items 1 and 3 appear in both lists, so they should rank highest.
        assert!(merged.len() >= 2);
        let top_ids: Vec<i64> = merged.iter().map(|(id, _, _)| *id).collect();
        assert!(top_ids.contains(&1));
        assert!(top_ids.contains(&3));
        // Both-list items must score higher than single-list items.
        let score_1 = merged.iter().find(|(id, _, _)| *id == 1).unwrap().2;
        let score_4 = merged.iter().find(|(id, _, _)| *id == 4).unwrap().2;
        assert!(score_1 > score_4);
    }

    #[test]
    fn rrf_merge_respects_limit() {
        let list: Vec<(i64, ())> = (0..100).map(|i| (i, ())).collect();
        let merged = rrf_merge(&[&list], 60.0, 5);
        assert_eq!(merged.len(), 5);
    }

    #[test]
    fn rrf_merge_empty_lists() {
        let empty: Vec<(i64, ())> = vec![];
        let merged = rrf_merge(&[&empty, &empty], 60.0, 10);
        assert!(merged.is_empty());
    }

    #[test]
    fn default_model_dir_is_under_qartez() {
        if let Some(dir) = default_model_dir() {
            assert!(dir.to_string_lossy().contains(".qartez"));
            assert!(dir.to_string_lossy().contains("jina-embeddings"));
        }
    }
}
