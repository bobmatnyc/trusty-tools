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
/// are now silently running CPU-only at a fraction of ANE throughput. The
/// `explicit` parameter lets operators suppress the warning when they
/// intentionally want CPU-only mode (set `TRUSTY_DEVICE_EXPLICIT=1`).
///
/// What: returns `true` when `device` resolves to `"cpu"` (case-insensitive)
/// AND `is_apple_silicon` is `true` AND `explicit` is `false`. The combination
/// of cpu+apple-silicon is almost always a stale workaround. Returns `false`
/// on non-Apple-Silicon hosts (where `cpu` is the only option and the warning
/// is noise), when `TRUSTY_DEVICE` is unset / set to `auto` / set to `gpu`,
/// or when `explicit` is `true` (operator opted in via `TRUSTY_DEVICE_EXPLICIT=1`).
///
/// Test: `should_warn_cpu_on_apple_silicon_true`,
/// `should_warn_cpu_on_apple_silicon_false_not_apple_silicon`,
/// `should_warn_cpu_on_apple_silicon_false_not_cpu`,
/// `should_warn_cpu_on_apple_silicon_false_explicit_set`.
pub fn should_warn_cpu_on_apple_silicon(
    device: &str,
    is_apple_silicon: bool,
    explicit: bool,
) -> bool {
    !explicit && is_apple_silicon && device.eq_ignore_ascii_case("cpu")
}

/// Return `true` when `TRUSTY_DEVICE_EXPLICIT` is set to a truthy value.
///
/// Why: lets `warn_if_stale_cpu_device_on_apple_silicon` honour the
/// suppression promise in the warning text without exposing env reads inside
/// the pure predicate (keeping it testable).
/// What: reads `TRUSTY_DEVICE_EXPLICIT` from the environment. Returns `true`
/// for `"1"`, `"true"`, or `"yes"` (case-insensitive); `false` for any other
/// value or when the variable is absent.
/// Test: `device_explicit_flag_parses_truthy` and
/// `device_explicit_flag_parses_falsy` in the `tests` module.
pub fn is_device_explicit() -> bool {
    matches!(
        std::env::var("TRUSTY_DEVICE_EXPLICIT")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Emit a `tracing::warn!` when `TRUSTY_DEVICE=cpu` is detected on Apple
/// Silicon and `TRUSTY_DEVICE_EXPLICIT` is not set (issue #747 Fix D).
///
/// Why: calls `should_warn_cpu_on_apple_silicon` with the live process
/// environment after `load_daemon_env()` has populated `TRUSTY_DEVICE`. This
/// is the only call site that touches the environment; the predicate itself is
/// pure and testable. The `TRUSTY_DEVICE_EXPLICIT=1` suppression promise in
/// the warning text is now honoured by checking `is_device_explicit()` before
/// evaluating the predicate.
///
/// What: reads `TRUSTY_DEVICE` and `TRUSTY_DEVICE_EXPLICIT` from the
/// environment. On Apple Silicon
/// (`#[cfg(all(target_os = "macos", target_arch = "aarch64"))]`) calls the
/// predicate and, if it returns `true`, emits a one-time `tracing::warn!` on
/// stderr explaining the issue and the fix. Does nothing on non-Apple-Silicon
/// hosts at compile time (zero overhead). Suppressed entirely when
/// `TRUSTY_DEVICE_EXPLICIT` is truthy.
///
/// Test: the predicate is covered by `should_warn_cpu_on_apple_silicon_*`
/// tests (including the explicit-suppression case). This wrapper's stderr
/// side-effect is intentionally not unit-tested (logging call, no return value).
pub fn warn_if_stale_cpu_device_on_apple_silicon() {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let device = std::env::var("TRUSTY_DEVICE").unwrap_or_default();
        let explicit = is_device_explicit();
        if should_warn_cpu_on_apple_silicon(&device, true, explicit) {
            tracing::warn!(
                "TRUSTY_DEVICE=cpu is set on Apple Silicon — this disables CoreML ANE \
                 acceleration and is almost certainly a stale workaround from the resolved \
                 issue #24 (macOS jetsam SIGKILL during indexing). The root cause was fixed \
                 in trusty-search 0.3.55 by switching CoreML to CPUAndNeuralEngine mode, \
                 which avoids the unified-memory spike entirely. Remove TRUSTY_DEVICE=cpu \
                 from your daemon.env to restore ANE throughput (~10x CPU). If you \
                 intentionally want CPU-only mode, set TRUSTY_DEVICE_EXPLICIT=1 to suppress \
                 this warning."
            );
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{is_device_explicit, should_warn_cpu_on_apple_silicon};

    /// Why: the core case — on Apple Silicon with TRUSTY_DEVICE=cpu the
    /// operator is almost certainly running with a stale workaround; warn.
    /// What: `device="cpu"`, `is_apple_silicon=true`, `explicit=false` → `true`.
    /// Test: this test.
    #[test]
    fn should_warn_cpu_on_apple_silicon_true() {
        assert!(should_warn_cpu_on_apple_silicon("cpu", true, false));
        // Case-insensitive.
        assert!(should_warn_cpu_on_apple_silicon("CPU", true, false));
        assert!(should_warn_cpu_on_apple_silicon("Cpu", true, false));
    }

    /// Why: on non-Apple-Silicon hosts `TRUSTY_DEVICE=cpu` is the only
    /// available option and the warning would be noise.
    /// What: `is_apple_silicon=false` → always `false`.
    /// Test: this test.
    #[test]
    fn should_warn_cpu_on_apple_silicon_false_not_apple_silicon() {
        assert!(!should_warn_cpu_on_apple_silicon("cpu", false, false));
        assert!(!should_warn_cpu_on_apple_silicon("CPU", false, false));
    }

    /// Why: when `TRUSTY_DEVICE` is unset/auto/gpu on Apple Silicon there is
    /// nothing to warn about.
    /// What: `device != "cpu"`, `is_apple_silicon=true` → `false`.
    /// Test: this test.
    #[test]
    fn should_warn_cpu_on_apple_silicon_false_not_cpu() {
        assert!(!should_warn_cpu_on_apple_silicon("", true, false));
        assert!(!should_warn_cpu_on_apple_silicon("auto", true, false));
        assert!(!should_warn_cpu_on_apple_silicon("gpu", true, false));
        assert!(!should_warn_cpu_on_apple_silicon("GPU", true, false));
    }

    /// Why: when `TRUSTY_DEVICE_EXPLICIT=1` is set the operator has
    /// intentionally chosen CPU-only mode — the warning is unwanted noise and
    /// the warning text promises suppression via that variable.
    /// What: `explicit=true` → `false` regardless of device or platform.
    /// Test: this test.
    #[test]
    fn should_warn_cpu_on_apple_silicon_false_explicit_set() {
        // explicit=true suppresses the warning unconditionally.
        assert!(!should_warn_cpu_on_apple_silicon("cpu", true, true));
        assert!(!should_warn_cpu_on_apple_silicon("CPU", true, true));
        // Also suppressed on non-apple-silicon (already false there, but be explicit).
        assert!(!should_warn_cpu_on_apple_silicon("cpu", false, true));
    }

    /// Why: `is_device_explicit()` must parse the well-known truthy values
    /// that the warning text advertises.
    /// What: sets `TRUSTY_DEVICE_EXPLICIT` to `"1"`, `"true"`, `"yes"` and
    /// asserts `true`; sets it to `"0"`, `"false"`, `"no"`, and empty and
    /// asserts `false`.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn device_explicit_flag_parses_truthy() {
        for val in &["1", "true", "TRUE", "True", "yes", "YES"] {
            // SAFETY: test-only, single-threaded.
            unsafe { std::env::set_var("TRUSTY_DEVICE_EXPLICIT", val) };
            assert!(
                is_device_explicit(),
                "TRUSTY_DEVICE_EXPLICIT={val} must be truthy"
            );
        }
        unsafe { std::env::remove_var("TRUSTY_DEVICE_EXPLICIT") };
    }

    /// Why: values other than the advertised truthy set must NOT suppress the
    /// warning — otherwise an accidental `TRUSTY_DEVICE_EXPLICIT=0` would
    /// silently suppress it.
    /// What: sets the var to falsy / empty values and asserts `false`.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn device_explicit_flag_parses_falsy() {
        for val in &["0", "false", "no", "off", ""] {
            unsafe { std::env::set_var("TRUSTY_DEVICE_EXPLICIT", val) };
            assert!(
                !is_device_explicit(),
                "TRUSTY_DEVICE_EXPLICIT={val} must be falsy"
            );
        }
        unsafe { std::env::remove_var("TRUSTY_DEVICE_EXPLICIT") };
        // Absent var must also be falsy.
        assert!(
            !is_device_explicit(),
            "absent TRUSTY_DEVICE_EXPLICIT must be falsy"
        );
    }
}
