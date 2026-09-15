//! Hermetic test temp directories for the `tm` BIN target.
//!
//! Why: the `trusty-mpm` lib already owns this fixture
//! (`trusty_mpm::test_support::hermetic_temp_dir`, #3382/#3390), but it is
//! declared `#[cfg(test)] pub(crate) mod test_support` — compiled only into the
//! lib's own test binary, so no visibility change can make it reachable from
//! here. The `tm` binary is a separate compilation target and needs its own
//! copy, the same conclusion `commands::session::start_tests::HomeGuard`
//! already reached for `$HOME`.
//!
//! What it buys: a bare `tempfile::tempdir()` resolves through
//! `std::env::temp_dir()`, which honors an inherited `$TMPDIR`. That makes
//! every such call site hostage to whatever set the variable — a polluted
//! harness environment (#3382, litter in a project tree) or a sibling test that
//! mutated it mid-run (PR #4914 run 31023632348: five `pm_guard_budget` tests
//! panicked `NotFound` on a Linux runner because another test in this same
//! binary had `TMPDIR` pinned to a macOS-only path). Rooting under the
//! hardcoded system temp path removes the variable from the equation entirely.
//!
//! Deliberately NOT duplicated from the lib's copy: the stale-directory sweep.
//! Both targets emit the same `tm-test-` prefix into the same `/tmp`, so the
//! lib's once-per-process sweep already reaps anything this module leaks.
//! Test: `tests` below.

use std::path::PathBuf;

use tempfile::TempDir;

/// RAII ownership of a real tmux session created by a test (#6116).
///
/// Unlike `hermetic_temp_dir` above, this one is NOT duplicated for the binary:
/// the lib's copy and this one are the same source file, included from both.
/// A kill-on-drop guardrail written twice is one edit away from drifting, and
/// the file needs nothing from either crate root, so `#[path]` costs nothing
/// here that the duplication above pays for.
#[path = "../../test_tmux_session.rs"]
pub(crate) mod tmux_session;

/// The spawn primitive [`tmux_session`] runs every tmux invocation through
/// (#7060) — the binary target's spelling of the lib `test_support`'s
/// re-export of the same function; see that module for why the seam exists.
pub(crate) use trusty_mpm::core::spawn_disclaim::disclaimed_output as tmux_spawn;

/// The one lock every `$PATH` mutation and every PATH-resolved spawn in THIS
/// target takes (#7996).
///
/// Why: `gh_identity`'s tests prepend a fake-`gh` directory to `$PATH` with
/// `std::env::set_var`, and [`tmux_session`]'s fixtures spawn a bare `tmux`
/// the OS resolves through `$PATH` at exec time. `setenv` is not atomic
/// against a concurrent `execvp` reading the same block, so an exec that
/// straddles one of those writes finds no `tmux` — reported once as
/// `spawn tmux new-session: No such file or directory`, and once as a
/// `Drop`-time `kill-session` that silently never ran and left the session
/// alive. A module-local mutex in `gh_identity` could not close it, because
/// the other side of the race is in another module.
/// What: one process-wide mutex; poisoning is recovered so a panicking test
/// cannot wedge its siblings. The LIB target's `test_support` supplies the
/// same name by re-exporting `core::trusty_tools_config::env_test_lock`,
/// already its own single PATH regime — which is what lets the shared fixture
/// file spell this `super::lock_path_env()` and compile into both targets.
/// Test: `gh_identity::tests::gh_path_override_and_the_tmux_fixture_share_one_lock`.
static PATH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The mutex behind [`lock_path_env`], for a test that must observe contention
/// on it rather than take it.
pub(crate) fn path_env_mutex() -> &'static std::sync::Mutex<()> {
    &PATH_ENV_LOCK
}

/// Hold the target's PATH regime for the caller's scope. See [`PATH_ENV_LOCK`].
pub(crate) fn lock_path_env() -> std::sync::MutexGuard<'static, ()> {
    PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Same prefix the lib's fixture uses, so its sweep reaps these too.
const TEST_DIR_PREFIX: &str = "tm-test-";

/// An absolute, installed-looking `tm` path every hook-writing test can pin
/// (#7244) — this target's spelling of `trusty_mpm::test_support::STABLE_HOOK_EXE`,
/// duplicated for the same reason `hermetic_temp_dir` is: the lib's copy is
/// `#[cfg(test)] pub(crate)` and no visibility change reaches this target.
///
/// Why the value matters: the hooks writer refuses a build-artifact binary, and
/// a test process is one; a CI runner then has no installed `tm` to fall back
/// to. This path passes both of the writer's gates and is never created or
/// executed — only its spelling is inspected. It must stay outside any temp
/// root, which `is_ephemeral_build_path` also refuses.
/// Test: `install_claude_hooks_at_is_idempotent`, `update_cmd_errors_if_not_loaded`.
pub(crate) const STABLE_HOOK_EXE: &str = "/usr/local/bin/tm";

/// The real, hardcoded OS temp root — deliberately NOT `std::env::temp_dir()`.
///
/// Why: `env::temp_dir()` reads `$TMPDIR`, which is the exact indirection this
/// module exists to remove. `/tmp` exists on both targets the suite runs on
/// (macOS locally, `ubuntu-latest` in CI), is never inside a project tree, and
/// no environment variable can redirect it.
fn real_system_tmp() -> PathBuf {
    PathBuf::from("/tmp")
}

/// Create a test `TempDir` immune to an inherited or sibling-mutated `$TMPDIR`.
///
/// What: the one replacement for a bare `tempfile::tempdir()` in this binary's
/// test code.
/// Test: [`tests::hermetic_temp_dir_ignores_tmpdir`].
pub(crate) fn hermetic_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix(TEST_DIR_PREFIX)
        .tempdir_in(real_system_tmp())
        .expect("create hermetic test temp dir")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point, asserted rather than assumed: the directory lands under
    /// the hardcoded root, not wherever `$TMPDIR` currently points.
    #[test]
    fn hermetic_temp_dir_ignores_tmpdir() {
        let dir = hermetic_temp_dir();
        assert!(
            dir.path().starts_with(real_system_tmp()),
            "hermetic dir must live under the hardcoded root: {:?}",
            dir.path()
        );
        let name = dir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assert!(
            name.starts_with(TEST_DIR_PREFIX),
            "expected {name:?} to start with {TEST_DIR_PREFIX:?} so the lib's sweep reaps it"
        );
    }
}
