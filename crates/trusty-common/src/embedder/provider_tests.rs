//! Tests for `resolve_expected_python_provider`, `embedding_model_name`, and
//! `FastEmbedder::model_name` (epic #3524 slice 6 — PR 2/5, closes #3530 /
//! #3493 P1).
//!
//! Why: split out of `mod.rs`'s own `#[cfg(test)] mod tests` (issue #610
//! line-cap) rather than growing that file past its grandfathered SLOC
//! budget — this crate's `embedder` module has no other logical home for new
//! test code that doesn't touch `mod.rs` itself.
//! What: uses the module tree's SHARED `ENV_LOCK` + `EnvVarGuard` from
//! `test_env` for serialised env-var mutation across parallel tests.
//!
//! Issue #3711: this file used to declare its own independent
//! `tokio::sync::Mutex` lock, so its `TRUSTY_DEVICE` / `TRUSTY_EMBEDDER_MODEL`
//! mutations did NOT serialise against `mod.rs`'s tests — two separate locks
//! guard nothing from each other, which left the wrong-model race open (e.g.
//! on macOS, or the moment either `#[ignore]` below is lifted) even after the
//! first fix. The tests here are now plain `#[test]`s so they can share the
//! one synchronous lock: the two below that need an async runtime build an
//! explicit current-thread one rather than forcing the whole module tree onto
//! an async lock that synchronous tests cannot take.
//! Test: this file.

use super::*;
use crate::embedder::test_env::{ENV_LOCK, EnvVarGuard};

/// Build a current-thread runtime for a synchronous test that must drive one
/// async call while holding the synchronous [`ENV_LOCK`] (issue #3711).
fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread tokio runtime must build")
}

/// Issue #3493 P1: `TRUSTY_DEVICE=cpu` must force `Cpu` for the Python
/// sidecar prediction too, mirroring the ORT resolver's behaviour.
#[test]
fn resolve_expected_python_provider_forces_cpu() {
    let _g = ENV_LOCK.lock().unwrap();
    let _d = EnvVarGuard::apply("TRUSTY_DEVICE", Some("CPU"));
    assert_eq!(resolve_expected_python_provider(), ExecutionProvider::Cpu);
}

/// Issue #3493 P1: with `TRUSTY_DEVICE` unset, Apple Silicon predicts `Mps`
/// (the Python sidecar's device resolver picks MPS first on aarch64-macOS);
/// an `embedder-cuda` build predicts `Cuda`; every other host predicts `Cpu`.
#[test]
fn resolve_expected_python_provider_default_matches_platform() {
    let _g = ENV_LOCK.lock().unwrap();
    let _d = EnvVarGuard::apply("TRUSTY_DEVICE", None);

    let got = resolve_expected_python_provider();

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    let expected = ExecutionProvider::Mps;
    #[cfg(all(
        not(all(target_arch = "aarch64", target_os = "macos")),
        feature = "embedder-cuda"
    ))]
    let expected = ExecutionProvider::Cuda;
    #[cfg(all(
        not(all(target_arch = "aarch64", target_os = "macos")),
        not(feature = "embedder-cuda")
    ))]
    let expected = ExecutionProvider::Cpu;

    assert_eq!(
        got, expected,
        "predicted python-sidecar provider must match resolve_device's precedence \
         (mps > cuda > cpu) for this build/platform"
    );
}

/// Issue #3530 — the `(Q)` observability bug. `embedding_model_name` must
/// report the truth for both variants this crate ever selects, and never
/// panic on an unrecognised `EmbeddingModel` (defensive fallback).
#[test]
fn embedding_model_name_reports_resolved_variant() {
    // Pure function, no env mutation — no ENV_LOCK needed.
    use crate::embedder::fast_embedder::embedding_model_name;
    assert_eq!(
        embedding_model_name(&fastembed::EmbeddingModel::AllMiniLML6V2),
        "all-MiniLM-L6-v2",
        "fp32 default must report the un-suffixed name, not the stale (Q) tag"
    );
    assert_eq!(
        embedding_model_name(&fastembed::EmbeddingModel::AllMiniLML6V2Q),
        "all-MiniLM-L6-v2-int8",
        "the explicit INT8 opt-in must report an int8-suffixed name"
    );
}

/// Issue #3530 — `FastEmbedder::model_name()` must report the fp32 default's
/// real name, not the stale `"AllMiniLML6V2Q"` hardcode. `#[ignore]`:
/// downloads a real ONNX model.
#[test]
#[ignore]
fn fast_embedder_model_name_reports_fp32_default() {
    // #3711: sync test + explicit runtime so the SHARED std `ENV_LOCK` covers
    // the whole construct window without `clippy::await_holding_lock`.
    let _g = ENV_LOCK.lock().unwrap();
    let _m = EnvVarGuard::apply("TRUSTY_EMBEDDER_MODEL", None);
    let name = current_thread_runtime().block_on(async {
        let e = FastEmbedder::new().await.unwrap();
        e.model_name()
    });
    assert_eq!(name, "all-MiniLM-L6-v2");
}

/// Issue #3530 — the explicit INT8 opt-in must be reflected by
/// `model_name()` too. `#[ignore]`: downloads a real ONNX model.
#[test]
#[ignore]
fn fast_embedder_model_name_reports_int8_opt_in() {
    // #3711: see the sibling test — same shared-lock discipline.
    let _g = ENV_LOCK.lock().unwrap();
    let _m = EnvVarGuard::apply("TRUSTY_EMBEDDER_MODEL", Some("int8"));
    let name = current_thread_runtime().block_on(async {
        let e = FastEmbedder::new().await.unwrap();
        e.model_name()
    });
    assert_eq!(name, "all-MiniLM-L6-v2-int8");
}
