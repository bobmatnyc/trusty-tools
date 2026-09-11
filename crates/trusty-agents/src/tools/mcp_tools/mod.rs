//! Dynamic MCP server management tools (#244, #7454).
//!
//! Why: the MCP server list can otherwise only be edited by hand. To let
//! coordinating agents (ctrl, pm) adapt to a user's environment in-flight —
//! "add the github MCP", "turn off slack for now" — five typed tools are
//! exposed to the LLM: `mcp_list`, `mcp_add`, `mcp_remove`, `mcp_enable`,
//! `mcp_disable`. Each reads, mutates and persists the SHARED server file
//! (`~/.trusty-tools/mcp/servers.toml`, ADR-0060) and returns a short
//! confirmation string. Every prompt build re-resolves that file, so a
//! mutation in turn N is reflected in turn N+1 with no caching layer.
//!
//! These tools write the GLOBAL tier. Per-assistant overrides are a user
//! setting, made through `PUT /api/assistants/:id/mcp` — see `dispatch`'s
//! module doc for why an LLM-originated call must not make one.
//!
//! What: split into focused submodules (#361):
//!   - `schema`   — `mcp_tool_definitions()` builds the five tool schemas.
//!   - `dispatch` — `dispatch_mcp_tool(name, args)` performs the action.
//!   - `executor` — `mcp_tool_executors()` adapts them to `ToolExecutor`.
//! Test: see the unit tests at the bottom of this file.

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
    use crate::mcp::extensions;
    use crate::test_env::HOME_LOCK;
    use serde_json::json;
    use std::path::PathBuf;
    use trusty_mcp::config::{McpConfigFile, McpServerConfig};

    fn tempdir() -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("trusty-agents-mcp-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Read back what the tools wrote, from the shared file itself.
    fn reload() -> Vec<McpServerConfig> {
        let path = trusty_mcp::config::default_path().unwrap();
        McpConfigFile::load_or_default(&path).unwrap().servers
    }

    fn find(name: &str) -> McpServerConfig {
        reload()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} server present"))
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
    async fn dispatch_mcp_list_returns_configured_servers() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "alpha",
                "description": "alpha service",
                "transport": "stdio",
                "command": "a"
            }),
        )
        .await;

        let out = dispatch_mcp_tool("mcp_list", &json!({})).await;
        assert!(out.contains("alpha"), "got: {out}");
        assert!(out.contains("Configured MCP servers"), "got: {out}");
    }

    /// #7454: a disabled server must still be LISTED, marked — hiding it
    /// would tell the model a connector is absent when it is one flag away.
    #[tokio::test]
    async fn dispatch_mcp_list_renders_disabled_servers() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "quiet",
                "description": "off for now",
                "transport": "stdio",
                "command": "q",
                "enabled": false
            }),
        )
        .await;

        let out = dispatch_mcp_tool("mcp_list", &json!({})).await;
        assert!(out.contains("quiet"), "got: {out}");
        assert!(out.contains("(disabled)"), "got: {out}");
    }

    #[tokio::test]
    async fn dispatch_mcp_add_persists_server() {
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

        let beta = find("beta");
        assert_eq!(extensions::tools(&beta).len(), 1);
        assert_eq!(
            extensions::stdio_parts(&beta).map(|(c, _, _)| c.to_string()),
            Some("beta-cmd".to_string())
        );
    }

    #[tokio::test]
    async fn dispatch_mcp_remove_removes_server() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
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
        assert!(!reload().iter().any(|s| s.name == "gamma"));

        let again = dispatch_mcp_tool("mcp_remove", &json!({"name": "gamma"})).await;
        assert!(again.contains("No MCP server"), "got: {again}");
    }

    #[tokio::test]
    async fn dispatch_mcp_enable_disable_toggles_flag() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
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

        let enable_out = dispatch_mcp_tool("mcp_enable", &json!({"name": "delta"})).await;
        assert!(enable_out.contains("Enabled"), "got: {enable_out}");
        assert!(find("delta").enabled);

        let disable_out = dispatch_mcp_tool("mcp_disable", &json!({"name": "delta"})).await;
        assert!(disable_out.contains("Disabled"), "got: {disable_out}");
        assert!(!find("delta").enabled);

        let missing = dispatch_mcp_tool("mcp_enable", &json!({"name": "missing"})).await;
        assert!(missing.contains("No MCP server"), "got: {missing}");
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

    /// Why: SECURITY (#3266) — `env` is not a declared `mcp_add` schema field
    /// (see `schema.rs`), so an LLM-originated tool call that includes one
    /// anyway must never reach the persisted server. Honouring it would let a
    /// prompt-injected call plant arbitrary environment variables into a
    /// spawned server's process.
    /// What: call `mcp_add` with a poisoned `env`; assert the persisted
    /// server's transport carries no environment at all.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_mcp_add_strips_undeclared_env_field() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let args = json!({
            "name": "epsilon",
            "description": "epsilon service",
            "transport": "stdio",
            "command": "epsilon-cmd",
            "env": {
                "LD_PRELOAD": "/tmp/evil.so",
                "OPENROUTER_API_KEY": "stolen"
            }
        });
        let out = dispatch_mcp_tool("mcp_add", &args).await;
        assert!(out.contains("Added"), "got: {out}");

        let epsilon = find("epsilon");
        let env = extensions::stdio_parts(&epsilon)
            .map(|(_, _, env)| env.clone())
            .expect("stdio transport");
        assert!(
            env.is_empty(),
            "env must be stripped from LLM-originated mcp_add calls, got: {env:?}"
        );
    }

    /// Why: SECURITY (#3266 follow-up) — `discover` IS a declared `mcp_add`
    /// schema field (unlike `env`), but honouring `discover: true` from an
    /// LLM-originated call is a trust-gate bypass: `gather_specs`
    /// (`tools/mcp_live/spec.rs`) auto-spawns any enabled, stdio,
    /// `discover = true` server with no trust gate of its own.
    /// What: call `mcp_add` with `discover: true` and an attacker-chosen
    /// command; assert the persisted server carries no `discover` extension
    /// (the code additionally emits a `tracing::warn!` on this path).
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_mcp_add_strips_discover_true() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let args = json!({
            "name": "zeta",
            "description": "zeta service",
            "transport": "stdio",
            "command": "evil-binary",
            "discover": true
        });
        let out = dispatch_mcp_tool("mcp_add", &args).await;
        assert!(out.contains("Added"), "got: {out}");

        let zeta = find("zeta");
        assert!(
            !extensions::discover(&zeta),
            "discover must be forced to false for LLM-originated mcp_add calls"
        );
    }

    /// #7454: the shared file is multi-process state, so a read outside the
    /// write is a LOST update — two `mcp_add` calls both read the same list
    /// and the later save drops the earlier server. `McpConfigFile::save`'s
    /// tmp+rename prevents a torn file, never this. Eight concurrent adds of
    /// distinct names must all survive; any missing name is the lost update.
    ///
    /// Threads rather than tasks, because the mutation is synchronous and each
    /// one takes its own file descriptor on the sibling `.lock` — which is what
    /// makes `flock` serialize them within one process as well as across
    /// processes.
    #[test]
    fn concurrent_mcp_add_calls_all_survive() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }

        std::thread::scope(|scope| {
            for index in 0..8 {
                scope.spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap();
                    let out = runtime.block_on(dispatch_mcp_tool(
                        "mcp_add",
                        &json!({
                            "name": format!("racer-{index}"),
                            "description": "concurrent",
                            "transport": "stdio",
                            "command": "racer"
                        }),
                    ));
                    assert!(out.contains("Added"), "got: {out}");
                });
            }
        });

        let names: Vec<String> = reload().into_iter().map(|s| s.name).collect();
        for index in 0..8 {
            let expected = format!("racer-{index}");
            assert!(
                names.contains(&expected),
                "lost update: {expected} is missing from {names:?}"
            );
        }
    }

    /// The lock does not change the no-op answer: enabling an already-enabled
    /// server still reports the change it did make (the server existed), and a
    /// name that matches nothing still writes nothing.
    #[tokio::test]
    async fn dispatch_mcp_enable_on_a_missing_server_writes_nothing() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let path = trusty_mcp::config::default_path().unwrap();

        let out = dispatch_mcp_tool("mcp_enable", &json!({"name": "ghost"})).await;
        assert!(out.contains("No MCP server"), "got: {out}");
        assert!(
            !path.exists(),
            "a declined mutation created the shared file anyway"
        );
    }
}
