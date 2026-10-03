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
//! `tokio::time::timeout`; on expiry it prints a descriptive error (issue
//! #1633 + remediation steps) and exits the process with status 1. A fast
//! `Err` from the wrapped future is propagated unchanged (never mistaken for
//! a timeout).
//!
//! #8616: the timeout arm exits instead of returning. On timeout the
//! still-blocked OS thread inside `spawn_blocking` (`FastEmbedder::with_cache_size`
//! runs the ORT init on a blocking-pool thread) cannot be cancelled, and a
//! tokio runtime's drop waits for its blocking threads — so a returned error
//! left the shim's `#[tokio::main]` process alive forever.
//!
//! Test: `model_init_timeout_default`, `model_init_timeout_reads_env`,
//! `model_init_timeout_ignores_malformed`, `run_bounded_returns_ok_when_fast`,
//! `run_bounded_propagates_fast_error`, `hung_init_error_names_issue_and_knob`,
//! and `run_bounded_timeout_ends_process_despite_stuck_blocking_thread` (all
//! below) — exercise the timeout resolver and the race mechanics against
//! synthetic futures, with no ONNX/ORT runtime involved. The real `FastEmbedder::new()` call site is covered indirectly
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
/// actionable failure instead of blocking the caller forever.
///
/// Why: `TextEmbedding::try_new` (inside `FastEmbedder::new()`) offers no
/// cancellation hook and can deadlock the underlying OS thread indefinitely
/// on AL2023/glibc-2.34 hosts (issue #1633), or inside `ort::api()` after a
/// failed load-dynamic load (#8616). Without a bound, `run_with_args`
/// would await it forever, reporting nothing to logs or `/health` — a silent
/// false-negative that looks identical to "still starting up."
/// What: on success returns `Ok(value)`. A fast `Err` from `fut` is
/// propagated unchanged (annotated with `op_name` context) — that is a real
/// failure, not a hang, and must not be reworded as a timeout. On timeout it
/// never returns: it prints [`hung_init_error`] to stderr and exits the
/// process with status 1 (#8616 — see [`exit_hung`]).
/// Test: `run_bounded_returns_ok_when_fast`,
/// `run_bounded_propagates_fast_error`,
/// `run_bounded_timeout_ends_process_despite_stuck_blocking_thread`.
pub(crate) async fn run_bounded<F, T>(op_name: &str, timeout: Duration, fut: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e).with_context(|| format!("{op_name} failed")),
        Err(_) => exit_hung(&hung_init_error(op_name, timeout)),
    }
}

/// Print `err` to stderr and end the process with status 1.
///
/// Why (#8616): a timed-out init leaves its `spawn_blocking` thread stuck, and
/// dropping a tokio runtime waits for every blocking thread. Returning the
/// error to the shim's `#[tokio::main]` therefore never ended the process.
/// `std::process::exit` does not wait for other threads.
/// What: writes `Error: {err:#}` (the same shape a `main` returning `Err`
/// prints) and calls `std::process::exit(1)`.
/// Test: `run_bounded_timeout_ends_process_despite_stuck_blocking_thread`.
fn exit_hung(err: &anyhow::Error) -> ! {
    tracing::error!("{err:#}");
    eprintln!("Error: {err:#}");
    std::process::exit(1)
}

/// The operator-facing error for an init that outlived its bound.
///
/// Why: an operator needs the issue, the likely trigger, and the knob.
/// What: names issue #1633, the AL2023/glibc trigger, the #8616 load-dynamic
/// trigger, and the remediations: raise `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS`
/// if the host is just slow, or rebuild with load-dynamic + `ORT_DYLIB_PATH`
/// on AL2023 / glibc < 2.38.
/// Test: `hung_init_error_names_issue_and_knob`.
fn hung_init_error(op_name: &str, timeout: Duration) -> anyhow::Error {
    anyhow::anyhow!(
        "{op_name} did not complete within {timeout:?} (issue #1633) — presumed hung. \
         Known triggers: (a) ONNX Runtime CPU(no-arena) execution-provider init deadlocks on \
         Amazon Linux 2023 / glibc 2.34 hosts because the default `bundled-ort` feature \
         links a statically-bundled ONNX Runtime built assuming glibc >= 2.38; (b) on a \
         load-dynamic build, an ONNX Runtime at ORT_DYLIB_PATH that loads but then stalls \
         (#8616). Remediation: (1) if this host legitimately needs more time, raise \
         TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS above {timeout:?}; (2) on AL2023 / older-glibc \
         hosts, reinstall with `--no-default-features --features load-dynamic` and set \
         ORT_DYLIB_PATH to a host-compatible libonnxruntime.so 1.24.x instead of the \
         bundled static ORT — or use the prebuilt `x86_64-linux-al2023` release asset, \
         which already ships that configuration.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_env::env_lock;

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

    /// Why: the timeout error must name the issue and remediation so an
    /// operator isn't left guessing (#1633).
    /// What: builds the error `run_bounded` prints on timeout and checks it.
    /// The timeout arm itself ends the process (#8616), so it is exercised
    /// in a child process by
    /// `run_bounded_timeout_ends_process_despite_stuck_blocking_thread`.
    /// Test: itself.
    #[test]
    fn hung_init_error_names_issue_and_knob() {
        let msg = hung_init_error("test-op", Duration::from_millis(50)).to_string();
        assert!(
            msg.contains("test-op did not complete within 50ms"),
            "error must name the op and the bound: {msg}"
        );
        assert!(
            msg.contains("1633"),
            "error must reference issue #1633: {msg}"
        );
        assert!(
            msg.contains("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS"),
            "error must name the override knob: {msg}"
        );
    }

    /// Marks the re-executed child of
    /// `run_bounded_timeout_ends_process_despite_stuck_blocking_thread`.
    const STUCK_CHILD_ENV: &str = "TRUSTY_EMBEDDERD_8616_STUCK_CHILD";

    /// Why (#8616): a timed-out init leaves its `spawn_blocking` thread stuck
    /// (in `ort::api()` on a failed load-dynamic load). Dropping a tokio
    /// runtime waits for its blocking threads, so returning the timeout error
    /// to a `#[tokio::main]` caller kept the process alive forever.
    /// What: re-executes this test binary as a child that mirrors the shim's
    /// `#[tokio::main]`: it builds a runtime, runs `run_bounded` over a
    /// blocking task that never returns, then drops the runtime. The parent
    /// waits at most `BOUND` and asserts the child ended by itself, non-zero,
    /// with the timeout error on stderr.
    /// Test: itself.
    #[test]
    fn run_bounded_timeout_ends_process_despite_stuck_blocking_thread() {
        if std::env::var_os(STUCK_CHILD_ENV).is_some() {
            let rt = tokio::runtime::Runtime::new().expect("child runtime");
            let result = rt.block_on(run_bounded("stuck-op", Duration::from_millis(200), async {
                tokio::task::spawn_blocking(|| loop {
                    std::thread::park();
                })
                .await?;
                Ok::<_, anyhow::Error>(())
            }));
            eprintln!("child: run_bounded returned {result:?}; dropping runtime");
            drop(rt);
            eprintln!("child: runtime dropped");
            return;
        }

        const BOUND: Duration = Duration::from_secs(30);
        let stderr_file = tempfile::NamedTempFile::new().expect("stderr capture file");
        let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .args([
                "--exact",
                "readiness::tests::run_bounded_timeout_ends_process_despite_stuck_blocking_thread",
                "--nocapture",
            ])
            .env(STUCK_CHILD_ENV, "1")
            .stdout(std::process::Stdio::null())
            .stderr(stderr_file.reopen().expect("reopen stderr capture"))
            .spawn()
            .expect("spawn child test process");

        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll child") {
                break Some(status);
            }
            if started.elapsed() > BOUND {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let stderr = std::fs::read_to_string(stderr_file.path()).unwrap_or_default();
        let status = status.unwrap_or_else(|| {
            panic!(
                "child still alive after {BOUND:?}: a stuck blocking thread kept the process \
                 alive past run_bounded's timeout (#8616). child stderr:\n{stderr}"
            )
        });
        assert!(
            !status.success(),
            "a timed-out init must end the process non-zero, got {status}; stderr:\n{stderr}"
        );
        assert!(
            stderr.contains("did not complete within"),
            "the timeout error must reach stderr before exit; stderr:\n{stderr}"
        );
    }
}
