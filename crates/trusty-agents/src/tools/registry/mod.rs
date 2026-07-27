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
// #3987 (option C): dead scope-pattern diagnostics. Lives beside `scope`
// because it is the "why did this pattern match nothing" half of the same
// matching model, but is kept in its own module so the pure matcher stays
// free of vocabulary knowledge.
pub mod dead_scope;
pub mod direct;
pub mod discovery;
pub mod driver;
mod google_scope;
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
    /// What: Thin wrapper over [`Self::build_with_scope_vocabulary`] that
    /// discards the vocabulary — for the callers that only register tools.
    pub async fn build(self) -> Result<Vec<Arc<dyn ToolExecutor>>> {
        Ok(self.build_with_scope_vocabulary().await?.0)
    }

    /// Build the executors AND report the scope vocabulary every endpoint
    /// advertised, BEFORE operator scope filtering (#3987).
    ///
    /// Why: `dead_scope`'s diagnostic needs to know what an endpoint COULD
    /// advertise, not what an operator's `[[endpoints]].scopes` policy let
    /// through. Those differ, and the difference is a false-positive
    /// generator: an operator who narrows a non-Google endpoint to, say,
    /// `memory.read` makes the `memory` namespace *known* (so
    /// `dead_scope`'s namespace guard starts judging it) while
    /// simultaneously removing `memory.write` from the registry — and an
    /// agent's perfectly legitimate `memory.write` grant would then be
    /// reported dead, with "nearest reachable: memory.read" as advice that
    /// silently recommends dropping a capability the operator deliberately
    /// revoked. Denied-by-policy and cannot-possibly-exist are different
    /// conditions and must not share a diagnostic. Returning the
    /// UNFILTERED vocabulary keeps `dead_scope` reporting only the latter.
    ///
    /// **This property must hold before the diagnostic is escalated to a
    /// hard load error** (see `dead_scope`'s module doc): failing a load
    /// closed on an operator's own narrowing policy would turn a
    /// deliberate configuration choice into an outage.
    /// What: same work as `build`, plus a de-duplicated, sorted list of every
    /// non-empty `scope` string in every successfully-discovered endpoint
    /// manifest. Discovery runs exactly once per endpoint — the vocabulary is
    /// collected from the same manifest the filter consumes, never by a second
    /// `rpc.discover` call.
    /// Test: `unfiltered_vocabulary_survives_operator_scope_narrowing`.
    pub async fn build_with_scope_vocabulary(
        self,
    ) -> Result<(Vec<Arc<dyn ToolExecutor>>, Vec<String>)> {
        let Some(cfg) = self.config else {
            return Ok((Vec::new(), Vec::new()));
        };
        let mut executors: Vec<Arc<dyn ToolExecutor>> = Vec::new();
        let mut vocabulary: std::collections::BTreeSet<String> = Default::default();

        for ep in &cfg.endpoints {
            if !ep.enabled {
                tracing::debug!(endpoint = %ep.name, "tool registry: endpoint disabled, skipping");
                continue;
            }
            match build_endpoint(ep).await {
                Ok((mut endpoint_executors, advertised)) => {
                    tracing::info!(
                        endpoint = %ep.name,
                        count = endpoint_executors.len(),
                        advertised_scopes = advertised.len(),
                        "tool registry: endpoint contributed tools",
                    );
                    executors.append(&mut endpoint_executors);
                    vocabulary.extend(advertised);
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

        Ok((executors, vocabulary.into_iter().collect()))
    }
}

/// Build executors for one endpoint, plus the scope vocabulary it advertised
/// BEFORE operator filtering (#3987 — see
/// [`ToolRegistryBuilder::build_with_scope_vocabulary`]).
///
/// Returns an error if the driver itself fails to construct or eager
/// discovery is requested and fails.
async fn build_endpoint(ep: &EndpointConfig) -> Result<(Vec<Arc<dyn ToolExecutor>>, Vec<String>)> {
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
        return Ok((Vec::new(), Vec::new()));
    }

    let manifest = driver.discover().await?;

    // #3987: captured BEFORE the operator scope filter below, on purpose —
    // this is what the endpoint CAN advertise, which is the only honest
    // baseline for "could this pattern ever match anything". Tools whose
    // scope could not be derived carry `""` (see `discovery::method_to_tool`,
    // which warns) and are excluded: an empty string is a discovery failure,
    // not a vocabulary entry.
    let advertised_scopes: Vec<String> = manifest
        .tools
        .iter()
        .filter(|t| !t.scope.trim().is_empty())
        .map(|t| t.scope.clone())
        .collect();

    // #3577: zero tools from a manifest we successfully understood is a
    // DIFFERENT condition from zero tools because the wire shape didn't
    // parse (that case is a hard `Err` from `driver.discover()`, above,
    // and never reaches this line). A well-formed manifest can still
    // legitimately advertise nothing — a server mid-feature-rollout, for
    // example — so this is a warning, not an error: it must be loud
    // enough that an operator staring at "gworkspace contributed 0 tools"
    // in the startup log has an immediate next step, without treating a
    // genuinely-empty server as a hard startup failure.
    if manifest.tools.is_empty() {
        tracing::warn!(
            endpoint = %ep.name,
            "tool registry: endpoint '{}' discovery succeeded but advertised zero tools \
             — verify the server implements the tools it's expected to, and that its \
             rpc.discover response shape matches what this registry parses \
             (see crates/trusty-agents/src/tools/registry/discovery.rs)",
            ep.name,
        );
    }

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

    Ok((executors, advertised_scopes))
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

    /// #3987: the scope vocabulary `build_endpoint` reports must be the
    /// endpoint's ADVERTISED set, unaffected by the operator's narrowing.
    ///
    /// Why: this is the property that keeps `dead_scope` from reporting a
    /// grant the operator deliberately revoked as a broken pattern. The
    /// executors ARE narrowed (policy is enforced); only the vocabulary the
    /// diagnostic reasons over stays whole.
    #[tokio::test]
    async fn unfiltered_vocabulary_survives_operator_scope_narrowing() {
        let manifest = EndpointManifest {
            server: ServerInfo::default(),
            protocol_version: "openrpc/1".into(),
            capabilities: EndpointCapabilities::default(),
            tools: vec![
                tool("mem_read", "memory.read"),
                tool("mem_write", "memory.write"),
                // A tool whose scope could not be derived: excluded from the
                // vocabulary, since `""` is a discovery failure not a scope.
                tool("mystery", ""),
            ],
        };
        // Reproduce `build_endpoint`'s two computations against a narrowing
        // operator policy that admits only `memory.read`.
        let advertised: Vec<String> = manifest
            .tools
            .iter()
            .filter(|t| !t.scope.trim().is_empty())
            .map(|t| t.scope.clone())
            .collect();
        let patterns = vec![ScopePattern::new("memory.read")];
        let filtered = filter_by_endpoint_scopes(&patterns, &manifest.tools, |t| &t.scope);

        assert_eq!(
            filtered.len(),
            1,
            "operator policy must still be enforced on the executors"
        );
        assert_eq!(
            advertised,
            vec!["memory.read".to_string(), "memory.write".to_string()],
            "the reported vocabulary must survive the narrowing (and drop empty scopes)"
        );

        // End to end: the narrowed endpoint must not make `memory.write` look
        // dead once the advertised vocabulary is unioned in.
        let reachable = self::dead_scope::reachable_scopes(advertised.iter().map(String::as_str));
        assert!(
            self::dead_scope::dead_scope_patterns(&[ScopePattern::new("memory.write")], &reachable)
                .is_empty()
        );
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
