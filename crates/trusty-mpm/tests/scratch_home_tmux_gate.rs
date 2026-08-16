//! Regression proof for #5784: a daemon under a throwaway `$HOME` spawns no
//! tmux process during startup auto-discovery.
//!
//! Why its own test binary: the proof narrows `$PATH` to a single directory
//! holding a fake `tmux`, and `$PATH` is process-global. Inside the lib test
//! binary that narrowing raced unrelated tests — three `doctor_scaffold_tracking`
//! tests failed with `failed to spawn git: No such file or directory` — and
//! `#[serial_test::serial]` could not help, since it only orders tests that
//! carry the attribute. A dedicated integration binary is its own process, and
//! this file holds exactly ONE test, so nothing else can observe the narrowed
//! `$PATH`.
//!
//! What: writes an executable `tmux` that records every invocation, points
//! `$PATH` at only that directory, reassigns `$HOME` to a scratch dir, and runs
//! the startup pane scan twice — once gated, once with the opt-in.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test scratch_home_tmux_gate`.

#![cfg(feature = "daemon")]

use std::path::{Path, PathBuf};

use trusty_mpm::core::host_state_gate::ALLOW_HOST_STATE_ENV;
use trusty_mpm::daemon::DaemonState;
use trusty_mpm::daemon::discovery::{DiscoveryResult, discover_claude_sessions};

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
///
/// The write handle is closed before the path is returned so no exec races a
/// writable fd — the shape `core::tmux_tests::write_fake_tmux` settled on.
fn write_fake_tmux(dir: &Path, log: &Path) -> PathBuf {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let path = dir.join("tmux");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\necho 'scratch-fake-pane claude'\nexit 0\n",
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

/// A scratch-`$HOME` daemon must spawn no tmux process at all.
///
/// The instrument is an executable named `tmux` on a `$PATH` that contains
/// nothing else, recording every invocation. `$PATH` is narrowed first and the
/// resolution asserted before any scan runs, so this test cannot reach the
/// operator's real tmux server however the gate decides — reaching it is the
/// bug.
///
/// Phase 1 (scratch `$HOME`, no opt-in) must leave the log absent. Phase 2
/// changes one thing, `TRUSTY_MPM_ALLOW_HOST_STATE=1`, and the same call then
/// runs the fake. Phase 2 is what makes phase 1 mean anything: it shows the
/// scan was stopped by the gate and not by a broken fixture. Before this change
/// phase 1 behaved exactly like phase 2 — it adopted `scratch-fake-pane`.
#[test]
fn scratch_home_daemon_does_not_spawn_tmux() {
    let fake_bin = tempfile::tempdir().expect("fake bin dir");
    let scratch_home = tempfile::tempdir().expect("scratch home");
    let invocations = fake_bin.path().join("tmux-invocations.log");
    let fake_tmux = write_fake_tmux(fake_bin.path(), &invocations);

    let _path = EnvOverride::set("PATH", fake_bin.path());
    let _home = EnvOverride::set("HOME", scratch_home.path());
    // Empty, not absent: neutralises any opt-in the ambient environment already
    // carries, so phase 1 tests the gate rather than the shell it ran from.
    let _no_opt_in = EnvOverride::set(ALLOW_HOST_STATE_ENV, "");

    assert_eq!(
        trusty_common::bin_resolve::resolve_binary("tmux").as_deref(),
        Some(fake_tmux.as_path()),
        "the fixture must own tmux resolution, or this test could reach the real server"
    );

    let state = DaemonState::new();

    let blocked = discover_claude_sessions(&state);
    assert_eq!(
        blocked,
        DiscoveryResult::default(),
        "a scratch-$HOME daemon must adopt nothing"
    );
    assert!(
        !invocations.exists(),
        "a scratch-$HOME daemon must not run tmux at all; it ran: {}",
        std::fs::read_to_string(&invocations).unwrap_or_default()
    );

    let _opt_in = EnvOverride::set(ALLOW_HOST_STATE_ENV, "1");
    let _ = discover_claude_sessions(&state);
    let ran = std::fs::read_to_string(&invocations)
        .expect("the opt-in must let the same scan reach tmux");
    assert!(
        ran.contains("list-panes"),
        "the opt-in scan must issue the pane listing; log was: {ran}"
    );
}
