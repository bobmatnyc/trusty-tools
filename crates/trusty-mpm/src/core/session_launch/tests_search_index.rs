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
/// What: points `TRUSTY_DATA_DIR_OVERRIDE` at an isolated data dir, writes a
/// fake daemon's bound address to `<data_dir>/trusty-search/http_addr`
/// (mirroring `resolve_daemon_base_url`'s on-disk discovery contract), calls
/// `register_project_index` with a `tempfile`-backed, git-rooted project
/// path, and asserts the captured request body's `allow_sensitive_path` field
/// is `false`. `#[serial]` because the override env var is process-global.
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
    // harness has to be opted out of. Safe here because the override above
    // points discovery at this test's OWN loopback socket, never the
    // operator's daemon. Restored by the guard's `Drop`.
    let _allow_production =
        EnvVarGuard::set_str(trusty_common::test_harness::ALLOW_PRODUCTION_ENV, "1");

    // Fake daemon: accept one connection, capture the request body, answer
    // 200, then close the listener so the follow-up reindex calls fail fast
    // instead of idling in the kernel's accept backlog.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        drop(listener);
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let _ =
            stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let _ = stream.flush();
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        let _ = tx.send(body);
    });

    let search_data_dir = data_dir.path().join("trusty-search");
    std::fs::create_dir_all(&search_data_dir).unwrap();
    std::fs::write(search_data_dir.join("http_addr"), addr.to_string()).unwrap();

    // A `tempfile`-backed, git-rooted project — the exact shape a
    // session-launch test's workspace fixture takes.
    let project = tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();

    let _ = register_project_index(project.path());

    let body_json: serde_json::Value = serde_json::from_str(
        &rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("fake daemon must have received the create-index POST"),
    )
    .expect("captured body must be valid JSON");
    let _ = server.join();

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
