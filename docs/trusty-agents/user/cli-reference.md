# CLI Reference

`open-mpm` is a single binary that dispatches based on flags.

```
open-mpm [FLAGS]
```

## Mode flags (mutually exclusive)

| Flag | Description |
|---|---|
| `--ctrl` | Interactive multi-project session manager (also the default when no mode flag is set) |
| `--pm` | Single-shot PM orchestrator; reads one line from stdin |
| `--agent <name>` | Sub-agent runner; reads one NDJSON Task from stdin, emits one Result, exits |
| `--direct <name>` | Bypass PM LLM, send task to sub-agent directly |
| `--workflow <name>` | Run `.open-mpm/workflows/<name>.json` |
| `--api` (alias `--serve`) | Launch HTTP API server + embedded web UI |
| `--reindex` | Full re-index of the working tree, then exit |
| `--watch` | Live filesystem watcher; keeps the code index in sync, blocks until killed |
| `--version` / `-V` | Print version + git hash, exit |

## Task input flags

Used with `--direct` and `--workflow`.

| Flag | Description |
|---|---|
| `--task <text>` | Inline task string |
| `--task-file <path>` | Read task from file |
| `--out-dir <dir>` | Sandbox for `write_file` tool calls and file extraction |
| `--json` | Emit a single `PmResponse` JSON envelope on stdout (machine-readable) |

## API mode flags

Used with `--api` / `--serve`.

| Flag | Description |
|---|---|
| `--port <N>` | TCP port (default `8080`) |
| `--api-token <TOK>` | Require this bearer token on every `/api/*` request (except `/api/health` and `/api/config`). Falls back to `OPEN_MPM_API_TOKEN` env var |

## Diagnostic / maintenance flags

| Flag | Description |
|---|---|
| `--check-orphans` | Print tracked sub-agent PIDs and their live status |
| `--clear-sessions` | Clear in-memory agent session history |
| `--reinit` | Force project re-initialization and memory seeding |

## Subcommands

```
open-mpm code search "<query>"     # Search the local code index
open-mpm memory search "<query>"   # Search the history/turn-log index
open-mpm agents list               # List available agents
open-mpm skills list               # List discoverable skills
open-mpm skills sources            # Show skill discovery directories
open-mpm postmortem [--last N | --session <id>]
open-mpm postmortem --tag <tag>
```

## Environment variables

| Variable | Description |
|---|---|
| `OPENROUTER_API_KEY` | OpenRouter API key (default routing for most agents) |
| `ANTHROPIC_API_KEY` | Direct Anthropic API key (for agents with `use_anthropic_direct = true`) |
| `CLAUDE_CODE_OAUTH_TOKEN` | OAuth token from `claude setup-token` (only for agents with `runner = "claude-code"`) |
| `BRAVE_API_KEY` | Optional — enables `web_search` tool |
| `OPEN_MPM_API_TOKEN` | Default bearer token for `--api` mode |
| `RUST_LOG` | `trace`, `debug`, `info`, `warn`, `error` (default: `info`) |
| `OPEN_MPM_CONFIG_DIR` | Override for `.open-mpm/agents/` lookup path |
| `OPEN_MPM_OUT_DIR` | Default output root when `--out-dir` is omitted |
| `OPEN_MPM_RUN_ID` | Auto-set; inherited by sub-agents for run correlation |
| `OPEN_MPM_MAX_TURNS` | Per-invocation max-turns override for sub-agents |
| `OPEN_MPM_MODEL_<AGENT>` | Per-agent model override (e.g. `OPEN_MPM_MODEL_CODE_AGENT`) |
| `OPEN_MPM_DEFAULT_MODEL` | Fallback model when an agent TOML has no model set |
| `OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY` | When `1`, skill discovery only walks project-local sources |

## Examples

```bash
# Default: CTRL REPL
open-mpm

# Workflow with telemetry
open-mpm --workflow prescriptive \
  --task-file ./task.md \
  --out-dir ./out/run1 \
  --json > result.json

# Direct mode against a single agent
open-mpm --direct research-agent \
  --task "Compare Rust async runtimes"

# API server with auth
OPEN_MPM_API_TOKEN=secret123 open-mpm --api --port 7654

# Live indexer (run in background while editing)
open-mpm --watch &

# Debug a single agent invocation
RUST_LOG=debug open-mpm --direct python-engineer --task-file ./task.md
```

See [configuration.md](./configuration.md) for the file/directory layout the
binary expects.
