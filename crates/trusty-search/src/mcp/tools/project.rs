//! The optional `project` argument on the bridge's search tools (#9168).
//!
//! Why: callers guessed index ids (`trusty-tools` for `trusty-tools-4e2cf878`)
//! and got `unknown index`. The daemon owns the project→index map and answers
//! it through `search.project.resolve` (#9169, ruling f6), so the bridge asks
//! rather than re-deriving ids. The matching rules (f6/f7) live in the daemon
//! only.
//! What: [`PROJECT_TOOLS`] names the tools that accept `project`;
//! [`McpServer::resolve_target`] applies the precedence explicit `index_id` >
//! `project` > session pin; a miss becomes [`DispatchError::ProjectUnresolved`]
//! carrying the daemon's candidates; [`annotate_project_tools`] advertises the
//! argument from the same list the dispatcher reads.
//! Test: `tests_project.rs`.

use serde_json::Value;

use super::{types::DispatchError, McpServer};
use crate::service::rpc::project::METHOD_PROJECT_RESOLVE;

/// The tools that accept `project` in place of `index_id` (#9168).
///
/// Why: these are the read tools that take one index. Write tools are left out
/// on purpose: writing to an index picked by a name match is not something a
/// caller can undo.
/// What: one list for the dispatcher and the descriptor annotation, so the
/// schema and the behaviour cannot drift.
/// Test: `project_tools_advertise_the_project_argument`,
/// `project_tools_are_all_read_scoped`.
pub(super) const PROJECT_TOOLS: &[&str] = &[
    "search",
    "search_lexical",
    "search_semantic",
    "search_kg",
    "search_all",
    "grep",
    "typeahead",
    "index_status",
    "list_chunks",
    "get_call_chain",
];

/// `_meta.error_code` / `error.data.error_code` for an unresolved project.
pub const PROJECT_UNRESOLVED: &str = "PROJECT_UNRESOLVED";

/// Bare-method JSON-RPC code for an unresolved project.
///
/// Two slots past [`super::INDEX_UNAVAILABLE_CODE`] (-32012) in the
/// server-reserved range, clear of the daemon's own -32013, so an
/// orchestrator branches on the number alone.
pub const PROJECT_UNRESOLVED_CODE: i32 = -32014;

/// The description given to the `project` property.
const PROP_NOTE: &str = "A project name, `owner/repo`, or absolute path, resolved by the daemon \
                         to its one live index (`search.project.resolve`). Used only when \
                         `index_id` is absent; an explicit `index_id` always wins, and either \
                         one overrides the session pin. A miss returns PROJECT_UNRESOLVED with \
                         the nearest candidates.";

/// The non-empty `project` argument, if any.
///
/// # Errors
///
/// `InvalidParams` when `project` is present but not a string.
pub(super) fn project_arg(args: &Value) -> Result<Option<&str>, DispatchError> {
    match args.get("project") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(other) => Err(DispatchError::InvalidParams(format!(
            "project must be a string (a project name, owner/repo, or path); got {other}"
        ))),
    }
}

/// Refuse `project` on a known tool outside [`PROJECT_TOOLS`] (#9168).
///
/// Why: those tools never read `project`, so a pinned session's
/// `delete_index {project: "other-repo"}` would act on the PINNED index.
/// What: `InvalidParams` when `tool` is advertised, is not in
/// [`PROJECT_TOOLS`], and `project` is present and non-null; `Ok` otherwise,
/// so an unknown tool still reports itself as unknown. The caller checks this
/// before any daemon call.
/// Test: `project_on_a_tool_that_does_not_read_it_is_refused`.
pub(super) fn refuse_unread_project(tool: &str, args: &Value) -> Result<(), DispatchError> {
    let named = args.get("project").is_some_and(|v| !v.is_null());
    if !named || PROJECT_TOOLS.contains(&tool) {
        return Ok(());
    }
    let known = super::tool_descriptors()
        .as_array()
        .is_some_and(|defs| defs.iter().any(|d| d["name"].as_str() == Some(tool)));
    if !known {
        return Ok(());
    }
    Err(DispatchError::InvalidParams(format!(
        "project is not accepted on {tool}; pass index_id"
    )))
}

impl McpServer {
    /// Resolve the index a read tool targets: `index_id`, else `project`, else
    /// the session pin (#9168).
    ///
    /// Why: `index_id` must keep working exactly as before, and a session pin
    /// is a default, so an explicit `project` in the call outranks it.
    /// What: an explicit non-empty `index_id` → that id, with no daemon call.
    /// A `project` → `search.project.resolve`, returning the daemon's
    /// `index_id`. Otherwise the pin, or `None`.
    ///
    /// # Errors
    ///
    /// `InvalidParams` for a non-string `project`; `ProjectUnresolved` when the
    /// daemon has no single live index for it; the usual transport errors when
    /// the call itself fails.
    ///
    /// Test: `project_resolves_through_the_daemon`,
    /// `index_id_wins_over_project_without_a_daemon_call`,
    /// `project_outranks_the_session_pin`.
    pub(super) async fn resolve_target(
        &self,
        args: &Value,
    ) -> Result<Option<String>, DispatchError> {
        if let Some(id) = args.get("index_id").and_then(Value::as_str) {
            if !id.is_empty() {
                return Ok(Some(id.to_string()));
            }
        }
        if let Some(project) = project_arg(args)? {
            return self.resolve_project(project).await.map(Some);
        }
        Ok(self.pinned_index.clone())
    }

    /// Whether a call names a target at all, without resolving it.
    ///
    /// Why: `search_all` fans out only when nothing names an index; it must
    /// make that choice before it resolves anything.
    pub(super) fn names_a_target(&self, args: &Value) -> bool {
        self.resolve_index_id(args).is_some() || matches!(project_arg(args), Ok(Some(_)) | Err(_))
    }

    /// Ask the daemon which index `project` names.
    ///
    /// # Errors
    ///
    /// `ProjectUnresolved` for a refusal whose `data` carries a miss;
    /// otherwise the standard mapping in [`Self::dispatch_error`].
    async fn resolve_project(&self, project: &str) -> Result<String, DispatchError> {
        let resolved = match self
            .daemon
            .call(
                METHOD_PROJECT_RESOLVE,
                serde_json::json!({ "project": project }),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                if let Some(miss) = project_miss(e.data()) {
                    return Err(miss);
                }
                return Err(self.dispatch_error(&e, None));
            }
        };
        resolved
            .get("index_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                DispatchError::Transport(format!(
                    "{METHOD_PROJECT_RESOLVE} answered without an index_id: {resolved}"
                ))
            })
    }
}

/// Turn a resolve refusal's `data` into a structured miss.
///
/// Why: the daemon sends `{error, project, candidates}` for each of its three
/// misses; the caller needs the candidates as data, not inside prose.
/// What: `Some` only when `data.error` is one of the daemon's three miss
/// codes. The payload is the daemon's body plus `error_code`.
/// Test: `an_unresolved_project_surfaces_its_candidates`.
fn project_miss(data: Option<&Value>) -> Option<DispatchError> {
    let obj = data?.as_object()?;
    let code = obj.get("error").and_then(Value::as_str)?;
    if !matches!(
        code,
        "project_not_found" | "project_ambiguous" | "no_live_index"
    ) {
        return None;
    }
    let project = obj.get("project").and_then(Value::as_str).unwrap_or("?");
    let ids: Vec<&str> = obj
        .get("candidates")
        .and_then(Value::as_array)
        .map(|c| {
            c.iter()
                .filter_map(|e| e.get("index_id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let listed = if ids.is_empty() {
        "none".to_string()
    } else {
        ids.join(", ")
    };
    let mut payload = obj.clone();
    payload.insert("error_code".into(), Value::from(PROJECT_UNRESOLVED));
    Some(DispatchError::ProjectUnresolved {
        message: format!(
            "project {project:?} did not resolve to one live index ({code}). \
             Candidates: {listed}. Retry with `index_id` set to one of \
             `candidates[].index_id`, or a more specific `project` (owner/repo or a path)."
        ),
        payload: Value::Object(payload),
    })
}

/// Advertise `project` on every tool in [`PROJECT_TOOLS`].
///
/// Why: a schema-obeying client sends only the arguments the schema lists, and
/// never omits a `required` one — so `index_id` stops being required where
/// `project` can stand in for it (`list_chunks`, `get_call_chain`; the other
/// tools already treat it as optional).
/// What: adds an optional `project` string property and drops `index_id` from
/// `required`. Nothing becomes required, so every call valid before stays
/// valid. A call with neither still gets the same error or directory it did.
/// Test: `project_tools_advertise_the_project_argument`.
pub(super) fn annotate_project_tools(defs: &mut Value) {
    let Some(tools) = defs.as_array_mut() else {
        return;
    };
    for tool in tools.iter_mut() {
        let named = tool
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| PROJECT_TOOLS.contains(&n));
        if !named {
            continue;
        }
        let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) else {
            continue;
        };
        if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|v| v.as_str() != Some("index_id"));
        }
        if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            props.insert(
                "project".into(),
                serde_json::json!({ "type": "string", "description": PROP_NOTE }),
            );
        }
    }
}
