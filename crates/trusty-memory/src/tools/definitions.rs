//! MCP `tools/list` schema + server marker for trusty-memory.
//!
//! Why: Concentrates the public tool contract (the `tools/list` payload) in
//! one place so the MCP schema stays auditable and in sync with the handlers.
//! What: Defines `MemoryMcpServer` and the `tool_definitions{,_with}` schema
//! builders moved out of the former monolithic `tools.rs` (issue #607).
//! Test: `tool_definitions_lists_all_tools`,
//! `tool_definitions_drops_palace_required_when_default_set` in `tools::tests`.

use serde_json::{json, Value};

use super::task_definitions::task_tool_definitions;

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
    // Issue #664: add_alias and discover_aliases both call resolve_palace() but
    // previously omitted `palace` from their schemas, making them uncallable
    // without a server-side default. Now follow the memory_remember pattern.
    let add_alias_required: Vec<&str> = if has_default {
        vec!["short", "full"]
    } else {
        vec!["palace", "short", "full"]
    };
    let discover_aliases_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    // spec-001 chat-session tools: `palace` is optional only when a server
    // default is configured, matching the convention used by every other
    // palace-scoped tool above.
    let chat_session_palace_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    let chat_session_get_required: Vec<&str> = if has_default {
        vec!["session_id"]
    } else {
        vec!["palace", "session_id"]
    };
    let chat_session_add_turn_required: Vec<&str> = if has_default {
        vec!["session_id", "role", "content"]
    } else {
        vec!["palace", "session_id", "role", "content"]
    };
    let dream_consolidate_room_required: Vec<&str> =
        if has_default { vec![] } else { vec!["palace"] };
    // chat_turn_append requires palace + session_id + prompt + response.
    let chat_turn_append_required: Vec<&str> = if has_default {
        vec!["session_id", "prompt", "response"]
    } else {
        vec!["palace", "session_id", "prompt", "response"]
    };
    let chat_session_delete_required: Vec<&str> = if has_default {
        vec!["session_id"]
    } else {
        vec!["palace", "session_id"]
    };

    let mut result = json!({
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
                        "description": {"type": "string"},
                        "cwd":         {"type": "string", "description": "Optional caller working directory used for palace-name enforcement. Pass the project root (or any path inside it) so the pin file at `.trusty-tools/trusty-memory.yaml` is honoured. When omitted, the daemon's own cwd is used (rarely meaningful for remote calls)."},
                        "force":       {"type": "boolean", "description": "Bypass project-slug validation so an application can create a palace under an arbitrary slug (spec-001: chat-session manager, one palace per app/tenant). Defaults to false.", "default": false}
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
                        "palace": {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                        "short": {"type": "string", "description": "Short name / alias (subject)"},
                        "full":  {"type": "string", "description": "Full / canonical name (object)"},
                        "extra": {"type": "string", "description": "Optional extra context appended to the full name"}
                    },
                    "required": add_alias_required,
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
                        "palace": {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                        "project_root": {"type": "string", "description": "Optional filesystem path to scan. Defaults to the process cwd."}
                    },
                    "required": discover_aliases_required,
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
            },
            {
                "name": "upgrade",
                "description": "Check for or install a new version of trusty-memory (issue #537). With check=true (or without confirm): report current vs. available version only — NEVER installs. With confirm=true: install via `cargo install trusty-memory --locked`, run a binary health gate, then restart the daemon under launchd (or print a restart hint when not supervised). The MCP response is returned BEFORE the daemon exits so the client sees the result before reconnecting.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "check":   {"type": "boolean", "description": "Report current and available versions only. No install. Default: true when confirm is absent.", "default": true},
                        "confirm": {"type": "boolean", "description": "Set to true to install the new version. NEVER set automatically — the operator must explicitly pass confirm=true.", "default": false}
                    },
                    "required": []
                }
            },
            {
                "name": "chat_session_create",
                "description": "Create a new chat session in a palace (spec-001 chat-session manager). Returns the session id, its creation timestamp, and the message count (0 for a fresh session). Pass an optional session_id to use a caller-chosen id (idempotent — an existing session is returned unchanged); pass an optional title to name a server-generated session. Sessions are stored in the palace's dedicated redb chat store, NOT the generic memory drawer surface.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string", "description": "Palace slug (optional if server started with --palace)"},
                        "session_id": {"type": "string", "description": "Optional caller-supplied session id; a UUID is generated when omitted."},
                        "title":      {"type": "string", "description": "Optional session name (applied only when session_id is omitted)."}
                    },
                    "required": chat_session_palace_required,
                }
            },
            {
                "name": "chat_session_add_turn",
                "description": "Append a message (prompt or response) to a chat session's history. Creates the session if it does not yet exist. Returns the new message_count and updated_at. Bypasses the memory_remember signal/noise + dedup gates so sequential conversational turns persist verbatim.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string"},
                        "session_id": {"type": "string"},
                        "role":       {"type": "string", "enum": ["user", "assistant", "system"]},
                        "content":    {"type": "string"}
                    },
                    "required": chat_session_add_turn_required,
                }
            },
            {
                "name": "chat_session_get",
                "description": "Retrieve a full chat session: metadata plus every turn in chronological order. Errors if the session id is unknown.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string"},
                        "session_id": {"type": "string"}
                    },
                    "required": chat_session_get_required,
                }
            },
            {
                "name": "chat_session_list",
                "description": "List chat sessions in a palace as paginated metadata (id, title, timestamps, message_count) ordered most-recently-updated first. Does not include message bodies. Returns { sessions, total_count }.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"},
                        "limit":  {"type": "integer", "default": 50},
                        "offset": {"type": "integer", "default": 0}
                    },
                    "required": chat_session_palace_required,
                }
            },
            {
                "name": "dream_consolidate_room",
                "description": "Trigger LLM-driven semantic consolidation for one room (or all rooms) of a palace, on demand and synchronously (spec-001). Consolidates facts older than max_age_days into canonical summaries, then evicts the superseded originals so history shrinks. Task drawers are always skipped. No-op (zero counts) when no inference backend (OpenRouter key / local model) is configured. Returns { summary_facts_created, facts_evicted }.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":       {"type": "string"},
                        "room":         {"type": "string", "description": "Room to scope to (e.g. Backend, Planning, or a custom name). Omit or null to consolidate all rooms."},
                        "max_age_days": {"type": "integer", "default": 7, "description": "Only consolidate facts older than this many days."}
                    },
                    "required": dream_consolidate_room_required,
                }
            },
            {
                "name": "palace_dream",
                "description": "On-demand LLM-driven consolidation for a palace (issue #1721). Alias for dream_consolidate_room with the same parameters; use this name when following the palace_* convention. Triggers a scoped dream/consolidation cycle immediately for the named palace, optionally filtered to one room. Task drawers are always skipped. Gracefully returns zero counts when no inference backend is configured. Returns { palace, room, summary_facts_created, facts_evicted }.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":       {"type": "string"},
                        "room":         {"type": "string", "description": "Room to scope to. Omit or null to consolidate all rooms."},
                        "max_age_days": {"type": "integer", "default": 7, "description": "Only consolidate facts older than this many days."}
                    },
                    "required": dream_consolidate_room_required,
                }
            },
            {
                "name": "chat_session_recall",
                "description": "Retrieve a full chat session with all turns in order (alias for chat_session_get, preferred name for agent-facing recall). Errors if the session id is unknown.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string"},
                        "session_id": {"type": "string"}
                    },
                    "required": chat_session_get_required,
                }
            },
            {
                "name": "chat_session_delete",
                "description": "Delete a chat session (and its full history) from a palace. Idempotent: deleting an unknown session id is a no-op, not an error. Returns { deleted: session_id }.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string"},
                        "session_id": {"type": "string"}
                    },
                    "required": chat_session_delete_required,
                }
            },
            {
                "name": "chat_turn_append",
                "description": "Append a prompt/response PAIR to a chat session as two consecutive messages (user role then assistant role). Atomic at the session level — both messages are written together. Creates the session implicitly when it does not exist. Returns { message_count, updated_at }.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string"},
                        "session_id": {"type": "string"},
                        "prompt":     {"type": "string", "description": "User-side message (stored with role=user)."},
                        "response":   {"type": "string", "description": "Assistant-side message (stored with role=assistant)."}
                    },
                    "required": chat_turn_append_required,
                }
            },
            crate::console_metrics::descriptor()
        ]
    });
    // spec-001 Phase 4 (issue #1722): splice task tool schemas.
    // Defined in task_definitions.rs to respect the 500-SLOC cap on this file.
    let tools = result["tools"].as_array_mut().expect("tools is array");
    let metrics = tools.pop().expect("console_metrics sentinel");
    tools.extend(task_tool_definitions(has_default));
    tools.push(metrics);
    result
}
