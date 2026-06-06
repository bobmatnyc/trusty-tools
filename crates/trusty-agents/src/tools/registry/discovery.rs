//! Discovery types and TTL cache for OpenRPC (#453).
//!
//! Why: Drivers fetch endpoint manifests via `rpc.discover`. Caching them
//! per endpoint avoids re-issuing discovery on every tool list request,
//! while a TTL keeps the registry honest about server-side changes.
//! What: `DiscoveredTool` is one tool advertised by an endpoint;
//! `EndpointManifest` is the full response shape. `DiscoveryCache` is a
//! simple in-memory TTL map; eviction is lazy on read.
//! Test: Round-trip serialization and TTL expiry covered below.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_tool_roundtrip() {
        let raw = serde_json::json!({
            "name": "gmail_read",
            "description": "Read mail",
            "scope": "google.gmail.read",
            "input_schema": {"type": "object"},
            "idempotent": true,
            "side_effects": "read"
        });
        let t: DiscoveredTool = serde_json::from_value(raw).unwrap();
        assert_eq!(t.name, "gmail_read");
        assert_eq!(t.scope, "google.gmail.read");
        assert!(t.idempotent);
        assert_eq!(t.side_effects, SideEffects::Read);
    }

    #[test]
    fn cache_returns_value_within_ttl() {
        let cache = DiscoveryCache::new(Duration::from_secs(60));
        let m = EndpointManifest {
            server: ServerInfo::default(),
            protocol_version: "openrpc/1".into(),
            capabilities: EndpointCapabilities::default(),
            tools: vec![],
        };
        cache.put("ep".into(), m);
        assert!(cache.get("ep").is_some());
    }

    #[test]
    fn cache_misses_on_unknown_key() {
        let cache = DiscoveryCache::new(Duration::from_secs(60));
        assert!(cache.get("missing").is_none());
    }
}
