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

/// Same prefix the lib's fixture uses, so its sweep reaps these too.
const TEST_DIR_PREFIX: &str = "tm-test-";

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
