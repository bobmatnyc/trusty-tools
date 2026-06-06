//! Agent + session listing handler tests (#407).
//!
//! Why: `/api/agents` and `/api/sessions` must return stable JSON envelopes
//! even when their backing directories/files are absent. These drive the
//! extracted `scan_agents_dir` / `load_sessions_from` helpers directly against
//! tempdirs so they don't race sibling tests on process-global cwd. Split from
//! the parent `tests` module to keep each file under the 500-line cap.
//! What: Envelope-shape + filter assertions for the agent catalogue and
//! session history loaders.
//! Test: This module IS the test.

use crate::api::server::projects::{load_sessions_from, scan_agents_dir};

/// Why: Confirms `/api/agents` returns the `{"agents": [...]}` envelope
/// with the spec-required fields (name, role, model, runner) parsed from
/// agent TOML, sorted alphabetically. Drives `scan_agents_dir` directly
/// against a tempdir so the test does not depend on process cwd.
/// What: Writes two TOML fixtures, scans, asserts envelope shape and
/// content.
/// Test: Self-explanatory — run via `cargo test list_agents_returns`.
#[tokio::test]
async fn list_agents_returns_agents_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
            tmp.path().join("pm.toml"),
            "[agent]\nname = \"pm\"\nrole = \"orchestrator\"\nmodel = \"claude-sonnet-4-6\"\nrunner = \"claude-code\"\n",
        )
        .unwrap();
    std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\nrole = \"engineer\"\nmodel = \"claude-opus-4-6\"\nrunner = \"claude-code\"\n",
        )
        .unwrap();

    let agents = scan_agents_dir(tmp.path()).await;
    let envelope = serde_json::json!({ "agents": &agents });

    assert!(envelope["agents"].is_array(), "envelope shape: {envelope}");
    let arr = envelope["agents"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Sorted by name alphabetically: engineer < pm.
    assert_eq!(arr[0]["name"], "engineer");
    assert_eq!(arr[0]["role"], "engineer");
    assert_eq!(arr[0]["model"], "claude-opus-4-6");
    assert_eq!(arr[0]["runner"], "claude-code");
    assert_eq!(arr[1]["name"], "pm");
    assert_eq!(arr[1]["runner"], "claude-code");
}

/// Why: When the agents directory is missing, the route must still
/// return a valid empty envelope so the UI does not crash.
/// What: Points scan at a nonexistent path, asserts empty vec.
#[tokio::test]
async fn list_agents_missing_dir_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let agents = scan_agents_dir(&missing).await;
    assert!(agents.is_empty());
}

/// Why: `/api/sessions` must return `{"sessions": []}` (not HTML, not 404)
/// when no sessions.json exists. Falling through to the SPA catch-all
/// would break clients that parse the body as JSON (#407 root cause).
/// Drives `load_sessions_from` directly against a temp path so the test
/// does not race with sibling tests on the process-global cwd.
/// What: Points the loader at a nonexistent path, asserts empty list, then
/// wraps in the same envelope the route produces and asserts shape.
/// Test: Self-explanatory.
#[tokio::test]
async fn list_sessions_empty_returns_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist.json");
    let sessions = load_sessions_from(&missing, None).await;
    let envelope = serde_json::json!({ "sessions": &sessions });
    assert!(
        envelope["sessions"].is_array(),
        "envelope shape: {envelope}"
    );
    assert_eq!(envelope["sessions"].as_array().unwrap().len(), 0);
}

/// Why: When sessions.json contains entries, the loader must return them
/// untouched and the optional `project` filter must select by `project`
/// or `path` field equality.
/// What: Writes a fixture sessions.json, loads with and without filter,
/// asserts shape and counts.
/// Test: Self-explanatory.
#[tokio::test]
async fn list_sessions_filters_by_project() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sessions.json");
    std::fs::write(
        &path,
        br#"{"sessions":[
                {"id":"a","project":"/p1","status":"idle"},
                {"id":"b","project":"/p2","status":"idle"},
                {"id":"c","path":"/p1","status":"idle"}
            ]}"#,
    )
    .unwrap();

    let all = load_sessions_from(&path, None).await;
    assert_eq!(all.len(), 3);

    let p1 = load_sessions_from(&path, Some("/p1")).await;
    assert_eq!(p1.len(), 2, "both `project` and `path` should match");
    assert_eq!(p1[0]["id"], "a");
    assert_eq!(p1[1]["id"], "c");
}
