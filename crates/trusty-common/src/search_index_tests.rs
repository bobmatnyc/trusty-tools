//! Unit tests for `search_index`'s find-or-create/incremental-index helpers.
//!
//! Why: isolated in a sibling file (declared via `#[path = "search_index_tests.rs"]
//! mod tests;` in `search_index.rs`) to keep `search_index.rs` under the 500-SLOC
//! production cap while retaining full test coverage. As a child module,
//! `super::` reaches private items in `search_index` (issue #2914 split — the
//! module grew past the cap when the `allow_sensitive_path` regression tests
//! were added).
//!
//! What: exercises `ensure_project_indexed`'s daemon-down/no-op paths, the
//! `allow_sensitive_path` plumbing (both the pure body-builder and the
//! live-daemon wire regression), the incremental per-file index-update helpers,
//! the freshness predicate, and the per-file retry/backoff schedule. #7237 moved
//! every registration rig from a hand-rolled HTTP `TcpListener` onto
//! `crate::uds_mock`, because registration now speaks the daemon's socket; the
//! per-file incremental rigs are still HTTP, because that path is.
//!
//! Test: `cargo test -p trusty-common --features search-index -- search_index::tests`

use super::*;
use std::fs;
use std::path::PathBuf;

fn scratch_dir(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!("trusty-search-index-{tag}-{pid}-{nanos}"));
    let _ = fs::remove_dir_all(&p);
    p
}

/// Derivation still walks to the git root, and nothing registered means nothing
/// pinnable (#1373, #5091).
///
/// Why: this test used to be called
/// `ensure_project_indexed_returns_derived_id_when_daemon_down` and asserted the
/// id came back regardless. It was wrong twice over. The contract it pinned is
/// the #5091 defect — an id handed to a caller that will pin it, for an index
/// nothing created. And its name never matched what it ran: under `cargo test`
/// the #4255 harness guard short-circuits before daemon discovery, so the
/// daemon-down branch it claims to exercise is never reached (the branch that
/// IS reached, `SkippedUnderTest`, sends nothing either — same conclusion).
/// What: keeps the derivation assertion — the id is the git-root basename even
/// from a nested directory, read off the reporting entry point which still
/// carries it — and adds the #5091 one: the id-only entry point withholds it,
/// because no registration was observed.
/// Test: this test.
#[test]
fn ensure_project_indexed_withholds_id_when_nothing_was_registered() {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir("data");
    fs::create_dir_all(&data_dir).unwrap();
    // SAFETY: guarded by ENV_LOCK; removed below before returning.
    unsafe {
        std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
    }

    // A git-rooted project: id == the git-root basename, even from a nested dir.
    let project = scratch_dir("git");
    fs::create_dir_all(project.join(".git")).unwrap();
    let nested = project.join("crates/inner");
    fs::create_dir_all(&nested).unwrap();

    let report = ensure_project_indexed_reporting(
        &nested,
        IndexOptions::default().with_allow_sensitive_path(true),
    );
    let pinnable = ensure_project_indexed(&nested, true);
    let expected = crate::derive_index_id(&project);

    unsafe {
        std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
    }
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_dir_all(&data_dir);

    assert_eq!(
        report.index_id,
        Some(expected),
        "id is the git-root basename"
    );
    assert_ne!(
        report.registration,
        IndexRegistration::Confirmed,
        "no daemon was contacted, so nothing can be confirmed"
    );
    assert_eq!(
        pinnable, None,
        "an unregistered index must not come back as a pinnable id (#5091)"
    );
}

/// A test process gets `SkippedUnderTest`, never a claim of registration
/// (#5065 review).
///
/// Why: this is the whole point of the reporting variant. The id-only return
/// is `Some(id)` here, identical to a genuine 2xx registration, which is why
/// trusty-mpm's worktree hook could log `worktree index registered` for a call
/// that never left the process. The report has to say otherwise, and the test
/// harness is the one branch every `cargo test` run exercises for free.
/// What: calls the reporting entry point on a real git-rooted temp project
/// under the default (test-harness-detected) environment and asserts the id
/// still comes back while `registration` is `SkippedUnderTest` — not
/// `Confirmed`. Holds `ENV_LOCK` for the same reason
/// `running_under_test_harness_is_true_in_this_test_binary` does: the verdict
/// reads `ALLOW_PRODUCTION_ENV`, which sibling tests set and clear, so without
/// the lock this asserts on whatever another thread happened to leave in the
/// process env.
/// Test: this test.
#[test]
fn reporting_says_skipped_under_test_harness() {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let project = scratch_dir("report-skip");
    fs::create_dir_all(project.join(".git")).unwrap();

    let report = ensure_project_indexed_reporting(&project, IndexOptions::default());

    let _ = fs::remove_dir_all(&project);

    assert_eq!(
        report.registration,
        IndexRegistration::SkippedUnderTest,
        "a test process suppresses the write (#4255) and must say so"
    );
    assert!(
        report.index_id.is_some(),
        "the id is still returned — the fail-open contract is unchanged"
    );
}

/// With no discoverable daemon, the report says `DaemonUnreachable` (#5065
/// review).
///
/// Why: the failure mode #5045 measured at ~94% is "the daemon was not there",
/// and it is exactly the one the id-only return renders invisible. Opting out
/// of the #4255 harness guard is what makes this branch reachable at all; the
/// empty data dir then guarantees the derived socket path does not exist, so
/// nothing is ever dialled and the operator's real daemon is never touched
/// despite the opt-in (#7237).
/// What: sets `TRUSTY_ALLOW_PRODUCTION_STATE=1` and points the data dir at an
/// empty temp dir, then asserts the reported registration is
/// `DaemonUnreachable` while the id is still returned.
/// Test: this test.
#[test]
fn reporting_says_daemon_unreachable_when_no_daemon_is_discoverable() {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir("report-nodaemon-data");
    fs::create_dir_all(&data_dir).unwrap();
    let project = scratch_dir("report-nodaemon");
    fs::create_dir_all(project.join(".git")).unwrap();

    // SAFETY: guarded by ENV_LOCK; both vars are removed below before returning.
    unsafe {
        std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
        std::env::set_var(crate::test_harness::ALLOW_PRODUCTION_ENV, "1");
    }

    let report = ensure_project_indexed_reporting(&project, IndexOptions::default());

    unsafe {
        std::env::remove_var(crate::test_harness::ALLOW_PRODUCTION_ENV);
        std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
    }
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_dir_all(&data_dir);

    assert_eq!(
        report.registration,
        IndexRegistration::DaemonUnreachable,
        "no address file means nothing was sent — that is not a registration"
    );
    assert!(report.index_id.is_some(), "the id is still returned");
}

#[test]
fn ensure_project_indexed_none_for_root() {
    // The filesystem root is refused before derivation (#6550), so the helper
    // returns None without touching the daemon.
    assert_eq!(ensure_project_indexed(Path::new("/"), true), None);
    assert_eq!(ensure_project_indexed(Path::new("/"), false), None);
    assert_eq!(
        ensure_project_indexed_reporting(Path::new("/"), IndexOptions::default()).registration,
        IndexRegistration::RefusedUnindexableRoot(crate::IndexRootRefusal::FilesystemRoot)
    );
}

/// #6550, the defect itself: a registration handed `$HOME` derived index
/// `masa` from the home directory's basename, and a later reindex repointed
/// that id at a real repository it does not name.
///
/// Why: this asserts the guard at the REGISTRATION site, not just the
/// predicate — the pre-fix code reached `derive_index_id` here and returned a
/// pinnable id, so this assertion fails against it.
/// What: calls the real entry point with the real home directory and asserts
/// no id comes back and the refusal is reported. No daemon is touched: the
/// refusal precedes both the `#[4255]` harness guard and address discovery.
/// Test: this test.
#[test]
fn ensure_project_indexed_refuses_the_real_home_directory() {
    let Some(home) = dirs::home_dir() else {
        panic!("this test needs a resolvable home directory");
    };
    // `resolve_project_root` walks UP for a `.git`, so a repository ABOVE the
    // home directory would resolve elsewhere and the case under test would not
    // arise. Assert it, rather than skipping, so the test can never pass vacuously.
    assert_eq!(
        crate::resolve_project_root(&home),
        home,
        "no ancestor of $HOME may be a git repository for this case to exist"
    );

    let report = ensure_project_indexed_reporting(&home, IndexOptions::default());

    assert_eq!(
        report.registration,
        IndexRegistration::RefusedUnindexableRoot(crate::IndexRootRefusal::HomeDirectory)
    );
    assert_eq!(
        report.index_id, None,
        "the home directory's basename is the wrong id and must not be handed back"
    );
    assert_eq!(ensure_project_indexed(&home, false), None);
    assert_eq!(ensure_project_indexed(&home, true), None);
}

/// The incremental per-file path derives the same `(root, index_id)` pair, so
/// it inherits the same refusal (#6550) rather than posting file content into
/// an index named after the operator.
#[test]
fn index_files_inner_refuses_the_real_home_directory() {
    let Some(home) = dirs::home_dir() else {
        panic!("this test needs a resolvable home directory");
    };
    // Returns without panicking and without any daemon I/O; the guard runs
    // before the id is derived, so nothing can be posted.
    index_files_inner(&home, &[PathBuf::from("some/file.rs")]);
}

/// `index_files_inner` is a true no-op — no filesystem or network I/O —
/// when handed an empty path list.
///
/// Why: [`index_files_best_effort`] is called from every successful write
/// tool executor; a batch write with zero files (should not normally
/// happen, but must not misbehave if it does) must not derive an index id
/// or attempt any I/O.
/// What: calls `index_files_inner` with `project_root = "/"` (which would
/// otherwise short-circuit on the empty-id path anyway) and an empty
/// `paths` slice; asserts it returns immediately without panicking.
/// Test: this test.
#[test]
fn index_files_inner_is_noop_for_empty_paths() {
    index_files_inner(Path::new("/"), &[]);
}

/// `index_files_inner` skips cleanly when `derive_index_id` yields an
/// empty id (mirrors `ensure_project_indexed_none_for_root`'s "no index to
/// target" case for the incremental path).
///
/// Why: the filesystem root has no meaningful basename to derive an id
/// from; posting to a daemon under an empty id would be meaningless. This
/// must be detected and skipped before any daemon lookup or file read.
/// What: calls `index_files_inner` with `project_root = "/"` and a
/// non-empty `paths` slice; asserts it returns without panicking (no
/// index id to target, so no I/O is attempted).
/// Test: this test.
#[test]
fn index_files_inner_skips_when_index_id_empty() {
    index_files_inner(Path::new("/"), &[PathBuf::from("some/file.rs")]);
}

/// `index_files_inner` fails open — no panic, no propagated error — when
/// the trusty-search daemon is unreachable.
///
/// Why: this is the core "never block or fail a tool result on index
/// error" contract the mid-task incremental re-index hook depends on. We
/// force the daemon-down path the same way
/// `ensure_project_indexed_returns_derived_id_when_daemon_down` does:
/// point the data dir at an empty temp dir so `resolve_daemon_base_url`
/// finds no address file, guaranteeing no HTTP call is attempted.
/// What: seeds a git-rooted scratch project with one real file, calls
/// `index_files_inner` with that file's path, and asserts it returns
/// promptly without panicking.
/// Test: this test.
#[test]
fn index_files_inner_skips_gracefully_when_daemon_down() {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir("data-incr");
    fs::create_dir_all(&data_dir).unwrap();
    // SAFETY: guarded by ENV_LOCK; removed below before returning.
    unsafe {
        std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
    }

    let project = scratch_dir("git-incr");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::write(project.join("main.rs"), "fn main() {}\n").unwrap();

    index_files_inner(&project, &[PathBuf::from("main.rs")]);

    unsafe {
        std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
    }
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_dir_all(&data_dir);
    // No assertion beyond "did not panic" — fail-open with no daemon
    // means there is nothing further to observe from this call.
}

/// `index_files_best_effort` DROPS a batch — observably — once the shared
/// bounded pool is saturated, instead of spawning another thread (#2798).
///
/// Why: the pre-fix implementation called `std::thread::spawn` per batch, so
/// there was no saturation point at all: this test could not fail because a
/// submission could never be refused. It is the end-to-end half of the bound;
/// the pool's own boundary and concurrency ceiling are pinned deterministically
/// in `index_dispatch`'s tests.
/// What: occupies every worker with a job that blocks until released, fills the
/// queue by submitting no-ops until one is refused, then calls
/// `index_files_best_effort` and asserts the process-wide rejection counter
/// advanced by exactly one — i.e. THIS batch was the one dropped. Filling by
/// "submit until refused" rather than by a fixed count keeps the test honest if
/// a sibling test ever shares the pool. The blocked workers are released before
/// returning so the queued no-ops drain.
/// Test: this test.
#[test]
fn index_files_best_effort_drops_the_batch_when_the_shared_pool_is_saturated() {
    use crate::index_dispatch::{INDEX_QUEUE_CAPACITY, MAX_INDEX_WORKERS, global};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    let wait = Duration::from_secs(30);
    let (started_tx, started_rx) = channel();
    let mut releases = Vec::with_capacity(MAX_INDEX_WORKERS);
    for _ in 0..MAX_INDEX_WORKERS {
        let (release_tx, release_rx) = channel::<()>();
        releases.push(release_tx);
        let started = started_tx.clone();
        assert!(
            global().try_submit(Box::new(move || {
                let _ = started.send(());
                let _ = release_rx.recv_timeout(wait);
            })),
            "the shared pool refused a job before every worker was even busy"
        );
    }
    for i in 0..MAX_INDEX_WORKERS {
        started_rx
            .recv_timeout(wait)
            .unwrap_or_else(|e| panic!("blocker {i} never started: {e}"));
    }

    // Fill the queue until a submission is actually refused.
    let mut filled = 0usize;
    while global().try_submit(Box::new(|| {})) {
        filled += 1;
        assert!(
            filled <= INDEX_QUEUE_CAPACITY,
            "the queue accepted {filled} jobs, more than its {INDEX_QUEUE_CAPACITY}-slot capacity"
        );
    }

    let before = global().rejected();
    index_files_best_effort(Path::new("/nonexistent-2798"), &[PathBuf::from("main.rs")]);
    let after = global().rejected();
    let stats = index_drop_stats();

    for release in &releases {
        let _ = release.send(());
    }

    assert_eq!(
        after,
        before + 1,
        "a batch submitted to a saturated pool must be dropped and counted"
    );
    assert_eq!(
        stats.dropped_batches, after,
        "the public stats must read the same counter the pool increments"
    );
    assert!(
        stats
            .seconds_since_last_drop
            .is_some_and(|since| since <= 60),
        "a drop that just happened must be reported as recent, got {:?}",
        stats.seconds_since_last_drop
    );
}

/// The per-batch time budget stops the loop at the cap, not one file past it.
///
/// Why: a `write_files` batch has no size limit, so the only thing keeping one
/// large write from pinning a pool worker for minutes is this budget — and the
/// queue-drain reasoning behind the pool sizing depends on the exact boundary.
/// What: asserts the predicate is false just under the cap and true at and past
/// it. The predicate is pure so the boundary is testable without a daemon or a
/// 30-second wait.
/// Test: this test.
#[test]
fn batch_budget_is_exhausted_at_and_past_the_cap() {
    use std::time::Duration;
    assert!(!batch_budget_exhausted(Duration::from_secs(0)));
    assert!(!batch_budget_exhausted(
        BATCH_INDEX_BUDGET - Duration::from_millis(1)
    ));
    assert!(batch_budget_exhausted(BATCH_INDEX_BUDGET));
    assert!(batch_budget_exhausted(
        BATCH_INDEX_BUDGET + Duration::from_secs(600)
    ));
}

/// Stopping a batch on the budget is COUNTED, and lands in a different field
/// from a pool rejection (#2798 round-3 review).
///
/// Why: the budget's `break` was a `warn!` and nothing else. That is the exact
/// single-reader blind spot the drop counter was added to close — an episode
/// where every batch is ACCEPTED and then repeatedly truncated leaves files
/// unindexed batch after batch while `GET /health` reports
/// `dropped_batches: 0` forever. Against the code before this fix the second
/// half of this test fails: `truncated_batches` never moves off `0`.
/// What: drives `stop_batch_for_budget` — the loop's ONLY interaction with the
/// budget, so there is no reachable path that stops without recording — past
/// the cap, against the SHARED pool `index_drop_stats` reads. It must return
/// `true`, advance `truncated_batches` by exactly one, and report the age as
/// recent. Deliberately the shared pool and a delta rather than an isolated
/// instance: that is what proves a truncation lands in the counter `GET /health`
/// publishes, and this is the shared truncation counter's only test writer, so
/// no sibling can perturb the delta. That a truncation leaves the DROP counters
/// untouched is pinned absolutely, on an isolated pool, by
/// `a_truncation_is_counted_apart_from_a_rejection`.
/// Test: this test.
#[test]
fn a_truncated_batch_is_counted_separately_from_a_dropped_one() {
    let before = index_drop_stats().truncated_batches;

    assert!(
        stop_batch_for_budget(
            crate::index_dispatch::global(),
            BATCH_INDEX_BUDGET,
            "idx",
            3,
            10
        ),
        "a batch that has spent its budget must be stopped"
    );

    let after = index_drop_stats();
    assert_eq!(
        after.truncated_batches,
        before + 1,
        "stopping on the budget must be counted, not only logged"
    );
    assert!(
        after
            .seconds_since_last_truncation
            .is_some_and(|since| since <= 60),
        "a truncation that just happened must be reported as recent, got {:?}",
        after.seconds_since_last_truncation
    );
}

/// A batch that finishes INSIDE its budget records no truncation at all.
///
/// Why: the negative leg of the counter. Nothing else catches an over-counting
/// regression — recording unconditionally instead of only past the cap — and
/// that failure is worse than the under-counting one it guards the other side
/// of: every healthy batch would report as truncated, so an operator watching
/// `GET /health` learns nothing from a number that is always climbing.
/// What: runs `stop_batch_for_budget` under the cap against a FRESH isolated
/// pool, so the assertions are absolute rather than a delta — `truncated()`
/// exactly `0` and `last_truncation_unix_secs()` exactly `None`, which no
/// ordering against a sibling test can satisfy accidentally. Asserting `== 0`
/// on the process-wide pool would be unpinnable: a sibling increments it.
/// Test: this test.
#[test]
fn an_unexhausted_budget_records_no_truncation() {
    use crate::index_dispatch::BoundedDispatcher;
    use std::time::Duration;

    let pool = BoundedDispatcher::new(1, 1);

    assert!(
        !stop_batch_for_budget(&pool, Duration::from_secs(0), "idx", 0, 10),
        "a batch that has spent none of its budget must not be stopped"
    );
    assert!(
        !stop_batch_for_budget(
            &pool,
            BATCH_INDEX_BUDGET - Duration::from_millis(1),
            "idx",
            9,
            10
        ),
        "a batch one millisecond inside its budget must not be stopped"
    );

    assert_eq!(
        pool.truncated(),
        0,
        "a batch that was never stopped must not be counted as truncated"
    );
    assert_eq!(
        pool.last_truncation_unix_secs(),
        None,
        "with no truncation the stamp must stay unset, never a misleading epoch"
    );
}

/// `relative_index_path` strips the project root prefix so the posted
/// path matches the corpus's existing `file` field convention.
///
/// Why: the reindex walker stores chunk `file` fields relative to the
/// index root; posting an absolute path for an incremental update would
/// create a second, differently-keyed corpus entry for the same file
/// instead of updating the walker's original one.
/// What: builds `root/src/main.rs`, asserts `relative_index_path` returns
/// `"src/main.rs"`.
/// Test: this test.
#[test]
fn relative_index_path_strips_root_prefix() {
    let root = Path::new("/Users/dev/my-project");
    let abs = root.join("src/main.rs");
    assert_eq!(relative_index_path(root, &abs), "src/main.rs");
}

/// `relative_index_path` falls back to the absolute path (lossy) rather
/// than panicking when the candidate does not live under `root`.
///
/// Why: should not happen for a working-directory-scoped tool write, but
/// the fallback must fail safe, not crash the caller's thread.
/// What: passes a path with a different root; asserts the returned string
/// equals the absolute path.
/// Test: this test.
#[test]
fn relative_index_path_falls_back_for_paths_outside_root() {
    let root = Path::new("/Users/dev/my-project");
    let elsewhere = Path::new("/somewhere/else/file.py");
    assert_eq!(
        relative_index_path(root, elsewhere),
        "/somewhere/else/file.py"
    );
}

/// `index_file_request_body` targets exactly `{path, content}` with no
/// extraneous fields — in particular, no `allow_sensitive_path` (the
/// per-file endpoint does not consult the denylist at all; see
/// `index_files_best_effort`'s doc comment).
///
/// Why: pins the wire shape the daemon's `IndexFileRequest`
/// (`crates/trusty-search/src/service/server/router.rs`) expects, and
/// documents — via a negative assertion — the Step 0 finding that this
/// endpoint needs no sensitive-path opt-in.
/// What: builds the body for a relative path + content, asserts both
/// fields round-trip and that no `allow_sensitive_path` key is present.
/// Test: this test.
#[test]
fn index_file_request_body_targets_relative_path_and_content() {
    let body = index_file_request_body("src/main.rs", "fn main() {}\n");
    assert_eq!(
        body.get("path").and_then(serde_json::Value::as_str),
        Some("src/main.rs")
    );
    assert_eq!(
        body.get("content").and_then(serde_json::Value::as_str),
        Some("fn main() {}\n")
    );
    assert!(
        body.get("allow_sensitive_path").is_none(),
        "the per-file endpoint does not re-check the denylist, so no bypass \
         flag should be sent: {body:?}"
    );
}

/// `create_index_request_body`'s `allow_sensitive_path` field always
/// mirrors the caller-supplied parameter, for any root (issue #2914).
///
/// Why: before the fix this field was hardcoded `true` regardless of what
/// the caller wanted — the exact defect that let trusty-mpm's session-launch
/// caller (which never needs the OS-temp-prefix bypass) unconditionally
/// bypass the daemon's `SENSITIVE_PATH_PREFIXES` denylist for every
/// registration, including throwaway `tempfile` fixtures standing in for a
/// workspace in a test. Exercising `create_index_request_body` directly
/// (rather than spawning a thread and standing up a live daemon) keeps this
/// test fast and offline; `ensure_project_indexed_sends_allow_sensitive_path_through_to_create_body`
/// below proves the parameter actually reaches the wire from the public
/// entry point.
/// What: builds the request body for both a plain project root and a
/// `/var/folders/…`-style scratch root, for both `allow_sensitive_path`
/// values, and asserts the field always matches what was passed in — never
/// hardcoded, never path-dependent (the daemon decides what the path means).
/// Test: this test.
#[test]
fn create_index_request_body_respects_allow_sensitive_path_param() {
    for root in [
        Path::new("/Users/dev/projects/my-repo"),
        Path::new("/private/var/folders/xx/scratch-project"),
    ] {
        for allow in [true, false] {
            let body = create_index_request_body(
                "my-index",
                root,
                IndexOptions {
                    allow_sensitive_path: allow,
                    ..IndexOptions::default()
                },
            );
            assert_eq!(
                body.get("allow_sensitive_path"),
                Some(&serde_json::Value::Bool(allow)),
                "request body for root {root:?} must set allow_sensitive_path: {allow}"
            );
            assert_eq!(
                body.get("id").and_then(serde_json::Value::as_str),
                Some("my-index")
            );
        }
    }
}

/// `IndexOptions::skip_vector` reaches the `POST /indexes` wire body, and the
/// two option flags are independent (#5060).
///
/// Why: a worktree index is registered BM25+KG-only by asking the daemon for
/// `skip_vector: true`. If that flag were dropped between [`IndexOptions`] and
/// the request body, every worktree would silently embed again — the exact
/// cost this change exists to avoid, and invisible without an assertion,
/// because the index would still be created and still answer queries. The
/// cross-product also pins that `skip_vector` is not accidentally aliased to
/// `allow_sensitive_path` (the failure mode a second positional `bool` would
/// have invited).
/// What: builds the body for all four `(allow_sensitive_path, skip_vector)`
/// combinations and asserts each field independently equals what was passed.
/// Test: this test.
#[test]
fn create_index_request_body_sets_skip_vector() {
    let root = Path::new("/Users/dev/projects/my-repo/.worktrees/feat-x");
    for allow in [true, false] {
        for skip_vector in [true, false] {
            let body = create_index_request_body(
                "feat-x",
                root,
                IndexOptions {
                    allow_sensitive_path: allow,
                    skip_vector,
                },
            );
            assert_eq!(
                body.get("skip_vector"),
                Some(&serde_json::Value::Bool(skip_vector)),
                "body must set skip_vector: {skip_vector} (allow={allow})"
            );
            assert_eq!(
                body.get("allow_sensitive_path"),
                Some(&serde_json::Value::Bool(allow)),
                "skip_vector must not disturb allow_sensitive_path"
            );
        }
    }
}

/// `IndexOptions::default()` reproduces the pre-#5060 two-argument call.
///
/// Why: [`ensure_project_indexed`] is now a wrapper over
/// [`ensure_project_indexed_with`]. If `IndexOptions`' default ever gained a
/// non-`false` `skip_vector`, every existing caller (session launch, tcode
/// task start) would silently stop embedding — a behaviour change with no
/// visible error. This pins the default as the compatibility contract.
/// What: asserts the body built from `IndexOptions::default()` is identical to
/// one built with both flags explicitly `false`.
/// Test: this test.
#[test]
fn index_options_default_matches_legacy_ensure_call() {
    let root = Path::new("/Users/dev/projects/my-repo");
    assert_eq!(
        create_index_request_body("my-repo", root, IndexOptions::default()),
        create_index_request_body(
            "my-repo",
            root,
            IndexOptions {
                allow_sensitive_path: false,
                skip_vector: false,
            }
        )
    );
}

#[test]
fn index_is_fresh_true_when_recently_indexed_with_chunks() {
    // Why: the whole point of the optimisation is to skip a redundant reindex
    // when the index already has content and was built recently.
    let now = chrono::Utc::now();
    let status = serde_json::json!({
        "chunk_count": 42,
        "last_indexed": now.to_rfc3339(),
    });
    assert!(index_is_fresh(&status));
}

#[test]
fn index_is_fresh_false_when_no_chunks() {
    // Why: a zero-chunk index is empty regardless of how recent `last_indexed`
    // claims to be — it must always be reindexed.
    let now = chrono::Utc::now();
    let status = serde_json::json!({
        "chunk_count": 0,
        "last_indexed": now.to_rfc3339(),
    });
    assert!(!index_is_fresh(&status));
}

#[test]
fn index_is_fresh_false_when_stale() {
    // Why: an index last built more than an hour ago should be refreshed, even
    // though it has chunks.
    let stale = chrono::Utc::now() - chrono::Duration::hours(2);
    let status = serde_json::json!({
        "chunk_count": 10,
        "last_indexed": stale.to_rfc3339(),
    });
    assert!(!index_is_fresh(&status));
}

#[test]
fn index_is_fresh_false_when_last_indexed_missing_or_malformed() {
    // Why: fail-open toward reindexing — a missing or unparsable timestamp
    // must never be treated as "fresh".
    assert!(!index_is_fresh(&serde_json::json!({ "chunk_count": 10 })));
    assert!(!index_is_fresh(&serde_json::json!({
        "chunk_count": 10,
        "last_indexed": "not-a-timestamp",
    })));
    assert!(!index_is_fresh(&serde_json::json!({})));
}

/// The per-file index retry backoff schedule is bounded, capped, and
/// strictly increasing across the small attempt range we actually use.
///
/// Why: issue #2785's retry loop must add only a small, predictable stall
/// to a mid-task write (worst case ~200ms over 3 attempts) and never
/// overflow for a large `attempt`. Pinning the schedule prevents a future
/// edit from silently turning a best-effort retry into a multi-second stall.
/// What: asserts the exact first three delays (50/150/450ms), that they
/// increase, and that a very large attempt saturates to the 1s cap rather
/// than panicking or overflowing.
/// Test: this test.
#[test]
fn retry_backoff_is_bounded_and_increasing() {
    use std::time::Duration;
    assert_eq!(retry_backoff(1), Duration::from_millis(50));
    assert_eq!(retry_backoff(2), Duration::from_millis(150));
    assert_eq!(retry_backoff(3), Duration::from_millis(450));
    assert!(retry_backoff(2) > retry_backoff(1));
    assert!(retry_backoff(3) > retry_backoff(2));
    // Saturating + capped: no panic/overflow, never exceeds 1s.
    assert_eq!(retry_backoff(100), Duration::from_millis(1000));
}

/// Shared driver for the two per-file-retry regression tests below: binds
/// an ephemeral 127.0.0.1 listener, runs `server_fn` on it in a background
/// thread (which reports how many connections it accepted via the given
/// `Sender`), then drives [`post_index_file_with_retries`] against it.
/// Kept as one helper (rather than duplicating the listener/client/join
/// boilerplate per test) so both tests stay under the file's SLOC cap and
/// so their setup can never silently drift apart.
fn drive_retry_test(
    server_fn: impl FnOnce(std::net::TcpListener, std::sync::mpsc::Sender<usize>) + Send + 'static,
) -> (IndexOutcome, usize) {
    use std::net::TcpListener;
    use std::sync::mpsc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || server_fn(listener, tx));

    let client = build_index_client().unwrap();
    let url = format!("http://{addr}/indexes/test-index/index-file");
    let body = index_file_request_body("src/main.rs", "fn main() {}\n");
    let outcome = post_index_file_with_retries(&client, &url, &body);

    let accepted = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("server thread should have reported an accepted-connection count");
    let _ = server.join();
    (outcome, accepted)
}

/// A transient send failure on the per-file index POST is retried and the
/// update ultimately succeeds (issue #2785 regression test).
///
/// Why: this is the exact failure #2785 reports — under rapid repeated
/// writes the per-file HTTP call intermittently fails at the transport
/// layer. Before the fix a single such failure dropped the update; the fix
/// retries transport errors with backoff. We reproduce a transport failure
/// deterministically via [`drive_retry_test`] with a server that drops the
/// FIRST connection (no HTTP response → reqwest `send()` returns `Err`)
/// then answers 200 on the SECOND.
/// What: asserts the outcome is `Indexed` and that exactly two connections
/// were made (one failed attempt + one successful retry).
/// Test: this test.
#[test]
fn post_index_file_retries_transient_send_failure() {
    use std::io::{Read, Write};

    let (outcome, accepted) = drive_retry_test(|listener, tx| {
        let mut accepted = 0usize;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            accepted += 1;
            if accepted == 1 {
                // Transient send failure: accept then close with no
                // response, so the client's send() errors at the
                // transport layer.
                drop(stream);
                continue;
            }
            // Successful retry: consume the request, answer 200.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = stream.flush();
            let _ = tx.send(accepted);
            break;
        }
    });

    // Must recover via retry: Indexed, with exactly 2 connections (1
    // failed attempt + 1 successful retry).
    assert_eq!(outcome, IndexOutcome::Indexed);
    assert_eq!(accepted, 2);
}

/// When every attempt hits a transient send failure, `SendFailed` is
/// reported after exactly [`MAX_INDEX_ATTEMPTS`] attempts — the retry loop
/// terminates and fails open rather than retrying forever or panicking.
///
/// Why: pins the OTHER half of the fail-open contract that
/// `post_index_file_retries_transient_send_failure` does not cover — that
/// path only proves recovery WHEN a retry succeeds. A daemon that stays
/// unreachable/broken for the whole attempt budget must still terminate
/// promptly with `SendFailed`, so callers up the stack (which log-and-swallow)
/// are never left hanging. Code-critic review on PR #2796 flagged this gap.
/// What: via [`drive_retry_test`], with a server that accepts and
/// immediately drops EVERY connection (no HTTP response, so `send()`
/// errors on every attempt); asserts the outcome is `SendFailed` and that
/// exactly [`MAX_INDEX_ATTEMPTS`] connections were accepted (one per
/// attempt, no more, no less).
/// Test: this test.
#[test]
fn post_index_file_exhausts_retries_and_returns_send_failed() {
    let (outcome, accepted) = drive_retry_test(|listener, tx| {
        let mut accepted = 0usize;
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            accepted += 1;
            // Every connection fails transiently: accept then close with
            // no response, so the client's send() errors every time.
            drop(stream);
            if accepted >= MAX_INDEX_ATTEMPTS as usize {
                let _ = tx.send(accepted);
                break;
            }
        }
    });

    // Must fail open with SendFailed after exactly MAX_INDEX_ATTEMPTS
    // attempts — no more, no less.
    assert_eq!(outcome, IndexOutcome::SendFailed);
    assert_eq!(accepted, MAX_INDEX_ATTEMPTS as usize);
}

/// Offer the code under test a REAL, discoverable trusty-search daemon on BOTH
/// transports, run `body`, and report whether either was contacted (#4255,
/// #7237).
///
/// Why: "no write reached the operator's daemon" cannot be proved by pointing
/// discovery at a dead socket — the fail-open path and the guarded path both
/// look identical then. Standing up a daemon that WOULD accept the write is the
/// only arrangement where the guard is the thing making the difference. Both
/// transports are stood up because the two callers use different ones:
/// registration speaks the socket since #7237, the per-file incremental path is
/// still HTTP, and a helper covering one of them would silently stop proving
/// anything about the other.
/// What: binds a mock UDS daemon that counts every call and a `127.0.0.1:0`
/// listener published where `resolve_daemon_base_url("trusty-search")` reads it,
/// points `TRUSTY_SEARCH_SOCKET` at the former, asserts BOTH are discoverable,
/// runs `body`, restores the env, and returns `true` if either was reached.
/// Test: used by the two `never_writes_to_a_daemon_under_test` tests below.
fn daemon_was_contacted_during(body: impl FnOnce()) -> bool {
    use crate::data_dir::{DATA_DIR_OVERRIDE_ENV, ENV_LOCK};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stand-in daemon");
    let addr = listener.local_addr().expect("stand-in daemon local_addr");
    listener
        .set_nonblocking(true)
        .expect("stand-in daemon set_nonblocking");

    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir("4255-daemon");
    fs::create_dir_all(&data_dir).expect("create isolated data dir");
    let socket_dir = tempfile::tempdir().expect("tempdir for the stand-in socket");
    let socket = socket_dir.path().join("s.sock");
    let previous = std::env::var(DATA_DIR_OVERRIDE_ENV).ok();
    // SAFETY: guarded by ENV_LOCK; both vars are restored below before returning.
    unsafe {
        std::env::set_var(DATA_DIR_OVERRIDE_ENV, &data_dir);
        std::env::set_var(crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV, &socket);
    }
    crate::write_daemon_addr("trusty-search", &addr.to_string()).expect("publish daemon addr");
    assert_eq!(
        crate::resolve_daemon_base_url("trusty-search"),
        Some(format!("http://{addr}")),
        "the stand-in HTTP daemon must be discoverable, or this test proves nothing"
    );

    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&calls);
    let daemon = uds_mock::spawn_blocking_at(socket, move |_method, _params| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(serde_json::json!({ "id": "x", "created": true })) })
    });
    let derived = crate::search_rpc::search_socket().ok();
    assert_eq!(
        derived.as_deref(),
        Some(daemon.socket()),
        "the stand-in socket daemon must be discoverable, or this test proves nothing"
    );

    body();

    drop(daemon);
    // SAFETY: still guarded by ENV_LOCK, which is dropped just below.
    unsafe {
        std::env::remove_var(crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
        match previous {
            Some(p) => std::env::set_var(DATA_DIR_OVERRIDE_ENV, p),
            None => std::env::remove_var(DATA_DIR_OVERRIDE_ENV),
        }
    }
    drop(guard);
    let _ = fs::remove_dir_all(&data_dir);

    // A connection the code opened just before returning may still be in the
    // accept queue; give it a moment rather than racing it.
    std::thread::sleep(std::time::Duration::from_millis(250));
    let tcp_contacted = !matches!(
        listener.accept(),
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
    );
    tcp_contacted || calls.load(Ordering::SeqCst) > 0
}

/// Issue #4255: registering a project must not reach a live daemon from a test
/// process — and must still return the derived id.
///
/// Why: this is the defect the ticket reports, in the form it actually
/// occurred: trusty-code's and trusty-mpm's tests call this helper with a
/// `tempfile` fixture root, and every such call registered that throwaway path
/// in whatever real `indexes.toml` the discoverable daemon owned. The dead
/// roots then stalled warm boot. `allow_sensitive_path: true` is passed
/// deliberately — that is tcode's real caller, and the temp-dir denylist (the
/// only prior guard) is switched off on that path, so nothing else stands
/// between the fixture and the operator's registry.
/// What: with a discoverable stand-in daemon on both transports, calls
/// `ensure_project_indexed` on a temp fixture root; asserts nothing was
/// contacted, and — since a suppressed write registers nothing — that no
/// pinnable id came back either (#5091; before that fix this arm asserted the
/// opposite).
/// Test: this test.
#[test]
fn ensure_project_indexed_never_writes_to_a_daemon_under_test() {
    let root = scratch_dir("4255-ensure");
    fs::create_dir_all(&root).expect("create fixture root");
    let mut id = None;

    let contacted = daemon_was_contacted_during(|| {
        id = ensure_project_indexed(&root, true);
    });

    assert!(
        !contacted,
        "ensure_project_indexed contacted a live trusty-search daemon from a test \
         process — that is the issue #4255 registry leak"
    );
    assert!(
        id.is_none(),
        "the guard suppressed the write, so no index was registered — handing \
         back a pinnable id anyway is the #5091 fail-open shape"
    );
    let _ = fs::remove_dir_all(&root);
}

/// Issue #4255: incremental per-file indexing must not reach a live daemon
/// from a test process either.
///
/// Why: `index_files_best_effort` is the other mutating entry point in this
/// module. It does not create registry entries, but it POSTs fixture file
/// content into a real index — corrupting the operator's search results rather
/// than their registry. Guarding only the registration half would leave that
/// open.
/// What: with a discoverable stand-in daemon, calls `index_files_inner`
/// (the synchronous body, so there is no detached thread to race) with one
/// real file; asserts no connection was made.
/// Test: this test.
#[test]
fn index_files_inner_never_writes_to_a_daemon_under_test() {
    let root = scratch_dir("4255-incremental");
    fs::create_dir_all(&root).expect("create fixture root");
    let file = root.join("fixture.rs");
    fs::write(&file, "fn fixture() {}\n").expect("write fixture file");

    let contacted = daemon_was_contacted_during(|| {
        index_files_inner(&root, std::slice::from_ref(&file));
    });

    assert!(
        !contacted,
        "index_files_inner pushed fixture content to a live trusty-search daemon \
         from a test process (issue #4255)"
    );
    let _ = fs::remove_dir_all(&root);
}

/// The `with_*` setters produce exactly what field construction would (#5065
/// review).
///
/// Why: `#[non_exhaustive]` makes the setters the ONLY way another crate can
/// build a non-default `IndexOptions`, so a setter that assigned the wrong
/// field would silently flip an out-of-crate caller's intent — trusty-mpm asks
/// for `skip_vector`, and getting `allow_sensitive_path` instead would both
/// embed every worktree and disarm the denylist. In-crate tests can still use
/// field construction, which is what makes that comparison possible here.
/// What: asserts each setter equals the field-constructed value, and that
/// chaining both sets both.
/// Test: this test.
#[test]
fn index_options_builders_match_field_construction() {
    assert_eq!(
        IndexOptions::default().with_skip_vector(true),
        IndexOptions {
            allow_sensitive_path: false,
            skip_vector: true,
        }
    );
    assert_eq!(
        IndexOptions::default().with_allow_sensitive_path(true),
        IndexOptions {
            allow_sensitive_path: true,
            skip_vector: false,
        }
    );
    assert_eq!(
        IndexOptions::default()
            .with_skip_vector(true)
            .with_allow_sensitive_path(true),
        IndexOptions {
            allow_sensitive_path: true,
            skip_vector: true,
        }
    );
}

// ── #7237: registration speaks the daemon's socket, not `http://…:7878` ──────

use crate::search_rpc::{
    CODE_CONFLICT, METHOD_INDEX_CREATE, METHOD_INDEX_REINDEX, METHOD_INDEX_STATUS,
    METHOD_INDEXES_LIST,
};
use crate::uds_mock::{self, RpcError};
use std::sync::{Arc, Mutex};

/// Every call a mock daemon saw, in order, with the params it was sent.
type CallLog = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

/// A fresh, empty [`CallLog`].
fn call_log() -> CallLog {
    Arc::new(Mutex::new(Vec::new()))
}

/// Snapshot of what a mock daemon has been asked so far.
fn calls(log: &CallLog) -> Vec<(String, serde_json::Value)> {
    log.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The method names a mock daemon has been asked, in order.
fn methods(log: &CallLog) -> Vec<String> {
    calls(log).into_iter().map(|(method, _)| method).collect()
}

/// The params of the `nth` call to `method`, or `None` when it was never made.
fn params_of(log: &CallLog, method: &str, nth: usize) -> Option<serde_json::Value> {
    calls(log)
        .into_iter()
        .filter(|(m, _)| m == method)
        .map(|(_, p)| p)
        .nth(nth)
}

/// A mock handler that records every call and answers through `answer`.
///
/// Why `answer` takes the call's ORDINAL rather than its params: the #6864
/// recovery issues two creates in one flow and they must be answered
/// differently. Passing `(method, nth-call-of-that-method)` is what lets a
/// scripted rig say "refuse the first create, accept the second" without
/// pattern-matching a body.
fn recording(
    log: CallLog,
    answer: impl Fn(&str, usize) -> Result<serde_json::Value, RpcError> + Send + Sync + 'static,
) -> impl Fn(&str, serde_json::Value) -> uds_mock::MockFuture + Send + Sync + 'static {
    move |method, params| {
        let nth = {
            let mut seen = log.lock().unwrap_or_else(|e| e.into_inner());
            seen.push((method.to_string(), params));
            seen.iter().filter(|(m, _)| m == method).count() - 1
        };
        let out = answer(method, nth);
        Box::pin(async move { out })
    }
}

/// What the daemon answers for the two calls the reindex trigger makes.
///
/// A status with no chunks is never fresh, so the trigger always goes on to
/// issue the reindex — which keeps the reindex call visible in every rig's log.
fn reindex_lane(method: &str) -> Result<serde_json::Value, RpcError> {
    if method == METHOD_INDEX_STATUS {
        return Ok(serde_json::json!({ "chunk_count": 0 }));
    }
    if method == METHOD_INDEX_REINDEX {
        return Ok(serde_json::json!({ "queued": true }));
    }
    Err(RpcError::new(
        -32601,
        format!("unexpected method: {method}"),
    ))
}

/// The reply the daemon gives a create it accepted for `root`.
fn created_at(root: &Path) -> serde_json::Value {
    serde_json::json!({
        "id": "whatever",
        "created": false,
        "reason": "already exists",
        "root_path": root.to_string_lossy(),
    })
}

/// Run `body` against a mock trusty-search daemon that production socket
/// discovery will find (#7237).
///
/// Why: the retired rigs published an `http_addr` file and bound a
/// `TcpListener`. There is no address to publish any more, so the rig points
/// `TRUSTY_SEARCH_SOCKET` at a socket under a `TempDir` — deliberately not the
/// data-dir-derived path, whose length blows the ~104-byte `sun_path` budget
/// once a scratch tag and a pid are in it. `ALLOW_PRODUCTION_ENV` is safe here
/// for the same reason it always was: discovery points at this test's OWN
/// daemon, never the operator's.
/// What: locks `ENV_LOCK`, isolates the data dir, binds the mock, creates a
/// git-rooted `<scratch>/<project_name>`, runs `body(&project)`, then stops the
/// daemon, restores the env and removes both scratch trees before returning
/// `body`'s value.
/// Test: used by every socket-daemon test below.
fn with_socket_daemon<T>(
    tag: &str,
    project_name: &str,
    handler: impl Fn(&str, serde_json::Value) -> uds_mock::MockFuture + Send + Sync + 'static,
    body: impl FnOnce(&Path) -> T,
) -> T {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir(&format!("7237-data-{tag}"));
    fs::create_dir_all(&data_dir).unwrap();
    let socket_dir = tempfile::tempdir().expect("tempdir for the mock socket");
    let socket = socket_dir.path().join("s.sock");
    // SAFETY: guarded by ENV_LOCK; all three vars are removed below before
    // returning.
    unsafe {
        std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
        std::env::set_var(crate::test_harness::ALLOW_PRODUCTION_ENV, "1");
        std::env::set_var(crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV, &socket);
    }

    let daemon = uds_mock::spawn_blocking_at(socket, handler);

    let workspace = scratch_dir(&format!("7237-ws-{tag}"));
    let project = workspace.join(project_name);
    fs::create_dir_all(project.join(".git")).unwrap();

    let out = body(&project);

    drop(daemon);
    // SAFETY: still guarded by ENV_LOCK, dropped at the end of this function.
    unsafe {
        std::env::remove_var(crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
        std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        std::env::remove_var(crate::test_harness::ALLOW_PRODUCTION_ENV);
    }
    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&data_dir);
    out
}

/// The registration reaches the daemon as `search.index.create` carrying a BARE
/// `CreateIndexRequest` (#7237).
///
/// Why: this is the whole fix, asserted at the wire. The old path POSTed
/// `http://127.0.0.1:7878/indexes` at a listener ADR-0032 retired, so nothing
/// arrived at all. And the shape matters as much as the transport: the daemon
/// decodes this method's params as a bare `CreateIndexRequest`, so wrapping them
/// in the `{index_id, body}` envelope its INDEX-SCOPED writes use would be
/// refused as `invalid_params` — a failure that looks identical to the one being
/// fixed.
/// What: drives the public reporting entry point against a mock daemon and
/// asserts the first call is the create, that its params carry the four
/// `CreateIndexRequest` fields at the TOP level, that neither envelope key is
/// present, and that the registration came back `Confirmed` with the derived id.
/// Test: this test.
#[test]
fn the_create_call_carries_the_bare_create_index_params() {
    let log = call_log();
    let seen = Arc::clone(&log);

    let (report, expected) = with_socket_daemon(
        "bare-params",
        "wire-project",
        recording(seen, |method, _nth| {
            if method == METHOD_INDEX_CREATE {
                return Ok(serde_json::json!({ "id": "wire-project", "created": true }));
            }
            reindex_lane(method)
        }),
        |project| {
            (
                ensure_project_indexed_reporting(project, IndexOptions::default()),
                crate::derive_index_id(project),
            )
        },
    );

    assert_eq!(
        methods(&log).first().map(String::as_str),
        Some(METHOD_INDEX_CREATE),
        "the first thing the registration does must be the create: {:?}",
        methods(&log)
    );
    let params = params_of(&log, METHOD_INDEX_CREATE, 0).expect("the create must have been sent");
    assert_eq!(
        params.get("id").and_then(serde_json::Value::as_str),
        Some(expected.as_str()),
        "the derived id rides on `id`, at the top level: {params}"
    );
    assert!(
        params
            .get("root_path")
            .is_some_and(serde_json::Value::is_string),
        "`root_path` must be a top-level string: {params}"
    );
    assert!(
        params
            .get("allow_sensitive_path")
            .is_some_and(serde_json::Value::is_boolean),
        "`allow_sensitive_path` must be a top-level bool: {params}"
    );
    assert!(
        params
            .get("skip_vector")
            .is_some_and(serde_json::Value::is_boolean),
        "`skip_vector` must be a top-level bool: {params}"
    );
    assert!(
        params.get("index_id").is_none() && params.get("body").is_none(),
        "the create is a REGISTRY-level write and must not use the index-scoped \
         envelope, which the daemon would refuse as invalid_params: {params}"
    );
    assert_eq!(report.registration, IndexRegistration::Confirmed);
    assert_eq!(report.index_id, Some(expected));
}

/// A confirmed registration goes on to trigger the reindex over the same socket
/// (#1908, #7237).
///
/// Why: `search.index.create` only registers an EMPTY index. Without the
/// follow-up trigger the very first query answers nothing, and the migration
/// would have moved the create while leaving the populate step pointed at a
/// listener that is gone.
/// What: asserts the freshness probe and the reindex trigger both arrive, in
/// that order, naming the registered index.
/// Test: this test.
#[test]
fn a_confirmed_registration_triggers_a_reindex_over_the_socket() {
    let log = call_log();
    let seen = Arc::clone(&log);

    with_socket_daemon(
        "reindex",
        "reindex-project",
        recording(seen, |method, _nth| {
            if method == METHOD_INDEX_CREATE {
                return Ok(serde_json::json!({ "created": true }));
            }
            reindex_lane(method)
        }),
        |project| ensure_project_indexed_reporting(project, IndexOptions::default()),
    );

    assert_eq!(
        methods(&log),
        vec![
            METHOD_INDEX_CREATE.to_string(),
            METHOD_INDEX_STATUS.to_string(),
            METHOD_INDEX_REINDEX.to_string(),
        ],
        "create, then the freshness probe, then the trigger"
    );
    assert_eq!(
        params_of(&log, METHOD_INDEX_REINDEX, 0).and_then(|p| {
            p.get("index_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        }),
        Some("reindex-project".to_string()),
        "the reindex names the index that was just registered"
    );
}

/// End-to-end regression for issue #2914: `ensure_project_indexed`'s
/// `allow_sensitive_path` parameter actually reaches the create request — not
/// just `create_index_request_body` in isolation.
///
/// Why: `create_index_request_body_respects_allow_sensitive_path_param` proves
/// the pure body-builder is correct, but the regression this issue reports
/// happened at the PLUMBING layer — `ensure_project_indexed` forwarding its
/// parameter through `best_effort_create_index` into the body builder. A future
/// edit could silently drop the parameter partway through that chain without the
/// pure-function test catching it. This drives the real public entry point
/// against a daemon and inspects the params that actually arrived.
/// What: for each `allow_sensitive_path` value, runs `ensure_project_indexed`
/// against a mock daemon and asserts the recorded create params'
/// `allow_sensitive_path` equals it.
/// Test: this test.
#[test]
fn ensure_project_indexed_sends_allow_sensitive_path_through_to_create_body() {
    for allow in [true, false] {
        let log = call_log();
        let seen = Arc::clone(&log);

        with_socket_daemon(
            &format!("wire-{allow}"),
            "wire-project",
            recording(seen, |method, _nth| {
                if method == METHOD_INDEX_CREATE {
                    return Ok(serde_json::json!({ "created": true }));
                }
                reindex_lane(method)
            }),
            |project| ensure_project_indexed(project, allow),
        );

        let params =
            params_of(&log, METHOD_INDEX_CREATE, 0).expect("the create must have been sent");
        assert_eq!(
            params.get("allow_sensitive_path"),
            Some(&serde_json::Value::Bool(allow)),
            "the create params must carry allow_sensitive_path={allow} all the way \
             from ensure_project_indexed's parameter; got {params}"
        );
    }
}

/// Regression for #5091: a create the daemon REFUSES must not yield a pinnable
/// index id.
///
/// Why: this is the fail-open shape the ticket names. The create failed — the
/// daemon refused, so no index exists under the derived id — yet
/// `ensure_project_indexed` handed the id back anyway, session launch pinned it
/// into `.mcp.json`, and every later `search` in that session answered
/// `404 unknown index` while `search_health` stayed green. Withholding the id
/// leaves the pin unadvanced, which is the fix; the derived id itself stays
/// reachable through `ensure_project_indexed_reporting` for callers that need it
/// to log or to GC, where the adjacent `registration` field makes ignoring the
/// failure a visible choice rather than the default.
/// What: three arms against a daemon that refuses the create with the internal
/// code — the two id-only entry points must return `None`, and the reporting
/// entry point must still carry the derived id alongside `NotConfirmed`.
/// Test: this test.
#[test]
fn create_rejected_by_the_daemon_withholds_the_pinnable_id() {
    fn refusing(method: &str, _nth: usize) -> Result<serde_json::Value, RpcError> {
        if method == METHOD_INDEX_CREATE {
            return Err(RpcError::internal("the corpus would not open"));
        }
        reindex_lane(method)
    }

    let id = with_socket_daemon(
        "refuse-a",
        "refused",
        recording(call_log(), refusing),
        |p| ensure_project_indexed(p, false),
    );
    assert_eq!(
        id, None,
        "ensure_project_indexed returned a pinnable id after the daemon REFUSED \
         the create — pinning it makes every later search 404 (#5091)"
    );

    let id = with_socket_daemon(
        "refuse-b",
        "refused",
        recording(call_log(), refusing),
        |p| ensure_project_indexed_with(p, IndexOptions::default().with_skip_vector(true)),
    );
    assert_eq!(
        id, None,
        "ensure_project_indexed_with returned a pinnable id after the daemon \
         REFUSED the create — #5091"
    );

    let (report, expected) = with_socket_daemon(
        "refuse-c",
        "refused",
        recording(call_log(), refusing),
        |p| {
            (
                ensure_project_indexed_reporting(p, IndexOptions::default()),
                crate::derive_index_id(p),
            )
        },
    );
    assert_eq!(
        report.registration,
        IndexRegistration::NotConfirmed,
        "a refused create is not a registration"
    );
    assert_eq!(
        report.index_id,
        Some(expected),
        "the derived id stays available for logging and GC — it is the PIN that \
         is withheld, not the id"
    );
}

/// A daemon refusal that is not the conflict code is unrecoverable (#7237).
///
/// Why: [`reconcile::create_and_reconcile`] retries only a conflict. Reading a
/// generic refusal as one would send it hunting the registry for an index that
/// was never the problem, and reading a conflict as a generic refusal is #6864.
/// The split is on the daemon's own code, and this pins it at the
/// `best_effort_create_index` level, where the two arms are adjacent.
/// What: a daemon that answers the internal-error code must yield
/// `NotConfirmed`, and no registry read must follow.
/// Test: this test.
#[test]
fn a_daemon_refusal_is_not_a_registration() {
    let log = call_log();
    let seen = Arc::clone(&log);

    let outcome = with_socket_daemon(
        "refusal-arm",
        "refusal-arm",
        recording(seen, |_method, _nth| {
            Err(RpcError::internal("the embedder never initialised"))
        }),
        |project| {
            let socket = crate::search_rpc::search_socket().expect("derive the socket");
            best_effort_create_index(&socket, "api", project, IndexOptions::default())
        },
    );

    assert_eq!(outcome, CreateOutcome::NotConfirmed);
    assert_eq!(
        methods(&log),
        vec![METHOD_INDEX_CREATE.to_string()],
        "an unrecoverable refusal must not go on to read the registry"
    );
}

/// A reply this client cannot decode is not a registration either (#7237).
///
/// Why: fail-closed is the whole point of #5091 — an id the caller pins must be
/// one the daemon acknowledged. A peer that answers with something that is not a
/// JSON-RPC frame has acknowledged nothing, and the one outcome that must never
/// happen is a panic on a session-launch hot path.
/// What: binds a raw socket that replies with a line of non-JSON, hardened to
/// the same `0700`/`0600` modes `connect_hardened` verifies so the failure under
/// test is the DECODE and not the dial. Asserts `NotConfirmed`.
/// Test: this test.
#[test]
fn a_malformed_create_reply_is_not_a_registration() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir for the garbage daemon");
    let socket = dir.path().join("s.sock");
    let listener =
        std::os::unix::net::UnixListener::bind(&socket).expect("bind the garbage daemon");
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .expect("harden the garbage socket");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("harden the garbage socket directory");

    let server = std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        let _ = reader.read_line(&mut request);
        let mut stream = reader.into_inner();
        let _ = stream.write_all(b"this is not a json-rpc frame\n");
        let _ = stream.flush();
    });

    let root = scratch_dir("7237-garbage");
    fs::create_dir_all(&root).unwrap();
    let outcome = best_effort_create_index(&socket, "api", &root, IndexOptions::default());
    let _ = server.join();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(
        outcome,
        CreateOutcome::NotConfirmed,
        "a reply that cannot be read acknowledges nothing"
    );
}

/// The registration is fail-closed when trusty-search is not running, and it
/// does NOT fall back to the retired loopback port (#7237).
///
/// Why: this is the reported bug's other half. `resolve_daemon_base_url` read
/// `~/.trusty-search/http_addr` — a file an OLD daemon leaves behind and nothing
/// cleans up — so a client that kept HTTP as a fallback would go on dialling a
/// port on a machine where the socket is the only thing serving. Leaving that
/// file in place and pointing it at a listener that WOULD accept is the only
/// arrangement where a fallback shows up as evidence rather than as a silent
/// timeout.
/// What: publishes `http_addr` at a live loopback listener, leaves no socket
/// file, opts out of the #4255 guard, and calls the reporting entry point.
/// Asserts `DaemonUnreachable`, no pinnable id, and ZERO accepts on the port.
/// Test: this test.
#[test]
fn a_missing_socket_registers_nothing_and_contacts_no_tcp_port() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the legacy port");
    let addr = listener.local_addr().expect("legacy port local_addr");
    listener
        .set_nonblocking(true)
        .expect("legacy port set_nonblocking");

    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir("7237-nosocket-data");
    fs::create_dir_all(&data_dir).unwrap();
    // SAFETY: guarded by ENV_LOCK; both vars are removed below before returning.
    unsafe {
        std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
        std::env::set_var(crate::test_harness::ALLOW_PRODUCTION_ENV, "1");
    }
    crate::write_daemon_addr("trusty-search", &addr.to_string()).expect("publish the legacy addr");
    assert_eq!(
        crate::resolve_daemon_base_url("trusty-search"),
        Some(format!("http://{addr}")),
        "the legacy discovery file must resolve, or this test proves nothing"
    );
    let socket = crate::search_rpc::search_socket().expect("derive the socket path");
    assert!(
        !socket.exists(),
        "the rig's premise is that nothing is listening on the socket"
    );

    let project = scratch_dir("7237-nosocket");
    fs::create_dir_all(project.join(".git")).unwrap();
    let report = ensure_project_indexed_reporting(&project, IndexOptions::default());

    // SAFETY: still guarded by ENV_LOCK, dropped at the end of this function.
    unsafe {
        std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        std::env::remove_var(crate::test_harness::ALLOW_PRODUCTION_ENV);
    }
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_dir_all(&data_dir);

    assert_eq!(
        report.registration,
        IndexRegistration::DaemonUnreachable,
        "no socket means nothing was sent — that is not a registration"
    );
    assert_eq!(
        pinnable_index_id(report),
        None,
        "an unconfirmed registration must not advance a caller's pin (#5091)"
    );

    std::thread::sleep(std::time::Duration::from_millis(250));
    assert!(
        matches!(listener.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the registration dialled the retired loopback port — the #7237 fallback \
         that must not exist"
    );
}

// ── Same id, different tree: the find-or-create fail-open ─────────────────────

/// Why: `created: true` means the daemon adopted the root just sent, so there is
/// nothing to cross-check. `created: false` is the one answer where the
/// registered tree can differ from the requested one, so it is the only one that
/// carries a root worth reading.
/// Test: itself.
#[test]
fn registered_root_from_response_reads_the_already_exists_root() {
    let body = serde_json::json!({
        "id": "api", "created": false, "reason": "already exists", "root_path": "/srv/other"
    });
    assert_eq!(
        registered_root_from_response(&body),
        Some("/srv/other".to_string())
    );
}

/// Why: a fresh create needs no cross-check — the daemon took the root it was
/// handed.
/// Test: itself.
#[test]
fn registered_root_from_response_ignores_a_fresh_create() {
    let body = serde_json::json!({ "id": "api", "created": true, "root_path": "/srv/api" });
    assert_eq!(registered_root_from_response(&body), None);
}

/// Why: a daemon too old to report `root_path` must leave the verdict exactly as
/// it was. This check may only strengthen a conclusion it can actually reach —
/// it must never manufacture a failure out of a field that is simply absent, or
/// panic on a malformed one.
/// Test: itself.
#[test]
fn registered_root_from_response_tolerates_a_daemon_that_omits_it() {
    assert_eq!(
        registered_root_from_response(&serde_json::json!({ "id": "api", "created": false })),
        None
    );
    assert_eq!(
        registered_root_from_response(&serde_json::Value::String("not an object".into())),
        None
    );
    assert_eq!(
        registered_root_from_response(&serde_json::Value::Null),
        None
    );
    assert_eq!(
        registered_root_from_response(&serde_json::json!({ "created": false, "root_path": 42 })),
        None,
        "a non-string root_path must yield None, not a panic"
    );
}

/// Regression for the find-or-create fail-open: a successful create naming a
/// DIFFERENT tree is not a registration.
///
/// Why: this is the silent-wrong-answer bug. `best_effort_create_index` read
/// only whether the daemon answered, so "I already have that id, pointed
/// somewhere else" was indistinguishable from "I created what you asked for".
/// The caller pinned the id and every later query was answered from the OTHER
/// checkout, with no error and no warning. An answer that did not do what was
/// asked must not read as confirmation.
/// What: a daemon answering `{created: false}` with a `root_path` that is a
/// real, existing directory OTHER than the one requested, so the comparison runs
/// on `(dev, ino)` rather than falling back to string equality. #6864 renamed
/// the verdict from `NotConfirmed` to `CreateOutcome::Conflict` — the answer is
/// still not a registration, and is now marked as the recoverable kind so the
/// caller looks for the index that does serve this tree.
/// Test: itself.
#[test]
fn create_index_response_for_a_different_tree_reports_a_conflict() {
    let registered = scratch_dir("mismatch-registered");
    fs::create_dir_all(&registered).unwrap();
    let answer = created_at(&registered);

    let outcome = with_socket_daemon(
        "mismatch",
        "mismatch-requested",
        recording(call_log(), move |method, _nth| {
            if method == METHOD_INDEX_CREATE {
                return Ok(answer.clone());
            }
            reindex_lane(method)
        }),
        |project| {
            let socket = crate::search_rpc::search_socket().expect("derive the socket");
            best_effort_create_index(&socket, "api", project, IndexOptions::default())
        },
    );

    assert_eq!(
        outcome,
        CreateOutcome::Conflict { existing_id: None },
        "an answer naming a different tree must not confirm the registration"
    );
    let _ = fs::remove_dir_all(&registered);
}

/// Why: the ordinary case. Every relaunch in the SAME checkout gets
/// `created: false`, and that IS success — the guard must not turn the common
/// path into a failure.
/// Test: itself.
#[test]
fn create_index_response_for_the_same_tree_is_confirmed() {
    let outcome = with_socket_daemon(
        "same-tree",
        "same-tree",
        |method, params| {
            // The rig reflects the requested root back, so the registered and
            // requested trees identify one directory.
            let answer = if method == METHOD_INDEX_CREATE {
                let root = params
                    .get("root_path")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Ok(serde_json::json!({
                    "id": "api", "created": false, "reason": "already exists", "root_path": root
                }))
            } else {
                reindex_lane(method)
            };
            Box::pin(async move { answer })
        },
        |project| {
            let socket = crate::search_rpc::search_socket().expect("derive the socket");
            best_effort_create_index(&socket, "api", project, IndexOptions::default())
        },
    );

    assert_eq!(outcome, CreateOutcome::Confirmed);
}

// ── #6864: a colliding basename resolves to the index serving this root ───────

/// The message trusty-search sends when an id already identifies another tree.
///
/// Mirrors `root_path_mismatch_response` — note it names no owning index,
/// because no index serves the tree that was asked about.
fn root_mismatch_message(index_id: &str, registered: &Path) -> String {
    format!(
        "index '{index_id}' is registered at {:?}; it cannot be re-registered \
         because one index identifies one directory tree",
        registered.display()
    )
}

/// The message trusty-search sends when the requested ROOT already belongs to
/// another index.
///
/// Mirrors `root_path_collision_response`, which names the owning index.
fn root_collision_message(root: &Path, existing_id: &str) -> String {
    format!(
        "root_path {:?} is already registered to index '{existing_id}'; two \
         indexes cannot share one on-disk corpus (issues #2305, #2336)",
        root.display()
    )
}

/// Regression for #6864: a basename already taken by another tree resolves to
/// the index registered at THIS tree.
///
/// Why: this is the reported failure exactly. The daemon held `trusty-tools` for
/// `/Users/masa/Projects/trusty-tools` and refused the checkout's registration;
/// nothing then looked for the index that DID serve the checkout, so the session
/// got `NotConfirmed`, no pin, and every MCP `search` call in it failed with
/// `missing required string field: index_id` — while `trusty-tools-checkout` sat
/// in the same daemon serving the very tree the session was working in.
/// What: two checkouts named `trusty-tools`; the daemon refuses the create with
/// the same-id-different-tree conflict and then reports both indexes on
/// `search.indexes.list`. The report must come back `Confirmed` carrying
/// `trusty-tools-checkout`, the id whose `root_path` IS this project.
/// Test: this test.
#[test]
fn registration_matches_an_existing_index_by_root_path() {
    let other = scratch_dir("6864-other-checkout");
    fs::create_dir_all(&other).unwrap();
    let elsewhere = other.clone();
    let mine: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let recorded = Arc::clone(&mine);

    let report = with_socket_daemon(
        "match",
        "trusty-tools",
        move |method, params| {
            let answer = if method == METHOD_INDEX_CREATE {
                let requested = params
                    .get("root_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                *recorded.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(PathBuf::from(requested));
                Err(RpcError::new(
                    CODE_CONFLICT,
                    root_mismatch_message("trusty-tools", &elsewhere),
                ))
            } else if method == METHOD_INDEXES_LIST {
                let requested = recorded
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_default();
                Ok(serde_json::json!({
                    "indexes": [
                        { "id": "trusty-tools", "root_path": elsewhere.to_string_lossy() },
                        {
                            "id": "trusty-tools-checkout",
                            "root_path": requested.to_string_lossy(),
                        },
                    ]
                }))
            } else {
                reindex_lane(method)
            };
            Box::pin(async move { answer })
        },
        |project| ensure_project_indexed_reporting(project, IndexOptions::default()),
    );

    assert_eq!(
        report.registration,
        IndexRegistration::Confirmed,
        "an index registered at this root IS a confirmed registration (#6864)"
    );
    assert_eq!(
        report.index_id,
        Some("trusty-tools-checkout".to_string()),
        "the report must carry the id that serves this tree, not the colliding \
         basename the daemon refused (#6864)"
    );

    let _ = fs::remove_dir_all(&other);
}

/// Regression for #6864 over the socket: the root-collision refusal names the
/// owning index, and reading it costs no extra call (#7237).
///
/// Why: the HTTP refusal carried `existing_id` as a body field, and the socket's
/// error frame carries only a code and a message — so the id survives the
/// transport only in the daemon's wording. This is the tier that keeps the
/// common recoverable conflict at zero extra requests; the registry scan is the
/// guarantee behind it, and the assertion that NO list call was made is what
/// tells the two apart.
/// What: the daemon refuses the create with `root_path_collision_response`'s
/// wording naming `trusty-tools-checkout`. The report must pin that id, and
/// `search.indexes.list` must never be asked.
/// Test: this test.
#[test]
fn a_root_path_collision_recovers_the_owning_index_without_a_registry_read() {
    let log = call_log();
    let seen = Arc::clone(&log);

    let report = with_socket_daemon(
        "collision",
        "trusty-tools",
        recording(seen, |method, _nth| {
            if method == METHOD_INDEX_CREATE {
                return Err(RpcError::new(
                    CODE_CONFLICT,
                    root_collision_message(
                        Path::new("/nonexistent/checkout/trusty-tools"),
                        "trusty-tools-checkout",
                    ),
                ));
            }
            reindex_lane(method)
        }),
        |project| ensure_project_indexed_reporting(project, IndexOptions::default()),
    );

    assert_eq!(report.registration, IndexRegistration::Confirmed);
    assert_eq!(
        report.index_id,
        Some("trusty-tools-checkout".to_string()),
        "the id the refusal named is the one to pin (#6864)"
    );
    assert!(
        !methods(&log).contains(&METHOD_INDEXES_LIST.to_string()),
        "the refusal already named the owning index, so the registry read is \
         wasted work: {:?}",
        methods(&log)
    );
}

/// Regression for #6864: when nothing serves this tree, the create is retried
/// once under a collision-resistant id.
///
/// Why: the root-path scan answers "which index already serves me"; it cannot
/// answer "register me". A checkout whose basename is taken and which has no
/// index of its own would otherwise stay unregistered forever, because the only
/// id the client ever tried is the one the daemon refuses. Retrying under
/// `derive_checkout_index_id` — the path-digest form #6149 defined for two
/// checkouts of one repository — is what gets it indexed at all.
/// What: same conflict, but the registry names only the OTHER checkout. The
/// client must issue a second create under the digest id and confirm THAT.
/// Test: this test.
#[test]
fn registration_falls_back_to_a_collision_resistant_id() {
    let other = scratch_dir("6864-fallback-other");
    fs::create_dir_all(&other).unwrap();
    let elsewhere = other.clone();
    let log = call_log();
    let seen = Arc::clone(&log);

    let (report, expected) = with_socket_daemon(
        "fallback",
        "trusty-tools",
        recording(seen, move |method, nth| {
            if method == METHOD_INDEX_CREATE {
                if nth == 0 {
                    return Err(RpcError::new(
                        CODE_CONFLICT,
                        root_mismatch_message("trusty-tools", &elsewhere),
                    ));
                }
                return Ok(serde_json::json!({ "created": true }));
            }
            if method == METHOD_INDEXES_LIST {
                return Ok(serde_json::json!({
                    "indexes": [
                        { "id": "trusty-tools", "root_path": elsewhere.to_string_lossy() }
                    ]
                }));
            }
            reindex_lane(method)
        }),
        |project| {
            (
                ensure_project_indexed_reporting(project, IndexOptions::default()),
                crate::derive_checkout_index_id(project),
            )
        },
    );

    assert_eq!(
        report.registration,
        IndexRegistration::Confirmed,
        "the fallback create landed, so the registration is confirmed (#6864)"
    );
    assert_eq!(
        report.index_id, expected,
        "the id must be the shared checkout derivation, not a scheme invented here \
         (#6149 / #6864)"
    );
    assert_eq!(
        calls(&log)
            .iter()
            .filter(|(m, _)| m == METHOD_INDEX_CREATE)
            .count(),
        2,
        "exactly one retry, under the digest id"
    );

    let _ = fs::remove_dir_all(&other);
}
