//! CTO ops SQLite service — framework-agnostic adapter over `trusty-cto-db`.
//!
//! Why: The cto-assistant persona needs structured, read-only access to the
//! CTO ops SQLite DB (`~/Duetto/cto/data/cto.db`) for
//! headcount/budget/risks/work-classification queries. The query *engine*
//! lives in `trusty-cto-db`; this module owns the reusable *service adapter*
//! (schema emission + dispatch) so trusty-agents, trusty-izzie, and future
//! consumers stop re-deriving it. Calling `trusty_cto_db::handle_tool_call`
//! in-process keeps the path short and the data on-host (no MCP subprocess,
//! no OAuth churn).
//! What: One `CtoDbService` per query function (4 total). Each pulls its
//! schema from `trusty_cto_db::tool_list_response()` and dispatches via
//! `trusty_cto_db::handle_tool_call(name, args)`. `execute()` returns a
//! `CtoDbOutcome` (a plain `Ok`/`Err` result type) so this crate has NO
//! dependency on any host's tool-executor trait.
//! SECURITY: This service is sensitive (HR/budget data). Hosts must gate it
//! to the cto-assistant persona only; this module does NOT self-restrict.
//! Test: `cto_db_services_lists_four`, `schema_round_trips_for_each_service`,
//! `new_returns_none_for_unknown_name`,
//! `execute_returns_error_when_db_missing`.

use serde_json::{Value, json};

/// Names of the four CTO DB tools exposed by `trusty-cto-db`.
///
/// Why: Listed once so `cto_db_services()` and the tests stay in sync.
/// What: Plain `&'static str` slice.
pub const CTO_DB_TOOL_NAMES: &[&str] = &[
    "query_headcount",
    "query_budget",
    "query_risks",
    "query_work_classification",
];

/// Outcome of a CTO DB service call.
///
/// Why: `tc-services` must stay host-agnostic — it cannot know about
/// trusty-agents's `ToolResult` or any other host's result type. This plain enum
/// lets each host translate the outcome into its own representation.
/// What: `Ok` carries the serialised JSON result; `Err` carries a
/// human-readable, recoverable error message (a missing DB or schema drift
/// must never panic or abort the caller's loop).
/// Test: Returned by `CtoDbService::execute`; see module tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtoDbOutcome {
    /// Successful query — the serialised JSON result.
    Ok(String),
    /// Recoverable failure — a descriptive error message.
    Err(String),
}

impl CtoDbOutcome {
    /// Whether this outcome is an error.
    ///
    /// Why: Lets host wrappers branch without matching the enum.
    /// What: `true` for `Err`, `false` for `Ok`.
    /// Test: Exercised by `execute_returns_error_when_db_missing`.
    pub fn is_error(&self) -> bool {
        matches!(self, CtoDbOutcome::Err(_))
    }

    /// The payload string (result on `Ok`, message on `Err`).
    ///
    /// Why: Host wrappers need the text regardless of variant.
    /// What: Returns the inner `&str`.
    /// Test: Used by host wrappers and module tests.
    pub fn message(&self) -> &str {
        match self {
            CtoDbOutcome::Ok(s) | CtoDbOutcome::Err(s) => s,
        }
    }
}

/// One CTO DB service instance per query function.
///
/// Why: Hosts dispatch by tool name, so each of the four tools needs its own
/// instance carrying its name + cached schema. State is trivial, so a single
/// struct with a per-tool name suffices.
/// What: Holds the tool name and the pre-extracted OpenAI-compatible function
/// schema (so `schema()` is a cheap clone). `execute()` runs
/// `trusty_cto_db::handle_tool_call` and converts every failure into a
/// recoverable `CtoDbOutcome::Err`.
/// Test: See module tests below.
#[derive(Debug, Clone)]
pub struct CtoDbService {
    name: &'static str,
    /// Pre-built OpenAI-compatible function schema (`{"type": "function",
    /// "function": {...}}`). Cached at construction because
    /// `tool_list_response()` allocates fresh JSON each call.
    schema: Value,
}

impl CtoDbService {
    /// Build a service for the given tool name.
    ///
    /// Why: Constructor that pulls the matching entry from
    /// `trusty_cto_db::tool_list_response()` and wraps it in the OpenAI
    /// function-calling envelope hosts expect.
    /// What: Returns `None` if `name` is not one of the four published tools
    /// — callers should treat that as a programming error.
    /// Test: `cto_db_services_lists_four` exercises the four valid names;
    /// `new_returns_none_for_unknown_name` covers the negative path.
    pub fn new(name: &'static str) -> Option<Self> {
        let tools = trusty_cto_db::tool_list_response();
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
    /// Test: `schema_round_trips_for_each_service`.
    pub fn schema(&self) -> Value {
        self.schema.clone()
    }

    /// Run the CTO DB query.
    ///
    /// Why: `handle_tool_call` opens a read-only connection per call and
    /// dispatches by name. We run it inside `spawn_blocking` because rusqlite
    /// is synchronous and we must not stall the tokio runtime on disk I/O.
    /// What: Returns `CtoDbOutcome::Ok` with the serialised JSON result, or
    /// `CtoDbOutcome::Err` with a descriptive message for any failure
    /// (query error, JSON serialisation error, or blocking-task join error).
    /// Never panics; every error path is recoverable.
    /// Test: `execute_returns_error_when_db_missing`.
    pub async fn execute(&self, args: Value) -> CtoDbOutcome {
        let name = self.name;
        let result =
            tokio::task::spawn_blocking(move || trusty_cto_db::handle_tool_call(name, &args)).await;

        match result {
            Ok(Ok(value)) => match serde_json::to_string(&value) {
                Ok(s) => CtoDbOutcome::Ok(s),
                Err(e) => CtoDbOutcome::Err(format!(
                    "cto_db {name}: failed to serialise JSON result: {e}"
                )),
            },
            Ok(Err(e)) => CtoDbOutcome::Err(format!("cto_db {name} failed: {e:#}")),
            Err(join_err) => CtoDbOutcome::Err(format!(
                "cto_db {name}: blocking task join error: {join_err}"
            )),
        }
    }
}

/// Build the full list of CTO DB services (one per query function).
///
/// Why: Centralised constructor that host registries call when building the
/// cto-assistant tool surface.
/// What: Returns one `CtoDbService` per name in `CTO_DB_TOOL_NAMES`. Names
/// that fail to resolve a schema (should be impossible — `trusty-cto-db` is
/// the source of truth) are skipped with a warn-log rather than panicking.
/// Test: `cto_db_services_lists_four`.
pub fn cto_db_services() -> Vec<CtoDbService> {
    CTO_DB_TOOL_NAMES
        .iter()
        .filter_map(|name| match CtoDbService::new(name) {
            Some(s) => Some(s),
            None => {
                tracing::warn!(
                    tool = %name,
                    "cto_db: schema missing from trusty_cto_db::tool_list_response; skipping"
                );
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All four published tool names must yield a service.
    ///
    /// Why: Drift between `CTO_DB_TOOL_NAMES` and the names emitted by
    /// `trusty_cto_db::tool_list_response()` would silently drop tools.
    /// What: Asserts the returned vec has four entries with matching names.
    /// Test: `cargo test -p tc-services cto_db_services_lists_four`.
    #[test]
    fn cto_db_services_lists_four() {
        let services = cto_db_services();
        assert_eq!(services.len(), 4, "expected one service per CTO DB tool");
        let names: Vec<&str> = services.iter().map(CtoDbService::name).collect();
        for expected in CTO_DB_TOOL_NAMES {
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
    /// What: Builds each service, extracts `function.name` and
    /// `function.parameters.type`, asserts they're correct.
    /// Test: `cargo test -p tc-services schema_round_trips_for_each_service`.
    #[test]
    fn schema_round_trips_for_each_service() {
        for name in CTO_DB_TOOL_NAMES {
            let service = CtoDbService::new(name).expect("schema must resolve");
            let schema = service.schema();
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("function"));
            let function = schema.get("function").expect("function block");
            assert_eq!(function.get("name").and_then(Value::as_str), Some(*name));
            let params = function.get("parameters").expect("parameters block");
            assert_eq!(params.get("type").and_then(Value::as_str), Some("object"));
        }
    }

    /// Unknown tool names must return `None` rather than building a broken
    /// service.
    ///
    /// Why: Defence in depth — if a future refactor adds a name to
    /// `CTO_DB_TOOL_NAMES` that `trusty-cto-db` hasn't published, the
    /// registry should log and skip rather than ship a dead schema.
    /// What: Asserts `new("not_a_real_tool")` returns `None`.
    /// Test: `cargo test -p tc-services new_returns_none_for_unknown_name`.
    #[test]
    fn new_returns_none_for_unknown_name() {
        assert!(CtoDbService::new("not_a_real_tool").is_none());
    }

    /// Calling `execute` with a missing/unreadable DB must return a
    /// recoverable `CtoDbOutcome::Err` — never panic.
    ///
    /// Why: Consumers must keep running even if `cto.db` is unavailable (CI
    /// without the file, a broken symlink, etc).
    /// What: Points `CTO_DB_PATH` at a non-existent file and asserts the
    /// returned outcome is an error naming the tool. Restores the env var
    /// afterwards.
    /// Test: `cargo test -p tc-services execute_returns_error_when_db_missing`.
    #[tokio::test]
    async fn execute_returns_error_when_db_missing() {
        // Save and override CTO_DB_PATH. SAFETY: env mutation in tests is
        // serialised by the harness running this single test; we restore the
        // previous value before returning.
        let prev = std::env::var(trusty_cto_db::ENV_CTO_DB_PATH).ok();
        // SAFETY: single-threaded test, env access is acceptable.
        unsafe {
            std::env::set_var(
                trusty_cto_db::ENV_CTO_DB_PATH,
                "/tmp/definitely-not-a-real-cto-db-path-484.sqlite",
            );
        }

        let service = CtoDbService::new("query_headcount").expect("schema resolves");
        let outcome = service.execute(json!({})).await;
        assert!(outcome.is_error(), "missing DB must yield an error outcome");
        assert!(
            outcome.message().contains("query_headcount"),
            "error message must name the tool: {}",
            outcome.message()
        );

        // Restore.
        // SAFETY: same single-threaded test scope.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(trusty_cto_db::ENV_CTO_DB_PATH, v),
                None => std::env::remove_var(trusty_cto_db::ENV_CTO_DB_PATH),
            }
        }
    }
}
