//! `tm session disk` end-to-end against a stub `POST /rpc` daemon (#7313).
//!
//! Why: the unit tests in `commands::session_disk_tests` pin the fold, the
//! order and the error arms as pure functions; none of them proves that the
//! BINARY reaches the daemon, sends `group_by: "session"`, or puts the report
//! on stdout and its diagnostics on stderr. That wiring is what an operator
//! actually runs, and it is exactly what a unit test cannot see.
//! What: serves one axum route at `/rpc` that answers `tools/call` with a fixed
//! `disk_survey` payload, points the binary at it with `TRUSTY_MPM_URL`
//! (`--url`'s env form, which every resolver honours ahead of the lock file and
//! the gateway probe), and asserts the JSON schema, the table, and the
//! unknown-session error. The stub also RECORDS the arguments it was called
//! with, so the `group_by` contract is asserted rather than assumed.
//! Test: this file; run with
//! `cargo test -p trusty-mpm --test tm_session_disk_cli`.

use std::future::IntoFuture;
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

/// The survey the stub daemon answers with: two sessions plus the unattributed
/// bucket, and one session owning worktrees under two different projects.
fn survey_payload() -> Value {
    json!({
        "generated_at": "2026-09-10T00:00:00Z",
        "partial": false,
        "keep_list": { "patterns": [], "invalid": [] },
        "by_session": [
            {
                "session_id": "trusty-tools-95",
                "bytes": 30_000_000_000u64,
                "build_dir_bytes": 24_000_000_000u64,
                "worktree_count": 2,
                "tiers": { "stale": 1, "review": 1, "keep": 0, "missing": 0 },
                "worktree_paths": [
                    "/w/bobmatnyc/trusty-tools/.claude/worktrees/a",
                    "/w/bobmatnyc/other/.claude/worktrees/c"
                ]
            },
            {
                "session_id": null,
                "bytes": 5_000_000_000u64,
                "build_dir_bytes": 4_000_000_000u64,
                "worktree_count": 1,
                "tiers": { "stale": 1, "review": 0, "keep": 0, "missing": 0 },
                "worktree_paths": ["/w/bobmatnyc/trusty-tools/.claude/worktrees/z"]
            }
        ],
        "root": {
            "path": "/w",
            "bytes": 35_000_000_000u64,
            "counts": { "stale": 2, "review": 1, "keep": 0, "missing": 0 },
            "stale_bytes": 5_000_000_000u64,
            "stale_measured": 2,
            "projects": [
                {
                    "name": "bobmatnyc/trusty-tools",
                    "path": "/w/bobmatnyc/trusty-tools",
                    "bytes": 25_000_000_000u64,
                    "worktrees": [
                        {
                            "path": "/w/bobmatnyc/trusty-tools/.claude/worktrees/a",
                            "tier": "review",
                            "bytes": 20_000_000_000u64,
                            "build_dir_bytes": 18_000_000_000u64,
                            "owning_session": "trusty-tools-95"
                        },
                        {
                            "path": "/w/bobmatnyc/trusty-tools/.claude/worktrees/z",
                            "tier": "stale",
                            "bytes": 5_000_000_000u64,
                            "build_dir_bytes": 4_000_000_000u64,
                            "owning_session": null
                        }
                    ]
                },
                {
                    "name": "bobmatnyc/other",
                    "path": "/w/bobmatnyc/other",
                    "bytes": 10_000_000_000u64,
                    "worktrees": [
                        {
                            "path": "/w/bobmatnyc/other/.claude/worktrees/c",
                            "tier": "stale",
                            "bytes": 10_000_000_000u64,
                            "build_dir_bytes": 6_000_000_000u64,
                            "owning_session": "trusty-tools-95"
                        }
                    ]
                }
            ]
        }
    })
}

/// Serve one `/rpc` route answering `disk_survey`, recording each call's
/// arguments; return the base URL and the recorder.
async fn serve_stub() -> (String, Arc<Mutex<Vec<Value>>>) {
    let calls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&calls);
    let router = Router::new().route(
        "/rpc",
        post(move |Json(req): Json<Value>| {
            let recorder = Arc::clone(&recorder);
            async move {
                let args = req
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(Value::Null);
                recorder.lock().expect("record the call").push(args);
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": req.get("id").cloned().unwrap_or(Value::Null),
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": survey_payload().to_string()
                        }],
                        "isError": false
                    }
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("read the bound address");
    tokio::spawn(axum::serve(listener, router).into_future());
    (format!("http://{addr}"), calls)
}

/// Run the built `tm` binary against the stub, from a hermetic temp cwd.
///
/// The cwd is outside any managed workspace root, so the project filter
/// declines and the survey covers everything — which is what makes the
/// assertions independent of the developer's own machine.
fn run_tm(url: &str, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tm"))
        .env("TRUSTY_MPM_URL", url)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn the tm binary")
}

/// `tm session disk --json` reports every session, in bytes-descending order,
/// with a total — and asks the daemon for the per-session roll-up.
#[tokio::test(flavor = "multi_thread")]
async fn session_disk_json_reports_every_session() {
    let (url, calls) = serve_stub().await;
    let cwd = tempfile::tempdir().expect("temp cwd");

    let out = tokio::task::spawn_blocking({
        let url = url.clone();
        let cwd = cwd.path().to_path_buf();
        move || run_tm(&url, &cwd, &["session", "disk", "--json"])
    })
    .await
    .expect("the binary run must not panic");

    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value =
        serde_json::from_slice(&out.stdout).expect("`--json` must put a JSON document on stdout");

    assert_eq!(report["view"], "sessions");
    let sessions = report["sessions"].as_array().expect("a sessions array");
    assert_eq!(sessions.len(), 2, "{report}");
    assert_eq!(sessions[0]["session_id"], "trusty-tools-95");
    assert_eq!(sessions[0]["bytes"], 30_000_000_000u64);
    assert_eq!(
        sessions[0]["source_bytes"], 6_000_000_000u64,
        "source is the remainder after the build directories"
    );
    assert_eq!(
        sessions[1]["session_id"],
        Value::Null,
        "the unattributed bucket sorts last: {report}"
    );
    assert_eq!(report["total"]["bytes"], 35_000_000_000u64);
    assert_eq!(report["total"]["worktree_count"], 3);
    assert_eq!(report["partial"], false);
    assert_eq!(report["generated_at"], "2026-09-10T00:00:00Z");

    // The roll-up is what slice 1 added; asking for it is the whole contract
    // between this command and the tool.
    let recorded = calls.lock().expect("read the recorded calls");
    assert_eq!(recorded.len(), 1, "exactly one survey call: {recorded:?}");
    assert_eq!(recorded[0]["group_by"], "session");
    assert!(
        recorded[0]["budget_seconds"].is_u64(),
        "the call must carry a budget the client's own timeout outlives: {:?}",
        recorded[0]
    );
}

/// Naming a session prints its breakdown by class and its worktrees across
/// every project.
#[tokio::test(flavor = "multi_thread")]
async fn session_disk_breaks_one_session_down_by_class() {
    let (url, _calls) = serve_stub().await;
    let cwd = tempfile::tempdir().expect("temp cwd");

    let out = tokio::task::spawn_blocking({
        let url = url.clone();
        let cwd = cwd.path().to_path_buf();
        move || {
            run_tm(
                &url,
                &cwd,
                &["session", "disk", "trusty-tools-95", "--json"],
            )
        }
    })
    .await
    .expect("the binary run must not panic");

    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).expect("a JSON document on stdout");

    assert_eq!(report["view"], "session");
    assert_eq!(report["session_id"], "trusty-tools-95");
    let classes = report["classes"].as_array().expect("a classes array");
    assert_eq!(classes[0]["class"], "build");
    assert_eq!(classes[0]["bytes"], 24_000_000_000u64);
    assert_eq!(classes[1]["class"], "source");
    assert_eq!(classes[1]["bytes"], 6_000_000_000u64);

    let worktrees = report["worktrees"].as_array().expect("a worktrees array");
    assert_eq!(worktrees.len(), 2, "{report}");
    assert_eq!(
        worktrees[0]["path"], "/w/bobmatnyc/trusty-tools/.claude/worktrees/a",
        "bytes descending: {report}"
    );
    let projects: Vec<&str> = worktrees
        .iter()
        .map(|w| w["project"].as_str().expect("a project label"))
        .collect();
    assert!(
        projects.contains(&"bobmatnyc/trusty-tools") && projects.contains(&"bobmatnyc/other"),
        "a session's worktrees span projects: {projects:?}"
    );
}

/// The human table goes to stdout, with a header and a total line.
#[tokio::test(flavor = "multi_thread")]
async fn session_disk_prints_a_table_without_json() {
    let (url, _calls) = serve_stub().await;
    let cwd = tempfile::tempdir().expect("temp cwd");

    let out = tokio::task::spawn_blocking({
        let url = url.clone();
        let cwd = cwd.path().to_path_buf();
        move || run_tm(&url, &cwd, &["session", "disk"])
    })
    .await
    .expect("the binary run must not panic");

    assert!(out.status.success(), "{:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("SESSION"), "{stdout}");
    assert!(stdout.contains("trusty-tools-95"), "{stdout}");
    assert!(stdout.contains("(unattributed)"), "{stdout}");
    assert!(
        stdout.lines().any(|l| l.starts_with("TOTAL")),
        "the table must end on a total line: {stdout}"
    );
    assert!(
        !stdout.contains('{'),
        "the default form is a table, not JSON: {stdout}"
    );
}

/// An unknown session exits non-zero with the reason on stderr and nothing on
/// stdout.
///
/// Why this matters more than it looks: an empty report for a typo is
/// indistinguishable from a session that genuinely holds nothing, and an
/// operator would clear the wrong thing — or nothing — on the strength of it.
#[tokio::test(flavor = "multi_thread")]
async fn session_disk_rejects_an_unknown_session() {
    let (url, _calls) = serve_stub().await;
    let cwd = tempfile::tempdir().expect("temp cwd");

    let out = tokio::task::spawn_blocking({
        let url = url.clone();
        let cwd = cwd.path().to_path_buf();
        move || {
            run_tm(
                &url,
                &cwd,
                &["session", "disk", "no-such-session", "--json"],
            )
        }
    })
    .await
    .expect("the binary run must not panic");

    assert!(
        !out.status.success(),
        "an unknown session must exit non-zero; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-such-session"),
        "stderr must name the session that matched nothing: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "nothing may reach stdout — a partial document would parse as a report"
    );
}
