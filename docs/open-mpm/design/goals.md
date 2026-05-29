# Project Goals

## What open-mpm is

A lightweight, composable AI agent orchestration harness in Rust that:

- Accepts user requests via a CTRL REPL or HTTP API
- Routes work to a PM (Project Manager) orchestrator process
- Delegates tasks to specialized sub-agents (e.g. `python-engineer`,
  `research-agent`) via subprocess IPC
- Calls LLMs via OpenRouter, the direct Anthropic API, or the local
  `claude` CLI — using OpenAI-compatible function calling
- Loads agent definitions from TOML config files
- Injects skill context from Markdown files into agent prompts
- Validates the architecture against real coding tasks
  (`ai-coding-bake-off` challenges)

It is analogous to `claude-mpm` but implemented in Rust for performance,
safety, and as a learning exercise in building agentic systems.

## What open-mpm is not

- Not a hosted service — it's a binary you run locally
- Not a replacement for an IDE — it's an orchestrator that delegates to
  agents which can read/write files in a sandboxed `out_dir`
- Not a chatbot — every run produces durable artifacts (files,
  performance JSON, audit logs)
- Not a model — it depends on third-party LLM APIs
- Not a vector database — local memory uses lightweight redb+usearch+
  fastembed for code search and a small TF-IDF index for docs search;
  no external services required

## Design principles

### 1. Single binary, multiple modes

One `open-mpm` executable handles every mode: CTRL REPL, PM orchestrator,
sub-agent runner, workflow engine, HTTP API server, CLI subcommands.
Modes dispatch from `main.rs` based on argv. The web UI is embedded via
`rust-embed`. Deployment is a single file copy.

### 2. Subprocess isolation

Each sub-agent invocation is a short-lived `current_exe --agent <name>`
subprocess. This trades some per-call overhead for:

- Predictable resource bounds (the OS reaps the process)
- Trivial state isolation (each sub-agent starts fresh)
- Crash containment (a buggy agent can't take down the orchestrator)
- Easy parallelism (just spawn more processes)

### 3. NDJSON IPC

PM ↔ sub-agent communication uses newline-delimited JSON over stdin/stdout.
One message per line, three variants: `task`, `result`, `error`. No
binary protocols, no shared memory, no RPC framework. Easy to debug
(`cat | tee | jq`).

### 4. Configuration as code

Agents are TOML files. Skills are Markdown files. Workflows are JSON files.
All committed to the repository. No databases for definitions; the
filesystem is the source of truth.

### 5. Composition over inheritance

Agents don't inherit from each other. Skills are mixed into agent prompts
declaratively (`[skills].include = ["..."]`). Tools register into a
flat `ToolRegistry` and are gated per-agent via allowlists.

### 6. Observability by default

Every workflow run emits:

- Per-phase performance JSON to `docs/performance/runs/`
- Tracing logs to stderr (filterable via `RUST_LOG`)
- Progress events streamed as `__OMPM_PROGRESS__ {…}` lines
- Persistent turn logs to `.open-mpm/state/sessions/<run_id>/`
- A monotonic build counter that correlates runs to source revisions

### 7. Search-first development

The harness includes:

- A code index (tree-sitter + redb + usearch + fastembed) for semantic
  code search
- A TF-IDF docs index over `docs/` for natural-language doc search
- A turn-log index for "what did the PM say last week?" queries

These are in-process, dependency-free at runtime, and fast enough to
build at startup.

### 8. Fail loudly, recover gracefully

- Compile-time guarantees come first (Rust's type system, `Result<T, E>`)
- Runtime errors propagate via `anyhow` with full context chains
- The CTRL Taskmaster persona retries failed phases with adjusted context
  before escalating to the user
- Background tasks (docs index build, file watcher) never block the REPL

### 9. Local-first, network-second

- Memory and code indexes are local (no cloud sync)
- LLM calls are the only network dependency
- The web UI is served from the same binary; no CDN
- Inter-project messaging uses UNIX sockets, not TCP

### 10. Don't ship features that aren't tested

CI runs ~700 tests on every commit. New features land with their tests.
Live-API integration tests gate on env vars so the suite passes without
credentials.

## Trade-offs taken

| We chose | Over | Because |
|---|---|---|
| Subprocess per agent | Long-lived in-process actors | Isolation + crash containment |
| NDJSON over stdio | gRPC / message queue | Debuggability, zero deps |
| TF-IDF for docs | Sentence embeddings | Zero model downloads, fast startup |
| `redb`+`usearch` | External vector DB (Pinecone, Weaviate) | Zero ops, embedded |
| Rust | Python (claude-mpm) | Type safety, performance, single binary |
| OpenRouter default | Direct provider APIs | One credential covers most agents |
| CLAUDE.md ancestor walk | Project-config DSL | Compatible with claude-mpm |
| Embedded web UI | External SPA deploy | Single-binary distribution |

## What "done" looks like for v1

- ✅ Single binary that runs every mode
- ✅ TOML agents, Markdown skills, JSON workflows
- ✅ OpenRouter / direct Anthropic / claude-CLI backends
- ✅ Workflow engine with phase loop, wave loop, parallel subtasks,
  worktree protection
- ✅ HTTP API server with embedded web UI
- ✅ Local code index + memory graph + docs search
- ✅ CTRL multi-project Taskmaster
- ✅ Mistake log + postmortem agent
- ⏳ Polished release pipeline (cargo install, GitHub releases, Homebrew)
- ⏳ Cross-platform CI (currently macOS-only)
- ⏳ User-friendly error messages for common misconfigurations

## Non-goals

- Not building a model
- Not building a hosted SaaS
- Not building an IDE plugin
- Not aiming for feature parity with claude-mpm (the Python harness)
- Not aiming to support every LLM provider — OpenRouter handles 200+
  through one interface
