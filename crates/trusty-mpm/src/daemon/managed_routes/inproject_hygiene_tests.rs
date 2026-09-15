//! Unit tests for startup hygiene's pure decision logic (#2177, #4961).
//!
//! Why: the update gate is the only thing standing between a daemon restart and
//! a user's uncommitted work. Its decision logic is deliberately a pure
//! function so every refusal path can be asserted without a git fixture; the
//! git-shelling half is covered by the integration tests in
//! `crates/trusty-mpm/tests/inproject_hygiene_test.rs`.
//! What: one test per `decide_update` refusal reason, plus the single
//! proceed case, plus the cheap non-git early returns.
//! Test: this file IS the test module.

use super::*;

#[test]
fn get_default_branch_returns_none_for_non_git() {
    // A non-git directory must return None cleanly.
    let tmp = std::env::temp_dir();
    assert!(get_default_branch(&tmp).is_none());
}

#[test]
fn run_hygiene_skips_missing_dir() {
    // A path that does not have .git must return Ok(()) immediately.
    let tmp = std::env::temp_dir();
    let result = run_hygiene_for_base(&tmp);
    assert!(
        result.is_ok(),
        "should skip non-git dir cleanly: {result:?}"
    );
}

#[test]
fn run_hygiene_for_all_bases_skips_missing_root() {
    // A non-existent repos root must complete without panicking.
    let missing = std::path::Path::new("/tmp/trusty-nonexistent-repos-root-hygiene-test");
    run_hygiene_for_all_bases(missing); // must not panic
}

#[test]
fn hygiene_opt_out_marker_detected() {
    // The marker short-circuits the sweep before any git command runs, so a
    // directory that merely LOOKS like a repo is enough to assert the path.
    let tmp = tempfile::TempDir::new().expect("temp dir");
    std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
    std::fs::write(tmp.path().join(HYGIENE_OPT_OUT_MARKER), "").expect("write marker");
    // Without the marker this would shell out to git against a bogus .git dir;
    // with it, the function returns before that happens.
    assert!(run_hygiene_for_base(tmp.path()).is_ok());
}

// ── decide_update: pure decision-logic unit tests (#2177, #4961) ──────────

#[test]
fn decide_update_ahead_skips() {
    // Any unpushed commit must refuse the update, even on a clean tree.
    match decide_update(Some("main"), "main", Some(1), Some(false)) {
        UpdateDecision::Skip(reason) => assert!(reason.contains("ahead")),
        UpdateDecision::Update => panic!("an ahead branch must never be updated"),
    }
}

#[test]
fn decide_update_dirty_skips() {
    // A dirty tree must refuse the update, even when not ahead.
    match decide_update(Some("main"), "main", Some(0), Some(true)) {
        UpdateDecision::Skip(reason) => assert!(reason.contains("uncommitted")),
        UpdateDecision::Update => panic!("a dirty tree must never be updated"),
    }
}

#[test]
fn decide_update_unknown_ahead_skips() {
    // No upstream / rev-list failure (ahead=None) must conservatively refuse,
    // regardless of the dirty state.
    match decide_update(Some("main"), "main", None, Some(false)) {
        UpdateDecision::Skip(_) => {}
        UpdateDecision::Update => panic!("unknown ahead-count must never be updated"),
    }
}

#[test]
fn decide_update_unknown_dirty_skips() {
    // A `git status` failure (dirty=None) must conservatively refuse,
    // regardless of the ahead-count.
    match decide_update(Some("main"), "main", Some(0), None) {
        UpdateDecision::Skip(_) => {}
        UpdateDecision::Update => panic!("unknown dirty-state must never be updated"),
    }
}

#[test]
fn decide_update_detached_head_skips() {
    // Detached HEAD: there is no branch to fast-forward, so refuse outright
    // rather than moving whatever ref happens to be nearby.
    match decide_update(None, "main", Some(0), Some(false)) {
        UpdateDecision::Skip(reason) => assert!(reason.contains("detached")),
        UpdateDecision::Update => panic!("a detached HEAD must never be updated"),
    }
}

#[test]
fn decide_update_non_default_branch_skips() {
    // #4961 second finding: ahead-count is measured against `origin/<branch>`
    // but the update targets `origin/<default>`. Off the default branch the
    // check proves nothing about the ref being moved, so refuse.
    match decide_update(Some("feature"), "main", Some(0), Some(false)) {
        UpdateDecision::Skip(reason) => {
            assert!(
                reason.contains("feature"),
                "reason names the branch: {reason}"
            );
            assert!(
                reason.contains("main"),
                "reason names the default: {reason}"
            );
        }
        UpdateDecision::Update => panic!("a non-default branch must never be moved to origin/main"),
    }
}

#[test]
fn decide_update_clean_and_even_updates() {
    // The only case that may proceed: on the default branch, zero ahead,
    // confirmed clean.
    assert_eq!(
        decide_update(Some("main"), "main", Some(0), Some(false)),
        UpdateDecision::Update
    );
}

// ---------------------------------------------------------------------------
// #7965: the fetch gate and the pass budget.
// ---------------------------------------------------------------------------

/// A base clone directory with a `.git/FETCH_HEAD` aged `age` ago, or none.
///
/// Why a helper: all four `fetch_is_due` cases differ only in that file's state,
/// and writing the mtime by hand at each call site is where an accidental "now"
/// would creep in and make a test pass for the wrong reason.
fn base_with_fetch_head(dir: &std::path::Path, age: Option<Duration>) -> std::path::PathBuf {
    let base = dir.join("owner").join("repo");
    std::fs::create_dir_all(base.join(".git")).expect("base clone");
    if let Some(age) = age {
        set_fetch_head_mtime(&base, std::time::SystemTime::now() - age);
    }
    base
}

/// Write `<base>/.git/FETCH_HEAD` and stamp its mtime to `when`.
///
/// Why std rather than a crate: `File::set_times` covers this, and a test helper
/// is not a reason to add a dependency to the whole crate.
fn set_fetch_head_mtime(base: &std::path::Path, when: std::time::SystemTime) {
    let head = base.join(".git").join("FETCH_HEAD");
    let file = std::fs::File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&head)
        .expect("FETCH_HEAD");
    file.set_times(std::fs::FileTimes::new().set_modified(when))
        .expect("mtime");
}

/// 🔴 #7965: no `FETCH_HEAD` means the base has never been fetched — always due.
///
/// Why this is the fail-open direction: every unreadable input must FETCH. The
/// cost of being wrong that way is one redundant network call; the cost of the
/// other way is a base clone that is never freshened again.
#[test]
fn fetch_is_due_without_a_fetch_head() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = base_with_fetch_head(tmp.path(), None);
    assert!(
        fetch_is_due(&base, Duration::from_secs(6 * 60 * 60)),
        "a base with no FETCH_HEAD has never been fetched"
    );
    // A directory that is not a base clone at all takes the same branch.
    assert!(fetch_is_due(tmp.path(), Duration::from_secs(6 * 60 * 60)));
}

/// The interval is a threshold, asserted on BOTH sides of itself.
///
/// Why both: a gate tested only on the skip side passes just as well when it
/// always skips, which would stop every base clone being fetched ever again.
#[test]
fn fetch_is_due_on_both_sides_of_the_interval() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let interval = Duration::from_secs(6 * 60 * 60);

    let fresh = base_with_fetch_head(&tmp.path().join("fresh"), Some(Duration::from_secs(60)));
    assert!(
        !fetch_is_due(&fresh, interval),
        "a base fetched a minute ago is not due"
    );

    let stale = base_with_fetch_head(
        &tmp.path().join("stale"),
        Some(Duration::from_secs(7 * 60 * 60)),
    );
    assert!(
        fetch_is_due(&stale, interval),
        "a base fetched seven hours ago is due"
    );
}

/// 🔴 #7965: an mtime in the FUTURE fetches rather than skipping forever.
///
/// Why: `SystemTime::duration_since` returns `Err` when the timestamp is ahead of
/// now — a clock stepped backwards, or an NFS mtime written by a faster host.
/// Treating that as "recently fetched" would suppress the fetch until the clock
/// caught up, which on a badly skewed host is never.
#[test]
fn fetch_is_due_when_the_mtime_is_in_the_future() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = base_with_fetch_head(tmp.path(), Some(Duration::ZERO));
    set_fetch_head_mtime(
        &base,
        std::time::SystemTime::now() + Duration::from_secs(48 * 60 * 60),
    );
    assert!(
        fetch_is_due(&base, Duration::from_secs(6 * 60 * 60)),
        "a FETCH_HEAD dated in the future must fetch, not skip"
    );
}

/// Initialise `<repos_root>/owner/<name>` as a git repo whose `origin` blackholes.
///
/// Why a non-routable remote: it makes "this base was visited" measurable. A
/// visited base pays the per-command ceiling on its fetch; a skipped one pays
/// nothing, and the two are then tens of times apart in wall clock.
fn base_with_a_blackhole_remote(repos_root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let base = repos_root.join("owner").join(name);
    std::fs::create_dir_all(&base).expect("base");
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git://10.255.255.1:9418/x.git"],
    ] {
        let out = trusty_common::git::command_in(&base)
            .args(&args)
            .output()
            .expect("git must be available");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    base
}

/// 🔴 #7965: a recently fetched base clone skips its fetch during a real pass.
///
/// Why drive `run_hygiene_for_base_within` rather than `fetch_is_due` alone: the
/// unit tests above prove the predicate; this proves the sweep CONSULTS it.
/// `origin` blackholes, so a fetch that was NOT skipped blocks until the
/// per-command ceiling — 3 s here — while the remaining steps are local git reads
/// that cost milliseconds. The 2 s assertion sits in that gap.
#[test]
fn hygiene_skips_a_recently_fetched_base() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repos_root = tmp.path().join("repos");
    let base = base_with_a_blackhole_remote(&repos_root, "repo");
    // `git init` never writes FETCH_HEAD, so this is the only one present.
    set_fetch_head_mtime(
        &base,
        std::time::SystemTime::now() - Duration::from_secs(60),
    );

    let started = std::time::Instant::now();
    run_hygiene_for_base_within(&base, Duration::from_secs(3)).expect("pass completes");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "a base fetched a minute ago must not pay for a fetch at all; took {elapsed:?}"
    );
}

/// 🔴 #7965: the pass budget STOPS the walk rather than slowing it.
///
/// Why elapsed time IS the right observable here: each base's `origin` blackholes,
/// so a base the sweep visits costs its whole per-command ceiling on the fetch and
/// a base it skips costs nothing. An expired budget must therefore return almost
/// instantly across three bases, while a live budget pays for all three — which is
/// also what proves the fast result is the budget and not an empty enumeration.
#[test]
fn hygiene_pass_budget_stops_the_sweep() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repos_root = tmp.path().join("repos");
    for name in ["a", "b", "c"] {
        base_with_a_blackhole_remote(&repos_root, name);
    }
    // 1 s is ten times what the local git steps cost and a tenth of what an
    // unbounded blocked connect costs, so the two passes stay far apart without
    // adding seconds to the suite.
    let ceiling = Duration::from_secs(1);

    let started = std::time::Instant::now();
    let skipped = run_hygiene_for_all_bases_within(&repos_root, Duration::ZERO, ceiling);
    let skipped_pass = started.elapsed();
    assert_eq!(
        skipped, 3,
        "every base past the budget must be counted as skipped — this is the number \
         the `warn!` line reports"
    );
    assert!(
        skipped_pass < Duration::from_secs(1),
        "an expired budget must abandon every base immediately; took {skipped_pass:?}"
    );

    let started = std::time::Instant::now();
    let skipped = run_hygiene_for_all_bases_within(&repos_root, Duration::from_secs(120), ceiling);
    let full_pass = started.elapsed();
    assert_eq!(skipped, 0, "a live budget skips nothing");
    assert!(
        full_pass >= ceiling,
        "a live budget must actually visit the bases, paying the ceiling on each \
         blocked fetch; took {full_pass:?} — if this is fast, the zero-budget result \
         above proves nothing"
    );
}
