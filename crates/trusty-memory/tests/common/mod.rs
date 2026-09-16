//! Shared fixtures for the `trusty-memory` integration tests.
//!
//! Why: each file under `tests/` compiles as its own binary, so a fixture used
//! by more than one of them has to live in a module they all declare. This is
//! that module.
//! What: [`DaemonGuard`], the owning handle for a spawned
//! `trusty-memory serve --foreground`.
//! Test: exercised by every test that spawns a daemon —
//! `serve_stdio_concurrent_e2e`, `serve_stdio_e2e`, `codex_stdio_e2e_5265`;
//! the parent-death guarantee itself by
//! `guard_spawned_daemon_exits_when_its_spawner_is_sigkilled`.

// Each `tests/` file compiles this module into its OWN binary, and no single
// binary uses every fixture in it — `pid` is only wanted by `orphan_reap_7085`.
// Without this the unused half is a `dead_code` warning, which `-D warnings`
// turns into a build failure in the other three.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Owns a spawned `serve --foreground` daemon and kills it on drop.
///
/// Why (#5188): the spawn helpers returned a raw `Child` and relied on an
/// explicit `.kill()` at the end of the test body. Every `assert!` between
/// those two points — including the readiness-poll assert inside the spawn
/// helper itself — orphans the daemon on failure. An orphan re-parents to PID
/// 1, keeps its dream loop running against the temp data dir, and on the
/// reporting machine six of them accumulated from one `cargo test` run and
/// went on calling a local model. `Drop` runs on the panic unwind, so the
/// daemon dies with the test that spawned it.
/// What: kills and reaps in `Drop`. Both results are discarded — the child may
/// already have exited, and a teardown error must not mask the test's own
/// failure.
///
/// #7085: `Drop` is not enough on its own. SIGKILL this test binary — a `cargo
/// test` timeout, an interrupted run, a torn-down process tree — and no
/// destructor runs at all, which is how 102 orphaned daemons accumulated. The
/// daemon therefore also carries
/// `trusty_common::parent_death::exit_with_parent`, so it watches the test
/// process and self-exits on its own. The two are complementary: `Drop` is the
/// immediate reap on the ordinary path, the stamp is the backstop.
///
/// Why [`DaemonGuard::spawn`] and no `new` (#7085 recurrence): the stamp used
/// to be applied by each call site, three of which hand-copied the same
/// `exit_with_parent(...)` block beside a `DaemonGuard::new(child)`. A guard
/// that accepts an already-spawned `Child` cannot tell a stamped one from an
/// unstamped one, so the linkage was a convention a fourth call site — or one
/// edit to an existing one — could drop without any gate noticing. Owning the
/// spawn is what makes the stamp unskippable.
/// Test: `orphan_reap_7085::guard_spawned_daemon_exits_when_its_spawner_is_sigkilled`
/// kills a spawner outright and requires the guard's daemon to follow;
/// `stdio_serve_concurrent_two_bridges_both_work` and the other daemon tests
/// cover the ordinary path.
pub struct DaemonGuard {
    child: Child,
}

impl DaemonGuard {
    /// Spawn `serve --foreground` against `data_dir` under the parent-death
    /// stamp, and own it.
    ///
    /// Why the fixed argument and environment set: all three stdio suites
    /// spawned a byte-identical command, and a per-site copy is what let the
    /// stamp drift. `TRUSTY_DATA_DIR_OVERRIDE` confines every byte of daemon
    /// state to `data_dir`, `TRUSTY_SKIP_PALACE_ENFORCEMENT` keeps a test off
    /// the operator's real palace, and stderr is inherited so a daemon that
    /// fails to boot says why in the test output.
    /// What: stamps the `Command` with
    /// [`trusty_common::parent_death::exit_with_parent`], spawns it, and wraps
    /// the child. Panics on a spawn failure — a test that cannot start its
    /// daemon has nothing left to assert.
    /// Test: as the type.
    pub fn spawn(data_dir: &Path) -> Self {
        let child = trusty_common::parent_death::exit_with_parent(
            Command::new(binary())
                .arg("serve")
                .arg("--foreground")
                .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
                .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
                .env("RUST_LOG", "warn")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit()),
        )
        .spawn()
        .expect("spawn trusty-memory serve --foreground");
        Self { child }
    }

    /// The daemon's pid, for a test that has to observe the process itself.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

/// The socket `serve --foreground` binds under `data_dir` — the readiness
/// signal every caller polls for.
pub fn socket_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("trusty-memory").join("trusty-memory.sock")
}

/// The `trusty-memory` binary Cargo built for this test run.
fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
