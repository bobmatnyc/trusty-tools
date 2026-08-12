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
//! `allow_sensitive_path` plumbing (both the pure body-builder and the live-HTTP
//! wire-body regression), the incremental per-file index-update helpers, the
//! freshness predicate, and the per-file retry/backoff schedule.
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
/// empty data dir then guarantees `resolve_daemon_base_url` finds no address
/// file, so no HTTP request is ever built and the operator's real daemon is
/// never touched despite the opt-in.
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
    // Derivation yields an empty id for the filesystem root, so the helper
    // returns None without touching the daemon.
    assert_eq!(ensure_project_indexed(Path::new("/"), true), None);
    assert_eq!(ensure_project_indexed(Path::new("/"), false), None);
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
/// budget, so there is no reachable path that stops without recording — first
/// under the cap, then past it. Under the cap it must return `false` and leave
/// the counter alone; past it, it must return `true`, advance
/// `truncated_batches` by exactly one, and report the age as recent. Both
/// halves live in one test because the counter is process-wide and this is its
/// only writer, so a sibling test can never perturb the delta. That the
/// truncation does not touch the DROP counters is pinned deterministically on
/// an isolated pool by `a_truncation_is_counted_apart_from_a_rejection`.
/// Test: this test.
#[test]
fn a_truncated_batch_is_counted_separately_from_a_dropped_one() {
    use std::time::Duration;

    let before = index_drop_stats().truncated_batches;

    assert!(
        !stop_batch_for_budget(Duration::from_secs(0), "idx", 0, 10),
        "a batch inside its budget must not be stopped"
    );
    assert_eq!(
        index_drop_stats().truncated_batches,
        before,
        "a batch that was never stopped must not be counted as truncated"
    );

    assert!(
        stop_batch_for_budget(BATCH_INDEX_BUDGET, "idx", 3, 10),
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

/// End-to-end regression for issue #2914: `ensure_project_indexed`'s
/// `allow_sensitive_path` parameter actually reaches the `POST /indexes`
/// wire body — not just `create_index_request_body` in isolation.
///
/// Why: `create_index_request_body_respects_allow_sensitive_path_param`
/// proves the pure body-builder is correct, but the actual regression this
/// issue reports happened at the PLUMBING layer — `ensure_project_indexed`
/// forwarding its parameter through `best_effort_create_index` into the
/// body builder. A future edit could silently drop the parameter partway
/// through that chain (e.g. hardcode `true` back into
/// `best_effort_create_index`'s call) without this pure-function test
/// catching it. This test drives the real public entry point against a
/// bound TCP listener standing in for the trusty-search daemon and
/// inspects the actual bytes sent over the wire.
/// What: for each `allow_sensitive_path` value, binds an ephemeral
/// listener, writes its address to the isolated `TRUSTY_DATA_DIR_OVERRIDE`
/// data dir's `trusty-search/http_addr` file (mirroring
/// `resolve_daemon_base_url`'s discovery contract), calls
/// `ensure_project_indexed(project, allow)`, and asserts the captured
/// `POST /indexes` body's `allow_sensitive_path` field equals `allow`.
/// `ENV_LOCK` serialises against sibling env-mutating tests in this module,
/// matching `ensure_project_indexed_returns_derived_id_when_daemon_down`.
/// Test: this test.
#[test]
fn ensure_project_indexed_sends_allow_sensitive_path_through_to_create_body() {
    for allow in [true, false] {
        let _guard = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let data_dir = scratch_dir(&format!("wire-{allow}"));
        fs::create_dir_all(&data_dir).unwrap();
        // SAFETY: guarded by ENV_LOCK; removed below before returning.
        unsafe {
            std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
            // #4255: this is the ONE test that genuinely needs the daemon-write
            // path — asserting on the wire body requires the POST to actually
            // happen. Opting in explicitly is safe here because the "daemon"
            // is this test's own loopback socket, not the operator's: the
            // override above points discovery at it. Every other caller stays
            // guarded.
            std::env::set_var(crate::test_harness::ALLOW_PRODUCTION_ENV, "1");
        }

        // Fake daemon: accept one connection, capture the request body,
        // answer 200 so `best_effort_create_index` logs success (not that
        // it matters — the assertion is on the captured body).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            // Close the listening socket immediately after accepting the
            // ONE connection this test cares about (the create-index
            // POST), so `ensure_project_indexed`'s follow-up
            // `best_effort_trigger_reindex` calls hit a fast
            // connection-refused instead of idling in the kernel's accept
            // backlog until their own multi-second timeouts elapse.
            drop(listener);
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = stream.flush();
            let body = request.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let _ = tx.send(body);
        });

        // Mirrors `write_daemon_addr("trusty-search", ..)`'s on-disk
        // discovery contract so `resolve_daemon_base_url` finds this fake
        // daemon instead of reporting "not discoverable".
        let search_data_dir = data_dir.join("trusty-search");
        fs::create_dir_all(&search_data_dir).unwrap();
        fs::write(search_data_dir.join("http_addr"), addr.to_string()).unwrap();

        let project = scratch_dir(&format!("wire-project-{allow}"));
        fs::create_dir_all(project.join(".git")).unwrap();

        let _ = ensure_project_indexed(&project, allow);

        let body_json: serde_json::Value = serde_json::from_str(
            &rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("fake daemon must have received the create-index POST"),
        )
        .expect("captured body must be valid JSON");
        let _ = server.join();

        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
            std::env::remove_var(crate::test_harness::ALLOW_PRODUCTION_ENV);
        }
        let _ = fs::remove_dir_all(&project);
        let _ = fs::remove_dir_all(&data_dir);

        assert_eq!(
            body_json.get("allow_sensitive_path"),
            Some(&serde_json::Value::Bool(allow)),
            "POST /indexes body must carry allow_sensitive_path={allow} \
             all the way from ensure_project_indexed's parameter; got {body_json:?}"
        );
    }
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

/// Offer the code under test a REAL, discoverable trusty-search daemon, run
/// `body`, and report whether anything connected to it (issue #4255).
///
/// Why: "no write reached the operator's daemon" cannot be proved by pointing
/// discovery at a dead port — the fail-open path and the guarded path both
/// look identical then. Standing up a socket that WOULD accept the write is
/// the only arrangement where the guard is the thing making the difference.
/// What: binds `127.0.0.1:0`, publishes that address where
/// `resolve_daemon_base_url("trusty-search")` reads it (via an isolated
/// `DATA_DIR_OVERRIDE_ENV` data dir, serialised on `ENV_LOCK` like every other
/// env-mutating test here), asserts discovery actually finds it, runs `body`,
/// restores the env, and returns `true` if a connection arrived.
/// Test: used by the two `never_writes_to_a_daemon_under_test` tests below.
fn daemon_was_contacted_during(body: impl FnOnce()) -> bool {
    use crate::data_dir::{DATA_DIR_OVERRIDE_ENV, ENV_LOCK};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stand-in daemon");
    let addr = listener.local_addr().expect("stand-in daemon local_addr");
    listener
        .set_nonblocking(true)
        .expect("stand-in daemon set_nonblocking");

    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir("4255-daemon");
    fs::create_dir_all(&data_dir).expect("create isolated data dir");
    let previous = std::env::var(DATA_DIR_OVERRIDE_ENV).ok();
    unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &data_dir) };
    crate::write_daemon_addr("trusty-search", &addr.to_string()).expect("publish daemon addr");
    assert_eq!(
        crate::resolve_daemon_base_url("trusty-search"),
        Some(format!("http://{addr}")),
        "the stand-in daemon must be discoverable, or this test proves nothing"
    );

    body();

    match previous {
        Some(p) => unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, p) },
        None => unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) },
    }
    drop(guard);
    let _ = fs::remove_dir_all(&data_dir);

    // A connection the code opened just before returning may still be in the
    // accept queue; give it a moment rather than racing it.
    std::thread::sleep(std::time::Duration::from_millis(250));
    !matches!(
        listener.accept(),
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
    )
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
/// What: with a discoverable stand-in daemon, calls `ensure_project_indexed`
/// on a temp fixture root; asserts no connection was made, and — since a
/// suppressed write registers nothing — that no pinnable id came back either
/// (#5091; before that fix this arm asserted the opposite).
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

/// Stand up a one-shot fake trusty-search daemon answering with `status_line`.
///
/// Why: the #5091 error arm is only reachable against a daemon that is
/// REACHABLE and REFUSES — pointing discovery at a dead port lands in
/// `DaemonUnreachable`, a different branch. Accepting exactly one connection and
/// then dropping the listener also makes the follow-up reindex probes fail fast
/// with connection-refused instead of idling in the accept backlog until their
/// own multi-second timeouts elapse (the trick
/// `ensure_project_indexed_sends_allow_sensitive_path_through_to_create_body`
/// already uses).
/// What: binds `127.0.0.1:0`, spawns a thread that accepts ONE connection, reads
/// the request, replies `status_line` with an empty body, and exits. Returns the
/// bound address and the thread handle.
/// Test: used by `create_rejected_by_the_daemon_withholds_the_pinnable_id`.
fn one_shot_daemon(
    status_line: &'static str,
) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake daemon");
    let addr = listener.local_addr().expect("fake daemon local_addr");
    let handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        drop(listener);
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(
            format!("{status_line}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").as_bytes(),
        );
        let _ = stream.flush();
    });
    (addr, handle)
}

/// Publish `addr` where `resolve_daemon_base_url("trusty-search")` will find it.
///
/// Why: mirrors `write_daemon_addr`'s on-disk discovery contract so a test's own
/// loopback socket stands in for the daemon.
/// What: creates `<data_dir>/trusty-search/` and writes `http_addr`.
/// Test: used by `create_rejected_by_the_daemon_withholds_the_pinnable_id`.
fn publish_daemon_addr(data_dir: &Path, addr: std::net::SocketAddr) {
    let search_data_dir = data_dir.join("trusty-search");
    fs::create_dir_all(&search_data_dir).unwrap();
    fs::write(search_data_dir.join("http_addr"), addr.to_string()).unwrap();
}

/// Run `body` with discovery pointed at a fake daemon that answers `status_line`.
///
/// Why: the three arms of the #5091 regression need the identical arrangement —
/// `ENV_LOCK`, an isolated data dir, the #4255 opt-out, a fake daemon, and a
/// git-rooted project — and repeating it three times is where a missed env
/// restore leaks into a sibling serial test.
/// What: locks `ENV_LOCK`, points `DATA_DIR_OVERRIDE_ENV` at a scratch dir, sets
/// `ALLOW_PRODUCTION_ENV=1` (safe: discovery points at this test's OWN loopback
/// socket, never the operator's daemon), creates a git-rooted scratch project,
/// runs `body(&project)`, then restores the env and removes both scratch dirs
/// before returning `body`'s value.
/// Test: used by `create_rejected_by_the_daemon_withholds_the_pinnable_id`.
fn with_refusing_daemon<T>(
    tag: &str,
    status_line: &'static str,
    body: impl FnOnce(&Path) -> T,
) -> T {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data_dir = scratch_dir(&format!("5091-data-{tag}"));
    fs::create_dir_all(&data_dir).unwrap();
    // SAFETY: guarded by ENV_LOCK; both vars are removed below before returning.
    unsafe {
        std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
        std::env::set_var(crate::test_harness::ALLOW_PRODUCTION_ENV, "1");
    }

    let (addr, server) = one_shot_daemon(status_line);
    publish_daemon_addr(&data_dir, addr);

    let project = scratch_dir(&format!("5091-project-{tag}"));
    fs::create_dir_all(project.join(".git")).unwrap();

    let out = body(&project);

    let _ = server.join();
    unsafe {
        std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        std::env::remove_var(crate::test_harness::ALLOW_PRODUCTION_ENV);
    }
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_dir_all(&data_dir);
    out
}

/// Regression for #5091: a `POST /indexes` the daemon REFUSES must not yield a
/// pinnable index id.
///
/// Why: this is the fail-open shape the ticket names. The create failed — the
/// daemon answered 500, so no index exists under the derived id — yet
/// `ensure_project_indexed` handed the id back anyway, session launch pinned it
/// into `.mcp.json`, and every later `search` in that session answered
/// `404 unknown index` while `search_health` stayed green. Withholding the id
/// leaves the pin unadvanced, which is the fix; the derived id itself stays
/// reachable through `ensure_project_indexed_reporting` for callers that need it
/// to log or to GC, where the adjacent `registration` field makes ignoring the
/// failure a visible choice rather than the default.
/// What: three arms against a fake daemon that 500s the create — the two id-only
/// entry points must return `None`, and the reporting entry point must still
/// carry the derived id alongside `NotConfirmed`.
/// Test: this test.
#[test]
fn create_rejected_by_the_daemon_withholds_the_pinnable_id() {
    let refused = "HTTP/1.1 500 Internal Server Error";

    let id = with_refusing_daemon("ensure", refused, |project| {
        ensure_project_indexed(project, false)
    });
    assert_eq!(
        id, None,
        "ensure_project_indexed returned a pinnable id after the daemon REFUSED \
         the create (HTTP 500) — pinning it makes every later search 404 (#5091)"
    );

    let id = with_refusing_daemon("ensure-with", refused, |project| {
        ensure_project_indexed_with(project, IndexOptions::default().with_skip_vector(true))
    });
    assert_eq!(
        id, None,
        "ensure_project_indexed_with returned a pinnable id after the daemon \
         REFUSED the create (HTTP 500) — #5091"
    );

    let (report, expected) = with_refusing_daemon("report", refused, |project| {
        (
            ensure_project_indexed_reporting(project, IndexOptions::default()),
            crate::derive_index_id(project),
        )
    });
    assert_eq!(
        report.registration,
        IndexRegistration::NotConfirmed,
        "a 500 on the create is not a registration"
    );
    assert_eq!(
        report.index_id,
        Some(expected),
        "the derived id stays available for logging and GC — it is the PIN that \
         is withheld, not the id"
    );
}
