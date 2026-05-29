# open-mpm

Rust-based AI agent orchestration harness. PM orchestrator + subprocess delegation + tool calling via OpenRouter.

> **Coordination:** Shared library patterns, consistent conventions, and CI/CD configuration for this project are managed by [trusty-common](../trusty-common). See that repo's CLAUDE.md for cross-project guidelines.

## Project Goals

Build a lightweight, composable AI agent harness in Rust that:
- Accepts user requests via a PM (Project Manager) orchestrator process
- Delegates tasks to specialized sub-agents (e.g., python-engineer) via subprocess IPC
- Calls LLMs via OpenRouter using OpenAI-compatible function calling
- Loads agent definitions from TOML config files
- Injects skill context from Markdown files into agent prompts
- Validates the architecture against real coding tasks (ai-coding-bake-off challenges)

This is analogous to claude-mpm but implemented in Rust for performance, safety, and as a learning exercise in building agentic systems.

## Architecture

```
User Input
    |
    v
PM Orchestrator (main process)
    |  reads: .open-mpm/agents/pm.toml
    |  LLM: OpenRouter (claude-sonnet-4-6)
    |  tool: delegate_to_agent(agent_name, task)
    |
    v
Sub-Agent Process (spawned via tokio::process::Command)
    |  reads: .open-mpm/agents/<name>.toml
    |  IPC: NDJSON over stdin/stdout
    |  LLM: OpenRouter (model from agent config)
    |
    v
OpenRouter API --> LLM response --> result returned to PM --> user
```

### Key Components

- **PM Orchestrator** (`src/main.rs`, `src/agents/`): Main process. Reads user input, calls LLM with tool definitions, receives `delegate_to_agent` tool calls, spawns sub-agent processes.
- **Sub-Agent Process** (`src/agents/`): Spawned per-task. Receives task via NDJSON on stdin, calls LLM, returns result via NDJSON on stdout, then exits.
- **NDJSON IPC** (`src/ipc/`): Newline-delimited JSON protocol for PM <-> sub-agent communication. Each message is a single JSON object on one line.
- **Tool Calling** (`src/tools/`): OpenAI function-calling format. PM uses `delegate_to_agent` tool. Sub-agents may use additional tools.
- **Agent Config** (`.open-mpm/agents/*.toml`): Defines agent name, role, model, LLM parameters, and system prompt.
- **Skills** (`.open-mpm/skills/*.md`): Markdown files injected into agent system prompts for domain knowledge.

### IPC Message Format

PM to sub-agent (via stdin):
```json
{"type": "task", "id": "uuid", "task": "Write a Python script that..."}
```

Sub-agent to PM (via stdout):
```json
{"type": "result", "id": "uuid", "content": "```python\n...\n```", "status": "success"}
```

Error format:
```json
{"type": "error", "id": "uuid", "error": "description", "status": "error"}
```

## Stack

- **Language**: Rust (2021 edition — note: Cargo.toml uses `edition = "2024"` which maps to Rust 2024)
- **Async runtime**: tokio (full features)
- **LLM client**: async-openai 0.28 (configured to use OpenRouter base URL)
- **IPC**: NDJSON over stdin/stdout via tokio-util codec + bytes
- **Serialization**: serde + serde_json
- **Agent config**: TOML files parsed with the `toml` crate
- **Error handling**: anyhow (application-level), thiserror (library errors)
- **Logging**: tracing + tracing-subscriber with env-filter
- **Env vars**: dotenvy (loads `.env.local`)
- **IDs**: uuid v4 for message correlation

## Key Conventions

### Agent Definition Format (TOML)

```toml
[agent]
name = "python-engineer"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "Python software engineer"

[llm]
temperature = 0.2
max_tokens = 8192

[system_prompt]
content = """
Your system prompt here.
"""
```

### OpenRouter Configuration

The async-openai client is configured with:
- Base URL: `https://openrouter.ai/api/v1`
- API key: from `OPENROUTER_API_KEY` env var (loaded from `.env.local`)
- Model names: use OpenRouter format, e.g. `anthropic/claude-sonnet-4-6`

### Tool Calling Convention

Tools follow OpenAI function-calling JSON schema format. The PM defines:
```json
{
  "name": "delegate_to_agent",
  "description": "Delegate a task to a specialized sub-agent",
  "parameters": {
    "type": "object",
    "properties": {
      "agent_name": {"type": "string"},
      "task": {"type": "string"}
    },
    "required": ["agent_name", "task"]
  }
}
```

### Subprocess IPC Pattern

Sub-agents are spawned with:
- `stdin`: piped (PM writes task JSON)
- `stdout`: piped (PM reads result JSON)
- `stderr`: inherited (for logging visibility)

Use separate tokio tasks for reading stdout and writing stdin to prevent deadlock.

## Development

### Prerequisites

- Rust stable (1.80+)
- `.env.local` file in project root with:
  ```
  OPENROUTER_API_KEY=sk-or-v1-...
  ```

### Build and Run

```bash
# Check compilation
cargo check

# Build debug
cargo build

# Run PM orchestrator
cargo run

# Run with logging
RUST_LOG=debug cargo run

# Run tests
cargo test

# Build release
cargo build --release
```

### REPL Testing (mandatory for any ctrl/repl/banner changes)

Before committing changes to `src/repl/`, `src/ctrl/`, or `src/main.rs`, run the
interactive tmux e2e test:

```bash
./scripts/tmux-repl-test.sh
```

This test spawns a real tmux session, launches the REPL in it, verifies the banner
renders correctly, and sends a chat message to confirm the LLM integration is live.
Pipe-based tests (`echo | cargo run`) do NOT substitute for this — they miss terminal
rendering bugs, cursor issues, and async timing problems.

### Environment Variables

Credential priority (highest → lowest, applied per-agent at dispatch time — see #249):

1. **`CLAUDE_CODE_OAUTH_TOKEN`** — primary local-dev credential. When this is set
   AND the agent's TOML declares `runner = "claude-code"`, the harness routes the
   call through the `claude` CLI subprocess (`ClaudeCodeAgentRunner`). This is
   the default for `pm`, `engineer`, `qa-agent`, `ticketing-agent`, `ctrl`, etc.
2. **`ANTHROPIC_API_KEY`** — when set AND the agent TOML has
   `[llm] use_anthropic_direct = true`, the harness POSTs directly to
   `api.anthropic.com` with `x-api-key`. Lower latency than OpenRouter; requires
   a paid console.anthropic.com key (NOT the OAuth token).
3. **`OPENROUTER_API_KEY`** — preserved as the deployment / CI fallback. Used
   for any agent that doesn't qualify for paths 1 or 2 (e.g. OpenAI / Bedrock
   models, or when no OAuth token is present in CI).

| Variable | Description |
|---|---|
| `CLAUDE_CODE_OAUTH_TOKEN` | OAuth token from `claude setup-token` (sk-ant-oat01-*). Primary local-dev credential. Only valid for `runner = "claude-code"` agents (ClaudeCodeAgentRunner). NOT used for `use_anthropic_direct = true` — api.anthropic.com rejects these tokens with 401. |
| `ANTHROPIC_API_KEY` | Direct Anthropic API key from console.anthropic.com (sk-ant-api03-*). Required when `use_anthropic_direct = true`. |
| `OPENROUTER_API_KEY` | OpenRouter API key (sk-or-v1-*). Deployment / CI fallback for agents not covered by the OAuth or direct-Anthropic paths. |
| `RUST_LOG` | Log level: `trace`, `debug`, `info`, `warn`, `error` |

### Using Direct Anthropic API

To call `api.anthropic.com` directly (lower latency, no OpenRouter markup):

1. Get an API key from console.anthropic.com
2. Add to `.env.local`: `ANTHROPIC_API_KEY=sk-ant-api03-...`
3. In agent TOML, set: `[llm] use_anthropic_direct = true`

### Using runner = "claude-code" agents

`CLAUDE_CODE_OAUTH_TOKEN` (sk-ant-oat01-* tokens from `claude setup-token`) is ONLY
valid for agents with `runner = "claude-code"`. These agents call Claude via the
`claude` CLI subprocess (ClaudeAgentRunner), not via the REST API directly.
Do NOT set `use_anthropic_direct = true` with OAuth tokens — Anthropic's REST API
returns 401 for OAuth tokens.

### Project Structure

```
open-mpm/
├── CLAUDE.md               # This file
├── Cargo.toml
├── .env.local              # API keys (not committed)
├── .open-mpm/              # Bundled config (committed) + runtime state
│   ├── agents/             # Agent TOML configs (pm.toml, python-engineer.toml, …)
│   ├── skills/             # Skill markdown files
│   ├── workflows/          # Workflow JSON definitions
│   ├── tasks/              # Bake-off task files (level-1.txt … level-5.txt)
│   ├── agent-templates/    # Starter templates for user-authored agents
│   └── state/              # Runtime state (gitignored): build.json, history/,
│                           # initialized, project-index.md, processes.json,
│                           # code/, sessions/, worktrees/
└── src/
    ├── main.rs             # Entry point + PM loop
    ├── agents/             # Agent loading + subprocess spawning
    ├── ipc/                # NDJSON IPC protocol
    └── tools/              # Tool definitions + dispatch
```

Note: The project previously used a separate `config/` directory for bundled
config; it has been folded into `.open-mpm/` (v0.1.25) so the harness's own
config follows the same layout users adopt in their projects. Runtime state
lives under `.open-mpm/state/` (gitignored) to keep it out of commits.

Note: Crate documentation (research, design, spec, user, developer docs) now
lives in the repo-level tree at `docs/open-mpm/` rather than in-crate. See
`docs/open-mpm/README.md` for the index.

## POC Status

**Phase**: Initial scaffolding. Cargo.toml dependencies added, agent configs created, main.rs stub in place.

**Working**:
- Project compiles (`cargo check` passes)
- `.env.local` loaded via dotenvy
- tracing initialized
- Agent TOML configs defined

**Next Steps**:
1. Implement NDJSON codec in `src/ipc/`
2. Implement agent config loader in `src/agents/`
3. Wire up async-openai client pointed at OpenRouter
4. Implement PM message loop with tool calling
5. Implement sub-agent process spawning with IPC
6. Run POC against Level 1 bake-off challenge

## Test Cases

### Level 1: Markdown Table Formatter (ai-coding-bake-off)

User prompt to PM:
> "Write a Python script that formats data as a markdown table"

Expected flow:
1. PM receives request
2. PM calls LLM, gets `delegate_to_agent("python-engineer", "Write a Python script that takes tabular data and outputs a formatted markdown table")`
3. PM spawns `python-engineer` sub-agent process
4. Sub-agent sends task JSON to sub-agent stdin
5. Sub-agent calls OpenRouter LLM, generates Python script
6. Sub-agent returns result JSON on stdout
7. PM reads result, returns to user

Reference: `docs/open-mpm/research/bake-off-challenges.md`

## Research

All research docs live in the repo-level tree at `docs/open-mpm/research/`
(indexed by `docs/open-mpm/research/README.md`). Highlights:

| File | Topic |
|---|---|
| `rust-ai-frameworks.md` | Evaluated Rust LLM client libraries |
| `subprocess-ipc-patterns.md` | NDJSON IPC design, deadlock prevention |
| `agent-delegation-patterns.md` | PM orchestrator + sub-agent patterns |
| `openrouter-api.md` | OpenRouter API configuration for async-openai |
| `openai-plans.md` | Notes on OpenAI function-calling format |
| `bake-off-challenges.md` | Test case definitions |
