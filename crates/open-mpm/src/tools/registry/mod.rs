//! OpenRPC tool registry (#453).
//!
//! Why: External JSON-RPC 2.0 endpoints (`gworkspace`, `trusty-memory`, etc.)
//! advertise tools via `rpc.discover` and execute them via `tools/call` /
//! JSON-RPC 2.0 Batch. This module is the assembly point: read the
//! `[tool_registry]` config section, build one driver per `[[endpoints]]`,
//! optionally run `rpc.discover` eagerly, scope-filter the manifest by the
//! operator's `scopes` list, and wrap each surviving tool in a
//! `RegistryToolExecutor` so the harness can dispatch it through the same
//! `dyn ToolExecutor` path as in-process tools (#445/#447 surface).
//! What: `ToolRegistryBuilder` is the public entry point; `build()` returns
//! `Vec<Arc<dyn ToolExecutor>>` ready to register. Endpoints flagged
//! `enabled = false`, configured for unsupported drivers, or that fail
//! discovery at startup are skipped with a tracing warning so a single bad
//! endpoint can't take down the harness.
//! Test: `builder_with_no_endpoints_returns_empty`,
//! `scope_filter_drops_tools_outside_endpoint_scopes`,
//! `disabled_endpoint_is_skipped`.

pub mod adapter;
pub mod config;
pub mod direct;
pub mod discovery;
pub mod driver;
pub mod scope;

use std::sync::Arc;

use anyhow::Result;

use crate::mcp::config::GlobalConfig;
use crate::tools::traits::ToolExecutor;

use self::adapter::RegistryToolExecutor;
use self::config::{DriverKind, EndpointConfig, ToolRegistryConfig};
use self::direct::DirectDriver;
use self::discovery::DiscoveredTool;
use self::driver::ArcDriver;
use self::scope::{ScopePattern, filter_by_endpoint_scopes};

/// Public builder. Constructed from a `GlobalConfig`; `build()` consumes it.
///
/// Why: A builder keeps the construction parameters distinct from the
/// (async) work that creates drivers and runs discovery. Callers in
/// startup code only need to know `ToolRegistryBuilder::from_config(&cfg)
/// .build().await`.
/// What: Holds a clone of the `[tool_registry]` section (or `None`).
/// Test: `builder_with_no_endpoints_returns_empty`.
pub struct ToolRegistryBuilder {
    config: Option<ToolRegistryConfig>,
}

impl ToolRegistryBuilder {
    /// Construct a builder from the global config. If `[tool_registry]` is
    /// absent the builder is a no-op (returns empty on `build()`).
    pub fn from_config(global: &GlobalConfig) -> Self {
        Self {
            config: global.tool_registry.clone(),
        }
    }

    /// Build the list of `ToolExecutor`s.
    ///
    /// Why: Each endpoint contributes zero or more executors (one per
    /// discovered tool that survives scope filtering). Endpoint-level
    /// failures degrade gracefully: log a warning and continue.
    /// What: Iterates `endpoints`, instantiates the driver, runs
    /// `rpc.discover` if `eager_discovery = true` (otherwise skips —
    /// non-eager endpoints contribute nothing until lazy discovery is
    /// wired in a follow-up), filters tools by the operator-declared
    /// scope patterns, and wraps each surviving tool in a
    /// `RegistryToolExecutor`.
    pub async fn build(self) -> Result<Vec<Arc<dyn ToolExecutor>>> {
        let Some(cfg) = self.config else {
            return Ok(Vec::new());
        };
        let mut executors: Vec<Arc<dyn ToolExecutor>> = Vec::new();

        for ep in &cfg.endpoints {
            if !ep.enabled {
                tracing::debug!(endpoint = %ep.name, "tool registry: endpoint disabled, skipping");
                continue;
            }
            match build_endpoint(ep).await {
                Ok(mut endpoint_executors) => {
                    tracing::info!(
                        endpoint = %ep.name,
                        count = endpoint_executors.len(),
                        "tool registry: endpoint contributed tools",
                    );
                    executors.append(&mut endpoint_executors);
                }
                Err(e) => {
                    tracing::warn!(
                        endpoint = %ep.name,
                        error = %e,
                        "tool registry: endpoint init failed, skipping",
                    );
                }
            }
        }

        Ok(executors)
    }
}

/// Build executors for one endpoint. Returns an error if the driver itself
/// fails to construct or eager discovery is requested and fails.
async fn build_endpoint(ep: &EndpointConfig) -> Result<Vec<Arc<dyn ToolExecutor>>> {
    let driver: ArcDriver = match ep.driver {
        DriverKind::Direct => Arc::new(DirectDriver::new(ep)?),
        DriverKind::StdioMcp => {
            // Stdio-MCP driver lives in `crate::plugins::stdio_mcp` and is
            // already wired into the existing `mcp_service_tools` path.
            // Routing it through this registry is a follow-up so the
            // initial #453 surface stays self-contained.
            anyhow::bail!(
                "stdio-mcp driver not yet wired into the OpenRPC registry; use [[mcp.services]] for now"
            );
        }
    };

    if !ep.eager_discovery {
        tracing::debug!(
            endpoint = %ep.name,
            "tool registry: lazy discovery not yet implemented; endpoint contributes 0 tools",
        );
        return Ok(Vec::new());
    }

    let manifest = driver.discover().await?;
    let patterns: Vec<ScopePattern> = ep
        .scopes
        .iter()
        .map(|s| ScopePattern::new(s.clone()))
        .collect();

    let filtered: Vec<DiscoveredTool> =
        filter_by_endpoint_scopes(&patterns, &manifest.tools, |t| &t.scope);

    let dropped = manifest.tools.len().saturating_sub(filtered.len());
    if dropped > 0 {
        tracing::warn!(
            endpoint = %ep.name,
            kept = filtered.len(),
            dropped,
            "tool registry: dropped tools whose scope is not in operator-declared scopes",
        );
    }

    let executors: Vec<Arc<dyn ToolExecutor>> = filtered
        .into_iter()
        .map(|t| {
            Arc::new(RegistryToolExecutor::new(
                t,
                ep.name.clone(),
                driver.clone(),
            )) as Arc<dyn ToolExecutor>
        })
        .collect();

    Ok(executors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::GlobalConfig;
    use async_trait::async_trait;
    use serde_json::Value;

    use self::discovery::{EndpointCapabilities, EndpointManifest, ServerInfo, SideEffects};
    use self::driver::{BatchCall, BatchResult, RegistryDriver};

    /// Mock driver returning a fixed manifest. Used to exercise the
    /// scope-filter / adapter wiring without an HTTP server.
    struct MockDriver {
        name: String,
        manifest: EndpointManifest,
    }

    #[async_trait]
    impl RegistryDriver for MockDriver {
        fn endpoint_name(&self) -> &str {
            &self.name
        }
        async fn discover(&self) -> Result<EndpointManifest> {
            Ok(self.manifest.clone())
        }
        async fn call_tool(&self, name: &str, _args: Value) -> Result<Value> {
            Ok(serde_json::json!({"echoed_tool": name}))
        }
        fn capabilities(&self) -> &EndpointCapabilities {
            &self.manifest.capabilities
        }
    }

    fn tool(name: &str, scope: &str) -> DiscoveredTool {
        DiscoveredTool {
            name: name.into(),
            description: Some(format!("desc for {name}")),
            scope: scope.into(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            idempotent: false,
            side_effects: SideEffects::None,
        }
    }

    #[tokio::test]
    async fn builder_with_no_endpoints_returns_empty() {
        let global = GlobalConfig::default();
        let execs = ToolRegistryBuilder::from_config(&global)
            .build()
            .await
            .unwrap();
        assert!(execs.is_empty());
    }

    #[tokio::test]
    async fn builder_with_empty_registry_section_returns_empty() {
        let global = GlobalConfig {
            tool_registry: Some(ToolRegistryConfig::default()),
            ..Default::default()
        };
        let execs = ToolRegistryBuilder::from_config(&global)
            .build()
            .await
            .unwrap();
        assert!(execs.is_empty());
    }

    #[test]
    fn scope_filter_drops_tools_outside_endpoint_scopes() {
        let manifest = EndpointManifest {
            server: ServerInfo::default(),
            protocol_version: "openrpc/1".into(),
            capabilities: EndpointCapabilities::default(),
            tools: vec![
                tool("gmail_read", "google.gmail.read"),
                tool("outlook_read", "microsoft.outlook.read"),
                tool("cal_write", "google.calendar.write"),
            ],
        };
        let patterns = vec![ScopePattern::new("google.*")];
        let kept = filter_by_endpoint_scopes(&patterns, &manifest.tools, |t| &t.scope);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|t| t.name == "gmail_read"));
        assert!(kept.iter().any(|t| t.name == "cal_write"));
        assert!(!kept.iter().any(|t| t.name == "outlook_read"));
    }

    #[tokio::test]
    async fn registry_tool_executor_dispatches_through_driver() {
        let manifest = EndpointManifest {
            server: ServerInfo::default(),
            protocol_version: "openrpc/1".into(),
            capabilities: EndpointCapabilities::default(),
            tools: vec![tool("gmail_read", "google.gmail.read")],
        };
        let driver: ArcDriver = Arc::new(MockDriver {
            name: "mock".into(),
            manifest: manifest.clone(),
        });
        let exec =
            RegistryToolExecutor::new(manifest.tools[0].clone(), "mock".into(), driver.clone());
        assert_eq!(exec.name(), "gmail_read");
        let schema = exec.schema();
        assert_eq!(schema["type"], "function");
        assert_eq!(schema["function"]["name"], "gmail_read");
        let r = exec.execute(serde_json::json!({})).await;
        assert!(!r.is_error());
        assert!(r.content().contains("gmail_read"));
    }

    #[tokio::test]
    async fn default_batch_runs_in_parallel() {
        let manifest = EndpointManifest {
            server: ServerInfo::default(),
            protocol_version: "openrpc/1".into(),
            capabilities: EndpointCapabilities::default(),
            tools: vec![],
        };
        let driver: ArcDriver = Arc::new(MockDriver {
            name: "mock".into(),
            manifest,
        });
        let calls = vec![
            BatchCall {
                id: "1".into(),
                name: "a".into(),
                args: Value::Null,
            },
            BatchCall {
                id: "2".into(),
                name: "b".into(),
                args: Value::Null,
            },
        ];
        let results: Vec<BatchResult> = driver.call_tool_batch(calls).await;
        assert_eq!(results.len(), 2);
        for r in results {
            assert!(r.result.is_ok());
        }
    }
}
