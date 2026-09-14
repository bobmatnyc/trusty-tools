//! Assembling the set and registering it (#5428).

use tempfile::tempdir;

use super::support::{global_file, one_stdio_server};
use crate::mcp::{McpToolSet, register_configured_tools};
use crate::tools::registry::ToolRegistry;

/// Why: nothing configured must be an empty set with no findings — the
/// no-op case every machine without a `servers.toml` takes.
#[tokio::test]
async fn an_unconfigured_run_registers_nothing_and_reports_nothing() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");

    let set = McpToolSet::load_at(&home.path().join("servers.toml"), project.path()).await;
    let mut registry = ToolRegistry::new();
    let diagnostics = register_configured_tools(&mut registry, &set);

    assert!(set.is_empty());
    assert!(diagnostics.statuses.is_empty());
    assert!(diagnostics.issues.is_empty());
    assert_eq!(diagnostics.usable_count(), 0);
}

/// Why: the isolation contract. Two servers, both unstartable — each must be
/// reported on its own and neither may abort the load or panic.
#[tokio::test]
async fn a_failed_server_does_not_stop_the_others() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let mut text = one_stdio_server("ghost-a", "/nonexistent/mcp/a-5428");
    text.push_str(&one_stdio_server("ghost-b", "/nonexistent/mcp/b-5428"));
    let global = global_file(home.path(), &text);

    let set = McpToolSet::load_at(&global, project.path()).await;
    let diagnostics = set.diagnostics();

    assert_eq!(diagnostics.statuses.len(), 2, "both are reported");
    assert!(diagnostics.statuses.iter().all(|s| !s.usable));
    assert!(
        diagnostics.statuses[1]
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("b-5428")),
        "the second server was still attempted: {:?}",
        diagnostics.statuses[1],
    );
    assert!(set.tool_names().is_empty());
}

/// Why: a malformed global file must reach the caller as a finding, not as a
/// quiet empty catalog — the Fail-Open Check, asserted at the level the two
/// registration sites actually see.
#[tokio::test]
async fn a_malformed_global_file_reaches_the_diagnostics() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(home.path(), "not = = toml [[[");

    let set = McpToolSet::load_at(&global, project.path()).await;
    let mut registry = ToolRegistry::new();
    let diagnostics = register_configured_tools(&mut registry, &set);

    assert!(diagnostics.tools.is_empty());
    assert!(
        !diagnostics.issues.is_empty(),
        "a broken file MUST be reported, never read as an empty catalog",
    );
    diagnostics.log();
}

/// Why: `load` is the only path that reads the OPERATOR's real
/// `~/.trusty-tools/mcp/servers.toml`, and a `cargo test` run must never spawn
/// their actual daemons as a side effect — the same production-state refusal
/// `trusty_common::running_under_test_harness` gates the search-index writes
/// with (#4255). Without it, every full-pipeline test in this crate starts the
/// developer's whole MCP fleet and blows its deadline.
#[tokio::test]
async fn the_live_path_loads_nothing_under_a_test_harness() {
    let project = tempdir().expect("tempdir");

    let set = McpToolSet::load(project.path()).await;

    assert!(set.is_empty());
    assert!(set.diagnostics().statuses.is_empty());
}
