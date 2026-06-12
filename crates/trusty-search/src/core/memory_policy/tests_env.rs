//! Tests for memory_policy — env-var overrides, coreml, tripwire, RAM detect.
//!
//! Why: env-mutating tests need a shared mutex (ENV_LOCK) to prevent races
//! between parallel test threads. They are isolated here so their
//! process-global side effects don't mix with the pure compute tests in
//! `tests_basic`.
//! What: tests for `MemoryPolicy::from_total_ram_mb` with env overrides,
//! tier hard caps, coreml batch-size helpers, tripwire helpers, and RAM
//! detection.
//! Test: run with `cargo test -p trusty-search`.

use super::compute::{
    resolve_coreml_batch_size, resolve_coreml_tripwire_mb, COREML_BATCH_SIZE_MAX,
    DEFAULT_COREML_BATCH_SIZE, DEFAULT_COREML_TRIPWIRE_MB,
};
use super::detect::detect_total_ram_mb;
use super::policy::MemoryPolicy;
use super::tier::MemoryTier;
use std::sync::Mutex;

/// Serialize env-mutating tests within this module. Cargo runs tests on
/// multiple threads by default and `std::env::set_var` is process-global,
/// so without this guard a concurrent test can stomp on the env vars a
/// sibling test relies on (e.g. `TRUSTY_MAX_BATCH_SIZE_EXPLICIT`).
pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_index_memory_limit_resolution_floors_at_global() {
    // When TRUSTY_INDEX_MEMORY_LIMIT_MB is set below TRUSTY_MEMORY_LIMIT_MB,
    // MemoryPolicy must clamp it back up to at least the global limit.
    // Otherwise the indexing pipeline would run with a tighter ceiling
    // than the steady-state daemon — almost certainly a misconfiguration.
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior_idx = std::env::var("TRUSTY_INDEX_MEMORY_LIMIT_MB").ok();
    let prior_mem = std::env::var("TRUSTY_MEMORY_LIMIT_MB").ok();
    // SAFETY: serialized via ENV_LOCK within this module.
    unsafe {
        std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", "8192");
        std::env::set_var("TRUSTY_INDEX_MEMORY_LIMIT_MB", "2048"); // < global
    }
    let policy = MemoryPolicy::from_total_ram_mb(32 * 1024);
    assert_eq!(policy.memory_limit_mb, 8_192);
    // Indexing limit must be floored at the global limit, not the
    // operator's smaller value.
    assert_eq!(policy.index_memory_limit_mb, 8_192);

    // SAFETY: same.
    unsafe {
        match prior_idx {
            Some(v) => std::env::set_var("TRUSTY_INDEX_MEMORY_LIMIT_MB", v),
            None => std::env::remove_var("TRUSTY_INDEX_MEMORY_LIMIT_MB"),
        }
        match prior_mem {
            Some(v) => std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", v),
            None => std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB"),
        }
    }
}

#[test]
fn test_memory_limit_scales_proportionally_across_xlarge_hosts() {
    // Regression test for issue #120: two XLarge hosts of different sizes
    // must produce different memory limits (the old code returned 16 GB
    // for both 64 GB and 128 GB boxes).
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("TRUSTY_MEMORY_LIMIT_MB").ok();
    // SAFETY: tests run single-threaded within this module's env block.
    unsafe {
        std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB");
    }
    let p64 = MemoryPolicy::from_total_ram_mb(64 * 1024);
    // The 64 GB call wrote TRUSTY_MEMORY_LIMIT_MB=16384 back into the env;
    // clear it again so the 128 GB call sees a clean slate.
    // SAFETY: same as above.
    unsafe {
        std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB");
    }
    let p128 = MemoryPolicy::from_total_ram_mb(128 * 1024);
    // Restore prior value.
    // SAFETY: same as above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", v),
            None => std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB"),
        }
    }
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
/// process env. We restore the prior values at the end of the test to
/// avoid bleeding into other tests in the same binary.
#[test]
fn test_env_override() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Save & override.
    let prior = std::env::var("TRUSTY_MAX_CHUNKS").ok();
    // SAFETY: tests run single-threaded within this module's env block.
    unsafe {
        std::env::set_var("TRUSTY_MAX_CHUNKS", "42");
    }

    // 16 GB → Medium tier (default max_chunks = 200_000). Env should win.
    let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
    assert_eq!(policy.tier, MemoryTier::Medium);
    assert_eq!(policy.max_chunks, 42);

    // Restore.
    // SAFETY: same as above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_MAX_CHUNKS", v),
            None => std::env::remove_var("TRUSTY_MAX_CHUNKS"),
        }
    }
}

#[test]
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
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Save & override.
    let prior = std::env::var("TRUSTY_MAX_BATCH_SIZE").ok();
    let prior_explicit = std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT").ok();
    // SAFETY: tests run single-threaded within this module's env block.
    unsafe {
        std::env::set_var("TRUSTY_MAX_BATCH_SIZE", "2048");
        // Ensure the explicit-bypass flag is unset for this test.
        std::env::remove_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT");
    }

    // 16 GB → Medium tier, hard cap = 128. Env value of 2048 must be
    // clamped down to the tier cap.
    let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
    assert_eq!(policy.tier, MemoryTier::Medium);
    assert_eq!(
        policy.max_batch_size, 128,
        "Medium tier must clamp TRUSTY_MAX_BATCH_SIZE=2048 down to 128"
    );

    // Restore.
    // SAFETY: same as above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE"),
        }
        match prior_explicit {
            Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT", v),
            None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT"),
        }
    }
}

#[test]
fn test_batch_size_explicit_flag_bypasses_clamp() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Save & override.
    let prior = std::env::var("TRUSTY_MAX_BATCH_SIZE").ok();
    let prior_explicit = std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT").ok();
    let prior_mem = std::env::var("TRUSTY_MEMORY_LIMIT_MB").ok();
    // SAFETY: tests run single-threaded within this module's env block.
    unsafe {
        std::env::set_var("TRUSTY_MAX_BATCH_SIZE", "512");
        std::env::set_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT", "1");
        // Clear leftover memory-limit env from sibling tests so the
        // proportional default for 16 GB host applies cleanly.
        std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB");
    }

    // 16 GB → Medium tier, hard cap = 16. With EXPLICIT=1 the operator's
    // 512 must be honored verbatim (GPU path, expert opt-out).
    let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
    assert_eq!(policy.tier, MemoryTier::Medium);
    assert_eq!(
        policy.max_batch_size, 512,
        "TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 must bypass the tier hard cap"
    );

    // Restore.
    // SAFETY: same as above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE"),
        }
        match prior_explicit {
            Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT", v),
            None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT"),
        }
        match prior_mem {
            Some(v) => std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", v),
            None => std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB"),
        }
    }
}

#[test]
fn test_coreml_batch_size_default() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::remove_var("TRUSTY_COREML_BATCH_SIZE") };
    assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
    // SAFETY: serialized via ENV_LOCK.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
        }
    }
}

#[test]
fn test_coreml_batch_size_env_override() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "64") };
    assert_eq!(resolve_coreml_batch_size(), 64);
    // SAFETY: serialized via ENV_LOCK.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
        }
    }
}

#[test]
fn test_coreml_batch_size_env_clamp() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
    // Out-of-range upper: clamp to MAX.
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "10000") };
    assert_eq!(resolve_coreml_batch_size(), COREML_BATCH_SIZE_MAX);
    // Zero: fall back to default (with warn).
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "0") };
    assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
    // Garbage: fall back to default (with warn).
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "not-a-number") };
    assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
    // SAFETY: serialized via ENV_LOCK.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
            None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
        }
    }
}

#[test]
fn test_coreml_tripwire_default() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB") };
    assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
    // SAFETY: serialized via ENV_LOCK.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
            None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
        }
    }
}

#[test]
fn test_coreml_tripwire_env_override() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "8192") };
    assert_eq!(resolve_coreml_tripwire_mb(), 8192);
    // SAFETY: serialized via ENV_LOCK.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
            None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
        }
    }
}

#[test]
fn test_coreml_tripwire_env_invalid() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
    // Zero: fall back to default (with warn).
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "0") };
    assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
    // Garbage: fall back to default (with warn).
    // SAFETY: serialized via ENV_LOCK.
    unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "not-a-number") };
    assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
    // SAFETY: serialized via ENV_LOCK.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
            None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
        }
    }
}

#[test]
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
