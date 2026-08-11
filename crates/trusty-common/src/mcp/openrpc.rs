//! Shared OpenRPC 1.3.2 service-description helpers.
//!
//! Why: Every trusty-* MCP server (trusty-memory, trusty-search,
//! trusty-gworkspace, …) exposes the same JSON-RPC method names for its
//! tools and benefits from a machine-readable `rpc.discover` manifest so
//! orchestrators such as trusty-agents can route requests and reason about
//! per-tool scopes without bespoke adapters. The OpenRPC document shape is
//! identical across servers — only the title/version and the scope mapping
//! differ. Consolidating the builder here removes ~100 lines of duplication
//! per server and ensures every server emits an identically-structured
//! document.
//!
//! What: A small `OpenRpcBuilder` plus the convenience `discover_response`
//! function. Both accept the server's tool list (one JSON object per tool
//! with `name`, `description`, and `inputSchema`) and a scope-resolver
//! callback. Output is the exact OpenRPC 1.3.2 document already produced
//! by `trusty-gworkspace/src/openrpc.rs` — `{ openrpc, info, methods }` —
//! with each `methods[i]` carrying an `x-scopes` extension field so the
//! scope vocabulary is server-defined (Google OAuth, memory.read /
//! memory.write, search scopes, …).
//!
//! Test: `cargo test -p trusty-mcp-core openrpc` covers builder output,
//! the `x-scopes` extension, and a representative tool with required
//! params being flattened into the OpenRPC `params` array.

use crate::mcp::service::ServiceDescriptor;
use serde_json::{Map, Value, json};

/// Fluent builder for an OpenRPC 1.3.2 service-description document.
///
/// Why: Most servers want to set a title/version and then push a handful
/// of tools with their scopes. The builder gives that ergonomic surface
/// without forcing callers to assemble JSON by hand.
/// What: Accumulates an internal `methods` array; `build()` wraps it in
/// the OpenRPC envelope. Each tool is converted via `tool_to_method`.
/// Test: `builder_emits_valid_envelope` and
/// `builder_attaches_x_scopes_extension`.
pub struct OpenRpcBuilder {
    title: String,
    version: String,
    description: Option<String>,
    methods: Vec<Value>,
}

impl OpenRpcBuilder {
    /// Start a new builder.
    ///
    /// Why: Title and version are mandatory OpenRPC `info` fields, so we
    /// take them in the constructor rather than as optional setters.
    /// What: Stores `title` and `version`; methods list starts empty.
    /// Test: `builder_emits_valid_envelope`.
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            description: None,
            methods: Vec::new(),
        }
    }

    /// Set an optional human-readable description for `info.description`.
    ///
    /// Why: Lets the manifest carry a one-liner explaining what the server
    /// does, surfaced by OpenRPC playgrounds and `rpc.discover` consumers.
    /// What: Stores the string; emitted only when non-empty.
    /// Test: `builder_includes_description_when_set`.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add one tool to the manifest with its scope list.
    ///
    /// Why: Servers register tools incrementally and may compute scopes
    /// from a lookup table; the builder accepts both in one call.
    /// What: Converts the MCP tool JSON (with `name`, `description`,
    /// `inputSchema`) into an OpenRPC `Method` and appends it to the list.
    /// `scopes` is stored under `x-scopes` (always present, possibly empty).
    /// Test: `builder_attaches_x_scopes_extension`.
    pub fn add_tool(mut self, tool: &Value, scopes: &[&str]) -> Self {
        let scope_vec: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
        self.methods.push(tool_to_method(tool, &scope_vec));
        self
    }

    /// Build a merged OpenRPC document from a list of `ServiceDescriptor`s.
    ///
    /// Why: The host process (e.g. trusty-agents / a unified trusty daemon) links
    /// several MCP services into one binary and needs a single `rpc.discover`
    /// document covering all of them. This constructor accepts a slice of
    /// trait objects, iterates each service's tools, and produces one merged
    /// manifest with each method annotated by `x-service` so consumers can
    /// see which service owns each tool.
    /// What: For each service, converts every tool via `add_tool` (using the
    /// service's `scopes_for` for the `x-scopes` extension), then injects an
    /// `x-service` field onto the resulting method object. The top-level
    /// `info.title` and `info.version` are taken from the caller, not from
    /// individual services, so the host controls overall identity.
    /// Test: `from_services_merges_tools_and_annotates_x_service`.
    pub fn from_services(title: &str, version: &str, services: &[&dyn ServiceDescriptor]) -> Self {
        let mut builder = Self::new(title, version);
        for svc in services {
            let service_name = svc.name().to_string();
            for tool in svc.tools() {
                let tool_name = tool
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let scopes = svc.scopes_for(&tool_name);
                let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
                builder = builder.add_tool(&tool, &scope_refs);
                // Inject x-service onto the just-appended method.
                if let Some(method) = builder.methods.last_mut()
                    && let Some(obj) = method.as_object_mut()
                {
                    obj.insert("x-service".into(), Value::String(service_name.clone()));
                }
            }
        }
        builder
    }

    /// Finalise the builder and return the OpenRPC 1.3.2 document.
    ///
    /// Why: Produces the value to be returned as the `result` of a
    /// `rpc.discover` JSON-RPC response.
    /// What: Wraps `methods` in `{ openrpc: "1.3.2", info: {…}, methods }`.
    /// `info.description` is only included when the caller set one.
    /// Test: `builder_emits_valid_envelope`.
    pub fn build(self) -> Value {
        let mut info = Map::new();
        info.insert("title".into(), Value::String(self.title));
        info.insert("version".into(), Value::String(self.version));
        if let Some(d) = self.description {
            info.insert("description".into(), Value::String(d));
        }
        json!({
            "openrpc": "1.3.2",
            "info": Value::Object(info),
            "methods": self.methods,
        })
    }
}

/// One-shot helper: build a full discover response from a tool slice.
///
/// Why: Most servers already have a `tools/list` array on hand and only
/// need a per-tool scope lookup. This function captures that pattern in a
/// single call so server wiring is a one-liner.
/// What: Iterates `tools`, invokes `scope_fn(tool_name)` for each, and
/// returns the same document `OpenRpcBuilder::build()` would emit.
/// Test: `discover_response_matches_builder`.
pub fn discover_response(
    title: &str,
    version: &str,
    tools: &[Value],
    scope_fn: impl Fn(&str) -> Vec<String>,
) -> Value {
    let mut builder = OpenRpcBuilder::new(title, version);
    for tool in tools {
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let scopes = scope_fn(name);
        let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();
        builder = builder.add_tool(tool, &scope_refs);
    }
    builder.build()
}

/// Convert a single MCP tool descriptor into an OpenRPC `Method` object.
///
/// Why: OpenRPC requires `params` to be an array of named
/// `ContentDescriptor`s, whereas MCP tool registries store inputs as a
/// JSON Schema `properties` object. This helper bridges the two shapes
/// and attaches the `x-scopes` extension uniformly.
/// What: Flattens `inputSchema.properties` into `params[]` while
/// propagating `inputSchema.required[]` as a per-param boolean, emits a
/// permissive `result` content descriptor, and attaches `x-scopes`.
/// Test: Covered by `builder_attaches_x_scopes_extension` and
/// `tool_to_method_marks_required_params`.
fn tool_to_method(tool: &Value, scopes: &[String]) -> Value {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = tool
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let input_schema = tool.get("inputSchema").cloned().unwrap_or(Value::Null);
    let properties = input_schema
        .get("properties")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let required: Vec<String> = input_schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut params: Vec<Value> = Vec::with_capacity(properties.len());
    for (param_name, schema) in properties.iter() {
        let param_description = schema
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        params.push(json!({
            "name": param_name,
            "description": param_description,
            "required": required.iter().any(|r| r == param_name),
            "schema": schema,
        }));
    }

    json!({
        "name": name,
        "description": description,
        "params": params,
        "result": {
            "name": format!("{name}_result"),
            "description": "Tool result envelope; structure varies by tool.",
            "schema": { "type": "object" }
        },
        "x-scopes": scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tool() -> Value {
        json!({
            "name": "memory_remember",
            "description": "Store a fact in the active memory palace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Fact text" },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["content"]
            }
        })
    }

    #[test]
    fn builder_emits_valid_envelope() {
        let doc = OpenRpcBuilder::new("trusty-memory-mcp", "1.2.3").build();
        assert_eq!(doc["openrpc"].as_str(), Some("1.3.2"));
        assert_eq!(doc["info"]["title"], "trusty-memory-mcp");
        assert_eq!(doc["info"]["version"], "1.2.3");
        let methods = doc["methods"].as_array().expect("methods array");
        assert!(methods.is_empty());
    }

    #[test]
    fn builder_includes_description_when_set() {
        let doc = OpenRpcBuilder::new("trusty-x", "0.1.0")
            .description("Test server")
            .build();
        assert_eq!(doc["info"]["description"], "Test server");
    }

    #[test]
    fn builder_attaches_x_scopes_extension() {
        let tool = sample_tool();
        let doc = OpenRpcBuilder::new("trusty-memory-mcp", "0.1.0")
            .add_tool(&tool, &["memory.write"])
            .build();
        let methods = doc["methods"].as_array().expect("methods array");
        assert_eq!(methods.len(), 1);
        let m = &methods[0];
        assert_eq!(m["name"], "memory_remember");
        let scopes = m["x-scopes"].as_array().expect("x-scopes array");
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0], "memory.write");
        assert!(m["params"].is_array());
        assert!(m["result"].is_object());
    }

    #[test]
    fn tool_to_method_marks_required_params() {
        let tool = sample_tool();
        let doc = OpenRpcBuilder::new("t", "0")
            .add_tool(&tool, &["memory.write"])
            .build();
        let params = doc["methods"][0]["params"].as_array().unwrap();
        let content = params
            .iter()
            .find(|p| p["name"] == "content")
            .expect("content param");
        assert_eq!(content["required"], true);
        let tags = params
            .iter()
            .find(|p| p["name"] == "tags")
            .expect("tags param");
        assert_eq!(tags["required"], false);
    }

    #[test]
    fn discover_response_matches_builder() {
        let tools = vec![sample_tool()];
        let doc = discover_response("trusty-memory-mcp", "0.1.0", &tools, |name| match name {
            "memory_remember" => vec!["memory.write".to_string()],
            _ => vec![],
        });
        assert_eq!(doc["openrpc"], "1.3.2");
        assert_eq!(doc["info"]["title"], "trusty-memory-mcp");
        let methods = doc["methods"].as_array().unwrap();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["x-scopes"][0], "memory.write");
    }

    struct MockService {
        name: String,
        version: String,
        tools: Vec<Value>,
        scopes: std::collections::HashMap<String, Vec<String>>,
    }

    impl ServiceDescriptor for MockService {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            &self.version
        }
        fn tools(&self) -> Vec<Value> {
            self.tools.clone()
        }
        fn scopes_for(&self, tool: &str) -> Vec<String> {
            self.scopes.get(tool).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn service_descriptor_trait_object_dispatches() {
        let mut scopes = std::collections::HashMap::new();
        scopes.insert(
            "memory_remember".to_string(),
            vec!["memory.write".to_string()],
        );
        let svc = MockService {
            name: "trusty-memory".to_string(),
            version: "1.0.0".to_string(),
            tools: vec![sample_tool()],
            scopes,
        };
        let dyn_svc: &dyn ServiceDescriptor = &svc;
        assert_eq!(dyn_svc.name(), "trusty-memory");
        assert_eq!(dyn_svc.version(), "1.0.0");
        let tools = dyn_svc.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "memory_remember");
        assert_eq!(dyn_svc.scopes_for("memory_remember"), vec!["memory.write"]);
        assert!(dyn_svc.scopes_for("unknown").is_empty());
    }

    #[test]
    fn from_services_merges_tools_and_annotates_x_service() {
        let mut memory_scopes = std::collections::HashMap::new();
        memory_scopes.insert(
            "memory_remember".to_string(),
            vec!["memory.write".to_string()],
        );
        let memory = MockService {
            name: "trusty-memory".to_string(),
            version: "1.0.0".to_string(),
            tools: vec![sample_tool()],
            scopes: memory_scopes,
        };

        let search_tool = json!({
            "name": "search_code",
            "description": "Search the code index.",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }
        });
        let mut search_scopes = std::collections::HashMap::new();
        search_scopes.insert("search_code".to_string(), vec!["search.read".to_string()]);
        let search = MockService {
            name: "trusty-search".to_string(),
            version: "2.0.0".to_string(),
            tools: vec![search_tool],
            scopes: search_scopes,
        };

        let services: Vec<&dyn ServiceDescriptor> = vec![&memory, &search];
        let doc = OpenRpcBuilder::from_services("trusty-host", "0.1.0", &services).build();

        assert_eq!(doc["info"]["title"], "trusty-host");
        assert_eq!(doc["info"]["version"], "0.1.0");
        let methods = doc["methods"].as_array().expect("methods array");
        assert_eq!(methods.len(), 2);

        let memory_method = methods
            .iter()
            .find(|m| m["name"] == "memory_remember")
            .expect("memory_remember method");
        assert_eq!(memory_method["x-service"], "trusty-memory");
        assert_eq!(memory_method["x-scopes"][0], "memory.write");

        let search_method = methods
            .iter()
            .find(|m| m["name"] == "search_code")
            .expect("search_code method");
        assert_eq!(search_method["x-service"], "trusty-search");
        assert_eq!(search_method["x-scopes"][0], "search.read");
    }

    #[test]
    fn from_services_with_no_scopes_yields_empty_scope_list() {
        let svc = MockService {
            name: "svc-a".to_string(),
            version: "0.0.1".to_string(),
            tools: vec![sample_tool()],
            scopes: std::collections::HashMap::new(),
        };
        let services: Vec<&dyn ServiceDescriptor> = vec![&svc];
        let doc = OpenRpcBuilder::from_services("host", "0", &services).build();
        let method = &doc["methods"][0];
        assert_eq!(method["x-service"], "svc-a");
        assert!(method["x-scopes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn discover_response_handles_unknown_tool_with_empty_scopes() {
        let tools = vec![sample_tool()];
        let doc = discover_response("t", "0", &tools, |_| Vec::new());
        let scopes = doc["methods"][0]["x-scopes"].as_array().unwrap();
        assert!(scopes.is_empty(), "unknown tool yields empty scope list");
    }
}
