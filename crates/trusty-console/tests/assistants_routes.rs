//! `GET /api/console/assistants` and `/api/console/assistants/{id}/turns`
//! through the real router (#11747).
//!
//! Why: the Trusty Assistants page reads both routes; these cases prove the
//! listing rule `ta list` applies, the turn tail, and the refusals.
//! What: a temp assistants root per case, seeded with fake instance homes and
//! turn logs, and a `build_router` pointed at it through
//! `AppState::with_assistants_root`.
//! Test: this file IS the test.

use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_console::server::{AppState, build_router};

async fn get(root: &Path, uri: &str) -> (StatusCode, Value) {
    let router = build_router(AppState::new(vec![]).with_assistants_root(root.to_path_buf()));
    let resp = router
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("{uri}: not JSON ({e}): {}", String::from_utf8_lossy(&bytes)));
    (status, body)
}

/// An instance home: a directory holding `instructions.md`.
fn instance(root: &Path, id: &str) {
    let home = root.join(id);
    std::fs::create_dir_all(&home).expect("mkdir");
    std::fs::write(home.join("instructions.md"), "be helpful\n").expect("write");
}

fn turn(n: usize) -> String {
    format!(r#"{{"v":1,"at":"2026-11-23T00:00:{n:02}Z","question":"q{n}","answer":"a{n}"}}"#)
}

fn write_log(root: &Path, id: &str, body: &str) {
    let stores = root.join(id).join("stores");
    std::fs::create_dir_all(&stores).expect("mkdir");
    std::fs::write(stores.join("ask-turns.jsonl"), body).expect("write");
}

/// AC1: exactly what `ta list` names; non-instance entries are absent.
#[tokio::test]
async fn list_names_only_instance_homes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    instance(root, "izzie");
    instance(root, "bob");
    std::fs::create_dir_all(root.join("OKG")).expect("mkdir");
    std::fs::write(root.join("OKG").join("instructions.md"), "x").expect("write");
    std::fs::create_dir_all(root.join("bob-kb")).expect("mkdir");
    std::fs::write(root.join("memory-bindings.lock"), "").expect("write");
    std::fs::write(root.join("README.md"), "# root\n").expect("write");
    // A directory whose `instructions.md` is itself a directory is no home.
    std::fs::create_dir_all(root.join("hollow").join("instructions.md")).expect("mkdir");

    let (status, body) = get(root, "/api/console/assistants").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "instances": [{ "id": "bob" }, { "id": "izzie" }] })
    );
}

#[tokio::test]
async fn list_on_a_missing_root_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (status, body) = get(&dir.path().join("absent"), "/api/console/assistants").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "instances": [] }));
}

/// AC2: more than N records returns N — the newest, oldest first.
#[tokio::test]
async fn turns_returns_the_last_n_oldest_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    instance(dir.path(), "izzie");
    let log: String = (1..=5).map(|n| turn(n) + "\n").collect();
    write_log(dir.path(), "izzie", &log);

    let (status, body) = get(dir.path(), "/api/console/assistants/izzie/turns?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "instance": "izzie",
            "order": "oldest_first",
            "limit": 2,
            "total": 5,
            "skipped": 0,
            "truncated": false,
            "turns": [
                { "at": "2026-11-23T00:00:04Z", "question": "q4", "answer": "a4" },
                { "at": "2026-11-23T00:00:05Z", "question": "q5", "answer": "a5" },
            ],
        })
    );

    let (status, body) = get(dir.path(), "/api/console/assistants/izzie/turns").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["limit"], 20);
    assert_eq!(body["turns"].as_array().map(Vec::len), Some(5));

    let (status, body) = get(dir.path(), "/api/console/assistants/izzie/turns?limit=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn turns_on_a_missing_log_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    instance(dir.path(), "izzie");
    let (status, body) = get(dir.path(), "/api/console/assistants/izzie/turns").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["turns"], json!([]));
    assert_eq!(
        (body["total"].clone(), body["skipped"].clone()),
        (json!(0), json!(0))
    );
}

/// Fail-open check: a bad line is skipped and counted, never a 500 or silent.
#[tokio::test]
async fn turns_counts_a_malformed_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    instance(dir.path(), "izzie");
    write_log(
        dir.path(),
        "izzie",
        &format!("{}\n{{\"v\":1,\"at\":\n{}\n", turn(1), turn(2)),
    );
    let (status, body) = get(dir.path(), "/api/console/assistants/izzie/turns").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["skipped"], 1);
    assert_eq!(body["turns"].as_array().map(Vec::len), Some(2));
}

/// AC3: an unknown id is 404 naming the available instances.
#[tokio::test]
async fn turns_for_an_unknown_id_is_404_with_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    instance(dir.path(), "izzie");
    instance(dir.path(), "bob");
    std::fs::create_dir_all(dir.path().join("bob-kb")).expect("mkdir");
    for id in ["ghost", "bob-kb"] {
        let (status, body) = get(dir.path(), &format!("/api/console/assistants/{id}/turns")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body["id"], id);
        assert_eq!(body["available"], json!(["bob", "izzie"]));
        assert!(body["error"].as_str().is_some_and(|e| e.contains(id)));
    }
}

/// Traversal and invalid ids are 400 before any filesystem access: the root
/// here is a regular file, so any read of it would answer 500, not 400.
#[tokio::test]
async fn turns_refuses_traversal_before_the_filesystem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("not-a-directory");
    std::fs::write(&root, "").expect("write");
    for id in ["..", "%2E%2E", "..%2Fetc", "a%2Fb", "Izzie", "a%20b", "%00"] {
        let (status, body) = get(&root, &format!("/api/console/assistants/{id}/turns")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{id}: {body}");
        assert!(body["error"].is_string(), "{id}: {body}");
    }
    // The control: a valid id does reach the filesystem, and fails there.
    let (status, _) = get(&root, "/api/console/assistants/izzie/turns").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// #11747: a log larger than the 4 MiB window answers its newest records,
/// says it was truncated, and counts only whole lines inside the window — the
/// partial first line is dropped, not skipped.
#[tokio::test]
async fn turns_reads_only_the_tail_window() {
    let dir = tempfile::tempdir().expect("tempdir");
    instance(dir.path(), "izzie");
    // ~1 KiB lines; 5000 of them is ~5 MiB, past the 4 MiB window, and the
    // window edge lands inside a line because the width does not divide it.
    let pad = "x".repeat(1000);
    let log: String = (0..5000)
        .map(|n| format!(r#"{{"v":1,"at":"t{n}","question":"{pad}","answer":"a{n}"}}"#) + "\n")
        .collect();
    assert!(log.len() > 4 * 1024 * 1024);
    write_log(dir.path(), "izzie", &log);

    let (status, body) = get(dir.path(), "/api/console/assistants/izzie/turns?limit=2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["truncated"], true);
    assert_eq!(
        body["skipped"], 0,
        "the window's partial first line is not a bad line"
    );
    let total = body["total"].as_u64().expect("total");
    assert!(
        total < 5000 && total > 3000,
        "only the window is counted: {total}"
    );
    let ats: Vec<_> = body["turns"]
        .as_array()
        .expect("turns")
        .iter()
        .map(|t| t["at"].clone())
        .collect();
    assert_eq!(ats, [json!("t4998"), json!("t4999")]);
}

/// #11747: a FIFO at the log path must not hang the request.
#[cfg(unix)]
#[tokio::test]
async fn turns_on_a_fifo_returns_promptly() {
    let dir = tempfile::tempdir().expect("tempdir");
    instance(dir.path(), "izzie");
    let stores = dir.path().join("izzie").join("stores");
    std::fs::create_dir_all(&stores).expect("mkdir");
    let fifo = std::ffi::CString::new(
        stores
            .join("ask-turns.jsonl")
            .into_os_string()
            .into_encoded_bytes(),
    )
    .expect("path");
    // SAFETY: `fifo` is a valid NUL-terminated path for the call's duration.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "mkfifo");

    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        get(dir.path(), "/api/console/assistants/izzie/turns"),
    )
    .await;
    let Ok((status, body)) = answer else {
        // Release a reader stuck on the blocking pool, so the runtime's drop
        // does not hang the suite, then fail.
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .open(stores.join("ask-turns.jsonl")),
        );
        panic!("a FIFO log must not hang the request");
    };
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(
        body["error"],
        "cannot read the turn log: the turn log is not a regular file"
    );
}

/// #11747: a symlink to a directory or a device is refused, not read, and the
/// body names no path.
#[cfg(unix)]
#[tokio::test]
async fn turns_refuses_a_non_regular_log() {
    for target in ["dir", "/dev/null"] {
        let dir = tempfile::tempdir().expect("tempdir");
        instance(dir.path(), "izzie");
        let stores = dir.path().join("izzie").join("stores");
        std::fs::create_dir_all(&stores).expect("mkdir");
        let target = if target == "dir" {
            let d = dir.path().join("elsewhere");
            std::fs::create_dir_all(&d).expect("mkdir");
            d
        } else {
            std::path::PathBuf::from(target)
        };
        std::os::unix::fs::symlink(&target, stores.join("ask-turns.jsonl")).expect("symlink");

        let (status, body) = get(dir.path(), "/api/console/assistants/izzie/turns").await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{target:?}: {body}"
        );
        let error = body["error"].as_str().expect("error");
        assert_eq!(
            error,
            "cannot read the turn log: the turn log is not a regular file"
        );
        assert!(!error.contains('/'), "the body names no path: {error}");
    }
}
