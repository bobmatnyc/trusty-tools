//! The MCP loader against a REAL stdio MCP server (#5428).
//!
//! Why: the loader's whole contract is transport — spawn a subprocess, run the
//! JSON-RPC handshake, register what it advertises, dispatch a call back to
//! it. A mocked client would prove none of that. `examples/mcp_fixture_server.rs`
//! is the counterpart server; `cargo test` builds it, `cargo install` does not
//! ship it.
//! What: config written into a tempdir, never the developer's real
//! `~/.trusty-tools/mcp/servers.toml`.
//! Test: this file.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::tempdir;
use trusty_code::mcp::{McpToolSet, register_configured_tools};
use trusty_code::tools::registry::ToolRegistry;
use trusty_code::tools::traits::{ToolExecutor, ToolResult};

/// The fixture server's built path.
///
/// Why: `CARGO_BIN_EXE_*` covers `[[bin]]` targets only, and this fixture is
/// deliberately an example so it never ships. The path is derived from the
/// test binary's own location, so it follows a custom `CARGO_TARGET_DIR` too.
/// An absent binary FAILS rather than skipping: a gate that silently does not
/// run is not a gate.
fn fixture_binary() -> String {
    let mut path: PathBuf = std::env::current_exe().expect("the test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("examples");
    path.push(format!(
        "mcp_fixture_server{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        path.exists(),
        "the fixture example is not built at {}; `cargo test` builds examples",
        path.display(),
    );
    path.display().to_string()
}

/// A global `servers.toml` declaring one stdio server per `(name, command)`.
fn global_config(dir: &std::path::Path, servers: &[(&str, &str)]) -> PathBuf {
    let mut text = String::new();
    for (name, command) in servers {
        text.push_str(&format!(
            "[[servers]]\nname = {name:?}\n[servers.transport]\ntype = \"stdio\"\ncommand = {command:?}\n[servers.transport.env]\nTCODE_FIXTURE_TOKEN = \"fixture-token-5428\"\n\n"
        ));
    }
    let path = dir.join("servers.toml");
    std::fs::write(&path, text).expect("writing the global fixture");
    path
}

/// A tool that only has to occupy a name.
struct Squatter(String);

#[async_trait]
impl ToolExecutor for Squatter {
    fn name(&self) -> &str {
        &self.0
    }
    fn schema(&self) -> Value {
        json!({"type": "function", "function": {"name": self.0}})
    }
    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::ok("squatted")
    }
}

/// Why: the whole slice, end to end — a configured server is spawned, its
/// tools are discovered and registered under `mcp__<server>__<tool>`, and a
/// dispatch through the registry reaches the real subprocess and comes back.
#[tokio::test]
async fn end_to_end_dispatch_echoes_through_the_registry() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_config(home.path(), &[("fixture", &fixture_binary())]);

    let set = McpToolSet::load_at(&global, project.path()).await;
    let mut registry = ToolRegistry::new();
    let diagnostics = register_configured_tools(&mut registry, &set);

    assert!(diagnostics.issues.is_empty(), "{:?}", diagnostics.issues);
    assert_eq!(diagnostics.usable_count(), 1);
    assert_eq!(
        diagnostics.tools,
        vec!["mcp__fixture__echo", "mcp__fixture__echo_env"],
    );

    let result = registry
        .dispatch("mcp__fixture__echo", json!({"message": "hello 5428"}))
        .await;
    assert!(!result.is_error(), "{}", result.content());
    assert_eq!(result.content(), "hello 5428");

    // The advertised schema reaches the model unmodified.
    let schemas = registry.schemas();
    let echo = schemas
        .iter()
        .find(|s| s["function"]["name"] == "mcp__fixture__echo")
        .expect("the echo schema is advertised");
    assert_eq!(
        echo["function"]["parameters"]["required"],
        json!(["message"])
    );
}

/// Why: a configured `env` carries the credential an MCP server needs, and it
/// has to actually reach the child — an overlay that silently did not apply
/// would look exactly like a server misbehaving.
#[tokio::test]
async fn a_configured_env_reaches_the_child_process() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_config(home.path(), &[("fixture", &fixture_binary())]);

    let set = McpToolSet::load_at(&global, project.path()).await;
    let mut registry = ToolRegistry::new();
    register_configured_tools(&mut registry, &set);

    let result = registry.dispatch("mcp__fixture__echo_env", json!({})).await;

    assert_eq!(result.content(), "fixture-token-5428");
}

/// Why: the isolation ruling. One unstartable server must cost exactly itself
/// — the healthy sibling's tools still register and still dispatch.
#[tokio::test]
async fn spawn_failure_isolates_the_healthy_server() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_config(
        home.path(),
        &[
            ("ghost", "/nonexistent/mcp/binary/xyzzy-5428"),
            ("fixture", &fixture_binary()),
        ],
    );

    let set = McpToolSet::load_at(&global, project.path()).await;
    let mut registry = ToolRegistry::new();
    let diagnostics = register_configured_tools(&mut registry, &set);

    assert_eq!(diagnostics.statuses.len(), 2, "both are reported");
    let ghost = &diagnostics.statuses[0];
    assert_eq!(ghost.name, "ghost");
    assert!(!ghost.usable);
    assert!(ghost.detail.as_deref().is_some_and(|d| d.contains("xyzzy")));
    assert!(diagnostics.statuses[1].usable, "the sibling is unaffected");

    let result = registry
        .dispatch("mcp__fixture__echo", json!({"message": "still here"}))
        .await;
    assert_eq!(result.content(), "still here");
}

/// Why: `ToolRegistry::register` overwrites a duplicate outside a debug build,
/// so a colliding MCP tool could silently replace a tool the run depends on.
/// It must be skipped, the incumbent left intact, and the caller told.
#[tokio::test]
async fn a_name_already_in_the_registry_is_skipped() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_config(home.path(), &[("fixture", &fixture_binary())]);

    let set = McpToolSet::load_at(&global, project.path()).await;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(Squatter("mcp__fixture__echo".to_string())));
    let diagnostics = register_configured_tools(&mut registry, &set);

    assert_eq!(
        registry
            .dispatch("mcp__fixture__echo", json!({"message": "hi"}))
            .await
            .content(),
        "squatted",
        "registration must never overwrite an existing tool",
    );
    assert!(
        !diagnostics
            .tools
            .contains(&"mcp__fixture__echo".to_string()),
        "a skipped tool is not reported as registered: {:?}",
        diagnostics.tools,
    );
    assert!(
        diagnostics
            .issues
            .iter()
            .any(|i| i.detail.contains("mcp__fixture__echo")),
        "the collision is reported: {:?}",
        diagnostics.issues,
    );
    // The server's other tool still registered.
    assert_eq!(
        registry
            .dispatch("mcp__fixture__echo_env", json!({}))
            .await
            .content(),
        "fixture-token-5428",
    );
}

/// Why: the trust gate, over a real spawn. A project entry that keeps a
/// trusted NAME but changes the command must not be what runs — and the
/// refusal must be visible.
#[tokio::test]
async fn a_project_set_with_a_new_command_never_spawns() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_config(home.path(), &[("fixture", &fixture_binary())]);
    let project_dir = project.path().join(".trusty-code");
    std::fs::create_dir_all(&project_dir).expect("project config dir");
    std::fs::write(
        project_dir.join("mcp.toml"),
        "[[servers]]\nname = \"fixture\"\n[servers.transport]\ntype = \"stdio\"\ncommand = \"/nonexistent/mcp/attacker-5428\"\n",
    )
    .expect("project fixture");

    let set = McpToolSet::load_at(&global, project.path()).await;
    let mut registry = ToolRegistry::new();
    let diagnostics = register_configured_tools(&mut registry, &set);

    assert_eq!(diagnostics.usable_count(), 1, "the GLOBAL entry ran");
    assert_eq!(
        registry
            .dispatch("mcp__fixture__echo", json!({"message": "global wins"}))
            .await
            .content(),
        "global wins",
    );
    assert!(
        diagnostics
            .issues
            .iter()
            .any(|i| i.detail.contains("fixture")),
        "the refusal is reported: {:?}",
        diagnostics.issues,
    );
}
