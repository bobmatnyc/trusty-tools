//! `resolve_indexes_for_cwd` against a mock daemon socket (#9214).
//!
//! Why: the resolver reads every resident index's status; these pin that a
//! status it cannot read refuses the lookup instead of being skipped (H1).
//! What: a mock daemon answering `search.indexes.list` and per-id
//! `search.index.status`.
//! Test: this file.

use super::*;
use crate::commands::mock_socket::mock_daemon;
use serde_json::json;
use trusty_common::uds::server::RpcError;

/// A root that exists on disk, canonicalized, as a string.
fn root(dir: &tempfile::TempDir) -> String {
    std::fs::canonicalize(dir.path())
        .expect("canonical root")
        .to_string_lossy()
        .into_owned()
}

/// #9214 H1: an index whose status refuses could be the one covering CWD, so
/// the resolver must fail naming it rather than answer "no match".
#[tokio::test]
async fn an_unreadable_status_refuses_instead_of_matching_nothing() {
    let owner = tempfile::tempdir().expect("owner root");
    let cwd = std::fs::canonicalize(owner.path()).expect("canonical cwd");
    let daemon = mock_daemon(|method, params| match method {
        "search.indexes.list" => Ok(json!({ "indexes": ["owner"] })),
        "search.index.status" if params["index_id"] == "owner" => {
            Err(RpcError::internal("status unavailable"))
        }
        other => Err(RpcError::internal(format!("unexpected {other}"))),
    })
    .await;

    let err = resolve_indexes_for_cwd(&daemon.client, &cwd)
        .await
        .expect_err("an unreadable status must refuse")
        .to_string();
    assert!(
        err.contains("\"owner\""),
        "the refusal names the index: {err}"
    );
}

/// A deleted-since-listed index (`not found`) is skipped; readable ones whose
/// root covers CWD match, broadest first.
#[tokio::test]
async fn a_readable_registry_matches_the_covering_roots() {
    let outer = tempfile::tempdir().expect("outer root");
    let inner_dir = outer.path().join("pkg");
    std::fs::create_dir(&inner_dir).expect("inner root");
    let (outer_root, inner_root) = (
        root(&outer),
        std::fs::canonicalize(&inner_dir)
            .expect("canonical inner")
            .to_string_lossy()
            .into_owned(),
    );
    let cwd = std::path::PathBuf::from(&inner_root);
    let daemon = mock_daemon(move |method, params| match method {
        "search.indexes.list" => Ok(json!({ "indexes": ["inner", "gone", "outer"] })),
        "search.index.status" => match params["index_id"].as_str() {
            Some("inner") => Ok(json!({ "root_path": inner_root })),
            Some("outer") => Ok(json!({ "root_path": outer_root })),
            _ => Err(RpcError::new(
                trusty_search::service::rpc::error::CODE_NOT_FOUND,
                "unknown index",
            )),
        },
        other => Err(RpcError::internal(format!("unexpected {other}"))),
    })
    .await;

    let found = resolve_indexes_for_cwd(&daemon.client, &cwd)
        .await
        .expect("a readable registry resolves");
    let ids: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["outer", "inner"]);
}
