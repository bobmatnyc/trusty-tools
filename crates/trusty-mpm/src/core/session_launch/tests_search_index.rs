//! Issue #2914 coverage: `register_project_index` must never bypass the
//! trusty-search daemon's sensitive-path denylist.
//!
//! Why: split out of `tests.rs` (mirroring the `tests_roster.rs` /
//! `tests_scaffold_gitignore.rs` split pattern already used in this crate) to
//! keep `tests.rs` under the 1500-SLOC test-file cap after this regression
//! test was added.
//! What: `register_project_index_never_bypasses_sensitive_path_denylist`
//! drives the real `register_project_index` entry point against a bound TCP
//! listener standing in for the trusty-search daemon and inspects the actual
//! `POST /indexes` wire body, proving `allow_sensitive_path` is always
//! `false` for this caller — see `crates/trusty-common/src/search_index.rs`
//! for the shared-helper half of this fix.
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;
use tempfile::tempdir;

/// Regression for issue #2914 (ephemeral test/self-heal indexes leaking into
/// the production trusty-search index set): `register_project_index` must
/// NEVER set `allow_sensitive_path: true` on its `POST /indexes` call.
///
/// Why: trusty-mpm's session-launch pipeline is exactly the code path that
/// produced the incident's `*-selfheal-ws`/`*-stale-heal-ws` ephemeral
/// registrations — session-launch tests across this crate stand a
/// `tempfile`-backed directory in for `project_root` (a real session
/// workspace is always the user's checked-out repo or a `.worktrees/<uuid>`
/// leaf inside it, never an OS-temp path, but a TEST FIXTURE for one often
/// lives under `/var/folders/…`/`/tmp`). Before this fix,
/// `register_project_index` unconditionally forwarded
/// `allow_sensitive_path: true` to the shared
/// `trusty_common::search_index::ensure_project_indexed` helper, bypassing the
/// daemon's `SENSITIVE_PATH_PREFIXES` denylist and letting exactly such a
/// fixture register against whatever trusty-search daemon happened to be
/// discoverable on the developer/CI machine. This test drives the REAL
/// `register_project_index` entry point against a bound TCP listener standing
/// in for the daemon and inspects the actual `POST /indexes` wire body, so a
/// future regression that re-hardcodes (or silently drops) the parameter
/// threaded through `ensure_project_indexed` is caught here, not just in
/// `trusty_common::search_index`'s own unit tests.
/// What: points `TRUSTY_DATA_DIR_OVERRIDE` at an isolated data dir and
/// `TRUSTY_SEARCH_SOCKET` at a mock daemon, calls `register_project_index` with
/// a `tempfile`-backed, git-rooted project path, and asserts the captured
/// create params' `allow_sensitive_path` field is `false`. `#[serial]` because
/// the override env vars are process-global.
/// Test: this test.
#[test]
#[serial_test::serial]
fn register_project_index_never_bypasses_sensitive_path_denylist() {
    let data_dir = tempdir().unwrap();
    let _env = EnvVarGuard::set(
        trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV,
        data_dir.path(),
    );
    // #4255: this test asserts on the wire body, so the POST must actually
    // happen — the guard that otherwise suppresses daemon writes under a test
    // harness has to be opted out of. Safe here because the override below
    // points discovery at this test's OWN socket, never the operator's daemon.
    // Restored by the guard's `Drop`.
    let _allow_production =
        EnvVarGuard::set_str(trusty_common::test_harness::ALLOW_PRODUCTION_ENV, "1");

    // Fake daemon on a Unix socket (#7237): record the create params, answer
    // every method `{}` so the follow-up status/reindex calls complete.
    let socket_dir = tempdir().unwrap();
    let socket = socket_dir.path().join("s.sock");
    let seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&seen);
    let _daemon = crate::uds_mock::spawn_blocking_at(socket.clone(), move |method, params| {
        if method == trusty_common::search_rpc::METHOD_INDEX_CREATE {
            log.lock().unwrap_or_else(|e| e.into_inner()).push(params);
        }
        Box::pin(async move { Ok(serde_json::json!({})) })
    });
    let _socket_env = EnvVarGuard::set_str(
        trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV,
        &socket.to_string_lossy(),
    );

    // A `tempfile`-backed, git-rooted project — the exact shape a
    // session-launch test's workspace fixture takes.
    let project = tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();

    let _ = register_project_index(project.path());

    let body_json = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .first()
        .cloned()
        .expect("the fake daemon must have received the create call");

    assert_eq!(
        body_json.get("allow_sensitive_path"),
        Some(&serde_json::Value::Bool(false)),
        "register_project_index must never set allow_sensitive_path: true; got {body_json:?}"
    );
}

/// A worktree root asks for `skip_vector` at LAUNCH too (#5065 review).
///
/// Why: #5060 registers a worktree BM25+KG-only at creation, but `POST
/// /indexes` is find-or-create and short-circuits on an existing id, so
/// whichever call lands first decides. Session launch reached the same worktree
/// path with `skip_vector` at its `false` default — so any time creation-time
/// indexing failed, was skipped, or lost the race, launch minted a
/// vector-bearing index and the daemon persisted it. The invariant the PR title
/// claims only holds if BOTH call sites decide from the path.
/// What: builds a real worktree-shaped root (`.git` is a FILE) and asserts the
/// launch-side decision is `true`.
/// Test: this test.
#[test]
fn worktree_skip_vector_true_for_worktree_root() {
    let tmp = tempdir().unwrap();
    let wt = tmp.path().join("feat-branch");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: /elsewhere/.git/worktrees/feat").unwrap();

    assert!(
        super::search_index::worktree_skip_vector(&wt),
        "a worktree keeps its BM25+KG-only registration at launch"
    );
}

/// Launching from a SUBDIRECTORY of a worktree still decides `true`.
///
/// Why: this is why the check runs against `resolve_project_root(project_root)`
/// rather than `project_root` itself. The index is keyed to the git-root, so a
/// session launched from `<worktree>/crates/foo` registers the WORKTREE's
/// index — but a naive `is_git_worktree(project_root)` answers `false` for that
/// path (no `.git` file in a subdirectory) and would re-register the worktree's
/// index with the vector lane on. Same drift, narrower door.
/// Test: this test.
#[test]
fn worktree_skip_vector_true_from_worktree_subdirectory() {
    let tmp = tempdir().unwrap();
    let wt = tmp.path().join("feat-branch");
    let nested = wt.join("crates").join("inner");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: /elsewhere/.git/worktrees/feat").unwrap();

    assert!(
        super::search_index::worktree_skip_vector(&nested),
        "the decision must follow the git-root the index is keyed to, not the cwd"
    );
}

/// A plain clone keeps the vector lane.
///
/// Why: the BM25+KG-only ruling is about worktrees specifically — the expensive
/// embedding lane is built ONCE, on the base checkout. Suppressing it there too
/// would delete semantic search for the primary repo, which is the opposite of
/// what #5060 decided.
/// Test: this test.
#[test]
fn worktree_skip_vector_false_for_plain_clone() {
    let tmp = tempdir().unwrap();
    let clone = tmp.path().join("my-repo");
    std::fs::create_dir_all(clone.join(".git")).unwrap();

    assert!(
        !super::search_index::worktree_skip_vector(&clone),
        "a primary clone still embeds — skip_vector is a worktree-only decision"
    );
}

/// Why: a test process must never reach the operator's daemon through the
/// launch-time `search.project.ensure` (#4255 posture, #10531).
/// Test: this test.
#[test]
fn ensure_via_daemon_is_silent_under_the_test_harness() {
    let dir = tempdir().expect("tempdir");
    let registered = trusty_common::RegisteredProject::new("demo", "o/demo");
    assert_eq!(
        super::search_index::ensure_via_daemon(dir.path(), &registered),
        None
    );
}

/// Run `ensure_and_refresh` against a mock daemon; return the id and the
/// methods the daemon saw.
async fn launch_against(created: bool, status: serde_json::Value) -> (Option<String>, Vec<String>) {
    use crate::uds_mock::{self, MockFuture};
    use std::sync::{Arc, Mutex};
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let rec = Arc::clone(&seen);
    let daemon = uds_mock::spawn(move |method, _| -> MockFuture {
        rec.lock().expect("lock").push(method.to_string());
        let answer = match method {
            "search.project.ensure" => {
                serde_json::json!({ "index_id": "demo", "created": created })
            }
            "search.index.status" => status.clone(),
            _ => serde_json::json!({ "queued": true }),
        };
        Box::pin(async move { Ok(answer) })
    })
    .await;
    let socket = daemon.socket().to_path_buf();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let id = tokio::task::spawn_blocking(move || {
        let registered = trusty_common::RegisteredProject::new("demo", "o/demo");
        super::search_index::ensure_and_refresh(&socket, &root, &registered)
    })
    .await
    .expect("worker");
    let calls = seen.lock().expect("lock").clone();
    (id, calls)
}

/// Why: a found index that is empty (or whose first queue failed) must still be
/// populated by launch, as the older client-side path did (#10531 review).
/// Test: this test. Before the fix `ensure_and_refresh` returned at once and no
/// `search.index.reindex` was sent.
#[tokio::test]
async fn a_found_but_empty_index_gets_a_reindex_on_launch() {
    let (id, calls) = launch_against(false, serde_json::json!({ "chunk_count": 0 })).await;
    assert_eq!(id.as_deref(), Some("demo"));
    assert!(
        calls.contains(&"search.index.reindex".to_string()),
        "{calls:?}"
    );
}

/// Why: a stale found index (indexed over an hour ago) is refreshed on launch.
/// Test: this test.
#[tokio::test]
async fn a_found_stale_index_gets_a_reindex_on_launch() {
    let stale = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
    let (_, calls) = launch_against(
        false,
        serde_json::json!({ "chunk_count": 9, "last_indexed": stale }),
    )
    .await;
    assert!(
        calls.contains(&"search.index.reindex".to_string()),
        "{calls:?}"
    );
}

/// Why: a fresh index must not be reindexed on every launch.
/// Test: this test.
#[tokio::test]
async fn a_found_fresh_index_gets_no_reindex_on_launch() {
    let fresh = chrono::Utc::now().to_rfc3339();
    let (_, calls) = launch_against(
        false,
        serde_json::json!({ "chunk_count": 9, "last_indexed": fresh }),
    )
    .await;
    assert!(
        !calls.contains(&"search.index.reindex".to_string()),
        "{calls:?}"
    );
}

/// Why: ensure already queued the first reindex for a created index.
/// Test: this test.
#[tokio::test]
async fn a_created_index_gets_no_second_reindex_on_launch() {
    let (_, calls) = launch_against(true, serde_json::json!({ "chunk_count": 0 })).await;
    assert_eq!(calls, vec!["search.project.ensure".to_string()]);
}
