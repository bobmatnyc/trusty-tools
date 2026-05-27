//! Granola meeting-notes service — native HTTP client over the Granola
//! public API.
//!
//! Why: The Trusty personas (cto-assistant, izzie) need read access to
//! Granola meeting documents and transcripts. Previously this was reached
//! through an MCP subprocess; a native `reqwest` client keeps the call
//! in-process, removes the subprocess/OAuth churn, and lets every Rust
//! consumer share one implementation.
//! What: One `GranolaService` per published tool (5 total). Each carries its
//! name + pre-built OpenAI-compatible function schema. `execute()` reads
//! `GRANOLA_API_KEY` from the environment at call time (consistent with the
//! `CTO_DB_PATH` pattern in `cto_db.rs`), issues the HTTP request, and
//! returns the JSON response body as a string.
//! Test: `granola_services_lists_five`, `schema_round_trips_for_each_service`,
//! `service_for_unknown_name`, `execute_errors_when_api_key_missing`.

use serde_json::{Value, json};

/// Base URL for the Granola public API (v1).
const GRANOLA_API_BASE: &str = "https://public-api.granola.ai/v1";

/// Environment variable holding the Granola API key.
///
/// Why: Named once so the service and tests stay in sync.
/// What: `&'static str`; read at `execute()` time, not at construction.
pub const ENV_GRANOLA_API_KEY: &str = "GRANOLA_API_KEY";

/// Names of the five Granola tools exposed by this module.
///
/// Why: Listed once so `granola_services()` and the tests stay in sync.
/// What: Plain `&'static str` slice.
pub const GRANOLA_TOOL_NAMES: &[&str] = &[
    "granola_list",
    "granola_get",
    "granola_get_transcript",
    "granola_search",
    "granola_get_shared_document",
];

/// Outcome of a Granola service call.
///
/// Why: `tc-services` must stay host-agnostic — it cannot know about any
/// host's tool-result type. This plain enum lets each host translate the
/// outcome into its own representation.
/// What: `Ok` carries the JSON response body; `Err` carries a
/// human-readable, recoverable error message (a missing API key, network
/// failure, or non-2xx status must never panic or abort the caller's loop).
/// Test: Returned by `GranolaService::execute`; see module tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GranolaOutcome {
    /// Successful request — the JSON response body.
    Ok(String),
    /// Recoverable failure — a descriptive error message.
    Err(String),
}

impl GranolaOutcome {
    /// Whether this outcome is an error.
    ///
    /// Why: Lets host wrappers branch without matching the enum.
    /// What: `true` for `Err`, `false` for `Ok`.
    /// Test: Exercised by `execute_errors_when_api_key_missing`.
    pub fn is_error(&self) -> bool {
        matches!(self, GranolaOutcome::Err(_))
    }

    /// The payload string (body on `Ok`, message on `Err`).
    ///
    /// Why: Host wrappers need the text regardless of variant.
    /// What: Returns the inner `&str`.
    /// Test: Used by host wrappers and module tests.
    pub fn message(&self) -> &str {
        match self {
            GranolaOutcome::Ok(s) | GranolaOutcome::Err(s) => s,
        }
    }
}

/// HTTP method + path template for one Granola tool.
///
/// Why: Each tool maps to exactly one REST endpoint; pairing the verb with a
/// path template keeps the routing table next to the schema.
/// What: `path` may contain a single `{id}` placeholder substituted from the
/// `id` argument; `needs_id` / `needs_query` drive both the schema and the
/// argument validation in `execute()`.
#[derive(Debug, Clone, Copy)]
struct Endpoint {
    needs_id: bool,
    needs_query: bool,
}

/// Resolve the endpoint shape for a tool name.
///
/// Why: One match keeps the per-tool contract greppable.
/// What: Returns `None` for unknown names.
fn endpoint_for(name: &str) -> Option<Endpoint> {
    let ep = match name {
        "granola_list" => Endpoint {
            needs_id: false,
            needs_query: false,
        },
        "granola_get" => Endpoint {
            needs_id: true,
            needs_query: false,
        },
        "granola_get_transcript" => Endpoint {
            needs_id: true,
            needs_query: false,
        },
        "granola_search" => Endpoint {
            needs_id: false,
            needs_query: true,
        },
        "granola_get_shared_document" => Endpoint {
            needs_id: true,
            needs_query: false,
        },
        _ => return None,
    };
    Some(ep)
}

/// Build the `{method, url}` for a tool call from validated arguments.
///
/// Why: Centralises path templating + query-string assembly so `execute()`
/// stays focused on the HTTP round-trip.
/// What: Returns `anyhow::Err` (recoverable) if a required `id`/`query`
/// argument is missing or not a string. The `id` argument is validated eagerly
/// before the `match` so that tools requiring it receive a `&str` directly —
/// eliminating any `Option::expect` calls (issue #253).
/// Test: `build_request_requires_id` covers the missing-id error path.
fn build_request(name: &str, args: &Value) -> anyhow::Result<(reqwest::Method, String)> {
    let ep = endpoint_for(name).ok_or_else(|| anyhow::anyhow!("granola: unknown tool '{name}'"))?;

    // Validate `id` up-front so arms that use it receive a plain `&str`.
    // This removes the need for any `Option::expect` at each call site and
    // makes the validation visible in one place rather than three.
    let id: Option<&str> =
        if ep.needs_id {
            Some(args.get("id").and_then(Value::as_str).ok_or_else(|| {
                anyhow::anyhow!("granola {name}: missing required string arg 'id'")
            })?)
        } else {
            None
        };

    let url = match name {
        "granola_list" => format!("{GRANOLA_API_BASE}/documents"),
        "granola_get" => {
            // id is validated above; ep.needs_id == true for this arm.
            let id = id.ok_or_else(|| anyhow::anyhow!("granola_get: id missing"))?;
            format!("{GRANOLA_API_BASE}/documents/{id}")
        }
        "granola_get_transcript" => {
            let id = id.ok_or_else(|| anyhow::anyhow!("granola_get_transcript: id missing"))?;
            format!("{GRANOLA_API_BASE}/documents/{id}/transcript")
        }
        "granola_get_shared_document" => {
            let id =
                id.ok_or_else(|| anyhow::anyhow!("granola_get_shared_document: id missing"))?;
            format!("{GRANOLA_API_BASE}/documents/{id}/shared")
        }
        "granola_search" => {
            let query = args
                .get("q")
                .or_else(|| args.get("query"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    anyhow::anyhow!("granola {name}: missing required string arg 'q'")
                })?;
            let encoded = encode_query_component(query);
            format!("{GRANOLA_API_BASE}/documents/search?q={encoded}")
        }
        // Unreachable: endpoint_for already validated the name.
        other => return Err(anyhow::anyhow!("granola: unknown tool '{other}'")),
    };

    let _ = ep.needs_query; // documented above; query handled inline for search
    Ok((reqwest::Method::GET, url))
}

/// Percent-encode a query-string component (RFC 3986 unreserved set kept).
///
/// Why: `reqwest` does not encode interpolated query strings; meeting search
/// terms routinely contain spaces and punctuation. A tiny dependency-free
/// encoder avoids pulling in `urlencoding` for one call site.
/// What: Keeps `A-Z a-z 0-9 - _ . ~`; percent-encodes everything else.
/// Test: Exercised indirectly by `build_request` in `search_url_is_encoded`.
fn encode_query_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One Granola service instance per published tool.
///
/// Why: Hosts dispatch by tool name, so each tool needs its own instance
/// carrying its name + cached schema.
/// What: Holds the tool name and the pre-built OpenAI-compatible function
/// schema (so `schema()` is a cheap clone). `execute()` issues the HTTP
/// request and converts every failure into a recoverable `GranolaOutcome::Err`.
/// Test: See module tests below.
#[derive(Debug, Clone)]
pub struct GranolaService {
    name: &'static str,
    schema: Value,
}

impl GranolaService {
    /// Build a service for the given tool name.
    ///
    /// Why: Constructor that builds the OpenAI function schema for one of the
    /// five published Granola tools.
    /// What: Returns `None` if `name` is not a known tool — callers should
    /// treat that as a programming error.
    /// Test: `granola_services_lists_five` covers the valid names;
    /// `service_for_unknown_name` covers the negative path.
    pub fn new(name: &'static str) -> Option<Self> {
        let ep = endpoint_for(name)?;

        let (description, mut properties, mut required): (&str, Value, Vec<&str>) = match name {
            "granola_list" => ("List recent Granola meeting documents.", json!({}), vec![]),
            "granola_get" => (
                "Fetch a single Granola meeting document by ID.",
                json!({}),
                vec![],
            ),
            "granola_get_transcript" => (
                "Fetch the transcript for a Granola meeting document by ID.",
                json!({}),
                vec![],
            ),
            "granola_search" => (
                "Search Granola meeting documents by free-text query.",
                json!({}),
                vec![],
            ),
            "granola_get_shared_document" => (
                "Fetch the shared (public) view of a Granola meeting document by ID.",
                json!({}),
                vec![],
            ),
            // Unreachable: endpoint_for already validated the name.
            _ => return None,
        };

        // All `*_get*` tools need an `id`; `granola_search` needs a `q`.
        if ep.needs_id {
            properties["id"] = json!({
                "type": "string",
                "description": "The Granola document ID.",
            });
            required.push("id");
        }
        if ep.needs_query {
            properties["q"] = json!({
                "type": "string",
                "description": "Free-text search query.",
            });
            required.push("q");
        }

        let schema = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            }
        });

        Some(Self { name, schema })
    }

    /// The tool name this service answers to.
    pub fn name(&self) -> &str {
        self.name
    }

    /// The OpenAI-compatible function schema for this service.
    ///
    /// Why: Hosts attach this schema to LLM requests.
    /// What: Cheap clone of the cached `Value`.
    /// Test: `schema_round_trips_for_each_service`.
    pub fn schema(&self) -> Value {
        self.schema.clone()
    }

    /// Run the Granola API call.
    ///
    /// Why: Reads `GRANOLA_API_KEY` at call time (so the env can change
    /// between calls and CI without a key still loads the module), builds the
    /// request, and issues it with a fresh `reqwest::Client`.
    /// What: Returns `GranolaOutcome::Ok` with the JSON response body, or
    /// `GranolaOutcome::Err` with a descriptive message for any failure
    /// (missing key, bad arguments, network error, non-2xx status). Never
    /// panics; every error path is recoverable.
    /// Test: `execute_errors_when_api_key_missing`.
    pub async fn execute(&self, args: Value) -> GranolaOutcome {
        match self.execute_inner(args).await {
            Ok(body) => GranolaOutcome::Ok(body),
            Err(e) => GranolaOutcome::Err(format!("granola {} failed: {e:#}", self.name)),
        }
    }

    /// Fallible inner body of `execute` — keeps error mapping in one place.
    async fn execute_inner(&self, args: Value) -> anyhow::Result<String> {
        let api_key = std::env::var(ENV_GRANOLA_API_KEY).map_err(|_| {
            anyhow::anyhow!("{ENV_GRANOLA_API_KEY} is not set; cannot call the Granola API")
        })?;

        let (method, url) = build_request(self.name, &args)?;

        let client = reqwest::Client::builder()
            .user_agent("tc-services/granola")
            .build()?;

        let response = client
            .request(method, &url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Accept", "application/json")
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(anyhow::anyhow!("HTTP {status}: {body}"));
        }
        Ok(body)
    }
}

/// Build the full list of Granola services (one per published tool).
///
/// Why: Centralised constructor that host registries call when building the
/// Granola tool surface.
/// What: Returns one `GranolaService` per name in `GRANOLA_TOOL_NAMES`. A
/// name that fails to build a schema (should be impossible) is skipped with a
/// warn-log rather than panicking.
/// Test: `granola_services_lists_five`.
pub fn granola_services() -> Vec<GranolaService> {
    GRANOLA_TOOL_NAMES
        .iter()
        .filter_map(|name| match GranolaService::new(name) {
            Some(s) => Some(s),
            None => {
                tracing::warn!(tool = %name, "granola: failed to build schema; skipping");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All five published tool names must yield a service.
    ///
    /// Why: Drift between `GRANOLA_TOOL_NAMES` and the constructor would
    /// silently drop tools.
    /// What: Asserts the returned vec has five entries with matching names.
    /// Test: `cargo test -p tc-services granola_services_lists_five`.
    #[test]
    fn granola_services_lists_five() {
        let services = granola_services();
        assert_eq!(services.len(), 5, "expected one service per Granola tool");
        let names: Vec<&str> = services.iter().map(GranolaService::name).collect();
        for expected in GRANOLA_TOOL_NAMES {
            assert!(
                names.contains(expected),
                "missing tool {expected} in {names:?}"
            );
        }
    }

    /// Each service's schema must round-trip through the OpenAI envelope with
    /// the right name and an `object`-typed parameters block.
    ///
    /// Why: Hosts hand these schemas straight to the LLM; a missing
    /// `function.name` or non-object `parameters` would be rejected.
    /// What: Builds each service, re-serialises and re-parses the schema,
    /// asserts `function.name` and `function.parameters` are correct.
    /// Test: `cargo test -p tc-services schema_round_trips_for_each_service`.
    #[test]
    fn schema_round_trips_for_each_service() {
        for name in GRANOLA_TOOL_NAMES {
            let service = GranolaService::new(name).expect("schema must build");
            // Round-trip through a string to prove it is valid JSON.
            let serialized = serde_json::to_string(&service.schema()).expect("serialise");
            let schema: Value = serde_json::from_str(&serialized).expect("deserialise");

            assert_eq!(schema.get("type").and_then(Value::as_str), Some("function"));
            let function = schema.get("function").expect("function block");
            assert_eq!(function.get("name").and_then(Value::as_str), Some(*name));
            let params = function.get("parameters").expect("parameters block");
            assert_eq!(params.get("type").and_then(Value::as_str), Some("object"));
            assert!(
                params.get("properties").is_some(),
                "{name}: parameters must have a properties key"
            );
        }
    }

    /// Unknown tool names must return `None` rather than building a broken
    /// service.
    ///
    /// Why: Defence in depth — a typo in a name should not ship a dead
    /// schema.
    /// What: Asserts `new("not_a_real_tool")` returns `None`.
    /// Test: `cargo test -p tc-services service_for_unknown_name`.
    #[test]
    fn service_for_unknown_name() {
        assert!(GranolaService::new("not_a_real_tool").is_none());
        assert!(endpoint_for("not_a_real_tool").is_none());
    }

    /// `granola_search` must produce a percent-encoded query string.
    ///
    /// Why: Search terms with spaces/punctuation would otherwise build an
    /// invalid URL.
    /// What: Builds a request for `granola_search` with a spaced query and
    /// asserts the URL contains `%20` and no raw space.
    /// Test: `cargo test -p tc-services search_url_is_encoded`.
    #[test]
    fn search_url_is_encoded() {
        let (_method, url) =
            build_request("granola_search", &json!({"q": "weekly sync"})).expect("builds");
        assert!(url.contains("q=weekly%20sync"), "url not encoded: {url}");
        assert!(!url.contains("q=weekly sync"), "raw space in url: {url}");
    }

    /// Calling `execute` with no `GRANOLA_API_KEY` set must return a
    /// recoverable `GranolaOutcome::Err` — never panic.
    ///
    /// Why: CI and consumers without a key must keep running.
    /// What: Removes `GRANOLA_API_KEY`, calls `execute`, asserts the outcome
    /// is an error naming the tool. Restores the env var afterwards.
    /// Test: `cargo test -p tc-services execute_errors_when_api_key_missing`.
    #[tokio::test]
    async fn execute_errors_when_api_key_missing() {
        // Save and clear GRANOLA_API_KEY. SAFETY: env mutation in tests is
        // serialised by the harness running this single test; we restore the
        // previous value before returning.
        let prev = std::env::var(ENV_GRANOLA_API_KEY).ok();
        // SAFETY: single-threaded test scope, env access is acceptable.
        unsafe {
            std::env::remove_var(ENV_GRANOLA_API_KEY);
        }

        let service = GranolaService::new("granola_list").expect("schema builds");
        let outcome = service.execute(json!({})).await;
        assert!(
            outcome.is_error(),
            "missing API key must yield an error outcome"
        );
        assert!(
            outcome.message().contains("granola_list"),
            "error message must name the tool: {}",
            outcome.message()
        );

        // Restore.
        // SAFETY: same single-threaded test scope.
        unsafe {
            if let Some(v) = prev {
                std::env::set_var(ENV_GRANOLA_API_KEY, v);
            }
        }
    }

    /// `build_request` must reject a `*_get*` call with no `id` argument.
    ///
    /// Why: A missing path parameter would build a malformed URL.
    /// What: Asserts `build_request("granola_get", {})` is an `Err`.
    /// Test: `cargo test -p tc-services build_request_requires_id`.
    #[test]
    fn build_request_requires_id() {
        assert!(build_request("granola_get", &json!({})).is_err());
        assert!(build_request("granola_get_transcript", &json!({})).is_err());
        let ok = build_request("granola_get", &json!({"id": "doc_123"}));
        assert!(ok.is_ok(), "valid id should build a request");
    }
}
