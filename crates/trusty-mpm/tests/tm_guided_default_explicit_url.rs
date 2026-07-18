//! End-to-end regression test for issue #1737's one confirmed LIVE bug: the
//! bare `tm` guided default silently auto-starting/reconnecting to a
//! DIFFERENT daemon when given an unreachable, explicitly-supplied `--url`.
//!
//! Why: `commands::guided::run_guided_default`'s daemon-unreachable branch
//! calls `guided_autostart::ensure_daemon_started`, which intentionally
//! ignores whatever URL it is handed and always re-resolves IMPLICITLY (lock
//! file → default) once it has confirmed *some* daemon is healthy — correct
//! for the ordinary "daemon not started yet" first-run UX (#1705), but it
//! meant an unreachable EXPLICIT `--url`/`TRUSTY_MPM_URL` silently ended up
//! using a completely different, real daemon instead of erroring. This was
//! empirically confirmed on a dev host actually running `trusty-mpm daemon`:
//! `tm --url http://127.0.0.1:1` (no subcommand) printed the REAL daemon's
//! live session list rather than any indication the requested URL failed.
//! `run_guided_default` now probes an explicit URL up front (before any of
//! that fallback-prone logic runs) and errors with exit code 75 instead.
//! What: spawns `tm --url http://127.0.0.1:1` (bare, no subcommand) with
//! stdin closed and asserts exit 75 plus a stderr message naming the failed
//! URL and stating no fallback occurred. Uses `CARGO_BIN_EXE_tm` (set by
//! Cargo for integration tests) so no extra dev-dependency is needed. Runs
//! from a hermetic, non-git temp directory so the test never depends on this
//! repo's own git/GitHub-remote structure (`derive_project` never needs to
//! run — the explicit-URL guard fires before it).
//! Test: this test.

use std::process::Command;

#[test]
fn bare_tm_explicit_unreachable_url_errors_not_silent_fallback() {
    let tmp = tempfile::tempdir().expect("create temp cwd");

    let bin = env!("CARGO_BIN_EXE_tm");
    let output = Command::new(bin)
        .args(["--url", "http://127.0.0.1:1"])
        .current_dir(tmp.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn tm binary");

    assert_eq!(
        output.status.code(),
        Some(75),
        "explicit unreachable --url on the bare guided default must exit 75; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("http://127.0.0.1:1"),
        "stderr must name the explicit unreachable URL; got: {stderr}"
    );
    assert!(
        stderr.contains("refusing to fall back"),
        "stderr must state that no fallback was attempted; got: {stderr}"
    );

    // The whole point of the regression: no session data from a DIFFERENT
    // daemon should ever reach stdout for this invocation.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "no output must be printed when the explicit URL is unreachable \
         (a non-empty stdout would indicate a silent fallback to a \
         different daemon); got: {stdout}"
    );
}
