//! Shared helper for allocating index-root directories in tests without
//! tripping the sensitive-path denylist (issue #3955).
//!
//! Why: `tempfile::tempdir()` roots under `std::env::temp_dir()`, which
//! resolves to `/var/folders/…` by default on macOS and `/tmp` on Linux —
//! both hard-denylisted by `crate::allowlist::SENSITIVE_PATH_PREFIXES`. A
//! `create_index`/`relocate_index` test that hands such a path straight to
//! the handler gets refused with `400` for a reason unrelated to what the
//! test is actually checking.
//!
//! Several tests independently worked around this by rooting a
//! `tempfile::Builder` dir at `<checkout>/target/…` instead. That helps when
//! `$TMPDIR` is the problem, but not when the *checkout itself* lives under
//! a denylisted prefix: `is_denied` matches the full, checkout-independent
//! path string, so a worktree placed under `/private/tmp` (as could happen
//! before the harness-level guard added in #3978) denies `target/…` too —
//! every path under such a checkout starts with `/private/tmp`. A directory
//! anchored at `$HOME` under a name that appears in none of
//! `SENSITIVE_HOME_TOP_DIRS` / `SENSITIVE_COMPONENT_NAMES` is the one
//! location that is safe regardless of both the checkout's location and
//! `$TMPDIR` — see `crate::allowlist::is_denied_inner`'s four checks.
//!
//! What: [`allowlisted_index_root`] creates a fresh RAII-cleaned `TempDir`
//! under `$HOME/.trusty-search-test-roots/<prefix>…` and returns it
//! alongside its canonicalized path, ready to hand to
//! `CreateIndexRequest.root_path` / `RelocateIndexRequest.root_path`. Stale
//! entries older than a day are opportunistically swept on each call
//! (best-effort — a run that panics past unwind or is SIGKILLed leaks a
//! directory since `Drop` never runs; the sweep bounds that litter instead
//! of relying on cleanup happening every time).
//!
//! Test: every `create_index_*` / `relocate_index_*` test that needs a real,
//! allowlist-passing root uses this helper (`tests_1073`, `tests_2336`,
//! `tests_2984`, `tests_index`, `tests_state`). `tests_denylist` deliberately
//! keeps using raw `tempfile::tempdir()` for its two "still rejects" /
//! "opts in" tests — those exist to prove the denylist itself works, so
//! they must not use an allowlisted root.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

/// How long a leftover test-root directory survives before the next call
/// opportunistically sweeps it. Generous enough to never race a slow CI
/// test run, short enough to keep `$HOME` tidy across many local runs.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Base directory for allowlist-safe test index roots: `$HOME/.trusty-search-test-roots`.
fn test_roots_base() -> PathBuf {
    let home = dirs::home_dir().expect("HOME must be set to run trusty-search tests");
    let base = home.join(".trusty-search-test-roots");
    std::fs::create_dir_all(&base).expect("create ~/.trusty-search-test-roots");
    sweep_stale_entries(&base);
    base
}

/// Best-effort removal of entries older than [`STALE_AFTER`]. Failures
/// (permission issues, concurrent removal by another test process racing
/// this one) are swallowed — this is tidiness, not correctness, and must
/// never fail a test.
fn sweep_stale_entries(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let is_stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if is_stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Create a fresh, RAII-cleaned directory under the allowlist-safe base for
/// use as an index `root_path` in tests.
///
/// Returns the `TempDir` guard (drop it to clean up immediately — keep it
/// alive for the lifetime of the test) alongside its canonicalized path,
/// since handlers under test canonicalize `root_path` before storing it and
/// tests assert against that canonical form.
pub(super) fn allowlisted_index_root(prefix: &str) -> (TempDir, PathBuf) {
    let base = test_roots_base();
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .unwrap_or_else(|e| panic!("create tempdir under {}: {e}", base.display()));
    let canonical = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    (dir, canonical)
}
