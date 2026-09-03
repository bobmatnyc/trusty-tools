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

use std::path::{Path, PathBuf};
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
