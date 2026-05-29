# open-mpm

A lightweight, composable AI agent orchestration harness in Rust.

`open-mpm` runs a **PM (Project Manager) orchestrator** that delegates tasks
to specialized **sub-agent subprocesses** via NDJSON IPC. Each sub-agent
calls an LLM (OpenRouter, Anthropic direct, or the `claude` CLI) and streams
results back. It's analogous to `claude-mpm` in spirit, but implemented in
Rust for performance, memory safety, and to serve as a proving ground for
agentic system design.

## What it is

- **PM orchestrator**: a long-running process that reads user input, calls an
  LLM with a `delegate_to_agent` tool, and routes work to sub-agents.
- **Sub-agent subprocesses**: spawned per-task. Read a task JSON on stdin,
  emit a result JSON on stdout, exit.
- **Tool-using agents**: sub-agents can run multi-turn tool-calling loops
  (`read_file`, `shell_exec`, `write_file`, `web_search`, `load_skill`, etc.).
- **Skill injection**: Markdown skill files get composed into agent system
  prompts for domain-specific knowledge.
- **Workflow engine**: declarative phase-based runs for complex tasks
  (design → plan → implement → review → verify).
- **Token compression**: sliding-window session history compaction so long
  conversations stay within model context limits.
- **CTRL CLI**: interactive REPL that manages multiple PM sessions across
  different project directories with inter-project message bus.

## Architecture

```
                    ┌──────────────────────────────┐
                    │          User (stdin)         │
                    └───────────────┬──────────────┘
                                    │
                                    ▼
                    ┌──────────────────────────────┐
                    │       CTRL CLI (--ctrl)      │
                    │   multi-project dispatcher   │
                    └───────────────┬──────────────┘
                                    │
                                    ▼
                    ┌──────────────────────────────┐
                    │      PM[project] actor       │
                    │  LLM + delegate_to_agent     │
                    └───────────────┬──────────────┘
                                    │  spawn(subprocess)
                                    ▼
                 ┌────────────────────────────────────┐
                 │  Sub-agent (--agent <name>)        │
                 │  NDJSON in → LLM + tools → NDJSON  │
                 └───────────────┬────────────────────┘
                                 │
                                 ▼
                    ┌──────────────────────────────┐
                    │   OpenRouter / Anthropic API │
                    │    (or local claude CLI)     │
                    └──────────────────────────────┘
```

## Quick start

### Prerequisites

- Rust stable 1.80+ (the crate uses edition 2024)
- One of:
  - `OPENROUTER_API_KEY` for default OpenRouter routing, or
  - `ANTHROPIC_API_KEY` for direct Anthropic API calls (when `use_anthropic_direct = true`)
  - `CLAUDE_CODE_OAUTH_TOKEN` for agents configured with `runner = "claude-code"`

### Setup

```bash
# Clone and configure
git clone <repo-url> open-mpm
cd open-mpm

# Create .env.local with your API keys
cat > .env.local <<'EOF'
OPENROUTER_API_KEY=sk-or-v1-...
# Optional: direct Anthropic
# ANTHROPIC_API_KEY=sk-ant-api03-...
# Optional: claude-code runner agents
# CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-...
EOF

# Build and check
make build
make test
```

### Run

```bash
# Interactive CTRL REPL
cargo run -- --ctrl

# One-shot PM from stdin
echo "Write a markdown table formatter in Python" | cargo run

# Workflow with a task file
cargo run -- --workflow prescriptive --task-file ./my-task.md --out-dir ./out/run1

# Direct sub-agent invocation (bypass PM)
cargo run -- --direct python-engineer --task-file ./my-task.md

# Show version and bump build counter
cargo run -- --version
```

## Module map

| Module            | Purpose                                                    |
|-------------------|------------------------------------------------------------|
| `src/main.rs`     | Entry point; argv dispatch to PM / sub-agent / workflow.   |
| `src/ctrl`        | Interactive REPL managing multiple per-project PM actors.  |
| `src/agents`      | Agent config loading, prompt builder, runners.             |
| `src/workflow`    | Multi-phase workflow engine (prescriptive, wave, etc.).    |
| `src/tools`       | LLM-callable tools (delegate, read/write files, shell, …). |
| `src/ipc`         | NDJSON codec for stdin/stdout subprocess IPC.              |
| `src/llm`         | async-openai client configured for OpenRouter/Anthropic.   |
| `src/compress`    | Session history compaction under context-window budgets.   |
| `src/bus`         | Inter-project message bus over UNIX sockets.               |
| `src/registry`    | Global project registry (`~/.open-mpm/projects.json`).     |
| `src/skills`      | Skill discovery and composition into agent prompts.        |
| `src/memory`      | Local redb + usearch + fastembed vector store.             |
| `src/search`      | Code indexer and file watcher (tree-sitter).               |
| `src/init`        | Project self-initialization and auto-index seeding.        |
| `src/build_info`  | Persistent build counter + version string.                 |
| `src/session`     | Per-run session directory management.                      |
| `src/subprocess`  | Spawn and NDJSON-interact with sub-agent processes.        |
| `src/perf`        | Performance telemetry stamps.                              |
| `src/cli`         | `memory search` / `code search` sub-commands.              |

## CLI flags

| Flag                         | Description                                                  |
|------------------------------|--------------------------------------------------------------|
| `--ctrl`                     | Start the interactive CTRL REPL (multi-project dispatcher). |
| `--pm`                       | Force PM mode (default when no other flags supplied).       |
| `--agent <name>`             | Sub-agent mode: read one NDJSON task from stdin.            |
| `--direct <name>`            | Bypass PM, send task-file straight to sub-agent.            |
| `--workflow <name>`          | Run `.open-mpm/workflows/<name>.json` phase by phase.       |
| `--task <text>`              | Inline task string (used with `--direct` / `--workflow`).   |
| `--task-file <path>`         | Read task from file (used with `--direct` / `--workflow`).  |
| `--out-dir <dir>`            | Per-run output directory (sandboxes `write_file`).          |
| `--reindex`                  | Rebuild the local code index and exit.                      |
| `--watch`                    | Run the file-watcher / live indexer in the foreground.      |
| `--check-orphans`            | Print stores/sessions with no matching project entry.       |
| `--clear-sessions`           | Clear persistent agent session history.                     |
| `--reinit`                   | Re-run project initialization / agent seeding.              |
| `--version`, `-V`            | Print `open-mpm vX.Y.Z (<git-hash>)` and exit.              |

## Config reference

### Agent TOML (`.open-mpm/agents/*.toml`)

```toml
[agent]
name = "python-engineer"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "Python software engineer"
# Optional: "subprocess" (default) or "claude-code"
# runner = "claude-code"

[llm]
temperature = 0.2
max_tokens = 8192
# Optional: call api.anthropic.com directly instead of OpenRouter
# use_anthropic_direct = true

[system_prompt]
content = """
You are a Python engineer. …
"""

# Optional: restrict which tools this agent may call
# [tools]
# allowed = ["read_file", "write_file", "shell_exec"]
```

### Skill Markdown (`.open-mpm/skills/*.md`)

Skill files are plain Markdown documents loaded via the `load_skill` tool or
composed into agent system prompts. Frontmatter (YAML-style) is optional:

```markdown
---
name: tdd-workflow
tags: [testing, tdd]
---

# Test-Driven Development

When implementing a feature…
```

## Runner types

Set `runner` in the `[agent]` section of the TOML:

- `"subprocess"` (default): Spawn the same binary with `--agent <name>`. Talks
  to the configured LLM over HTTP via `async-openai`.
- `"claude-code"`: Shell out to the local `claude` CLI. Requires
  `CLAUDE_CODE_OAUTH_TOKEN` (generated via `claude setup-token`). Supports the
  `--model` flag for per-invocation model override.

## Token compression

`src/compress` implements a sliding-window compactor so the PM and long-lived
sub-agents can keep conversations within model context limits.

- `CompressConfig` declares target/max token budgets, preserved head/tail
  turns, and the summarizer model.
- Session histories persist under `.open-mpm/sessions/<run_id>/` and are
  compacted on write when they exceed the budget.
- Compaction replaces middle turns with a single summary message while
  preserving tool-call/tool-result adjacency rules required by the API.

## Global infrastructure

Open-mpm maintains per-user state outside the project tree:

```
~/.open-mpm/
├── projects.json         # global project registry (path, last_seen, …)
├── processes.json        # running PM/sub-agent PIDs for coordination
├── sockets/              # UNIX sockets for inter-project MessageBus
│   └── <project-id>.sock
├── skills/               # shared skill markdown (project skills shadow these)
└── memory/               # shared vector/memory stores
```

Per-project state lives in `<project>/.open-mpm/`:

```
.open-mpm/
├── build.json            # monotonic build counter
├── sessions/<run_id>/    # per-invocation session logs
├── worktrees/            # wave-loop git worktrees
├── store/                # code index (redb + usearch)
└── out/                  # workflow output (write_file sandbox)
```

## Environment variables

| Variable                    | Description                                                           |
|-----------------------------|-----------------------------------------------------------------------|
| `OPENROUTER_API_KEY`        | OpenRouter API key. Required for default routing.                    |
| `ANTHROPIC_API_KEY`         | Direct Anthropic API key. Required when `use_anthropic_direct=true`. |
| `CLAUDE_CODE_OAUTH_TOKEN`   | OAuth token (`claude setup-token`). Only valid for `runner="claude-code"` agents. |
| `RUST_LOG`                  | Log level: `trace`, `debug`, `info`, `warn`, `error`.                |
| `OPEN_MPM_CONFIG_DIR`       | Override for `.open-mpm/agents/` lookup (defaults to repo-relative). |
| `OPEN_MPM_OUT_DIR`          | Default per-run output root when `--out-dir` is omitted.             |
| `OPEN_MPM_RUN_ID`           | Set automatically; inherited by sub-agents to share a `run_id`.      |

> **Note**: `CLAUDE_CODE_OAUTH_TOKEN` (sk-ant-oat01-*) is **not** accepted by
> `api.anthropic.com` directly. Don't combine it with `use_anthropic_direct = true`.

## Development

```bash
# Standard flow
make check           # cargo check
make test            # cargo test
make clippy          # cargo clippy --all-targets -- -D warnings
make fmt             # cargo fmt
make lint            # clippy + fmt

# Run modes
make ctrl                                  # interactive REPL
make run-task TASK_FILE=./my-task.md       # prescriptive workflow
make release                               # optimized build
make version                               # print semver from Cargo.toml
make clean                                 # cargo clean
```

The `build.rs` script captures the short git SHA at compile time and exposes
it as `build_info::GIT_HASH`. Combined with `build_info::VERSION`, the
`version_string()` function renders `open-mpm vX.Y.Z (<sha>)` and is shown in
the CTRL banner and `--version` output.

## Status

**POC phase.** What works today:

- Multi-mode binary dispatch (PM / sub-agent / workflow / direct / CTRL).
- NDJSON IPC with proper stdout/stderr separation.
- Tool registry with OpenAI-compatible schemas and per-agent allowlists.
- Atomic `write_file` tool with `out_dir` sandboxing + optional `allowed_path`.
- Format translators (Markdown→HTML, JSON↔TOML, YAML→JSON) behind a
  pluggable trait.
- OpenRouter + direct Anthropic + `claude` CLI runner backends.
- Token compression and per-run session logs.
- Local code index (tree-sitter + fastembed) and inter-project bus.
- Workflow engine with prescriptive and wave modes, including git worktrees
  for parallel phases.

**Next up:**

1. Broader skill ecosystem and a `load_skill` registry.
2. Richer workflow primitives (conditional branches, retries).
3. More format translators and content-aware write_file heuristics.
4. Operational tooling: `/status` metrics, traceability across PM↔sub-agents.

See `CLAUDE.md` for architectural detail.

## 📚 Documentation

Design, research, specification, user, and developer docs live in the
repo-level `docs/open-mpm/` tree — not in this crate. Start at
[`docs/open-mpm/README.md`](../../docs/open-mpm/README.md) (the master index),
or jump straight to:

- [`docs/open-mpm/spec/`](../../docs/open-mpm/spec/) — canonical PRD, ARCHITECTURE, COMPONENTS.
- [`docs/open-mpm/research/`](../../docs/open-mpm/research/) — investigation & design notes.
- [`docs/open-mpm/decisions/`](../../docs/open-mpm/decisions/) — crate-specific ADRs.
- [`docs/adr/`](../../docs/adr/) — workspace-wide architecture decisions.
