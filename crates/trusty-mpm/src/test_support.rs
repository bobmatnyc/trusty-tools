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
//! Test: `test_support::tests` below, including regression coverage for a
//! degenerate-`$HOME` false positive (`HOME=/tmp` or `HOME=/`, as seen in
//! root-container / OpenShift-style environments) that an earlier revision
//! of [`guard_against_project_tree`] panicked on unconditionally; see also
//! the TMPDIR-pollution proof in the #3382 PR description
//! (`TMPDIR=$HOME/... cargo test -p trusty-mpm provisioner` deposits nothing
//! under that tree).

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

/// Whether `home` is a "meaningful" (non-degenerate) containment boundary
/// relative to `candidate` — more than one path component, and not
/// identical to `candidate` itself.
///
/// Why: shared by [`guard_against_project_tree`] AND its own test suite
/// (`tests::hermetic_temp_dir_is_prefixed_and_outside_home`) so both apply
/// the EXACT same degenerate-`$HOME` exclusion — a code reviewer reproduced
/// a real crash by running the compiled test binary with `HOME=/tmp` (or
/// `HOME=/`): every real system temp path trivially "starts with" `/`, and
/// `/tmp` trivially equals `HOME=/tmp`, so a naive `Path::starts_with` check
/// panicked on EVERY [`hermetic_temp_dir`] call in that environment —
/// strictly worse than the pre-fix behavior, not defense in depth. Having
/// the assertion logic live in one place prevents the test suite's own
/// containment check from drifting out of sync with the production check and
/// reintroducing the same false positive from the other direction.
/// What: `home.components().count() > 1 && home != candidate`.
fn is_meaningful_home_boundary(home: &Path, candidate: &Path) -> bool {
    home.components().count() > 1 && home != candidate
}

/// Panic loudly if `root` resolves inside the user's home directory.
///
/// Why: defense in depth. [`real_system_tmp`]'s hardcoded `/tmp` never trips
/// this in practice, but if that function is ever changed to consult an env
/// var again (or a future platform's fallback resolves somewhere
/// unexpected), a hermetic root that lands inside `$HOME` — where every
/// observed project tree lives — must fail the test run loudly rather than
/// silently littering it, per #3382.
/// What: compares `root` against `$HOME` with `Path::starts_with`, but ONLY
/// when [`is_meaningful_home_boundary`] says `$HOME` is non-degenerate
/// relative to `root`. No-ops if `$HOME` is unset.
/// Test: [`tests::guard_panics_on_genuine_home_containment`],
/// [`tests::guard_does_not_panic_when_home_is_tmp`],
/// [`tests::guard_does_not_panic_when_home_is_root`].
fn guard_against_project_tree(root: &Path) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    if is_meaningful_home_boundary(&home, root) && root.starts_with(&home) {
        panic!(
            "hermetic test temp root {root:?} resolves inside $HOME ({home:?}) — \
             refusing to risk littering a project tree (see #3382)"
        );
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

    /// Why serial: reads `$HOME` (via `guard_against_project_tree`, indirectly
    /// through `hermetic_temp_dir`) and asserts on it below. Must be
    /// serialized against every other test in this binary that mutates or
    /// reads `$HOME` for the same reason — same shared default group as
    /// `session_manager::workspace_guard::tests::is_safe_to_remove_rejects_home`
    /// (#2461 sweep) and the three `HomeOverride`-mutating tests below; a
    /// code reviewer noted the mutating tests' own `Mutex` only serializes
    /// them against each other, not against `$HOME`-reading tests elsewhere,
    /// which `#[serial]`'s shared default group closes.
    #[serial_test::serial]
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
        // Same degenerate-HOME exclusion as `guard_against_project_tree`
        // (compared against the ROOT, not the leaf `dir.path()` — otherwise
        // this assertion reintroduces the exact false positive it's meant to
        // catch: `dir.path()` is always a child of, never equal to, the
        // root, so a naive `home != dir.path()` check is never degenerate).
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            if is_meaningful_home_boundary(&home, &real_system_tmp()) {
                assert!(
                    !dir.path().starts_with(&home),
                    "hermetic dir must never resolve inside $HOME: {:?}",
                    dir.path()
                );
            }
        }
    }

    /// RAII guard that overrides `$HOME` for the duration of a `#[serial]`
    /// test and restores the prior value on drop (including on panic-driven
    /// unwind, via the `#[should_panic]` test below) — mirrors
    /// `core::session_launch::tests::EnvVarGuard`'s established pattern in
    /// this crate.
    ///
    /// Why NOT an internal `Mutex` (a code reviewer flagged an earlier
    /// revision that had one): a `Mutex` scoped to this struct only
    /// serializes `HomeOverride`-using tests against EACH OTHER, not against
    /// every other test in this binary that reads `$HOME` without going
    /// through this guard — e.g.
    /// [`hermetic_temp_dir_is_prefixed_and_outside_home`] above, or
    /// `session_manager::workspace_guard::tests::is_safe_to_remove_rejects_home`.
    /// `#[serial_test::serial]`'s shared default group, tagged on every one
    /// of those tests, closes that gap crate-wide instead of only locally.
    struct HomeOverride {
        prev: Option<std::ffi::OsString>,
    }

    impl Drop for HomeOverride {
        fn drop(&mut self) {
            // SAFETY: every caller of `override_home` is `#[serial]`, so no
            // other test thread races this set/restore.
            match self.prev.take() {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    /// Set `$HOME` to `value`, returning a guard that restores it on drop.
    /// Callers MUST be tagged `#[serial_test::serial]` — see [`HomeOverride`].
    fn override_home(value: &str) -> HomeOverride {
        let prev = std::env::var_os("HOME");
        // SAFETY: caller is `#[serial]`.
        unsafe { std::env::set_var("HOME", value) };
        HomeOverride { prev }
    }

    /// The genuine positive case: a real per-user home (e.g. `/Users/x`,
    /// `/home/x`) containing the candidate root must still panic.
    ///
    /// Why serial: mutates `$HOME` via [`override_home`] — see
    /// [`HomeOverride`] for why serialization must be crate-wide, not a
    /// locally scoped lock.
    #[serial_test::serial]
    #[test]
    #[should_panic(expected = "resolves inside $HOME")]
    fn guard_panics_on_genuine_home_containment() {
        let _home = override_home("/Users/test-user");
        guard_against_project_tree(&PathBuf::from("/Users/test-user/trusty-mpm-projects"));
    }

    /// Regression for the false-positive a code reviewer reproduced: with
    /// `HOME=/tmp` (root-container / OpenShift-style environments),
    /// `real_system_tmp()`'s `/tmp` trivially equals `$HOME`, so a naive
    /// `starts_with` check panicked on EVERY `hermetic_temp_dir()` call —
    /// strictly worse than the pre-fix behavior. Must be a no-op.
    ///
    /// Why serial: see [`guard_panics_on_genuine_home_containment`].
    #[serial_test::serial]
    #[test]
    fn guard_does_not_panic_when_home_is_tmp() {
        let _home = override_home("/tmp");
        guard_against_project_tree(&PathBuf::from("/tmp"));
    }

    /// Regression for the same false-positive with `HOME=/`: every absolute
    /// path trivially "starts with" `/`, so a naive check panicked on every
    /// `hermetic_temp_dir()` call. Must be a no-op.
    ///
    /// Why serial: see [`guard_panics_on_genuine_home_containment`].
    #[serial_test::serial]
    #[test]
    fn guard_does_not_panic_when_home_is_root() {
        let _home = override_home("/");
        guard_against_project_tree(&PathBuf::from("/tmp"));
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
