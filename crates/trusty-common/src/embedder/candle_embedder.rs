//! Candle-backed embedder for `sentence-transformers/all-MiniLM-L6-v2`
//! (issue #54).
//!
//! Why: the default fastembed/ONNX path runs the BERT forward pass through
//! ONNX Runtime, which on Apple Silicon needs the CoreML execution provider
//! for Metal/ANE acceleration. CoreML EP session-init has been observed to
//! spike virtual RSS to ~72 GB on large indexing batches, tripping macOS
//! jetsam SIGKILL. Candle talks to Metal directly through the
//! `candle_core::Device::Metal` backend, bypassing CoreML entirely while
//! preserving the 384-dim L2-normalised f32 embedding contract that the rest
//! of the workspace (HNSW search, memory-core, etc.) is built on.
//!
//! What: loads `all-MiniLM-L6-v2` from HuggingFace Hub via `hf-hub`,
//! tokenises with the upstream `tokenizers` crate, runs `BertModel::forward`
//! on either Metal (Apple Silicon when available) or CPU, mean-pools the
//! last hidden state masking padding tokens, and L2-normalises each row
//! before returning. Implements the shared [`Embedder`] trait so it slots
//! into any existing embed pool without per-call branching.
//!
//! Test: see the `tests` module — all tests are `#[ignore]` because they
//! download ~90 MB from HuggingFace on first run, mirroring the existing
//! convention for `FastEmbedder` (which downloads its ONNX model the same
//! way). Run them locally with
//! `cargo test -p trusty-common --features embedder-candle -- --ignored`.

#![cfg(feature = "embedder-candle")]

use anyhow::{Context, Result};
use async_trait::async_trait;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE, HiddenAct};
use hf_hub::{Repo, RepoType, api::sync::Api};
use thiserror::Error;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::{EMBED_DIM, Embedder};

/// HuggingFace repo id for the sentence-transformers MiniLM model.
///
/// Why: pinned as a constant so the model source is auditable in one place
/// and future swaps land in a single edit.
/// What: 384-dim BERT-small, MIT-licensed, ~90 MB safetensors.
/// Test: covered transitively — every `CandleEmbedder::new()` call resolves
/// this string.
const MODEL_REPO: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Maximum tokenisation length.
///
/// Why: MiniLM-L6-v2 is trained on 128-token inputs; longer inputs only
/// waste compute via padding/truncation.
/// What: hard cap applied via `TruncationParams::max_length`.
/// Test: not directly tested — exercised by every embed call.
const MAX_SEQ_LEN: usize = 128;

/// Structured error surface for `CandleEmbedder`.
///
/// Why: per project conventions library code uses `thiserror` for typed
/// errors so callers can match on failure modes. The wrapper preserves the
/// underlying `anyhow::Error` for diagnostic logging while still letting
/// downstream code distinguish init failures from per-call embed failures.
/// What: two variants — `Init` for one-shot construction failures (model
/// download, weights load, device init) and `Embed` for runtime tokenise /
/// forward-pass failures.
/// Test: covered indirectly via `Result` returns from `new()` / `embed()`.
#[derive(Debug, Error)]
pub enum CandleEmbedderError {
    /// Failure during one-shot model construction (network, parse, device).
    #[error("candle embedder init failed: {0}")]
    Init(#[source] anyhow::Error),
    /// Failure during a per-call embed dispatch.
    #[error("candle embedder embed failed: {0}")]
    Embed(#[source] anyhow::Error),
}

/// Candle-backed all-MiniLM-L6-v2 embedder.
///
/// Why: holds the loaded `BertModel` + `Tokenizer` + active `Device` so the
/// model-load cost is paid once at process start. The struct is `Send +
/// Sync` because all candle handles are owned by value and the forward pass
/// takes `&self` internally.
/// What: public surface is `new()` / `new_with_device()` (one-shot model
/// load) plus the [`Embedder`] trait impl (tokenise → forward → mean-pool →
/// L2-normalise).
/// Test: `candle_cpu_embeds_single_text`, `candle_batch_embeds_consistently`,
/// `candle_similar_texts_closer_than_dissimilar` — all `#[ignore]`d behind
/// the model download.
pub struct CandleEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    dim: usize,
}

impl CandleEmbedder {
    /// Load `all-MiniLM-L6-v2` via hf-hub, preferring Metal on Apple Silicon
    /// when `use_metal` is true and the host supports it.
    ///
    /// Why: callers want a single one-shot constructor that mirrors
    /// `FastEmbedder::new()`. The boolean lets test environments and
    /// containers without GPU passthrough force CPU without consulting
    /// `cfg!` macros at the call site.
    /// What: resolves a `Device` (Metal on aarch64-macos if requested and
    /// available, else CPU), fetches `config.json` / `tokenizer.json` /
    /// `model.safetensors` from HF Hub into the local cache, configures
    /// padding + truncation, mmaps the weights via `VarBuilder`, and
    /// constructs the BERT model. Returns a typed
    /// `CandleEmbedderError::Init` on any failure.
    /// Test: every test in the `tests` module exercises this code path.
    pub fn new(use_metal: bool) -> Result<Self, CandleEmbedderError> {
        let device = pick_device(use_metal);
        Self::new_with_device(device)
    }

    /// Construct with a caller-provided device.
    ///
    /// Why: lets advanced callers (e.g. multi-GPU hosts, deterministic
    /// tests) pin to an exact `Device` instead of going through the
    /// `use_metal` heuristic.
    /// What: same load path as `new()` but skips the device selector.
    /// Test: exercised transitively via `new()`.
    pub fn new_with_device(device: Device) -> Result<Self, CandleEmbedderError> {
        let metal = matches!(device, Device::Metal(_));
        tracing::info!(
            "trusty-common::embedder: building CandleEmbedder ({})",
            if metal { "metal" } else { "cpu" }
        );

        let api = Api::new()
            .context("hf_hub api init")
            .map_err(CandleEmbedderError::Init)?;
        let repo = api.repo(Repo::new(MODEL_REPO.to_string(), RepoType::Model));
        let config_filename = repo
            .get("config.json")
            .context("download config.json")
            .map_err(CandleEmbedderError::Init)?;
        let tokenizer_filename = repo
            .get("tokenizer.json")
            .context("download tokenizer.json")
            .map_err(CandleEmbedderError::Init)?;
        let weights_filename = repo
            .get("model.safetensors")
            .context("download model.safetensors")
            .map_err(CandleEmbedderError::Init)?;

        let config_bytes = std::fs::read(&config_filename)
            .context("read config.json")
            .map_err(CandleEmbedderError::Init)?;
        let mut config: Config = serde_json::from_slice(&config_bytes)
            .context("parse bert config")
            .map_err(CandleEmbedderError::Init)?;
        // sentence-transformers configs historically ship with
        // `hidden_act: "gelu"`; candle's BERT impl uses an explicit enum, so
        // pin the approximate variant to keep older configs working.
        config.hidden_act = HiddenAct::GeluApproximate;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_filename)
            .map_err(|e| CandleEmbedderError::Init(anyhow::anyhow!(e)))?;
        tokenizer
            .with_padding(Some(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                ..Default::default()
            }))
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_LEN,
                ..Default::default()
            }))
            .map_err(|e| CandleEmbedderError::Init(anyhow::anyhow!(e)))?;

        // SAFETY: `from_mmaped_safetensors` mmaps the file; the lifetime is
        // tied to the returned `VarBuilder`, which `BertModel::load`
        // consumes here. After construction the mmap is no longer referenced.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                std::slice::from_ref(&weights_filename),
                DTYPE,
                &device,
            )
            .context("load safetensors weights")
            .map_err(CandleEmbedderError::Init)?
        };
        let model = BertModel::load(vb, &config)
            .context("instantiate bert model")
            .map_err(CandleEmbedderError::Init)?;

        Ok(Self {
            model,
            tokenizer,
            device,
            dim: EMBED_DIM,
        })
    }

    /// Active candle device (Metal or CPU).
    ///
    /// Why: lets callers log or branch on which backend they actually got
    /// (Metal init can silently fall back to CPU on hosts without GPU
    /// passthrough).
    /// What: returns a reference to the owned `Device`.
    /// Test: not directly tested — value is implementation-defined per host.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Synchronous embed primitive (slice of `&str`).
    ///
    /// Why: candle tensor math is CPU- or GPU-bound and not naturally async.
    /// Exposing the sync version lets advanced callers integrate without
    /// going through the trait's `async fn embed_batch`, which the
    /// [`Embedder`] impl uses internally.
    /// What: tokenise → stack `[B, T]` `input_ids` + `attention_mask`
    /// tensors on the active device → `BertModel::forward` → mean-pool the
    /// last hidden state along the sequence axis (masking pads) →
    /// L2-normalise each row.
    /// Test: covered by `candle_cpu_embeds_single_text` and
    /// `candle_batch_embeds_consistently`.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, CandleEmbedderError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_string()).collect();
        let encodings = self
            .tokenizer
            .encode_batch(owned, true)
            .map_err(|e| CandleEmbedderError::Embed(anyhow::anyhow!(e)))
            .context("tokenize batch")
            .map_err(CandleEmbedderError::Embed)?;

        let batch = encodings.len();
        let seq_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);
        if seq_len == 0 {
            return Ok(vec![vec![0.0; self.dim]; batch]);
        }

        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for enc in &encodings {
            ids.extend_from_slice(enc.get_ids());
            mask.extend_from_slice(enc.get_attention_mask());
        }

        let input_ids = Tensor::from_vec(ids, (batch, seq_len), &self.device)
            .context("stack input_ids")
            .map_err(CandleEmbedderError::Embed)?;
        let attn_mask_u32 = Tensor::from_vec(mask, (batch, seq_len), &self.device)
            .context("stack attention_mask")
            .map_err(CandleEmbedderError::Embed)?;
        let token_type_ids = input_ids
            .zeros_like()
            .context("zeros for token_type_ids")
            .map_err(CandleEmbedderError::Embed)?;

        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attn_mask_u32))
            .context("bert forward")
            .map_err(CandleEmbedderError::Embed)?;

        // Mean-pool along the sequence axis weighted by the attention mask.
        // hidden: [B, T, D] (f32). mask: [B, T] (u32 → f32, broadcast to D).
        let mask_f = attn_mask_u32
            .to_dtype(DType::F32)
            .context("mask to f32")
            .map_err(CandleEmbedderError::Embed)?;
        let mask_b_t_1 = mask_f
            .unsqueeze(2)
            .context("unsqueeze mask")
            .map_err(CandleEmbedderError::Embed)?;
        let masked = hidden
            .broadcast_mul(&mask_b_t_1)
            .context("apply mask to hidden")
            .map_err(CandleEmbedderError::Embed)?;
        let summed = masked
            .sum(1)
            .context("sum along seq")
            .map_err(CandleEmbedderError::Embed)?;
        let counts = mask_f
            .sum(1)
            .context("sum mask along seq")
            .map_err(CandleEmbedderError::Embed)?
            .clamp(1e-9_f64, f64::INFINITY)
            .context("clamp counts")
            .map_err(CandleEmbedderError::Embed)?;
        let counts_b_1 = counts
            .unsqueeze(1)
            .context("unsqueeze counts")
            .map_err(CandleEmbedderError::Embed)?;
        let pooled = summed
            .broadcast_div(&counts_b_1)
            .context("mean pool")
            .map_err(CandleEmbedderError::Embed)?;

        // L2-normalise along the embedding axis so cosine similarity reduces
        // to a dot product downstream.
        let norms = pooled
            .sqr()
            .context("square pooled")
            .map_err(CandleEmbedderError::Embed)?
            .sum_keepdim(1)
            .context("sum squares")
            .map_err(CandleEmbedderError::Embed)?
            .sqrt()
            .context("sqrt norms")
            .map_err(CandleEmbedderError::Embed)?
            .clamp(1e-12_f64, f64::INFINITY)
            .context("clamp norms")
            .map_err(CandleEmbedderError::Embed)?;
        let normed = pooled
            .broadcast_div(&norms)
            .context("l2 normalise")
            .map_err(CandleEmbedderError::Embed)?;

        let out: Vec<Vec<f32>> = normed
            .to_vec2::<f32>()
            .context("materialise embeddings to host")
            .map_err(CandleEmbedderError::Embed)?;
        Ok(out)
    }
}

/// Shared-trait impl so `CandleEmbedder` slots into any
/// `Arc<dyn trusty_common::embedder::Embedder>` consumer.
///
/// Why: the embed pool, memory-core ingest path, and search query path all
/// take `Arc<dyn Embedder>`; implementing the trait here is what makes this
/// embedder a drop-in replacement for `FastEmbedder` rather than a separate
/// surface every caller has to special-case.
/// What: forwards to the sync [`CandleEmbedder::embed`] primitive. The
/// trait is `async` but the candle forward pass is CPU/GPU-bound (not I/O),
/// so we deliberately do NOT wrap the call in `spawn_blocking` here — the
/// pool / caller already owns the worker-task scheduling decision and
/// blocking inside a worker is acceptable for this short-lived call.
/// Test: covered indirectly by the sync `embed()` tests; the trait method
/// is the same code path.
#[async_trait]
impl Embedder for CandleEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.embed(&refs).map_err(anyhow::Error::from)
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    // #9275: the tokenizer truncates at `MAX_SEQ_LEN` (128), specials
    // included, so the window stays below the model's 256.
    fn input_window(&self) -> Option<usize> {
        Some(super::fast_embedder::MINILM_MAX_SEQ_LENGTH.min(MAX_SEQ_LEN))
    }
}

/// Pick the best available device.
///
/// Why: candle exposes Metal as an opt-in device; constructing it can fail
/// in containers / VMs without GPU passthrough, so we fall back to CPU
/// rather than panic the caller. The `use_metal` flag lets the caller opt
/// out entirely without needing `cfg!` macros at the call site.
/// What: returns `Device::new_metal(0)` on `aarch64-macos` when `use_metal`
/// is true and the device builds, else `Device::Cpu`.
/// Test: not directly tested — value is implementation-defined per host.
fn pick_device(use_metal: bool) -> Device {
    if use_metal {
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            match Device::new_metal(0) {
                Ok(d) => return d,
                Err(e) => {
                    tracing::warn!(
                        "trusty-common::embedder: metal device init failed ({e}); \
                         falling back to CPU"
                    );
                }
            }
        }
    }
    Device::Cpu
}

#[cfg(test)]
#[cfg(feature = "embedder-candle")]
mod tests {
    use super::*;

    /// Why: regression guard — the rest of the pipeline assumes 384-dim
    /// vectors. A model swap that changed this would silently break every
    /// downstream consumer.
    /// What: embed one short string on CPU and assert the result is exactly
    /// `EMBED_DIM` floats with L2 norm ≈ 1.0.
    /// Test: this test.
    #[test]
    #[ignore = "downloads ~90 MB from HuggingFace; run with --include-ignored"]
    fn candle_cpu_embeds_single_text() {
        let emb = CandleEmbedder::new(false).expect("load candle embedder");
        assert_eq!(emb.dimension(), EMBED_DIM);
        let out = emb.embed(&["hello world"]).expect("embed succeeds");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), EMBED_DIM);

        // L2 norm should be ~1.0 (we normalise inside the embedder).
        let norm: f32 = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "L2 norm should be ~1.0 (got {norm})"
        );
    }

    /// Why: determinism is a load-bearing property for any caching layer
    /// downstream (LRU embedding cache, content-hash skip in reindex). If
    /// two calls on the same input drift, every cache hit becomes a silent
    /// correctness bug.
    /// What: embed the same string twice, dot-product the (already
    /// normalised) vectors, assert cosine ≈ 1.0.
    /// Test: this test.
    #[test]
    #[ignore = "downloads ~90 MB from HuggingFace; run with --include-ignored"]
    fn candle_batch_embeds_consistently() {
        let emb = CandleEmbedder::new(false).expect("load candle embedder");
        let out_a = emb.embed(&["the quick brown fox"]).expect("first embed");
        let out_b = emb.embed(&["the quick brown fox"]).expect("second embed");
        let cos: f32 = out_a[0]
            .iter()
            .zip(out_b[0].iter())
            .map(|(a, b)| a * b)
            .sum();
        assert!(cos > 0.999, "self-cosine should be > 0.999 (got {cos})");
    }

    /// Why: end-to-end sanity that the model produces a semantically useful
    /// embedding space — semantically similar inputs must be closer than
    /// semantically dissimilar ones. Without this guard a misloaded model
    /// (e.g. weights swapped, wrong pooling) could pass dimension and
    /// determinism checks while silently degrading search quality.
    /// What: embed three tokens, assert cos(dog, cat) > cos(dog, airplane).
    /// Test: this test.
    #[test]
    #[ignore = "downloads ~90 MB from HuggingFace; run with --include-ignored"]
    fn candle_similar_texts_closer_than_dissimilar() {
        let emb = CandleEmbedder::new(false).expect("load candle embedder");
        let out = emb
            .embed(&["dog", "cat", "airplane"])
            .expect("embed succeeds");
        let cos = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let dog_cat = cos(&out[0], &out[1]);
        let dog_airplane = cos(&out[0], &out[2]);
        assert!(
            dog_cat > dog_airplane,
            "cos(dog, cat) should exceed cos(dog, airplane) (got {dog_cat} vs {dog_airplane})"
        );
    }
}
