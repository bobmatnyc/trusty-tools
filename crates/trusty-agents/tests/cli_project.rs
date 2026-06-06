//! Integration tests for the `Project` test object.
//!
//! Why: Verify that the harness-level CLI (`trusty-agents inspect --dry-run`)
//! works end-to-end against the bundled config tree without needing a live
//! LLM API key. This is the dry-run safety net that lets us catch regressions
//! in agent/skill loading before paying for an LLM round-trip.
//! What: Exercises `Project::run_inspect` in two scenarios — a FastAPI task
//! (expect non-empty `matched_skills`) and a Python-tests task (expect a
//! non-empty agent match). Both run with `--dry-run` so no env vars beyond
//! Cargo's are required.
//! Test: `cargo test --test cli_project`.

mod support;

use support::project::Project;

#[tokio::test]
async fn project_inspect_returns_matched_skills() {
    let proj = Project::new();
    let report = proj
        .run_inspect("build a fastapi app")
        .await
        .expect("inspect should succeed");
    // The current schema nests matched_skills under `registry`. Tolerate
    // either shape so the test stays green if the schema flattens later.
    let arr = report
        .get("registry")
        .and_then(|r| r.get("matched_skills"))
        .or_else(|| report.get("matched_skills"))
        .and_then(|v| v.as_array())
        .expect("matched_skills array present");
    assert!(
        !arr.is_empty(),
        "expected at least one matched skill for fastapi task; report={report}"
    );
}

#[tokio::test]
async fn project_inspect_returns_agent_match() {
    let proj = Project::new();
    let report = proj
        .run_inspect("write python tests")
        .await
        .expect("inspect should succeed");
    let agent = report
        .get("registry")
        .and_then(|r| r.get("best_match"))
        .or_else(|| report.get("best_agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !agent.is_empty(),
        "expected non-empty best_match agent; report={report}"
    );
}
