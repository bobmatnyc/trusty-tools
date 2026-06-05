//! Unit tests for `EmbedderSupervisor` and `sidecar_batch_size`.
//!
//! Why: isolated in a sibling file (declared via `#[path = "supervisor_tests.rs"]
//! mod tests;` in `supervisor.rs`) to keep `supervisor.rs` under its 709-line
//! allowlist budget while retaining full test coverage.
//!
//! What: exercises `SupervisorConfig::from_env`, `sidecar_batch_size` (all
//! branches including the new CUDA cap from fix #763), and
//! `locate_embedderd_binary` override handling.
//!
//! Test: `cargo test -p trusty-common --features embedder-client,embedder-bundled-ort`

use super::*;

#[test]
fn from_env_uses_defaults_when_no_vars_set() {
    // Why: validate that unset env vars produce the documented defaults.
    // What: construct from env (no vars set in test process by default)
    //       and compare each field.
    // Test: this test.

    // Save any existing env vars to restore later.
    let saved_max = std::env::var("TRUSTY_EMBEDDERD_MAX_RESTARTS").ok();
    let saved_backoff = std::env::var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS").ok();
    let saved_timeout = std::env::var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS").ok();

    // Ensure they are unset during the test.
    // SAFETY: test-only, single-threaded by test framework convention.
    unsafe {
        std::env::remove_var("TRUSTY_EMBEDDERD_MAX_RESTARTS");
        std::env::remove_var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS");
        std::env::remove_var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS");
    }

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.max_restarts, 5);
    assert_eq!(cfg.backoff_max_secs, 60);
    assert_eq!(cfg.startup_timeout_secs, 5);

    // Restore.
    unsafe {
        if let Some(v) = saved_max {
            std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", v);
        }
        if let Some(v) = saved_backoff {
            std::env::set_var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS", v);
        }
        if let Some(v) = saved_timeout {
            std::env::set_var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", v);
        }
    }
}

#[test]
fn parse_env_uses_override() {
    // Why: verify that a valid env-var value overrides the default.
    // What: set the var to "99", call `from_env`, check the field.
    // Test: this test.
    let saved = std::env::var("TRUSTY_EMBEDDERD_MAX_RESTARTS").ok();
    // SAFETY: test-only.
    unsafe {
        std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", "99");
    }
    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.max_restarts, 99);
    unsafe {
        if let Some(v) = saved {
            std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", v);
        } else {
            std::env::remove_var("TRUSTY_EMBEDDERD_MAX_RESTARTS");
        }
    }
}

// ── sidecar_batch_size tests (Fix C issue #747, Fix 2 issue #763) ───────

// Helper: default cuda_cap for tests (same as the runtime constant).
const CUDA_CAP: usize = DEFAULT_CUDA_SIDECAR_BATCH_CAP; // 64

#[test]
fn sidecar_batch_size_cpu_passthrough() {
    // Why: CPU path must forward resolved value unchanged.
    // What: is_coreml=false, is_cuda=false → returns resolved.
    // Test: this test.
    assert_eq!(sidecar_batch_size(128, false, 32, false, CUDA_CAP), 128);
    assert_eq!(sidecar_batch_size(512, false, 32, false, CUDA_CAP), 512);
    assert_eq!(sidecar_batch_size(32, false, 32, false, CUDA_CAP), 32);
}

#[test]
fn sidecar_batch_size_coreml_caps_and_passes_through() {
    // Why: CoreML path must cap at coreml_cap to prevent OOM/jetsam on
    // Apple Silicon, but pass through values at or below the cap.
    // What: is_coreml=true → min(resolved, coreml_cap).
    // Test: this test.
    assert_eq!(sidecar_batch_size(256, true, 32, false, CUDA_CAP), 32);
    assert_eq!(sidecar_batch_size(512, true, 64, false, CUDA_CAP), 64);
    assert_eq!(sidecar_batch_size(16, true, 32, false, CUDA_CAP), 16);
    assert_eq!(sidecar_batch_size(32, true, 32, false, CUDA_CAP), 32);
}

#[test]
fn sidecar_batch_size_zero_resolved_clamps_to_one() {
    // Why: resolved=0 would cause TRUSTY_EMBED_BATCH_SIZE=0 which ONNX
    // Runtime rejects; the guard must clamp to 1 regardless of is_coreml/is_cuda.
    // What: resolved=0, is_coreml=false, is_cuda=false → 1 (clamped from 0).
    // Test: this test.
    assert_eq!(
        sidecar_batch_size(0, false, 32, false, CUDA_CAP),
        1,
        "zero resolved (non-coreml, non-cuda) must clamp to 1"
    );
}

#[test]
fn sidecar_batch_size_zero_coreml_cap_clamps_to_one() {
    // Why: if the CoreML cap is 0, min(resolved, 0) = 0, which is
    // invalid. The guard must still clamp to 1.
    // What: resolved=32, is_coreml=true, coreml_cap=0 → 1 (clamped from 0).
    // Test: this test.
    assert_eq!(
        sidecar_batch_size(32, true, 0, false, CUDA_CAP),
        1,
        "zero coreml_cap must clamp result to 1"
    );
}

#[test]
fn sidecar_batch_size_both_zero_clamps_to_one() {
    // Why: both inputs at zero must still produce a valid result.
    // What: resolved=0, is_coreml=true, coreml_cap=0 → 1.
    // Test: this test.
    assert_eq!(
        sidecar_batch_size(0, true, 0, false, CUDA_CAP),
        1,
        "resolved=0, coreml_cap=0 must clamp to 1"
    );
}

// ── CUDA cap tests (Fix 2, issue #763) ──────────────────────────────────

#[test]
fn sidecar_batch_size_cuda_caps_at_cuda_cap() {
    // Why: Fix #763 — the parent's TRUSTY_MAX_BATCH_SIZE=512 (CUDA wave
    // size) must NOT be forwarded directly to the sidecar's ORT session.
    // With INFLIGHT=2 that would produce two concurrent 512-chunk sessions
    // saturating the T4 BFCArena. The cuda_cap (default 64) bounds the
    // per-ORT-call batch size.
    // What: is_cuda=true, resolved=512, cuda_cap=64 → 64.
    // Test: this test.
    assert_eq!(
        sidecar_batch_size(512, false, 32, true, 64),
        64,
        "CUDA: 512 must be capped to 64"
    );
    assert_eq!(
        sidecar_batch_size(256, false, 32, true, 64),
        64,
        "CUDA: 256 must be capped to 64"
    );
    assert_eq!(
        sidecar_batch_size(32, false, 32, true, 64),
        32,
        "CUDA: 32 is already below the 64 cap — passes through"
    );
    assert_eq!(
        sidecar_batch_size(64, false, 32, true, 64),
        64,
        "CUDA: exactly at cap — passes through"
    );
}

#[test]
fn sidecar_batch_size_cuda_zero_cap_clamps_to_one() {
    // Why: cuda_cap=0 would produce min(resolved, 0)=0 which ORT rejects.
    // What: is_cuda=true, cuda_cap=0 → 1 (guard clamps to 1).
    // Test: this test.
    assert_eq!(
        sidecar_batch_size(32, false, 32, true, 0),
        1,
        "zero cuda_cap must clamp result to 1"
    );
}

#[test]
fn sidecar_batch_size_coreml_takes_priority_over_cuda() {
    // Why: is_coreml and is_cuda should not both be true in practice, but
    // the function must behave deterministically — CoreML branch is checked
    // first, so coreml_cap wins.
    // What: is_coreml=true, is_cuda=true → CoreML path applies.
    // Test: this test.
    assert_eq!(
        sidecar_batch_size(512, true, 32, true, 64),
        32,
        "when both flags set, CoreML takes priority"
    );
}

#[test]
fn locate_binary_respects_explicit_override() {
    // Why: `TRUSTY_EMBEDDERD_BIN` must take priority over all discovery.
    // What: set `TRUSTY_EMBEDDERD_BIN` to a non-existent path — the
    //       function should return an error mentioning the path.
    // Test: this test.
    let saved = std::env::var("TRUSTY_EMBEDDERD_BIN").ok();
    unsafe {
        std::env::set_var("TRUSTY_EMBEDDERD_BIN", "/no/such/binary");
    }
    let result = locate_embedderd_binary();
    assert!(result.is_err(), "must fail on non-existent override path");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("TRUSTY_EMBEDDERD_BIN"),
        "error must mention the env var"
    );
    unsafe {
        if let Some(v) = saved {
            std::env::set_var("TRUSTY_EMBEDDERD_BIN", v);
        } else {
            std::env::remove_var("TRUSTY_EMBEDDERD_BIN");
        }
    }
}
