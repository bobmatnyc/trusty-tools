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
