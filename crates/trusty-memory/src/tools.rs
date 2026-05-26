//! MCP tool surface for trusty-memory.
//!
//! Why: Concentrates the public tool contract in one file so changes are
//! auditable and the MCP schema stays in sync with the implementation.
//! What: Defines `MemoryMcpServer`, `tool_definitions()` (the MCP
//! `tools/list` payload), and the in-process tool dispatcher wired to the
//! real `PalaceRegistry` + retrieval / KG APIs.
//! Test: `cargo test -p trusty-memory-mcp` validates the schema and dispatch.
//!
//! Tools exposed:
//! - `memory_remember(palace, text, room?, tags?)` -> drawer_id
//! - `memory_recall(palace, query, top_k?)`        -> Vec<Drawer> (L0+L1+L2)
//! - `memory_recall_deep(palace, query, top_k?)`   -> Vec<Drawer> (L3 deep)
//! - `memory_list(palace, room?, tag?, limit?)`    -> Vec<Drawer>
//! - `memory_forget(palace, drawer_id)`            -> ()
//! - `palace_create(name, description?)`           -> PalaceId
//! - `palace_list()`                                -> Vec<PalaceId>
//! - `palace_info(palace)`                          -> palace metadata + stats
//! - `kg_assert(palace, subject, predicate, object, confidence?, provenance?)` -> ()
//! - `kg_query(palace, subject)`                    -> Vec<Triple>

use crate::attribution::{session_tag_from_tags, CreatorInfo, CreatorSource, MCP_CLIENT_NAME};
use crate::kg_extract::{extract_triples, ExtractInput};
use crate::{ActivitySource, AppState, DaemonEvent};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::filter::{FilterConfig, MCP_MIN_TOKENS};
use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    recall, recall_across_palaces, recall_deep, RememberOptions,
};
use trusty_common::memory_core::store::kg::Triple;
use uuid::Uuid;

/// Look up the friendly palace name (Palace.name) from disk, falling back to
/// the id when the metadata can't be read.
///
/// Why (issue #96): MCP-side emit calls need the same `palace_name` field
/// the HTTP path emits so the activity feed renders identical labels
/// regardless of origin. Re-reading the on-disk metadata is the simplest
/// way to avoid drift — name changes propagate immediately.
/// What: walks the registry's on-disk listing and returns the matching
/// `name`. On any error, returns the id verbatim so emit calls never fail.
/// Test: implicit — the MCP emit tests assert the `palace_id` matches; the
/// fallback is the same id-as-name behaviour the HTTP path uses.
fn lookup_palace_name(state: &AppState, palace_id: &str) -> String {
    trusty_common::memory_core::PalaceRegistry::list_palaces(&state.data_root)
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.id.0 == palace_id).map(|p| p.name))
        .unwrap_or_else(|| palace_id.to_string())
}

/// Minimum standalone-content word count enforced by [`content_gate`].
///
/// Why (issue #215): single-word user replies ("yes", "ok", "no thanks") have
/// no standalone memory value when the surrounding turn isn't captured
/// alongside them — they end up in the palace as orphan fragments that
/// pollute recall results. Requiring at least four whitespace-separated tokens
/// is a cheap heuristic that matches the natural boundary between "just a
/// reaction" and "an actual statement".
/// What: the threshold the gate compares against. Tokens are counted via
/// `split_whitespace().count()`, so punctuation does not inflate the count.
/// Test: `content_gate_blocks_short_no_context`, `content_gate_keeps_long`.
const CONTENT_GATE_MIN_WORDS: usize = 4;

/// Gate short standalone content unless a `context` wrapper is supplied.
///
/// Why: single-word or very-short standalone user responses ("yes", "ok")
/// have no standalone memory value (issue #215). Gate them unless a context
/// is provided.
/// What: returns `None` if `content` has fewer than [`CONTENT_GATE_MIN_WORDS`]
/// whitespace-separated tokens AND `context` is `None` (the write should be
/// skipped). Returns `Some(combined)` where `combined = "<context>\n\n---\n\n<content>"`
/// when `context` is `Some` and non-empty after trimming. Returns
/// `Some(content)` unchanged when `content` has at least
/// [`CONTENT_GATE_MIN_WORDS`] tokens. Tokens are counted on the trimmed
/// `content` so trailing whitespace doesn't inflate the count.
/// Test: `content_gate_blocks_short_no_context`,
/// `content_gate_wraps_short_with_context`,
/// `content_gate_keeps_long`, `content_gate_blank_context_treated_as_none`.
fn content_gate(content: &str, context: Option<&str>) -> Option<String> {
    let trimmed = content.trim();
    let word_count = trimmed.split_whitespace().count();
    // Treat a context that is empty or whitespace-only as "no context" — a
    // caller passing `""` should not unlock a write the gate would otherwise
    // drop, and the combined output would otherwise begin with a meaningless
    // separator.
    let context_clean = context.map(str::trim).filter(|s| !s.is_empty());
    if let Some(ctx) = context_clean {
        return Some(format!("{ctx}\n\n---\n\n{content}"));
    }
    if word_count < CONTENT_GATE_MIN_WORDS {
        return None;
    }
    Some(content.to_string())
}

/// Patterns whose content should never be stored as standalone memories.
///
/// Why (issue #220): the activity panel was being flooded with low-value
/// Claude Code auto-captures — `Tool use: Bash`, `Tool use: Edit File: …`,
/// `Claude Code session ended: <uuid>` — that carry no semantic value once
/// the surrounding turn is gone. They pollute recall results and burn UI
/// real estate. A blocklist is the cheapest way to filter them at write
/// time without coordinating with the auto-capture hook source.
/// What: substring patterns (not regexes) checked via `str::contains` so
/// the matcher stays branch-predictable and never panics on malformed
/// input. Patterns are intentionally lower-case-friendly but matched
/// case-sensitively because the auto-capture hooks always emit the exact
/// English prefix.
/// Test: `blocklist_gate_blocks_tool_use`,
/// `blocklist_gate_blocks_session_ended`,
/// `blocklist_gate_passes_normal_content`.
const BLOCKLIST_PATTERNS: &[&str] = &[
    "Tool use: ",          // Claude Code tool-use captures
    "Claude Code session", // Session lifecycle events
];

/// Rolling-window horizon for the dedup gate.
///
/// Why (issue #220): identical content is often emitted multiple times in
/// quick succession (auto-capture hook bursts, retries, copy-paste). A
/// 5-minute window catches the burst without rejecting deliberate user
/// re-statements hours later.
/// What: `chrono::Duration` value. Drawers created before
/// `now - DEDUP_WINDOW` are ignored by the dedup pass.
/// Test: indirect via `dedup_skips_near_duplicate` and
/// `dedup_allows_different_content` (use the helper directly).
const DEDUP_WINDOW_MINUTES: i64 = 5;

/// Maximum number of recent drawers the dedup pass scans.
///
/// Why: a palace can hold tens of thousands of drawers; we never need to
/// compare the new write against more than the most-recent handful to
/// catch the bursty-duplicate case. Capping the scan keeps the hot path
/// O(1) in the palace size.
/// What: ceiling on the candidate list pulled from
/// `PalaceHandle::list_drawers` before the time-window filter.
/// Test: `dedup_skips_near_duplicate` exercises the scan against a small
/// candidate set; the cap is enforced by `list_drawers`'s `limit` arg.
const DEDUP_SCAN_LIMIT: usize = 50;

/// Jaro-Winkler similarity threshold above which a candidate counts as a
/// near-duplicate of the new content.
///
/// Why: 0.92 is the empirically-chosen cutoff documented in the issue —
/// high enough to allow distinct facts to coexist, low enough to catch
/// trivial whitespace / punctuation / suffix variation. Jaro-Winkler is
/// preferred over plain Jaro because the auto-capture noise tends to share
/// the same prefix (`Tool use: …`, `Edit File: …`), which Jaro-Winkler
/// weights heavily.
/// What: `f64` threshold compared against `strsim::jaro_winkler`'s output.
/// Test: `dedup_skips_near_duplicate`, `dedup_allows_different_content`.
const DEDUP_SIMILARITY_THRESHOLD: f64 = 0.92;

/// Blocklist gate: returns true when the content should be silently
/// skipped because it matches a known low-value auto-capture pattern.
///
/// Why (issue #220): Centralises the pattern-match logic so both
/// `memory_remember` and `memory_note` go through the same filter. Trims
/// leading whitespace before matching so indented variants still hit.
/// What: returns `true` iff `content.contains(pat)` for any pattern in
/// `BLOCKLIST_PATTERNS`. Trimming uses `str::trim_start` to keep the
/// substring check predictable (the suffixes after the prefix can vary).
/// Test: `blocklist_gate_blocks_tool_use`,
/// `blocklist_gate_blocks_session_ended`,
/// `blocklist_gate_passes_normal_content`.
fn blocklist_gate(content: &str) -> bool {
    let trimmed = content.trim_start();
    BLOCKLIST_PATTERNS.iter().any(|pat| trimmed.contains(pat))
}

/// Dedup gate: returns true when the new content is a near-duplicate of a
/// drawer written to the same palace within the rolling window.
///
/// Why (issue #220): bursts of identical or near-identical content (auto-
/// capture retries, hook re-emissions, copy-paste artefacts) were
/// inflating the palace with no recall benefit. A short rolling window
/// catches the burst without rejecting deliberate re-statements hours
/// later.
/// What: pulls up to `DEDUP_SCAN_LIMIT` recent drawers from the live
/// in-memory table via `list_drawers` (a cheap snapshot, no I/O), filters
/// to those created within `DEDUP_WINDOW_MINUTES` of `now`, then computes
/// `strsim::jaro_winkler` against each. Returns `true` on the first match
/// above `DEDUP_SIMILARITY_THRESHOLD`. Returns `false` if `content` is
/// empty after trimming (the content gate handles that case separately)
/// or if the palace has no recent drawers.
/// Test: `dedup_skips_near_duplicate`, `dedup_allows_different_content`.
fn dedup_gate(handle: &trusty_common::memory_core::PalaceHandle, content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let now = chrono::Utc::now();
    let window_start = now - chrono::Duration::minutes(DEDUP_WINDOW_MINUTES);
    let recent = handle.list_drawers(None, None, DEDUP_SCAN_LIMIT);
    recent
        .iter()
        .filter(|d| d.created_at >= window_start)
        .any(|d| strsim::jaro_winkler(trimmed, d.content.trim()) > DEDUP_SIMILARITY_THRESHOLD)
}

/// Build the strict MCP-level `RememberOptions`.
///
/// Why: Issue #61 — the MCP boundary is where auto-capture hooks deposit
/// raw tool/commit/prompt data; we want the 8-token threshold there even
/// though the library default is more permissive for direct callers.
/// What: Clones the default filter and bumps `min_tokens` to `MCP_MIN_TOKENS`.
/// Test: `dispatch_remember_rejects_short_content`.
fn mcp_remember_opts(force: bool) -> RememberOptions {
    let filter = FilterConfig {
        min_tokens: MCP_MIN_TOKENS,
        ..FilterConfig::default()
    };
    RememberOptions {
        filter,
        force,
        ..RememberOptions::default()
    }
}

/// Marker server type. Reserved for future stateful MCP server impls.
///
/// Why: Keep a stable type name while the protocol-loop is implemented at
/// module level, so external callers can still depend on a server symbol.
/// What: Zero-sized struct with `new` / `Default`.
/// Test: `MemoryMcpServer::default()` constructs without panic.
pub struct MemoryMcpServer;

impl MemoryMcpServer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

/// MCP `tools/list` response payload.
///
/// Why: Claude Code calls `tools/list` once on connect and uses the schema
/// to drive the tool picker; the schema is the source of truth for arg names.
/// `palace` is required only when the server has no `--palace` default
/// configured — when a default is set, the schema omits `palace` from
/// `required` so clients can drop it.
/// What: Returns a JSON object `{ "tools": [...] }` with all 10 tool defs.
/// Test: `tool_definitions_lists_all_tools`,
/// `tool_definitions_drops_palace_required_when_default_set`.
pub fn tool_definitions() -> Value {
    tool_definitions_with(false)
}

/// Variant of `tool_definitions` aware of whether a default palace is
/// configured. When `has_default` is true, the `palace` argument is moved
/// out of the `required` list for every tool that takes it.
///
/// Why: Lets `handle_message` emit a schema that matches the running
/// server's actual contract — clients reading the schema should see exactly
/// what they need to send.
/// What: Builds the same shape as `tool_definitions` but with conditional
/// `required` arrays.
/// Test: `tool_definitions_drops_palace_required_when_default_set`.
pub fn tool_definitions_with(has_default: bool) -> Value {
    let memory_remember_required: Vec<&str> = if has_default {
        vec!["text"]
    } else {
        vec!["palace", "text"]
    };
    let memory_recall_required: Vec<&str> = if has_default {
        vec!["query"]
    } else {
        vec!["palace", "query"]
    };
    let kg_assert_required: Vec<&str> = if has_default {
        vec!["subject", "predicate", "object"]
    } else {
        vec!["palace", "subject", "predicate", "object"]
    };
    let kg_query_required: Vec<&str> = if has_default {
        vec!["subject"]
    } else {
        vec!["palace", "subject"]
    };
    let memory_list_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    let memory_forget_required: Vec<&str> = if has_default {
        vec!["drawer_id"]
    } else {
        vec!["palace", "drawer_id"]
    };
    let palace_info_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    let palace_compact_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    let memory_note_required: Vec<&str> = if has_default {
        vec!["content"]
    } else {
        vec!["palace", "content"]
    };

    json!({
        "tools": [
            {
                "name": "memory_remember",
                "description": "Store a memory (drawer) in a palace room. Content is filtered for signal vs. noise (issue #61): rejects empty/very short content, raw tool/commit output, and code-only blobs. Issue #215: very short standalone content (< 4 words) is silently dropped unless a `context` is supplied, in which case the context is prepended so the stored memory has standalone value. Pass force=true to bypass filtering, or use memory_note for short curated facts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":  {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                        "text":    {"type": "string", "description": "Memory content"},
                        "room":    {"type": "string", "description": "Room type (optional)"},
                        "tags":    {"type": "array", "items": {"type": "string"}},
                        "force":   {"type": "boolean", "description": "Bypass the signal/noise filter. Use sparingly — intended for explicit operator overrides.", "default": false},
                        "context": {"type": "string", "description": "Optional surrounding context. When supplied alongside very short content (< 4 words), the context is prepended (separated by `---`) so the stored memory has standalone meaning; without it, short content is dropped (issue #215)."}
                    },
                    "required": memory_remember_required,
                }
            },
            {
                "name": "memory_note",
                "description": "Curated shortcut for short, high-signal facts (\"User prefers snake_case\", \"Deploy target is prod-east\"). Bypasses the token-length filter but still rejects auto-capture noise. Stored as DrawerType::UserFact with importance 1.0. Issue #215: a `context` argument can be supplied to wrap an otherwise meaningless single-word response.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":  {"type": "string"},
                        "content": {"type": "string", "description": "Brief fact to remember"},
                        "tags":    {"type": "array", "items": {"type": "string"}},
                        "context": {"type": "string", "description": "Optional surrounding context. Prepended to `content` (separated by `---`) when supplied; with very short content (< 4 words) and no context the write is skipped (issue #215)."}
                    },
                    "required": memory_note_required,
                }
            },
            {
                "name": "memory_recall",
                "description": "Recall memories using L0+L1+L2 progressive retrieval.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"},
                        "query":  {"type": "string"},
                        "top_k":  {"type": "integer", "default": 10}
                    },
                    "required": memory_recall_required,
                }
            },
            {
                "name": "memory_recall_deep",
                "description": "Deep recall using L3 full HNSW search.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"},
                        "query":  {"type": "string"},
                        "top_k":  {"type": "integer", "default": 10}
                    },
                    "required": memory_recall_required,
                }
            },
            {
                "name": "palace_create",
                "description": "Create a new memory palace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name":        {"type": "string"},
                        "description": {"type": "string"}
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "palace_list",
                "description": "List all palaces on this machine.",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "palace_delete",
                "description": "Delete an entire memory palace, including its drawers, vectors, and knowledge graph. Refuses to delete a non-empty palace unless `force=true` is set.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace_id": {"type": "string", "description": "Id of the palace to delete."},
                        "force":     {"type": "boolean", "description": "Required when the palace still has drawers; defaults to false.", "default": false}
                    },
                    "required": ["palace_id"]
                }
            },
            {
                "name": "palace_update",
                "description": "Update the display name of an existing palace. The palace's drawers, vectors, and knowledge graph are preserved; only the human-readable name changes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace_id": {"type": "string", "description": "Id of the palace to rename."},
                        "name":      {"type": "string", "description": "New display name. Trimmed; must be non-empty."}
                    },
                    "required": ["palace_id", "name"]
                }
            },
            {
                "name": "kg_assert",
                "description": "Assert a fact in the temporal knowledge graph.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string"},
                        "subject":    {"type": "string"},
                        "predicate":  {"type": "string"},
                        "object":     {"type": "string"},
                        "confidence": {"type": "number", "default": 1.0},
                        "provenance": {"type": "string"}
                    },
                    "required": kg_assert_required,
                }
            },
            {
                "name": "kg_query",
                "description": "Query active knowledge-graph triples for a subject.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":  {"type": "string"},
                        "subject": {"type": "string"}
                    },
                    "required": kg_query_required,
                }
            },
            {
                "name": "memory_list",
                "description": "List drawers in a palace, optionally filtered by room type or tag.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"},
                        "room":   {"type": "string", "description": "Filter by room type (Frontend, Backend, Testing, Planning, Documentation, Research, Configuration, Meetings, General, or custom)"},
                        "tag":    {"type": "string", "description": "Filter by tag"},
                        "limit":  {"type": "integer", "description": "Max results (default 50)"}
                    },
                    "required": memory_list_required,
                }
            },
            {
                "name": "memory_forget",
                "description": "Delete a drawer from a palace by its UUID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":    {"type": "string"},
                        "drawer_id": {"type": "string", "description": "UUID of the drawer to delete"}
                    },
                    "required": memory_forget_required,
                }
            },
            {
                "name": "palace_info",
                "description": "Get metadata and stats for a single palace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"}
                    },
                    "required": palace_info_required,
                }
            },
            {
                "name": "palace_compact",
                "description": "Remove orphaned vector index entries (vectors with no matching drawer row). See issue #49.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"}
                    },
                    "required": palace_compact_required,
                }
            },
            {
                "name": "add_alias",
                "description": "Add a short→full alias (e.g. tga → trusty-git-analytics) to the prompt-facts surface. Asserts the alias as a hot KG triple and refreshes the session-init prompt cache.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "short": {"type": "string", "description": "Short name / alias (subject)"},
                        "full":  {"type": "string", "description": "Full / canonical name (object)"},
                        "extra": {"type": "string", "description": "Optional extra context appended to the full name"}
                    },
                    "required": ["short", "full"],
                }
            },
            {
                "name": "list_prompt_facts",
                "description": "List every active prompt-fact triple (aliases, conventions, facts, shorthands) across all palaces.",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "remove_prompt_fact",
                "description": "Retract the active triple for a (subject, predicate) pair from the prompt-facts surface. Closes the interval without inserting a replacement.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "subject":   {"type": "string"},
                        "predicate": {"type": "string", "description": "One of is_alias_for, has_convention, is_fact, is_shorthand_for"}
                    },
                    "required": ["subject", "predicate"],
                }
            },
            {
                "name": "get_prompt_context",
                "description": "Fetch the current project context (aliases, conventions, facts, shorthands) from the memory palace as a Markdown block ready to drop into the model's working context. Call at the start of each turn. Pass an optional `query` to filter to facts whose subject or object contains the query string (case-insensitive).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Optional filter — only return facts whose subject or object contains this string (case-insensitive). Omit to return all hot facts."
                        }
                    }
                }
            },
            {
                "name": "discover_aliases",
                "description": "Auto-discover project aliases by scanning Cargo workspace members, binary names, first-letter abbreviations, and the git remote. Asserts any newly-discovered (short, is_alias_for, full) triples into the resolved palace and rebuilds the prompt cache. Skips triples that already exist active in the KG.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project_root": {"type": "string", "description": "Optional filesystem path to scan. Defaults to the process cwd."}
                    }
                }
            },
            {
                "name": "kg_gaps",
                "description": "List knowledge gaps detected in the memory palace graph. Returns communities (clusters of related entities) with low internal density that may benefit from additional knowledge. Populated by the dream cycle; an empty list means no cycle has run yet.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string", "description": "Palace name (optional, defaults to the active palace)"}
                    }
                }
            },
            {
                "name": "kg_bootstrap",
                "description": "Seed the knowledge graph from well-known project files (Cargo.toml, package.json, pyproject.toml, go.mod, CLAUDE.md, .git/config). Asserts structured triples (has_language, has_version, source_repo, ...) plus temporal metadata (created_at, bootstrapped_at). Idempotent: re-running refreshes bootstrapped_at without disturbing created_at. See issue #60.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":       {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                        "project_path": {"type": "string", "description": "Filesystem path to scan. Omit to scan the palace's own data dir (temporal metadata only)."}
                    }
                }
            },
            {
                "name": "memory_recall_all",
                "description": "Semantic search across ALL palaces simultaneously. Returns the top-k most relevant drawers ranked by similarity, regardless of which palace they belong to. Each result includes a `palace_id` field identifying its source.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "q":     {"type": "string", "description": "Free-text query"},
                        "top_k": {"type": "integer", "default": 10},
                        "deep":  {"type": "boolean", "default": false}
                    },
                    "required": ["q"],
                }
            },
            {
                "name": "memory_send_message",
                "description": "Send an inter-project message (issue #99). Writes a tagged drawer into the recipient palace; the recipient's SessionStart hook picks it up via `trusty-memory inbox-check`. `to_palace` is the recipient repo slug (e.g. `trusty-tools`, `claude-mpm`). `from_palace` defaults to the calling project's cwd-derived slug when omitted.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "to_palace":   {"type": "string", "description": "Recipient palace id (repo slug)."},
                        "purpose":     {"type": "string", "description": "Free-text purpose / category (e.g. `task`, `notify`, `reply`)."},
                        "content":     {"type": "string", "description": "Message body — plain text, no length limit. Rendered into the recipient session as a Markdown block."},
                        "from_palace": {"type": "string", "description": "Sender palace id (optional, defaults to cwd-derived slug)."}
                    },
                    "required": ["to_palace", "purpose", "content"],
                }
            }
        ]
    })
}

/// Reverse of `parse_room`: produce a stable label for KG `in-room`
/// extraction.
///
/// Why: The auto-extractor wants the same friendly label the caller passed
/// (`"Backend"`, `"General"`, …) so the graph stays consistent across
/// remember calls regardless of how the MCP client spelled the argument.
/// What: Returns the canonical enum-name string for the built-in variants
/// and the inner string for `Custom`.
/// Test: Indirect — `auto_kg_extraction_hooks_into_memory_remember`
/// round-trips a known room label.
pub(crate) fn room_label(room: &RoomType) -> Option<String> {
    let label = match room {
        RoomType::Frontend => "Frontend",
        RoomType::Backend => "Backend",
        RoomType::Testing => "Testing",
        RoomType::Planning => "Planning",
        RoomType::Documentation => "Documentation",
        RoomType::Research => "Research",
        RoomType::Configuration => "Configuration",
        RoomType::Meetings => "Meetings",
        RoomType::General => "General",
        RoomType::Custom(s) => return Some(s.clone()),
    };
    Some(label.to_string())
}

/// Parse a `RoomType` from an optional string (`"Backend"`, `"Frontend"`,
/// etc.) — falls back to `RoomType::General` when unset or unknown.
///
/// Why: MCP arguments are JSON; we accept the friendly enum-name forms so
/// callers don't have to learn an internal serialization.
/// What: Match-on-string returning the corresponding `RoomType`.
/// Test: Indirectly via `dispatch_remember_then_recall`.
fn parse_room(s: Option<&str>) -> RoomType {
    match s.unwrap_or("General") {
        "Frontend" => RoomType::Frontend,
        "Backend" => RoomType::Backend,
        "Testing" => RoomType::Testing,
        "Planning" => RoomType::Planning,
        "Documentation" => RoomType::Documentation,
        "Research" => RoomType::Research,
        "Configuration" => RoomType::Configuration,
        "Meetings" => RoomType::Meetings,
        "General" => RoomType::General,
        other => RoomType::Custom(other.to_string()),
    }
}

/// Resolve (or lazily open) the palace handle for a tool call.
fn open_palace_handle(
    state: &AppState,
    palace_id: &str,
) -> Result<std::sync::Arc<trusty_common::memory_core::PalaceHandle>> {
    let pid = PalaceId::new(palace_id);
    state
        .registry
        .open_palace(&state.data_root, &pid)
        .with_context(|| format!("open palace {palace_id}"))
}

/// Run deterministic KG extraction over a freshly-written drawer and assert
/// any resulting triples through the palace's `KnowledgeGraph`.
///
/// Why: Issue #97 — `memory_remember` and `memory_note` should auto-populate
/// the KG so palaces with drawers always have a graph. The extractor is pure
/// and offline so the write hot path stays fast; failures *must never* fail
/// the parent write (the drawer is already on disk), so this function logs
/// and swallows every error.
/// What: Builds an `ExtractInput`, runs `extract_triples`, then calls
/// `handle.kg.assert` for each triple. Any failure during assertion is
/// captured as a `tracing::warn!` and the rest of the triples are still
/// attempted; the function returns nothing.
/// Test: `auto_kg_extraction_hooks_into_memory_remember`,
/// `auto_kg_extraction_no_op_does_not_fail_remember`,
/// `web::tests::http_create_drawer_runs_auto_kg_extraction`.
pub(crate) async fn auto_extract_and_assert(
    handle: &std::sync::Arc<trusty_common::memory_core::PalaceHandle>,
    drawer_id: Uuid,
    content: &str,
    tags: &[String],
    room: Option<&str>,
) {
    let input = ExtractInput {
        drawer_id,
        content,
        tags,
        room,
    };
    let triples = extract_triples(&input);
    if triples.is_empty() {
        return;
    }
    for triple in triples {
        let s = triple.subject.clone();
        let p = triple.predicate.clone();
        if let Err(e) = handle.kg.assert(triple).await {
            tracing::warn!(
                drawer_id = %drawer_id,
                subject = %s,
                predicate = %p,
                "auto kg extraction: assert failed (non-fatal): {e:#}",
            );
        }
    }
}

/// Resolve a palace argument, falling back to `state.default_palace` when
/// the caller omitted `palace`.
///
/// Why: `serve --palace <name>` lets the operator bind a process to a single
/// project namespace; tool calls then no longer need to repeat the palace
/// every time. This helper centralises the precedence rule (explicit arg
/// wins over default) and produces a uniform error when neither is set.
/// What: Returns the explicit `args["palace"]` string if present, otherwise
/// `state.default_palace`. Errors with a helpful message if both are absent.
/// Test: `default_palace_used_when_arg_omitted` and
/// `dispatch_unknown_tool_errors`.
fn resolve_palace<'a>(state: &'a AppState, args: &'a Value, tool: &str) -> Result<String> {
    if let Some(p) = args.get("palace").and_then(|v| v.as_str()) {
        return Ok(p.to_string());
    }
    state
        .default_palace
        .clone()
        .ok_or_else(|| anyhow!("{tool}: missing 'palace' (no --palace default configured)"))
}

/// Dispatch a tool call by name to its real handler.
///
/// Why: Centralises the name → handler mapping; every handler now performs a
/// real read/write against the live `PalaceRegistry` instead of returning a
/// stub.
/// What: Returns `Ok(Value)` on success, `Err` on unknown tool / bad args /
/// underlying failure.
/// Test: `dispatch_palace_create_persists`, `dispatch_remember_then_recall`,
/// `dispatch_kg_assert_then_query`, `dispatch_unknown_tool_errors`.
pub async fn dispatch_tool(state: &AppState, name: &str, args: Value) -> Result<Value> {
    match name {
        "memory_remember" => {
            let palace = resolve_palace(state, &args, "memory_remember")?;
            let palace = palace.as_str();
            let raw_text = args
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_remember: missing 'text'"))?
                .to_string();
            // Issue #220: blocklist gate — silently drop content matching
            // known low-value auto-capture patterns (e.g. `Tool use: Bash`,
            // `Claude Code session ended: …`). Logged at debug so operators
            // can audit when investigating missing writes.
            if blocklist_gate(&raw_text) {
                tracing::debug!(
                    palace = %palace,
                    "content gate: skipped (blocked pattern)",
                );
                return Ok(json!({
                    "palace": palace,
                    "status": "skipped",
                    "reason": "content gate: skipped (blocked pattern)",
                }));
            }
            // Issue #215: content gate — drop very short standalone content
            // unless the caller supplied a `context` wrapper. When skipped,
            // return a success envelope with an explanatory status so the
            // caller can see the write was a no-op without having to parse
            // a custom error shape.
            let ctx = args.get("context").and_then(|v| v.as_str());
            let text = match content_gate(&raw_text, ctx) {
                Some(t) => t,
                None => {
                    return Ok(json!({
                        "palace": palace,
                        "status": "skipped",
                        "reason": "content gate: skipped (short prompt, no context)",
                    }));
                }
            };
            let room = parse_room(args.get("room").and_then(|v| v.as_str()));
            let mut tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            // Submission-logging Part B: attach `creator:*` attribution so
            // every MCP-origin drawer carries the writer identity (client
            // = `trusty-memory-mcp`, source = `mcp`, version + cwd of the
            // MCP server process). Issue #202: also project a bare-UUID
            // session tag (when present in the caller's tags) into the
            // reserved `creator:session=<first-8>` slot so the activity
            // panel can surface it without inspecting every tag.
            if let Some(session_tag) = session_tag_from_tags(&tags) {
                tags.push(session_tag);
            }
            CreatorInfo::new_self(MCP_CLIENT_NAME, CreatorSource::Mcp).merge_into(&mut tags);

            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

            let handle = open_palace_handle(state, palace)?;
            // Issue #230: serialise the dedup-check + write sequence
            // per-palace so two concurrent identical writes can't both
            // pass the gate. The lock is scoped to the palace id — writes
            // to different palaces still run in parallel. The guard is
            // held until the end of this match arm so the post-write
            // emits / KG extraction also see a consistent view; the redb
            // write inside `remember_with_options` is the only operation
            // that strictly needs the lock, but holding it longer is
            // cheap and keeps the activity-log ordering coherent.
            let write_lock = state.palace_write_lock(palace);
            let _write_guard = write_lock.lock().await;
            // Issue #220: rolling dedup window — skip when a near-duplicate
            // landed in the same palace within the last 5 minutes. The
            // `force=true` operator override bypasses the gate so
            // intentional re-writes are not silently dropped.
            if !force && dedup_gate(&handle, &text) {
                tracing::debug!(
                    palace = %palace,
                    "content gate: skipped (duplicate within window)",
                );
                return Ok(json!({
                    "palace": palace,
                    "status": "skipped",
                    "reason": "content gate: skipped (duplicate within window)",
                }));
            }
            let opts = mcp_remember_opts(force);
            // Snapshot the content preview *before* moving `text` into
            // `remember_with_options` so the activity feed shows what was
            // stored (matches the HTTP path's behaviour).
            let preview = crate::service::drawer_content_preview(&text);
            // Issue #97: keep originals so the auto-KG extractor sees the
            // same content / tags that landed in the drawer.
            // `remember_with_options` consumes them, so clone before the call.
            let content_for_kg = text.clone();
            let tags_for_kg = tags.clone();
            let room_label_for_kg = room_label(&room);
            let drawer_id = handle
                .remember_with_options(text, room, tags, 0.5, opts)
                .await
                .context("PalaceHandle::remember_with_options")?;
            // Issue #156: opt-in BM25 lexical lane. Fire-and-forget so the
            // redb write returns immediately; daemon errors are logged but
            // never block the MCP response.
            bm25_index_fire_and_forget(state, palace, drawer_id, &content_for_kg);
            // Issue #96: emit a DrawerAdded so the activity feed shows
            // MCP-origin writes with `source = Mcp`.
            let palace_name = lookup_palace_name(state, palace);
            let drawer_count = handle.drawers.read().len();
            state.emit(DaemonEvent::DrawerAdded {
                palace_id: palace.to_string(),
                palace_name,
                drawer_count,
                timestamp: chrono::Utc::now(),
                content_preview: preview,
                source: ActivitySource::Mcp,
            });
            state.emit(crate::service::MemoryService::new(state.clone()).aggregate_status_event());
            // Issue #97: best-effort auto-extraction. Failures never fail
            // the write — the drawer is already on disk.
            auto_extract_and_assert(
                &handle,
                drawer_id,
                &content_for_kg,
                &tags_for_kg,
                room_label_for_kg.as_deref(),
            )
            .await;
            Ok(json!({
                "drawer_id": drawer_id.to_string(),
                "palace": palace,
                "status": "stored",
            }))
        }
        "memory_note" => {
            // Issue #61: curated short-fact shortcut. Bypasses the token
            // threshold (so "User prefers snake_case" is accepted) but still
            // applies noise-pattern rejects so the tool can't be used to
            // smuggle in auto-capture garbage. Pinned `DrawerType::UserFact`
            // and `importance = 1.0` so the entry surfaces in L1 essentials.
            let palace = resolve_palace(state, &args, "memory_note")?;
            let palace = palace.as_str();
            let raw_content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_note: missing 'content'"))?
                .to_string();
            // Issue #220: blocklist gate — silently drop content matching
            // known low-value auto-capture patterns. Same filter as
            // `memory_remember` so the gate is uniform across the write
            // surface.
            if blocklist_gate(&raw_content) {
                tracing::debug!(
                    palace = %palace,
                    "content gate: skipped (blocked pattern)",
                );
                return Ok(json!({
                    "palace": palace,
                    "status": "skipped",
                    "reason": "content gate: skipped (blocked pattern)",
                }));
            }
            // Issue #215: same content gate as `memory_remember`. A `context`
            // arg can be passed to wrap a one-word answer; otherwise short
            // standalone content is silently dropped with an explanatory
            // status envelope.
            let ctx = args.get("context").and_then(|v| v.as_str());
            let content = match content_gate(&raw_content, ctx) {
                Some(c) => c,
                None => {
                    return Ok(json!({
                        "palace": palace,
                        "status": "skipped",
                        "reason": "content gate: skipped (short prompt, no context)",
                    }));
                }
            };
            let mut tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            // Submission-logging Part B: same attribution as memory_remember.
            // Issue #202: project a bare-UUID session tag (when present)
            // into the reserved `creator:session=<first-8>` slot.
            if let Some(session_tag) = session_tag_from_tags(&tags) {
                tags.push(session_tag);
            }
            CreatorInfo::new_self(MCP_CLIENT_NAME, CreatorSource::Mcp).merge_into(&mut tags);
            let handle = open_palace_handle(state, palace)?;
            // Issue #230: per-palace write lock — same rationale as
            // `memory_remember`. Held across the dedup gate and the
            // `remember_with_options` write so two concurrent identical
            // notes can't both pass the gate.
            let write_lock = state.palace_write_lock(palace);
            let _write_guard = write_lock.lock().await;
            // Issue #220: rolling dedup window — same gate as
            // `memory_remember`. `memory_note` has no `force` arg, so the
            // gate is unconditional: curated short-fact writes that happen
            // to duplicate an existing recent note are still skipped.
            if dedup_gate(&handle, &content) {
                tracing::debug!(
                    palace = %palace,
                    "content gate: skipped (duplicate within window)",
                );
                return Ok(json!({
                    "palace": palace,
                    "status": "skipped",
                    "reason": "content gate: skipped (duplicate within window)",
                }));
            }
            // Issue #97: mirror memory_remember — keep originals so the KG
            // extractor sees the same content / tags that landed in the
            // drawer. `remember_with_options` consumes them, so clone before.
            let content_for_kg = content.clone();
            let tags_for_kg = tags.clone();
            // note() preset skips the token threshold; we keep the default
            // filter for noise patterns. No MCP-stricter min_tokens override
            // is needed because `enforce_min_tokens = false`.
            let preview = crate::service::drawer_content_preview(&content);
            let drawer_id = handle
                .remember_with_options(
                    content,
                    RoomType::General,
                    tags,
                    1.0,
                    RememberOptions::note(),
                )
                .await
                .context("PalaceHandle::remember_with_options (note)")?;
            // Issue #156: opt-in BM25 lexical lane. Fire-and-forget — note
            // writes never block on the daemon round-trip.
            bm25_index_fire_and_forget(state, palace, drawer_id, &content_for_kg);
            // Issue #96: emit a DrawerAdded so the activity feed sees notes.
            let palace_name = lookup_palace_name(state, palace);
            let drawer_count = handle.drawers.read().len();
            state.emit(DaemonEvent::DrawerAdded {
                palace_id: palace.to_string(),
                palace_name,
                drawer_count,
                timestamp: chrono::Utc::now(),
                content_preview: preview,
                source: ActivitySource::Mcp,
            });
            state.emit(crate::service::MemoryService::new(state.clone()).aggregate_status_event());
            // Issue #97: best-effort auto-extraction (same hook as
            // memory_remember). `memory_note` is pinned to the General room.
            auto_extract_and_assert(
                &handle,
                drawer_id,
                &content_for_kg,
                &tags_for_kg,
                Some("General"),
            )
            .await;
            Ok(json!({
                "drawer_id": drawer_id.to_string(),
                "palace": palace,
                "status": "stored",
                "drawer_type": "UserFact",
            }))
        }
        "memory_recall" => {
            let palace = resolve_palace(state, &args, "memory_recall")?;
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_recall: missing 'query'"))?;
            let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

            let handle = open_palace_handle(state, &palace)?;
            let embedder = state.embedder().await?;
            // Issue #156: when the BM25 lane is enabled, run it in parallel
            // with the vector recall and RRF-fuse the two ranked lists.
            // When the daemon is unavailable or the env var is unset, the
            // helper returns `None` and we return the vector-only results
            // verbatim — zero behavioural change for existing deployments.
            let vector_fut = recall(&handle, embedder.as_ref(), query, top_k);
            let bm25_fut = bm25_search_optional(state, &palace, query, top_k);
            let (vector_res, bm25_res) = tokio::join!(vector_fut, bm25_fut);
            let mut results = vector_res.context("recall")?;
            if let Some(bm25_hits) = bm25_res {
                fuse_bm25_into_recall(&mut results, &bm25_hits, top_k);
            }
            Ok(serialize_recall(&palace, query, results))
        }
        "memory_recall_deep" => {
            let palace = resolve_palace(state, &args, "memory_recall_deep")?;
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_recall_deep: missing 'query'"))?;
            let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

            let handle = open_palace_handle(state, &palace)?;
            let embedder = state.embedder().await?;
            let results = recall_deep(&handle, embedder.as_ref(), query, top_k)
                .await
                .context("recall_deep")?;
            Ok(serialize_recall(&palace, query, results))
        }
        "palace_create" => {
            let palace_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("palace_create: missing 'name'"))?;
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let palace = Palace {
                id: PalaceId::new(palace_name),
                name: palace_name.to_string(),
                description,
                created_at: chrono::Utc::now(),
                data_dir: state.data_root.join(palace_name),
            };
            let _handle = state
                .registry
                .create_palace(&state.data_root, palace)
                .context("create_palace")?;
            // Issue #96: emit so MCP-driven palace creation lands in the
            // dashboard activity feed alongside HTTP-origin creates.
            state.emit(DaemonEvent::PalaceCreated {
                id: palace_name.to_string(),
                name: palace_name.to_string(),
                source: ActivitySource::Mcp,
            });
            // Issue #60: auto-seed the KG with temporal metadata so every
            // new palace has at least `created_at` + `bootstrapped_at`
            // triples anchored to the palace name. We deliberately do NOT
            // pass a project_path here — that requires an explicit user
            // decision (which directory belongs to this palace?). Failures
            // are non-fatal: the palace was already created, and the user
            // can re-run `kg_bootstrap` manually if needed.
            let bootstrap_summary =
                match crate::bootstrap::bootstrap_palace(state, palace_name, None).await {
                    Ok(r) => Some(serde_json::json!({
                        "triples_asserted": r.triples_asserted,
                        "project_subject": r.project_subject,
                    })),
                    Err(e) => {
                        tracing::warn!(
                            palace = %palace_name,
                            "auto-bootstrap on palace_create failed: {e:#}",
                        );
                        None
                    }
                };
            Ok(json!({
                "palace_id": palace_name,
                "status": "created",
                "bootstrap": bootstrap_summary,
            }))
        }
        "palace_list" => {
            let root = state.data_root.clone();
            let palaces = tokio::task::spawn_blocking(move || {
                trusty_common::memory_core::PalaceRegistry::list_palaces(&root)
            })
            .await
            .context("join list_palaces")??;
            let ids: Vec<String> = palaces.iter().map(|p| p.id.as_str().to_string()).collect();
            Ok(json!({"palaces": ids}))
        }
        "palace_delete" => {
            // Issue #180: full palace teardown. The HTTP layer is the
            // canonical implementation; we just delegate to the same
            // `MemoryService::delete_palace` method to keep behaviour
            // (and the conflict / not-found / 204 split) identical
            // across surfaces. ServiceError variants are folded into
            // anyhow here so the MCP wire shape matches every other
            // tool's error contract.
            let palace_id = args
                .get("palace_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("palace_delete: missing 'palace_id'"))?
                .to_string();
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            use crate::service::{MemoryService, ServiceError};
            let svc = MemoryService::new(state.clone());
            match svc.delete_palace(&palace_id, force).await {
                Ok(()) => Ok(json!({"deleted": palace_id})),
                Err(ServiceError::NotFound(_)) => Err(anyhow!("Palace not found: {palace_id}")),
                Err(ServiceError::Conflict(msg)) => Err(anyhow!(msg)),
                Err(e) => Err(anyhow!("palace_delete: {e}")),
            }
        }
        "palace_update" => {
            // Issue #180 follow-up: rename a palace's display name. The HTTP
            // layer is the canonical implementation; we delegate to the
            // same `MemoryService::update_palace_name` so the
            // load-mutate-save-emit chain stays consistent across surfaces.
            // The MCP wire shape is the minimal acknowledgement payload —
            // callers needing the enriched palace info should use
            // `palace_info` (or the HTTP endpoint, which returns the full
            // shape).
            let palace_id = args
                .get("palace_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("palace_update: missing 'palace_id'"))?
                .to_string();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("palace_update: missing 'name'"))?
                .to_string();
            use crate::service::MemoryService;
            let svc = MemoryService::new(state.clone());
            match svc.update_palace_name(&palace_id, &name).await {
                Ok(_info) => Ok(json!({"updated": palace_id, "name": name.trim()})),
                Err(e) => Err(anyhow!("palace_update: {e}")),
            }
        }
        "kg_assert" => {
            let palace = resolve_palace(state, &args, "kg_assert")?;
            let palace = palace.as_str();
            let subject = args
                .get("subject")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("kg_assert: missing 'subject'"))?
                .to_string();
            let predicate = args
                .get("predicate")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("kg_assert: missing 'predicate'"))?
                .to_string();
            let object = args
                .get("object")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("kg_assert: missing 'object'"))?
                .to_string();
            let confidence = args
                .get("confidence")
                .and_then(|v| v.as_f64())
                .map(|c| (c as f32).clamp(0.0, 1.0))
                .unwrap_or(1.0);
            let provenance = args
                .get("provenance")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let handle = open_palace_handle(state, palace)?;
            let triple = Triple {
                subject,
                predicate,
                object,
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence,
                provenance,
            };
            let is_hot = crate::prompt_facts::is_hot_predicate(&triple.predicate);
            handle.kg.assert(triple).await.context("kg.assert")?;
            // Rebuild the prompt cache if this assertion touched a hot
            // predicate; otherwise the cache stays valid and we skip the
            // gather/format pass. Failures are logged but non-fatal — the
            // write succeeded, the cache is only a denormalisation.
            if is_hot {
                if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
                    tracing::warn!("rebuild_prompt_cache after kg_assert failed: {e:#}");
                }
            }
            Ok(json!({"status": "asserted"}))
        }
        "add_alias" => {
            let short = args
                .get("short")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("add_alias: missing 'short'"))?
                .to_string();
            let full = args
                .get("full")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("add_alias: missing 'full'"))?
                .to_string();
            let extra = args
                .get("extra")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // `add_alias` is bound to the default palace when configured;
            // otherwise it lands in whatever palace the caller names. This
            // mirrors `resolve_palace`'s rule but without the helpful error
            // — aliases are typically project-scoped via `--palace`.
            let palace = resolve_palace(state, &args, "add_alias")?;
            let handle = open_palace_handle(state, &palace)?;
            // Compose the object: "<full>" or "<full> (<extra>)".
            let object = match extra.as_deref() {
                Some(e) if !e.is_empty() => format!("{full} ({e})"),
                _ => full.clone(),
            };
            let triple = Triple {
                subject: short.clone(),
                predicate: "is_alias_for".to_string(),
                object,
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: Some("add_alias".to_string()),
            };
            handle
                .kg
                .assert(triple)
                .await
                .context("kg.assert (alias)")?;
            if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
                tracing::warn!("rebuild_prompt_cache after add_alias failed: {e:#}");
            }
            Ok(json!({"asserted": true, "short": short, "full": full}))
        }
        "list_prompt_facts" => {
            let triples = crate::prompt_facts::gather_hot_triples(state).await?;
            let payload: Vec<Value> = triples
                .into_iter()
                .map(|(subject, predicate, object)| {
                    json!({"subject": subject, "predicate": predicate, "object": object})
                })
                .collect();
            Ok(json!({"facts": payload}))
        }
        "remove_prompt_fact" => {
            let subject = args
                .get("subject")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("remove_prompt_fact: missing 'subject'"))?
                .to_string();
            let predicate = args
                .get("predicate")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("remove_prompt_fact: missing 'predicate'"))?
                .to_string();

            // The prompt-fact surface spans every palace, so try retracting
            // across all of them and report `true` if any palace closed an
            // active interval. This matches `list_prompt_facts`' scope so
            // round-tripping list→remove never silently no-ops because the
            // caller didn't name the right palace.
            let mut closed_total: usize = 0;
            for palace_id in state.registry.list() {
                if let Some(handle) = state.registry.get(&palace_id) {
                    match handle.kg.retract(&subject, &predicate).await {
                        Ok(n) => closed_total += n,
                        Err(e) => tracing::warn!(
                            palace = %palace_id.as_str(),
                            "retract failed: {e:#}",
                        ),
                    }
                }
            }
            if closed_total > 0 {
                if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
                    tracing::warn!("rebuild_prompt_cache after remove_prompt_fact failed: {e:#}");
                }
                Ok(json!({"removed": true, "closed": closed_total}))
            } else {
                Ok(json!({"removed": false, "reason": "not found"}))
            }
        }
        "kg_query" => {
            let palace = resolve_palace(state, &args, "kg_query")?;
            let subject = args
                .get("subject")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("kg_query: missing 'subject'"))?;
            let handle = open_palace_handle(state, &palace)?;
            let triples = handle
                .kg
                .query_active(subject)
                .await
                .context("kg.query_active")?;
            let payload: Vec<Value> = triples
                .iter()
                .map(|t| {
                    json!({
                        "subject": t.subject,
                        "predicate": t.predicate,
                        "object": t.object,
                        "valid_from": t.valid_from.to_rfc3339(),
                        "valid_to": t.valid_to.as_ref().map(|d| d.to_rfc3339()),
                        "confidence": t.confidence,
                        "provenance": t.provenance,
                    })
                })
                .collect();
            // Issue #60: surface a hint when the requested subject has no
            // active triples so the model knows `kg_bootstrap` and
            // `kg_assert` exist. Empty payload is the only signal we have
            // at the per-subject query layer; that's the user-visible
            // "nothing here" case the hint is for.
            let mut response = json!({"subject": subject, "triples": payload});
            if crate::bootstrap::is_kg_empty_for_subject(&triples) {
                response["hint"] = Value::String(crate::bootstrap::KG_EMPTY_HINT.to_string());
            }
            Ok(response)
        }
        "memory_list" => {
            let palace = resolve_palace(state, &args, "memory_list")?;
            let handle = open_palace_handle(state, &palace)?;
            let room = args
                .get("room")
                .and_then(|v| v.as_str())
                .map(|s| parse_room(Some(s)));
            let tag = args
                .get("tag")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let drawers = handle.list_drawers(room, tag, limit);
            let payload: Vec<Value> = drawers
                .iter()
                .map(|d| {
                    json!({
                        "drawer_id": d.id.to_string(),
                        "content": d.content,
                        "importance": d.importance,
                        "tags": d.tags,
                        "created_at": d.created_at.to_rfc3339(),
                        "drawer_type": d.drawer_type.as_str(),
                        "expires_at": d.expires_at.map(|t| t.to_rfc3339()),
                    })
                })
                .collect();
            Ok(json!({"palace": palace, "drawers": payload}))
        }
        "memory_forget" => {
            let palace = resolve_palace(state, &args, "memory_forget")?;
            let drawer_id_str = args
                .get("drawer_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_forget: missing 'drawer_id'"))?;
            let drawer_id = Uuid::parse_str(drawer_id_str)
                .map_err(|e| anyhow!("memory_forget: invalid drawer_id UUID: {e}"))?;
            let handle = open_palace_handle(state, &palace)?;
            handle.forget(drawer_id).await.context("forget")?;
            // Issue #96: emit so MCP-driven deletes are visible in the feed.
            let drawer_count = handle.drawers.read().len();
            state.emit(DaemonEvent::DrawerDeleted {
                palace_id: palace.clone(),
                drawer_count,
                source: ActivitySource::Mcp,
            });
            state.emit(crate::service::MemoryService::new(state.clone()).aggregate_status_event());
            Ok(json!({"status": "deleted", "drawer_id": drawer_id_str, "palace": palace}))
        }
        "palace_info" => {
            let palace = resolve_palace(state, &args, "palace_info")?;
            let handle = open_palace_handle(state, &palace)?;
            let drawer_count = handle.list_drawers(None, None, usize::MAX).len();
            let data_dir = handle
                .data_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string());
            Ok(json!({
                "id": handle.id.as_str(),
                "name": handle.id.as_str(),
                "drawer_count": drawer_count,
                "data_dir": data_dir,
            }))
        }
        "palace_compact" => {
            let palace = resolve_palace(state, &args, "palace_compact")?;
            let handle = open_palace_handle(state, &palace)?;
            // Use the live drawer table (sourced from SQLite at palace open) as
            // the authoritative valid-id set, then run the vector store's
            // synchronous compaction on a blocking thread.
            let valid_ids: std::collections::HashSet<Uuid> =
                handle.drawers.read().iter().map(|d| d.id).collect();
            let vector_store = handle.vector_store.clone();
            let res = tokio::task::spawn_blocking(move || vector_store.compact_orphans(&valid_ids))
                .await
                .context("join palace_compact")??;
            Ok(json!({
                "palace": palace,
                "total_checked": res.total_checked,
                "orphans_removed": res.orphans_removed,
                "index_size_before": res.index_size_before,
                "index_size_after": res.index_size_after,
            }))
        }
        "kg_gaps" => {
            // Why (issue #53): Surface the cached community-detection output
            // so the model can plan exploration without re-running Louvain.
            // We deliberately do NOT recompute on the read path; the cache is
            // refreshed by the dream cycle.
            // What: Resolves the palace (explicit arg or daemon default),
            // validates it exists by opening the handle, and returns the
            // cached vec (an empty array when the dream cycle has not yet
            // populated it).
            // Test: `dispatch_kg_gaps_returns_cached`.
            let palace = resolve_palace(state, &args, "kg_gaps")?;
            // Ensure the palace exists; this also surfaces a useful error for
            // typos in the palace argument.
            let _handle = open_palace_handle(state, &palace)?;
            let pid = PalaceId::new(&palace);
            let cached = state.registry.get_gaps(&pid).unwrap_or_default();
            let payload: Vec<Value> = cached
                .into_iter()
                .map(|g| {
                    json!({
                        "entities": g.entities,
                        "internal_density": g.internal_density,
                        "external_bridges": g.external_bridges,
                        "suggested_exploration": g.suggested_exploration,
                    })
                })
                .collect();
            Ok(json!({ "palace": palace, "gaps": payload }))
        }
        "memory_recall_all" => {
            let query = args
                .get("q")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_recall_all: missing 'q'"))?;
            let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);

            // List every palace on disk and open a handle for each. Palaces
            // that fail to open are skipped with a warning so a single bad
            // namespace cannot fail the whole fan-out.
            let root = state.data_root.clone();
            let palaces = tokio::task::spawn_blocking(move || {
                trusty_common::memory_core::PalaceRegistry::list_palaces(&root)
            })
            .await
            .context("join list_palaces")??;

            let mut handles = Vec::with_capacity(palaces.len());
            for p in &palaces {
                match state.registry.open_palace(&state.data_root, &p.id) {
                    Ok(h) => handles.push(h),
                    Err(e) => {
                        tracing::warn!(palace = %p.id, "memory_recall_all: open failed: {e:#}")
                    }
                }
            }

            let embedder = state.embedder().await?;
            let erased: std::sync::Arc<
                dyn trusty_common::memory_core::embed::Embedder + Send + Sync,
            > = embedder;
            let results = recall_across_palaces(&handles, &erased, query, top_k, deep)
                .await
                .context("recall_across_palaces")?;

            let payload: Vec<Value> = results
                .iter()
                .map(|r| {
                    json!({
                        "palace_id":  r.palace_id,
                        "drawer_id":  r.result.drawer.id.to_string(),
                        "content":    r.result.drawer.content,
                        "importance": r.result.drawer.importance,
                        "tags":       r.result.drawer.tags,
                        "score":      r.result.score,
                        "layer":      r.result.layer,
                        "drawer_type": r.result.drawer.drawer_type.as_str(),
                    })
                })
                .collect();
            Ok(json!({ "query": query, "results": payload }))
        }
        "get_prompt_context" => {
            // Why (issue #42): the model calls this at the start of each
            // turn to pull aliases/conventions/facts into its working
            // context. A `query` filter lets it scope the result to just
            // the facts that matter for the current task — cheap on the
            // wire and keeps the prompt focused.
            // What: read-locks the cache once, clones the snapshot, then
            // releases the lock so the formatter runs without blocking
            // concurrent readers. When `query` is set we re-format a
            // filtered subset of the raw triples; otherwise we serve the
            // pre-formatted string directly.
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            let cache_snapshot = {
                let guard = state.prompt_context_cache.read().await;
                guard.clone()
            };

            let body = if let Some(q) = query.as_deref() {
                let needle = q.to_lowercase();
                let filtered: Vec<(String, String, String)> = cache_snapshot
                    .triples
                    .into_iter()
                    .filter(|(subject, _predicate, object)| {
                        subject.to_lowercase().contains(&needle)
                            || object.to_lowercase().contains(&needle)
                    })
                    .collect();
                let formatted = crate::prompt_facts::build_prompt_context(&filtered);
                if formatted.is_empty() {
                    "No project context found matching your query.".to_string()
                } else {
                    formatted
                }
            } else if cache_snapshot.formatted.is_empty() {
                "No prompt facts stored yet.".to_string()
            } else {
                cache_snapshot.formatted
            };

            // Return the body as a bare JSON string so the MCP envelope's
            // `content[0].text` carries the formatted Markdown verbatim
            // (ready to paste into the model's working context) without an
            // extra `{"context": "..."}` wrapper that callers would have
            // to strip.
            Ok(Value::String(body))
        }
        "discover_aliases" => {
            // Why (issue #42): Surface project shorthand automatically so the
            // model never has to be told `tga == trusty-git-analytics`. The
            // tool resolves a palace (default or argument), runs the
            // pure-discovery scanner against the requested root (or cwd),
            // checks each candidate against the palace's active KG, and
            // asserts only the new ones. The prompt cache is rebuilt once
            // at the end iff anything was actually asserted.
            // What: returns `{ discovered: [...], already_known: N, new: M }`
            // so callers can audit the delta.
            // Test: `dispatch_discover_aliases_inserts_new_and_dedupes`.
            let palace = resolve_palace(state, &args, "discover_aliases")?;
            let project_root = args
                .get("project_root")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .ok_or_else(|| anyhow!("discover_aliases: no project_root and cwd unavailable"))?;

            let discoveries = crate::discovery::discover_project_aliases(&project_root).await?;

            let handle = open_palace_handle(state, &palace)?;

            let mut already_known = 0usize;
            let mut newly_asserted = 0usize;
            let mut reported: Vec<Value> = Vec::with_capacity(discoveries.len());

            for d in &discoveries {
                // Check active triples for the subject; if any matches the
                // same predicate + object, skip the assertion.
                let active = handle
                    .kg
                    .query_active(&d.short)
                    .await
                    .context("kg.query_active")?;
                let exists = active
                    .iter()
                    .any(|t| t.predicate == "is_alias_for" && t.object == d.full);
                if exists {
                    already_known += 1;
                    continue;
                }

                let triple = Triple {
                    subject: d.short.clone(),
                    predicate: "is_alias_for".to_string(),
                    object: d.full.clone(),
                    valid_from: chrono::Utc::now(),
                    valid_to: None,
                    confidence: 1.0,
                    provenance: Some(format!("discover_aliases:{}", d.source.as_str())),
                };
                handle
                    .kg
                    .assert(triple)
                    .await
                    .context("kg.assert (discover)")?;
                newly_asserted += 1;
                reported.push(json!({
                    "short": d.short,
                    "full": d.full,
                    "source": d.source.as_str(),
                }));
            }

            if newly_asserted > 0 {
                if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
                    tracing::warn!("rebuild_prompt_cache after discover_aliases failed: {e:#}");
                }
            }

            Ok(json!({
                "discovered": reported,
                "already_known": already_known,
                "new": newly_asserted,
                "palace": palace,
            }))
        }
        "kg_bootstrap" => {
            // Issue #60: scan well-known project files and seed the KG with
            // structured triples + temporal metadata. The handler resolves
            // the palace (explicit arg or daemon default) and forwards the
            // optional `project_path` to the bootstrap helper.
            let palace = resolve_palace(state, &args, "kg_bootstrap")?;
            let project_path = args
                .get("project_path")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from);
            let result =
                crate::bootstrap::bootstrap_palace(state, &palace, project_path.as_deref())
                    .await
                    .context("bootstrap_palace")?;
            // Rebuild the prompt cache: bootstrap can land hot predicates
            // (descriptions, language tags) that affect the prompt-facts
            // surface. Cache failures are non-fatal.
            if let Err(e) = crate::prompt_facts::rebuild_prompt_cache(state).await {
                tracing::warn!("rebuild_prompt_cache after kg_bootstrap failed: {e:#}");
            }
            crate::bootstrap::result_to_json(&result)
        }
        "memory_send_message" => {
            // Issue #99: inter-project messaging via palace memories.
            let to_palace = args
                .get("to_palace")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_send_message: missing 'to_palace'"))?
                .to_string();
            let purpose = args
                .get("purpose")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_send_message: missing 'purpose'"))?
                .to_string();
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("memory_send_message: missing 'content'"))?
                .to_string();
            // from_palace defaults to the explicit `from_palace` arg, then
            // the server's --palace default, then the cwd-derived slug.
            let from_palace = if let Some(s) = args.get("from_palace").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(d) = state.default_palace.clone() {
                d
            } else {
                crate::messaging::cwd_palace_slug()
                    .context("memory_send_message: derive from_palace from cwd")?
            };
            let drawer_id = crate::messaging::send_message_to_palace(
                &state.registry,
                &state.data_root,
                &from_palace,
                &to_palace,
                &purpose,
                content,
                CreatorInfo::new_self(MCP_CLIENT_NAME, CreatorSource::Mcp),
            )
            .await
            .context("memory_send_message")?;
            Ok(json!({
                "drawer_id": drawer_id.to_string(),
                "from_palace": from_palace,
                "to_palace": to_palace,
                "purpose": purpose,
                "status": "sent",
            }))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

/// Per-palace BM25 data directory derived from the daemon's data root.
///
/// Why (issue #193): the spawn supervisor must hand the BM25 daemon a
/// data-dir argument so each palace's BM25 snapshot lives next to its
/// other palace data (redb, kg.db, embeddings) — not in a shared scratch
/// directory. The convention is `<data_root>/<palace>/bm25/`, which is
/// stable across daemon restarts and lets operators inspect the snapshot
/// file alongside everything else in the palace.
/// What: appends `<palace>/bm25` to the daemon's `data_root`. Pure path
/// arithmetic — no I/O. The supervisor itself creates the directory
/// before spawning the child.
/// Test: implicitly via the spawn supervisor's integration test.
fn bm25_data_dir_for_palace(state: &AppState, palace: &str) -> std::path::PathBuf {
    state.data_root.join(palace).join("bm25")
}

/// Try to ensure the BM25 daemon for `palace` is running. Returns `true`
/// when the daemon is (now) reachable.
///
/// Why (issue #193): callers want a single yes/no — should I send a BM25
/// op to this palace right now? — without each having to thread the
/// supervisor's `Result` through every code path. When the supervisor
/// returns an error (binary not found, spawn rejected, socket never
/// appeared) we log and return `false` so the caller degrades to
/// vector-only behaviour, exactly as it did before #193 when the daemon
/// simply wasn't running.
/// What: when `state.bm25_supervisor` is `None`, returns `true` (the
/// caller falls back to the original "use the env-var-only socket path"
/// behaviour). When `Some`, delegates to `ensure_running` and treats any
/// error as a soft failure — the supervisor's logs explain why.
/// Test: covered indirectly by the spawn supervisor's unit tests and the
/// `bm25_supervisor_e2e` integration test.
async fn ensure_bm25_running_for_palace(state: &AppState, palace: &str) -> bool {
    let Some(supervisor) = state.bm25_supervisor.as_ref() else {
        // No supervisor — the client (if present) connects to whatever
        // socket happens to be live. This matches pre-#193 behaviour.
        return true;
    };
    let data_dir = bm25_data_dir_for_palace(state, palace);
    match supervisor.ensure_running(palace, &data_dir).await {
        Ok(_socket) => true,
        Err(e) => {
            tracing::warn!(
                palace = %palace,
                "bm25 supervisor could not start daemon (degrading to vector-only): {e:#}"
            );
            false
        }
    }
}

/// Fire-and-forget BM25 indexing after a drawer write (issue #156).
///
/// Why: `memory_remember` / `memory_note` must return as fast as the redb
/// write completes. Routing the BM25 daemon call through `tokio::spawn`
/// keeps the daemon's RTT off the response path entirely, and bails out
/// cheaply when the env-var-gated client is absent.
/// What: clones the client `Arc` and the inputs, spawns a detached task that
/// (a) ensures the daemon is running via the spawn supervisor (issue #193),
/// and (b) calls `client.index()`. Daemon errors are logged at `warn!` and
/// dropped — the drawer is durable in redb regardless of whether the BM25
/// lane saw it.
/// Test: behaviour is exercised end-to-end by the integration tests in
/// `trusty-bm25-daemon/tests/`; the no-op branch is covered by
/// `bm25_client_disabled_by_default`.
fn bm25_index_fire_and_forget(state: &AppState, palace: &str, drawer_id: Uuid, content: &str) {
    let Some(client) = state.bm25_client.clone() else {
        return;
    };
    let supervisor = state.bm25_supervisor.clone();
    let data_dir = bm25_data_dir_for_palace(state, palace);
    let palace = palace.to_string();
    let drawer_id_s = drawer_id.to_string();
    let content = content.to_string();
    tokio::spawn(async move {
        // Issue #193: try to start the daemon before the first index call.
        // If the supervisor returns an error we silently skip the index op;
        // the daemon will be retried on the next fire-and-forget call.
        if let Some(sup) = supervisor.as_ref() {
            if let Err(e) = sup.ensure_running(&palace, &data_dir).await {
                tracing::warn!(
                    palace = %palace,
                    "bm25 supervisor failed to start daemon for index (non-fatal): {e:#}"
                );
                return;
            }
        }
        if let Err(e) = client.index(&drawer_id_s, &content).await {
            tracing::warn!(
                palace = %palace,
                drawer_id = %drawer_id_s,
                "bm25 daemon index failed (non-fatal): {e:#}"
            );
        }
    });
}

/// Optional BM25 search lane used by `memory_recall` (issue #156).
///
/// Why: lets the recall handler join a BM25 future with the vector future
/// without sprinkling `if state.bm25_client.is_some()` checks across the
/// call site. Returning `Option<Vec<_>>` makes the "daemon unavailable"
/// branch explicit at the consumer.
/// What: returns `None` when the env-var-gated client is absent OR when the
/// daemon errors (treated as a graceful degradation — the caller falls back
/// to vector-only results). Otherwise ensures the daemon is running via the
/// spawn supervisor (issue #193), then returns the BM25 hits the daemon
/// served. `top_k` is forwarded verbatim.
/// Test: integration coverage via the daemon's `tests/bm25_daemon.rs`; the
/// `None` path is covered by `bm25_client_disabled_by_default`.
async fn bm25_search_optional(
    state: &AppState,
    palace: &str,
    query: &str,
    top_k: usize,
) -> Option<Vec<trusty_common::bm25_client::BM25Hit>> {
    let client = state.bm25_client.as_ref()?;
    // Issue #193: spawn the daemon if it isn't already running. On error
    // we fall through to vector-only behaviour exactly as we did before
    // #193 when the operator forgot to start the daemon manually.
    if !ensure_bm25_running_for_palace(state, palace).await {
        return None;
    }
    match client.search(query, top_k).await {
        Ok(hits) => Some(hits),
        Err(e) => {
            tracing::warn!(
                palace = %palace,
                "bm25 daemon search failed (falling back to vector-only): {e:#}"
            );
            None
        }
    }
}

/// Reciprocal Rank Fusion (RRF) blender for BM25 hits + vector recall hits.
///
/// Why: BM25 wins on identifier-heavy queries ("cargo test", "PalaceHandle"),
/// the vector lane wins on conceptual queries. RRF is the canonical fusion
/// because it is parameter-light, rank-only, and robust to scale differences
/// between the two lanes.
/// What: walks the BM25 ranked list once and adds `1 / (k + rank)` to the
/// matching drawer's vector score (RRF with `k = 60`, the IR-literature
/// default). Drawers that appear in BM25 but not in the vector list are
/// appended with `layer = 4` so the caller knows they came from the lexical
/// lane (L0/L1/L2/L3 are reserved). The combined list is re-sorted by score
/// desc and truncated to `top_k`.
/// Test: integration coverage via the daemon's `tests/bm25_daemon.rs` plus
/// downstream RRF behaviour observed end-to-end.
fn fuse_bm25_into_recall(
    results: &mut Vec<trusty_common::memory_core::retrieval::RecallResult>,
    bm25_hits: &[trusty_common::bm25_client::BM25Hit],
    top_k: usize,
) {
    /// RRF damping constant (Cormack et al. 2009). 60 is the literature
    /// default and what trusty-search uses in its hybrid pipeline.
    const RRF_K: f32 = 60.0;
    if bm25_hits.is_empty() {
        return;
    }
    // Boost existing vector hits whose drawer id appears in BM25.
    for (rank, hit) in bm25_hits.iter().enumerate() {
        let bonus = 1.0 / (RRF_K + rank as f32 + 1.0);
        if let Some(existing) = results
            .iter_mut()
            .find(|r| r.drawer.id.to_string() == hit.doc_id)
        {
            existing.score += bonus;
        }
        // BM25-only hits (those that don't appear in the vector list) are
        // intentionally NOT appended here — without hydrating the drawer
        // payload (content, tags, importance) from disk we cannot construct
        // a `RecallResult`, and the per-call disk walk would defeat the
        // whole purpose of the daemon. The hits that already appear in the
        // vector list still benefit from the RRF boost, which is enough to
        // improve identifier-heavy queries.
    }
    // Re-sort by score desc; preserve layer for tie-breaking (lower layer
    // wins because L0/L1 are pinned identity/essentials).
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.layer.cmp(&b.layer))
    });
    results.truncate(top_k);
}

/// Serialize `recall` results into a JSON shape the MCP client can render.
fn serialize_recall(
    palace: &str,
    query: &str,
    results: Vec<trusty_common::memory_core::retrieval::RecallResult>,
) -> Value {
    let payload: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "drawer_id": r.drawer.id.to_string(),
                "content":   r.drawer.content,
                "score":     r.score,
                "layer":     r.layer,
                "tags":      r.drawer.tags,
                "importance": r.drawer.importance,
                "drawer_type": r.drawer.drawer_type.as_str(),
            })
        })
        .collect();
    json!({
        "palace": palace,
        "query": query,
        "results": payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;

    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        AppState::new(root)
    }

    /// Why: Issue #26 — when the server is started with `--palace`, the
    /// `tools/list` schema must drop `palace` from the `required` array for
    /// every tool that accepts it, so MCP clients know it's optional.
    /// Test: Build the schema both ways and check the required arrays.
    #[test]
    fn tool_definitions_drops_palace_required_when_default_set() {
        let with_default = tool_definitions_with(true);
        let without_default = tool_definitions_with(false);
        for (name, palace_required_when_no_default) in [
            ("memory_remember", true),
            ("memory_recall", true),
            ("memory_recall_deep", true),
            ("memory_list", true),
            ("memory_forget", true),
            ("palace_info", true),
            ("palace_compact", true),
            ("kg_assert", true),
            ("kg_query", true),
        ] {
            for (defs, has_default) in [(&with_default, true), (&without_default, false)] {
                let tools = defs["tools"].as_array().unwrap();
                let tool = tools.iter().find(|t| t["name"] == name).unwrap();
                let required: Vec<&str> = tool["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect();
                let palace_required = required.contains(&"palace");
                let expected = palace_required_when_no_default && !has_default;
                assert_eq!(
                    palace_required, expected,
                    "tool={name} has_default={has_default} required={required:?}"
                );
            }
        }
    }

    #[test]
    fn tool_definitions_lists_all_tools() {
        let defs = tool_definitions();
        let tools = defs
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array");
        assert_eq!(tools.len(), 23);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        for expected in [
            "memory_remember",
            "memory_note",
            "memory_recall",
            "memory_recall_deep",
            "memory_list",
            "memory_forget",
            "palace_create",
            "palace_delete",
            "palace_update",
            "palace_list",
            "palace_info",
            "palace_compact",
            "kg_assert",
            "kg_query",
            "memory_recall_all",
            "kg_gaps",
            "add_alias",
            "list_prompt_facts",
            "remove_prompt_fact",
            "get_prompt_context",
            "discover_aliases",
            "kg_bootstrap",
            "memory_send_message",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    /// Why: Confirm `palace_create` actually persists a palace under the
    /// configured data root and `palace_list` then sees it.
    #[tokio::test]
    async fn dispatch_palace_create_persists() {
        let state = test_state();
        let created = dispatch_tool(&state, "palace_create", json!({"name": "alpha"}))
            .await
            .expect("palace_create");
        assert_eq!(created["palace_id"], "alpha");

        let listed = dispatch_tool(&state, "palace_list", json!({}))
            .await
            .expect("palace_list");
        let ids = listed["palaces"].as_array().expect("palaces array");
        assert!(ids.iter().any(|v| v.as_str() == Some("alpha")));
    }

    /// Why: End-to-end confirmation that a remembered drawer is recallable
    /// through the MCP tool surface using the real embedder + retrieval path.
    #[tokio::test]
    async fn dispatch_remember_then_recall() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "beta"}))
            .await
            .expect("palace_create");

        let remembered = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "beta",
                "text": "Quokkas are the happiest marsupials in Australia by general consensus",
                "room": "General",
                "tags": ["wildlife"],
            }),
        )
        .await
        .expect("memory_remember");
        assert!(remembered["drawer_id"].as_str().is_some());

        let recalled = dispatch_tool(
            &state,
            "memory_recall",
            json!({"palace": "beta", "query": "Quokkas marsupials Australia", "top_k": 5}),
        )
        .await
        .expect("memory_recall");
        let results = recalled["results"].as_array().expect("results");
        assert!(
            results
                .iter()
                .any(|r| r["content"].as_str().unwrap_or("").contains("Quokkas")),
            "expected to recall the Quokkas drawer; got {results:?}"
        );
    }

    /// Why: Issue #97 — `memory_remember` should auto-populate the KG so
    /// every drawer leaves a graph trail. Confirm a freshly remembered
    /// drawer leaves `has-tag`/`in-room`/`mentions` triples (using the
    /// tag-as-subject encoding) in the palace KG.
    /// What: Create a palace, write one drawer with known tags + room +
    /// recognisable pattern content, then read all active triples and
    /// assert the expected auto-extracted shapes show up.
    /// Test: This test.
    #[tokio::test]
    async fn auto_kg_extraction_hooks_into_memory_remember() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "kgauto"}))
            .await
            .expect("palace_create");

        let _ = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "kgauto",
                "text": "Rustc is a compiler for the Rust language; tracks #performance",
                "room": "Backend",
                "tags": ["compiler", "language"],
            }),
        )
        .await
        .expect("memory_remember");

        let handle = open_palace_handle(&state, "kgauto").expect("open palace");
        let triples = handle.kg.list_active(1000, 0).await.expect("list_active");
        let auto: Vec<_> = triples
            .iter()
            .filter(|t| t.provenance.as_deref() == Some(crate::kg_extract::AUTO_PROVENANCE))
            .collect();
        assert!(
            !auto.is_empty(),
            "expected at least one auto-extracted triple after memory_remember; got: {triples:?}"
        );
        // Tag/room/topic encoding: each metadata category becomes its own
        // subject so multiple tags coexist under the KG's "one active
        // triple per (s, p)" invariant. Confirm both tags survive.
        assert!(
            auto.iter()
                .any(|t| t.subject == "tag:compiler" && t.predicate == "tags"),
            "expected tag:compiler edge in auto subset: {auto:?}"
        );
        assert!(
            auto.iter()
                .any(|t| t.subject == "tag:language" && t.predicate == "tags"),
            "expected tag:language edge in auto subset: {auto:?}"
        );
        assert!(
            auto.iter()
                .any(|t| t.subject == "room:Backend" && t.predicate == "contains"),
            "expected room:Backend edge in auto subset: {auto:?}"
        );
        assert!(
            auto.iter().any(|t| t.predicate == "mentioned-in"),
            "expected at least one #hashtag mention triple in auto subset: {auto:?}"
        );
    }

    /// Why: Issue #97 — failures inside the auto-extraction pass must
    /// never fail the parent write. We can't easily inject a failure into
    /// the live `KnowledgeGraph::assert`, so this test exercises the
    /// documented contract by verifying the parent `memory_remember`
    /// succeeds even when the content produces zero auto-extracted triples
    /// (the closest natural no-op to "extraction failed").
    /// What: Remember a drawer with empty tags + minimal patternless
    /// content; confirm `memory_remember` returns a drawer id and no
    /// auto-extracted triples are emitted (the only built-in auto triples
    /// would have come from tags/room/hashtags/patterns).
    /// Test: This test.
    #[tokio::test]
    async fn auto_kg_extraction_no_op_does_not_fail_remember() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "kgnoop"}))
            .await
            .expect("palace_create");

        let res = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "kgnoop",
                // 8+ tokens to clear MCP_MIN_TOKENS; no tags, no room, no
                // hashtags, no pattern triggers.
                "text": "The quick brown fox jumped over the lazy dog repeatedly",
            }),
        )
        .await
        .expect("memory_remember should succeed even when extraction yields nothing");
        assert!(res["drawer_id"].as_str().is_some());
    }

    /// Why: Confirm `kg_assert` writes a triple and `kg_query` returns it
    /// through the MCP tool surface.
    #[tokio::test]
    async fn dispatch_kg_assert_then_query() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "gamma"}))
            .await
            .expect("palace_create");

        let _ = dispatch_tool(
            &state,
            "kg_assert",
            json!({
                "palace": "gamma",
                "subject": "alice",
                "predicate": "works_at",
                "object": "Acme",
                "confidence": 0.9,
                "provenance": "test",
            }),
        )
        .await
        .expect("kg_assert");

        let queried = dispatch_tool(
            &state,
            "kg_query",
            json!({"palace": "gamma", "subject": "alice"}),
        )
        .await
        .expect("kg_query");
        let triples = queried["triples"].as_array().expect("triples array");
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0]["object"], "Acme");
        assert_eq!(triples[0]["predicate"], "works_at");
    }

    /// Why: Issue #53 — verify the MCP `kg_gaps` tool returns whatever was
    /// last cached on the registry. Two cases: empty cache returns an empty
    /// array, and a seeded cache returns the cached entries verbatim.
    /// What: Creates a palace, dispatches `kg_gaps` (expects empty), then
    /// directly seeds the registry cache via `set_gaps` and dispatches again
    /// to confirm the entry round-trips through serialization.
    /// Test: This test itself.
    #[tokio::test]
    async fn dispatch_kg_gaps_returns_cached() {
        use trusty_common::memory_core::community::KnowledgeGap;

        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "delta"}))
            .await
            .expect("palace_create");

        // Empty cache → empty gaps list (not an error).
        let initial = dispatch_tool(&state, "kg_gaps", json!({"palace": "delta"}))
            .await
            .expect("kg_gaps empty");
        let gaps = initial["gaps"].as_array().expect("gaps array");
        assert_eq!(gaps.len(), 0);

        // Seed the cache and re-dispatch.
        state.registry.set_gaps(
            PalaceId::new("delta"),
            vec![KnowledgeGap {
                entities: vec!["x".to_string(), "y".to_string()],
                internal_density: 0.05,
                external_bridges: 0,
                suggested_exploration: "Explore connections between x and y".to_string(),
            }],
        );
        let seeded = dispatch_tool(&state, "kg_gaps", json!({"palace": "delta"}))
            .await
            .expect("kg_gaps seeded");
        let gaps = seeded["gaps"].as_array().expect("gaps array");
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0]["entities"][0], "x");
        assert_eq!(gaps[0]["external_bridges"], 0);
        assert!(gaps[0]["suggested_exploration"]
            .as_str()
            .unwrap()
            .contains("x"));
    }

    /// Why: Issue #42 — `add_alias` must (a) assert the triple in the KG,
    /// (b) cause `list_prompt_facts` to surface it, (c) refresh the prompt
    /// cache so `prompts/get` returns it, and (d) be reversible via
    /// `remove_prompt_fact`.
    #[tokio::test]
    async fn add_alias_round_trip_through_prompt_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root).with_default_palace(Some("ctx".to_string()));

        // Pre-create the default palace.
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "ctx"}))
            .await
            .expect("palace_create");

        // (a) add_alias asserts the triple.
        let added = dispatch_tool(
            &state,
            "add_alias",
            json!({"short": "tga", "full": "trusty-git-analytics"}),
        )
        .await
        .expect("add_alias");
        assert_eq!(added["asserted"], true);
        assert_eq!(added["short"], "tga");

        // (b) list_prompt_facts surfaces it.
        let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
            .await
            .expect("list_prompt_facts");
        let facts = listed["facts"].as_array().expect("facts array");
        assert!(
            facts.iter().any(|f| f["subject"] == "tga"
                && f["predicate"] == "is_alias_for"
                && f["object"] == "trusty-git-analytics"),
            "expected tga alias in facts; got {facts:?}"
        );

        // (c) prompt cache has been refreshed with the formatted block.
        {
            let guard = state.prompt_context_cache.read().await;
            assert!(
                guard.formatted.contains("tga → trusty-git-analytics"),
                "prompt cache should contain alias; got: {}",
                guard.formatted
            );
        }

        // add_alias with `extra` appends parenthetical context.
        let _ = dispatch_tool(
            &state,
            "add_alias",
            json!({"short": "tm", "full": "trusty-memory", "extra": "the MCP frontend"}),
        )
        .await
        .expect("add_alias with extra");
        {
            let guard = state.prompt_context_cache.read().await;
            assert!(
                guard
                    .formatted
                    .contains("tm → trusty-memory (the MCP frontend)"),
                "alias with extra not formatted; got: {}",
                guard.formatted
            );
        }

        // (d) remove_prompt_fact retracts and refreshes.
        let removed = dispatch_tool(
            &state,
            "remove_prompt_fact",
            json!({"subject": "tga", "predicate": "is_alias_for"}),
        )
        .await
        .expect("remove_prompt_fact");
        assert_eq!(removed["removed"], true);
        {
            let guard = state.prompt_context_cache.read().await;
            assert!(
                !guard.formatted.contains("tga → trusty-git-analytics"),
                "retracted alias still in cache: {}",
                guard.formatted
            );
            assert!(
                guard.formatted.contains("tm → trusty-memory"),
                "non-retracted alias missing from cache: {}",
                guard.formatted
            );
        }

        // Removing a non-existent fact reports not found.
        let missing = dispatch_tool(
            &state,
            "remove_prompt_fact",
            json!({"subject": "nope", "predicate": "is_alias_for"}),
        )
        .await
        .expect("remove_prompt_fact missing");
        assert_eq!(missing["removed"], false);
    }

    /// Why (issue #42): `get_prompt_context` is the per-message replacement
    /// for the deprecated `prompts/get` flow. It must (a) return a hint when
    /// the cache is empty, (b) return the formatted block when populated,
    /// and (c) filter by `query` against subject/object case-insensitively.
    #[tokio::test]
    async fn get_prompt_context_serves_cache_and_filters() {
        let state = test_state();

        // (a) empty cache -> "No prompt facts stored yet."
        let resp = dispatch_tool(&state, "get_prompt_context", json!({}))
            .await
            .expect("get_prompt_context empty");
        assert_eq!(resp.as_str().unwrap(), "No prompt facts stored yet.");

        // Populate the cache by hand with a known triple set.
        {
            let mut guard = state.prompt_context_cache.write().await;
            let triples = vec![
                (
                    "tga".to_string(),
                    "is_alias_for".to_string(),
                    "trusty-git-analytics".to_string(),
                ),
                (
                    "tm".to_string(),
                    "is_alias_for".to_string(),
                    "trusty-memory".to_string(),
                ),
                (
                    "fact-1".to_string(),
                    "is_fact".to_string(),
                    "MSRV is 1.88".to_string(),
                ),
            ];
            let formatted = crate::prompt_facts::build_prompt_context(&triples);
            *guard = crate::prompt_facts::PromptFactsCache { triples, formatted };
        }

        // (b) unfiltered -> serves the full formatted block.
        let resp = dispatch_tool(&state, "get_prompt_context", json!({}))
            .await
            .expect("get_prompt_context populated");
        let text = resp.as_str().expect("string body");
        assert!(text.contains("tga → trusty-git-analytics"));
        assert!(text.contains("tm → trusty-memory"));
        assert!(text.contains("MSRV is 1.88"));

        // (c) filtered to "tga" -> only the matching alias.
        let resp = dispatch_tool(&state, "get_prompt_context", json!({"query": "tga"}))
            .await
            .expect("get_prompt_context filtered");
        let text = resp.as_str().expect("string body");
        assert!(text.contains("tga → trusty-git-analytics"));
        assert!(!text.contains("tm → trusty-memory"));
        assert!(!text.contains("MSRV is 1.88"));

        // Case-insensitive match on the object side.
        let resp = dispatch_tool(&state, "get_prompt_context", json!({"query": "MEMORY"}))
            .await
            .expect("get_prompt_context case-insensitive");
        let text = resp.as_str().expect("string body");
        assert!(text.contains("tm → trusty-memory"));
        assert!(!text.contains("tga → trusty-git-analytics"));

        // No match -> "No project context found matching your query."
        let resp = dispatch_tool(
            &state,
            "get_prompt_context",
            json!({"query": "zzz-nonexistent"}),
        )
        .await
        .expect("get_prompt_context no-match");
        assert_eq!(
            resp.as_str().unwrap(),
            "No project context found matching your query."
        );

        // Empty/whitespace `query` is treated as no filter.
        let resp = dispatch_tool(&state, "get_prompt_context", json!({"query": "   "}))
            .await
            .expect("get_prompt_context whitespace");
        let text = resp.as_str().expect("string body");
        assert!(text.contains("tga → trusty-git-analytics"));
        assert!(text.contains("tm → trusty-memory"));
    }

    /// Why (issue #42): `discover_aliases` must (a) auto-discover the
    /// canonical workspace shorthand (`tga → trusty-git-analytics`),
    /// (b) assert each discovery as an `is_alias_for` triple, (c) refresh
    /// the prompt cache, and (d) dedupe on a second invocation — the second
    /// call should report zero new and N already_known.
    /// Test: this test itself.
    #[tokio::test]
    async fn dispatch_discover_aliases_inserts_new_and_dedupes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root).with_default_palace(Some("disc".to_string()));
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "disc"}))
            .await
            .expect("palace_create");

        // Use the live workspace root so the discovery actually finds
        // something. CARGO_MANIFEST_DIR points at the crate dir; walk up
        // twice to the workspace root.
        let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .to_path_buf();

        let first = dispatch_tool(
            &state,
            "discover_aliases",
            json!({"project_root": workspace_root.to_string_lossy()}),
        )
        .await
        .expect("discover_aliases first");

        let new_count = first["new"].as_u64().expect("new is u64");
        assert!(new_count > 0, "expected new discoveries on first call");
        let discovered = first["discovered"].as_array().expect("discovered array");
        assert!(
            discovered
                .iter()
                .any(|d| d["short"] == "tga" && d["full"] == "trusty-git-analytics"),
            "expected tga alias in discoveries; got {discovered:?}"
        );

        // The prompt cache must contain the new alias after discovery.
        {
            let guard = state.prompt_context_cache.read().await;
            assert!(
                guard.formatted.contains("tga → trusty-git-analytics"),
                "prompt cache missing tga alias after discover_aliases; got: {}",
                guard.formatted
            );
        }

        // Second invocation should report zero new and at least `new_count`
        // already_known — the same discoveries are now in the KG.
        let second = dispatch_tool(
            &state,
            "discover_aliases",
            json!({"project_root": workspace_root.to_string_lossy()}),
        )
        .await
        .expect("discover_aliases second");
        assert_eq!(second["new"].as_u64(), Some(0), "expected 0 new on rerun");
        let already_known = second["already_known"].as_u64().expect("already_known");
        assert!(
            already_known >= new_count,
            "expected already_known >= {new_count}, got {already_known}"
        );
    }

    /// Why (issue #60): `palace_create` must auto-seed temporal metadata so
    /// every new palace has at least `created_at` + `bootstrapped_at`
    /// triples — without auto-bootstrap, brand-new palaces had a zero-triple
    /// KG and no signal to users that they were supposed to seed it.
    /// Test: create a palace, then query the seeded subject (the palace id)
    /// and confirm the temporal triples are present.
    #[tokio::test]
    async fn palace_create_auto_seeds_temporal_metadata() {
        let state = test_state();
        let created = dispatch_tool(&state, "palace_create", json!({"name": "auto"}))
            .await
            .expect("palace_create");
        assert_eq!(created["palace_id"], "auto");
        // bootstrap summary is present on success
        let summary = &created["bootstrap"];
        assert!(summary.is_object(), "expected bootstrap summary object");
        assert!(summary["triples_asserted"].as_u64().unwrap_or(0) >= 2);

        let queried = dispatch_tool(
            &state,
            "kg_query",
            json!({"palace": "auto", "subject": "auto"}),
        )
        .await
        .expect("kg_query");
        let triples = queried["triples"].as_array().expect("triples");
        let predicates: Vec<&str> = triples
            .iter()
            .filter_map(|t| t["predicate"].as_str())
            .collect();
        assert!(
            predicates.contains(&"created_at"),
            "expected created_at after palace_create; got {predicates:?}",
        );
        assert!(
            predicates.contains(&"bootstrapped_at"),
            "expected bootstrapped_at after palace_create; got {predicates:?}",
        );
        // Hint must NOT appear when triples are present.
        assert!(
            queried.get("hint").is_none(),
            "hint should be absent when triples exist"
        );
    }

    /// Why (issue #60): `kg_query` against a subject with no triples must
    /// surface a `hint` field pointing the user at `kg_bootstrap` /
    /// `kg_assert`. Without the hint, brand-new palaces returned empty
    /// arrays with no breadcrumb back to the seeding tools.
    #[tokio::test]
    async fn kg_query_emits_hint_when_palace_empty() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "hinted"}))
            .await
            .expect("palace_create");
        // Query a subject that auto-bootstrap did NOT seed.
        let queried = dispatch_tool(
            &state,
            "kg_query",
            json!({"palace": "hinted", "subject": "unrelated-subject"}),
        )
        .await
        .expect("kg_query");
        assert_eq!(queried["triples"].as_array().unwrap().len(), 0);
        let hint = queried["hint"].as_str().expect("hint field present");
        assert!(hint.contains("kg_bootstrap"));
        assert!(hint.contains("kg_assert"));
    }

    /// Why (issue #60): `kg_bootstrap` against the live workspace root must
    /// extract Cargo facts (language, version, rust-version) and the git
    /// origin URL, then make them queryable through `kg_query`.
    #[tokio::test]
    async fn kg_bootstrap_seeds_workspace_facts() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "ws"}))
            .await
            .expect("palace_create");

        let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .to_path_buf();

        let result = dispatch_tool(
            &state,
            "kg_bootstrap",
            json!({"palace": "ws", "project_path": workspace_root.to_string_lossy()}),
        )
        .await
        .expect("kg_bootstrap");
        assert!(result["triples_asserted"].as_u64().unwrap() > 0);
        let subject = result["project_subject"]
            .as_str()
            .expect("project_subject")
            .to_string();

        // Verify the workspace facts are queryable.
        let queried = dispatch_tool(
            &state,
            "kg_query",
            json!({"palace": "ws", "subject": subject}),
        )
        .await
        .expect("kg_query");
        let triples = queried["triples"].as_array().expect("triples");
        let predicates: Vec<&str> = triples
            .iter()
            .filter_map(|t| t["predicate"].as_str())
            .collect();
        // Either Rust language (single-crate manifest) or workspace member
        // triples must appear; the trusty-tools root manifest is a workspace
        // so we expect has_workspace_member.
        assert!(
            predicates.contains(&"has_workspace_member") || predicates.contains(&"has_language"),
            "expected workspace/language fact; got {predicates:?}",
        );
        // source_repo from .git/config.
        assert!(
            predicates.contains(&"source_repo"),
            "expected source_repo from .git/config; got {predicates:?}",
        );
        // Temporal metadata always.
        assert!(predicates.contains(&"bootstrapped_at"));
    }

    // -----------------------------------------------------------------
    // Issue #215 — content gate for short prompts
    // -----------------------------------------------------------------

    /// Why: short single-word content with no `context` must be skipped so
    /// the palace doesn't accumulate orphan "yes"/"ok" fragments.
    /// What: passes "yes" through the gate and asserts `None`.
    /// Test: itself.
    #[test]
    fn content_gate_blocks_short_no_context() {
        assert_eq!(content_gate("yes", None), None);
        assert_eq!(content_gate("ok", None), None);
        assert_eq!(
            content_gate("  no thanks  ", None),
            None,
            "2 words still < 4"
        );
        assert_eq!(
            content_gate("one two three", None),
            None,
            "3 words still < 4"
        );
    }

    /// Why: when the caller wraps a short answer with `context`, the gate
    /// must keep the content but prepend the context with a `---` separator
    /// so the stored memory has standalone value.
    /// What: passes "yes" + context, asserts the combined shape.
    /// Test: itself.
    #[test]
    fn content_gate_wraps_short_with_context() {
        let combined = content_gate(
            "yes",
            Some("Do you want to enable auto-bootstrap on new palaces?"),
        )
        .expect("context should unlock the gate");
        assert_eq!(
            combined,
            "Do you want to enable auto-bootstrap on new palaces?\n\n---\n\nyes",
        );
        // Even content that would otherwise pass the threshold is wrapped
        // when context is supplied — the caller is explicit.
        let combined = content_gate(
            "the quick brown fox jumps over the lazy dog",
            Some("Famous typing pangram"),
        )
        .expect("long content + context still combines");
        assert!(combined.starts_with("Famous typing pangram"));
        assert!(combined.contains("\n\n---\n\n"));
        assert!(combined.ends_with("the quick brown fox jumps over the lazy dog"));
    }

    /// Why: content that meets the threshold should pass through untouched
    /// when no context is supplied — the gate must not rewrite or reformat
    /// passing content.
    /// What: passes a 5-word string through and asserts the output equals
    /// the input verbatim.
    /// Test: itself.
    #[test]
    fn content_gate_keeps_long() {
        let body = "User prefers snake_case for python";
        let kept = content_gate(body, None).expect(">= 4 words passes");
        assert_eq!(kept, body, "passing content must round-trip verbatim");
        // Exactly four words is the boundary — it must pass.
        let boundary = "one two three four";
        assert_eq!(content_gate(boundary, None).as_deref(), Some(boundary));
    }

    /// Why: an empty or whitespace-only `context` argument must be treated
    /// the same as `None` so callers can't accidentally smuggle short
    /// content through by passing `""`.
    /// What: passes blank context with short content and asserts the gate
    /// still skips the write.
    /// Test: itself.
    #[test]
    fn content_gate_blank_context_treated_as_none() {
        assert_eq!(content_gate("yes", Some("")), None);
        assert_eq!(content_gate("yes", Some("   ")), None);
        assert_eq!(content_gate("yes", Some("\n\t")), None);
    }

    /// Why: the dispatch path must return a structured "skipped" envelope
    /// without writing to the store when the gate fires on `memory_remember`.
    /// What: dispatch with single-word `text` and no `context`; assert the
    /// response carries `status = "skipped"` and that no drawer landed.
    /// Test: itself.
    #[tokio::test]
    async fn dispatch_remember_skips_short_no_context() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "gate"}))
            .await
            .expect("palace_create");

        let res = dispatch_tool(
            &state,
            "memory_remember",
            json!({"palace": "gate", "text": "yes"}),
        )
        .await
        .expect("memory_remember (short)");
        assert_eq!(res["status"], "skipped");
        assert!(res["reason"]
            .as_str()
            .unwrap_or("")
            .contains("content gate"));
        // No drawer was written.
        let listed = dispatch_tool(
            &state,
            "memory_list",
            json!({"palace": "gate", "limit": 10}),
        )
        .await
        .expect("memory_list");
        let drawers = listed["drawers"].as_array().expect("drawers array");
        assert!(
            drawers.is_empty(),
            "no drawer should be written; got {drawers:?}"
        );
    }

    /// Why: confirm the `context` argument unlocks a short content write —
    /// the resulting drawer must carry the combined `context + content`
    /// body so downstream recall sees the wrapping.
    /// What: dispatch with one-word text plus a context arg, then list and
    /// assert the stored content begins with the context and ends with the
    /// original short body.
    /// Test: itself.
    #[tokio::test]
    async fn dispatch_remember_with_context_writes_combined() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "ctxgate"}))
            .await
            .expect("palace_create");

        let res = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "ctxgate",
                "text": "yes",
                "context": "Do you want to enable auto-bootstrap on new palaces?",
                "force": true,
            }),
        )
        .await
        .expect("memory_remember (with context)");
        assert_eq!(res["status"], "stored");

        let listed = dispatch_tool(
            &state,
            "memory_list",
            json!({"palace": "ctxgate", "limit": 10}),
        )
        .await
        .expect("memory_list");
        let drawers = listed["drawers"].as_array().expect("drawers array");
        assert_eq!(drawers.len(), 1);
        let body = drawers[0]["content"].as_str().expect("content");
        assert!(body.starts_with("Do you want to enable auto-bootstrap"));
        assert!(body.contains("\n\n---\n\n"));
        assert!(body.ends_with("yes"));
    }

    /// Why: `memory_note` must respect the same content gate as
    /// `memory_remember` so the short-prompt protection is uniform across
    /// the write surface.
    /// What: dispatch `memory_note` with a one-word content and no context;
    /// assert it returns a skipped envelope and no drawer is written.
    /// Test: itself.
    #[tokio::test]
    async fn dispatch_note_skips_short_no_context() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "noteg"}))
            .await
            .expect("palace_create");

        let res = dispatch_tool(
            &state,
            "memory_note",
            json!({"palace": "noteg", "content": "ok"}),
        )
        .await
        .expect("memory_note (short)");
        assert_eq!(res["status"], "skipped");
        let listed = dispatch_tool(
            &state,
            "memory_list",
            json!({"palace": "noteg", "limit": 10}),
        )
        .await
        .expect("memory_list");
        assert!(listed["drawers"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_errors() {
        let state = test_state();
        let err = dispatch_tool(&state, "does_not_exist", json!({}))
            .await
            .expect_err("should error");
        assert!(err.to_string().contains("unknown tool"));
    }

    // -----------------------------------------------------------------
    // Issue #220 — blocklist pattern + rolling dedup window
    // -----------------------------------------------------------------

    /// Why: the blocklist gate must reject Claude Code tool-use captures
    /// (`Tool use: Bash`, `Tool use: Edit File: …`) because those entries
    /// have no standalone semantic value.
    /// What: passes the literal prefix and a realistic example through
    /// the gate and asserts `true` (blocked).
    /// Test: itself.
    #[test]
    fn blocklist_gate_blocks_tool_use() {
        assert!(blocklist_gate("Tool use: Bash"));
        assert!(blocklist_gate(
            "Tool use: Edit File: /Users/me/Projects/foo/bar.rs"
        ));
        // Leading whitespace should not let it through.
        assert!(blocklist_gate("   Tool use: Read"));
    }

    /// Why: session-lifecycle events are auto-emitted by Claude Code and
    /// should not pollute the palace.
    /// What: passes the prefix through the gate and asserts `true`.
    /// Test: itself.
    #[test]
    fn blocklist_gate_blocks_session_ended() {
        assert!(blocklist_gate(
            "Claude Code session ended: 1d2c3b4a-0000-0000-0000-000000000000"
        ));
        assert!(blocklist_gate("Claude Code session started"));
    }

    /// Why: normal user content (with no blocklist substring) must pass
    /// the gate untouched so the regular content gate (issue #215) gets
    /// to make the next decision.
    /// What: passes normal prose / facts through and asserts `false`.
    /// Test: itself.
    #[test]
    fn blocklist_gate_passes_normal_content() {
        assert!(!blocklist_gate("User prefers snake_case for python"));
        assert!(!blocklist_gate(
            "Quokkas are the happiest marsupials in Australia"
        ));
        assert!(!blocklist_gate("Note: refactor the dispatcher next sprint"));
        // Substring-only — a tool-use mention inside legitimate prose is
        // still blocked. This is intentional: the prefix is rare enough
        // outside the auto-capture path that the false-positive rate is
        // acceptable, and a future regex upgrade can tighten it.
        assert!(blocklist_gate("I used Tool use: Bash here"));
    }

    /// Why: the dedup gate must reject a fresh write whose content is a
    /// near-duplicate (Jaro-Winkler > 0.92) of a drawer landed inside the
    /// rolling window. Without this gate, bursty auto-captures inflate
    /// the palace with no recall benefit (issue #220).
    /// What: creates a palace, writes one drawer through the MCP path,
    /// then runs the gate directly against a string that differs by one
    /// trailing word — Jaro-Winkler should score that above 0.92 and the
    /// gate should return `true`.
    /// Test: itself.
    #[tokio::test]
    async fn dedup_skips_near_duplicate() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "dedup1"}))
            .await
            .expect("palace_create");

        // Land the seed drawer through the real write path so its
        // `created_at` is `Utc::now()` and falls inside the dedup window.
        let _ = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "dedup1",
                "text": "The quick brown fox jumped over the lazy dog repeatedly today",
            }),
        )
        .await
        .expect("memory_remember seed");

        let handle = open_palace_handle(&state, "dedup1").expect("open handle");
        // Near-duplicate: same prefix, trailing word replaced. Jaro-Winkler
        // weights the shared prefix heavily so this should clear the 0.92
        // bar comfortably.
        assert!(
            dedup_gate(
                &handle,
                "The quick brown fox jumped over the lazy dog repeatedly yesterday"
            ),
            "near-duplicate should be detected"
        );
        // Exact match also blocks.
        assert!(
            dedup_gate(
                &handle,
                "The quick brown fox jumped over the lazy dog repeatedly today"
            ),
            "exact match should be detected"
        );
    }

    /// Why: a write whose content is genuinely different from every drawer
    /// in the window must pass the dedup gate so the palace can grow.
    /// What: writes one seed drawer, then runs the gate against an
    /// unrelated string. Asserts `false`.
    /// Test: itself.
    #[tokio::test]
    async fn dedup_allows_different_content() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "dedup2"}))
            .await
            .expect("palace_create");

        let _ = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "dedup2",
                "text": "Quokkas are the happiest marsupials in Australia by general consensus",
            }),
        )
        .await
        .expect("memory_remember seed");

        let handle = open_palace_handle(&state, "dedup2").expect("open handle");
        // Completely different content — far below 0.92.
        assert!(
            !dedup_gate(
                &handle,
                "Rust is a systems programming language focused on safety and concurrency"
            ),
            "unrelated content should pass the dedup gate"
        );
        // Empty/whitespace content is also a pass — the content gate
        // handles the empty case upstream.
        assert!(!dedup_gate(&handle, "   "));
    }

    /// Why (issue #230): the dedup gate previously had a TOCTOU race —
    /// two concurrent `memory_remember` calls with identical content
    /// both saw the empty pre-write snapshot, both passed the gate, and
    /// both wrote duplicate drawers. The per-palace write mutex on
    /// `AppState` now serialises the gate-then-write sequence so the
    /// second writer observes the first writer's drawer in
    /// `list_drawers` and bails. This test would have failed before the
    /// fix and passes after.
    /// What: spawns two `tokio` tasks that race to write the same long
    /// content into a fresh palace, joins both, then asserts that
    /// `memory_list` returns exactly one drawer (the loser's envelope
    /// carries `status = "skipped"` with a `duplicate within window`
    /// reason).
    /// Test: itself — fail-then-pass on this commit.
    #[tokio::test]
    async fn dedup_gate_blocks_concurrent_duplicate_writes() {
        let state = std::sync::Arc::new(test_state());
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "dedup_race"}))
            .await
            .expect("palace_create");

        // Long enough to clear the 8-token MCP filter; identical content
        // in both racers so the dedup gate is the only thing keeping
        // them from both landing.
        let text =
            "Concurrent identical writes must collapse to a single drawer under the dedup gate";

        let s1 = state.clone();
        let t1 = tokio::spawn(async move {
            dispatch_tool(
                &s1,
                "memory_remember",
                json!({"palace": "dedup_race", "text": text}),
            )
            .await
        });
        let s2 = state.clone();
        let t2 = tokio::spawn(async move {
            dispatch_tool(
                &s2,
                "memory_remember",
                json!({"palace": "dedup_race", "text": text}),
            )
            .await
        });
        let r1 = t1.await.expect("join t1").expect("dispatch t1");
        let r2 = t2.await.expect("join t2").expect("dispatch t2");

        // Exactly one of the two should be `stored`; the other should be
        // `skipped` with the documented duplicate-window reason.
        let statuses = [
            r1["status"].as_str().unwrap_or(""),
            r2["status"].as_str().unwrap_or(""),
        ];
        let stored = statuses.iter().filter(|s| **s == "stored").count();
        let skipped = statuses.iter().filter(|s| **s == "skipped").count();
        assert_eq!(
            stored, 1,
            "exactly one concurrent write should be stored; got responses {r1:?} {r2:?}"
        );
        assert_eq!(
            skipped, 1,
            "exactly one concurrent write should be skipped; got responses {r1:?} {r2:?}"
        );
        let skipped_reason = if r1["status"] == "skipped" {
            r1["reason"].as_str().unwrap_or("")
        } else {
            r2["reason"].as_str().unwrap_or("")
        };
        assert!(
            skipped_reason.contains("duplicate within window"),
            "skipped envelope should cite dedup reason; got {skipped_reason:?}"
        );

        // Belt-and-braces: confirm the palace contains exactly one drawer.
        let listed = dispatch_tool(
            &state,
            "memory_list",
            json!({"palace": "dedup_race", "limit": 10}),
        )
        .await
        .expect("memory_list");
        let drawers = listed["drawers"].as_array().expect("drawers array");
        assert_eq!(
            drawers.len(),
            1,
            "only one drawer should be persisted after concurrent identical writes; got {drawers:?}"
        );
    }

    /// Why: end-to-end confirmation that the blocklist short-circuits the
    /// MCP `memory_remember` dispatch — no drawer is written, the
    /// response envelope carries the documented `status = "skipped"` and
    /// reason. Mirrors the issue-215 short-prompt test.
    /// What: dispatch a `Tool use:` payload through `memory_remember`,
    /// then `memory_list` and assert no drawer landed.
    /// Test: itself.
    #[tokio::test]
    async fn dispatch_remember_blocks_blocklist_pattern() {
        let state = test_state();
        let _ = dispatch_tool(&state, "palace_create", json!({"name": "blk"}))
            .await
            .expect("palace_create");

        let res = dispatch_tool(
            &state,
            "memory_remember",
            json!({"palace": "blk", "text": "Tool use: Bash"}),
        )
        .await
        .expect("memory_remember (blocked)");
        assert_eq!(res["status"], "skipped");
        assert!(
            res["reason"]
                .as_str()
                .unwrap_or("")
                .contains("blocked pattern"),
            "reason should mention blocked pattern; got {res:?}"
        );

        let listed = dispatch_tool(&state, "memory_list", json!({"palace": "blk", "limit": 10}))
            .await
            .expect("memory_list");
        let drawers = listed["drawers"].as_array().expect("drawers array");
        assert!(drawers.is_empty(), "no drawer should be written");
    }
}
