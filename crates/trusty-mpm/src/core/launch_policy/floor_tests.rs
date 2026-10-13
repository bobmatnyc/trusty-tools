//! Tests for the never-deny floor, including the agent-grant drift guard.

use std::collections::BTreeSet;
use std::path::Path;

use super::*;

/// Every `mcp__<server>` named on an agent's `tools:` line.
fn agent_tools_mcp_servers(agents_dir: &Path) -> BTreeSet<String> {
    let mut servers = BTreeSet::new();
    for entry in std::fs::read_dir(agents_dir).expect("read content/agents") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read agent file");
        for line in text.lines().filter(|l| l.starts_with("tools:")) {
            for token in line.split(|c: char| c == '[' || c == ']' || c == ',') {
                if let Some(rest) = token.trim().strip_prefix("mcp__") {
                    let server = rest.split("__").next().unwrap_or(rest);
                    servers.insert(server.to_string());
                }
            }
        }
    }
    servers
}

#[test]
fn floor_holds_the_builtins_and_chrome() {
    for name in [
        "trusty-memory",
        "trusty-mpm",
        "trusty-review",
        "trusty-search",
        "claude-in-chrome",
    ] {
        assert!(is_floor(name), "{name} must be in the floor");
    }
    assert!(!is_floor("heygen"));
    assert!(!is_floor("slack-mcp"));
}

/// #8988 Q2 drift guard: a new agent MCP grant must join the floor, or an
/// owner `deny` could break that agent.
#[test]
fn floor_covers_every_agent_tools_mcp_grant() {
    let agents = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/agents");
    let granted = agent_tools_mcp_servers(&agents);
    assert!(
        granted.contains("trusty-search"),
        "the scan found no grants — the parser or the path is wrong: {granted:?}"
    );
    let missing: Vec<&String> = granted.iter().filter(|s| !is_floor(s)).collect();
    assert!(
        missing.is_empty(),
        "agent tools: lines grant MCP servers outside the never-deny floor: {missing:?}"
    );
}
