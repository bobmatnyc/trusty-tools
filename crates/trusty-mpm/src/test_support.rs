//! Hermetic test temp-directory helper (#3382).
//!
//! Why: bare `tempfile::TempDir::new()` resolves via `std::env::temp_dir()`,
//! which honors an inherited `$TMPDIR`. A test-invoking harness/sandbox set
//! `TMPDIR` to a real project directory (observed: `~/trusty-mpm-projects`),
//! so every bare `TempDir::new()` call in the suite deposited its mktemp
//! scaffold directly into that project tree instead of a scratch location —
//! 50 directories / ~167MB accumulated over ~24h with nothing reaping them.
//! What: [`hermetic_temp_dir`] is the one replacement for every bare
//! `TempDir::new()` in trusty-mpm's test code. It roots the directory under
//! [`real_system_tmp`] — the hardcoded OS temp path, chosen deliberately
//! over `std::env::temp_dir()` so an inherited `TMPDIR` can never redirect
//! test litter into a project tree again — and tags it with the
//! [`TEST_DIR_PREFIX`] so any directory that survives a hard-killed test
//! process (Drop-based cleanup cannot run after SIGKILL) is trivially
//! attributable and safe to sweep. [`sweep_stale_test_dirs`] runs once per
//! test process (via [`std::sync::Once`], triggered by the first
//! `hermetic_temp_dir()` call) and best-effort removes `tm-test-*`
//! directories older than a day, bounding leak growth without a background
//! daemon.
//! Test: `test_support::tests` below; see also the TMPDIR-pollution proof in
//! the #3382 PR description (`TMPDIR=$HOME/... cargo test -p trusty-mpm
//! provisioner` deposits nothing under that tree).

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

/// Prefix every hermetic test temp directory carries.
///
/// Why: `TempDir::drop` cannot run after a hard-killed test process (e.g. a
/// CI timeout `SIGKILL`), so some leakage is unavoidable. A stable, greppable
/// prefix makes any leaked directory trivially attributable to trusty-mpm's
/// test suite (vs. some other tool's temp files) and safe for
/// [`sweep_stale_test_dirs`] — or a human running `rm -rf` — to reap without
/// guessing.
pub(crate) const TEST_DIR_PREFIX: &str = "tm-test-";

/// How long a `tm-test-*` directory may sit in the hermetic root before
/// [`sweep_stale_test_dirs`] treats it as leaked and removes it.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

static SWEEP_ONCE: Once = Once::new();

/// Return the real, hardcoded OS temp root.
///
/// Why: deliberately NOT `std::env::temp_dir()`, which honors `$TMPDIR` —
/// the exact mechanism #3382's incident exploited. The chosen rule: on Unix
/// (macOS + Linux, the only targets trusty-mpm's test suite runs on — CI is
/// `ubuntu-latest`, local dev is macOS), always resolve to `/tmp`. It is the
/// one system scratch path that exists on every Unix trusty-mpm supports,
/// is never inside a user's home or project tree, and is NOT configurable
/// via any environment variable — so no inherited `TMPDIR`, however
/// polluted, can ever redirect it. On non-Unix targets (the Tauri GUI's
/// Windows builds only; no test suite runs there) this falls back to
/// `std::env::temp_dir()`, since Windows has no established TMPDIR-pollution
/// pattern and no equivalent always-present hardcoded system path.
/// What: returns `/tmp` on Unix, `std::env::temp_dir()` otherwise.
/// Test: [`tests::real_system_tmp_ignores_tmpdir_env`].
fn real_system_tmp() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}

/// Panic loudly if `root` resolves inside the user's home directory.
///
/// Why: defense in depth. [`real_system_tmp`]'s hardcoded `/tmp` never trips
/// this in practice, but if that function is ever changed to consult an env
/// var again (or a future platform's fallback resolves somewhere
/// unexpected), a hermetic root that lands inside `$HOME` — where every
/// observed project tree lives — must fail the test run loudly rather than
/// silently littering it, per #3382.
/// What: compares `root` against `$HOME` with `Path::starts_with`; no-ops if
/// `$HOME` is unset.
/// Test: [`tests::guard_panics_on_home_relative_root`].
fn guard_against_project_tree(root: &Path) {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if !home.as_os_str().is_empty() && root.starts_with(&home) {
            panic!(
                "hermetic test temp root {root:?} resolves inside $HOME ({home:?}) — \
                 refusing to risk littering a project tree (see #3382)"
            );
        }
    }
}

/// Create a hermetic test `TempDir`, immune to inherited `$TMPDIR` pollution.
///
/// Why: the one replacement for every bare `TempDir::new()` call site
/// flagged by #3382 — rooting under [`real_system_tmp`] instead of
/// `env::temp_dir()` means a polluted `TMPDIR` can never again cause test
/// scaffolding to land in a user's project tree.
/// What: sweeps stale leaked directories once per test process (see
/// [`sweep_stale_test_dirs`]), then creates a `TempDir` under the hermetic
/// root with the [`TEST_DIR_PREFIX`] prefix.
/// Test: [`tests::hermetic_temp_dir_is_prefixed_and_outside_home`].
pub(crate) fn hermetic_temp_dir() -> TempDir {
    SWEEP_ONCE.call_once(sweep_stale_test_dirs);
    let root = real_system_tmp();
    guard_against_project_tree(&root);
    tempfile::Builder::new()
        .prefix(TEST_DIR_PREFIX)
        .tempdir_in(&root)
        .expect("create hermetic test temp dir")
}

/// Best-effort sweep of stale `tm-test-*` directories left behind by
/// hard-killed test processes.
///
/// Why: `TempDir::drop` cannot fire after a `SIGKILL`, so leaked directories
/// are an accepted possibility, not a bug to eliminate outright — #3382's
/// incident was 50 of them accumulating over ~24h with nothing reaping them.
/// A lightweight sweep at the start of a test process bounds that growth
/// without a background daemon, proportionate to how rare and small the
/// leaks actually are.
/// What: reads [`real_system_tmp`], and for every entry whose name starts
/// with [`TEST_DIR_PREFIX`] and whose modified time is more than
/// [`STALE_AFTER`] old, best-effort `remove_dir_all`s it. All errors
/// (permission, concurrent removal by another test process racing the same
/// sweep, a non-existent root) are silently ignored — this is hygiene, not a
/// correctness requirement, so it must never fail a test run.
/// Test: [`tests::sweep_removes_only_stale_prefixed_dirs`].
fn sweep_stale_test_dirs() {
    let root = real_system_tmp();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(TEST_DIR_PREFIX) {
            continue;
        }
        let is_stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if is_stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    /// `real_system_tmp` must ignore `$TMPDIR` even when it points somewhere
    /// that would otherwise cause litter (e.g. a project tree).
    #[test]
    fn real_system_tmp_ignores_tmpdir_env() {
        // Safety/portability note: this test only asserts the *return value*
        // is independent of TMPDIR; it does not mutate global env state.
        let root = real_system_tmp();
        #[cfg(unix)]
        assert_eq!(root, PathBuf::from("/tmp"));
        assert!(root.exists(), "hermetic root must exist: {root:?}");
    }

    #[test]
    fn hermetic_temp_dir_is_prefixed_and_outside_home() {
        let dir = hermetic_temp_dir();
        let name = dir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assert!(
            name.starts_with(TEST_DIR_PREFIX),
            "expected {name:?} to start with {TEST_DIR_PREFIX:?}"
        );
        if let Some(home) = std::env::var_os("HOME") {
            assert!(
                !dir.path().starts_with(PathBuf::from(home)),
                "hermetic dir must never resolve inside $HOME: {:?}",
                dir.path()
            );
        }
    }

    #[test]
    #[should_panic(expected = "resolves inside $HOME")]
    fn guard_panics_on_home_relative_root() {
        let home = std::env::var_os("HOME").expect("HOME must be set to run this test");
        guard_against_project_tree(&PathBuf::from(home).join("trusty-mpm-projects"));
    }

    #[test]
    fn sweep_removes_only_stale_prefixed_dirs() {
        let root = real_system_tmp();
        let stale = root.join(format!(
            "{TEST_DIR_PREFIX}sweep-stale-{}",
            std::process::id()
        ));
        let fresh = root.join(format!(
            "{TEST_DIR_PREFIX}sweep-fresh-{}",
            std::process::id()
        ));
        let unrelated = root.join(format!("not-tm-prefixed-{}", std::process::id()));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();

        // Back-date the "stale" directory's mtime by 2 days (std::fs::FileTimes,
        // stable since 1.75 — no need for an extra crate dependency here).
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        let times = std::fs::FileTimes::new().set_modified(two_days_ago);
        std::fs::File::open(&stale)
            .unwrap()
            .set_times(times)
            .unwrap();

        sweep_stale_test_dirs();

        assert!(!stale.exists(), "stale tm-test- dir should be swept");
        assert!(fresh.exists(), "fresh tm-test- dir should survive");
        assert!(unrelated.exists(), "non-prefixed dir must never be touched");

        // Clean up what the sweep left behind (this test writes directly
        // under the real /tmp, not a TempDir, so nothing auto-removes them).
        let _ = std::fs::remove_dir_all(&fresh);
        let _ = std::fs::remove_dir_all(&unrelated);
        let _ = std::fs::remove_dir_all(&stale);
    }

    /// Guards against `UNIX_EPOCH` underflow bugs in age math (belt-and-braces).
    #[test]
    fn stale_after_is_positive_duration() {
        assert!(STALE_AFTER > Duration::from_secs(0));
        assert!(SystemTime::now().duration_since(UNIX_EPOCH).is_ok());
    }
}
