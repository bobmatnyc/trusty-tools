//! Dynamic MCP service management tools (#244).
//!
//! Why: Previously the MCP service registry at `~/.trusty-agents/config.toml`
//! could only be edited by hand. To let coordinating agents (ctrl, pm)
//! adapt to a user's environment in-flight — "add the github MCP", "turn
//! off slack for now" — we expose five typed tools the LLM can call:
//! `mcp_list`, `mcp_add`, `mcp_remove`, `mcp_enable`, `mcp_disable`.
//! Each tool reads, mutates, and persists `GlobalConfig` via the methods on
//! that type, then returns a short confirmation string. Because every
//! prompt build re-reads the config from disk via `GlobalConfig::load()`, a
//! mutation made in turn N is reflected in the prompt for turn N+1
//! without any caching layer.
//! What: Split into focused submodules (#361):
//!   - `schema`   — `mcp_tool_definitions()` builds the five tool schemas.
//!   - `dispatch` — `dispatch_mcp_tool(name, args)` performs the action.
//!   - `executor` — `mcp_tool_executors()` adapts them to `ToolExecutor`.
//! Test: See unit tests at the bottom of this file.

mod dispatch;
mod executor;
mod schema;

#[allow(unused_imports)]
pub use dispatch::dispatch_mcp_tool;
pub use executor::mcp_tool_executors;
#[allow(unused_imports)]
pub use schema::mcp_tool_definitions;

#[cfg(test)]
mod tests {
    // Why: These tests hold `HOME_LOCK` (a `std::sync::Mutex`) across async
    // I/O to serialize global $HOME mutation between tests. See
    // `crate::test_env` for the rationale.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::mcp::{GlobalConfig, McpService};
    use crate::test_env::HOME_LOCK;
    use serde_json::json;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("trusty-agents-mcp-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn mcp_tool_definitions_returns_five_tools() {
        let tools = mcp_tool_definitions();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"mcp_list"));
        assert!(names.contains(&"mcp_add"));
        assert!(names.contains(&"mcp_remove"));
        assert!(names.contains(&"mcp_enable"));
        assert!(names.contains(&"mcp_disable"));
    }

    #[tokio::test]
    async fn dispatch_mcp_list_returns_registered_services() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // Seed config with one service.
        let mut cfg = GlobalConfig::default();
        cfg.add_service(McpService {
            name: "alpha".to_string(),
            description: "alpha service".to_string(),
            command: "a".to_string(),
            args: vec![],
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![],
        })
        .await
        .unwrap();

        let out = dispatch_mcp_tool("mcp_list", &json!({})).await;
        assert!(out.contains("alpha"));
        assert!(out.contains("Registered MCP services"));
    }

    #[tokio::test]
    async fn dispatch_mcp_add_persists_service() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let args = json!({
            "name": "beta",
            "description": "beta service",
            "transport": "stdio",
            "command": "beta-cmd",
            "args": ["mcp"],
            "tools": [
                {"name": "beta_op", "description": "do beta things"}
            ]
        });
        let out = dispatch_mcp_tool("mcp_add", &args).await;
        assert!(out.contains("Added"), "got: {out}");
        assert!(out.contains("beta"));

        // Verify it persists by reloading. Note: #245 — load() now returns
        // documented defaults (gworkspace-mcp + slack-user-proxy) when the
        // file doesn't exist, so mcp_add against a missing file persists
        // those defaults plus the new "beta" service (3 total).
        let reloaded = GlobalConfig::load().await;
        let beta = reloaded
            .mcp
            .services
            .iter()
            .find(|s| s.name == "beta")
            .expect("beta service present");
        assert_eq!(beta.tools.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_mcp_remove_removes_service() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // Add a service first.
        dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "gamma",
                "description": "g",
                "transport": "stdio",
                "command": "g"
            }),
        )
        .await;

        let out = dispatch_mcp_tool("mcp_remove", &json!({"name": "gamma"})).await;
        assert!(out.contains("Removed"), "got: {out}");

        // #245: defaults remain in the registry; assert gamma is gone.
        let reloaded = GlobalConfig::load().await;
        assert!(!reloaded.mcp.services.iter().any(|s| s.name == "gamma"));

        // Removing again returns the not-found message.
        let again = dispatch_mcp_tool("mcp_remove", &json!({"name": "gamma"})).await;
        assert!(again.contains("No service"), "got: {again}");
    }

    #[tokio::test]
    async fn dispatch_mcp_enable_disable_toggles_flag() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // Add a disabled service.
        dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "delta",
                "description": "d",
                "transport": "stdio",
                "command": "d",
                "enabled": false
            }),
        )
        .await;

        // Enable it.
        let enable_out = dispatch_mcp_tool("mcp_enable", &json!({"name": "delta"})).await;
        assert!(enable_out.contains("Enabled"), "got: {enable_out}");
        let cfg = GlobalConfig::load().await;
        let delta = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "delta")
            .expect("delta service present");
        assert!(delta.enabled);

        // Disable it.
        let disable_out = dispatch_mcp_tool("mcp_disable", &json!({"name": "delta"})).await;
        assert!(disable_out.contains("Disabled"), "got: {disable_out}");
        let cfg = GlobalConfig::load().await;
        let delta = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "delta")
            .expect("delta service present");
        assert!(!delta.enabled);

        // Unknown name returns not-found.
        let missing = dispatch_mcp_tool("mcp_enable", &json!({"name": "missing"})).await;
        assert!(missing.contains("No service"), "got: {missing}");
    }

    #[tokio::test]
    async fn dispatch_mcp_add_rejects_invalid_transport() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let out = dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "bad",
                "description": "x",
                "transport": "bogus"
            }),
        )
        .await;
        assert!(out.contains("Invalid"), "got: {out}");
        assert!(out.contains("transport"));
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_returns_error_string() {
        let out = dispatch_mcp_tool("mcp_bogus", &json!({})).await;
        assert!(out.contains("Unknown"));
    }
}
