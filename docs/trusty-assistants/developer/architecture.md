# Architecture

A Rust-based AI agent orchestration harness. A single binary hosts every
execution mode: an interactive CTRL REPL, a PM orchestrator, a sub-agent
subprocess runner, a declarative workflow engine, an HTTP API server with
embedded web UI, and utility CLI subcommands.

The design trades maximum flexibility for a straightforward process model —
each agent invocation is a short-lived subprocess, making state isolation
and resource bounds predictable.

## High-level data flow

```
User (stdin / HTTP / CLI)
       │
       ▼
 ┌──────────────────────────────┐
 │  Entry point (src/main.rs)   │
 │  argv dispatch               │
 └──────┬───────────────────────┘
        │
        ├──── --ctrl ──► CTRL REPL (multi-project Taskmaster, search_docs)
        ├──── --pm   ──► PM Orchestrator (single shot)
        ├──── --api  ──► Axum HTTP server + embedded web UI
        ├──── --workflow ──► WorkflowEngine
        └──── --agent <n> ──► SubprocessAgentRunner
                              │ NDJSON over stdin/stdout
                              ▼
                    ┌───────────────────────┐
                    │  LLM backends         │
                    │  • OpenRouter         │
                    │  • api.anthropic.com  │
                    │  • claude CLI         │
                    └───────────────────────┘
```

## Execution modes

| Flag | Mode | Description |
|---|---|---|
| `--ctrl` | CTRL REPL | Multi-project session manager (default) |
| `--pm` | PM | Single-shot orchestrator, reads from stdin |
| `--agent <n>` | Sub-agent | Reads one NDJSON Task, returns one Result, exits |
| `--direct <n>` | Direct | Bypasses PM LLM, sends task straight to sub-agent |
| `--workflow <n>` | Workflow | Runs `.open-mpm/workflows/<n>.json` |
| `--api` (`--serve`) | API | Axum HTTP server on `0.0.0.0:<port>` + embedded web UI |
| `code` / `memory` | CLI search | Query indexes, no LLM |
| `--reindex` | Index | Full re-index of working tree |
| `--watch` | Index | notify-based live indexer |

## Module map

```
src/
├── main.rs              Entry point; argv dispatch; per-mode runner functions
│
├── agents/              Agent config loading + runner selection
│   ├── mod.rs           AgentConfig, AgentInfo, LlmParams, RunnerKind
│   ├── claude_code_runner.rs  ClaudeCodeAgentRunner, DispatchingAgentRunner
│   ├── claude_mpm_loader.rs   Fallback loader from .claude/agents/
│   └── prompt_builder.rs      SystemPromptBuilder: base + CLAUDE.md + skills
│
├── workflow/            Declarative multi-phase orchestration
│   ├── config.rs        WorkflowDef, PhaseDef, AutoPushConfig, TicketManagementConfig
│   ├── engine.rs        WorkflowEngine: phase loop, wave loop, file extraction
│   ├── context.rs       WorkflowContext: phase output accumulator
│   ├── parallel.rs      Parallel subtask dispatch (FuturesUnordered)
│   ├── resolver.rs      Template variable substitution ({{task}}, {{phase}}, …)
│   ├── tickets.rs       TicketManager: GitHub issue lifecycle via gh CLI
│   ├── worktree.rs      WorktreeManager: git worktrees for parallel phases
│   └── autopush.rs      Auto-commit/push after a successful run
│
├── tools/               LLM function-calling tool implementations
│   ├── mod.rs           ToolRegistry: register/dispatch/schema
│   ├── traits.rs        ToolExecutor, AgentRunner, ToolResult
│   ├── delegate.rs      DelegateToAgentTool: PM's primary tool
│   ├── fs_reader.rs     ReadFileTool, ListDirTool, GrepFilesTool
│   ├── write_file.rs    WriteFileTool: atomic writes, out_dir sandboxing
│   ├── shell.rs / shell_exec.rs   ShellExecTool variants
│   ├── web_search.rs    BraveSearchTool, FetchUrlTool
│   ├── memory*.rs       Memory + vector search tools
│   ├── skill_loader.rs  SkillLoaderTool, SkillListTool
│   ├── phase_audit.rs   Workflow phase lifecycle management
│   ├── finish_task.rs   Terminal signal for tool_choice=any agents
│   ├── native_*.rs      Memory, ticketing, search tool implementations
│   └── format_translator.rs   Markdown↔HTML, JSON↔TOML, YAML↔JSON
│
├── llm/                 LLM client and chat loops
│   ├── mod.rs           create_client, chat, chat_with_tools_gated
│   ├── adapter.rs       ModelAdapter trait + Anthropic/OpenAI/Generic impls
│   └── anthropic_native.rs    Native /v1/messages format builder/parser
│
├── api/                 HTTP API + embedded web UI (#151, #181, #187)
│   ├── server.rs        Axum router, CORS, optional bearer-token auth,
│   │                    rust-embed UI assets, /api/docs/search
│   ├── builder.rs       PmResponse projection from WorkflowContext
│   └── types.rs         Wire shapes: PmResponse, PhaseProgress, …
│
├── ctrl/                CTRL REPL — multi-project PM session management
│   └── mod.rs           Ctrl, PmHandle, pm_actor_task, ctrl_chat_turn,
│                        SearchDocsTool, MessageBus relay
│
├── ipc/                 NDJSON inter-process communication
│   └── mod.rs           IpcMessage enum, parse/serialize, file extraction
│
├── docs_index.rs        TF-IDF doc index for CTRL search_docs (#187)
├── subprocess.rs        SubprocessAgentRunner: spawn binary --agent NAME
├── skills/              Skill discovery and injection
│   ├── mod.rs / registry.rs   SkillRegistry, SkillsLoader
│   └── global_cache.rs        ~/.open-mpm/skills discovery
│
├── memory/              Local vector/graph memory stores
│   ├── code_store.rs    redb+usearch chunk storage
│   ├── embed.rs         fastembed-based local embeddings (384-d)
│   ├── graph.rs         kuzu-based knowledge graph
│   └── user_store.rs    UserMemoryStore: ~/.kuzu-memory/user/
│
├── search/              Code indexer + file watcher
│   ├── indexer.rs       tree-sitter parse + chunk + embed
│   └── watcher.rs       notify-based live indexer
│
├── context/             Context window management
│   ├── mod.rs           ContextManager: token budget trimming
│   ├── cluster.rs       Turn clustering for compaction
│   └── indexer.rs       HistoryIndexer: persist turn log
│
├── interaction_log.rs   Persisted interaction log (PM↔agent turns)
├── mistake_log.rs       Captures repeated agent mistakes (#186)
├── progress.rs          Phase progress streaming (__OMPM_PROGRESS__ stderr lines)
├── perf.rs              TokenUsage, PhaseRecord, PerfCollector
├── session.rs           AgentSession, SessionManager, HistoryMessage
├── session_record.rs    Append-only run record JSONL
├── build_info.rs        VERSION, GIT_HASH, monotonic build counter
├── init/                ProjectInitializer: first-run seeding
├── registry.rs          ProjectRegistry: ~/.open-mpm/projects.json
├── process_tracker.rs   .open-mpm/state/processes.json
├── bus.rs               MessageBus: inter-project UNIX-socket pub/sub
├── ticketing/           TicketingClient + GitHub/JIRA/Linear backends
├── compress/            Session history compactor
└── cli/                 CLI subcommands (memory search, code search)
```

## Key components

### CTRL REPL (`src/ctrl`)

Default mode. Manages multiple PM actors keyed by project path. Each actor
is a tokio task fed via mpsc channel. Hosts the Taskmaster persona with a
small tool set (`start_pm`, `list_projects`, `task_status`,
`self_project_status`, `initiate_self_task`, `memory_store`,
`memory_recall`, `search_docs`). Connected to the inter-project
`MessageBus` for cross-project relay; relayed envelopes are appended to
`~/.open-mpm/sessions/pm-messages.jsonl` for an audit trail.

### PM orchestrator (`src/main.rs::run_pm`)

Reads one line of input, calls the PM LLM with `delegate_to_agent`
registered, and dispatches any tool calls to the registry. The PM tool
set is intentionally minimal — its job is routing.

### Sub-agent process (`src/main.rs::run_subagent`)

Started as `current_exe --agent <name>`. Reads one NDJSON `Task`, builds
the effective system prompt (base + CLAUDE.md ancestor walk + resolved
skills), selects a tool registry based on the agent name, and runs
either a single-shot `llm::chat` or a multi-turn `chat_with_tools_gated`
loop. Emits one NDJSON `Result` or `Error` and exits.

### Workflow engine (`src/workflow/engine.rs`)

Owns the phase loop. For each `PhaseDef`:

1. Renders `context_template` against the accumulated `WorkflowContext`
2. Runs the phase agent via the configured `AgentRunner`
3. If `produces_files`, extracts `## File: <path>` sections to `out_dir`
4. If `parallel_subtasks`, dispatches concurrently
5. If `worktree_protection`, isolates each subtask in its own git worktree
6. If `assignments.json` exists in `out_dir`, runs the wave loop —
   one sub-agent per file in topological order
7. Streams `__OMPM_PROGRESS__ {…}` lines to stderr so an outer process
   (the API server, Tauri UI) can render a live phase timeline
8. Optionally wraps each run with `TicketManager` and `PerfCollector`

### Tool registry (`src/tools/mod.rs`)

`ToolRegistry` holds `Arc<dyn ToolExecutor>` keyed by `tool.name()`.
Both the PM loop and sub-agent tool loop dispatch through it.
`dispatch_gated` applies a per-agent allowlist; `openai_tools()`
projects schemas into the async-openai typed builder.

### LLM client (`src/llm/mod.rs`)

Two primary call paths:

- `chat`: single system+user request, no tools
- `chat_with_tools_gated`: multi-turn loop. Dispatches tool calls
  concurrently via `FuturesUnordered`, injects tool-discipline
  reminders on plain-text turns, respects `max_turns`, `tool_choice`,
  and `finish_task`.

Three HTTP backends:

- `async-openai` typed builder (default, OpenRouter-compatible)
- Raw `reqwest` POST (when fields like `cache_control` aren't typed)
- Native Anthropic `/v1/messages` (when `use_anthropic_direct = true`)

### NDJSON IPC (`src/ipc/mod.rs`)

```
PM → sub-agent:   {"type":"task",   "id":"<uuid>", "task":"...", "history":[...]}
sub-agent → PM:   {"type":"result", "id":"<uuid>", "content":"...", "summary":"...", "usage":{...}}
sub-agent → PM:   {"type":"error",  "id":"<uuid>", "error":"...", "status":"error"}
```

`extract_files_from_content` parses `## File: <path>` sections from LLM
output into `(PathBuf, String)` pairs.

### HTTP API + embedded web UI (`src/api`)

Axum router with five JSON routes plus an SPA fallback:

- `POST /api/task` → spawn workflow, return `{id, status:"running"}`
- `GET  /api/task/:id` → cached `PmResponse` (or running placeholder)
- `GET  /api/tasks` → up to 20 recent responses
- `GET  /api/health` → `{status, version}`
- `GET  /api/config` → `{auth_required}` (UI bootstrap)
- `GET  /api/docs/search?q=<query>&n=<top_n>` → TF-IDF docs results (#187)
- `GET  /` and `GET  /*path` → SPA assets baked into the binary via
  `rust-embed` (`ui/dist/`), with `index.html` fallback for client-side
  routing

Optional bearer-token auth via `--api-token` or `OPEN_MPM_API_TOKEN`.

### Docs index (`src/docs_index.rs`)

In-memory TF-IDF index over `<project>/docs/*.md`. Built once at startup
(in a `tokio::spawn_blocking` task) and held in `Arc`. Powers:

- CTRL `search_docs` tool (lazy: index installs into
  `Arc<Mutex<Option<...>>>` after the REPL is already running)
- `GET /api/docs/search` route (built before the server binds)

Cosine similarity over L2-normalized TF-IDF vectors. No model downloads,
no external vector DB; sufficient for hundreds of Markdown files.

### Memory and code index

- **redb**: embedded KV for chunk metadata
- **usearch**: in-process ANN index
- **fastembed**: local 384-dim sentence-transformer embeddings
- **tree-sitter**: language-aware AST chunking (Rust, Python, JS/TS, Go, Markdown)
- **kuzu**: knowledge graph for cross-session memory

Search exposed via:

- `code search <query>` / `memory search <query>` CLI subcommands
- `VectorSearchTool`, `SearchCodeTool` callable by agents
- `MemorySearchTool`: hybrid vector + BM25 over the history turn log

### Mistake log + postmortem agent (#186)

`mistake_log.rs` appends a structured record every time a phase fails or
is retried. `postmortem-agent` reads recent records and proposes process
improvements. Invoke via `open-mpm postmortem [--last N | --session <id>]`.

## State and storage layout

### Per-project (`<cwd>/.open-mpm/`)

```
.open-mpm/
├── agents/              committed: agent TOML configs
├── skills/              committed: project-local skills
├── workflows/           committed: workflow JSON
└── state/               gitignored
    ├── build.json       monotonic build counter
    ├── sessions/<run_id>/   per-invocation turn logs
    ├── worktrees/       parallel-phase git worktrees
    ├── history/         HistoryIndexer turn log
    ├── code/            redb+usearch code index
    └── processes.json   tracked sub-agent PIDs
```

### Global (`~/.open-mpm/`)

```
~/.open-mpm/
├── projects.json        global project registry
├── sockets/<name>.sock  MessageBus UNIX sockets
├── skills/files/        globally-shared skills
├── memory/              shared memory stores
└── sessions/            cross-session audit logs
```

## Startup sequence

`main()` executes in order on every invocation:

1. Fast path: `--version` / `-V`
2. Load `.env.local` and `.env`
3. Initialize tracing
4. Increment build counter, emit banner
5. Set `OPEN_MPM_RUN_ID` (UUID inherited by sub-agents)
6. Migrate legacy memory layout if needed
7. Clean up stale worktrees from interrupted runs
8. Register the project in `ProjectRegistry`
9. Clean up stale sub-agent PIDs via `ProcessTracker`
10. Start the inter-project `MessageBus`
11. Dispatch to the requested execution mode

## Skill injection priority

1. `<project>/.claude/skills/`
2. `~/.claude/skills/`
3. `~/.open-mpm/skills/files/`
4. `<project>/.open-mpm/skills/`

`SystemPromptBuilder` layers them after the base prompt:

```
[base system prompt]
[CLAUDE.md instructions from CWD ancestors]
[# Skill: <name>\n\n<content>] for each resolved include
```

## Progress streaming

Sub-agents and the workflow engine emit `__OMPM_PROGRESS__ {…json…}` lines
to stderr at phase transitions. The API server (`src/api/server.rs::run_task`)
and the Tauri UI both read these lines off the child process's stderr and
forward them into the `PmResponse` so the polling client renders a live
phase timeline.
