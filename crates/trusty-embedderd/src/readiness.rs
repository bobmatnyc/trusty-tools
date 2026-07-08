//! Bounded model-init guard — fail loud instead of hanging (issue #1633).
//!
//! Why: the published `trusty-embedderd` binary calls `FastEmbedder::new()`
//! with no bound at all. On Amazon Linux 2023 / glibc 2.34 hosts the ORT
//! CPU(no-arena) execution-provider init deadlocks in `futex_wait_queue`
//! *indefinitely* (observed 12+ consecutive spawns over several hours, 0% CPU,
//! no error, no log line past "registering CPU(no-arena) execution
//! provider"). The likely trigger is a toolchain/glibc mismatch: the default
//! `embedder-bundled-ort` feature links a statically-bundled ONNX Runtime
//! built assuming glibc >= 2.38 (see `trusty-common/Cargo.toml`), while AL2023
//! ships glibc 2.34. Because `run_with_args` awaits the model load *before*
//! binding any HTTP/stdio/UDS listener, a hang here means the process never
//! answers anything — indistinguishable from a slow-but-working cold start
//! until an operator notices no embeddings are ever produced (silent
//! semantic-search degradation to lexical-only).
//!
//! What: `model_init_timeout()` resolves a bounded ceiling from
//! `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (default 180 s — the same env var name
//! and default already established by
//! `trusty_common::memory_core::timeouts::embedder_init_timeout` for the
//! memory-core embedder singleton, so operators only need to learn one knob).
//! `run_bounded` races an arbitrary init future against that timeout via
//! `tokio::time::timeout`; on expiry it returns a descriptive `anyhow::Error`
//! (issue #1633 + remediation steps) instead of hanging forever, so the
//! caller can `bail!` out of `run_with_args` and the process exits loudly
//! with a nonzero code and an actionable stderr message. A fast `Err` from
//! the wrapped future is propagated unchanged (never mistaken for a timeout).
//!
//! Note: on timeout the still-blocked OS thread inside `spawn_blocking`
//! (`FastEmbedder::with_cache_size` runs the ORT init on a blocking-pool
//! thread) is abandoned, exactly like the existing CoreML-hang bound in
//! `trusty_common::embedder::fast_embedder::try_new_bounded` (issue #2111).
//! This is acceptable here because the *process itself* exits immediately
//! after — there is no long-lived caller left to leak threads under repeated
//! retries.
//!
//! Test: `model_init_timeout_default`, `model_init_timeout_reads_env`,
//! `model_init_timeout_ignores_malformed`, `run_bounded_returns_ok_when_fast`,
//! `run_bounded_propagates_fast_error`, `run_bounded_times_out_when_slow` (all
//! below) — exercise the timeout resolver and the race mechanics against
//! synthetic futures (`tokio::time::sleep`), with no ONNX/ORT runtime
//! involved. The real `FastEmbedder::new()` call site is covered indirectly
//! by the `embedder_supervisor_e2e` integration tests in `trusty-search`.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};

/// Default ceiling (seconds) for the one-shot model-init call in
/// `run_with_args` before it is treated as hung.
///
/// Why: mirrors `trusty_common::memory_core::timeouts::embedder_init_timeout`
/// (180 s) — cold ONNX model downloads plus a legitimately slow (but
/// eventually successful) CoreML/CPU cold-compile can take up to a couple of
/// minutes; 180 s gives generous headroom without risking an indefinite hang
/// on the AL2023 futex deadlock (issue #1633).
pub(crate) const DEFAULT_MODEL_INIT_TIMEOUT_SECS: u64 = 180;

/// Resolve the bounded model-init timeout from the environment.
///
/// Why: operators on a host with a legitimately slow cold start (large model
/// download over a slow link, busy CI runner) need to be able to raise the
/// bound; operators who want faster failure detection can lower it.
/// What: reads `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (positive integer
/// seconds); falls back to [`DEFAULT_MODEL_INIT_TIMEOUT_SECS`] (180) when
/// unset, non-numeric, or non-positive.
/// Test: `model_init_timeout_default`, `model_init_timeout_reads_env`,
/// `model_init_timeout_ignores_malformed`.
pub(crate) fn model_init_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MODEL_INIT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Race `fut` against `timeout`, turning an unbounded hang into a loud,
/// actionable error instead of blocking the caller forever.
///
/// Why: `TextEmbedding::try_new` (inside `FastEmbedder::new()`) offers no
/// cancellation hook and can deadlock the underlying OS thread indefinitely
/// on AL2023/glibc-2.34 hosts (issue #1633). Without a bound, `run_with_args`
/// would await it forever, reporting nothing to logs or `/health` — a silent
/// false-negative that looks identical to "still starting up."
/// What: on success returns `Ok(value)`. A fast `Err` from `fut` is
/// propagated unchanged (annotated with `op_name` context) — that is a real
/// failure, not a hang, and must not be reworded as a timeout. On timeout
/// returns a descriptive `anyhow::Error` naming issue #1633, the known
/// AL2023/glibc trigger, and two remediations: raise
/// `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` if the host is just slow, or rebuild
/// with `--features embedder-load-dynamic` + `ORT_DYLIB_PATH` if the host is
/// on AL2023 / glibc < 2.38.
/// Test: `run_bounded_returns_ok_when_fast`,
/// `run_bounded_propagates_fast_error`, `run_bounded_times_out_when_slow`.
pub(crate) async fn run_bounded<F, T>(op_name: &str, timeout: Duration, fut: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e).with_context(|| format!("{op_name} failed")),
        Err(_) => Err(anyhow::anyhow!(
            "{op_name} did not complete within {timeout:?} (issue #1633) — presumed hung. \
             Known trigger: ONNX Runtime CPU(no-arena) execution-provider init deadlocks on \
             Amazon Linux 2023 / glibc 2.34 hosts because the default `embedder-bundled-ort` \
             feature links a statically-bundled ONNX Runtime built assuming glibc >= 2.38. \
             Remediation: (1) if this host legitimately needs more time, raise \
             TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS above {timeout:?}; (2) on AL2023 / older-glibc \
             hosts, reinstall with `--no-default-features --features \
             http-server,embedder-load-dynamic` and set ORT_DYLIB_PATH to a host-compatible \
             libonnxruntime.so (e.g. an Ubuntu 20.04 / glibc 2.31 build) instead of the \
             bundled static ORT — or use the prebuilt `x86_64-linux-al2023` release asset, \
             which already ships that configuration.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-global lock guarding every test that mutates
    /// `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` — env vars are process-global and
    /// tests run in parallel by default.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        ENV_MUTEX
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env_lock mutex poisoned")
    }

    /// Why: guard that the default is 180 s when the env var is absent.
    /// What: clears the var, asserts the default is returned.
    /// Test: itself.
    #[test]
    fn model_init_timeout_default() {
        let _guard = env_lock();
        // SAFETY: serialised under `env_lock()`; no other thread mutates
        // this var while the guard is held.
        unsafe { std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS") };
        assert_eq!(
            model_init_timeout(),
            Duration::from_secs(DEFAULT_MODEL_INIT_TIMEOUT_SECS)
        );
        assert_eq!(DEFAULT_MODEL_INIT_TIMEOUT_SECS, 180);
    }

    /// Why: operators must be able to raise or lower the bound without a
    /// code change.
    /// What: sets an explicit value, asserts it is honoured.
    /// Test: itself.
    #[test]
    fn model_init_timeout_reads_env() {
        let _guard = env_lock();
        // SAFETY: serialised under `env_lock()`.
        unsafe { std::env::set_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS", "45") };
        assert_eq!(model_init_timeout(), Duration::from_secs(45));
        // SAFETY: serialised under `env_lock()`.
        unsafe { std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS") };
    }

    /// Why: a malformed or zero override must never panic or silently
    /// disable the bound (which would reintroduce the unbounded hang).
    /// What: feeds non-numeric and zero values, asserts the default wins.
    /// Test: itself.
    #[test]
    fn model_init_timeout_ignores_malformed() {
        let _guard = env_lock();
        for bad in ["not-a-number", "0", ""] {
            // SAFETY: serialised under `env_lock()`.
            unsafe { std::env::set_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS", bad) };
            assert_eq!(
                model_init_timeout(),
                Duration::from_secs(DEFAULT_MODEL_INIT_TIMEOUT_SECS),
                "malformed value {bad:?} must fall back to the default"
            );
        }
        // SAFETY: serialised under `env_lock()`.
        unsafe { std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS") };
    }

    /// Why: the common case (init completes well inside the bound) must
    /// succeed and return the wrapped value unchanged.
    /// What: races a near-instant future against a generous timeout.
    /// Test: itself.
    #[tokio::test]
    async fn run_bounded_returns_ok_when_fast() {
        let result = run_bounded("test-op", Duration::from_secs(5), async {
            Ok::<_, anyhow::Error>(42)
        })
        .await;
        assert_eq!(result.unwrap(), 42);
    }

    /// Why: a fast, real failure (e.g. a malformed model file) must be
    /// propagated as-is — it must never be reworded as a timeout, which
    /// would mislead an operator into raising a timeout knob that cannot
    /// fix the actual problem.
    /// What: races a future that resolves immediately to `Err`.
    /// Test: itself.
    #[tokio::test]
    async fn run_bounded_propagates_fast_error() {
        let result = run_bounded("test-op", Duration::from_secs(5), async {
            Err::<i32, _>(anyhow::anyhow!("boom"))
        })
        .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("test-op failed"));
        assert!(format!("{err:#}").contains("boom"));
    }

    /// Why: the core fix for issue #1633 — a hung future must fail loudly
    /// within the bound instead of hanging forever, and the error must name
    /// the issue and remediation so an operator isn't left guessing.
    /// What: races a future that never resolves within the test's lifetime
    /// (a long sleep) against a short timeout; asserts the timeout fires and
    /// the message is actionable.
    /// Test: itself.
    #[tokio::test]
    async fn run_bounded_times_out_when_slow() {
        let result = run_bounded("test-op", Duration::from_millis(50), async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok::<_, anyhow::Error>(())
        })
        .await;
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("1633"),
            "error must reference issue #1633: {msg}"
        );
        assert!(
            msg.contains("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS"),
            "error must name the override knob: {msg}"
        );
    }
}
