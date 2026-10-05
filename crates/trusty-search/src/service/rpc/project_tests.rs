//! `search.project.resolve` over the socket router (#9169).
//!
//! Why: the core is tested in `project_resolve_tests.rs`; these cases prove
//! the wire contract — a real `indexes.toml`, real directories on disk, the
//! JSON a client reads, and a miss whose `data` carries the candidates.
//! Test: this module.

use std::sync::Arc;

use trusty_common::uds::server::{RpcRouter, CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS};

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use crate::service::rpc::error::CODE_NOT_FOUND;
use crate::service::rpc::project;
use crate::service::server::SearchAppState;

const REPO: &str = "bobmatnyc/trusty-tools";

async fn rpc(
    router: &RpcRouter,
    params: serde_json::Value,
) -> trusty_common::uds::server::RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": project::METHOD_PROJECT_RESOLVE, "params": params,
    }))
    .expect("frame");
    router.dispatch(&frame).await
}

/// A main checkout (`.git` dir) and a NEWER linked worktree (`.git` file) of
/// one repo, seeded into `indexes.toml`; only the main checkout is resident.
struct Fixture {
    _tmp: tempfile::TempDir,
    main: std::path::PathBuf,
    worktree: std::path::PathBuf,
    router: RpcRouter,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("trusty-tools");
    std::fs::create_dir_all(main.join(".git")).expect("main checkout");
    let worktree = tmp.path().join("wt-feat");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").expect("linked .git");

    let mut main_row = PersistedIndex::new("trusty-tools-4e2cf878", &main);
    main_row.repo_identity = Some(REPO.to_string());
    main_row.last_indexed_unix = Some(10);
    let mut wt_row = PersistedIndex::new("wt-feat", &worktree);
    wt_row.repo_identity = Some(REPO.to_string());
    wt_row.last_indexed_unix = Some(9_999);
    let toml = tmp.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(&toml, &[main_row, wt_row])
        .expect("seed indexes.toml");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("trusty-tools-4e2cf878".to_string()),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(
            "trusty-tools-4e2cf878",
            &main,
        ))),
        main.clone(),
    ));
    let state = Arc::new(SearchAppState::new(registry).with_registry_path(toml));
    let router = project::register(RpcRouter::new(), &state);
    Fixture {
        _tmp: tmp,
        main,
        worktree,
        router,
    }
}

/// Why: name, `owner/repo` and path all reach the main checkout, and the
/// newer worktree is reported in `duplicates` rather than winning.
/// Test: this test.
#[tokio::test]
async fn resolve_over_the_socket_answers_by_name_identity_and_path() {
    let fx = fixture();
    let in_worktree = fx.worktree.join("src");
    for (project, how) in [
        ("trusty-tools".to_string(), "name"),
        (REPO.to_string(), "repo_identity"),
        (in_worktree.display().to_string(), "path"),
    ] {
        let got = rpc(&fx.router, serde_json::json!({ "project": project }))
            .await
            .result
            .unwrap_or_else(|| panic!("{project} resolves"));
        assert_eq!(got["index_id"], "trusty-tools-4e2cf878", "{project}: {got}");
        assert_eq!(got["root_path"], fx.main.display().to_string());
        assert_eq!(got["repo_identity"], REPO);
        assert_eq!(got["kind"], "main_checkout");
        assert_eq!(got["resident"], true);
        assert_eq!(got["matched_by"], how);
        assert_eq!(got["duplicates"][0]["index_id"], "wt-feat", "{got}");
        assert_eq!(got["duplicates"][0]["kind"], "worktree");
        assert_eq!(got["duplicates"][0]["resident"], false);
    }
}

/// Why: #9169 — a miss is a structured error whose `data` names the nearest
/// candidates; a malformed query is the caller's fault.
/// Test: this test.
#[tokio::test]
async fn a_socket_miss_carries_the_nearest_candidates_as_data() {
    let fx = fixture();
    let err = rpc(&fx.router, serde_json::json!({ "project": "trusty-tool" }))
        .await
        .error
        .expect("a miss is an error");
    assert_eq!(err.code, CODE_NOT_FOUND);
    let data = err.data.expect("the miss carries data");
    assert_eq!(data["error"], "project_not_found");
    assert_eq!(data["project"], "trusty-tool");
    assert_eq!(data["candidates"][0]["index_id"], "trusty-tools-4e2cf878");
    assert_eq!(data["candidates"][1]["index_id"], "wt-feat");

    for bad in [
        serde_json::json!({ "project": "" }),
        serde_json::json!({ "project": "x", "stray": 1 }),
    ] {
        let err = rpc(&fx.router, bad).await.error.expect("refused");
        assert_eq!(err.code, CODE_INVALID_PARAMS);
    }
}

/// Why: an unreadable registry must not read as "no index matches", which
/// would send a caller off to create a duplicate.
/// Test: this test.
#[tokio::test]
async fn an_unreadable_registry_is_an_internal_error_not_a_miss() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let toml = tmp.path().join("indexes.toml");
    std::fs::write(&toml, "this is [[ not toml").expect("garbage");
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()).with_registry_path(toml));
    let router = project::register(RpcRouter::new(), &state);
    let err = rpc(&router, serde_json::json!({ "project": "anything" }))
        .await
        .error
        .expect("refused");
    assert_eq!(err.code, CODE_INTERNAL_ERROR, "{err:?}");
}
