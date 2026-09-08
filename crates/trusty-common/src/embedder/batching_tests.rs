//! Tests for the #7106 per-inference ONNX batch ceiling.
//!
//! Why: the defect these cover is invisible in `embed_batch`'s return value —
//! 600 inputs produce 600 vectors whether ONNX saw them as one batch of 600 or
//! 38 batches of 16, and only the second bounds the daemon's memory. The batch
//! SHAPE is the contract, so a test has to observe the calls, not the result.
//! Doing that without downloading a 90 MB ONNX model means testing
//! [`embed_in_bounded_batches`] — the function `FastEmbedder::embed_batch`
//! hands its cache-miss list to — against a stub that records what it is
//! handed. That function's body IS the fix: replace it with a single
//! `embed_chunk(inputs, batch)` call and this file goes red.
//! What: a recording stub closure plus the env-knob resolution cases, all
//! sharing the module tree's `ENV_LOCK` for the env-touching ones.
//! Test: this file.

use super::types::{DEFAULT_EMBED_ONNX_BATCH, embed_in_bounded_batches, resolve_embed_onnx_batch};
use crate::embedder::test_env::{EnvVarGuard, env_lock};
use anyhow::Result;
use std::cell::RefCell;

/// `n` distinct inputs, so a mis-ordered result cannot pass by coincidence.
fn inputs(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("drawer {i}")).collect()
}

/// One vector per input, its first slot carrying the input's own number.
///
/// Lets a test assert ORDER, not just count: `vectors[i][0]` must be `i`.
fn echo_vectors(chunk: &[String]) -> Vec<Vec<f32>> {
    chunk
        .iter()
        .map(|s| {
            let n: f32 = s
                .rsplit(' ')
                .next()
                .and_then(|d| d.parse().ok())
                .unwrap_or(-1.0);
            vec![n, 0.0]
        })
        .collect()
}

/// Records every `(batch_len, ceiling)` pair the chunker hands the stub.
///
/// Why: `embed_in_bounded_batches` takes an `FnMut`, so the log has to live
/// outside the closure. A `RefCell` keeps the tests single-threaded and free of
/// lock-poisoning noise.
/// What: `calls` is one entry per underlying `embed` call, in call order.
/// Test: used by every test below.
#[derive(Default)]
struct CallLog {
    calls: RefCell<Vec<(usize, usize)>>,
}

impl CallLog {
    fn record(&self, chunk: &[String], ceiling: usize) {
        self.calls.borrow_mut().push((chunk.len(), ceiling));
    }

    fn batch_sizes(&self) -> Vec<usize> {
        self.calls.borrow().iter().map(|(len, _)| *len).collect()
    }

    fn ceilings(&self) -> Vec<usize> {
        self.calls.borrow().iter().map(|(_, c)| *c).collect()
    }
}

/// Why: this is the #7106 regression. Pre-fix, `FastEmbedder::embed_batch`
/// handed its whole cache-miss list to fastembed with `None`, so 600 drawers
/// became ONE ONNX batch of 600 at up to 512 tokens — the ~20 GB transient.
/// What: pushes 600 inputs through the chunker at the production default and
/// asserts the stub saw 37 calls of 16 plus one of 8 — every call bounded, none
/// dropped. Pre-fix (a chunker body of one `embed_chunk(inputs, batch)` call)
/// the log is a single entry of 600 and this fails on the first assert.
/// Test: itself.
#[test]
fn bounded_batches_never_exceed_the_ceiling() {
    let log = CallLog::default();
    let texts = inputs(600);

    let out = embed_in_bounded_batches(&texts, DEFAULT_EMBED_ONNX_BATCH, |chunk, ceiling| {
        log.record(chunk, ceiling);
        Ok(echo_vectors(chunk))
    })
    .expect("bounded embed must succeed");

    let sizes = log.batch_sizes();
    let full = 600 / DEFAULT_EMBED_ONNX_BATCH;
    let remainder = 600 % DEFAULT_EMBED_ONNX_BATCH;
    let mut expected = vec![DEFAULT_EMBED_ONNX_BATCH; full];
    if remainder > 0 {
        expected.push(remainder);
    }
    assert_eq!(
        sizes, expected,
        "600 inputs must reach ONNX as bounded batches, not one call of 600"
    );
    assert!(
        sizes.iter().all(|n| *n <= DEFAULT_EMBED_ONNX_BATCH),
        "no ONNX batch may exceed the ceiling: {sizes:?}"
    );
    assert_eq!(
        sizes.iter().sum::<usize>(),
        600,
        "chunking must not drop or duplicate an input"
    );
    assert!(
        log.ceilings()
            .iter()
            .all(|c| *c == DEFAULT_EMBED_ONNX_BATCH),
        "every call must carry the ceiling itself, so fastembed's own \
         batch_size is pinned and a dynamically-quantised model is never handed \
         a batch_size below its input count: {:?}",
        log.ceilings()
    );
    assert_eq!(out.len(), 600, "one vector per input");
}

/// Why: chunking is only safe if the caller cannot tell it happened. A result
/// re-ordered or short by one silently mis-assigns every vector to the wrong
/// drawer.
/// What: embeds 600 numbered inputs across 38 batches and asserts the
/// concatenated result is 600 vectors in input order.
/// Test: itself.
#[test]
fn bounded_batches_preserve_input_order_and_count() {
    let texts = inputs(600);

    let out = embed_in_bounded_batches(&texts, DEFAULT_EMBED_ONNX_BATCH, |chunk, _| {
        Ok(echo_vectors(chunk))
    })
    .expect("bounded embed must succeed");

    assert_eq!(out.len(), texts.len());
    for (i, vector) in out.iter().enumerate() {
        assert_eq!(
            vector[0], i as f32,
            "vector {i} came back out of input order"
        );
    }
}

/// Why: the overwhelmingly common call is one text (`embed_one`, the write
/// pipeline, `share::import`). Chunking must not add a hop there.
/// What: 5 inputs under a ceiling of 16 must be exactly one call of 5.
/// Test: itself.
#[test]
fn a_short_input_still_makes_one_call() {
    let log = CallLog::default();
    let texts = inputs(5);

    embed_in_bounded_batches(&texts, DEFAULT_EMBED_ONNX_BATCH, |chunk, ceiling| {
        log.record(chunk, ceiling);
        Ok(echo_vectors(chunk))
    })
    .expect("bounded embed must succeed");

    assert_eq!(log.batch_sizes(), vec![5]);
}

/// Why: an empty batch must cost nothing — never a session run on zero rows,
/// which fastembed's tokenizer rejects.
/// What: no inputs, no calls, empty result.
/// Test: itself.
#[test]
fn an_empty_input_makes_no_call() {
    let log = CallLog::default();

    let out = embed_in_bounded_batches(&[], DEFAULT_EMBED_ONNX_BATCH, |chunk, ceiling| {
        log.record(chunk, ceiling);
        Ok(echo_vectors(chunk))
    })
    .expect("an empty batch must succeed");

    assert!(log.batch_sizes().is_empty(), "no input, no ONNX call");
    assert!(out.is_empty());
}

/// Why: splitting one call into 38 must not turn a hard failure into a partial
/// success — a truncated result would be silently mis-aligned with its inputs.
/// What: fails the third chunk and asserts the whole call errors.
/// Test: itself.
#[test]
fn a_mid_batch_error_fails_the_whole_call() {
    let log = CallLog::default();
    let texts = inputs(600);

    // `.map(len)` keeps a failure message compact — `expect_err` would otherwise
    // print all 600 vectors.
    let err = embed_in_bounded_batches(&texts, DEFAULT_EMBED_ONNX_BATCH, |chunk, ceiling| {
        log.record(chunk, ceiling);
        if log.batch_sizes().len() == 3 {
            anyhow::bail!("ORT session run failed");
        }
        Ok(echo_vectors(chunk))
    })
    .map(|v| v.len())
    .expect_err("a failing chunk must fail the whole call");

    assert!(
        format!("{err:#}").contains("ORT session run failed"),
        "the underlying error must survive: {err:#}"
    );
    assert_eq!(
        log.batch_sizes().len(),
        3,
        "the call must stop at the failing chunk, not run the remaining 35"
    );
}

/// Why: the per-call count guard `FastEmbedder::embed_batch` already had for a
/// single call has to hold per CHUNK now, or a short chunk shifts every later
/// vector one slot left.
/// What: returns one vector too few from the second chunk and asserts the call
/// fails naming both counts.
/// Test: itself.
#[test]
fn a_short_chunk_result_fails_the_whole_call() {
    let log = CallLog::default();
    let texts = inputs(600);

    let err = embed_in_bounded_batches(&texts, DEFAULT_EMBED_ONNX_BATCH, |chunk, ceiling| {
        log.record(chunk, ceiling);
        let mut vectors = echo_vectors(chunk);
        if log.batch_sizes().len() == 2 {
            vectors.pop();
        }
        Ok(vectors)
    })
    .map(|v| v.len())
    .expect_err("a short chunk result must fail the whole call");

    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("15") && rendered.contains("16"),
        "the error must name the mismatched counts: {rendered}"
    );
}

/// Why: a `0` ceiling from a future caller must not become an infinite loop
/// over a zero-length chunk.
/// What: a ceiling of 0 behaves as 1 — one call per input.
/// Test: itself.
#[test]
fn a_zero_ceiling_is_clamped_to_one() {
    let log = CallLog::default();
    let texts = inputs(3);

    embed_in_bounded_batches(&texts, 0, |chunk, ceiling| {
        log.record(chunk, ceiling);
        Ok(echo_vectors(chunk))
    })
    .expect("bounded embed must succeed");

    assert_eq!(log.batch_sizes(), vec![1, 1, 1]);
    assert_eq!(log.ceilings(), vec![1, 1, 1]);
}

/// Why: the production bound is what every daemon runs with, so the unset case
/// is the one that matters most.
/// What: `TRUSTY_EMBED_ONNX_BATCH` removed resolves to the compiled default.
/// Test: itself.
#[test]
fn embed_onnx_batch_defaults_when_unset() {
    let _g = env_lock();
    let _e = EnvVarGuard::apply("TRUSTY_EMBED_ONNX_BATCH", None);
    assert_eq!(resolve_embed_onnx_batch(), DEFAULT_EMBED_ONNX_BATCH);
}

/// Why: the knob exists so an operator can trade memory for throughput without
/// a rebuild; if it were ignored, the escape hatch would be a lie.
/// What: a positive integer (with surrounding whitespace) is honoured.
/// Test: itself.
#[test]
fn embed_onnx_batch_reads_env() {
    let _g = env_lock();
    let _e = EnvVarGuard::apply("TRUSTY_EMBED_ONNX_BATCH", Some(" 64 "));
    assert_eq!(resolve_embed_onnx_batch(), 64);
}

/// Why: a typo'd knob must not become an unbounded batch by accident.
/// What: a non-numeric value falls back to the default.
/// Test: itself.
#[test]
fn embed_onnx_batch_defaults_on_garbage() {
    let _g = env_lock();
    let _e = EnvVarGuard::apply("TRUSTY_EMBED_ONNX_BATCH", Some("lots"));
    assert_eq!(resolve_embed_onnx_batch(), DEFAULT_EMBED_ONNX_BATCH);
}

/// Why: `0` would mean "no inputs per call" — a stall, not a bound.
/// What: zero falls back to the default rather than being taken literally.
/// Test: itself.
#[test]
fn embed_onnx_batch_defaults_on_zero() {
    let _g = env_lock();
    let _e = EnvVarGuard::apply("TRUSTY_EMBED_ONNX_BATCH", Some("0"));
    assert_eq!(resolve_embed_onnx_batch(), DEFAULT_EMBED_ONNX_BATCH);
}

/// Why: the chunker must respect an operator's larger ceiling, not silently
/// keep the compiled default.
/// What: resolves the knob at `100` and asserts 600 inputs become
/// `[100 × 6]` calls.
/// Test: itself.
#[test]
fn a_resolved_env_ceiling_drives_the_chunking() -> Result<()> {
    let _g = env_lock();
    let _e = EnvVarGuard::apply("TRUSTY_EMBED_ONNX_BATCH", Some("100"));

    let log = CallLog::default();
    let texts = inputs(600);
    embed_in_bounded_batches(&texts, resolve_embed_onnx_batch(), |chunk, ceiling| {
        log.record(chunk, ceiling);
        Ok(echo_vectors(chunk))
    })?;

    assert_eq!(log.batch_sizes(), vec![100; 6]);
    Ok(())
}
