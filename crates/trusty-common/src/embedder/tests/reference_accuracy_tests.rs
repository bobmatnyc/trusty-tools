//! The `sentence-transformers` reference-vector accuracy gate for the default
//! embedding model (issue #3486 / #3493 P0, deflaked by #3711).
//!
//! Why: split out of `mod.rs`'s inline `mod tests` (issue #610 line-cap — that
//! file is at its grandfathered SLOC budget) so #3711's wrong-model diagnosis
//! could be added without growing it.
//! What: a CHILD module of `mod.rs`'s `mod tests`, deliberately NOT a sibling
//! of `provider_tests`. A sibling would be outside that module and would need
//! to duplicate `ENV_LOCK`/`EnvVarGuard` the way `provider_tests` does — and a
//! second lock does not serialise against the first, which is precisely the
//! env race #3711 fixes. As a child it inherits the parent's single lock and
//! guard through `use super::*`.
//! Test: this file.

use super::*;

/// Fixed sample used by [`default_model_matches_sentence_transformers_reference`].
///
/// Why: issue #3486 / #3493 P0 — the default model swap (INT8 →
/// `AllMiniLML6V2` fp32) must be verified against a genuine, independent
/// reference, not just "fastembed didn't error". These 5 texts and their
/// reference vectors were generated with a real
/// `sentence-transformers/all-MiniLM-L6-v2` CPU fp32 model (`device="cpu"`,
/// `normalize_embeddings=True`) — the same independent-library method the
/// #3486 CoreML/fp16 experiment's correctness gate used (there: 0.999999+
/// mean cosine for fp32/fp16 vs 0.9897 for INT8, across a 50-chunk sample).
const REFERENCE_TEXTS: [&str; 5] = [
    "fn authenticate(user: &str) -> bool",
    "pub struct Embedder { model: TextEmbedding }",
    "the quick brown fox jumps over the lazy dog",
    "async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>",
    "memory palace consolidation cycle",
];

include!("../reference_vectors_test_data.rs");

/// Name `FastEmbedder::model_name()` reports for the fp32 production
/// default — the ONLY model whose accuracy
/// [`default_model_matches_sentence_transformers_reference`] is a valid
/// gate for (see `embedding_model_name`, issue #3530).
const REFERENCE_FP32_MODEL_NAME: &str = "all-MiniLM-L6-v2";

/// Minimum mean cosine similarity required between the default embedder
/// and `REFERENCE_VECTORS` by
/// [`default_model_matches_sentence_transformers_reference`].
///
/// Why this exact value (issue #3711): the threshold only has to land in the
/// gap between the two models that can plausibly be loaded. Compare them by
/// DEFICIT from a perfect match (`1 - cosine`), since that is what the
/// threshold actually budgets — this constant allows a deficit of 1e-3:
///
///   - fp32 `AllMiniLML6V2` — the production default this test guards —
///     measures 0.999999+ against the `sentence-transformers` reference, a
///     deficit of ~1e-6. The only variance a *correctly loaded* fp32 model
///     can contribute is float re-association across different
///     GEMM/reduction microkernels (ORT thread counts, AVX2 vs AVX-512,
///     different runners), which is ~1e-6 relative on a 6-layer MiniLM and
///     cannot compound into a visible cosine change. So the budget is ~1000x
///     larger than the drift it must tolerate.
///   - INT8 `AllMiniLML6V2Q` measures 0.9897 (see
///     `resolve_default_embedding_model`'s doc), a deficit of ~1.03e-2 —
///     ~10x larger than the budget, so it fails. The local reproduction for
///     #3711 was tighter still: 0.995703, a deficit of ~4.3e-3, only ~4x
///     over. That ~4x is the REAL margin on the wrong-model side, not the
///     comfortable gap an earlier draft of this comment claimed, and it is
///     precisely why the wrong-model discriminator in the test body is the
///     `model_name()` assertion rather than this number.
///
/// So: platform numeric variance cannot trip this threshold, and a wrong
/// quantization still fails it — but only by a single order of magnitude.
///
/// Why it is NOT loosened (issue #3711): the 0.990649 CI failure was not
/// fp32 drift. A value that low is unreachable by re-association and is
/// within sampling distance of INT8's 0.9897 — the test was measuring a
/// genuinely INT8 embedder and reporting it as an fp32 accuracy
/// regression. The defect was the missing wrong-model diagnosis (now two
/// guards in the test body), not the tolerance, so nudging this constant
/// would have deleted the test's entire diagnostic value while leaving
/// both causes in place. Any future failure of this assertion with
/// `REFERENCE_FP32_MODEL_NAME` confirmed loaded is a REAL fp32 accuracy
/// regression and must not be papered over here either.
const REFERENCE_MIN_MEAN_COSINE: f64 = 0.999;

/// Why: verifies the actual production default (`FastEmbedder::new()`,
/// which now resolves to `AllMiniLML6V2` fp32 per
/// `resolve_default_embedding_model`) produces embeddings numerically
/// consistent with a genuine, independent `sentence-transformers`
/// reference — not just "some vector came back" (issue #3486 / #3493 P0).
/// What: pins `TRUSTY_EMBEDDER_MODEL` unset under `ENV_LOCK`, embeds
/// [`REFERENCE_TEXTS`] with the default embedder, asserts the model that
/// actually loaded is [`REFERENCE_FP32_MODEL_NAME`], and only then asserts
/// mean cosine similarity against `REFERENCE_VECTORS` is
/// `>= REFERENCE_MIN_MEAN_COSINE`.
/// Test: this test. NOT `#[ignore]`d (issue #3493 P0 part 2 — this is the
/// correctness gate that would have caught the CoreML-default accuracy
/// regression had it been wired into CI; see `ci.yml`'s `test` job for
/// the fastembed-model pre-seed step that makes this safe to run on every
/// PR without HuggingFace-download flakiness).
///
/// Deflaked in issue #3711. Two independent paths let this test measure an
/// INT8 embedder while blaming fp32 accuracy, and both are now diagnosed
/// rather than tolerated:
///   1. env race — `FastEmbedder::new()` reads `TRUSTY_EMBEDDER_MODEL`
///      inside `resolve_default_embedding_model`, and this test held no
///      `ENV_LOCK`, so the sibling
///      `resolve_default_embedding_model_int8_opt_in` (which sets that var
///      process-globally) could be mid-loop at the instant this test
///      resolved its model. Now serialised like every other env-touching
///      test in this module, which additionally covers the `TRUSTY_DEVICE`
///      write `with_cache_size` performs on its CPU-fallback path.
///   2. silent INT8 substitution — `FastEmbedder::with_cache_size`'s
///      two-model robustness net loads `AllMiniLML6V2Q` when the fp32
///      primary fails to initialise (plausible on a disk- and
///      memory-tight CI runner), and nothing checked which model came
///      back. `model_name()` (issue #3530) is now asserted first, so this
///      surfaces as "the fp32 primary never loaded" instead of an
///      inscrutable cosine number.
///
/// A synchronous `#[test]` driving an explicit current-thread runtime,
/// rather than `#[tokio::test]`, is what lets this module's existing std
/// `ENV_LOCK` cover the whole construct-and-embed window without tripping
/// `clippy::await_holding_lock` or introducing a second, non-serialising
/// lock (the trap `provider_tests.rs` documents).
#[test]
fn default_model_matches_sentence_transformers_reference() {
    fn cosine(a: &[f32], b: &[f32]) -> f64 {
        let dot: f64 = a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum();
        let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        dot / (norm_a * norm_b)
    }

    // #3711: pin model selection for the whole construct-and-embed window
    // — an unlocked read raced a sibling test's TRUSTY_EMBEDDER_MODEL=int8.
    let _guard = env_lock();
    let _model_env = EnvVarGuard::apply("TRUSTY_EMBEDDER_MODEL", None);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread tokio runtime must build");
    let (model_name, vectors) = rt.block_on(async {
        let e = FastEmbedder::new().await.unwrap();
        let texts: Vec<String> = REFERENCE_TEXTS.iter().map(|s| s.to_string()).collect();
        let vectors = e.embed_batch(&texts).await.unwrap();
        (e.model_name(), vectors)
    });

    // #3711: which model loaded must be checked BEFORE the cosine gate —
    // INT8 scores ~0.9897, which the old cosine-only assertion misreported
    // as an fp32 accuracy regression.
    assert_eq!(
        model_name, REFERENCE_FP32_MODEL_NAME,
        "this test gates fp32 accuracy, but the embedder loaded {model_name:?} — either \
         TRUSTY_EMBEDDER_MODEL leaked an INT8 opt-in or the two-model fallback net in \
         FastEmbedder::with_cache_size substituted INT8 because the fp32 primary failed \
         to initialise (check the tracing warn it emits, and ci.yml's fastembed pre-seed \
         step). This is a wrong-model failure, NOT an accuracy regression (issue #3711)"
    );
    assert_eq!(vectors.len(), REFERENCE_VECTORS.len());

    let sims: Vec<f64> = vectors
        .iter()
        .zip(REFERENCE_VECTORS.iter())
        .map(|(v, r)| cosine(v, r))
        .collect();
    let mean_sim = sims.iter().sum::<f64>() / sims.len() as f64;
    let min_sim = sims.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "default_model_matches_sentence_transformers_reference: model={model_name} mean_cosine={mean_sim:.6} min_cosine={min_sim:.6}"
    );

    // #3711: reached only with the fp32 model confirmed loaded, so a
    // failure here is a genuine numeric regression, not a wrong model.
    assert!(
        mean_sim >= REFERENCE_MIN_MEAN_COSINE,
        "fp32 {REFERENCE_FP32_MODEL_NAME} (confirmed loaded) must match the \
         sentence-transformers reference to >= {REFERENCE_MIN_MEAN_COSINE} mean cosine \
         similarity, but got {mean_sim:.6} (min {min_sim:.6}). fp32 measures 0.999999+ \
         and platform float re-association is ~1e-6, so this is a real accuracy \
         regression in the embedding path — see REFERENCE_MIN_MEAN_COSINE's doc before \
         touching the threshold (issue #3486 / #3493 P0 / #3711)"
    );
}
