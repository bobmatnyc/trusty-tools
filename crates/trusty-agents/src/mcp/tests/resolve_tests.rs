//! `crate::mcp::shared::resolve` — the assistant tier (#7454).
//!
//! These are the tests #7454 names as failing before the change: there was no
//! assistant tier at all, so every assistant saw the identical global list.

use std::path::Path;

use serial_test::serial;
use trusty_mcp::config::McpServerConfig;

use super::{stdio, tempdir};
use crate::assistants::mcp::{McpOverrides, read_overrides};
use crate::assistants::{AssistantHome, AssistantInstanceId};
use crate::mcp::extensions::{self, AuthKind, AuthSpec};
use crate::mcp::shared::{GlobalTier, McpTier, ResolvedMcp, resolve_in};
use crate::tools::mcp_live::gather_specs;

fn tier(servers: Vec<McpServerConfig>) -> GlobalTier {
    GlobalTier {
        servers,
        issues: Vec::new(),
        path: std::path::PathBuf::from("servers.toml"),
    }
}

/// The production resolution path with the assistants root injected — the two
/// live lookups `resolve_for_assistant` adds are all that is skipped.
fn resolve_at(root: &Path, global: GlobalTier, name: &str) -> ResolvedMcp {
    let home = AssistantHome::under(root, AssistantInstanceId::new(name).unwrap());
    let read = read_overrides(&home);
    let error = read.error.map(|detail| (read.path, detail));
    resolve_in(Some(name), global, read.overrides, error)
}

/// Write one assistant's `[mcp]` table into its home.
fn write_overrides(root: &Path, name: &str, body: &str) {
    let home = root.join(name);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), body).unwrap();
}

fn global_two() -> GlobalTier {
    tier(vec![
        stdio("github", "github-mcp"),
        stdio("granola", "granola-mcp"),
    ])
}

/// #7454 closure condition: an assistant-level disable hides a global server
/// from THAT assistant, and from nobody else.
#[test]
fn an_assistant_level_disable_hides_a_global_server() {
    let root = tempdir("resolve-disable");
    write_overrides(
        &root,
        "izzie",
        "id = \"izzie\"\n\n[mcp]\ndisabled = [\"github\"]\n",
    );
    write_overrides(&root, "cto-assistant", "id = \"cto-assistant\"\n");

    let izzie = resolve_at(&root, global_two(), "izzie");
    assert!(!izzie.has_usable("github"), "the disable must hide it");
    assert!(izzie.has_usable("granola"), "and hide nothing else");

    let other = resolve_at(&root, global_two(), "cto-assistant");
    assert!(
        other.has_usable("github"),
        "a second assistant with no override must still see it"
    );
}

/// The same fact, one layer down: the live-discovery specs an assistant's
/// registry is built from must reflect the disable too.
#[test]
fn a_disable_reaches_the_live_discovery_specs() {
    let root = tempdir("resolve-disable-specs");
    write_overrides(&root, "izzie", "[mcp]\ndisabled = [\"github\"]\n");

    let mut github = stdio("github", "github-mcp");
    extensions::set(&mut github.extensions, extensions::DISCOVER, &true);
    let mut granola = stdio("granola", "granola-mcp");
    extensions::set(&mut granola.extensions, extensions::DISCOVER, &true);

    let izzie = resolve_at(&root, tier(vec![github, granola]), "izzie");
    let names: Vec<String> = gather_specs(&izzie.usable())
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["granola"]);
}

/// #7454 closure condition: an assistant-level `Set` of a NEW server is
/// invisible to every other assistant.
#[test]
fn an_override_only_affects_its_own_assistant() {
    let root = tempdir("resolve-add");
    write_overrides(
        &root,
        "izzie",
        "[mcp]\n\n[[mcp.servers]]\nname = \"izzie-only\"\nenabled = true\n\n[mcp.servers.transport]\ntype = \"stdio\"\ncommand = \"izzie-bin\"\n",
    );
    write_overrides(&root, "cto-assistant", "id = \"cto-assistant\"\n");

    let izzie = resolve_at(&root, global_two(), "izzie");
    assert!(izzie.has_usable("izzie-only"));
    let added = izzie
        .statuses
        .iter()
        .find(|s| s.name == "izzie-only")
        .unwrap();
    assert_eq!(added.tier, McpTier::Assistant);

    let other = resolve_at(&root, global_two(), "cto-assistant");
    assert!(!other.has_usable("izzie-only"));
}

/// #7454 closure condition: an assistant-level `Set` RE-ENABLES a server the
/// global file disabled, for that assistant only.
#[test]
fn an_override_can_re_enable_a_globally_disabled_server() {
    let root = tempdir("resolve-reenable");
    write_overrides(
        &root,
        "izzie",
        "[mcp]\n\n[[mcp.servers]]\nname = \"github\"\nenabled = true\n\n[mcp.servers.transport]\ntype = \"stdio\"\ncommand = \"github-mcp\"\n",
    );
    write_overrides(&root, "cto-assistant", "id = \"cto-assistant\"\n");

    let mut github = stdio("github", "github-mcp");
    github.enabled = false;
    let global = || tier(vec![github.clone(), stdio("granola", "granola-mcp")]);

    assert!(resolve_at(&root, global(), "izzie").has_usable("github"));
    assert!(!resolve_at(&root, global(), "cto-assistant").has_usable("github"));
}

/// The resolver folds disables before sets, so naming one server in both lists
/// resolves as the more specific instruction.
#[test]
fn a_set_after_a_disable_wins() {
    let overrides = McpOverrides {
        servers: vec![stdio("github", "override-bin")],
        disabled: vec!["github".to_string()],
    };
    let resolved = resolve_in(Some("izzie"), global_two(), overrides, None);
    assert!(resolved.has_usable("github"));
    assert_eq!(
        extensions::stdio_parts(&resolved.servers[0]).map(|(c, _, _)| c),
        Some("override-bin")
    );
}

/// ADR-0060 decision 6, the middle case: a malformed `[mcp]` table falls back
/// to the global set and reports the OVERRIDE file, not the shared one.
#[test]
fn a_malformed_override_table_falls_back_with_a_named_issue() {
    let root = tempdir("resolve-bad-override");
    write_overrides(&root, "izzie", "[mcp]\ndisabled = \"not-a-list\"\n");

    let izzie = resolve_at(&root, global_two(), "izzie");
    assert!(
        izzie.has_usable("github"),
        "the global set must still apply"
    );
    assert_eq!(izzie.issues.len(), 1);
    assert_eq!(izzie.issues[0].tier, McpTier::Assistant);
    assert!(
        izzie.issues[0].path.ends_with("config.toml"),
        "the issue must name the override file, got {:?}",
        izzie.issues[0].path
    );
}

/// ADR-0060 decision 6, the third case: a server whose credential does not
/// resolve is SKIPPED with a per-server status — never fatal, and never a
/// silent absence.
#[test]
#[serial(mcp_auth_env)]
fn a_server_with_an_unresolvable_credential_is_skipped_not_fatal() {
    let mut paid = stdio("paid", "paid-bin");
    extensions::set(
        &mut paid.extensions,
        extensions::AUTH,
        &AuthSpec {
            kind: AuthKind::BearerEnv,
            env: Some("TRUSTY_TEST_MCP_RESOLVE_7454".into()),
            header: None,
        },
    );
    unsafe {
        std::env::remove_var("TRUSTY_TEST_MCP_RESOLVE_7454");
    }

    let resolved = resolve_in(
        Some("izzie"),
        tier(vec![paid, stdio("granola", "granola-mcp")]),
        McpOverrides::default(),
        None,
    );
    assert!(!resolved.has_usable("paid"));
    assert!(
        resolved.has_usable("granola"),
        "one broken credential must not cost the registry its other servers"
    );

    let status = resolved.statuses.iter().find(|s| s.name == "paid").unwrap();
    assert!(status.enabled, "it is configured, just not usable");
    assert!(!status.usable);
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("TRUSTY_TEST_MCP_RESOLVE_7454")),
        "got: {:?}",
        status.reason
    );
}

/// A disabled server is REPORTED, not dropped — DOC-57 §4.4 C-03.2.
#[test]
fn a_disabled_server_keeps_a_status_with_a_reason() {
    let mut off = stdio("off", "off-bin");
    off.enabled = false;
    let resolved = resolve_in(
        Some("izzie"),
        tier(vec![off]),
        McpOverrides::default(),
        None,
    );
    assert!(!resolved.has_usable("off"));
    let status = &resolved.statuses[0];
    assert!(!status.enabled);
    assert_eq!(status.reason.as_deref(), Some("disabled in configuration"));
}

/// A global-tier issue survives resolution — the assistant that starts with
/// zero connectors because the shared file is broken must carry the reason.
#[test]
fn a_global_issue_survives_resolution() {
    let mut global = tier(Vec::new());
    global.issues.push(crate::mcp::shared::McpIssue {
        tier: McpTier::Global,
        path: std::path::PathBuf::from("servers.toml"),
        detail: "broken".into(),
        remedy: "fix it".into(),
    });
    let resolved = resolve_in(Some("izzie"), global, McpOverrides::default(), None);
    assert!(resolved.servers.is_empty());
    assert_eq!(resolved.issues.len(), 1);
}
