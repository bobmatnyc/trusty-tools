//! Tests for memory_policy — env-var overrides, coreml, tripwire, RAM detect.
//!
//! Why: env-mutating tests need the crate's `#[serial]` group to prevent races
//! between parallel test threads; a test that resolves a `MemoryPolicy` runs
//! in an isolated child instead (#12111). They are isolated here so their
//! process-global side effects don't mix with the pure compute tests in
//! `tests_basic`.
//! What: tests for `MemoryPolicy::from_total_ram_mb` with env overrides,
//! tier hard caps, coreml batch-size helpers, tripwire helpers, and RAM
//! detection.
//! Test: run with `cargo test -p trusty-search`.

use super::coreml::{
    resolve_coreml_batch_size, resolve_coreml_tripwire_mb, COREML_BATCH_SIZE_MAX,
    DEFAULT_COREML_BATCH_SIZE, DEFAULT_COREML_TRIPWIRE_MB,
};
use super::policy::MemoryPolicy;
// #6820: re-exported from trusty-common through `super`, so these tests exercise
// the same path every production call site takes.
use super::{detect_total_ram_mb, MemoryTier};

// #5937: env-mutating tests here run in the crate's unnamed `#[serial]`
// group; a module-private mutex excluded no other env test in the binary.
//
// #12111: `#[serial]` does not fence non-serial readers. Every
// `MemoryPolicy::from_total_ram_mb` call runs `apply_to_env`, which writes
// `TRUSTY_MAX_CHUNKS` and seven other caps process-wide; the non-serial
// `core::store::tests::test_adopt_*` tests read `TRUSTY_MAX_CHUNKS=42` from
// `test_env_override` mid-run. Each such test now resolves its policy in an
// isolated child process (`run_isolated`, #4213), so the parent env is never
// written.

/// Run the named `tests_env` test alone in a child with `envs` set at spawn.
/// `true` only inside the child, where the caller runs its body.
fn isolate(test: &str, envs: &[(&str, &str)]) -> bool {
    crate::service::test_isolation::run_isolated(
        &format!("core::memory_policy::tests_env::{test}"),
        envs,
    )
}

/// Remove `keys` from this process's env. Callers run only inside an
/// isolated child, which runs one test on one thread.
fn clear_env(keys: &[&str]) {
    for key in keys {
        // SAFETY: called only inside an isolated child (see above).
        unsafe { std::env::remove_var(key) };
    }
}

#[test]
fn test_index_memory_limit_resolution_floors_at_global() {
    // When TRUSTY_INDEX_MEMORY_LIMIT_MB is set below TRUSTY_MEMORY_LIMIT_MB,
    // MemoryPolicy must clamp it back up to at least the global limit.
    // Otherwise the indexing pipeline would run with a tighter ceiling
    // than the steady-state daemon — almost certainly a misconfiguration.
    if !isolate(
        "test_index_memory_limit_resolution_floors_at_global",
        &[
            ("TRUSTY_MEMORY_LIMIT_MB", "8192"),
            ("TRUSTY_INDEX_MEMORY_LIMIT_MB", "2048"), // < global
        ],
    ) {
        return;
    }
    let policy = MemoryPolicy::from_total_ram_mb(32 * 1024);
    assert_eq!(policy.memory_limit_mb, 8_192);
    // Indexing limit must be floored at the global limit, not the
    // operator's smaller value.
    assert_eq!(policy.index_memory_limit_mb, 8_192);
}

#[test]
fn test_memory_limit_scales_proportionally_across_xlarge_hosts() {
    // Regression test for issue #120: two XLarge hosts of different sizes
    // must produce different memory limits (the old code returned 16 GB
    // for both 64 GB and 128 GB boxes).
    if !isolate(
        "test_memory_limit_scales_proportionally_across_xlarge_hosts",
        &[],
    ) {
        return;
    }
    clear_env(&["TRUSTY_MEMORY_LIMIT_MB"]);
    let p64 = MemoryPolicy::from_total_ram_mb(64 * 1024);
    // The 64 GB call wrote TRUSTY_MEMORY_LIMIT_MB=16384 back into the env;
    // clear it again so the 128 GB call sees a clean slate.
    clear_env(&["TRUSTY_MEMORY_LIMIT_MB"]);
    let p128 = MemoryPolicy::from_total_ram_mb(128 * 1024);
    assert_eq!(p64.tier, MemoryTier::XLarge);
    assert_eq!(p128.tier, MemoryTier::XLarge);
    assert!(
        p128.memory_limit_mb > p64.memory_limit_mb,
        "128 GB host ({} MB) should have a larger memory_limit_mb than \
         a 64 GB host ({} MB) — see issue #120",
        p128.memory_limit_mb,
        p64.memory_limit_mb,
    );
    // Specifically: 128 GB → 32 GB limit; 64 GB → 16 GB limit.
    assert_eq!(p64.memory_limit_mb, 16 * 1024);
    assert_eq!(p128.memory_limit_mb, 32 * 1024);
}

/// Verify that an env-var override beats the tier default.
///
/// Note: `from_total_ram_mb` calls `apply_to_env`, which mutates the
/// process env, so the body runs in an isolated child (#12111): an in-process
/// `TRUSTY_MAX_CHUNKS=42` capped the non-serial store adopt tests at 42.
#[test]
fn test_env_override() {
    if !isolate("test_env_override", &[("TRUSTY_MAX_CHUNKS", "42")]) {
        return;
    }
    // 16 GB → Medium tier (default max_chunks = 200_000). Env should win.
    let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
    assert_eq!(policy.tier, MemoryTier::Medium);
    assert_eq!(policy.max_chunks, 42);
}

#[test]
#[serial_test::serial]
fn test_tier_batch_size_hard_cap() {
    // Issue #89: tier-specific batch-size hard caps protect against
    // runaway TRUSTY_MAX_BATCH_SIZE overrides on memory-constrained hosts.
    // Raised in issue #19 to track the recalibrated CPU-no-arena per-slot
    // cost (~32 MB instead of the prior 200 MB arena estimate).
    assert_eq!(MemoryTier::Medium.batch_size_hard_cap(), 128);
    assert_eq!(MemoryTier::Large.batch_size_hard_cap(), 256);
    assert_eq!(MemoryTier::XLarge.batch_size_hard_cap(), 512);
}

#[test]
fn test_batch_size_env_override_clamped_by_hard_cap() {
    if !isolate(
        "test_batch_size_env_override_clamped_by_hard_cap",
        &[("TRUSTY_MAX_BATCH_SIZE", "2048")],
    ) {
        return;
    }
    // Ensure the explicit-bypass flag is unset for this test.
    clear_env(&["TRUSTY_MAX_BATCH_SIZE_EXPLICIT"]);

    // 16 GB → Medium tier, hard cap = 128. Env value of 2048 must be
    // clamped down to the tier cap.
    let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
    assert_eq!(policy.tier, MemoryTier::Medium);
    assert_eq!(
        policy.max_batch_size, 128,
        "Medium tier must clamp TRUSTY_MAX_BATCH_SIZE=2048 down to 128"
    );
}

#[test]
fn test_batch_size_explicit_flag_bypasses_clamp() {
    if !isolate(
        "test_batch_size_explicit_flag_bypasses_clamp",
        &[
            ("TRUSTY_MAX_BATCH_SIZE", "512"),
            ("TRUSTY_MAX_BATCH_SIZE_EXPLICIT", "1"),
        ],
    ) {
        return;
    }
    // Clear any inherited memory-limit override so the proportional default
    // for a 16 GB host applies cleanly.
    clear_env(&["TRUSTY_MEMORY_LIMIT_MB"]);

    // 16 GB → Medium tier, hard cap = 16. With EXPLICIT=1 the operator's
    // 512 must be honored verbatim (GPU path, expert opt-out).
    let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
    assert_eq!(policy.tier, MemoryTier::Medium);
    assert_eq!(
        policy.max_batch_size, 512,
        "TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 must bypass the tier hard cap"
    );
}

#[test]
#[serial_test::serial]
fn test_coreml_batch_size_default() {
    let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
    // SAFETY: serialized via #[serial].
    unsafe { std::env::remove_var("TRUSTY_COREML_BATCH_SIZE") };
    assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
    // SAFETY: serialized via #[serial].
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
        }
    }
}

#[test]
#[serial_test::serial]
fn test_coreml_batch_size_env_override() {
    let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
    // SAFETY: serialized via #[serial].
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "64") };
    assert_eq!(resolve_coreml_batch_size(), 64);
    // SAFETY: serialized via #[serial].
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
        }
    }
}

#[test]
#[serial_test::serial]
fn test_coreml_batch_size_env_clamp() {
    let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
    // Out-of-range upper: clamp to MAX.
    // SAFETY: serialized via #[serial].
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "10000") };
    assert_eq!(resolve_coreml_batch_size(), COREML_BATCH_SIZE_MAX);
    // Zero: fall back to default (with warn).
    // SAFETY: serialized via #[serial].
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "0") };
    assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
    // Garbage: fall back to default (with warn).
    // SAFETY: serialized via #[serial].
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "not-a-number") };
    assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
    // SAFETY: serialized via #[serial].
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
        }
    }
}

#[test]
#[serial_test::serial]
fn test_coreml_tripwire_default() {
    let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
    // SAFETY: serialized via #[serial].
    unsafe { std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB") };
    assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
    // SAFETY: serialized via #[serial].
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
            None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
        }
    }
}

#[test]
#[serial_test::serial]
fn test_coreml_tripwire_env_override() {
    let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
    // SAFETY: serialized via #[serial].
    unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "8192") };
    assert_eq!(resolve_coreml_tripwire_mb(), 8192);
    // SAFETY: serialized via #[serial].
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
            None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
        }
    }
}

#[test]
#[serial_test::serial]
fn test_coreml_tripwire_env_invalid() {
    let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
    // Zero: fall back to default (with warn).
    // SAFETY: serialized via #[serial].
    unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "0") };
    assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
    // Garbage: fall back to default (with warn).
    // SAFETY: serialized via #[serial].
    unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "not-a-number") };
    assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
    // SAFETY: serialized via #[serial].
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
            None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
        }
    }
}

#[test]
#[serial_test::serial]
fn test_ram_detection_returns_nonzero() {
    // Best-effort: on macOS/Linux CI hosts this must return a real value.
    // On other platforms (none in our CI matrix today) the function
    // returns None and we skip the assertion rather than fail.
    if let Some(mb) = detect_total_ram_mb() {
        assert!(mb > 0, "detected RAM should be > 0, got {mb}");
        // Sanity ceiling: no host in our deployment fleet has > 4 TB.
        assert!(
            mb < 4 * 1024 * 1024,
            "detected RAM implausibly large: {mb} MB"
        );
    }
}

/// #9235 tier coupling: the cap the truncation report names is the cap the
/// memory tier set. Applying each tier's `MemoryPolicy` must move
/// `CodeBm25Index::corpus_cap()` to that policy's `bm25_corpus_cap`.
/// What: in an isolated child (`apply_to_env` writes process-global env,
/// #6369), resolves the policy at one RAM size per tier, applies it, and
/// compares. The tiers' caps differ, so a cap that ignores the policy fails.
/// Test: this test.
#[test]
fn applying_a_tier_policy_sets_the_cap_the_bm25_lane_reports() {
    use crate::core::bm25::CodeBm25Index;
    if !crate::service::test_isolation::run_isolated(
        "core::memory_policy::tests_env::applying_a_tier_policy_sets_the_cap_the_bm25_lane_reports",
        &[] as &[(&str, &str)],
    ) {
        return;
    }
    let mut caps = std::collections::BTreeSet::new();
    for (ram_mb, tier) in [
        (12 * 1024, MemoryTier::Degraded),
        (24 * 1024, MemoryTier::Medium),
        (48 * 1024, MemoryTier::Large),
        (128 * 1024, MemoryTier::XLarge),
    ] {
        // SAFETY: this isolated child runs one test on one thread. Clearing the
        // override keeps the previous tier's applied cap out of this policy.
        unsafe { std::env::remove_var("TRUSTY_BM25_CORPUS_CAP") };
        let policy = MemoryPolicy::from_total_ram_mb(ram_mb);
        assert_eq!(policy.tier, tier);
        policy.apply_to_env();
        assert_eq!(
            CodeBm25Index::corpus_cap(),
            policy.bm25_corpus_cap,
            "{tier}: the BM25 lane reports a cap the tier did not set"
        );
        caps.insert(policy.bm25_corpus_cap);
    }
    assert_eq!(caps.len(), 4, "every tier must set its own cap: {caps:?}");
}
