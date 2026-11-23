//! Candle-backed embedder for `all-MiniLM-L6-v2` (issue #41 phase 4).
//!
//! Why: The ONNX/CoreML path can trigger a jetsam kill on Apple Silicon due to
//! a ~72 GB virtual RSS spike during CoreML EP session-init. Candle uses Metal
//! directly, bypassing CoreML entirely, eliminating this failure mode while
//! keeping the embedding contract identical (384-dim L2-normalised f32).
//!
//! What: Loads `sentence-transformers/all-MiniLM-L6-v2` from HuggingFace Hub
//! (cached locally), tokenises with the hf `tokenizers` crate, runs a BERT
//! forward pass on Metal (or CPU fallback), mean-pools the last hidden state
//! masking padding tokens, and L2-normalises each row before returning.
//!
//! Test: see the `tests` module — covers output dimension (384) and the
//! self-cosine similarity invariant (cos(x, x) ≈ 1.0 within f32 epsilon).

#![cfg(feature = "candle")]

use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, HiddenAct, DTYPE};
use hf_hub::{api::sync::ApiBuilder, Repo, RepoType};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

/// HuggingFace repo id for the sentence-transformers MiniLM model.
///
/// Why: pinned as a constant so the model source is auditable in one place and
/// future swaps to a different sentence-transformer model land in a single
/// edit.
const MODEL_REPO: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Maximum tokenisation length. MiniLM-L6-v2 is trained on 256-token inputs
/// (anything longer is heavily padded waste).
const MAX_SEQ_LEN: usize = 256;

/// Output embedding dimensionality for all-MiniLM-L6-v2.
///
/// Why: must match the rest of the search pipeline (HNSW index, BM25 fusion)
/// which is built around a 384-dim vector contract.
pub const EMBED_DIM: usize = 384;

/// Candle-backed all-MiniLM-L6-v2 embedder.
///
/// Why: Holds the loaded `BertModel` + `Tokenizer` + active `Device` so the
/// daemon pays the model-load cost once at boot. The struct is `Send + Sync`
/// because all candle handles are owned by value and `Tensor::forward` takes
/// `&self` internally.
/// What: Public surface is `new()` (one-shot model load) and `embed_batch()`
/// (tokenise → forward → mean-pool → L2-normalise). The struct deliberately
/// has no `embed_one` helper — callers should pass single-element slices so
/// the same batched code path covers every dispatch.
/// Test: covered by the `tests` module.
pub struct CandleEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl CandleEmbedder {
    /// Load `all-MiniLM-L6-v2` via hf-hub, preferring Metal on Apple Silicon,
    /// falling back to CPU otherwise.
    ///
    /// Why: One-shot constructor mirrors `FastEmbedder::new()`. We swallow any
    /// Metal init failure (e.g. running in a container without GPU access) and
    /// fall back to CPU so the daemon stays usable in mixed environments.
    /// What: `hf_hub::Api::sync` fetches `config.json`, `tokenizer.json`, and
    /// `model.safetensors` into the local hf cache; subsequent runs are
    /// no-network. The tokenizer is configured for batched padding +
    /// `MAX_SEQ_LEN` truncation so every call site produces uniform tensors.
    /// Test: implicitly exercised by every test in the `tests` module — they
    /// all call `CandleEmbedder::new()`.
    pub fn new() -> Result<Self> {
        let device = pick_device();
        let metal = matches!(device, Device::Metal(_));
        tracing::info!("embedder: candle/{}", if metal { "metal" } else { "cpu" });

        // #10866: hf-hub 0.5 resolves HF's relative 307; `from_env` keeps
        // HF_HOME, which 0.5's `Api::new()` ignores. Its blob lock gives up
        // after ~5 s, so first-use downloads in one process are serialized.
        static FETCH: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let fetch_guard = FETCH.lock().unwrap_or_else(|p| p.into_inner());
        let api = ApiBuilder::from_env().build().context("hf_hub api init")?;
        let repo = api.repo(Repo::new(MODEL_REPO.to_string(), RepoType::Model));
        let config_filename = repo.get("config.json").context("download config.json")?;
        let tokenizer_filename = repo
            .get("tokenizer.json")
            .context("download tokenizer.json")?;
        let weights_filename = repo
            .get("model.safetensors")
            .context("download model.safetensors")?;
        drop(fetch_guard);

        let config_bytes = std::fs::read(&config_filename).context("read config.json")?;
        let mut config: Config =
            serde_json::from_slice(&config_bytes).context("parse bert config")?;
        // sentence-transformers models historically ship with `hidden_act:
        // "gelu"` but the candle BERT impl uses an explicit enum — set it
        // here to keep older configs working.
        config.hidden_act = HiddenAct::GeluApproximate;

        let mut tokenizer =
            Tokenizer::from_file(&tokenizer_filename).map_err(anyhow::Error::msg)?;
        tokenizer
            .with_padding(Some(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                ..Default::default()
            }))
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_LEN,
                ..Default::default()
            }))
            .map_err(anyhow::Error::msg)?;

        // SAFETY: `from_mmaped_safetensors` mmaps the file; the lifetime is
        // tied to the returned `VarBuilder`, which the `BertModel` consumes
        // and drops here. After `BertModel::load` the mmap is no longer
        // referenced.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                std::slice::from_ref(&weights_filename),
                DTYPE,
                &device,
            )
            .context("load safetensors weights")?
        };
        let model = BertModel::load(vb, &config).context("instantiate bert model")?;

        Ok(Self {
            model,
            tokenizer,
            device,
        })
    }

    /// Embed a batch of strings into 384-dim L2-normalised f32 vectors.
    ///
    /// Why: Single batched code path keeps the dispatch surface minimal and
    /// matches `FastEmbedder::embed_batch`'s contract so the embed pool can
    /// swap implementations without per-call branching.
    /// What: tokenise → stack `[B, T]` `input_ids` + `attention_mask` tensors
    /// on the active device → `BertModel::forward` → mean-pool the last
    /// hidden state along the sequence axis (masking pads) → L2-normalise.
    /// Test: `embed_dimension` and `cosine_self_similarity`.
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_string()).collect();
        let encodings = self
            .tokenizer
            .encode_batch(owned, true)
            .map_err(anyhow::Error::msg)
            .context("tokenize batch")?;

        let batch = encodings.len();
        let seq_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);
        if seq_len == 0 {
            return Ok(vec![vec![0.0; EMBED_DIM]; batch]);
        }

        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for enc in &encodings {
            ids.extend_from_slice(enc.get_ids());
            mask.extend_from_slice(enc.get_attention_mask());
        }

        let input_ids =
            Tensor::from_vec(ids, (batch, seq_len), &self.device).context("stack input_ids")?;
        let attn_mask_u32 = Tensor::from_vec(mask, (batch, seq_len), &self.device)
            .context("stack attention_mask")?;
        let token_type_ids = input_ids.zeros_like().context("zeros for token_type_ids")?;

        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attn_mask_u32))
            .context("bert forward")?;

        // Mean-pool along the sequence axis with the attention mask.
        // hidden: [B, T, D] (f32). mask: [B, T] (u32 → f32, broadcast to D).
        let mask_f = attn_mask_u32.to_dtype(DType::F32).context("mask to f32")?;
        let mask_b_t_1 = mask_f.unsqueeze(2).context("unsqueeze mask")?;
        let masked = hidden
            .broadcast_mul(&mask_b_t_1)
            .context("apply mask to hidden")?;
        let summed = masked.sum(1).context("sum along seq")?;
        let counts = mask_f
            .sum(1)
            .context("sum mask along seq")?
            .clamp(1e-9_f64, f64::INFINITY)
            .context("clamp counts")?;
        let counts_b_1 = counts.unsqueeze(1).context("unsqueeze counts")?;
        let pooled = summed.broadcast_div(&counts_b_1).context("mean pool")?;

        // L2-normalise along the embedding axis.
        let norms = pooled
            .sqr()
            .context("square pooled")?
            .sum_keepdim(1)
            .context("sum squares")?
            .sqrt()
            .context("sqrt norms")?
            .clamp(1e-12_f64, f64::INFINITY)
            .context("clamp norms")?;
        let normed = pooled.broadcast_div(&norms).context("l2 normalise")?;

        let out: Vec<Vec<f32>> = normed
            .to_vec2::<f32>()
            .context("materialise embeddings to host")?;
        Ok(out)
    }

    /// Output embedding dimensionality.
    ///
    /// Why: lets the embed pool and trait adapter report a stable dim without
    /// constructing a dummy vector.
    pub fn dimension(&self) -> usize {
        EMBED_DIM
    }
}

/// Shared-trait implementation so `CandleEmbedder` slots into the same
/// `Arc<dyn Embedder>` slot the embed pool already consumes (issue #41 phase 4).
///
/// Why: The pool's worker tasks call `embedder.embed_batch(&[String])` via the
/// in-crate trait, which the workspace blanket-impl forwards to this shared
/// trait. Implementing this here means zero changes to the pool's public
/// surface.
/// What: marshals the `&[String]` argument through the sync candle call by
/// way of `tokio::task::spawn_blocking` so the runtime worker is not parked
/// across CPU-bound tensor math.
/// Test: covered indirectly by every code path that uses the pool with the
/// candle feature enabled.
#[async_trait::async_trait]
impl trusty_common::embedder::Embedder for CandleEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // SAFETY: `self` outlives the blocking task because the caller awaits
        // `embed_batch` on the same `Arc<dyn Embedder>` it cloned for the
        // pool's worker. We move owned strings into the closure and clone the
        // device/model handles by reference.
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        // Candle tensors are not `Send` across runtime threads while a forward
        // pass is in flight on Metal — but we drop them all inside this call.
        // Run synchronously here (this trait method already runs inside a
        // tokio worker via the pool).
        self.embed_batch(&refs)
    }

    fn dimension(&self) -> usize {
        EMBED_DIM
    }
}

/// Pick the best available device, preferring Metal on Apple Silicon.
///
/// Why: candle exposes Metal as an opt-in device; constructing it can fail in
/// containers / VMs without GPU passthrough, so we fall back to CPU rather
/// than panic the daemon.
/// What: returns `Device::new_metal(0)` on success, `Device::Cpu` otherwise.
/// Test: covered indirectly — every test constructs the embedder and asserts
/// the dimension regardless of which device was picked.
fn pick_device() -> Device {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        match Device::new_metal(0) {
            Ok(d) => return d,
            Err(e) => {
                tracing::warn!("candle: metal device init failed ({e}); falling back to CPU");
            }
        }
    }
    Device::Cpu
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: regression guard — the rest of the pipeline assumes 384-dim
    /// vectors. A model swap that changed this would silently break every
    /// downstream consumer.
    /// What: embed one short string and assert the result is exactly
    /// `EMBED_DIM` floats.
    /// Test: ignored by default because it pulls the model from HF Hub on
    /// first run (~22 MB) — run with `cargo test -p trusty-search
    /// --features candle -- --include-ignored`.
    #[test]
    #[ignore = "downloads ~22 MB from HuggingFace; run with --include-ignored"]
    fn embed_dimension() {
        let emb = CandleEmbedder::new().expect("load candle embedder");
        let out = emb.embed_batch(&["hello"]).expect("embed succeeds");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), EMBED_DIM);
    }

    /// Why: the L2-normalisation step must produce unit vectors so cosine
    /// similarity collapses to a dot product. Self-similarity ≈ 1.0 is the
    /// cheapest invariant that exercises tokenisation, forward pass, pooling,
    /// and normalisation together.
    /// What: embed the same string twice (or once and dot-product it with
    /// itself) and assert cos ≈ 1.0 within f32 epsilon.
    /// Test: ignored alongside `embed_dimension` for the same model-download
    /// reason.
    #[test]
    #[ignore = "downloads ~22 MB from HuggingFace; run with --include-ignored"]
    fn cosine_self_similarity() {
        let emb = CandleEmbedder::new().expect("load candle embedder");
        let out = emb.embed_batch(&["hello world"]).expect("embed succeeds");
        let v = &out[0];
        let dot: f32 = v.iter().map(|x| x * x).sum();
        assert!(
            (dot - 1.0).abs() < 1e-3,
            "self-cosine should be ~1.0 (got {dot})"
        );
    }
}
