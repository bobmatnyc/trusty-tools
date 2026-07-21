//! Tests for `resolve_expected_python_provider`, `embedding_model_name`, and
//! `FastEmbedder::model_name` (epic #3524 slice 6 — PR 2/5, closes #3530 /
//! #3493 P1).
//!
//! Why: split out of `mod.rs`'s own `#[cfg(test)] mod tests` (issue #610
//! line-cap) rather than growing that file past its grandfathered SLOC
//! budget — this crate's `embedder` module has no other logical home for new
//! test code that doesn't touch `mod.rs` itself.
//! What: mirrors `mod.rs`'s test-module conventions (`ENV_LOCK` +
//! `EnvVarGuard` for serialised env-var mutation across parallel tests) —
//! duplicated here rather than shared because both are private to their
//! respective test modules and the crate has no shared test-support module.
//! Test: this file.

use super::*;
use tokio::sync::Mutex;

/// Process-global lock guarding every test in this module that mutates the
/// process environment. A `tokio::sync::Mutex` (not `std::sync::Mutex`, unlike
/// `mod.rs`'s synchronous-only `ENV_LOCK`) because the two `FastEmbedder::new`
/// tests below hold the guard across an `.await` point — `clippy::
/// await_holding_lock` forbids doing that with a std lock.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// RAII helper: set or clear an env var for the duration of a test and
/// restore it on drop — see `mod.rs`'s identical `EnvVarGuard`.
struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn apply(key: &'static str, value: Option<&str>) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: every caller holds `ENV_LOCK`, so no other thread reads or
        // writes the environment concurrently.
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: same single-threaded-under-ENV_LOCK invariant as `apply`.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Issue #3493 P1: `TRUSTY_DEVICE=cpu` must force `Cpu` for the Python
/// sidecar prediction too, mirroring the ORT resolver's behaviour.
#[tokio::test]
async fn resolve_expected_python_provider_forces_cpu() {
    let _g = ENV_LOCK.lock().await;
    let _d = EnvVarGuard::apply("TRUSTY_DEVICE", Some("CPU"));
    assert_eq!(resolve_expected_python_provider(), ExecutionProvider::Cpu);
}

/// Issue #3493 P1: with `TRUSTY_DEVICE` unset, Apple Silicon predicts `Mps`
/// (the Python sidecar's device resolver picks MPS first on aarch64-macOS);
/// an `embedder-cuda` build predicts `Cuda`; every other host predicts `Cpu`.
#[tokio::test]
async fn resolve_expected_python_provider_default_matches_platform() {
    let _g = ENV_LOCK.lock().await;
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
#[tokio::test]
#[ignore]
async fn fast_embedder_model_name_reports_fp32_default() {
    let _g = ENV_LOCK.lock().await;
    let _m = EnvVarGuard::apply("TRUSTY_EMBEDDER_MODEL", None);
    let e = FastEmbedder::new().await.unwrap();
    assert_eq!(e.model_name(), "all-MiniLM-L6-v2");
}

/// Issue #3530 — the explicit INT8 opt-in must be reflected by
/// `model_name()` too. `#[ignore]`: downloads a real ONNX model.
#[tokio::test]
#[ignore]
async fn fast_embedder_model_name_reports_int8_opt_in() {
    let _g = ENV_LOCK.lock().await;
    let _m = EnvVarGuard::apply("TRUSTY_EMBEDDER_MODEL", Some("int8"));
    let e = FastEmbedder::new().await.unwrap();
    assert_eq!(e.model_name(), "all-MiniLM-L6-v2-int8");
}
