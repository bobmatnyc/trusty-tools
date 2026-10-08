//! The reindex driver and its health check against a mock daemon socket
//! (#9214).
//!
//! Why: the driver now kicks off over `search.index.reindex` and reads
//! `search.index.reindex.stream`; these pin that a stream ending short of
//! `complete` fails the run (Q5), and that a health check which could not read
//! the index says so instead of calling it broken.
//! What: `mock_daemon_streaming` serves canned kickoff answers and stream items.
//! Test: this file.

use super::super::options::{ReindexOptions, ReindexOutcome};
use super::super::verify::verify_reindex_health;
use super::run_reindex_on;
use crate::commands::mock_socket::{mock_daemon, mock_daemon_streaming, MockDaemon};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use trusty_common::uds::server::RpcError;
use trusty_search::service::rpc::error::CODE_NOT_FOUND;

const STREAM: &str = "search.index.reindex.stream";

/// A daemon whose kickoff succeeds (recording its params) and whose stream
/// sends `items`.
async fn daemon_streaming(
    items: Vec<Result<Value, RpcError>>,
) -> (MockDaemon, Arc<Mutex<Vec<(String, Value)>>>) {
    let calls: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let seen = Arc::clone(&calls);
    let daemon = mock_daemon_streaming(
        move |method, params| {
            seen.lock()
                .expect("calls")
                .push((method.to_string(), params));
            Ok(json!({ "queued": true }))
        },
        STREAM,
        move |_| Ok(items.clone()),
    )
    .await;
    (daemon, calls)
}

fn opts() -> ReindexOptions {
    ReindexOptions {
        stall_secs: 30,
        ..ReindexOptions::default()
    }
}

/// Q5: a stream that ends cleanly before any `complete` event is a failed
/// reindex, not a success.
#[tokio::test]
async fn a_stream_ending_without_complete_is_an_error() {
    let (daemon, calls) =
        daemon_streaming(vec![Ok(json!({ "event": "start", "total_files": 3 }))]).await;

    let err = run_reindex_on(&daemon.client, "idx", std::path::Path::new("/r"), opts())
        .await
        .expect_err("no `complete` arrived")
        .to_string();
    assert!(err.contains("did not complete"), "{err}");
    assert!(err.contains("before `complete`"), "{err}");

    let calls = calls.lock().expect("calls");
    assert_eq!(calls[0].0, "search.index.reindex");
    assert_eq!(calls[0].1["index_id"], "idx");
    assert_eq!(calls[0].1["body"]["root_path"], "/r");
    assert_eq!(calls[0].1["body"]["force"], false);
}

/// Q5: a stream that ends with an error item fails the run and carries it.
#[tokio::test]
async fn a_stream_error_is_an_error() {
    let (daemon, _) = daemon_streaming(vec![
        Ok(json!({ "event": "start", "total_files": 3 })),
        Err(RpcError::internal("embedder died")),
    ])
    .await;

    let err = run_reindex_on(&daemon.client, "idx", std::path::Path::new("/r"), opts())
        .await
        .expect_err("the stream failed")
        .to_string();
    assert!(
        err.contains("did not complete") && err.contains("embedder died"),
        "{err}"
    );
}

/// A `complete` event finishes the run and fills the outcome.
#[tokio::test]
async fn a_complete_event_finishes_the_run() {
    let (daemon, _) = daemon_streaming(vec![
        Ok(json!({ "event": "start", "total_files": 2 })),
        Ok(json!({ "event": "complete", "indexed": 2, "total_chunks": 7, "elapsed_ms": 5 })),
    ])
    .await;

    let outcome: ReindexOutcome =
        run_reindex_on(&daemon.client, "idx", std::path::Path::new("/r"), opts())
            .await
            .expect("a complete stream succeeds");
    assert!(outcome.completed);
    assert_eq!(outcome.total_chunks, 7);
}

/// A kickoff the daemon refuses as `not found` names the index and never
/// opens the stream.
#[tokio::test]
async fn an_unknown_index_kickoff_names_it() {
    let daemon = mock_daemon_streaming(
        |_, _| Err(RpcError::new(CODE_NOT_FOUND, "unknown index: ghost")),
        STREAM,
        |_| panic!("the stream must not open after a refused kickoff"),
    )
    .await;

    let err = run_reindex_on(&daemon.client, "ghost", std::path::Path::new("/r"), opts())
        .await
        .expect_err("unknown index")
        .to_string();
    assert!(err.contains("'ghost' is not registered"), "{err}");
}

/// #9214: a status the health check cannot read is "could not verify", not a
/// 0-chunk unhealthy index.
#[tokio::test]
async fn a_status_that_cannot_be_read_is_not_verified() {
    let daemon = mock_daemon(|_, _| Err(RpcError::internal("corpus read failed"))).await;

    let err = verify_reindex_health(&daemon.client, "idx", &ReindexOutcome::default(), Some(9))
        .await
        .expect_err("an unreadable status cannot verify");
    let text = format!("{err:#}");
    assert!(text.contains("could not verify"), "{text}");
    assert!(text.contains("corpus read failed"), "{text}");
    assert!(!text.contains("unhealthy"), "{text}");
}

/// A populated index whose sanity query hits verifies.
#[tokio::test]
async fn a_healthy_index_verifies() {
    let daemon = mock_daemon(|method, _| match method {
        "search.index.status" => Ok(json!({ "chunk_count": 12 })),
        "search.query" => Ok(json!({ "results": [{ "id": "a:1:2" }] })),
        other => Err(RpcError::internal(format!("unexpected {other}"))),
    })
    .await;

    verify_reindex_health(&daemon.client, "idx", &ReindexOutcome::default(), None)
        .await
        .expect("healthy");
}
