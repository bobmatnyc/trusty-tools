//! Tests for the #11625 lane verbs: the `search.query` body each sends, the
//! fail-closed readiness refusals, and clap parsing.

use std::sync::{Arc, Mutex};

use super::*;
use crate::commands::mock_socket::{mock_daemon, MockDaemon};
use clap::Parser;

/// A stand-in for `Cli` with the same global `-i` and `-v` flags.
#[derive(Parser)]
struct Wrap {
    #[arg(short = 'i', long, global = true)]
    index: Option<String>,
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    verb: LaneVerb,
}

fn try_parse(args: &[&str]) -> Result<LaneVerb, clap::Error> {
    let mut argv = vec!["trusty-search"];
    argv.extend_from_slice(args);
    Wrap::try_parse_from(argv).map(|w| w.verb)
}

fn parse(args: &[&str]) -> LaneVerb {
    try_parse(args).expect("parses")
}

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A daemon whose status answer is `status`; every call is recorded.
async fn daemon(status: Value) -> (MockDaemon, Calls) {
    let calls: Calls = Arc::default();
    let seen = calls.clone();
    let d = mock_daemon(move |m, p| {
        seen.lock().expect("lock").push((m.to_string(), p));
        if m == METHOD_INDEX_STATUS {
            Ok(status.clone())
        } else {
            Ok(json!({"results": []}))
        }
    })
    .await;
    (d, calls)
}

fn ready() -> Value {
    json!({"search_capabilities": ["bm25", "vector", "kg"], "stages": {}})
}

fn query_bodies(calls: &Calls) -> Vec<Value> {
    calls
        .lock()
        .expect("lock")
        .iter()
        .filter(|(m, _)| m == METHOD_QUERY)
        .map(|(_, p)| p["body"].clone())
        .collect()
}

#[test]
fn the_wrapper_command_is_well_formed() {
    use clap::CommandFactory;
    Wrap::command().debug_assert();
}

/// Each verb pins the MCP tool's `stage` and `expand_graph`.
#[tokio::test]
async fn each_lane_verb_pins_its_stage() {
    for (verb, stage, expand) in [
        ("lexical", "lexical", false),
        ("semantic", "semantic", false),
        ("kg", "graph", true),
    ] {
        let (d, calls) = daemon(ready()).await;
        run(&d.client, &parse(&[verb, "find me"]), "demo")
            .await
            .expect("answered");
        let bodies = query_bodies(&calls);
        assert_eq!(bodies.len(), 1, "{verb}");
        assert_eq!(bodies[0]["stage"], stage, "{verb}");
        assert_eq!(bodies[0]["expand_graph"], expand, "{verb}");
        assert_eq!(bodies[0]["text"], "find me", "{verb}");
    }
}

/// `semantic` without `vector` and `kg` without `kg` are refused, and no
/// `search.query` is sent.
#[tokio::test]
async fn a_not_ready_lane_sends_no_query() {
    let lexical_only = json!({
        "search_capabilities": ["bm25"],
        "stages": {"lexical": {"status": "ready"}, "semantic": {"status": "pending"}}
    });
    for verb in ["semantic", "kg"] {
        let (d, calls) = daemon(lexical_only.clone()).await;
        let err = run(&d.client, &parse(&[verb, "x"]), "demo")
            .await
            .expect_err("refused")
            .to_string();
        assert!(err.starts_with("stage not ready"), "{verb}: {err}");
        assert!(query_bodies(&calls).is_empty(), "{verb} sent a query");
    }
    // The KG gap alone refuses `kg` but not `semantic`.
    let no_kg = json!({"search_capabilities": ["bm25", "vector"]});
    let (d, calls) = daemon(no_kg).await;
    assert!(run(&d.client, &parse(&["kg", "x"]), "demo").await.is_err());
    assert!(query_bodies(&calls).is_empty());
}

/// A status with no `search_capabilities` at all is refused, not run.
#[tokio::test]
async fn a_status_without_capabilities_is_refused() {
    let (d, calls) = daemon(json!({"index_id": "demo"})).await;
    let err = run(&d.client, &parse(&["kg", "x"]), "demo")
        .await
        .expect_err("refused")
        .to_string();
    assert!(err.starts_with("stage not ready"), "{err}");
    assert!(query_bodies(&calls).is_empty());
}

/// `--refine-query`, `--path-prefix`, `--top-k` and the rest reach the body.
#[tokio::test]
async fn lane_flags_reach_the_body() {
    let (d, calls) = daemon(ready()).await;
    let verb = parse(&[
        "kg",
        "seed",
        "--refine-query",
        "callers",
        "--path-prefix",
        "src/auth",
        "-k",
        "7",
        "--mode",
        "text",
        "--exclude-archived",
        "--lexical-limit",
        "40",
    ]);
    run(&d.client, &verb, "demo").await.expect("answered");
    let b = &query_bodies(&calls)[0];
    assert_eq!(b["refine_query"], "callers");
    assert_eq!(b["path_prefix"], "src/auth");
    assert_eq!(b["top_k"], 7);
    assert_eq!(b["mode"], "text");
    assert_eq!(b["exclude_archived"], true);
    assert_eq!(b["lexical_limit"], 40);
}

#[test]
fn a_bad_value_is_a_clap_error() {
    for args in [
        &["lexical", "q", "-k", "many"][..],
        &["semantic", "q", "--mode", "prose"][..],
        &["kg"][..],
    ] {
        assert!(try_parse(args).is_err(), "{args:?}");
    }
    // `--refine-query` belongs to `kg` only.
    assert!(try_parse(&["lexical", "q", "--refine-query", "r"]).is_err());
}

#[test]
fn lines_render_one_hit_per_line() {
    let body = json!({"results": [{"file": "a.rs", "start_line": 1, "end_line": 4,
        "score": 0.5, "match_reason": "bm25"}]});
    assert_eq!(lines(&body), Some(vec!["a.rs:1-4  (0.500, bm25)".into()]));
    assert_eq!(lines(&json!({})), None);
}
