//! `/api/console/disk/*` reaches trusty-mpm's `disk_survey` MCP tool (#6929).
//!
//! Why: DOC-73 §16.5 puts both Disk routes in the console and says each proxies
//! the #6927 tool over the stdio MCP bridge. The unit tests in `routes::disk`
//! cover the argument shape and the selection, but nothing there proves the
//! whole path — route, capability gate, `tools/call`, content unwrapping — nor
//! that the classification budget the module docs justify actually leaves the
//! process. These cases drive the REAL router against a stub `trusty-mpm` that
//! speaks stdio MCP, which is the same shape `search_uds_bridge.rs` and
//! `memory_uds_bridge.rs` take against a stub socket.
//!
//! What: ONE test. The console resolves `trusty-mpm` with `which`, so the stub
//! is installed by prepending a scratch directory to `PATH` — a process-global
//! mutation, and a single test in its own integration binary is what keeps it
//! from racing a sibling. Every assertion the two routes owe is made inside it.
//! Test: this file IS the test.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_console::server::{AppState, build_router};

/// A `disk_survey` payload shaped exactly as `crate::disk::survey` serializes it.
///
/// The `review` row is the past-budget case the tool emits when the deadline is
/// reached before a worktree is inspected — the reason the console sends a
/// budget at all.
fn survey() -> Value {
    json!({
        "generated_at": "2026-09-07T12:00:00+00:00",
        "keep_list": {
            "patterns": ["*/pinned"],
            "invalid": ["[bad"],
            "error": "disk.keep_list: could not read config.yaml"
        },
        "root": {
            "path": "/w",
            "bytes": 9_000_000_000u64,
            "size": { "from_cache": true, "truncated": false, "measured_at": "2026-09-07T11:59:00+00:00", "unreadable": 0 },
            "counts": { "stale": 1, "review": 1, "keep": 1, "missing": 0 },
            "stale_bytes": 3_000_000_000u64,
            "stale_measured": 1,
            "projects": [
                {
                    "name": "acme/one",
                    "path": "/w/one",
                    "bytes": 6_000_000_000u64,
                    "size": null,
                    "worktrees": [
                        {
                            "id": "/w/one/.worktrees/merged",
                            "path": "/w/one/.worktrees/merged",
                            "branch": "fix/merged",
                            "tier": "stale",
                            "reasons": [{ "code": "merged-pr", "detail": "PR #1 merged" }],
                            "gate": null, "reason": null, "reclaimable": true,
                            "bytes": 3_000_000_000u64, "size": null,
                            "pr": { "number": 1, "state": "merged" }, "session": null
                        },
                        {
                            "id": "/w/one/.worktrees/unbudgeted",
                            "path": "/w/one/.worktrees/unbudgeted",
                            "branch": "feat/late",
                            "tier": "review",
                            "reasons": [{ "code": "unknown-branch-state", "detail": "the survey deadline was reached before this worktree was inspected" }],
                            "gate": "deadline", "reason": "survey deadline reached before inspection",
                            "reclaimable": false, "bytes": null, "size": null,
                            "pr": null, "session": null
                        }
                    ]
                },
                {
                    "name": "acme/two",
                    "path": "/w/two",
                    "bytes": 2_000_000_000u64,
                    "size": null,
                    "worktrees": [
                        {
                            "id": "/w/two/.worktrees/dirty",
                            "path": "/w/two/.worktrees/dirty",
                            "branch": "wip",
                            "tier": "keep",
                            "reasons": [{ "code": "dirty", "detail": "3 uncommitted working-tree entries" }],
                            "gate": "unsaved_work", "reason": "3 uncommitted working-tree entries",
                            "reclaimable": false, "bytes": 2_000_000_000u64, "size": null,
                            "pr": null, "session": null
                        }
                    ]
                }
            ]
        }
    })
}

/// Install a stub `trusty-mpm` on `PATH` and return the request log's path.
///
/// The stub is a POSIX shell script rather than a compiled helper so nothing
/// has to be built for it. It reads NDJSON frames, echoes the request's `id`
/// back, and answers `initialize` / `tools/list` / `tools/call`. The tool
/// result is not inlined: the caller writes the escaped MCP content envelope to
/// a sidecar file the script `cat`s, which keeps a JSON string full of quotes
/// out of shell quoting entirely.
fn install_stub(dir: &std::path::Path) -> std::path::PathBuf {
    let log = dir.join("requests.log");
    // The `tools/call` reply body, minus its `{"jsonrpc":…,"id":N,"result":`
    // prefix. Serializing the survey INTO a JSON string is what makes this a
    // faithful MCP envelope rather than a shortcut through the raw-value
    // fallback in `unwrap_mcp_content`.
    let text = serde_json::to_string(&survey()).expect("survey to text");
    let envelope = json!({ "content": [{ "type": "text", "text": text }] });
    fs::write(
        dir.join("call_result.json"),
        format!(
            "{}}}\n",
            serde_json::to_string(&envelope).expect("envelope")
        ),
    )
    .expect("write call result");

    let script = r#"#!/bin/sh
# Stub trusty-mpm stdio MCP server (#6929 test double).
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$here/requests.log"
  case "$line" in
    *'"method":"notifications/'*) continue ;;
  esac
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"trusty-mpm-stub","version":"0.0.0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      # `console_metrics` is not optional: `ensure_connected` marks the handle
      # Degraded when tools/list omits it, and every route then 503s before it
      # reaches `disk_survey`.
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"console_metrics","description":"stub","inputSchema":{}},{"name":"disk_survey","description":"stub","inputSchema":{}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":' "$id"
      cat "$here/call_result.json"
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
  esac
done
"#;
    let bin = dir.join("trusty-mpm");
    fs::write(&bin, script).expect("write stub");
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("chmod stub");

    let path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: this integration binary holds exactly one test, so no other
    // thread is reading the environment while it is written.
    unsafe {
        std::env::set_var("PATH", format!("{}:{path}", dir.display()));
    }
    log
}

/// Issue one GET against the real router and return its status and JSON body.
async fn get(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = router
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn disk_routes_proxy_the_survey_tool_with_a_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_stub(dir.path());
    let state = AppState::new(Vec::new());

    // ── GET /api/console/disk/tree ───────────────────────────────────────────
    let (status, body) = get(build_router(state.clone()), "/api/console/disk/tree").await;
    assert_eq!(status, StatusCode::OK, "tree route: {body}");
    assert_eq!(
        body,
        survey(),
        "the console reshapes nothing — the view and the tool must agree on every tier"
    );
    // The keep-list state the view renders as a banner and a warning list rides
    // through untouched.
    assert!(body["keep_list"]["error"].as_str().is_some());
    assert_eq!(body["keep_list"]["invalid"], json!(["[bad"]));

    // ── the budget actually left the process ─────────────────────────────────
    let sent = fs::read_to_string(&log).expect("request log");
    let call = sent
        .lines()
        .find(|l| l.contains("\"tools/call\""))
        .expect("no tools/call reached the daemon");
    let call: Value = serde_json::from_str(call).expect("call frame");
    assert_eq!(call["params"]["name"], json!("disk_survey"));
    assert_eq!(
        call["params"]["arguments"]["budget_seconds"],
        json!(20),
        "an unbudgeted survey outruns the 30s stdio CALL_TIMEOUT (#6929 live check)"
    );
    assert!(
        call["params"]["arguments"].get("project").is_none(),
        "the tool's schema is closed — an unscoped survey sends no `project`"
    );

    // A caller override is clamped below the transport timeout, never obeyed
    // verbatim.
    let (status, _) = get(
        build_router(state.clone()),
        "/api/console/disk/tree?budget_seconds=600",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = fs::read_to_string(&log).expect("request log");
    let last: Value = serde_json::from_str(
        sent.lines()
            .rfind(|l| l.contains("\"tools/call\""))
            .expect("call"),
    )
    .expect("call frame");
    assert_eq!(last["params"]["arguments"]["budget_seconds"], json!(25));

    // ── GET /api/console/disk/worktrees/{id} ─────────────────────────────────
    // The id is the worktree's absolute path, so it travels percent-encoded as
    // one segment.
    let (status, body) = get(
        build_router(state.clone()),
        "/api/console/disk/worktrees/%2Fw%2Ftwo%2F.worktrees%2Fdirty",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "worktree route: {body}");
    assert_eq!(body["worktree"]["tier"], json!("keep"));
    assert_eq!(body["worktree"]["gate"], json!("unsaved_work"));
    assert_eq!(body["project"]["name"], json!("acme/two"));
    assert_eq!(body["generated_at"], json!("2026-09-07T12:00:00+00:00"));

    // The past-budget row is reachable too, and still reads `review`.
    let (status, body) = get(
        build_router(state.clone()),
        "/api/console/disk/worktrees/%2Fw%2Fone%2F.worktrees%2Funbudgeted",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "worktree route: {body}");
    assert_eq!(body["worktree"]["tier"], json!("review"));

    // An id nothing in the survey carries is a 404 naming it, not an empty 200.
    let (status, body) = get(
        build_router(state),
        "/api/console/disk/worktrees/%2Fw%2Fgone",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["id"], json!("/w/gone"));
}
