//! Regression proof for #6348: a daemon isolated by its FRAMEWORK ROOT alone,
//! under the operator's real `$HOME`, spawns no tmux process during startup
//! auto-discovery.
//!
//! Why its own test binary, and why a second one: the proof narrows `$PATH` to a
//! single directory holding a fake `tmux`, and `$PATH` is process-global — the
//! same reason `scratch_home_tmux_gate.rs` is its own binary, and the reason it
//! holds exactly one test. That file cannot host this case: it reassigns `$HOME`
//! to a scratch directory, which is the OTHER quadrant. #6348 is the quadrant
//! where `$HOME` is untouched and only the data root is scratch, so a test that
//! reassigns `$HOME` would pass on the pre-existing `$HOME` arm whether or not
//! the root arm exists.
//!
//! What: writes an executable `tmux` that records every invocation, points
//! `$PATH` at only that directory, pins `$HOME` to the home the password
//! database records for this uid (so the `$HOME` arm provably ALLOWS), builds a
//! `DaemonState` on a temp root, and runs `discover_all` twice — once gated,
//! once with the opt-in.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test scratch_root_tmux_gate`.

#![cfg(all(unix, feature = "daemon"))]

use std::path::{Path, PathBuf};

use trusty_mpm::core::host_state_gate::{ALLOW_HOST_STATE_ENV, host_state_access};
use trusty_mpm::daemon::DaemonState;
use trusty_mpm::daemon::discovery::discover_all;

/// RAII override of one environment variable, restored on drop.
///
/// Restoring on drop keeps a mid-test assertion failure from leaving the
/// narrowed `$PATH` behind for the harness's own teardown.
struct EnvOverride {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvOverride {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: this binary holds exactly one test, so no other thread reads
        // or writes the environment while this runs.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        // SAFETY: as in `set` — single-test binary.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// Write an executable `tmux` into `dir` that appends its argv to `log` and
/// prints one Claude-hosting pane row.
fn write_fake_tmux(dir: &Path, log: &Path) -> PathBuf {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let path = dir.join("tmux");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\necho 'scratch-root-fake-pane claude'\nexit 0\n",
        log = log.display()
    );
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o755)
        .open(&path)
        .expect("create fake tmux");
    f.write_all(body.as_bytes()).expect("write fake tmux");
    drop(f);
    path
}

/// The home the OS password database records for this uid.
///
/// Pinning `$HOME` to this value is what makes the fixture deterministic: the
/// `$HOME` arm of the gate then ALLOWS by construction, on any host, so the only
/// thing left that can refuse is the data root.
fn passwd_home() -> PathBuf {
    nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .expect("password-database lookup")
        .expect("a passwd entry for the current uid")
        .dir
}

/// A daemon on a scratch framework root must spawn no tmux, even under a real
/// `$HOME`.
///
/// The instrument is an executable named `tmux` on a `$PATH` that contains
/// nothing else, recording every invocation. `$PATH` is narrowed first and the
/// resolution asserted before any scan runs, so this test cannot reach the
/// operator's real tmux server however the gate decides — reaching it is the
/// bug.
///
/// Phase 1 asserts the FAIL-OPEN first: the `$HOME`-only gate allows this
/// environment. That is the precondition that makes the rest mean anything —
/// without it a refusal could be the pre-existing `$HOME` arm rather than the
/// root arm #6348 added. Phase 2 changes one thing,
/// `TRUSTY_MPM_ALLOW_HOST_STATE=1`, and the same call then runs the fake, which
/// shows the scan was stopped by the gate and not by a broken fixture.
#[test]
fn scratch_root_daemon_does_not_spawn_tmux() {
    let fake_bin = tempfile::tempdir().expect("fake bin dir");
    let scratch_root = tempfile::tempdir().expect("scratch framework root");
    let invocations = fake_bin.path().join("tmux-invocations.log");
    let fake_tmux = write_fake_tmux(fake_bin.path(), &invocations);

    let _path = EnvOverride::set("PATH", fake_bin.path());
    // The operator's REAL home, not a scratch one — that is the whole point.
    let _home = EnvOverride::set("HOME", passwd_home());
    // Empty, not absent: neutralises any opt-in the ambient environment already
    // carries, so phase 1 tests the gate rather than the shell it ran from.
    let _no_opt_in = EnvOverride::set(ALLOW_HOST_STATE_ENV, "");

    assert_eq!(
        trusty_common::bin_resolve::resolve_binary("tmux").as_deref(),
        Some(fake_tmux.as_path()),
        "the fixture must own tmux resolution, or this test could reach the real server"
    );
    assert!(
        host_state_access().is_allowed(),
        "the $HOME-only gate must ALLOW here — that is the #6348 fail-open this test closes"
    );

    let state = DaemonState::with_root(scratch_root.path().to_path_buf());

    let blocked = tokio_block_on(discover_all(&state));
    assert_eq!(
        blocked.adopted, 0,
        "a scratch-root daemon must adopt nothing"
    );
    let reason = blocked
        .skipped
        .expect("a refused scan must be distinguishable from one that found nothing");
    assert!(
        reason.contains(ALLOW_HOST_STATE_ENV),
        "the refusal must name the hatch; got: {reason}"
    );
    assert!(
        reason.contains(&scratch_root.path().display().to_string()),
        "the refusal must be the DATA-ROOT arm, naming {}; got: {reason}",
        scratch_root.path().display()
    );
    assert!(
        !invocations.exists(),
        "a scratch-root daemon must not run tmux at all; it ran: {}",
        std::fs::read_to_string(&invocations).unwrap_or_default()
    );

    let _opt_in = EnvOverride::set(ALLOW_HOST_STATE_ENV, "1");
    let _ = tokio_block_on(discover_all(&state));
    let ran = std::fs::read_to_string(&invocations)
        .expect("the opt-in must let the same scan reach tmux");
    assert!(
        ran.contains("list-panes"),
        "the opt-in scan must issue the pane listing; log was: {ran}"
    );
}

/// Drive one future to completion without pulling `#[tokio::test]` into a test
/// whose whole point is controlling process-global environment on one thread.
fn tokio_block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
        .block_on(fut)
}
