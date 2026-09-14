//! Unit tests for the `.mcp.json` approval reporter (#7892).
//!
//! Why: the reporter must read Claude Code's precedence, not invent one — a
//! test that only ever supplied a single tier would pass against an
//! implementation that read any tier at all.
//! What: the declared-name read, the three-state resolution, tier precedence,
//! and the fail-quiet arms.
//! Test: this file.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Write `<dir>/.mcp.json` declaring `names` as stdio servers.
fn mcp_json(dir: &Path, names: &[&str]) {
    let mut servers = serde_json::Map::new();
    for name in names {
        servers.insert(
            (*name).to_string(),
            json!({"type": "stdio", "command": name, "args": []}),
        );
    }
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(crate::core::mcp_config::MCP_JSON),
        json!({ "mcpServers": servers }).to_string(),
    )
    .unwrap();
}

/// Write a settings file at `path` holding `body`.
fn settings(path: &Path, body: Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body.to_string()).unwrap();
}

#[test]
fn project_mcp_state_is_empty_without_an_mcp_json() {
    let tmp = TempDir::new().unwrap();
    assert!(project_mcp_state(tmp.path(), tmp.path()).is_empty());
}

#[test]
fn project_mcp_state_is_empty_for_a_malformed_mcp_json() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(crate::core::mcp_config::MCP_JSON),
        "{ not json",
    )
    .unwrap();
    assert!(
        project_mcp_state(tmp.path(), tmp.path()).is_empty(),
        "a diagnostic read degrades to no information, never to an error"
    );
}

#[test]
fn project_mcp_state_reports_each_declared_server() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    mcp_json(&cwd, &["apex", "duetto-memory", "smuggled"]);
    settings(
        &cwd.join(".claude").join("settings.json"),
        json!({
            "enabledMcpjsonServers": ["apex"],
            "disabledMcpjsonServers": ["smuggled"],
        }),
    );

    let state = project_mcp_state(&cwd, &cfg);

    assert_eq!(
        state,
        vec![
            ProjectMcpServer {
                name: "apex".to_owned(),
                approval: Approval::Enabled
            },
            ProjectMcpServer {
                name: "duetto-memory".to_owned(),
                approval: Approval::Prompt
            },
            ProjectMcpServer {
                name: "smuggled".to_owned(),
                approval: Approval::Disabled
            },
        ]
    );
}

#[test]
fn project_mcp_state_honours_enable_all() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    mcp_json(&cwd, &["apex"]);
    settings(
        &cfg.join("settings.json"),
        json!({"enableAllProjectMcpServers": true}),
    );

    assert_eq!(
        project_mcp_state(&cwd, &cfg)[0].approval,
        Approval::Enabled,
        "a blanket approval in the user tier still approves"
    );
}

/// The precedence itself: the project tier's explicit refusal beats a user-tier
/// blanket approval, so a single-tier implementation fails here.
#[test]
fn approval_prefers_the_project_tier() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    mcp_json(&cwd, &["apex"]);
    settings(
        &cwd.join(".claude").join("settings.local.json"),
        json!({"disabledMcpjsonServers": ["apex"]}),
    );
    settings(
        &cfg.join("settings.json"),
        json!({"enableAllProjectMcpServers": true}),
    );

    assert_eq!(
        project_mcp_state(&cwd, &cfg)[0].approval,
        Approval::Disabled
    );
}

/// Claude Code records a prompt answer under `projects.<cwd>` of the managed
/// `.claude.json`; that record is a tier too.
#[test]
fn approval_reads_the_recorded_prompt_answer() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    mcp_json(&cwd, &["apex"]);
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join(".claude.json"),
        json!({"projects": {cwd.to_string_lossy(): {"enabledMcpjsonServers": ["apex"]}}})
            .to_string(),
    )
    .unwrap();

    assert_eq!(project_mcp_state(&cwd, &cfg)[0].approval, Approval::Enabled);
}

#[test]
fn approval_defaults_to_prompt() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    mcp_json(&cwd, &["apex"]);

    let state = project_mcp_state(&cwd, &tmp.path().join("cfg"));

    assert_eq!(state[0].approval, Approval::Prompt);
    assert_eq!(state[0].approval.label(), "unapproved");
}
