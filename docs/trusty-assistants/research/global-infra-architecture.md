---
title: Global Infrastructure Architecture Research
date: 2026-04-23
context: Pre-implementation analysis for global skills cache, project registry, inter-project message bus, and user memory
---

# Global Infrastructure Architecture Research

## Objective

Understand the current open-mpm Rust codebase architecture to inform implementation of:
- Global skills cache (`~/.open-mpm/skills/`)
- Project registry (`~/.open-mpm/projects.json`)
- Inter-project message bus (Unix domain sockets)
- User memory (`~/.open-mpm/memory/`)

---

## 1. Skills System (`src/skills/mod.rs`)

### Key Types

**`SkillEntry`** — indexed record for one Markdown skill file:
```rust
pub struct SkillEntry {
    pub name: String,        // from YAML frontmatter `name:` key, or filename stem
    pub description: String, // from frontmatter `description:` key
    pub tags: Vec<String>,   // from frontmatter `tags: [a, b, c]`
    pub path: PathBuf,       // absolute path to the .md file on disk
}
```

**`SkillRegistry`** — scan-once in-memory index of all skills in a directory:
```rust
pub struct SkillRegistry {
    pub skills: Vec<SkillEntry>,
}
```
- Loaded via `SkillRegistry::load(dir: &Path) -> anyhow::Result<Self>` — async directory scan
- Search via `search(query: &str, top_n: usize) -> Vec<&SkillEntry>`
- `auto_inject(task: &str, max_skills: usize) -> String` — builds `## Relevant Skills` prompt prefix
- `format_index()` — bulleted list for LLM tool responses
- Scoring: `relevance_score` — keyword substring matching: name hit +0.4, description +0.2, tag +0.4, capped at 1.0

**`SkillsLoader`** — task-aware loader with file-level caching:
```rust
pub struct SkillsLoader {
    skills_root: PathBuf,
    cache: tokio::sync::Mutex<HashMap<PathBuf, String>>,
}
```
- Constructed with `SkillsLoader::new(skills_root: PathBuf)`
- Resolves skill names via subdirectory search: `languages/<name>.md`, `frameworks/<name>.md`, `workflow/<name>.md`, flat `<name>.md`
- Language auto-detection: `Cargo.toml` → rust, `requirements.txt/pyproject.toml/setup.py` → python, `package.json` → typescript
- Framework detection from task text keywords: fastapi, pytest, sqlalchemy, tokio/axum, docker
- Workflow detection: tdd, wave-planning
- `build_skills_prefix(explicit: &[String], project_dir: &Path, task: &str) -> String` — returns `## Relevant Skills\n\n### Skill: <name>\n<body>` block

### Frontmatter Format

```markdown
---
name: skill-name
description: Short description
tags: [tag1, tag2, tag3]
---

# Skill body markdown content
```

### Caching Behavior

`SkillsLoader` caches raw file content (with frontmatter stripped) in a `tokio::sync::Mutex<HashMap<PathBuf, String>>`. Cache is per-instance, in-memory only — no persistence across process restarts.

### Implications for Global Skills Cache

- The existing `SkillRegistry` only points to absolute `PathBuf` values — no URLs or content hashes
- A global registry would need a new path root: `~/.open-mpm/skills/` with the same subdirectory structure (`languages/`, `frameworks/`, `workflow/`)
- Both `SkillRegistry` and `SkillsLoader` can be instantiated with any root path — drop-in compatible with a global root
- The in-process cache does not survive restarts; a persistent skills index (JSON or redb) would need to be added if deduplication across projects is required

---

## 2. Memory System (`src/memory/`)

### Module Structure

```
src/memory/
├── mod.rs           — public re-exports + migrate_if_needed()
├── store.rs         — MemoryStore trait, Segment enum, MemoryResult
├── redb_usearch.rs  — RedbUsearchStore: concrete redb + HNSW implementation
├── session_store.rs — SessionStore (per-run_id), SessionRegistry
├── code_store.rs    — CodeStore (shared code index with advisory lock)
├── embed.rs         — Embedder trait, FastEmbedder implementation
└── graph.rs         — AgentSession, MemoryGraph
```

### Core Trait (`store.rs`)

```rust
pub enum Segment { AgentMemory, CodeIndex }
impl Segment {
    pub fn prefix(&self) -> &'static str  // "mem" or "code"
}

pub struct MemoryResult {
    pub id: String,
    pub score: f32,        // 1.0 - cosine_distance
    pub payload: serde_json::Value,
    pub segment: String,
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn insert(segment, id, vector: &[f32], payload: Value) -> Result<()>;
    async fn search(segment, query_vec: &[f32], top_k: usize) -> Result<Vec<MemoryResult>>;
    async fn get(segment, id) -> Result<Option<Value>>;
    async fn delete(segment, id) -> Result<()>;
}
```

### Concrete Store (`redb_usearch.rs`)

`RedbUsearchStore` — redb for metadata/payloads + usearch HNSW for vectors:

```rust
pub struct RedbUsearchStore {
    db: Arc<Database>,              // redb database
    mem_index: Arc<Mutex<Index>>,   // usearch HNSW for AgentMemory
    code_index: Arc<Mutex<Index>>,  // usearch HNSW for CodeIndex
    mem_index_path: PathBuf,
    code_index_path: PathBuf,
}
```

- `open(store_dir: &Path, vector_dim: usize) -> Result<Self>`
- On-disk layout per store: `store.redb` + `mem.usearch` + `code.usearch`
- Cosine similarity (MetricKind::Cos + ScalarKind::F32)
- Auto-growing capacity: starts at 64, doubles when full
- Label mapping: auto-incrementing u64 per segment (stored in redb `counters` table)
- Re-insert of same `id`: removes old vector first, updates payload + vector atomically

**redb Tables:**
- `payloads` — key: `"{prefix}:{id}"`, value: JSON string
- `mem_label_to_id` / `mem_id_to_label` — bidirectional u64 ↔ str maps for `AgentMemory`
- `code_label_to_id` / `code_id_to_label` — bidirectional u64 ↔ str maps for `CodeIndex`
- `counters` — auto-increment per segment prefix

### Session Store (`session_store.rs`)

Each PM invocation gets a private memory namespace:

```rust
pub struct SessionStore {
    inner: RedbUsearchStore,
    pub run_id: String,
}
// On-disk: .open-mpm/sessions/<run_id>/store.redb + mem.usearch

pub struct SessionMeta {
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub task_preview: String,
}

pub struct SessionRegistry {
    db: Database,  // sessions/index.redb
}
```

`SessionStore` only accepts `Segment::AgentMemory` — it guards against CodeIndex writes.

`SessionRegistry` tracks all known run_ids in `sessions/index.redb` (single redb table). Sessions are listed sorted by `started_at`.

### Layout Migration (`mod.rs::migrate_if_needed`)

Migrates old monolithic `.open-mpm/store/` layout to split layout:
- `store.redb` → `code/store.redb`
- `code.usearch` → `code/code.usearch`
- `mem.usearch` → `sessions/default/mem.usearch`

### Context Module Memory (`src/context/`)

The `context/` module is distinct from `memory/` — it handles history indexing and retrieval for conversational context rather than vector storage:

- `context/indexer.rs` — `IndexedEntry`, `TurnRecord` — per-turn history records
- `context/cluster.rs` — `ClusterStore` — JSONL append-only store for synthesized memory clusters
- `context/retrieval.rs` — retrieval logic with BM25 + embedding hybrid search
- `context/manager.rs` — `ContextManager` — token budget enforcement for long conversations
- `context/cleaner.rs` — history cleanup
- `context/goals.rs` — goal tracking

**`ContextManager`** (`context/manager.rs`):
```rust
pub struct ContextManager {
    pub soft_threshold: f32,  // fraction of model context window (clamped 0.1..=1.0)
    budgets: HashMap<String, u32>,
}
```
- `trim_to_budget(messages, model, protected_count) -> (Vec<Value>, usize)` — evicts oldest non-protected messages when total estimated tokens exceeds `soft_threshold * context_window(model)`
- Context windows: Claude models = 200k, GPT-5.1-codex = 400k, unknown = 128k
- Token estimation: `content.len() / 4` (4 chars ≈ 1 token)

**`ClusterStore`** (`context/cluster.rs`):
```rust
pub struct ClusterStore { path: PathBuf }  // <store_dir>/clusters.jsonl
```
- Append-only JSONL file
- Stores `IndexedEntry` records containing synthesized summaries + embeddings
- Used by the retriever to surface consolidated memories with a 2x boost

### Implications for User Memory (`~/.open-mpm/memory/`)

- `RedbUsearchStore::open(path, vector_dim)` can be pointed at any directory — direct path substitution
- The `MemoryStore` trait ensures callers are backend-agnostic
- User-global memory would use `Segment::AgentMemory` namespace
- The `SessionRegistry` pattern (redb index of run_ids) can be adapted for a global project registry

---

## 3. Context System (`src/context/`)

### Context Injection Pipeline

1. `ProjectInitializer::initialize_if_needed()` produces `InitContext`
2. `InitContext::to_prompt_prefix()` renders a Markdown block
3. Workflow engine prepends this block to each phase's template
4. `ContextManager::trim_to_budget()` evicts oldest turns if the window budget is exceeded

### `InitContext` Structure

```rust
pub struct InitContext {
    pub project_summary: String,      // rendered Markdown from project-index.md
    pub relevant_memories: Vec<String>, // text snippets from kuzu-memory files
    pub initialized_at: DateTime<Utc>,
}
```

`to_prompt_prefix()` output:
```
## Project Context (auto-indexed)

<project_summary>

## Relevant Prior Knowledge

- <memory_snippet_1>
- <memory_snippet_2>

---

```

### `ProjectInitializer` (`src/init/mod.rs`)

```rust
pub struct ProjectInitializer {
    project_dir: PathBuf,
    open_mpm_dir: PathBuf,
}
```

**Key behavior:**
- Marker TTL: 24 hours (re-scans after expiry)
- Marker file: `.open-mpm/initialized` (JSON with `initialized_at`, `project_name`, `file_count`)
- Index file: `.open-mpm/project-index.md`
- Scan depth: 2 directory levels
- Included extensions: `.rs`, `.toml`, `.json`, `.md`
- Excluded directories: `.git`, `target`, `node_modules`, `.venv`, `.open-mpm`, `out`, `dist`, etc.
- Memory sources (in order): `<project>/kuzu-memories/`, `<project>/.kuzu-memory/`, `~/.kuzu-memory/exports/`, `~/.kuzu-memory/`
- Memory budget: 8000 chars (≈ 2000 tokens)

**`ProjectIndex`** intermediate type:
```rust
pub struct ProjectIndex {
    pub project_name: String,
    pub entries: Vec<IndexEntry>,
}
pub struct IndexEntry {
    pub rel_path: String,
    pub summary: String,  // first sentence of first doc comment / heading
}
```

### Implications for Global Infrastructure

- The kuzu-memory path `~/.kuzu-memory/` is already read during project init
- A global `~/.open-mpm/memory/` dir can be added to `read_kuzu_memories` candidates
- The `dirs_home()` helper returns `$HOME` from env — no external `dirs` crate dependency
- `ProjectInitializer` is instantiated per-project per-run; a global registry would be a new concept

---

## 4. IPC System (`src/ipc/mod.rs`)

### Message Types

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcMessage {
    Task {
        id: String,
        task: String,
        history: Option<Vec<HistoryMessage>>,
        session_reset: Option<bool>,
    },
    Result {
        id: String,
        content: String,
        summary: Option<String>,
        usage: Option<TokenUsage>,
        status: String,
    },
    Error {
        id: String,
        error: String,
        status: String,
    },
}
```

**Wire format:** Single-line JSON with `\n` terminator (NDJSON). Discriminated by `"type"` field.

**Key functions:**
- `serialize_message(msg: &IpcMessage) -> Result<String>` — appends `\n`
- `parse_message(line: &str) -> Result<IpcMessage>` — trims trailing `\r\n`
- `extract_summary(content: &str) -> String` — extracts `## Summary` section, 500-char fallback
- `extract_files_from_content(content: &str) -> Vec<(PathBuf, String)>` — parses `## File: <path>` sections

**Optional fields** use `#[serde(default, skip_serializing_if = "Option::is_none")]` — backward compatible with older agents that don't send `history`, `summary`, or `usage`.

### `HistoryMessage` (referenced from `session.rs`)

Used in `Task.history` for persistent-session agents. `HistoryMessage::user(text)` and `HistoryMessage::assistant(text)` constructors.

### Implications for Inter-Project Message Bus

- The NDJSON protocol is already proven and well-tested
- A Unix domain socket bus would need:
  - A server process listening on `~/.open-mpm/bus.sock`
  - Framing: existing NDJSON works (each line = one message)
  - New IPC variant or additional envelope field for routing (source project, target project, topic)
- The `IpcMessage` enum can be extended with new variants (backward-compat via `#[serde(tag)]`)

---

## 5. Project Initialization (`src/init/mod.rs`)

Already covered in Section 3. Key additional details:

### Startup Sequence (`src/main.rs` lines 72-150)

1. Handle `--version` early (no API key needed)
2. Load `.env.local` and `.env` via dotenvy
3. Initialize tracing (stderr only — stdout reserved for NDJSON)
4. Bump build counter (`.open-mpm/build.json`)
5. Set `OPEN_MPM_RUN_ID` env var (UUID v4) — inherited by all subprocesses
6. `memory::migrate_if_needed(&open_mpm_dir)` — migrate legacy store layout
7. `WorktreeManager::cleanup_stale()` — remove orphaned git worktrees
8. Dispatch to subcommand mode or PM/sub-agent mode

### Binary Modes

- No args → PM orchestrator mode
- `--agent <name>` → sub-agent mode (reads NDJSON Task from stdin)
- `--direct <name>` → bypass PM, call agent directly
- `--workflow <name>` → run declarative multi-phase workflow
- `memory` / `code` → CLI search subcommands

---

## 6. Agent Config (`config/agents/pm.toml`)

```toml
[agent]
name = "pm"
role = "orchestrator"
model = "anthropic/claude-sonnet-4-6"
description = "PM orchestrator — receives user requests, delegates to sub-agents"

[llm]
temperature = 0.3
max_tokens = 4096

[system_prompt]
content = """..."""
```

Additional agent config fields (from CLAUDE.md and other agents):
- `runner = "claude-code"` — uses `ClaudeCodeAgentRunner` (claude CLI subprocess) instead of REST API
- `[llm] use_anthropic_direct = true` — calls `api.anthropic.com` directly instead of OpenRouter

---

## 7. Key Interconnections

### Data Flow: Skills into Prompts

```
SkillsLoader::new("config/skills")
    → build_skills_prefix(explicit, project_dir, task)
        → detect_languages(project_dir)  // Cargo.toml → "rust"
        → detect_frameworks(task)        // "fastapi" in text → "fastapi"
        → resolve_skill_path("rust")     // languages/rust.md
        → load_skill_file(path)          // cached file read, strip frontmatter
    → "## Relevant Skills\n\n### Skill: rust\n<body>"
```

### Data Flow: Memory into Prompts

```
ProjectInitializer::initialize_if_needed()
    → check .open-mpm/initialized (TTL 24h)
    → scan_project() → ProjectIndex
    → write .open-mpm/project-index.md
    → read_kuzu_memories() from kuzu-memories/, .kuzu-memory/, ~/.kuzu-memory/
    → InitContext { project_summary, relevant_memories }
    → InitContext::to_prompt_prefix()
    → prepended to every phase template by WorkflowEngine
```

### Data Flow: Agent Memory Persistence

```
SessionStore::open(.open-mpm/sessions, run_id, vector_dim)
    → creates .open-mpm/sessions/<run_id>/store.redb
    → creates .open-mpm/sessions/<run_id>/mem.usearch
    → registers in .open-mpm/sessions/index.redb
    → MemoryStore::insert(AgentMemory, id, vector, payload)
    → MemoryStore::search(AgentMemory, query_vec, top_k)
```

---

## 8. Implementation Guidance for Global Infrastructure

### Global Skills Cache (`~/.open-mpm/skills/`)

**Pattern:** Instantiate `SkillsLoader` or `SkillRegistry` with `~/.open-mpm/skills/` as root. No code changes needed — both types accept any `PathBuf`.

**Directory structure to create:**
```
~/.open-mpm/skills/
├── languages/
│   ├── rust.md
│   └── python.md
├── frameworks/
│   ├── fastapi.md
│   └── tokio.md
└── workflow/
    └── tdd.md
```

**Integration point:** Before calling `SkillsLoader::build_skills_prefix`, also check the global root if local `config/skills/` yields no matches, or merge results.

### Project Registry (`~/.open-mpm/projects.json`)

**Pattern:** Mirror `SessionRegistry` — a redb or JSON file at `~/.open-mpm/projects.json` mapping project root paths to metadata.

**`ProjectMeta` struct to add:**
```rust
pub struct ProjectMeta {
    pub root: PathBuf,
    pub name: String,
    pub last_active: DateTime<Utc>,
    pub run_ids: Vec<String>,
}
```

**Integration point:** On startup in `main.rs`, after `initialize_if_needed()`, upsert current `cwd` into the global registry.

### Inter-Project Message Bus (Unix Domain Sockets)

**Pattern:** New server process listening on `~/.open-mpm/bus.sock`. Clients connect and exchange NDJSON messages with routing envelope.

**Message envelope extension:**
```rust
// New IpcMessage variant or wrapper:
pub struct BusEnvelope {
    pub source_project: PathBuf,
    pub target_project: Option<PathBuf>,  // None = broadcast
    pub topic: String,
    pub payload: IpcMessage,
}
```

**Key constraints from existing code:**
- All IPC is currently point-to-point (PM ↔ one sub-agent). A bus introduces fan-out.
- The `OPEN_MPM_RUN_ID` env var must be set before any threads spawn — bus connections happen after.
- Tracing writes to stderr; bus protocol must stay on the socket (not stdout/stderr).

### User Memory (`~/.open-mpm/memory/`)

**Pattern:** Open a `RedbUsearchStore` at `~/.open-mpm/memory/` with `Segment::AgentMemory`.

**Integration point:** Extend `ProjectInitializer::read_kuzu_memories()` to also read from `~/.open-mpm/memory/`, or add a new `UserMemoryStore` that wraps `RedbUsearchStore`.

**Vector dimension:** Must match the `FastEmbedder` output dimension — check `src/memory/embed.rs` for the fastembed model being used.

---

## Files Referenced

- `/Users/masa/Projects/open-mpm/src/skills/mod.rs` — `SkillEntry`, `SkillRegistry`, `SkillsLoader`
- `/Users/masa/Projects/open-mpm/src/memory/mod.rs` — module exports, `migrate_if_needed`
- `/Users/masa/Projects/open-mpm/src/memory/store.rs` — `MemoryStore` trait, `Segment`, `MemoryResult`
- `/Users/masa/Projects/open-mpm/src/memory/redb_usearch.rs` — `RedbUsearchStore`
- `/Users/masa/Projects/open-mpm/src/memory/session_store.rs` — `SessionStore`, `SessionRegistry`, `SessionMeta`
- `/Users/masa/Projects/open-mpm/src/context/manager.rs` — `ContextManager`, `context_window`
- `/Users/masa/Projects/open-mpm/src/context/cluster.rs` — `ClusterStore`
- `/Users/masa/Projects/open-mpm/src/ipc/mod.rs` — `IpcMessage`, `serialize_message`, `parse_message`, `extract_summary`, `extract_files_from_content`
- `/Users/masa/Projects/open-mpm/src/init/mod.rs` — `ProjectInitializer`, `InitContext`, `ProjectIndex`
- `/Users/masa/Projects/open-mpm/src/main.rs` — startup sequence, binary dispatch
- `/Users/masa/Projects/open-mpm/config/agents/pm.toml` — PM agent config shape
