//! The MCP-only trusty-memory tools as `tm memory` verbs (#11625).
//!
//! Why: `recall-deep`, `recall-all`, `list`, `prompt-context`, the task tools
//! and the knowledge-graph tools were reachable only through
//! `mcp__trusty-memory__*`, so a session whose MCP connection is dead lost
//! them. The daemon already forwards every one of them on its socket
//! (`TOOL_METHODS` in trusty-memory's `transport/rpc.rs`).
//! What: [`MemoryTool`] models each tool as data — method name, palace scope,
//! and the MCP schema keys from trusty-memory's `tools/definitions.rs` and
//! `tools/task_definitions.rs`. [`run_tool`] resolves the palace and socket
//! exactly as `core::memory_verbs` does and makes ONE direct-method call through
//! the same `call_method`, so no palace store is opened here (#1078).
//! Test: `memory_tool_verbs_tests.rs`, plus `tests/memory_tool_verbs_socket.rs`
//! for the end-to-end binary path.

use serde_json::{Map, Value, json};

use super::memory_verbs::{
    MemoryVerbError, MemoryVerbOptions, MemoryVerbOutcome, call_method, resolve_palace_for,
    resolve_verb_socket,
};

/// How a tool addresses a palace.
///
/// Why: a write needs a palace named; a read with none is answered by the
/// daemon with a palace index (#6318); a cross-palace tool takes no palace key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalaceScope {
    /// Writes to one palace; resolution failure is an error.
    Write,
    /// Reads one palace; resolution failure sends no `palace` key.
    Read,
    /// Spans every palace; never sends a `palace` key.
    None,
}

/// One MCP-only trusty-memory tool and the arguments it carries.
///
/// Why: the tools differ only in method, palace scope and argument keys, so
/// data keeps [`run_tool`] one path and the keys in one place to audit.
/// What: one variant per tool; field names are the schema keys they map to.
/// Test: `every_tool_names_its_method_and_scope`,
/// `arguments_carry_only_the_schema_keys_supplied`.
#[derive(Debug, Clone)]
pub enum MemoryTool {
    /// `memory_recall_deep`: L3 full HNSW search.
    RecallDeep {
        /// The search text.
        query: String,
        /// `top_k`; omitted leaves the daemon default.
        top_k: Option<u64>,
        /// Restrict the search to one room.
        room: Option<String>,
        /// Relevance floor.
        min_score: Option<f64>,
    },
    /// `memory_recall_all`: semantic search across palaces.
    RecallAll {
        /// The search text, sent as `q`.
        query: String,
        /// `top_k`; omitted leaves the daemon default.
        top_k: Option<u64>,
        /// Use the deep lane in each palace.
        deep: bool,
        /// `resident` or `all`; omitted leaves the daemon default.
        scope: Option<String>,
    },
    /// `memory_list`: list drawers, optionally filtered.
    List {
        /// Filter by room.
        room: Option<String>,
        /// Filter by wing.
        wing: Option<String>,
        /// Filter by tag.
        tag: Option<String>,
        /// Max drawers.
        limit: Option<u64>,
    },
    /// `get_prompt_context`: the hot prompt facts as Markdown.
    PromptContext {
        /// Substring filter on subject or object.
        query: Option<String>,
    },
    /// `task_add`: create a Task drawer.
    TaskAdd {
        /// Task description.
        content: String,
        /// Room to file it in.
        room: Option<String>,
        /// Tags to store alongside it.
        tags: Vec<String>,
    },
    /// `task_list`: list Task drawers.
    TaskList {
        /// Include completed tasks.
        include_completed: bool,
    },
    /// `task_complete`: mark a Task drawer completed.
    TaskComplete {
        /// UUID of the Task drawer.
        drawer_id: String,
    },
    /// `kg_assert`: assert a knowledge-graph triple.
    KgAssert {
        /// Triple subject.
        subject: String,
        /// Triple predicate.
        predicate: String,
        /// Triple object.
        object: String,
        /// Confidence; omitted leaves the daemon default (1.0).
        confidence: Option<f64>,
        /// Where the fact came from.
        provenance: Option<String>,
    },
    /// `kg_query`: active triples for one subject.
    KgQuery {
        /// The subject to query.
        subject: String,
    },
    /// `kg_gaps`: the cached knowledge gaps.
    KgGaps,
}

impl MemoryTool {
    /// The JSON-RPC method this tool calls.
    pub fn method(&self) -> &'static str {
        match self {
            Self::RecallDeep { .. } => "memory_recall_deep",
            Self::RecallAll { .. } => "memory_recall_all",
            Self::List { .. } => "memory_list",
            Self::PromptContext { .. } => "get_prompt_context",
            Self::TaskAdd { .. } => "task_add",
            Self::TaskList { .. } => "task_list",
            Self::TaskComplete { .. } => "task_complete",
            Self::KgAssert { .. } => "kg_assert",
            Self::KgQuery { .. } => "kg_query",
            Self::KgGaps => "kg_gaps",
        }
    }

    /// The CLI name, as `--json` reports it in `verb`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::RecallDeep { .. } => "recall-deep",
            Self::RecallAll { .. } => "recall-all",
            Self::List { .. } => "list",
            Self::PromptContext { .. } => "prompt-context",
            Self::TaskAdd { .. } => "task add",
            Self::TaskList { .. } => "task list",
            Self::TaskComplete { .. } => "task complete",
            Self::KgAssert { .. } => "kg assert",
            Self::KgQuery { .. } => "kg query",
            Self::KgGaps => "kg gaps",
        }
    }

    /// How this tool addresses a palace.
    pub fn scope(&self) -> PalaceScope {
        match self {
            Self::RecallAll { .. } | Self::PromptContext { .. } => PalaceScope::None,
            Self::TaskAdd { .. } | Self::TaskComplete { .. } | Self::KgAssert { .. } => {
                PalaceScope::Write
            }
            _ => PalaceScope::Read,
        }
    }

    /// The result key holding the tool's rows, for the envelope's `count`.
    pub fn items_key(&self) -> Option<&'static str> {
        match self {
            Self::RecallDeep { .. } | Self::RecallAll { .. } => Some("results"),
            Self::List { .. } => Some("drawers"),
            Self::TaskList { .. } => Some("tasks"),
            Self::KgQuery { .. } => Some("triples"),
            Self::KgGaps => Some("gaps"),
            _ => None,
        }
    }

    /// The tool arguments, without `palace`; only keys the caller supplied.
    ///
    /// Test: `arguments_carry_only_the_schema_keys_supplied`.
    pub fn arguments(&self) -> Map<String, Value> {
        let mut args = Map::new();
        let mut put = |key: &str, value: Option<Value>| {
            if let Some(value) = value {
                args.insert(key.to_string(), value);
            }
        };
        match self {
            Self::RecallDeep {
                query,
                top_k,
                room,
                min_score,
            } => {
                put("query", Some(json!(query)));
                put("top_k", top_k.map(Value::from));
                put("room", room.clone().map(Value::from));
                put("min_score", min_score.map(Value::from));
            }
            Self::RecallAll {
                query,
                top_k,
                deep,
                scope,
            } => {
                put("q", Some(json!(query)));
                put("top_k", top_k.map(Value::from));
                put("deep", deep.then_some(Value::Bool(true)));
                put("scope", scope.clone().map(Value::from));
            }
            Self::List {
                room,
                wing,
                tag,
                limit,
            } => {
                put("room", room.clone().map(Value::from));
                put("wing", wing.clone().map(Value::from));
                put("tag", tag.clone().map(Value::from));
                put("limit", limit.map(Value::from));
            }
            Self::PromptContext { query } => put("query", query.clone().map(Value::from)),
            Self::TaskAdd {
                content,
                room,
                tags,
            } => {
                put("content", Some(json!(content)));
                put("room", room.clone().map(Value::from));
                put("tags", (!tags.is_empty()).then(|| json!(tags)));
            }
            Self::TaskList { include_completed } => {
                put(
                    "include_completed",
                    include_completed.then_some(Value::Bool(true)),
                );
            }
            Self::TaskComplete { drawer_id } => put("drawer_id", Some(json!(drawer_id.trim()))),
            Self::KgAssert {
                subject,
                predicate,
                object,
                confidence,
                provenance,
            } => {
                put("subject", Some(json!(subject)));
                put("predicate", Some(json!(predicate)));
                put("object", Some(json!(object)));
                put("confidence", confidence.map(Value::from));
                put("provenance", provenance.clone().map(Value::from));
            }
            Self::KgQuery { subject } => put("subject", Some(json!(subject))),
            Self::KgGaps => {}
        }
        args
    }
}

/// Run one tool against the running trusty-memory daemon.
///
/// Why: the no-MCP path for the tools [`MemoryTool`] covers; it opens no
/// palace store, so a palace the daemon holds is never opened twice (#1078).
/// What: resolves the palace per [`MemoryTool::scope`] — `--palace` wins, a
/// cross-palace tool sends none — and the socket, makes one call, and wraps
/// the daemon's body verbatim in the shared [`MemoryVerbOutcome`] envelope.
/// `count` is the length of the tool's row array, `null` for a write or a
/// non-list answer.
///
/// # Errors
///
/// [`MemoryVerbError::Palace`] when a write has no palace,
/// [`MemoryVerbError::Socket`] when the socket path cannot be derived, and
/// [`MemoryVerbError::Call`], naming the socket, when nothing answers or the
/// daemon refuses.
///
/// Test: `a_write_tool_without_a_resolvable_palace_is_refused`,
/// `a_dead_socket_is_an_error_naming_it`, `recall_all_sends_no_palace`.
pub async fn run_tool(
    tool: &MemoryTool,
    opts: &MemoryVerbOptions,
) -> Result<MemoryVerbOutcome, MemoryVerbError> {
    let palace = match tool.scope() {
        PalaceScope::None => None,
        PalaceScope::Read => resolve_palace_for(false, opts)?,
        PalaceScope::Write => resolve_palace_for(true, opts)?,
    };
    let socket = resolve_verb_socket(opts)?;

    let mut args = tool.arguments();
    if let Some(palace) = palace.clone() {
        args.insert("palace".to_string(), Value::String(palace));
    }
    let result = call_method(&socket, tool.method(), Value::Object(args)).await?;
    let count = tool
        .items_key()
        .and_then(|key| result.get(key))
        .and_then(Value::as_array)
        .map(Vec::len);

    Ok(MemoryVerbOutcome {
        verb: tool.label().to_string(),
        palace,
        socket: socket.display().to_string(),
        count,
        result,
    })
}

#[cfg(test)]
#[path = "memory_tool_verbs_tests.rs"]
mod tests;
