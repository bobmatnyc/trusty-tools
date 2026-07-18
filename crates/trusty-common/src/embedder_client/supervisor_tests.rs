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
use serial_test::serial;

#[test]
#[serial]
fn from_env_uses_defaults_when_no_vars_set() {
    // Why: validate that unset env vars produce the documented defaults.
    // What: construct from env (no vars set in test process by default)
    //       and compare each field.
    // Test: this test.

    // Save any existing env vars to restore later.
    let saved_max = std::env::var("TRUSTY_EMBEDDERD_MAX_RESTARTS").ok();
    let saved_backoff = std::env::var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS").ok();
    let saved_timeout = std::env::var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS").ok();
    let saved_wedge_reset = std::env::var("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS").ok();

    // Ensure they are unset during the test.
    // SAFETY: test-only, single-threaded by test framework convention.
    unsafe {
        std::env::remove_var("TRUSTY_EMBEDDERD_MAX_RESTARTS");
        std::env::remove_var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS");
        std::env::remove_var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS");
        std::env::remove_var("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS");
    }

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.max_restarts, 5);
    assert_eq!(cfg.backoff_max_secs, 60);
    assert_eq!(cfg.startup_timeout_secs, 5);
    assert_eq!(cfg.wedge_reset_secs, 300);

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
        if let Some(v) = saved_wedge_reset {
            std::env::set_var("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", v);
        }
    }
}

#[test]
#[serial]
fn wedge_reset_secs_env_override() {
    // Why: operators must be able to tune the sustained-health reset window
    // without recompiling (#1450 HIGH follow-up).
    // What: set the var to "42", call `from_env`, check the field.
    // Test: this test.
    let saved = std::env::var("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS").ok();
    // SAFETY: test-only.
    unsafe {
        std::env::set_var("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", "42");
    }
    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.wedge_reset_secs, 42);
    unsafe {
        if let Some(v) = saved {
            std::env::set_var("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", v);
        } else {
            std::env::remove_var("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS");
        }
    }
}

#[test]
#[serial]
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
#[serial]
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

// ── Wedge-restart-storm prevention (#1450 HIGH follow-up) ──────────────
//
// `wedge_counter_should_reset` and `should_give_up` are pure functions
// (no async, no I/O) — the async supervision loop itself is exercised only
// by the ignored real-binary e2e tests, so this logic is unit-tested
// directly here per the review guidance ("test the counter logic as a pure
// function if the async path is hard to drive").

#[test]
fn wedge_counter_should_reset_none_never_resets() {
    // Why: no prior wedge this run — nothing to reset.
    // What: `elapsed_since_last_wedge = None` → always false regardless of
    // the configured window.
    // Test: this test.
    assert!(!wedge_counter_should_reset(None, 300));
    assert!(!wedge_counter_should_reset(None, 0));
}

#[test]
fn wedge_counter_should_reset_before_window_stays_escalated() {
    // Why: a wedge that recurs before the sustained-health window elapses
    // must NOT reset — that is exactly the storm case this fix targets.
    // What: elapsed < wedge_reset_secs → false.
    // Test: this test.
    assert!(!wedge_counter_should_reset(
        Some(Duration::from_secs(299)),
        300
    ));
    assert!(!wedge_counter_should_reset(
        Some(Duration::from_secs(0)),
        300
    ));
}

#[test]
fn wedge_counter_should_reset_after_window_resets() {
    // Why: sustained health for the configured window is the ONLY way the
    // counter resets (never an ordinary respawn-probe success).
    // What: elapsed >= wedge_reset_secs → true, at and beyond the boundary.
    // Test: this test.
    assert!(wedge_counter_should_reset(
        Some(Duration::from_secs(300)),
        300
    ));
    assert!(wedge_counter_should_reset(
        Some(Duration::from_secs(301)),
        300
    ));
}

#[test]
fn should_give_up_neither_counter_exceeds() {
    // Why: normal operation — a couple of crashes/wedges within budget must
    // not trip the ceiling.
    // What: both counters <= max_restarts → false.
    // Test: this test.
    assert!(!should_give_up(2, 2, 5));
    assert!(!should_give_up(5, 0, 5));
    assert!(!should_give_up(0, 5, 5));
}

#[test]
fn should_give_up_crash_storm_trips_ceiling() {
    // Why: the pre-existing crash-storm behaviour must be preserved.
    // What: consecutive_failures alone exceeding max_restarts → true.
    // Test: this test.
    assert!(should_give_up(6, 0, 5));
}

#[test]
fn should_give_up_wedge_storm_trips_ceiling_even_with_failures_reset() {
    // Why: THIS is the restart-storm fix under test — a workload-
    // deterministic wedge where every individual respawn probe succeeds
    // (so `consecutive_failures` is reset to 0 each cycle by the caller)
    // must still eventually give up once `consecutive_wedge_restarts`
    // climbs past `max_restarts`.
    // What: consecutive_failures=0 (just reset by a successful respawn),
    // consecutive_wedge_restarts=6 > max_restarts=5 → true.
    // Test: this test.
    assert!(should_give_up(0, 6, 5));
}

#[test]
fn should_give_up_at_boundary_does_not_trip() {
    // Why: the ceiling is `> max_restarts`, not `>=` — exactly
    // `max_restarts` consecutive failures/wedges is still tolerated (matches
    // the pre-existing crash-storm semantics).
    // What: both counters exactly at max_restarts → false.
    // Test: this test.
    assert!(!should_give_up(5, 5, 5));
}

// ── Cooperative shutdown (issue #2979) ──────────────────────────────────
//
// These tests spawn a real (but tiny) child process — a POSIX shell script
// that speaks just enough of the trusty-embedderd stdio JSON-RPC wire
// protocol to pass `spawn_child`'s startup probe, then idles until killed —
// so `EmbedderSupervisor::spawn_stdio` and `start_supervisor_task` exercise
// their real process-lifecycle code paths without needing the actual ONNX
// binary (which would pull in multi-second model-load time just for a
// lifecycle assertion).
#[cfg(unix)]
mod shutdown_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// Write a minimal stdio JSON-RPC mock of `trusty-embedderd --stdio` to a
    /// temp file; returns the path plus the guarding `TempDir` (kept alive by
    /// the caller for as long as the script must remain spawnable on disk).
    ///
    /// Why: `spawn_child`'s startup probe sends one real `embed` JSON-RPC
    /// request and requires a well-formed response within
    /// `startup_timeout_secs` — a no-op binary would fail the probe. The mock
    /// echoes back each request's `id` with one canned embedding (every
    /// request in these tests sends exactly one text), then loops reading
    /// (idling, like a real sidecar between requests) until the test process
    /// kills it.
    /// What: a `/bin/sh` script using only POSIX `read`/`sed`/`printf` — no
    /// interpreter dependency beyond the shell every CI runner in this
    /// workspace already has. Unix-only: the raw-kill behaviour this fix
    /// replaces was already Unix-only (see `idle_watchdog`'s `#[cfg(unix)]`
    /// gate in `trusty-search`), so Windows coverage is out of scope here.
    /// Test: used by `supervisor_shutdown_kills_child`,
    /// `supervisor_shutdown_handle_is_reachable_and_stops_child`,
    /// `supervisor_intentional_shutdown_does_not_respawn`.
    fn write_mock_embedderd() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("mock-embedderd.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || id=1
  printf '{"jsonrpc":"2.0","result":{"embeddings":[[0.1]]},"id":%s}\n' "$id"
done
"#,
        )
        .expect("write mock script");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod +x");
        (dir, path)
    }

    /// Best-effort liveness check via `kill -0 <pid>` (no signal sent, just
    /// an existence probe): success means the process still exists, failure
    /// means it is gone.
    /// Why: extracted for reuse across the tests below; trusty-common has no
    /// `nix` dependency, so this shells out to the POSIX `kill` utility
    /// rather than pulling one in just for a test assertion.
    /// What: `Command::new("kill").args(["-0", pid])`; `true` iff it exits 0.
    /// Test: exercised indirectly by every test in this module.
    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Verify the pre-detach `EmbedderSupervisor::shutdown()` actually kills
    /// the child — the one case where it was always reachable, since it only
    /// becomes unreachable once `start_supervisor_task` consumes `self`.
    ///
    /// Why: this method's doc has cited this test name since before issue
    /// #2979 (it was dangling — grandfathered in `.test-pointer-allowlist.tsv`);
    /// making it real is a drive-by fix while touching this file for the
    /// same issue.
    /// What: spawn the mock, capture its PID, call `shutdown()` without ever
    /// detaching, assert the OS process is gone.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_shutdown_kills_child() {
        let (_dir, binary) = write_mock_embedderd();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");
        let pid = pid_slot.load(Ordering::Acquire);
        assert!(pid > 0, "pid_slot must be populated after spawn");

        supervisor.shutdown().await;

        assert!(
            !process_alive(pid),
            "child process {pid} must be gone after shutdown()"
        );
    }

    /// Core issue #2979 assertion: `shutdown()` is reachable AFTER
    /// `start_supervisor_task` — via the `SupervisorHandle` it now returns —
    /// and actually stops the supervised child.
    ///
    /// Why: before this fix, `start_supervisor_task(self)` consumed `self`
    /// and returned nothing, so a caller that detached (the only way
    /// trusty-search ever used this type) had no way left to call
    /// `shutdown()` at all.
    /// What: spawn the mock, detach via `start_supervisor_task` (capturing
    /// the returned handle), call `handle.shutdown().await`, assert the
    /// child is gone and `child_pid_slot` was cleared to 0.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_shutdown_handle_is_reachable_and_stops_child() {
        let (_dir, binary) = write_mock_embedderd();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");
        let pid = pid_slot.load(Ordering::Acquire);
        assert!(pid > 0, "pid_slot must be populated after spawn");

        let handle = supervisor.start_supervisor_task();
        handle.shutdown().await;

        assert!(
            !process_alive(pid),
            "child process {pid} must be gone after SupervisorHandle::shutdown()"
        );
        assert_eq!(
            pid_slot.load(Ordering::Acquire),
            0,
            "child_pid_slot must be cleared to 0 after cooperative shutdown"
        );
    }

    /// The other half of issue #2979: an intentional shutdown must NEVER
    /// trigger the crash-restart path.
    ///
    /// Why: the bug this issue reports is not merely "shutdown doesn't
    /// work" — it's that the old out-of-band PID kill raced
    /// `supervision_loop`'s `child.wait()`, which had no way to distinguish
    /// that deliberate kill from a crash and respawned the sidecar the
    /// caller had just stopped.
    /// What: shut down via the handle, then wait comfortably longer than the
    /// first respawn back-off delay (1s) would take, and assert
    /// `child_pid_slot` is STILL 0 — if the loop had misclassified the
    /// shutdown as a crash, a respawn within that window would have
    /// published a fresh non-zero PID.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_intentional_shutdown_does_not_respawn() {
        let (_dir, binary) = write_mock_embedderd();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            max_restarts: 5,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");

        let handle = supervisor.start_supervisor_task();
        handle.shutdown().await;

        assert_eq!(
            pid_slot.load(Ordering::Acquire),
            0,
            "pid_slot must be 0 immediately after cooperative shutdown"
        );

        // Longer than the first exponential back-off delay (1s) a
        // misclassified-as-crash respawn would have used.
        tokio::time::sleep(Duration::from_millis(1500)).await;

        assert_eq!(
            pid_slot.load(Ordering::Acquire),
            0,
            "pid_slot must STILL be 0 well past the first respawn back-off \
             window — a non-zero PID here would mean the shutdown was \
             misclassified as a crash and the sidecar was respawned"
        );
    }
}
