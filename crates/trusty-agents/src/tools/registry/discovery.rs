//! Discovery types and TTL cache for OpenRPC (#453).
//!
//! Why: Drivers fetch endpoint manifests via `rpc.discover`. Caching them
//! per endpoint avoids re-issuing discovery on every tool list request,
//! while a TTL keeps the registry honest about server-side changes.
//! What: `DiscoveredTool` is one tool advertised by an endpoint;
//! `EndpointManifest` is the full response shape. `DiscoveryCache` is a
//! simple in-memory TTL map; eviction is lazy on read. `parse_manifest`
//! is the entry point that turns a raw `rpc.discover` JSON-RPC `result`
//! into an `EndpointManifest` — see its doc comment for why this is no
//! longer a bare `serde_json::from_value` call (#3577).
//! Test: Round-trip serialization and TTL expiry covered below;
//! `parse_manifest` wire-shape coverage in the `parse_manifest` tests
//! module.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::google_scope::dotted_scope_for_google_scopes;

/// `side_effects` field — kept as the OpenRPC enum.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SideEffects {
    #[default]
    None,
    Read,
    Write,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub scope: String,
    #[serde(default = "default_schema")]
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub side_effects: SideEffects,
}

fn default_schema() -> serde_json::Value {
    serde_json::json!({"type": "object"})
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EndpointCapabilities {
    #[serde(default)]
    pub supports_batch: bool,
    #[serde(default)]
    pub supports_streaming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointManifest {
    #[serde(default)]
    pub server: ServerInfo,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: EndpointCapabilities,
    #[serde(default)]
    pub tools: Vec<DiscoveredTool>,
}

fn default_protocol_version() -> String {
    "openrpc/1".to_string()
}

/// Parse a raw `rpc.discover` JSON-RPC `result` value into an
/// `EndpointManifest`.
///
/// Why (#3577): every real `rpc.discover` implementation in this repo
/// (`trusty-memory`, `trusty-search` — both via the shared
/// `trusty_common::mcp::openrpc::discover_response` builder — and
/// `trusty-gworkspace`, which predates that helper but emits the
/// structurally identical envelope) is standard OpenRPC 1.3.2:
/// `{ "openrpc": "1.3.2", "info": {...}, "methods": [...] }`. No endpoint
/// anywhere in this codebase emits the bespoke `{ "tools": [...] }` shape
/// this type used to deserialize directly via `serde_json::from_value`.
/// Because `EndpointManifest.tools` carried `#[serde(default)]`, that call
/// silently succeeded with `tools: vec![]` on every real payload instead
/// of erroring — the exact bug this function replaces. This is a
/// consume-side fix, not an emit-side one: OpenRPC is the converged,
/// deliberately-shared wire format across all three current/planned
/// `driver = "direct"` endpoints, so teaching the client to speak it
/// properly (rather than changing three server crates, one of which
/// exists specifically to avoid this duplication) is the durable fix —
/// the next OpenRPC server added to `[[tool_registry.endpoints]]` is
/// consumable for free.
///
/// What: Recognizes a top-level `methods` array (the real, standard
/// shape) and converts each entry via `method_to_tool`. A top-level
/// `tools` array (the originally-speced-but-never-implemented shape,
/// still exercised by this module's own round-trip tests and any future
/// endpoint that happens to emit it) is accepted as-is by delegating to
/// `serde_json::from_value`. When the raw value is a JSON object with
/// NEITHER key, that is precisely the condition that used to silently
/// deserialize to an empty tool list — this now returns a hard `Err`
/// naming the endpoint, the top-level keys actually present, and the
/// keys we know how to parse, so the failure is loud and diagnostic
/// instead of indistinguishable from "this server genuinely has zero
/// tools" (that legitimate case is handled one layer up, in
/// `mod.rs::build_endpoint`, as a warning once a manifest DID parse).
/// Test: `parse_manifest_methods_shape_gworkspace_fixture`,
/// `parse_manifest_tools_shape_still_accepted`,
/// `parse_manifest_unrecognized_shape_errors_loudly`.
pub fn parse_manifest(endpoint: &str, raw: &Value) -> Result<EndpointManifest> {
    let obj = raw.as_object().ok_or_else(|| {
        anyhow::anyhow!(
            "endpoint '{endpoint}': rpc.discover result is not a JSON object (got: {raw})"
        )
    })?;

    if let Some(methods) = obj.get("methods") {
        let methods = methods.as_array().ok_or_else(|| {
            anyhow::anyhow!(
                "endpoint '{endpoint}': rpc.discover `methods` is not an array (got: {methods})"
            )
        })?;
        let mut tools = Vec::with_capacity(methods.len());
        for (i, m) in methods.iter().enumerate() {
            match method_to_tool(endpoint, m) {
                Ok(t) => tools.push(t),
                Err(e) => {
                    tracing::warn!(
                        endpoint = %endpoint,
                        index = i,
                        error = %e,
                        "tool registry: skipping malformed OpenRPC method entry",
                    );
                }
            }
        }
        let server = obj
            .get("info")
            .and_then(|info| {
                Some(ServerInfo {
                    name: info.get("title")?.as_str()?.to_string(),
                    version: info
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .unwrap_or_default();
        return Ok(EndpointManifest {
            server,
            protocol_version: default_protocol_version(),
            capabilities: EndpointCapabilities::default(),
            tools,
        });
    }

    if obj.contains_key("tools") {
        return serde_json::from_value(raw.clone())
            .with_context(|| format!("endpoint '{endpoint}': malformed `tools`-shaped manifest"));
    }

    let present_keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    bail!(
        "endpoint '{endpoint}': rpc.discover result has neither a `methods` array (standard \
         OpenRPC, emitted by every trusty-* server today) nor a `tools` array (the legacy \
         shape). Top-level keys present: {present_keys:?}. This is the #3577 failure mode: a \
         wire-format we don't recognize must not silently deserialize to zero tools."
    );
}

/// Convert one OpenRPC `methods[]` entry into a `DiscoveredTool`.
///
/// Why: OpenRPC represents a tool's input as `params[]` (one named
/// `ContentDescriptor` per property) and its output as `result.schema`,
/// while `DiscoveredTool` wants a single JSON Schema object for each —
/// matching what `RegistryToolExecutor::schema()` hands the LLM as
/// OpenAI-style function parameters (`adapter.rs`). Scope resolution
/// prefers the generic `x-scopes` extension (already a dotted string,
/// emitted by `trusty-memory`/`trusty-search` via the shared
/// `trusty_common::mcp::openrpc` builder) and falls back to
/// `x-google-scopes` (OAuth URLs, `trusty-gworkspace`-specific) via
/// `google_scope::dotted_scope_for_google_scopes`.
/// What: Rebuilds `input_schema` as `{"type":"object","properties":{...},
/// "required":[...]}` from `params[]`; takes `output_schema` from
/// `result.schema` when present. `idempotent`/`side_effects` are not part
/// of the real wire format from any current server, so they keep their
/// struct defaults (`false` / `SideEffects::None`) — this is the same
/// `#[serde(default)]` behavior as before, but now applied to genuinely
/// optional metadata rather than to the `tools` array itself.
/// Test: `parse_manifest_methods_shape_gworkspace_fixture` covers the
/// full conversion; `method_to_tool_errors_on_missing_name` covers the
/// per-entry failure path.
fn method_to_tool(endpoint: &str, method: &Value) -> Result<DiscoveredTool> {
    let name = method
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("endpoint '{endpoint}': method entry missing `name`"))?
        .to_string();

    let description = method
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    if let Some(params) = method.get("params").and_then(|v| v.as_array()) {
        for p in params {
            let Some(pname) = p.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let schema = p.get("schema").cloned().unwrap_or(default_schema());
            properties.insert(pname.to_string(), schema);
            if p.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
                required.push(Value::String(pname.to_string()));
            }
        }
    }
    let input_schema = serde_json::json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": Value::Array(required),
    });

    let output_schema = method.get("result").and_then(|r| r.get("schema")).cloned();

    let scope = if let Some(scopes) = method.get("x-scopes").and_then(|v| v.as_array()) {
        scopes
            .iter()
            .find_map(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default()
    } else if let Some(scopes) = method.get("x-google-scopes").and_then(|v| v.as_array()) {
        let urls: Vec<String> = scopes
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        match dotted_scope_for_google_scopes(&urls) {
            Some(s) => s,
            None => {
                tracing::warn!(
                    endpoint = %endpoint,
                    tool = %name,
                    scopes = ?urls,
                    "tool registry: could not map x-google-scopes to a dotted scope \
                     (unrecognized OAuth scope URL); tool will be dropped by scope filtering",
                );
                String::new()
            }
        }
    } else {
        tracing::debug!(
            endpoint = %endpoint,
            tool = %name,
            "tool registry: method has neither x-scopes nor x-google-scopes; no scope \
             could be derived, tool will be dropped by scope filtering",
        );
        String::new()
    };

    Ok(DiscoveredTool {
        name,
        description,
        scope,
        input_schema,
        output_schema,
        idempotent: false,
        side_effects: SideEffects::default(),
    })
}

/// TTL cache keyed by endpoint name.
///
/// Why: Avoid hammering remote `rpc.discover` on every tool list request.
/// What: Single `Mutex<HashMap>` is fine — cache contention is dwarfed by
/// the network call it replaces.
/// Test: `cache_returns_value_within_ttl` / `cache_expires_after_ttl`.
pub struct DiscoveryCache {
    inner: Mutex<HashMap<String, (EndpointManifest, Instant)>>,
    ttl: Duration,
}

impl DiscoveryCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn get(&self, key: &str) -> Option<EndpointManifest> {
        let mut guard = self.inner.lock().ok()?;
        if let Some((manifest, ts)) = guard.get(key) {
            if ts.elapsed() <= self.ttl {
                return Some(manifest.clone());
            }
            // Expired — evict.
            guard.remove(key);
        }
        None
    }

    pub fn put(&self, key: String, manifest: EndpointManifest) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(key, (manifest, Instant::now()));
        }
    }
}

// Extracted to `discovery_tests.rs` to keep this file under the 500-SLOC
// production cap (#3577 added `parse_manifest` + `method_to_tool`); see
// `scripts/check_line_cap.sh`.
#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
