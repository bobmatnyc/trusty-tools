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

#[test]
fn ensure_project_indexed_returns_derived_id_when_daemon_down() {
    // Why (#1373): the helper must derive the project's index id (git-root
    // basename, via `derive_index_id`) AND stay graceful when the
    // trusty-search daemon is unreachable — it still returns the id so the
    // caller can pin it. We force the daemon-down path by pointing the data
    // dir at an empty temp dir so `resolve_daemon_base_url` finds no address
    // file (and thus issues no HTTP POST). `ENV_LOCK` serialises the
    // process-global override against sibling env-mutating tests (the same
    // guard `daemon_addr`/`data_dir` tests use).
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

    let id = ensure_project_indexed(&nested, true);
    let expected = crate::derive_index_id(&project);

    unsafe {
        std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
    }
    let _ = fs::remove_dir_all(&project);
    let _ = fs::remove_dir_all(&data_dir);

    assert_eq!(id, Some(expected), "id is the git-root basename");
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
            let body = create_index_request_body("my-index", root, allow);
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
