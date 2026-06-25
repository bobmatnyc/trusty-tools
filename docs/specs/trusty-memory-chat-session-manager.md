# trusty-memory as a Dedicated Chat Session Manager

**Status**: DRAFT / Proposed  
**Spec ID**: `spec-001-chat-session-manager`  
**Date**: 2026-06-25  
**Author**: Claude Code (Design Agent)

---

## Summary

This specification proposes repurposing **trusty-memory as a dedicated chat session manager** for applications that need persistent, LLM-aware conversation storage with automatic history compaction. The design unifies conversation turns, context preservation, and task-aware history management into a single MCP interface, leveraging trusty-memory's existing **redb-backed chat store**, **palace-based isolation**, and **semantic consolidation** infrastructure.

**Goal**: Enable applications to drive trusty-memory as a multi-tenant chat backend: create custom palaces per app, persist prompt/response pairs with session tags, trigger LLM-driven consolidation of older history, and protect task-related drawers from automatic eviction.

---

## Non-Goals

- **Full conversation AI**: trusty-memory does not reason about conversation state or suggest responses.
- **Real-time sync**: No bidirectional sync to external LLM services; this is a local store.
- **Structured dialogue management**: State machines, intent classification, and dialogue acts are out of scope.
- **Generative completions**: Prompt completion or response generation remain the application's responsibility.
- **Scheduled compaction**: Only on-demand or idle-triggered consolidation (existing dream cycle pattern).

---

## Requirements

### 1. Custom Palace per Application (PARTIAL)

**Requirement**: Applications specify a custom palace slug (e.g. `"my-app"`) for isolation. Multiple apps coexist in the same trusty-memory instance without cross-contamination.

**Current State**:
- **✅ Supported**: Palace isolation via redb database per palace + per-palace HNSW vector index (see `crates/trusty-common/src/memory_core/palace.rs`).
- **⚠️ Partial**: `palace_create` endpoint enforces slug validation (issue #88); slugs must match the CWD project slug or be `"personal"` (see `crates/trusty-common/src/memory_core/validation.rs`).
- **❌ Missing**: No programmatic `force` flag to bypass validation for non-project apps; gate is auth-like, not permission-like.
- **❌ Missing**: No palace-level TTL / GC; palaces persist indefinitely once created.

**Contract**: 
```
When an application calls palace_create(slug="acme-app", name="ACME Chat Manager", force=true),
  it receives a new palace with isolation gates:
  - Separate redb database at ~/.local/share/trusty-memory/acme-app/palace.redb
  - Separate HNSW index at ~/.local/share/trusty-memory/acme-app/vectors.hnsw
  - No cross-palace queries (retrieval is scoped to one palace at a time)
  - force=true bypasses slug validation, allowing arbitrary app names
```

**File:Line Evidence**:
- Palace creation: `crates/trusty-memory/src/chat/sessions.rs` (HTTP handlers, calls `create_palace` from core)
- Validation gate: `crates/trusty-common/src/memory_core/validation.rs` (issue #88 reference)
- Palace isolation: `crates/trusty-common/src/memory_core/palace.rs` and `crates/trusty-common/src/memory_core/store/palace_store.rs`

---

### 2. Prompt/Response Pair Storage (PARTIAL / MCP EXPOSURE MISSING)

**Requirement**: Store conversation turns (prompt + response) as immutable, timestamped records accessible via MCP. Query turns by session, within a time range, or by semantic search.

**Current State**:
- **✅ Supported**: Full redb-backed `ChatSessionStore` exists (see `crates/trusty-common/src/memory_core/store/chat_sessions/`).
  - Stores `ChatSession` (metadata + `Vec<ChatMessage>` history) per session UUID.
  - Supports `create_session`, `list_sessions`, `get_session`, `upsert_session`, `delete_session`.
  - Serializes history to postcard in redb blobs; round-trips are tested and working.
- **✅ Supported**: HTTP endpoints in trusty-memory (`crates/trusty-memory/src/chat/handler.rs` + `sessions.rs`).
- **❌ Missing**: **No MCP exposure** of chat-turn API. Only HTTP endpoints available; applications using trusty-memory as MCP server cannot access chat turns.
- **⚠️ Noise gate**: Generic `memory_remember` path has 5-min dedup + token-count gate; hostile to sequential turns.

**Contract**:
```
When an application calls new MCP tool chat_session_create(palace, session_id?, title?):
  it receives { session_id, created_at, message_count }
When it calls chat_session_add_turn(palace, session_id, role, content):
  the turn appends to the session's history (creates session if missing)
When it calls chat_session_get(palace, session_id):
  it receives full session metadata + all turns in order
When it calls chat_session_list(palace, limit, offset):
  it receives paginated metadata for all sessions (no full history)
```

**File:Line Evidence**:
- Chat store types: `crates/trusty-common/src/memory_core/store/chat_sessions/types.rs` (ChatSession, ChatMessage, ChatSessionMeta)
- Store implementation: `crates/trusty-common/src/memory_core/store/chat_sessions/store.rs` (line 1–80, redb backend confirmed)
- HTTP integration: `crates/trusty-memory/src/chat/handler.rs` + `sessions.rs`
- Re-export: `crates/trusty-common/src/memory_core/store/mod.rs` (line 23, ChatSessionStore exported)

---

### 3. LLM-Driven Auto-Compaction (PARTIAL / ROOM-FILTERED TRIGGER MISSING)

**Requirement**: Periodically consolidate older conversation history using an LLM (haiku-4-5) to produce a summary, then evict original turns. Compaction is per-room or per-tag to avoid mixing unrelated conversations.

**Current State**:
- **✅ Supported**: `semantic_consolidation_pass` exists in `crates/trusty-common/src/memory_core/dream/` (see `cycle.rs`, `helpers.rs`).
  - Uses OpenRouter haiku-4-5 to condense related facts via semantic similarity clustering.
  - Runs after ~300s idle, per palace, with configurable thresholds.
  - Produces synthetic `SessionEvent` facts with consolidated text.
  - Only triggers when `OPENROUTER_API_KEY` is set and inference available.
- **⚠️ Partial**: Currently operates on the entire palace (all rooms, all drawer types).
  - No room-level or tag-level scoping; consolidation is global-per-palace.
  - No on-demand trigger (only time-based idle).
  - No way to protect certain history from consolidation.
- **❌ Missing**: No explicit `on_demand_consolidate(palace, room?)` MCP tool.

**Contract**:
```
When dream cycle's idle timer (300s) expires for a palace:
  1. Fetch all facts in the palace (unscoped)
  2. Cluster by semantic similarity
  3. For each cluster, call LLM to produce summary
  4. Store summary as new SessionEvent
  5. Evict original cluster facts (if below importance threshold)
When an app calls new MCP tool dream_consolidate_room(palace, room?):
  1. Same pipeline, but scoped to one room (or all if room=null)
  2. Return summary facts created
```

**File:Line Evidence**:
- Dream module: `crates/trusty-common/src/memory_core/dream/` (multiple files: `cycle.rs`, `helpers.rs`, `config.rs`)
- Consolidation logic: `crates/trusty-common/src/memory_core/dream/semantic_consolidation/` (submodule)
- Trigger: `cycle.rs` idle timeout (search for `300` or `idle_secs`)

---

### 4. Task Tracking via Protected Drawers (MISSING)

**Requirement**: Store task-related memory (goals, milestones, checkpoints) in drawers that **are never evicted or consolidated**. Tasks survive history compaction so the application can re-derive task context across sessions.

**Current State**:
- **✅ Supported**: `DrawerType` enum exists (see `crates/trusty-common/src/memory_core/palace.rs`, line 115).
  - Current types: `UserFact`, `SessionEvent`, `AgentNote`, `Commit`, `Unknown`.
  - Each drawer carries `importance: u8` and optional `expires_at`.
- **⚠️ Partial**: Dream eviction uses importance + age to decide which drawers to remove; `SessionEvent` has an auto-set TTL (~7 days, configurable).
- **❌ Missing**: No `DrawerType::Task` variant.
- **❌ Missing**: No "protected from consolidation/eviction" semantics; all current types are subject to dream processing.

**Contract**:
```
Define new DrawerType::Task with semantics:
  - Never evicted by dream cycle, even if age > threshold
  - Never consolidated into summaries
  - Optional completed_at timestamp; once set, eligible for cleanup
When app calls memory_remember(palace, drawer_type=Task, content, ...):
  drawer is stored and guaranteed to survive all future dream cycles
  until explicitly deleted or marked completed_at
When dream cycle runs:
  it skips all Task drawers and all drawers with completed_at > threshold
```

**File:Line Evidence**:
- Drawer types: `crates/trusty-common/src/memory_core/palace.rs` (line 115–145)
- Dream eviction logic: `crates/trusty-common/src/memory_core/dream/cycle.rs` (search for `prune_old_facts` or `importance`)
- Consolidation skip-list: `crates/trusty-common/src/memory_core/dream/semantic_consolidation/` (review for consolidation gates)

---

## Current-State Capability Map

| Requirement | Status | Evidence | Effort to MVP |
|---|---|---|---|
| Custom palace per app | **PARTIAL** | Palace isolation ✅; validation gate ⚠️; `force` flag ❌ | S (add `force` to palace_create) |
| Prompt/response storage | **PARTIAL** | redb store ✅; HTTP endpoints ✅; **MCP exposure ❌** | S–M (expose HTTP as MCP tools) |
| LLM auto-compaction | **PARTIAL** | Dream consolidation ✅; room-scoped ❌; on-demand ❌ | S (wrap existing consolidation as on-demand MCP tool) |
| Task-protected drawers | **MISSING** | Type enum exists ⚠️; semantics ❌ | M (add type, gate, tests) |

---

## Data Model

### Chat Turn (Existing, Redb-Backed)

```rust
// In trusty-common/src/memory_core/store/chat_sessions/types.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,           // "user", "assistant", "system"
    pub content: String,        // Prompt or response text
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,             // Session UUID
    pub title: Option<String>,  // User-provided session name
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub history: Vec<ChatMessage>,  // Chronological turns
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSessionMeta {
    pub id: String,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,   // For pagination UI
}
```

**Storage**: Stored in per-palace redb table `SESSIONS` (table name from `crates/trusty-common/src/memory_core/store/kg_store.rs`, line 17). History serialized to postcard binary.

---

### Task Drawer (Proposed New Type)

```rust
// Extend DrawerType enum in trusty-common/src/memory_core/palace.rs
pub enum DrawerType {
    UserFact,
    SessionEvent,
    AgentNote,
    Commit,
    Task,          // NEW: protected from eviction/consolidation
    Unknown,
}

// Existing Drawer struct; Task drawers have semantic:
pub struct Drawer {
    pub id: Uuid,
    pub content: String,
    pub drawer_type: DrawerType,
    pub importance: u8,         // 1–255; Task ignores during eviction
    pub expires_at: Option<DateTime<Utc>>,  // ignored for Task
    pub completed_at: Option<DateTime<Utc>>, // NEW: marks Task as done
    pub created_at: DateTime<Utc>,
}
```

**Semantics**:
- Dream cycle skips all `DrawerType::Task` drawers (never consolidates or evicts).
- Once `completed_at` is set, drawer becomes eligible for manual cleanup (not auto-evicted).
- Task drawers survive all history compactions; use them for goals, checkpoints, milestones.

---

## API / MCP Tool Surface

### Proposed MCP Tools (MVP)

#### 1. `chat_session_create`
```json
{
  "name": "chat_session_create",
  "description": "Create a new chat session in a palace",
  "inputSchema": {
    "type": "object",
    "properties": {
      "palace": { "type": "string", "description": "Palace slug (e.g., 'acme-app')" },
      "session_id": { "type": "string", "description": "Optional UUID; generated if omitted" },
      "title": { "type": "string", "description": "Optional session name" }
    },
    "required": ["palace"]
  }
}
```
**Returns**: `{ session_id: string, created_at: DateTime, message_count: 0 }`

#### 2. `chat_session_add_turn`
```json
{
  "name": "chat_session_add_turn",
  "description": "Append a message (prompt or response) to a session",
  "inputSchema": {
    "type": "object",
    "properties": {
      "palace": { "type": "string" },
      "session_id": { "type": "string" },
      "role": { "type": "string", "enum": ["user", "assistant", "system"] },
      "content": { "type": "string" }
    },
    "required": ["palace", "session_id", "role", "content"]
  }
}
```
**Returns**: `{ message_count: usize, updated_at: DateTime }`

#### 3. `chat_session_get`
```json
{
  "name": "chat_session_get",
  "description": "Retrieve full session with all turns",
  "inputSchema": {
    "type": "object",
    "properties": {
      "palace": { "type": "string" },
      "session_id": { "type": "string" }
    },
    "required": ["palace", "session_id"]
  }
}
```
**Returns**: `ChatSession` (full object with history)

#### 4. `chat_session_list`
```json
{
  "name": "chat_session_list",
  "description": "List sessions in a palace (paginated metadata)",
  "inputSchema": {
    "type": "object",
    "properties": {
      "palace": { "type": "string" },
      "limit": { "type": "integer", "default": 50 },
      "offset": { "type": "integer", "default": 0 }
    },
    "required": ["palace"]
  }
}
```
**Returns**: `{ sessions: Vec<ChatSessionMeta>, total_count: usize }`

#### 5. `dream_consolidate_room` (On-Demand)
```json
{
  "name": "dream_consolidate_room",
  "description": "Trigger LLM-driven consolidation for a room in a palace",
  "inputSchema": {
    "type": "object",
    "properties": {
      "palace": { "type": "string" },
      "room": { "type": "string", "description": "Optional room filter; null = all rooms" },
      "max_age_days": { "type": "integer", "default": 7, "description": "Only consolidate facts older than N days" }
    },
    "required": ["palace"]
  }
}
```
**Returns**: `{ summary_facts_created: usize, facts_evicted: usize }`

#### 6. `palace_create` (Enhanced)
```json
{
  "name": "palace_create",
  "description": "Create a new palace for an application",
  "inputSchema": {
    "type": "object",
    "properties": {
      "slug": { "type": "string", "description": "Unique palace identifier (e.g., 'acme-app')" },
      "name": { "type": "string" },
      "description": { "type": "string" },
      "force": { "type": "boolean", "default": false, "description": "If true, bypass slug validation" }
    },
    "required": ["slug", "name"]
  }
}
```
**Returns**: `{ palace_id: string, data_dir: PathBuf, created: bool }`

---

## Compaction Design

### Existing Consolidation Cycle (Reuse)

The trusty-memory daemon already runs a **dream cycle** every ~300 seconds (configurable). Each cycle:

1. **Cluster facts** by semantic similarity (using embeddings + HNSW).
2. **Call LLM** (haiku-4-5 via OpenRouter) on each cluster to produce a summary.
3. **Store summary** as a new `SessionEvent` drawer with high importance.
4. **Evict originals** if they fall below a token/importance threshold and are older than TTL.

### Proposed Enhancement: Room-Scoped On-Demand Trigger

Add MCP tool `dream_consolidate_room` that wraps the same pipeline but:
- **Scopes** to a single room (or all rooms if `room=null`).
- **Skips** all `DrawerType::Task` drawers (never consolidated).
- **Respects** existing importance + TTL gates.
- **Returns** summary count and eviction count so the app can log progress.

**Implementation approach**:
- Extract core consolidation logic into a scoped helper (already mostly there; refactor if needed).
- Wrap as a synchronous MCP tool that calls the helper directly (or queue an async job).
- Add test: verify Task drawers survive consolidation, SessionEvent facts are summarized.

---

## Open Questions

1. **Palace-level TTL / Garbage Collection**: Should there be an expiration on palaces themselves, or are they permanent? Apps might create many short-lived palaces (per user, per project, etc.). Recommend: permanent for MVP; add palace deletion tool post-MVP.

2. **Session Tagging**: The spec suggests `session:<uuid>` tags to avoid unbounded palace growth, but current `ChatSessionStore` doesn't support tags. Should sessions be queryable by tag (e.g., all sessions for user X)? Recommend: defer to MVP+1; chat_session_list pagination is sufficient for MVP.

3. **Cross-Palace Consolidation**: Can an app consolidate history across multiple palaces (e.g., user's personal + shared palace)? Current dream cycle is per-palace. Recommend: NO for MVP; keep palaces isolated.

4. **Completion Tracking for Tasks**: If a task is marked `completed_at`, when should it be auto-deleted? Recommend: manual deletion only; app controls lifecycle. Or add a tool `task_mark_complete(palace, drawer_id, completed_at)`.

5. **LLM Model Selection**: Current consolidation uses `openai/gpt-4o-mini` (see `crates/trusty-common/src/memory_core/dream/` for hardcoded model). Should apps be able to override? Recommend: defer to post-MVP; allow via config or env var.

---

## MVP / Phasing

### Phase 1: Palace `force` Flag (Small – S)
**Effort**: 1–2 hours  
**Files**: 
- `crates/trusty-memory/src/chat/sessions.rs` (add `force` param to HTTP handler)
- `crates/trusty-common/src/memory_core/validation.rs` (add `force` bypass)

**Test**: `palace_create` with `force=true` bypasses slug validation for `"my-custom-app"`.

**Relates to Epic**: #88 (palace slug validation)

---

### Phase 2: MCP-Expose Chat Turn Store (Small–Medium – S–M)
**Effort**: 2–4 hours  
**Files**:
- `crates/trusty-memory/src/chat/mod.rs` (new `tools.rs` or extend existing)
- `crates/trusty-memory/src/mcp/mod.rs` or `crates/trusty-memory/src/tools.rs` (add 4–5 chat tools to MCP server)
- Tests: `crates/trusty-memory/tests/chat_mcp.rs` (new integration tests)

**API**:
- `chat_session_create`, `chat_session_add_turn`, `chat_session_get`, `chat_session_list`

**Relates to Epic**: #1683 (shared-memory service)

---

### Phase 3: On-Demand Room-Filtered Dream Consolidation (Small – S)
**Effort**: 1–3 hours  
**Files**:
- `crates/trusty-memory/src/mcp/mod.rs` (add `dream_consolidate_room` tool)
- `crates/trusty-common/src/memory_core/dream/cycle.rs` (extract core logic into scoped helper)
- Tests: verify Task drawers skip consolidation

**API**: `dream_consolidate_room(palace, room?, max_age_days?)`

**Relates to Epic**: #1531 (dream/LLM consolidation), #1683 (shared-memory service)

---

### Phase 4: DrawerType::Task with Consolidation Exemption (Medium – M)
**Effort**: 3–5 hours  
**Files**:
- `crates/trusty-common/src/memory_core/palace.rs` (add `Task` variant)
- `crates/trusty-common/src/memory_core/dream/cycle.rs` (skip Task in eviction loop)
- `crates/trusty-common/src/memory_core/dream/semantic_consolidation/` (skip Task in consolidation)
- Tests: `crates/trusty-common/tests/memory_palace.rs` (new test: Task survives dream cycle)

**Semantics**: Task drawers never evicted or consolidated; optional `completed_at` for manual lifecycle.

**Relates to Epic**: #1589 (typed memories), #1683 (shared-memory service)

---

### Post-MVP Enhancements

1. **Structured ChatTurn Type** (issue #1545 or new): Replace generic `ChatMessage` with a richer type that includes metadata (model, temperature, tokens, latency, error if failed).
2. **Palace-Level TTL / GC** (new): Allow apps to specify palace expiration; auto-cleanup after N days.
3. **Session Tagging** (issue #1683 / related): Tag sessions by user/project; query by tag.
4. **Cross-Session Profile Summarization** (new): Summarize a user's interaction style across all sessions for long-term memory.
5. **Configurable LLM Model** (new): Allow apps to override model for consolidation (for cost / latency trade-offs).

---

## Relation to Existing Epics

### #1683 — Shared-Memory Service
This spec is a **direct sub-feature** of #1683 (unified shared-memory architecture). The MCP tools (`chat_session_*`, `dream_consolidate_room`) are part of the shared-memory contract.

### #1531 — Dream / LLM Consolidation
Phase 3 (on-demand consolidation) reuses and extends the dream cycle infrastructure. Compaction design is rooted in #1531.

### #1589 — Typed Memories
Phase 4 (DrawerType::Task) introduces a new drawer type as part of the broader effort to categorize memory by kind. Complements #1589's direction.

### #1191 — Palace GUID Migration
**Note**: This epic has no code presence yet (identity remains slug-based). Mentioned for context; no blocking dependency.

---

## Acceptance Criteria

A complete MVP implementation of this spec is **DONE** when:

1. **Palace Creation**
   - [ ] `palace_create(slug, name, force=true)` bypasses validation and creates custom palaces.
   - [ ] Isolation is confirmed: separate redb + HNSW per palace.

2. **Chat Session Storage**
   - [ ] MCP tools `chat_session_*` expose the existing redb-backed store.
   - [ ] Turns persist across daemon restart and are queryable by session ID.

3. **On-Demand Consolidation**
   - [ ] `dream_consolidate_room(palace, room?, max_age_days?)` triggers LLM-driven consolidation synchronously.
   - [ ] Room-scoped filtering works; null room = all rooms.
   - [ ] Returns summary count and eviction count.

4. **Task-Protected Drawers**
   - [ ] `DrawerType::Task` exists and is stored.
   - [ ] Dream cycle skips all Task drawers (verified by test).
   - [ ] Task drawers survive consolidation and eviction.
   - [ ] Optional `completed_at` is tracked.

5. **Tests & Documentation**
   - [ ] Integration tests cover all four MCP tools.
   - [ ] Dream cycle test verifies Task skipping.
   - [ ] MCP tool signatures match the spec.
   - [ ] CLAUDE.md or reference docs updated with task drawer semantics.

---

## Risk Mitigation

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| **Validation gate too strict post-MVP** | High | Apps cannot create custom palaces | Add `force` in Phase 1; test with external app driver |
| **Dream consolidation is too slow** | Medium | MCP tool blocks; timeouts occur | Phase 3: make async; return job ID + polling endpoint |
| **Chat store fills unbounded** | Medium | Disk usage grows; queries slow | Palace-level GC (post-MVP); add session TTL |
| **Task drawer semantics unclear** | Low | Misuse; tasks evicted anyway | Clear docs + test case in Phase 4 |
| **LLM cost escalation** | Low | Consolidation becomes expensive | MVP uses haiku-4-5 (cheap); allow model override post-MVP |

---

## References

### Code Locations
- Chat store types: `crates/trusty-common/src/memory_core/store/chat_sessions/types.rs`
- Chat store impl: `crates/trusty-common/src/memory_core/store/chat_sessions/store.rs`
- Drawer types: `crates/trusty-common/src/memory_core/palace.rs` (line 115+)
- Dream consolidation: `crates/trusty-common/src/memory_core/dream/` (all files)
- Palace validation: `crates/trusty-common/src/memory_core/validation.rs` (issue #88)
- HTTP chat integration: `crates/trusty-memory/src/chat/` (sessions.rs, handler.rs, mod.rs)

### Related Issues & Epics
- **#88**: Palace slug validation gate
- **#1531**: Dream / LLM consolidation (consolidation backbone)
- **#1589**: Typed memories (new drawer types)
- **#1683**: Shared-memory service (umbrella epic for this spec)
- **#1191**: Palace GUID migration (future; slug-based for now)

### Release Workflow
Once implementation is complete, see `docs/reference/release-workflow.md` for trusty-memory version bumping and publishing.

---

## Appendix: Storage Backend Confirmation

As of June 2026, **trusty-memory stores chat sessions in redb, not SQLite**:

> Migration from SQLite to redb was completed under issues #44–#57 and #989. All chat stores now use redb with postcard serialization. Vectors use pure-Rust hnsw_rs. See `Cargo.toml:288-291` for dependency configuration.

**File: Line Evidence**:
```
crates/trusty-common/Cargo.toml:288-291
  redb = { workspace = true }
  postcard = { workspace = true }
  hnsw_rs = { version = "...", default-features = false }

crates/trusty-common/src/memory_core/store/chat_sessions/store.rs:1–2
  //! `ChatSessionStore` implementation backed by redb.
```

This spec reflects that correct backend throughout.

---

**END OF SPEC**
