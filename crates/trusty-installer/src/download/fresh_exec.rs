//! Execute a binary this process has just written, surviving the ETXTBSY fork
//! race (#6231).
//!
//! Why: the installer writes a binary (tar extraction, placement copy) and then
//! runs it with `--version`. Rust opens every file `O_CLOEXEC`, but another
//! thread that forks between our `open` and our `close` hands the child a copy
//! of the write fd, and that copy lives until the child execs. For that window
//! Linux refuses to exec the file with ETXTBSY ("Text file busy", os error 26).
//! `O_CLOEXEC` cannot close the window, and the installer cannot stop other
//! threads from forking, so the exec has to wait the window out. Only the FIRST
//! exec after a write is exposed: once one exec succeeds no process held a write
//! fd at that instant, and a later fork cannot inherit a fd the parent closed.
//!
//! What: [`retry_on_etxtbsy`] re-runs a spawn on ETXTBSY only, on a fixed,
//! bounded backoff, and returns the last error unchanged when the bound runs
//! out. [`probe_fresh_binary`] is the `--version` probe the installer runs on
//! a binary it just placed, with its spawn behind that retry.
//!
//! Test: `tests::etxtbsy_is_retried_until_the_spawn_succeeds`,
//! `tests::a_non_etxtbsy_spawn_error_is_not_retried`,
//! `tests::persistent_etxtbsy_fails_closed_with_the_original_error`,
//! `tests::probe_retries_etxtbsy_and_reports_the_version_line`.

use std::future::Future;
use std::io;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

/// Sleeps between spawn attempts on ETXTBSY: 8 attempts, 635 ms of sleep in
/// total. A leaked fd lives only until the forked child execs; a Linux
/// reproduction recovered on the first 5 ms retry every time.
const ETXTBSY_BACKOFF_MS: [u64; 7] = [5, 10, 20, 40, 80, 160, 320];

/// Same bound as `trusty_common::update::verify_installed_binary_at_path`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// True only for ETXTBSY. Non-unix platforms have no such exec error.
fn is_etxtbsy(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}

/// Run `spawn`, re-running it while it fails with ETXTBSY, within a fixed bound.
///
/// Why: see the module doc — a freshly written binary can be briefly
/// un-execable because a concurrently forked child still holds our write fd.
/// Go's toolchain retries the same errno for the same reason.
///
/// What: awaits `spawn()`. An ETXTBSY error (raw os error 26) sleeps the next
/// [`ETXTBSY_BACKOFF_MS`] step and tries again; any other result, success or
/// error, returns at once. When the steps run out, the last ETXTBSY error is
/// returned as-is, never wrapped and never turned into a success.
///
/// Test: `tests::etxtbsy_is_retried_until_the_spawn_succeeds`,
/// `tests::a_non_etxtbsy_spawn_error_is_not_retried`,
/// `tests::persistent_etxtbsy_fails_closed_with_the_original_error`.
pub(crate) async fn retry_on_etxtbsy<T, F, Fut>(mut spawn: F) -> io::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let mut delays = ETXTBSY_BACKOFF_MS.iter();
    loop {
        match spawn().await {
            Err(e) if is_etxtbsy(&e) => {
                let Some(ms) = delays.next() else {
                    return Err(e);
                };
                tracing::debug!(error = %e, retry_in_ms = ms, "exec hit ETXTBSY; retrying (#6231)");
                tokio::time::sleep(Duration::from_millis(*ms)).await;
            }
            other => return other,
        }
    }
}

/// Probe a binary this process just wrote with `--version`, retrying ETXTBSY.
///
/// Why: `trusty_common::update::verify_installed_binary_at_path` spawns once,
/// so a leaked write fd (#6231) failed a correct install with "Text file busy".
///
/// What: the same contract and messages as that function — `Err` when the path
/// is missing, on a non-zero exit, on a spawn failure, or after 10 s — but the
/// spawn goes through [`retry_on_etxtbsy`]. Returns the trimmed stdout line.
///
/// Test: `tests::probe_retries_etxtbsy_and_reports_the_version_line`; the real
/// spawn runs in `pinned::tests::documentation_files_are_not_installed`.
pub(crate) async fn probe_fresh_binary(bin_path: &Path) -> anyhow::Result<String> {
    probe_fresh_binary_with(bin_path, || async move {
        tokio::process::Command::new(bin_path)
            .arg("--version")
            .output()
            .await
    })
    .await
}

/// [`probe_fresh_binary`] with the spawn injected, so tests can script errors.
async fn probe_fresh_binary_with<F, Fut>(bin_path: &Path, spawn: F) -> anyhow::Result<String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = io::Result<Output>>,
{
    if !bin_path.exists() {
        anyhow::bail!(
            "binary not found at expected install path {}",
            bin_path.display()
        );
    }
    match tokio::time::timeout(PROBE_TIMEOUT, retry_on_etxtbsy(spawn)).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }
        Ok(Ok(output)) => Err(anyhow::anyhow!(
            "`{} --version` exited with status {} — new binary may be broken",
            bin_path.display(),
            output.status
        )),
        Ok(Err(e)) => Err(anyhow::anyhow!(
            "failed to spawn `{} --version`: {e}",
            bin_path.display()
        )),
        Err(_) => Err(anyhow::anyhow!(
            "`{} --version` timed out after 10 s — new binary may be hung",
            bin_path.display()
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt;

    fn etxtbsy() -> io::Error {
        io::Error::from_raw_os_error(libc::ETXTBSY)
    }

    fn version_output(line: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: line.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    /// One ETXTBSY, then success: the retry returns the success.
    #[tokio::test(start_paused = true)]
    async fn etxtbsy_is_retried_until_the_spawn_succeeds() {
        let calls = Cell::new(0u32);
        let got = retry_on_etxtbsy(|| {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move {
                if n == 1 {
                    Err(etxtbsy())
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(got.expect("the second attempt succeeds"), 2);
        assert_eq!(calls.get(), 2, "exactly one retry");
    }

    /// Any other spawn error returns at once, untouched.
    #[tokio::test(start_paused = true)]
    async fn a_non_etxtbsy_spawn_error_is_not_retried() {
        for errno in [libc::ENOENT, libc::EACCES, libc::ENOEXEC] {
            let calls = Cell::new(0u32);
            let got: io::Result<()> = retry_on_etxtbsy(|| {
                calls.set(calls.get() + 1);
                async move { Err(io::Error::from_raw_os_error(errno)) }
            })
            .await;
            assert_eq!(got.expect_err("must fail").raw_os_error(), Some(errno));
            assert_eq!(calls.get(), 1, "errno {errno} must not be retried");
        }
    }

    /// ETXTBSY that never clears: 8 attempts, 635 ms, then the original errno —
    /// and the probe built on it fails closed rather than reporting a version.
    #[tokio::test(start_paused = true)]
    async fn persistent_etxtbsy_fails_closed_with_the_original_error() {
        let calls = Cell::new(0u32);
        let start = tokio::time::Instant::now();
        let got: io::Result<()> = retry_on_etxtbsy(|| {
            calls.set(calls.get() + 1);
            async { Err(etxtbsy()) }
        })
        .await;
        assert_eq!(
            got.expect_err("must fail").raw_os_error(),
            Some(libc::ETXTBSY)
        );
        assert_eq!(calls.get(), 8);
        let slept = start.elapsed();
        assert_eq!(slept, Duration::from_millis(635));
        assert!(slept < Duration::from_secs(1), "bounded by ~1 s");

        let tmp = tempfile::tempdir().expect("tempdir");
        let bin = tmp.path().join("tool-a");
        std::fs::write(&bin, b"").expect("write stub");
        let err = probe_fresh_binary_with(&bin, || async { Err(etxtbsy()) })
            .await
            .expect_err("an exhausted retry must not pass the probe");
        let msg = err.to_string();
        assert!(msg.starts_with("failed to spawn"), "{msg}");
        assert!(msg.contains("os error 26"), "original errno kept: {msg}");
    }

    /// The probe rides out one ETXTBSY and reads the version line.
    #[tokio::test(start_paused = true)]
    async fn probe_retries_etxtbsy_and_reports_the_version_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin = tmp.path().join("tool-a");
        std::fs::write(&bin, b"").expect("write stub");
        let calls = Cell::new(0u32);
        let line = probe_fresh_binary_with(&bin, || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move {
                if n == 1 {
                    Err(etxtbsy())
                } else {
                    Ok(version_output("tool-a 1.2.3\n"))
                }
            }
        })
        .await
        .expect("probe passes after the retry");
        assert_eq!(line, "tool-a 1.2.3");
        assert_eq!(calls.get(), 2);
    }
}
