//! Startup-time sanity checks emitted as `tracing::warn!` during daemon boot.
//!
//! Why: certain misconfigurations (e.g. stale `TRUSTY_DEVICE=cpu` in
//! `daemon.env` on Apple Silicon) are silent by default — the daemon starts,
//! serves requests, and appears healthy while quietly running at a fraction of
//! its potential throughput. Centralising these checks in one place keeps
//! `start.rs` readable and makes the predicates independently testable.
//!
//! What: each public `should_warn_*` predicate is a pure function of its
//! inputs (no I/O, no env reads) so unit tests can drive it without touching
//! the process environment. Each `warn_if_*` wrapper reads the environment
//! once and delegates to the predicate.
//!
//! Test: `should_warn_cpu_on_apple_silicon_*` tests in this module's `tests`.

// ── Fix D (issue #747) ───────────────────────────────────────────────────────

/// Pure predicate: should the daemon warn about a stale `TRUSTY_DEVICE=cpu`
/// setting on Apple Silicon?
///
/// Why (issue #747 Fix D): `TRUSTY_DEVICE=cpu` was the documented workaround
/// for the macOS jetsam SIGKILL caused by CoreML's unified-memory over-
/// allocation (issue #24). That root cause was resolved in trusty-search
/// 0.3.55 by switching the default CoreML configuration to
/// `MLComputeUnits=CPUAndNeuralEngine`, which uses the Neural Engine's
/// dedicated memory pool instead of the GPU pool. Operators who followed the
/// old workaround and forgot to remove `TRUSTY_DEVICE=cpu` from `daemon.env`
/// are now silently running CPU-only at a fraction of ANE throughput.
///
/// What: returns `true` when `device` resolves to `"cpu"` (case-insensitive)
/// AND `is_apple_silicon` is `true`. The combination is almost always a stale
/// workaround. Returns `false` on non-Apple-Silicon hosts (where `cpu` is the
/// only option and the warning is noise) or when `TRUSTY_DEVICE` is unset /
/// set to `auto` / set to `gpu`.
///
/// Test: `should_warn_cpu_on_apple_silicon_true`,
/// `should_warn_cpu_on_apple_silicon_false_not_apple_silicon`,
/// `should_warn_cpu_on_apple_silicon_false_not_cpu`.
pub fn should_warn_cpu_on_apple_silicon(device: &str, is_apple_silicon: bool) -> bool {
    is_apple_silicon && device.eq_ignore_ascii_case("cpu")
}

/// Emit a `tracing::warn!` when `TRUSTY_DEVICE=cpu` is detected on Apple
/// Silicon (issue #747 Fix D).
///
/// Why: calls `should_warn_cpu_on_apple_silicon` with the live process
/// environment after `load_daemon_env()` has populated `TRUSTY_DEVICE`. This
/// is the only call site that touches the environment; the predicate itself is
/// pure and testable.
///
/// What: reads `TRUSTY_DEVICE` from the environment. On Apple Silicon
/// (`#[cfg(all(target_os = "macos", target_arch = "aarch64"))]`) calls the
/// predicate and, if it returns `true`, emits a one-time `tracing::warn!` on
/// stderr explaining the issue and the fix. Does nothing on non-Apple-Silicon
/// hosts at compile time (zero overhead).
///
/// Test: the predicate is covered by `should_warn_cpu_on_apple_silicon_*`
/// tests. This wrapper's stderr side-effect is intentionally not unit-tested
/// (it is a logging call with no return value).
pub fn warn_if_stale_cpu_device_on_apple_silicon() {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let device = std::env::var("TRUSTY_DEVICE").unwrap_or_default();
        if should_warn_cpu_on_apple_silicon(&device, true) {
            tracing::warn!(
                "TRUSTY_DEVICE=cpu is set on Apple Silicon — this disables CoreML ANE \
                 acceleration and is almost certainly a stale workaround from the resolved \
                 issue #24 (macOS jetsam SIGKILL during indexing). The root cause was fixed \
                 in trusty-search 0.3.55 by switching CoreML to CPUAndNeuralEngine mode, \
                 which avoids the unified-memory spike entirely. Remove TRUSTY_DEVICE=cpu \
                 from your daemon.env to restore ANE throughput (~10x CPU). If you \
                 intentionally want CPU-only mode, set TRUSTY_DEVICE=cpu explicitly in \
                 your shell and this warning can be suppressed with TRUSTY_DEVICE_EXPLICIT=1."
            );
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::should_warn_cpu_on_apple_silicon;

    /// Why: the core case — on Apple Silicon with TRUSTY_DEVICE=cpu the
    /// operator is almost certainly running with a stale workaround; warn.
    /// What: `device="cpu"`, `is_apple_silicon=true` → `true`.
    /// Test: this test.
    #[test]
    fn should_warn_cpu_on_apple_silicon_true() {
        assert!(should_warn_cpu_on_apple_silicon("cpu", true));
        // Case-insensitive.
        assert!(should_warn_cpu_on_apple_silicon("CPU", true));
        assert!(should_warn_cpu_on_apple_silicon("Cpu", true));
    }

    /// Why: on non-Apple-Silicon hosts `TRUSTY_DEVICE=cpu` is the only
    /// available option and the warning would be noise.
    /// What: `is_apple_silicon=false` → always `false`.
    /// Test: this test.
    #[test]
    fn should_warn_cpu_on_apple_silicon_false_not_apple_silicon() {
        assert!(!should_warn_cpu_on_apple_silicon("cpu", false));
        assert!(!should_warn_cpu_on_apple_silicon("CPU", false));
    }

    /// Why: when `TRUSTY_DEVICE` is unset/auto/gpu on Apple Silicon there is
    /// nothing to warn about.
    /// What: `device != "cpu"`, `is_apple_silicon=true` → `false`.
    /// Test: this test.
    #[test]
    fn should_warn_cpu_on_apple_silicon_false_not_cpu() {
        assert!(!should_warn_cpu_on_apple_silicon("", true));
        assert!(!should_warn_cpu_on_apple_silicon("auto", true));
        assert!(!should_warn_cpu_on_apple_silicon("gpu", true));
        assert!(!should_warn_cpu_on_apple_silicon("GPU", true));
    }
}
