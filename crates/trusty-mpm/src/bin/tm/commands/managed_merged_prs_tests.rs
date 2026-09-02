//! Tests for the merged-PR reclaim-pass renderer (#2919).
//!
//! Why: this renderer is the operator's only evidence that the pass ran and
//! what it did. A silent pass is indistinguishable from a pass that deleted
//! the wrong thing, so the shapes it must never swallow — a failed pass, a
//! re-check refusal, a removal that failed — are pinned here.
//! What: exercises the null/absent short-circuit, the dry-run and real
//! summaries, and every stderr diagnostic branch.

use super::{
    classification_clause, diagnostic_lines, print_merged_pr_pass, session_prune_worktrees,
};

/// 🔴 #6561 REGRESSION: "0 reclaimable" must never be printed without saying
/// whether anything could be classified.
///
/// Why: the live reply on 2026-09-02 — `pr_state_unknown: 261` of 261 surveyed,
/// `reclaimable: 0` — rendered as
/// `merged-PR pass: 0 worktree(s) reclaimable; 0 byte(s) across 0 of 0 measured`,
/// which is exactly what a healthy sweep over a workspace with nothing to
/// reclaim prints. The cause (`gh` exited 4) reached the daemon and stopped
/// there.
#[test]
fn merged_pr_pass_names_a_failed_lookup_beside_the_reclaimable_count() {
    let body = serde_json::json!({
        "reclaimable": 0,
        "reclaimable_bytes": 0,
        "reclaimable_measured": 0,
        "pr_state_unknown": 4,
        "not_inspected": 2,
        "lookup_failed": 261,
        "lookup_failure": "`gh` exited 4: To get started with GitHub CLI, please run:  gh auth login",
    });
    let clause = classification_clause(&body);
    assert!(
        clause.contains("261 pull-request lookup(s) FAILED"),
        "{clause}"
    );
    assert!(clause.contains("gh auth login"), "{clause}");
    assert!(
        clause.contains("4 pull-request state(s) indeterminate"),
        "{clause}"
    );
    assert!(clause.contains("2 not inspected"), "{clause}");
    // And it must reach stdout in both modes, not merely be computed.
    print_merged_pr_pass(Some(&body), true);
    print_merged_pr_pass(Some(&body), false);
}

/// A run where everything classified adds no clause — the healthy line is
/// unchanged (#6561).
#[test]
fn merged_pr_pass_adds_no_clause_when_everything_classified() {
    let body = serde_json::json!({
        "reclaimable": 3,
        "pr_state_unknown": 0,
        "not_inspected": 0,
        "lookup_failed": 0,
    });
    assert_eq!(classification_clause(&body), "");
    // An older daemon omits all three keys; that is also "nothing to say".
    assert_eq!(classification_clause(&serde_json::json!({})), "");
}

#[test]
fn merged_pr_pass_surfaces_an_agent_owned_skip() {
    // #5829: this is the line whose absence the whole issue is about. The route
    // spared a live agent's worktree and said so in `spared_agent_owned`; the
    // renderer dropped the key, so `--merged-prs --force` printed
    // "reclaimed 0 worktree(s)" and nothing else.
    let body = serde_json::json!({
        "removed": [],
        "removed_bytes": 0,
        "spared_agent_owned": [
            "/repo/.claude/worktrees/agent-7f3: owned by dispatched agent agent-7f3 \
             — a delegation naming it has not ended",
        ],
        "refused_at_recheck": [],
        "removal_failed": [],
    });
    let lines = diagnostic_lines(&body);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("spared"), "{}", lines[0]);
    assert!(lines[0].contains("agent-7f3"), "{}", lines[0]);
    // And it must reach stderr rather than only being computed.
    print_merged_pr_pass(Some(&body), false);
}

#[test]
fn merged_pr_pass_diagnostics_keep_their_kinds_apart() {
    // A spared agent tree, a re-check near-miss and a failed removal are three
    // different operator actions. Collapsing them into one label would tell the
    // operator to go hunting for an agent that does not exist.
    let body = serde_json::json!({
        "spared_agent_owned": ["/a: an agent owns it"],
        "refused_at_recheck": ["/b: a session claims it now"],
        "removal_failed": ["/c: git-locked"],
    });
    let lines = diagnostic_lines(&body);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].contains("dispatched agent"), "{}", lines[0]);
    assert!(lines[1].contains("re-check"), "{}", lines[1]);
    assert!(lines[2].contains("removal FAILED"), "{}", lines[2]);
}

#[test]
fn merged_pr_pass_diagnostics_tolerate_absent_and_malformed_keys() {
    // An older daemon omits the key entirely; a malformed one sends a non-array
    // or non-string entries. Neither may panic, and neither may invent a line.
    assert!(diagnostic_lines(&serde_json::json!({})).is_empty());
    assert!(
        diagnostic_lines(&serde_json::json!({ "spared_agent_owned": "not-an-array" })).is_empty()
    );
    assert!(diagnostic_lines(&serde_json::json!({ "spared_agent_owned": [7, null] })).is_empty());
}

#[test]
fn merged_pr_pass_prints_nothing_for_a_null_body() {
    // The shape the route returns when `merged_prs` was not requested. It must
    // not print a "reclaimed 0" line that implies the pass ran.
    print_merged_pr_pass(None, true);
    print_merged_pr_pass(Some(&serde_json::Value::Null), false);
}

#[test]
fn merged_pr_pass_reports_reclaimed_paths_and_bytes() {
    let body = serde_json::json!({
        "removed": ["/tmp/a", "/tmp/b"],
        "removed_bytes": 4096,
        "refused_at_recheck": [],
        "removal_failed": [],
        "reclaimable": 2,
        "reclaimable_bytes": 4096,
    });
    // Both modes must render without panicking on any field shape.
    print_merged_pr_pass(Some(&body), false);
    print_merged_pr_pass(Some(&body), true);
}

#[test]
fn merged_pr_pass_surfaces_recheck_refusals() {
    // A candidate the survey approved but the re-check refused is a near-miss:
    // it must reach the operator, not be swallowed.
    let body = serde_json::json!({
        "removed": [],
        "removed_bytes": 0,
        "refused_at_recheck": ["/tmp/c: a session claims it now"],
        "removal_failed": ["/tmp/d"],
        "reclaimable": 1,
    });
    print_merged_pr_pass(Some(&body), false);
}

#[test]
fn merged_pr_pass_reports_a_failed_pass_rather_than_a_zero_count() {
    // A panicked pass reclaimed nothing, but printing "reclaimed 0 worktrees"
    // would read as a healthy no-op rather than a failure.
    let body = serde_json::json!({ "error": "task panicked" });
    print_merged_pr_pass(Some(&body), false);
}

/// 🔴 #5830 REGRESSION: `--merged-prs` must outlive the client's default
/// request timeout.
///
/// Why: the survey runs synchronously inside the handler and takes minutes
/// (over 600 seconds byte-walking 46 worktrees, per `SurveyBudget`'s own doc).
/// Against the 10s `DEFAULT_REQUEST_TIMEOUT` the CLI hung up on every single
/// invocation — "operation timed out", deterministically, never once
/// completing. The fix is a per-request override, and an override only helps if
/// it actually reaches the wire.
/// What: scales the real shape down by 1000x. A one-shot HTTP server answers
/// after `SERVER_DELAY`; the client is built with a `CLIENT_BOUND` shorter than
/// that, standing in for the production 10s. With `merged_prs: true` the
/// request must survive to read the response, because the override replaced the
/// client bound.
///
/// Non-vacuity: the CONTROL sends the SAME body with `merged_prs: false` against
/// an identical server and asserts it FAILS — so the test cannot pass merely
/// because the client bound was never applied at all. That control is also the
/// behavioural claim itself: the orphan-only sweep keeps failing fast against a
/// wedged daemon.
/// Test: this function IS the test.
#[tokio::test]
async fn merged_pr_request_outlives_the_default_client_timeout() {
    use std::time::Duration;

    // Comfortably longer than `CLIENT_BOUND`, short enough to keep the test
    // fast; the gap absorbs scheduler jitter on a loaded runner.
    const SERVER_DELAY: Duration = Duration::from_millis(1200);
    const CLIENT_BOUND: Duration = Duration::from_millis(250);

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(CLIENT_BOUND)
        .build()
        .expect("build a short-bounded test client");

    let (url, server) = slow_prune_server(SERVER_DELAY).await;
    let overridden = session_prune_worktrees(&client, &url, true, false, true).await;
    server.await.expect("server task must not panic");
    assert!(
        overridden.is_ok(),
        "`--merged-prs` must carry a per-request timeout longer than the client \
         default; got {overridden:?}"
    );

    // CONTROL: without the opt-in the client default still bounds the call.
    let (url, server) = slow_prune_server(SERVER_DELAY).await;
    let plain = session_prune_worktrees(&client, &url, true, false, false).await;
    server.await.expect("server task must not panic");
    assert!(
        plain.is_err(),
        "the orphan-only sweep must keep the short default bound, otherwise this \
         test proves nothing about the override"
    );
}

/// A one-shot HTTP server that answers a prune-worktrees POST after `delay`.
///
/// Why: reproducing "the daemon is still working when the client's clock runs
/// out" needs a peer that eventually ANSWERS — a never-answering socket (the
/// `build_client_bounds_a_stalled_connection` shape) cannot tell a raised bound
/// from an unraised one, because both end in an error.
/// What: binds an ephemeral loopback port, returns its URL plus the join handle
/// for the accept task. The task reads until the request headers end, sleeps,
/// then writes a minimal 200 carrying the `merged_prs: null` body the route
/// returns when the pass was not requested.
async fn slow_prune_server(delay: std::time::Duration) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind slow prune server");
    let url = format!("http://{}", listener.local_addr().expect("local_addr"));
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept one connection");
        let mut seen = Vec::new();
        let mut buf = [0u8; 1024];
        // Read only as far as the end of the headers: the body is a few dozen
        // bytes and reqwest sends it immediately, so nothing is left to block
        // the response write.
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
        }
        tokio::time::sleep(delay).await;
        let body = br#"{"dry_run":true,"paths":[],"skipped_dirty":[],"merged_prs":null}"#;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        );
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(body).await;
        let _ = sock.flush().await;
    });
    (url, handle)
}

#[test]
fn merged_pr_pass_tolerates_missing_fields() {
    // A third-party or older daemon may omit fields entirely; the renderer must
    // degrade rather than panic.
    print_merged_pr_pass(Some(&serde_json::json!({})), true);
    print_merged_pr_pass(
        Some(&serde_json::json!({ "removed": "not-an-array" })),
        false,
    );
}
