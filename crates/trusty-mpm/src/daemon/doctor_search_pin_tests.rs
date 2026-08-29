//! Unit tests for `daemon::doctor_search_pin` (issue #5045).
//!
//! Why: split into a sibling file so the production module stays under the
//! 500-SLOC cap.
//! What: covers the `.mcp.json` reader against real files, the socket probe
//! against a real daemon (including the not-found refusal that defines this
//! check), and every `build_pin_check` verdict.
//! Test: this module IS the test suite for `super`.

use super::*;

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

/// Serve one fixed `search.index.status` answer on a temp Unix socket (#6285).
///
/// A real socket carrying a real JSON-RPC frame, not a hand-rolled fake: the
/// not-found path is the whole point of this check, and it now depends on
/// [`search_rpc::call_at`] surfacing the daemon's own error code through
/// `anyhow::Error::downcast_ref` — which a fabricated [`PinProbe`] would not
/// exercise at all.
async fn spawn_status_daemon(
    answer: Result<serde_json::Value, crate::uds_mock::RpcError>,
) -> crate::uds_mock::MockUdsDaemon {
    crate::uds_mock::spawn(move |_method: &str, _params: serde_json::Value| {
        let answer = answer.clone();
        Box::pin(async move { answer })
    })
    .await
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

// ------------------------------------------------------------ socket probing

#[tokio::test]
async fn probe_reports_unknown_index_when_the_daemon_says_not_found() {
    // The signature observed live in #5045, as trusty-search now spells it:
    // `search.index.status` refused with `CODE_NOT_FOUND` (#6285).
    let daemon = spawn_status_daemon(Err(crate::uds_mock::RpcError::new(
        crate::daemon::error::CODE_NOT_FOUND,
        "unknown index",
    )))
    .await;
    assert_eq!(
        probe_pinned_index(daemon.socket(), "ghost-index").await,
        PinProbe::UnknownIndex
    );
}

#[tokio::test]
async fn probe_reports_resolved_on_a_status_result() {
    let daemon = spawn_status_daemon(Ok(serde_json::json!({"chunk_count": 4210}))).await;
    assert_eq!(
        probe_pinned_index(daemon.socket(), "trusty-tools").await,
        PinProbe::Resolved {
            chunk_count: Some(4210)
        }
    );
}

/// Why: a refusal that is NOT "no such index" must stay distinguishable — the
/// verdict for it names the code, and folding it into `UnknownIndex` would tell
/// an operator to create an index that already exists.
/// Test: itself.
#[tokio::test]
async fn probe_reports_another_refusal_as_an_rpc_code() {
    let daemon = spawn_status_daemon(Err(crate::uds_mock::RpcError::internal("boom"))).await;
    assert!(matches!(
        probe_pinned_index(daemon.socket(), "trusty-tools").await,
        PinProbe::RpcRefusal(_)
    ));
}

#[tokio::test]
async fn probe_reports_unreachable_when_nothing_listens() {
    // A path under a directory that cannot exist is refused by the kernel
    // immediately, so the probe fails rather than consuming its budget.
    assert!(matches!(
        probe_pinned_index(
            std::path::Path::new("/nonexistent/trusty-search/ts.sock"),
            "trusty-tools"
        )
        .await,
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
fn build_pin_check_fails_on_another_refusal() {
    let check = build_pin_check(
        &PinState::Pinned("trusty-tools".to_string()),
        Some(&PinProbe::RpcRefusal(-32603)),
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
/// Mutates `TRUSTY_SEARCH_SOCKET` (#6285), so it is `#[serial]` against the
/// other env-touching tests in this crate.
#[tokio::test]
#[serial_test::serial]
async fn pinned_but_missing_index_is_fail() {
    let daemon = spawn_status_daemon(Err(crate::uds_mock::RpcError::new(
        crate::daemon::error::CODE_NOT_FOUND,
        "unknown index",
    )))
    .await;

    let project = tempfile::tempdir().unwrap();
    write_mcp_json(
        project.path(),
        &serde_json::json!(["serve", "--index", "2eb72dca-de08-481b-8dfa-22ab7f81b1f9"]),
    );
    let home = tempfile::tempdir().unwrap();

    unsafe {
        std::env::set_var(search_rpc::TRUSTY_SEARCH_SOCKET_ENV, daemon.socket());
    }
    let check = check_search_index_pin(home.path(), Some(project.path())).await;
    unsafe {
        std::env::remove_var(search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
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
