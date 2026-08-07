//! Unit tests for `daemon::doctor_search_pin` (issue #5045).
//!
//! Why: split into a sibling file so the production module stays under the
//! 500-SLOC cap.
//! What: covers the `.mcp.json` reader against real files, the HTTP probe
//! against a real socket (including the 404 that defines this check), and
//! every `build_pin_check` verdict.
//! Test: this module IS the test suite for `super`.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Write a `.mcp.json` carrying a trusty-search stub with `args`.
fn write_mcp_json(project: &Path, args: &serde_json::Value) {
    let body = serde_json::json!({
        "mcpServers": {
            "trusty-memory": {"type": "stdio", "command": "trusty-memory", "args": ["serve"]},
            "trusty-search": {"type": "stdio", "command": "trusty-search", "args": args},
        }
    });
    std::fs::write(
        project.join(".mcp.json"),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();
}

/// Serve one fixed raw HTTP response on a loopback port; returns its base URL.
///
/// A real socket, not a mock: the 404 path is the whole point of this check,
/// and asserting it against a hand-rolled fake response type would prove
/// nothing about how `reqwest` classifies a real `404 Not Found`.
async fn spawn_stub(response: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    format!("http://{addr}")
}

// ---------------------------------------------------------------- pin reader

#[test]
fn read_pin_finds_the_pinned_id() {
    // The shape `session_launch::search_index::trusty_search_mcp_value` writes.
    let tmp = tempfile::tempdir().unwrap();
    write_mcp_json(
        tmp.path(),
        &serde_json::json!(["serve", "--index", "trusty-tools"]),
    );
    assert_eq!(
        read_pinned_index_id(tmp.path()),
        PinState::Pinned("trusty-tools".to_string())
    );
}

#[test]
fn read_pin_reports_unpinned_stub() {
    // The pre-#1373 stub: tools present, no `--index`.
    let tmp = tempfile::tempdir().unwrap();
    write_mcp_json(tmp.path(), &serde_json::json!(["serve"]));
    assert_eq!(read_pinned_index_id(tmp.path()), PinState::Unpinned);
}

#[test]
fn read_pin_treats_a_blank_id_as_unpinned() {
    // `--index ""` pins nothing; reporting it as a pinned id would send the
    // probe after an empty URL segment.
    let tmp = tempfile::tempdir().unwrap();
    write_mcp_json(tmp.path(), &serde_json::json!(["serve", "--index", "  "]));
    assert_eq!(read_pinned_index_id(tmp.path()), PinState::Unpinned);
}

#[test]
fn read_pin_reports_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(read_pinned_index_id(tmp.path()), PinState::NoMcpJson);
}

#[test]
fn read_pin_reports_missing_server() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".mcp.json"),
        r#"{"mcpServers":{"trusty-memory":{"args":["serve"]}}}"#,
    )
    .unwrap();
    assert_eq!(read_pinned_index_id(tmp.path()), PinState::NoSearchServer);
}

#[test]
fn read_pin_reports_unreadable_json() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".mcp.json"), "{not json").unwrap();
    assert!(matches!(
        read_pinned_index_id(tmp.path()),
        PinState::Unreadable(_)
    ));
}

// -------------------------------------------------------------- http probing

#[tokio::test]
async fn probe_reports_unknown_index_on_404() {
    // The signature observed live in #5045:
    // `POST /indexes/<id>/search → 404 {"error":"unknown index"}`.
    let base = spawn_stub(
        "HTTP/1.1 404 Not Found\r\nContent-Length: 27\r\nConnection: close\r\n\r\n\
         {\"error\":\"unknown index\"}",
    )
    .await;
    assert_eq!(
        probe_pinned_index(&base, "ghost-index").await,
        PinProbe::UnknownIndex
    );
}

#[tokio::test]
async fn probe_reports_resolved_on_200() {
    let base = spawn_stub(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 21\r\n\
         Connection: close\r\n\r\n{\"chunk_count\": 4210}",
    )
    .await;
    assert_eq!(
        probe_pinned_index(&base, "trusty-tools").await,
        PinProbe::Resolved {
            chunk_count: Some(4210)
        }
    );
}

#[tokio::test]
async fn probe_reports_unreachable_when_nothing_listens() {
    // Port 0 never accepts a connection.
    assert!(matches!(
        probe_pinned_index("http://127.0.0.1:0", "trusty-tools").await,
        PinProbe::Unreachable(_)
    ));
}

// ------------------------------------------------------------------ verdicts

#[test]
fn build_pin_check_fails_on_unknown_index() {
    // THE regression this check exists for (#5045): the pin advanced because
    // index registration is fail-open, and nothing else in the report notices.
    let check = build_pin_check(
        &PinState::Pinned("trusty-tools".to_string()),
        Some(&PinProbe::UnknownIndex),
    );
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.message.contains("trusty-tools"));
    assert!(
        check.message.contains("#5045"),
        "the failure must cite the issue: {}",
        check.message
    );
}

#[test]
fn build_pin_check_warns_on_empty_index() {
    let check = build_pin_check(
        &PinState::Pinned("trusty-tools".to_string()),
        Some(&PinProbe::Resolved {
            chunk_count: Some(0),
        }),
    );
    assert_eq!(check.status, CheckStatus::Warn);
}

#[test]
fn build_pin_check_ok_when_index_resolves() {
    let check = build_pin_check(
        &PinState::Pinned("trusty-tools".to_string()),
        Some(&PinProbe::Resolved {
            chunk_count: Some(4210),
        }),
    );
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn build_pin_check_unreachable_is_unknown_not_ok() {
    // An unanswered probe establishes nothing about the pin — reporting Ok
    // here would rebuild the fail-open this check was added to catch.
    let check = build_pin_check(
        &PinState::Pinned("trusty-tools".to_string()),
        Some(&PinProbe::Unreachable("connection refused".to_string())),
    );
    assert_eq!(check.status, CheckStatus::Unknown);
}

#[test]
fn build_pin_check_fails_on_other_non_2xx() {
    let check = build_pin_check(
        &PinState::Pinned("trusty-tools".to_string()),
        Some(&PinProbe::HttpStatus(500)),
    );
    assert_eq!(check.status, CheckStatus::Fail);
}

#[test]
fn build_pin_check_warns_on_unpinned_stub() {
    let check = build_pin_check(&PinState::Unpinned, None);
    assert_eq!(check.status, CheckStatus::Warn);
}

#[test]
fn build_pin_check_warns_on_unreadable_mcp_json() {
    let check = build_pin_check(&PinState::Unreadable("bad".to_string()), None);
    assert_eq!(check.status, CheckStatus::Warn);
}

#[test]
fn build_pin_check_ok_when_nothing_is_pinned() {
    assert_eq!(
        build_pin_check(&PinState::NoMcpJson, None).status,
        CheckStatus::Ok
    );
    assert_eq!(
        build_pin_check(&PinState::NoSearchServer, None).status,
        CheckStatus::Ok
    );
}

// --------------------------------------------------------------- end-to-end

/// The whole path, exactly as issue #5045 asks: a session whose `.mcp.json`
/// pins an index the daemon does not have must report a FAILURE.
///
/// Mutates `TRUSTY_SEARCH_ADDR`, so it is `#[serial]` against the other
/// env-touching tests in this crate.
#[tokio::test]
#[serial_test::serial]
async fn pinned_but_missing_index_is_fail() {
    let base = spawn_stub(
        "HTTP/1.1 404 Not Found\r\nContent-Length: 27\r\nConnection: close\r\n\r\n\
         {\"error\":\"unknown index\"}",
    )
    .await;
    let addr = base.trim_start_matches("http://").to_string();

    let project = tempfile::tempdir().unwrap();
    write_mcp_json(
        project.path(),
        &serde_json::json!(["serve", "--index", "2eb72dca-de08-481b-8dfa-22ab7f81b1f9"]),
    );
    let home = tempfile::tempdir().unwrap();

    unsafe {
        std::env::set_var("TRUSTY_SEARCH_ADDR", &addr);
    }
    let check = check_search_index_pin(home.path(), Some(project.path())).await;
    unsafe {
        std::env::remove_var("TRUSTY_SEARCH_ADDR");
    }

    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "a pinned-but-nonexistent index must FAIL, not report healthy: {}",
        check.message
    );
    assert!(
        check
            .message
            .contains("2eb72dca-de08-481b-8dfa-22ab7f81b1f9")
    );
}

#[tokio::test]
async fn unscoped_run_warns_rather_than_reporting_ok() {
    // `tm doctor` outside a project must not claim a pin it never looked at is
    // fine.
    let home = tempfile::tempdir().unwrap();
    let check = check_search_index_pin(home.path(), None).await;
    assert_eq!(check.status, CheckStatus::Warn);
}
