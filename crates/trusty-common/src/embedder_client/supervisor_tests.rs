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

    /// Like `write_mock_embedderd`, but answers exactly the one startup-probe
    /// request and then exits non-zero — used to drive the supervisor into
    /// its respawn back-off path deterministically.
    ///
    /// Why: `supervisor_shutdown_during_respawn_backoff_returns_promptly`
    /// needs the mock child to actually crash (not merely idle) so
    /// `supervision_loop` takes the `RestartTrigger::ProcessExit` branch,
    /// increments `consecutive_failures`, and enters the exponential
    /// back-off `tokio::select!` (supervisor.rs ~835-847) that races
    /// `shutdown_rx.changed()` against the delay sleep.
    /// What: identical wire protocol to `write_mock_embedderd` for the first
    /// (and only) request, then `exit 1` instead of looping.
    /// Test: `supervisor_shutdown_during_respawn_backoff_returns_promptly`.
    fn write_mock_embedderd_crash_once() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("mock-embedderd-crash-once.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
[ -n "$id" ] || id=1
printf '{"jsonrpc":"2.0","result":{"embeddings":[[0.1]]},"id":%s}\n' "$id"
exit 1
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

    /// Drives the OTHER race window the #2979 doc comments claim to close:
    /// a shutdown requested while the respawn back-off sleep is already in
    /// progress (supervisor.rs's `tokio::select!` at ~835-847, racing
    /// `tokio::time::sleep(delay_secs)` against `shutdown_rx.changed()`).
    /// The two existing shutdown tests above only ever shut down while the
    /// child is idly alive, which never touches the backoff sleep at all.
    ///
    /// Why: before #2979, a shutdown requested mid-backoff would wait out
    /// the delay and then respawn anyway — the whole point of racing
    /// `shutdown_rx` inside that specific `select!` is to let `shutdown()`
    /// win immediately instead of blocking for the remainder of the sleep.
    /// What: the mock child answers the startup probe once and then exits
    /// non-zero, so `supervision_loop` takes the `RestartTrigger::ProcessExit`
    /// branch, clears `pid_slot` to 0, and enters its first back-off sleep
    /// (2s, since `consecutive_failures` becomes 1 and delay = 1 << 1). The
    /// test polls `pid_slot` for that 0 transition — a condition that can
    /// only be observed once the loop has processed the crash and is about
    /// to (or already does) race the backoff sleep — then calls
    /// `handle.shutdown()` and asserts it returns in well under the 2s
    /// delay, and that `pid_slot` never goes non-zero (no respawn) even
    /// after waiting past what the full backoff + respawn would have taken.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_shutdown_during_respawn_backoff_returns_promptly() {
        let (_dir, binary) = write_mock_embedderd_crash_once();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            backoff_max_secs: 60,
            max_restarts: 5,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");
        let initial_pid = pid_slot.load(Ordering::Acquire);
        assert!(initial_pid > 0, "pid_slot must be populated after spawn");

        let handle = supervisor.start_supervisor_task();

        // Condition-based wait (no arbitrary sleep-then-hope): poll until the
        // supervision loop has observed the mock child's non-zero exit and
        // cleared `pid_slot` to 0. That clear happens synchronously in the
        // `RestartTrigger::ProcessExit` arm, immediately before the loop
        // enters the back-off `tokio::select!` — so this condition proves
        // we're now racing (or about to race) the backoff sleep, without
        // guessing at how long the crash takes to propagate.
        let entered_backoff = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if pid_slot.load(Ordering::Acquire) == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(
            entered_backoff.is_ok(),
            "supervisor never observed the mock child's non-zero exit within 5s"
        );

        // The first exponential back-off delay for a process-exit restart is
        // 2s (1 << consecutive_failures, consecutive_failures == 1 here).
        // `shutdown()` must win the race against that sleep by returning
        // almost immediately, not block for (a large fraction of) the delay.
        let before_shutdown = tokio::time::Instant::now();
        handle.shutdown().await;
        let shutdown_elapsed = before_shutdown.elapsed();

        assert!(
            shutdown_elapsed < Duration::from_millis(500),
            "shutdown() took {shutdown_elapsed:?} — should return promptly by \
             winning the tokio::select! against the 2s respawn back-off sleep, \
             not wait it out"
        );
        assert_eq!(
            pid_slot.load(Ordering::Acquire),
            0,
            "pid_slot must stay 0 — shutdown() requested mid-backoff must not \
             let the loop proceed to respawn"
        );

        // Wait past what the full 2s backoff + a respawn would have taken, to
        // catch a delayed respawn a narrower race window might still allow.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(
            pid_slot.load(Ordering::Acquire),
            0,
            "pid_slot must STILL be 0 well past the backoff window — a \
             non-zero PID here would mean shutdown mid-backoff was still \
             followed by a respawn"
        );
    }

    /// Regression test for issue #3023 HIGH: dropping a `SupervisorHandle`
    /// WITHOUT ever calling `.shutdown()` drops `shutdown_tx`, closing the
    /// channel. `watch::Receiver::changed()` then resolves `Ready(Err(_))`
    /// on every subsequent poll — an immediately-ready future never yields
    /// to the scheduler, so before the fix the main-loop `select!` branch
    /// (`if !*shutdown_rx.borrow() { continue }` with the `Err` case
    /// unhandled) busy-spun a tokio worker for as long as the child lived.
    ///
    /// Why: every existing shutdown test calls `handle.shutdown().await`
    /// explicitly; none exercise the "handle just dropped" path that a
    /// caller forgetting (or racing a panic past) the `#[must_use]` hint
    /// would hit in production.
    /// What: spawns the idling mock sidecar, detaches via
    /// `start_supervisor_task`, resets the test-only
    /// `SUPERVISION_LOOP_ITERATIONS` counter, then drops the handle without
    /// calling `shutdown()`. A multi-thread runtime (2 workers) is used
    /// deliberately: if the bug regresses, the busy-spin monopolizes one
    /// worker while this test's own timer/assertions still progress on the
    /// other, so a regression shows up as an assertion failure (iteration
    /// count far exceeds a normal blocked-loop count) rather than a hang.
    /// It then asserts the loop is bounded over a 300ms window, and finally
    /// kills the (still-alive) child out-of-band and confirms the loop
    /// still notices the exit and respawns it — proving the closed
    /// shutdown channel only disabled its own `select!` branch
    /// (`shutdown_closed`) and did not wedge ongoing supervision.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_dropped_handle_does_not_busy_spin() {
        let (_dir, binary) = write_mock_embedderd();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            max_restarts: 5,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");
        let initial_pid = pid_slot.load(Ordering::Acquire);
        assert!(initial_pid > 0, "pid_slot must be populated after spawn");

        super::SUPERVISION_LOOP_ITERATIONS.store(0, Ordering::Relaxed);

        let handle = supervisor.start_supervisor_task();
        // Deliberately drop WITHOUT calling `.shutdown()`. This closes
        // `shutdown_tx` (the sender half); dropping the `JoinHandle` does
        // NOT abort the detached `tokio::spawn`ed supervision task, so it
        // keeps running in the background exactly as it would in
        // production if a caller lost the handle.
        drop(handle);

        tokio::time::sleep(Duration::from_millis(300)).await;

        let iterations = super::SUPERVISION_LOOP_ITERATIONS.load(Ordering::Relaxed);
        assert!(
            iterations < 200,
            "supervision_loop iterated {iterations} times in 300ms after its \
             shutdown handle was dropped without shutdown() — expected a \
             small, bounded count (the loop blocked on child.wait() / \
             unhealthy_signal as usual), not a busy-spin on the closed \
             shutdown_rx channel"
        );

        // Supervision must still function normally: force the still-alive
        // child down out-of-band and confirm the loop notices the exit and
        // respawns it, proving the closed shutdown channel didn't wedge the
        // loop — only its own (now-permanently-disabled) `select!` branch.
        std::process::Command::new("kill")
            .args(["-9", &initial_pid.to_string()])
            .status()
            .expect("kill -9 mock child");

        let respawned = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pid = pid_slot.load(Ordering::Acquire);
                if pid != 0 && pid != initial_pid {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            respawned.is_ok(),
            "supervisor never respawned the killed child within 5s — a \
             closed shutdown channel must not wedge ongoing supervision"
        );
    }

    /// PR #3560 review HIGH fix: `SupervisorHandle::has_given_up()` must flip
    /// to `true` exactly when `supervision_loop`'s own `should_give_up` check
    /// trips — this is the REAL signal trusty-search's
    /// `FallbackEmbedderAdapter` now observes instead of an independently
    /// counted request-failure proxy (see `embedder_fallback.rs`'s module
    /// doc in trusty-search for the full rationale).
    ///
    /// Why: without a test pinning this transition, the give-up signal could
    /// silently stop firing (e.g. a future refactor moves the `return` above
    /// the `send`) and nothing would catch it — `FallbackEmbedderAdapter`
    /// would then never trip at all, which is the OPPOSITE failure mode of
    /// the bug this fix closes (search would hard-fail forever instead of
    /// latching to the Rust ort fallback).
    /// What: uses `write_mock_embedderd_crash_once` — every spawned instance
    /// answers its own startup probe, then exits non-zero — so EVERY respawn
    /// attempt counts as a fresh crash. With `max_restarts: 1` and
    /// `backoff_max_secs: 0` (near-instant respawns), the loop crosses
    /// `should_give_up`'s `consecutive_failures > max_restarts` ceiling after
    /// the second crash. Asserts the flag is still `false` immediately after
    /// the first spawn, then polls `has_given_up()` under a bounded timeout
    /// (never a fixed sleep-and-hope) until it flips.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_gives_up_after_max_restarts_flips_has_given_up() {
        let (_dir, binary) = write_mock_embedderd_crash_once();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            backoff_max_secs: 0,
            max_restarts: 1,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, _pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");

        let handle = supervisor.start_supervisor_task();

        assert!(
            !handle.has_given_up(),
            "must not be given up immediately after the first spawn — the \
             supervisor has not even observed the first crash yet"
        );

        let gave_up = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if handle.has_given_up() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;

        assert!(
            gave_up.is_ok(),
            "has_given_up() never flipped to true within 10s of a \
             persistent crash loop with max_restarts=1"
        );
    }

    /// The definitive death signal (epic #3524 slice 6 PR-4 follow-up,
    /// code-critic BLOCK on PR #3584) must flip to `true` when the supervisor
    /// actually exhausts `max_restarts` — never respawning again.
    ///
    /// Why: this is the regression the swap-back watchdog now depends on
    /// instead of the ambiguous `pid==0` + stall-tracker heuristic that could
    /// false-trigger against a healthy sidecar recovering from an ordinary
    /// idle-shutdown (see `trusty-search`'s `swap_back_watchdog` module doc
    /// for the full false-positive analysis this signal replaces).
    /// What: a mock child that always crashes after answering exactly one
    /// startup probe (`write_mock_embedderd_crash_once`), `max_restarts: 1`
    /// so exhaustion is fast, `backoff_max_secs: 1` to keep the test itself
    /// fast. Captures `terminated_signal()` BEFORE detaching (must happen
    /// before `start_supervisor_task` consumes `self`), then polls it until
    /// `true` or a generous timeout.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_terminated_signal_fires_after_exhausting_max_restarts() {
        let (_dir, binary) = write_mock_embedderd_crash_once();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            backoff_max_secs: 1,
            max_restarts: 1,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");
        let initial_pid = pid_slot.load(Ordering::Acquire);
        assert!(initial_pid > 0, "pid_slot must be populated after spawn");

        // Must capture this BEFORE start_supervisor_task consumes `supervisor`.
        let terminated = supervisor.terminated_signal();
        assert!(
            !terminated.load(Ordering::Acquire),
            "must start false — nothing has failed yet"
        );

        let _handle = supervisor.start_supervisor_task();

        let gave_up = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if terminated.load(Ordering::Acquire) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            gave_up.is_ok(),
            "terminated_signal never flipped true within 10s of exhausting \
             max_restarts=1 against an always-crashing mock child"
        );
        assert_eq!(
            pid_slot.load(Ordering::Acquire),
            0,
            "pid_slot must also be 0 once the supervisor has given up"
        );
    }

    /// The definitive death signal must NEVER flip true on an intentional,
    /// cooperative `shutdown()` — only on genuine restart exhaustion.
    ///
    /// Why: this is the other half of the regression guard — a watchdog
    /// gating on `terminated_signal()` must not fire just because the daemon
    /// (or the idle-shutdown watchdog) deliberately stopped a perfectly
    /// healthy sidecar.
    /// What: a long-lived mock child, shut down cooperatively via
    /// `SupervisorHandle::shutdown()`, asserts `terminated_signal()` stays
    /// `false` throughout and after.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_terminated_signal_stays_false_on_intentional_shutdown() {
        let (_dir, binary) = write_mock_embedderd();
        let cfg = SupervisorConfig {
            startup_timeout_secs: 5,
            ..SupervisorConfig::default()
        };
        let (supervisor, _client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");
        assert!(pid_slot.load(Ordering::Acquire) > 0);

        let terminated = supervisor.terminated_signal();
        let handle = supervisor.start_supervisor_task();

        handle.shutdown().await;

        assert!(
            !terminated.load(Ordering::Acquire),
            "an intentional cooperative shutdown must never set terminated_signal"
        );
        assert_eq!(pid_slot.load(Ordering::Acquire), 0);
    }
}
