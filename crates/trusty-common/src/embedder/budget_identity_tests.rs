//! #9391: the per-call byte budget leaves every embedding bit-identical.
//!
//! Why: the budget exists to cut ONNX memory without changing a single stored
//! vector. It changes how many inputs share a call, and so how far the short
//! ones are padded; this test proves padding does not reach the output.
//! What: embeds a mix of short inputs and one input past the 512-token limit
//! through `FastEmbedder` (budgeted) and through one raw fastembed call over
//! the whole batch with the same session options (the pre-#9391 path), and
//! compares every vector bit for bit. Real fp32 model, like
//! `default_model_matches_sentence_transformers_reference`.
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
/// What: asserts the budget really splits these inputs, then that each
/// budgeted vector equals the unsplit vector in every bit.
/// Test: itself.
#[test]
fn a_budget_split_leaves_every_vector_bit_identical() {
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

    assert_eq!(budgeted.len(), unsplit.len());
    for (i, (a, b)) in budgeted.iter().zip(&unsplit).enumerate() {
        let a_bits: Vec<u32> = a.iter().map(|x| x.to_bits()).collect();
        let b_bits: Vec<u32> = b.iter().map(|x| x.to_bits()).collect();
        assert!(
            a_bits == b_bits,
            "input {i} ({} bytes): the budgeted vector differs from the unsplit one",
            texts[i].len()
        );
    }
}
