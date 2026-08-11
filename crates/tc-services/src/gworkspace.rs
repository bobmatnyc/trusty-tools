//! Google Workspace service bridge — Calendar + Tasks subset.
//!
//! Why: The Trusty personas need Google Calendar and Tasks access. The
//! authenticated Google API client + per-service handlers already live in
//! `trusty-gworkspace`; this module is a thin host-agnostic *bridge* that
//! emits OpenAI-compatible schemas and dispatches calls — the same shape as
//! `cto_db.rs` and `granola.rs` — so trusty-agents, trusty-izzie, and future
//! consumers reuse one surface instead of re-deriving it.
//! What: One `GworkspaceService` per published tool. Phase 2 scope is
//! Calendar (`manage_calendars`, `manage_events`, `query_free_busy`) and
//! Tasks (`manage_task_lists`, `manage_tasks`, `list_tasks`, `complete_task`)
//! only — Gmail, Drive, Docs, Sheets, Slides are deliberately excluded.
//! Each service pulls its schema from `trusty_gworkspace::tools` and
//! dispatches via the matching `trusty_gworkspace::api::services` handler.
//! `execute()` constructs a `BaseClient` (read-only token mode when OAuth
//! env vars are absent) and runs the call.
//! Test: `gworkspace_services_lists_seven`, `schema_round_trips`,
//! `service_for_unknown_name`, `execute_errors_without_credentials`.

use serde_json::{Value, json};

use trusty_gworkspace::api::client::BaseClient;
use trusty_gworkspace::api::services;

/// Names of the Google Workspace tools bridged in Phase 2.
///
/// Why: Listed once so `gworkspace_services()` and the tests stay in sync.
/// What: Calendar + Tasks tools only. TODO(Phase 3): extend to Gmail/Drive/
/// Docs once those personas need them — the dispatch table below already
/// shows the pattern.
pub const GWORKSPACE_TOOL_NAMES: &[&str] = &[
    // Calendar
    "manage_calendars",
    "manage_events",
    "query_free_busy",
    // Tasks
    "manage_task_lists",
    "manage_tasks",
    "list_tasks",
    "complete_task",
];

/// Outcome of a Google Workspace service call.
///
/// Why: `tc-services` must stay host-agnostic — it cannot know about any
/// host's tool-result type. This plain enum lets each host translate the
/// outcome into its own representation.
/// What: `Ok` carries the serialised JSON result; `Err` carries a
/// human-readable, recoverable error message (missing credentials, an API
/// failure, or schema drift must never panic or abort the caller's loop).
/// Test: Returned by `GworkspaceService::execute`; see module tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GworkspaceOutcome {
    /// Successful call — the serialised JSON result.
    Ok(String),
    /// Recoverable failure — a descriptive error message.
    Err(String),
}

impl GworkspaceOutcome {
    /// Whether this outcome is an error.
    ///
    /// Why: Lets host wrappers branch without matching the enum.
    /// What: `true` for `Err`, `false` for `Ok`.
    /// Test: Exercised by `execute_errors_without_credentials`.
    pub fn is_error(&self) -> bool {
        matches!(self, GworkspaceOutcome::Err(_))
    }

    /// The payload string (result on `Ok`, message on `Err`).
    ///
    /// Why: Host wrappers need the text regardless of variant.
    /// What: Returns the inner `&str`.
    /// Test: Used by host wrappers and module tests.
    pub fn message(&self) -> &str {
        match self {
            GworkspaceOutcome::Ok(s) | GworkspaceOutcome::Err(s) => s,
        }
    }
}

/// One Google Workspace service instance per bridged tool.
///
/// Why: Hosts dispatch by tool name, so each tool needs its own instance
/// carrying its name + cached schema.
/// What: Holds the tool name and the pre-built OpenAI-compatible function
/// schema (so `schema()` is a cheap clone). `execute()` builds a
/// `BaseClient` and dispatches to the matching `trusty-gworkspace` handler.
/// Test: See module tests below.
#[derive(Debug, Clone)]
pub struct GworkspaceService {
    name: &'static str,
    schema: Value,
}

impl GworkspaceService {
    /// Build a service for the given tool name.
    ///
    /// Why: Constructor that pulls the matching entry from
    /// `trusty_gworkspace::tools::tool_list_response()` and wraps it in the
    /// OpenAI function-calling envelope hosts expect.
    /// What: Returns `None` if `name` is not one of the Phase 2 bridged
    /// tools (`GWORKSPACE_TOOL_NAMES`), even if `trusty-gworkspace` itself
    /// publishes a tool by that name — Gmail/Drive/etc. are out of scope.
    /// Test: `gworkspace_services_lists_seven` covers the valid names;
    /// `service_for_unknown_name` covers the negative path.
    pub fn new(name: &'static str) -> Option<Self> {
        // Only bridge tools that are in Phase 2 scope.
        if !GWORKSPACE_TOOL_NAMES.contains(&name) {
            return None;
        }

        let tools = trusty_gworkspace::tools::tool_list_response();
        let entry = tools.get("tools")?.as_array()?.iter().find(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .map(|n| n == name)
                .unwrap_or(false)
        })?;

        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let parameters = entry
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

        let schema = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": parameters,
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
    /// Test: `schema_round_trips`.
    pub fn schema(&self) -> Value {
        self.schema.clone()
    }

    /// Run the Google Workspace call.
    ///
    /// Why: Builds a `BaseClient` per call (cheap — it only reads token
    /// storage; OAuth refresh is enabled only when the env vars are present)
    /// and dispatches to the matching `trusty-gworkspace` service handler.
    /// What: Returns `GworkspaceOutcome::Ok` with the serialised JSON result,
    /// or `GworkspaceOutcome::Err` with a descriptive message for any failure
    /// (no stored Google profile, API error, JSON serialisation error).
    /// Never panics; every error path is recoverable.
    /// Test: `execute_errors_without_credentials`.
    pub async fn execute(&self, args: Value) -> GworkspaceOutcome {
        match self.execute_inner(args).await {
            Ok(value) => match serde_json::to_string(&value) {
                Ok(s) => GworkspaceOutcome::Ok(s),
                Err(e) => GworkspaceOutcome::Err(format!(
                    "gworkspace {}: failed to serialise JSON result: {e}",
                    self.name
                )),
            },
            Err(e) => GworkspaceOutcome::Err(format!("gworkspace {} failed: {e:#}", self.name)),
        }
    }

    /// Fallible inner body of `execute` — builds the client and dispatches.
    async fn execute_inner(&self, args: Value) -> anyhow::Result<Value> {
        let client = BaseClient::new()?;
        dispatch(self.name, &client, args).await
    }
}

/// Route a tool call to its `trusty-gworkspace` handler.
///
/// Why: One match arm per bridged tool keeps the routing table greppable;
/// new tools added to `GWORKSPACE_TOOL_NAMES` need a matching arm here.
/// What: Returns the handler's `anyhow::Result<Value>` unchanged.
async fn dispatch(name: &str, client: &BaseClient, args: Value) -> anyhow::Result<Value> {
    match name {
        // Calendar
        "manage_calendars" => services::calendar::manage_calendars(client, args).await,
        "manage_events" => services::calendar::manage_events(client, args).await,
        "query_free_busy" => services::calendar::query_free_busy(client, args).await,
        // Tasks
        "manage_task_lists" => services::tasks::manage_task_lists(client, args).await,
        "manage_tasks" => services::tasks::manage_tasks(client, args).await,
        "list_tasks" => services::tasks::list_tasks(client, args).await,
        "complete_task" => services::tasks::complete_task(client, args).await,
        other => Err(anyhow::anyhow!(
            "gworkspace: tool '{other}' is not bridged in Phase 2"
        )),
    }
}

/// Build the full list of Google Workspace services (one per bridged tool).
///
/// Why: Centralised constructor that host registries call when building the
/// Google Workspace tool surface.
/// What: Returns one `GworkspaceService` per name in `GWORKSPACE_TOOL_NAMES`.
/// A name that fails to resolve a schema (would indicate `trusty-gworkspace`
/// drift) is skipped with a warn-log rather than panicking.
/// Test: `gworkspace_services_lists_seven`.
pub fn gworkspace_services() -> Vec<GworkspaceService> {
    GWORKSPACE_TOOL_NAMES
        .iter()
        .filter_map(|name| match GworkspaceService::new(name) {
            Some(s) => Some(s),
            None => {
                tracing::warn!(
                    tool = %name,
                    "gworkspace: schema missing from trusty_gworkspace::tools; skipping"
                );
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All seven bridged tool names must yield a service.
    ///
    /// Why: Drift between `GWORKSPACE_TOOL_NAMES` and the schemas published
    /// by `trusty-gworkspace` would silently drop tools.
    /// What: Asserts the returned vec has seven entries with matching names.
    /// Test: `cargo test -p tc-services gworkspace_services_lists_seven`.
    #[test]
    fn gworkspace_services_lists_seven() {
        let services = gworkspace_services();
        assert_eq!(
            services.len(),
            7,
            "expected one service per bridged Calendar/Tasks tool"
        );
        let names: Vec<&str> = services.iter().map(GworkspaceService::name).collect();
        for expected in GWORKSPACE_TOOL_NAMES {
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
    /// Test: `cargo test -p tc-services schema_round_trips`.
    #[test]
    fn schema_round_trips() {
        for name in GWORKSPACE_TOOL_NAMES {
            let service = GworkspaceService::new(name).expect("schema must resolve");
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

    /// Unknown tool names — and out-of-scope tools — must return `None`.
    ///
    /// Why: Phase 2 deliberately excludes Gmail/Drive/etc.; bridging them by
    /// accident would ship handlers that `dispatch` cannot route.
    /// What: Asserts `new` returns `None` for a nonsense name and for a real
    /// `trusty-gworkspace` tool that is out of Phase 2 scope.
    /// Test: `cargo test -p tc-services service_for_unknown_name`.
    #[test]
    fn service_for_unknown_name() {
        assert!(GworkspaceService::new("not_a_real_tool").is_none());
        // `search_gmail_messages` is a real trusty-gworkspace tool but out of
        // Phase 2 scope — the bridge must not expose it.
        assert!(GworkspaceService::new("search_gmail_messages").is_none());
    }

    /// Calling `execute` with no stored Google profile must return a
    /// recoverable `GworkspaceOutcome::Err` — never panic.
    ///
    /// Why: CI and consumers without Google credentials must keep running.
    /// What: Points the token-storage dir at an empty temp dir so no profile
    /// resolves, calls `execute`, asserts the outcome is an error naming the
    /// tool. Restores the env var afterwards.
    /// Test: `cargo test -p tc-services execute_errors_without_credentials`.
    #[tokio::test]
    async fn execute_errors_without_credentials() {
        // `trusty-gworkspace` resolves token storage relative to the home
        // directory; pointing HOME at an empty temp dir guarantees no stored
        // profile is found. SAFETY: env mutation in tests is serialised by
        // the harness running this single test; we restore HOME afterwards.
        let tmp = std::env::temp_dir().join("tc-services-gw-test-488");
        let _ = std::fs::create_dir_all(&tmp);
        let prev_home = std::env::var("HOME").ok();
        // SAFETY: single-threaded test scope, env access is acceptable.
        unsafe {
            std::env::set_var("HOME", &tmp);
        }

        let service = GworkspaceService::new("list_tasks").expect("schema resolves");
        let outcome = service.execute(json!({})).await;
        assert!(
            outcome.is_error(),
            "missing Google profile must yield an error outcome, got: {}",
            outcome.message()
        );
        assert!(
            outcome.message().contains("list_tasks"),
            "error message must name the tool: {}",
            outcome.message()
        );

        // Restore.
        // SAFETY: same single-threaded test scope.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
