//! The registry sweep — [`registrations`] and [`resident_statuses`] — against
//! a mock daemon socket (#9214).
//!
//! Why: every CWD- and PATH-based target lookup reads the registry through
//! these two. A broken list must not read as "nothing registered" (that
//! cleared local rows as stale, H5), and an unreadable status must not be
//! skipped (H1).
//! Test: this file.

use super::*;
use crate::commands::explicit_target::lookup_index_by_path;
use crate::commands::mock_socket::mock_daemon;
use trusty_common::uds::server::RpcError;
use trusty_search::service::rpc::error::CODE_NOT_FOUND;

/// #9214 H5: a list answer with no `indexes` array is an error, and the
/// PATH lookup `index remove` runs before clearing stale rows propagates it
/// rather than answering "not registered".
#[tokio::test]
async fn a_list_without_an_indexes_array_is_an_error() {
    let daemon = mock_daemon(|method, _| match method {
        "search.indexes.list" => Ok(json!({ "error": "half-started" })),
        other => Err(RpcError::internal(format!("unexpected {other}"))),
    })
    .await;

    let err = registrations(&daemon.client)
        .await
        .expect_err("no `indexes` array")
        .to_string();
    assert!(err.contains("`indexes`"), "{err}");

    let dir = tempfile::tempdir().expect("scratch root");
    let lookup = lookup_index_by_path(&daemon.client, dir.path()).await;
    assert!(
        lookup.is_err(),
        "a broken list must not read as `not registered`: {lookup:?}"
    );
}

/// The parked rows are optional; when present they are read with their root.
#[tokio::test]
async fn a_list_reads_resident_ids_and_parked_rows() {
    let daemon = mock_daemon(|_, _| {
        Ok(json!({
            "indexes": ["a"],
            "parked": [{ "id": "p", "root_path": "/p" }],
        }))
    })
    .await;
    let regs = registrations(&daemon.client).await.expect("list");
    assert_eq!(regs.resident, ["a"]);
    assert_eq!(regs.parked, [("p".to_string(), PathBuf::from("/p"))]);
}

/// #9214 H1: `not found` is skipped (deleted since the list); every other
/// failure, and a body with no `root_path`, is unreadable and refuses.
#[tokio::test]
async fn a_failed_status_is_unreadable_and_a_missing_one_is_skipped() {
    let daemon = mock_daemon(|_, params| match params["index_id"].as_str() {
        Some("ok") => Ok(json!({ "root_path": "/ok" })),
        Some("gone") => Err(RpcError::new(CODE_NOT_FOUND, "unknown index")),
        Some("rootless") => Ok(json!({ "chunk_count": 1 })),
        _ => Err(RpcError::internal("status chaos")),
    })
    .await;
    let ids = ["ok", "gone", "rootless", "broken"]
        .map(String::from)
        .to_vec();

    let sweep = resident_statuses(&daemon.client, ids).await;
    let read: Vec<&str> = sweep.read.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(read, ["ok"]);
    let unreadable: Vec<&str> = sweep.unreadable.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(unreadable, ["rootless", "broken"]);

    let err = sweep
        .require_all("refusing to guess")
        .expect_err("unreadable statuses refuse")
        .to_string();
    assert!(
        err.contains("\"rootless\"") && err.contains("\"broken\"") && err.contains("status chaos"),
        "{err}"
    );
    assert!(err.ends_with("refusing to guess"), "{err}");
}
