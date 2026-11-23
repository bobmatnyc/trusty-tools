//! `crate::mcp::shared` — the global tier and the `.mcp.json` merge (#7454).

use trusty_mcp::config::McpConfigFile;

use super::{stdio, tempdir};
use crate::mcp::extensions;
use crate::mcp::shared::{GlobalTier, McpTier, load_global_at, merge_mcp_json};

fn tier_from(servers: Vec<trusty_mcp::config::McpServerConfig>) -> GlobalTier {
    GlobalTier {
        servers,
        issues: Vec::new(),
        path: std::path::PathBuf::from("servers.toml"),
    }
}

/// ADR-0060 decision 6, the first case: a shared file that EXISTS and is
/// broken must start the assistant with zero servers AND say so. A silent
/// empty list is indistinguishable from "nothing configured yet", and falling
/// back to the retired tables would resurrect config the user has since moved.
#[test]
fn a_malformed_shared_file_yields_zero_servers_and_an_issue() {
    let dir = tempdir("global-malformed");
    let shared = dir.join("servers.toml");
    let legacy = dir.join("config.toml");
    std::fs::write(&shared, "servers = [ this is not toml").unwrap();
    std::fs::write(
        &legacy,
        "[[mcp.services]]\nname = \"stale\"\ndescription = \"d\"\ncommand = \"c\"\ntransport = \"stdio\"\n",
    )
    .unwrap();

    let tier = load_global_at(&shared, &legacy);
    assert!(tier.servers.is_empty(), "must not fall back to any list");
    assert_eq!(tier.issues.len(), 1);
    assert_eq!(tier.issues[0].tier, McpTier::Global);
    assert_eq!(tier.issues[0].path, shared);
    assert!(!tier.issues[0].remedy.is_empty());
}

#[test]
fn an_absent_shared_file_is_empty_without_an_issue() {
    let dir = tempdir("global-absent");
    let tier = load_global_at(&dir.join("servers.toml"), &dir.join("config.toml"));
    assert!(tier.servers.is_empty());
    assert!(tier.issues.is_empty(), "absence is not a fault");
}

#[test]
fn a_valid_shared_file_loads_in_file_order() {
    let dir = tempdir("global-valid");
    let shared = dir.join("servers.toml");
    McpConfigFile::new(vec![stdio("alpha", "a"), stdio("beta", "b")])
        .save(&shared)
        .unwrap();

    let tier = load_global_at(&shared, &dir.join("config.toml"));
    let names: Vec<&str> = tier.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert!(tier.issues.is_empty());
}

/// #3266's trust gate is unchanged by #7454 — it just moved to where the merge
/// happens. Untrusted means the file contributes nothing at all.
#[test]
fn untrusted_mcp_json_contributes_nothing() {
    let dir = tempdir("global-untrusted");
    let path = dir.join(".mcp.json");
    std::fs::write(
        &path,
        r#"{"mcpServers": {"hostile": {"type": "stdio", "command": "curl", "args": ["evil"]}}}"#,
    )
    .unwrap();

    let mut tier = tier_from(Vec::new());
    merge_mcp_json(&mut tier, &[path], false);
    assert!(tier.servers.is_empty());
    assert!(tier.issues.is_empty());
}

/// A trusted `.mcp.json` entry is APPENDED behind the shared file, marked with
/// its source, and marked `discover` — it carries no static tool list, so live
/// discovery is the only way it can contribute anything.
#[test]
fn mcp_json_entries_are_marked_and_ranked_last() {
    let dir = tempdir("global-trusted");
    let path = dir.join(".mcp.json");
    std::fs::write(
        &path,
        r#"{"mcpServers": {
            "from-repo": {"type": "stdio", "command": "repo-bin", "args": []},
            "alpha": {"type": "stdio", "command": "shadowed", "args": []}
        }}"#,
    )
    .unwrap();

    let mut tier = tier_from(vec![stdio("alpha", "a")]);
    merge_mcp_json(&mut tier, &[path], true);

    let names: Vec<&str> = tier.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["alpha", "from-repo"],
        "the shared file wins by name"
    );

    let alpha = &tier.servers[0];
    assert_eq!(
        extensions::stdio_parts(alpha).map(|(c, _, _)| c),
        Some("a"),
        "a .mcp.json entry must not replace the shared file's command"
    );
    let imported = &tier.servers[1];
    assert_eq!(
        extensions::source(imported).as_deref(),
        Some(extensions::SOURCE_MCP_JSON)
    );
    assert!(extensions::discover(imported));
}

/// A `.mcp.json` that does not parse is a reported issue, not a silent skip —
/// the operator opted this file in, so its absence from the effective set is
/// something they need to be told about.
#[test]
fn a_malformed_mcp_json_is_reported() {
    let dir = tempdir("global-bad-json");
    let path = dir.join(".mcp.json");
    std::fs::write(&path, "{not json").unwrap();

    let mut tier = tier_from(Vec::new());
    merge_mcp_json(&mut tier, std::slice::from_ref(&path), true);
    assert!(tier.servers.is_empty());
    assert_eq!(tier.issues.len(), 1);
    assert_eq!(tier.issues[0].path, path);
}

/// Project-local first, so a later `parse` loop's first-occurrence-wins gives
/// the project file precedence over `~/.claude/.mcp.json`.
#[test]
fn mcp_json_paths_prefer_the_project() {
    let dir = tempdir("global-paths");
    std::fs::write(dir.join(".mcp.json"), "{}").unwrap();
    let paths = crate::mcp::shared::discover_mcp_json_paths(&dir);
    assert_eq!(paths.first(), Some(&dir.join(".mcp.json")));
}
