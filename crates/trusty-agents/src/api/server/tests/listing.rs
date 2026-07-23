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

use crate::api::server::projects::{load_sessions_from, scan_agent_catalog, scan_agents_dir};

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

/// Why (#3737, per-message chat attribution, epic #3052): the GUI's agent
/// roster labels each assistant chat bubble with the persona's `display_name`
/// ("Izzie", "CTO Assistant"), so `GET /api/agents` must surface that field.
/// A named agent carries its `display_name`; an agent WITHOUT one falls back
/// to its `name` per the cross-PR contract (#3740) — `display_name` is never
/// empty, so consumers always have a non-empty label to render.
/// What: Writes one named + one display_name-less fixture, scans, asserts the
/// field (present → verbatim; absent → the agent name).
#[tokio::test]
async fn scan_agents_dir_exposes_display_name() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("izzie.toml"),
        "[agent]\nname = \"izzie\"\nrole = \"assistant\"\ndisplay_name = \"Izzie\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\nrole = \"engineer\"\n",
    )
    .unwrap();

    let agents = scan_agents_dir(tmp.path()).await;
    // Sorted by name: engineer < izzie.
    assert_eq!(agents[0]["name"], "engineer");
    assert_eq!(
        agents[0]["display_name"], "engineer",
        "an agent without display_name falls back to its name, never empty (#3740 contract)"
    );
    assert_eq!(agents[1]["name"], "izzie");
    assert_eq!(agents[1]["display_name"], "Izzie");
}

/// Why (#3741 — Bob's "picker does not list Izzie or CTO Bot" report): the
/// catalog must resolve directory PACKAGES (`<name>/agent.toml`), not just
/// flat `<name>.toml`, because `izzie`/`cto-assistant`/`assistant` ship as
/// packages. Within one directory a package must win over a flat file of the
/// same name (matching the runtime's resolution order), and archived
/// `*.stale.bak` copies must be ignored so they never shadow the live agent.
/// What: A fixture dir with a package-only agent, a flat-only agent, a name
/// present as BOTH (package must win), and a `*.stale.bak` backup dir + file
/// (must be skipped). Asserts exactly the three real agents, package-wins, and
/// no backup leakage.
#[tokio::test]
async fn scan_agents_dir_resolves_packages_flat_and_dedupes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Package-only agent: izzie/agent.toml
    std::fs::create_dir(root.join("izzie")).unwrap();
    std::fs::write(
        root.join("izzie").join("agent.toml"),
        "[agent]\nname = \"izzie\"\nrole = \"assistant\"\ndisplay_name = \"Izzie\"\n",
    )
    .unwrap();

    // Flat-only agent: engineer.toml
    std::fs::write(
        root.join("engineer.toml"),
        "[agent]\nname = \"engineer\"\nrole = \"engineer\"\n",
    )
    .unwrap();

    // Present as BOTH: a flat cto-assistant.toml AND a cto-assistant/ package.
    // The PACKAGE must win (display_name "CTO Bot", not "Flat Loser").
    std::fs::write(
        root.join("cto-assistant.toml"),
        "[agent]\nname = \"cto-assistant\"\nrole = \"assistant\"\ndisplay_name = \"Flat Loser\"\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("cto-assistant")).unwrap();
    std::fs::write(
        root.join("cto-assistant").join("agent.toml"),
        "[agent]\nname = \"cto-assistant\"\nrole = \"assistant\"\ndisplay_name = \"CTO Bot\"\n",
    )
    .unwrap();

    // Archived backups that must be skipped entirely.
    std::fs::create_dir(root.join("izzie.stale.bak")).unwrap();
    std::fs::write(
        root.join("izzie.stale.bak").join("agent.toml"),
        "[agent]\nname = \"izzie\"\nrole = \"assistant\"\ndisplay_name = \"STALE\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("engineer.toml.stale.bak"),
        "[agent]\nname = \"engineer\"\ndisplay_name = \"STALE\"\n",
    )
    .unwrap();

    let agents = scan_agents_dir(root).await;
    let names: Vec<&str> = agents.iter().filter_map(|a| a["name"].as_str()).collect();
    assert_eq!(
        names,
        vec!["cto-assistant", "engineer", "izzie"],
        "exactly the three real agents, name-sorted, no backup leakage"
    );

    let cto = agents
        .iter()
        .find(|a| a["name"] == "cto-assistant")
        .unwrap();
    assert_eq!(
        cto["display_name"], "CTO Bot",
        "the directory package must win over the flat file of the same name"
    );
    let izzie = agents.iter().find(|a| a["name"] == "izzie").unwrap();
    assert_eq!(izzie["display_name"], "Izzie");
    // The STALE backup must never have overridden the live izzie.
    assert_ne!(izzie["display_name"], "STALE");
}

/// Why (#3741): the catalog spans multiple candidate directories (project-local
/// then `$HOME` bundle tier); a name defined in the FIRST (higher-priority)
/// directory must shadow a same-named agent in a later one, matching
/// `agents_dir_candidates()`'s precedence. This is what lets a project-local
/// override win while still surfacing bundled personas from `$HOME`.
/// What: Two dirs both defining `izzie` (+ a dir-unique agent each); asserts
/// the first dir's `izzie` wins and both unique agents appear.
#[tokio::test]
async fn scan_agent_catalog_dedupes_across_dirs() {
    let primary = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    std::fs::write(
        primary.path().join("izzie.toml"),
        "[agent]\nname = \"izzie\"\ndisplay_name = \"Primary Izzie\"\n",
    )
    .unwrap();
    std::fs::write(
        primary.path().join("project-only.toml"),
        "[agent]\nname = \"project-only\"\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("izzie.toml"),
        "[agent]\nname = \"izzie\"\ndisplay_name = \"Home Izzie\"\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("cto-assistant.toml"),
        "[agent]\nname = \"cto-assistant\"\ndisplay_name = \"CTO Bot\"\n",
    )
    .unwrap();

    let dirs = vec![primary.path().to_path_buf(), home.path().to_path_buf()];
    let agents = scan_agent_catalog(&dirs).await;
    let names: Vec<&str> = agents.iter().filter_map(|a| a["name"].as_str()).collect();
    assert_eq!(names, vec!["cto-assistant", "izzie", "project-only"]);

    let izzie = agents.iter().find(|a| a["name"] == "izzie").unwrap();
    assert_eq!(
        izzie["display_name"], "Primary Izzie",
        "the first (higher-priority) directory's agent must shadow the $HOME one"
    );
    // The bundled-tier-only agent still surfaces.
    let cto = agents
        .iter()
        .find(|a| a["name"] == "cto-assistant")
        .unwrap();
    assert_eq!(cto["display_name"], "CTO Bot");
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
