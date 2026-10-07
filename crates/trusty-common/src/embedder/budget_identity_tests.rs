//! #9391: the per-call byte budget changes no embedding beyond the rounding
//! the pre-#9391 path already shows.
//!
//! Why: the budget exists to cut ONNX memory without changing stored vectors.
//! It changes how many inputs share a call, and so how far the short ones are
//! padded. Padding moves a vector by float rounding only: measured at most
//! 1.2e-7 per element, the same as main's own difference between an input
//! embedded alone and in a full batch.
//! What: embeds a mix of short inputs and one input past the 512-token limit
//! through `FastEmbedder` (budgeted) and through one raw fastembed call over
//! the whole batch with the same session options (the pre-#9391 path). The
//! call holding the long input is padded as before and must match bit for
//! bit; every vector must sit within main's own batch-composition noise.
//! Real fp32 model, like `default_model_matches_sentence_transformers_reference`.
//! Test: this file.

use super::fast_embedder::FastEmbedder;
use super::types::{DEFAULT_EMBED_ONNX_BATCH, Embedder, budgeted_len};
use crate::embedder::test_env::{EnvVarGuard, env_lock};
use fastembed::{EmbeddingModel, TextEmbedding};

/// Short drawers around one long one, so the budget splits the batch.
fn mixed_inputs() -> Vec<String> {
    let long = "The dream cycle re-embeds every drawer of an unchanged palace. ".repeat(60);
    let mut texts: Vec<String> = [
        "Redb stores each palace in one file.",
        "The recall path blends lexical and vector scores.",
        "A settled palace skips its embedding passes.",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    texts.push(long);
    texts.extend(
        [
            "fn embed_batch(&self, texts: &[String])",
            "Idle palaces are evicted after 300 seconds.",
            "The maintenance lease elects one dreamer per data root.",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    texts
}

/// Why: see the module doc.
/// What: asserts the budget really splits these inputs, then compares each
/// budgeted vector with the unsplit one as the module doc describes.
/// Test: itself.
#[test]
fn a_budget_split_moves_no_vector_beyond_main_rounding() {
    // Same lock and model pin as the reference-accuracy gate (#3711).
    let _guard = env_lock();
    let _model_env = EnvVarGuard::apply("TRUSTY_EMBEDDER_MODEL", None);
    let _batch_env = EnvVarGuard::apply("TRUSTY_EMBED_ONNX_BATCH", None);
    let texts = mixed_inputs();
    assert!(
        budgeted_len(&texts, DEFAULT_EMBED_ONNX_BATCH) < texts.len(),
        "the fixture must make the budget split the batch"
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread tokio runtime must build");
    let (model_name, budgeted) = rt.block_on(async {
        let e = FastEmbedder::new().await.expect("FastEmbedder::new");
        let vectors = e.embed_batch(&texts).await.expect("budgeted embed");
        (e.model_name(), vectors)
    });
    assert_eq!(model_name, "all-MiniLM-L6-v2", "the fp32 model must load");

    let (opts, _provider) = FastEmbedder::init_options(EmbeddingModel::AllMiniLML6V2);
    let mut raw = TextEmbedding::try_new(opts).expect("raw fastembed session");
    let unsplit = raw
        .embed(texts.as_slice(), Some(DEFAULT_EMBED_ONNX_BATCH))
        .expect("one unsplit fastembed call");

    // Pre-#9391 reference noise: the same input embedded alone instead of in
    // the full batch. Main already returns these vectors whenever the cache
    // misses of one call group differently.
    let main_noise = texts
        .iter()
        .zip(&unsplit)
        .map(|(t, b)| {
            let alone = raw
                .embed(std::slice::from_ref(t), Some(DEFAULT_EMBED_ONNX_BATCH))
                .expect("one input alone")
                .remove(0);
            max_abs_diff(&alone, b)
        })
        .fold(0f32, f32::max);

    assert_eq!(budgeted.len(), unsplit.len());
    let first_call = budgeted_len(&texts, DEFAULT_EMBED_ONNX_BATCH);
    for (i, (a, b)) in budgeted.iter().zip(&unsplit).enumerate() {
        if i < first_call {
            // Same companions, same padding as the unsplit call.
            assert!(
                a.iter()
                    .map(|x| x.to_bits())
                    .eq(b.iter().map(|x| x.to_bits())),
                "input {i}: a call padded exactly as before must return the same bits"
            );
        }
        let diff = max_abs_diff(a, b);
        assert!(
            diff <= main_noise,
            "input {i} ({} bytes): the split moved the vector by {diff:e}, more than \
             the {main_noise:e} main itself shows between batch compositions",
            texts[i].len()
        );
    }
}

/// Largest element-wise difference between two vectors.
fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max)
}
