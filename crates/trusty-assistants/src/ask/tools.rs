//! The ask engine's tool registry: read-only MCP tools plus `okg_search`
//! (#11650).
//!
//! Why: slice 3 exposes only the read-only part of the MCP catalog; write and
//! send tools wait for the approvals work. The catalog carries no read/write
//! marker (`trusty_common::stdio_mcp_client::McpTool` keeps only name,
//! description and input schema), so the rule is an explicit allowlist of
//! known read-only tools. Anything not on it is excluded, including every tool
//! of a server the list does not name.
//! What: [`READ_ONLY_MCP_TOOLS`] keyed by configured server name;
//! [`classify`] splits loaded tool names into kept and [`Exclusion`]s;
//! [`build_registry`] registers the loaded set into a scratch registry, keeps
//! only the allowlisted tools, and adds [`OkgSearchTool`]. No trusty-code
//! coding tool (bash, file edit, delegate, finish) is ever registered.
//! Test: `super::tests::tools_tests` — the whole module.

use std::path::Path;
use std::sync::Arc;

use trusty_code::mcp::{McpDiagnostics, McpToolSet, composed_name, register_configured_tools};
use trusty_code::tools::ToolRegistry;

use super::okg_search::OkgSearchTool;

/// Known read-only MCP tools, by the server name in `servers.toml`.
///
/// Why: derived by reading each server's tool dispatch in this workspace
/// (`trusty-search`, `trusty-memory`, `trusty-mpm`, `trusty-gworkspace`). A
/// tool is listed only when every call is a read; a tool that reads or writes
/// by an `action` argument (`manage_events`) is not listed. A renamed server
/// loses its tools, which is the safe direction.
///
/// Schema audit (#11650 fix round): every listed tool's `inputSchema` was read
/// for a path, destination, output, file or URL-to-write argument. None of
/// the listed tools has one. Excluded by that audit: `get_drive_file_content`
/// and `download_gmail_attachment` (`save_path`, written with `std::fs::write`
/// to any path), and trusty-search `search_similar` (a `file` argument). The
/// remaining path-like arguments are read filters only: trusty-search
/// `path_prefix`, `glob` and `entry_point` scope a query of the index.
/// `memory_recall` and `memory_recall_all` bump a drawer's access count on
/// disk; that is bookkeeping, not a write the model controls. `granola-notes`
/// is not listed: its server is an external binary with no source or
/// `tools/list` fixture in this repo, so its schemas cannot be audited.
/// Test: `super::tests::tools_tests::the_catalog_split_lists_every_exclusion`,
/// `super::tests::tools_tests::path_argument_tools_are_never_registered`.
pub const READ_ONLY_MCP_TOOLS: &[(&str, &[&str])] = &[
    (
        "trusty-search",
        &[
            "search",
            "search_all",
            "search_lexical",
            "search_semantic",
            "search_kg",
            "grep",
            "typeahead",
            "get_call_chain",
            "list_chunks",
            "list_indexes",
            "index_status",
            "search_health",
        ],
    ),
    (
        "trusty-memory",
        &[
            "memory_recall",
            "memory_recall_all",
            "memory_list",
            "kg_query",
            "kg_list_subjects",
            "palace_list",
            "palace_info",
            "get_prompt_context",
            "list_prompt_facts",
        ],
    ),
    (
        "trusty-mpm",
        &["session_list", "session_status", "project_list"],
    ),
    (
        "gworkspace",
        &[
            "list_accounts",
            "search_gmail_messages",
            "get_gmail_message_content",
            "list_message_attachments",
            "list_drive_contents",
            "search_drive_files",
            "list_shared_drives",
            "get_document",
            "get_document_structure",
            "get_document_named_styles",
            "find_tables_in_document",
            "get_spreadsheet",
            "get_slides",
            "list_tasks",
            "query_free_busy",
        ],
    ),
];

/// One loaded MCP tool the ask engine did not register, and why.
/// Test: `super::tests::tools_tests::the_catalog_split_lists_every_exclusion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclusion {
    /// The composed tool name, `mcp__<server>__<tool>`.
    pub tool: String,
    /// Why it was excluded.
    pub reason: &'static str,
}

/// Exclusion reason for a tool whose name says it sends or hands off work.
pub const REASON_SENDS: &str = "sends a message or hands work to another agent";
/// Exclusion reason for a tool whose name says it changes state.
pub const REASON_WRITES: &str = "writes or changes state";
/// Exclusion reason for a tool that takes a local file or path argument.
pub const REASON_PATH_ARG: &str = "takes a local file or path argument";
/// Exclusion reason for every other tool off the allowlist.
pub const REASON_UNLISTED: &str = "not on the read-only allowlist";

/// Tools excluded by the schema audit, by tool name, with the reason. Checked
/// before the name heuristics below, which cannot see a `save_path`.
const AUDITED_EXCLUSIONS: &[(&str, &str)] = &[
    ("get_drive_file_content", REASON_WRITES),
    ("download_gmail_attachment", REASON_WRITES),
    ("search_similar", REASON_PATH_ARG),
];

/// Name fragments that mark a send or hand-off tool.
const SEND_WORDS: &[&str] = &["send", "compose", "delegate", "chat"];

/// Name fragments that mark a state-changing tool.
const WRITE_WORDS: &[&str] = &[
    "create",
    "delete",
    "remove",
    "modify",
    "manage",
    "update",
    "insert",
    "append",
    "replace",
    "format",
    "set_",
    "add_",
    "write",
    "remember",
    "assert",
    "retract",
    "forget",
    "reindex",
    "index_file",
    "complete",
    "rename",
    "move",
    "apply",
    "download",
    "embed",
    "note",
    "dream",
    "compact",
    "migrate",
    "backup",
    "ensure",
    "initialize",
];

/// Whether `composed` (`mcp__<server>__<tool>`) is on the allowlist.
/// Test: `super::tests::tools_tests::the_catalog_split_lists_every_exclusion`.
pub fn is_read_only(composed: &str) -> bool {
    READ_ONLY_MCP_TOOLS.iter().any(|(server, tools)| {
        tools
            .iter()
            .any(|tool| composed_name(server, tool) == composed)
    })
}

/// Whether the server named `server` contributes any allowlisted tool.
///
/// Why: #11650 — the ask engine spawns only these servers; any other server
/// could only contribute tools the allowlist drops.
/// Test: `super::tests::engine_tests::only_servers_with_allowlisted_tools_are_spawned`.
pub fn serves_read_only_tools(server: &str) -> bool {
    READ_ONLY_MCP_TOOLS
        .iter()
        .any(|(name, tools)| *name == server && !tools.is_empty())
}

/// The reason an off-allowlist tool is excluded. Only the message depends on
/// the name; the decision is [`is_read_only`] alone.
fn exclusion_reason(composed: &str) -> &'static str {
    let tool = composed.rsplit("__").next().unwrap_or(composed);
    if let Some((_, reason)) = AUDITED_EXCLUSIONS.iter().find(|(t, _)| *t == tool) {
        reason
    } else if SEND_WORDS.iter().any(|w| tool.contains(w)) {
        REASON_SENDS
    } else if WRITE_WORDS.iter().any(|w| tool.contains(w)) {
        REASON_WRITES
    } else {
        REASON_UNLISTED
    }
}

/// Split loaded tool names into the kept names and the exclusions.
/// Test: `super::tests::tools_tests::the_catalog_split_lists_every_exclusion`.
pub fn classify(loaded: &[String]) -> (Vec<String>, Vec<Exclusion>) {
    let mut kept = Vec::new();
    let mut excluded = Vec::new();
    for name in loaded {
        if is_read_only(name) {
            kept.push(name.clone());
        } else {
            excluded.push(Exclusion {
                tool: name.clone(),
                reason: exclusion_reason(name),
            });
        }
    }
    (kept, excluded)
}

/// The registry one ask runs with, and what was left out.
pub struct AskTools {
    /// Allowlisted MCP tools plus `okg_search`.
    pub registry: ToolRegistry,
    /// Loaded MCP tools that were not registered.
    pub excluded: Vec<Exclusion>,
    /// The MCP loader's own findings (servers down, bad files).
    pub diagnostics: McpDiagnostics,
}

/// Build the ask registry from a loaded MCP set and the instance's OKG.
///
/// What: registers every loaded tool into a scratch registry through
/// trusty-code's own [`register_configured_tools`], keeps the allowlisted ones
/// with [`ToolRegistry::gated`], then registers [`OkgSearchTool`] rooted at
/// `okg_dir`.
/// Test: `super::tests::tools_tests::the_registry_holds_no_coding_write_or_send_tool`,
/// `super::tests::tools_tests::gating_drops_write_and_send_tools_from_a_registry`.
pub fn build_registry(okg_dir: &Path, set: &McpToolSet) -> AskTools {
    let mut scratch = ToolRegistry::new();
    let diagnostics = register_configured_tools(&mut scratch, set);
    let (mut registry, excluded) = read_only_view(&scratch, &diagnostics.tools);
    registry.register(Arc::new(OkgSearchTool::new(okg_dir)));
    AskTools {
        registry,
        excluded,
        diagnostics,
    }
}

/// The allowlisted subset of `scratch`, given the names loaded into it.
/// Test: `super::tests::tools_tests::gating_drops_write_and_send_tools_from_a_registry`.
pub(crate) fn read_only_view(
    scratch: &ToolRegistry,
    loaded: &[String],
) -> (ToolRegistry, Vec<Exclusion>) {
    let (kept, excluded) = classify(loaded);
    (scratch.gated(&kept), excluded)
}
