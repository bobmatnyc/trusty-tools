# open-mpm Architecture

## Overview

open-mpm is a Rust-based AI agent orchestration harness. A single binary hosts
every execution mode: an interactive CTRL REPL, a PM orchestrator, a sub-agent
subprocess runner, a declarative workflow engine, and utility CLI subcommands.
The design trades maximum flexibility for a straightforward process model — each
agent invocation is a short-lived subprocess, making state isolation and resource
bounds predictable.

---

## High-Level Data Flow

```
User (stdin / CLI)
       |
       v
 ┌─────────────────────────────┐
 │      CTRL CLI (--ctrl)      │  interactive REPL
 │  multi-project dispatcher   │  manages multiple PM actors
 └──────────────┬──────────────┘
                |
                v
 ┌─────────────────────────────┐
 │   PM Orchestrator (--pm)    │  reads: config/agents/pm.toml
 │  LLM: OpenRouter/Anthropic  │  tool: delegate_to_agent
 └──────────────┬──────────────┘
                |  spawn(current_exe --agent <name>)
                |  stdin: NDJSON Task
                v
 ┌─────────────────────────────────────────┐
 │  Sub-Agent Process (--agent <name>)     │
 │  reads: config/agents/<name>.toml       │
 │  LLM multi-turn loop + tool registry   │
 │  stdin  <- {"type":"task", ...}         │
 │  stdout -> {"type":"result", ...}       │
 └──────────────┬──────────────────────────┘
                |
                v
 ┌─────────────────────────────┐
 │  LLM Backend                │
 │  OpenRouter (default)       │
 │  api.anthropic.com (direct) │
 │  claude CLI (runner=claude) │
 └─────────────────────────────┘
```

---

## Execution Modes

The `main()` function inspects `argv` and dispatches to one of these modes:

| Flag              | Mode             | Description                                              |
|-------------------|------------------|----------------------------------------------------------|
| `--ctrl`          | CTRL REPL        | Interactive multi-project session manager (default)      |
| `--pm`            | PM orchestrator  | Single-shot orchestrator, reads user input from stdin    |
| `--agent <name>`  | Sub-agent        | Reads one NDJSON Task, returns one NDJSON Result, exits  |
| `--direct <name>` | Direct mode      | Bypasses PM LLM, sends task straight to named sub-agent  |
| `--workflow <n>`  | Workflow engine  | Runs `config/workflows/<n>.json` phase by phase          |
| `memory`/`code`   | CLI search       | Query local memory/code index, no LLM required           |
| `--reindex`       | Index rebuild    | Full re-index of working tree, then exits                |
| `--watch`         | Live indexer     | Watch filesystem and keep code index in sync             |

---

## Module Map

```
src/
├── main.rs            Entry point. argv dispatch. Per-mode runner functions.
│
├── agents/            Agent config loading + runner selection
│   ├── mod.rs         AgentConfig, AgentInfo, LlmParams, ToolsConfig, RunnerKind
│   ├── claude_code_runner.rs  ClaudeCodeAgentRunner, DispatchingAgentRunner
│   ├── claude_mpm_loader.rs   Fallback: load agents from .claude/agents/ (MD+YAML)
│   └── prompt_builder.rs      SystemPromptBuilder: layers base + CLAUDE.md + skills
│
├── workflow/          Declarative multi-phase orchestration
│   ├── mod.rs         Re-exports
│   ├── config.rs      WorkflowDef, PhaseDef, Assignments, AutoPushConfig, TicketManagementConfig
│   ├── engine.rs      WorkflowEngine: phase loop, wave loop, file extraction
│   ├── context.rs     WorkflowContext: phase output accumulator
│   ├── error.rs       WorkflowError enum
│   ├── parallel.rs    Parallel subtask dispatch (FuturesUnordered per subtask)
│   ├── resolver.rs    Template variable substitution ({{task}}, {{phase}}, etc.)
│   ├── tickets.rs     TicketManager: GitHub issue lifecycle via `gh` CLI
│   ├── worktree.rs    WorktreeManager: git worktrees for parallel phases
│   └── autopush.rs    Auto-commit/push after successful workflow run
│
├── tools/             LLM function-calling tool implementations
│   ├── mod.rs         ToolRegistry: register/dispatch/schema + native_tool_registry
│   ├── traits.rs      ToolExecutor, AgentRunner, AgentOutput, RunContext, ToolResult
│   ├── delegate.rs    DelegateToAgentTool: PM's primary tool
│   ├── fs_reader.rs   ReadFileTool, ListDirTool, GrepFilesTool
│   ├── write_file.rs  WriteFileTool: atomic writes, out_dir sandboxing
│   ├── shell.rs       ShellExecTool: allowlisted shell (local-ops-agent)
│   ├── shell_exec.rs  ShellExecTool: QA-agent shell executor
│   ├── web_search.rs  BraveSearchTool, FetchUrlTool
│   ├── memory.rs      KuzuRecallTool, VectorSearchTool
│   ├── memory_search.rs  MemorySearchTool: vector+BM25 hybrid over history
│   ├── skill_loader.rs   SkillLoaderTool, SkillListTool
│   ├── phase_audit.rs    PhaseAuditTool: workflow phase lifecycle management
│   ├── finish_task.rs    FinishTaskTool: terminal signal for tool_choice=any agents
│   ├── native_search.rs  SearchCodeTool, SearchMemoryTool, SearchSkillsTool
│   ├── native_memory.rs  StoreMemoryTool, RetrieveMemoryTool, ListMemoryKeysTool
│   ├── native_ticketing.rs CreateTicketTool, GetTicketTool, CloseTicketTool, etc.
│   └── format_translator.rs  Markdown→HTML, JSON↔TOML, YAML→JSON
│
├── llm/               LLM client and chat loops
│   ├── mod.rs         create_client, chat, chat_with_tools_gated, send_raw_completion
│   ├── adapter.rs     ModelAdapter trait, AnthropicAdapter, OpenAIAdapter, GenericAdapter
│   └── anthropic_native.rs  Native Anthropic /v1/messages format builder/parser
│
├── ipc/               NDJSON inter-process communication
│   └── mod.rs         IpcMessage enum, serialize_message, parse_message, extract_files_from_content
│
├── subprocess.rs      SubprocessAgentRunner: spawn binary in --agent mode, NDJSON IPC
│
├── skills/            Skill discovery and injection
│   ├── mod.rs         SkillRegistry, SkillsLoader
│   └── global_cache.rs  GlobalSkillsCache: ~/.open-mpm/skills discovery
│
├── memory/            Local vector/graph memory stores
│   ├── mod.rs         CodeStore (redb+usearch), FastEmbedder, MemoryGraph, AgentSession
│   ├── code_store.rs  Chunk storage with embedding vectors
│   ├── embed.rs       fastembed-based local embeddings
│   ├── graph.rs       MemoryGraph: kuzu-based knowledge graph
│   ├── redb_usearch.rs  Redb+usearch store backend
│   └── user_store.rs  UserMemoryStore: ~/.kuzu-memory/user/ prompt suffix
│
├── search/            Code indexer and file watcher
│   ├── indexer.rs     CodeIndexer: tree-sitter parse + chunk + embed
│   └── watcher.rs     FileWatcher: notify-based live indexer
│
├── context/           Context window management
│   ├── mod.rs         ContextManager: token budget trimming
│   ├── cleaner.rs     MemoryCleaner: async periodic cleanup
│   ├── cluster.rs     Turn clustering for context compaction
│   └── indexer.rs     HistoryIndexer: persist turn log to disk
│
├── session.rs         AgentSession, SessionManager, HistoryMessage
├── subprocess.rs      SubprocessAgentRunner, spawn_subagent_and_run
├── perf.rs            TokenUsage, PhaseRecord, PerfCollector
├── build_info.rs      VERSION, GIT_HASH, BuildInfo (monotonic build counter)
├── init/              ProjectInitializer: auto-index seeding on first run
├── registry.rs        ProjectRegistry: ~/.open-mpm/projects.json
├── process_tracker.rs ProcessTracker: .open-mpm/processes.json
├── bus.rs             MessageBus: inter-project UNIX socket pub/sub
├── ctrl/              CTRL REPL: multi-project PM session management
├── ticketing/         TicketingClient trait + GitHub/JIRA/Linear implementations
├── compress/          Session history compactor (sliding window)
└── cli/               CLI subcommands (memory search, code search)
```

---

## Component Responsibilities

### CTRL REPL (`src/ctrl`)

The default mode when no flags are given. Provides an interactive terminal
session that can manage multiple PM actors scoped to different project paths.
Routes user input to the right PM actor and displays formatted responses.
Connected to the inter-project `MessageBus` for cross-project coordination.

### PM Orchestrator (`src/main.rs::run_pm`)

Reads one line of user input, calls the PM LLM (`pm.toml` model) with the
`delegate_to_agent` tool registered, and dispatches any tool calls to the
`ToolRegistry`. The production registry has a single tool: `DelegateToAgentTool`,
which spawns a sub-agent subprocess and forwards the task over NDJSON IPC.

### Sub-Agent Process (`src/main.rs::run_subagent`)

Started as `current_exe --agent <name>`. Reads one NDJSON `Task` from stdin.
Builds an effective system prompt via `SystemPromptBuilder` (base prompt +
CLAUDE.md ancestor walk + resolved skills). Selects a tool registry based on
the agent name. Runs either a single-shot `llm::chat` (no tools) or a
multi-turn `llm::chat_with_tools_gated` loop. Emits one NDJSON `Result` or
`Error` to stdout and exits.

### Workflow Engine (`src/workflow/engine.rs`)

The `WorkflowEngine` owns the phase loop. For each `PhaseDef` in the loaded
`WorkflowDef`:

1. Renders the `context_template` against the accumulated `WorkflowContext`
   (substituting `{{task}}`, `{{out_dir}}`, `{{phase_name}}` etc.)
2. Runs the phase agent via the configured `AgentRunner`
3. If `produces_files: true`, extracts `## File: <path>` sections from the
   output and writes them under `out_dir`
4. If `parallel_subtasks` is set, dispatches each subtask concurrently
5. If `worktree_protection: true`, each parallel subtask gets its own git
   worktree via `WorktreeManager`
6. If `assignments.json` is found in `out_dir`, runs the wave loop: one
   sub-agent per file assignment, in topological wave order

Optionally wraps each run with `TicketManager` GitHub issue lifecycle calls
and `PerfCollector` token/latency telemetry.

### Tool Registry (`src/tools/mod.rs`)

`ToolRegistry` holds `Arc<dyn ToolExecutor>` entries keyed by `tool.name()`.
The PM loop and sub-agent tool loop both route through it:

- `register(tool)`: adds a tool; panics on duplicate names in debug builds
- `dispatch(name, args)`: calls the named tool, returns `ToolResult`
- `dispatch_gated(name, args, allowed)`: applies a per-agent allowlist before
  dispatching
- `openai_tools()`: converts registered schemas to async-openai typed values

### LLM Client (`src/llm/mod.rs`)

Two primary call paths:

- `llm::chat(...)`: single system+user request, no tools. Used by simple agents.
- `llm::chat_with_tools_gated(...)`: multi-turn loop. Dispatches tool calls
  concurrently via `FuturesUnordered`, injects tool-discipline reminders on
  plain-text turns, respects `max_turns`, `tool_choice`, and `finish_task`.

Three HTTP backends:

- `async-openai` typed builder (default, OpenRouter-compatible)
- Raw `reqwest` POST (when `cache_control` injection or `tool_choice` override
  requires fields that async-openai 0.28 cannot represent)
- Native Anthropic `/v1/messages` (when `use_anthropic_direct = true` and
  model adapter returns `Provider::Anthropic`)

### NDJSON IPC (`src/ipc/mod.rs`)

Protocol between PM and sub-agent processes. Each message is a single JSON
object terminated by `\n`. Three variants:

```
PM -> sub-agent:     {"type":"task",   "id":"<uuid>", "task":"...", "history":[...]}
sub-agent -> PM:     {"type":"result", "id":"<uuid>", "content":"...", "summary":"...", "usage":{...}}
sub-agent -> PM:     {"type":"error",  "id":"<uuid>", "error":"...", "status":"error"}
```

The `ipc` module also exports `extract_files_from_content` which parses
`## File: <path>` sections from LLM output into `(PathBuf, String)` pairs.

### Model Adapter (`src/llm/adapter.rs`)

`ModelAdapter` trait abstracts over provider-specific wire-level differences:

- `tool_choice_any()` / `tool_choice_auto()`: provider-appropriate JSON values
- `inject_cache_control(raw, active)`: patches Anthropic `cache_control` fields
- `parse_usage(json)`: extracts `TokenUsage` including Anthropic cache fields
- `api_endpoint(use_direct)`: resolves base URL, auth header name/value, extra
  headers

Concrete implementations: `AnthropicAdapter`, `OpenAIAdapter`, `GenericAdapter`.

---

## State and Storage Layout

### Per-project (in project CWD)

```
.open-mpm/
├── build.json            monotonic build counter (incremented on every start)
├── sessions/<run_id>/    per-invocation turn logs
├── worktrees/            git worktrees for parallel wave-loop phases
├── history/              HistoryIndexer turn log (vector-searchable)
├── code/                 redb+usearch code index (tree-sitter chunks)
└── processes.json        tracked sub-agent PIDs
```

### Global (per-user)

```
~/.open-mpm/
├── projects.json          global project registry
├── sockets/<project>.sock MessageBus UNIX sockets
├── skills/files/          globally-shared skill Markdown
└── memory/                shared memory stores
```

---

## Startup Sequence

On every invocation `main()` executes in order:

1. Check `--version` (fast path, no env/tracing needed)
2. Load `.env.local` and `.env` via dotenvy
3. Initialize tracing (stderr, env-filter)
4. Increment build counter, emit banner
5. Set `OPEN_MPM_RUN_ID` (UUID, inherited by sub-agents)
6. Migrate legacy memory layout if needed
7. Clean up stale worktrees from prior interrupted runs
8. Register the project in the global `ProjectRegistry`
9. Clean up stale sub-agent PIDs via `ProcessTracker`
10. Start the inter-project `MessageBus`
11. Dispatch to the requested execution mode

---

## Skill Injection

Skills are Markdown files that provide domain knowledge to agents. Discovery
priority (highest first):

1. Project `.claude/skills/` (claude-mpm compatible)
2. `~/.claude/skills/` (user global, claude-mpm compatible)
3. `~/.open-mpm/skills/files/` (open-mpm global)
4. `config/skills/` (project-local)

The `SystemPromptBuilder` layers skills after the base system prompt:

```
[base system prompt from TOML]
[CLAUDE.md instructions from CWD ancestors]
[# Skill: <name>\n\n<content> for each resolved skill]
```

---

## Memory and Code Index

The local code index (`src/memory`, `src/search`) uses:

- **redb**: embedded key-value store for chunk metadata and content
- **usearch**: in-process ANN (approximate nearest-neighbor) index
- **fastembed**: local sentence-transformer embeddings (384-dim, no API call)
- **tree-sitter**: language-aware AST parsing for accurate chunking

Language support: Rust, Python, TypeScript/JavaScript, Go, Markdown.

Search is exposed via:
- `code search <query>` CLI subcommand
- `VectorSearchTool` and `SearchCodeTool` callable by agents
- `MemorySearchTool`: hybrid vector + BM25 over the history turn log
