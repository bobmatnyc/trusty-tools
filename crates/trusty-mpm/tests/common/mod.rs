//! Per-process `$HOME` isolation for trusty-mpm's integration targets (#6671).
//!
//! Why: `DaemonState::project_registry()` seeds itself from
//! `TrustyToolsConfig::load()`, which resolves
//! `~/.trusty-tools/trusty-mpm/config.yaml` through `dirs::home_dir()` — i.e.
//! `$HOME`. A target that isolates only the daemon's framework root therefore
//! still reads the DEVELOPER's registered projects, so assertions on portfolio
//! counts describe that machine rather than the fixture (#6671 observed
//! `left Number(22) right 0`). #4120 established the per-test `$HOME` redirect;
//! these five targets never adopted it.
//!
//! What: [`scratch_home`] points the whole test PROCESS at a fresh scratch
//! directory exactly once, inside a [`std::sync::OnceLock`] initialiser. A test
//! that calls it either performs the redirect or blocks until another thread's
//! redirect is visible, so no test can observe the real `$HOME`. Exactly one
//! value is ever written, so — unlike the save-and-restore `HomeGuard` pattern
//! used elsewhere in this suite — there is no window in which one test's
//! restore un-isolates another test's read.
//!
//! Test: every test in `manager_routes`, `project_registry_routes`,
//! `manager_cli_client`, `manager_inference` and `mcp_spawn_gate`. Each now
//! asserts against a registry the host machine cannot reach.
//!
//! #7568 adds the second half of the same concern: a target that spawns the
//! built `tm` binary hands the CHILD an environment, and a child that inherits
//! `$HOME` resolves the operator's own `~/.trusty-mpm`. [`tm_command`],
//! [`tm_command_in`] and [`isolate_spawned_tm`] are the one seam every such
//! spawn goes through; `tests/spawned_tm_home_isolation.rs` is the ratchet that
//! keeps them the only one.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Redirect this test process's `$HOME` to a scratch directory, once (#6671).
///
/// Returns the scratch home, so a caller may plant fixtures under it.
///
/// The directory is deliberately `keep()`-ed rather than dropped: it must
/// outlive every test in the process, and a `static` is never dropped anyway.
/// It carries the crate's `tm-test-` prefix under `/tmp`, so
/// `test_support::sweep_stale_test_dirs` — which runs in the lib target on
/// every `cargo test -p trusty-mpm` — reaps it after a day.
pub fn scratch_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("tm-test-home-")
            .tempdir_in("/tmp")
            .expect("create scratch $HOME")
            .keep();
        // SAFETY: this runs inside `OnceLock::get_or_init`, so exactly one
        // thread ever writes `HOME` in this process and every other thread is
        // blocked until that write is visible. Nothing else in these targets
        // mutates `HOME`.
        unsafe { std::env::set_var("HOME", &dir) };
        dir
    })
    .as_path()
}

/// The `tm` binary cargo built for this integration target (#7568).
///
/// Why: this is the ONE place `CARGO_BIN_EXE_tm` is named. Every other spawn
/// site reaches the binary through [`tm_command`] / [`tm_command_in`], which
/// also isolate the child; the two targets that need the raw PATH (a `sh -c`
/// pipeline, a `PATH` shim) are ratcheted by
/// `tests/spawned_tm_home_isolation.rs` and isolate the outer process
/// themselves with [`isolate_spawned_tm`].
pub fn tm_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tm")
}

/// Variables that would point a spawned `tm` back at the operator's own state.
///
/// Why (#7568): redirecting `$HOME` alone is not enough. `TRUSTY_MPM_ROOT` and
/// the XDG config file both outrank the home-relative default in the framework
/// root chain (`--root` > `TRUSTY_MPM_ROOT` > XDG config > `~/.trusty-mpm`),
/// `CLAUDE_CONFIG_DIR` is absolute and survives a `$HOME` repoint (#5544), and
/// `CLAUDE_CODE_SESSION_ID` is what made a spawned child append a real row to
/// the operator's savings ledger (#7514). `TRUSTY_MPM_URL`, `TMUX` and the
/// managed-session ids make a child adopt the developer's live daemon, tmux
/// server and session rather than the fixture's.
///
/// What: cleared on the CHILD only. Nothing here touches this process's
/// environment, so the `#5544` hazard — a `set_var` visible to every parallel
/// sibling in the same test binary — does not arise.
/// Test: `the_helper_clears_every_state_pointing_var`.
const CHILD_STATE_ENV: &[&str] = &[
    "TRUSTY_MPM_ROOT",
    "TRUSTY_MPM_URL",
    "XDG_CONFIG_HOME",
    "CLAUDE_CONFIG_DIR",
    trusty_mpm::core::savings::CLAUDE_CODE_SESSION_ID_ENV,
    "CLAUDE_SESSION_ID",
    "TM_MANAGED_SESSION_ID",
    "TRUSTY_MPM_MANAGED_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
    "REPOS_ROOT",
    "GH_CONFIG_DIR",
];

/// Point `cmd`'s child at `home` and strip every other state-pointing variable.
///
/// Why (#7568): a spawned `tm` resolves `~/.trusty-mpm` from the `$HOME` it
/// inherits, so an integration target that spawns the binary writes into the
/// operator's own framework root — observed as `migrations.json`,
/// `compression.jsonl` and `usage/` markers appearing under a real `$HOME`
/// during `cargo test -p trusty-mpm`.
/// What: one `env("HOME", …)` plus an `env_remove` per [`CHILD_STATE_ENV`]
/// entry, applied to the child's environment block. A caller that needs its own
/// value for one of these chains `.env(…)` afterwards — the later call for a key
/// wins — and a caller that needs `$HOME` absent chains `.env_remove("HOME")`.
/// Test: `a_spawned_tm_resolves_the_helper_home_and_leaves_the_operators_alone`,
/// `the_helper_clears_every_state_pointing_var`.
pub fn isolate_spawned_tm<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    cmd.env("HOME", home);
    for key in CHILD_STATE_ENV {
        cmd.env_remove(key);
    }
    cmd
}

/// A `tm` command whose child is confined to `home`.
pub fn tm_command_in(home: &Path) -> Command {
    let mut cmd = Command::new(tm_bin());
    isolate_spawned_tm(&mut cmd, home);
    cmd
}

/// The scratch `$HOME` this test process hands its spawned children (#7568).
///
/// Why: most spawn sites never inspect what the child wrote — they assert on
/// stdout — so one scratch home per PROCESS is enough isolation and avoids
/// leaking a directory per spawn. A test that reads the child's writes back
/// calls [`tm_command_in`] with a home it owns.
/// What: a `OnceLock` scratch directory under `/tmp`, `keep()`-ed for the same
/// reason [`scratch_home`] keeps its own and carrying the same `tm-test-`
/// prefix, so `test_support::sweep_stale_test_dirs` reaps it after a day.
/// Unlike [`scratch_home`] this NEVER calls `set_var` — the path is handed to
/// spawned children and to nothing else.
pub fn tm_spawn_home() -> &'static Path {
    static SPAWN_HOME: OnceLock<PathBuf> = OnceLock::new();
    SPAWN_HOME
        .get_or_init(|| {
            tempfile::Builder::new()
                .prefix("tm-test-spawn-home-")
                .tempdir_in("/tmp")
                .expect("create scratch spawn $HOME")
                .keep()
        })
        .as_path()
}

/// A `tm` command confined to [`tm_spawn_home`].
pub fn tm_command() -> Command {
    tm_command_in(tm_spawn_home())
}
