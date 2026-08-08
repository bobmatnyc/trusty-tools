# CLI Reference

`tagent` (trusty-agents) is a single binary that dispatches based on flags.

```
tagent [FLAGS]
```

## Mode flags (mutually exclusive)

| Flag | Description |
|---|---|
| `--ctrl` | Interactive multi-project session manager (also the default when no mode flag is set) |
| `--pm` | Single-shot PM orchestrator; reads one line from stdin |
| `--agent <name>` | Sub-agent runner; reads one NDJSON Task from stdin, emits one Result, exits |
| `--direct <name>` | Bypass PM LLM, send task to sub-agent directly |
| `--workflow <name>` | Run `.trusty-agents/workflows/<name>.json` |
| `--api` (alias `--serve`) | Launch HTTP API server + embedded web UI |
| `--reindex` | Full re-index of the working tree, then exit |
| `--watch` | Live filesystem watcher; keeps the code index in sync, blocks until killed |
| `--version` / `-V` | Print version + build number, exit |

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
| `--api-token <TOK>` | Require this bearer token on every `/api/*` request. Falls back to the `TAGENT_API_TOKEN` env var. `/api/health` and `/api/config` are exempt (liveness probe and pre-auth UI bootstrap). `/api/events` is NOT exempt — it accepts this bearer token **or** a short-lived ticket from `POST /api/events/ticket`, because a browser `EventSource` cannot send headers ([#5052](https://github.com/bobmatnyc/trusty-tools/issues/5052)). The stream requires one of the two whether or not a token is configured |

## Diagnostic / maintenance flags

| Flag | Description |
|---|---|
| `--check-orphans` | Print tracked sub-agent PIDs and their live status |
| `--clear-sessions` | Clear in-memory agent session history |
| `--reinit` | Force project re-initialization and memory seeding |

## Subcommands

```
tagent code search "<query>"     # Search the local code index
tagent memory search "<query>"   # Search the history/turn-log index
tagent agents list               # List available agents
tagent skills list               # List discoverable skills
tagent skills sources            # Show skill discovery directories
tagent postmortem [--last N | --session <id>]
tagent postmortem --tag <tag>
```

## Environment variables

Current names use the `TAGENT_*` prefix. Deprecated `OPEN_MPM_*` names from
before the crate's rename are still read as a fallback (with a one-time
deprecation warning) — set the `TAGENT_*` name in new environments.

| Variable | Description |
|---|---|
| `OPENROUTER_API_KEY` | OpenRouter API key (default routing for most agents) |
| `ANTHROPIC_API_KEY` | Direct Anthropic API key (for agents with `use_anthropic_direct = true`) |
| `CLAUDE_CODE_OAUTH_TOKEN` | OAuth token from `claude setup-token` (only for agents with `runner = "claude-code"`) |
| `BRAVE_API_KEY` | Optional — enables `web_search` tool |
| `TAGENT_API_TOKEN` | Default bearer token for `--api` mode |
| `RUST_LOG` | `trace`, `debug`, `info`, `warn`, `error` (default: `info`) |
| `TAGENT_CONFIG_DIR` | Override for `.trusty-agents/agents/` lookup path |
| `TAGENT_OUT_DIR` | Default output root when `--out-dir` is omitted |
| `TAGENT_RUN_ID` | Auto-set; inherited by sub-agents for run correlation |
| `TAGENT_MAX_TURNS` | Per-invocation max-turns override for sub-agents |
| `TAGENT_MODEL_<AGENT>` | Per-agent model override (e.g. `TAGENT_MODEL_PYTHON_ENGINEER`) |
| `TAGENT_DEFAULT_MODEL` | Fallback model when an agent TOML has no model set |
| `TAGENT_SKILLS_PROJECT_LOCAL_ONLY` | When `1`, skill discovery only walks project-local sources |

## Examples

```bash
# Default: CTRL REPL
tagent

# Workflow with telemetry
tagent --workflow prescriptive \
  --task-file ./task.md \
  --out-dir ./out/run1 \
  --json > result.json

# Direct mode against a single agent
tagent --direct research-agent \
  --task "Compare Rust async runtimes"

# API server with auth
TAGENT_API_TOKEN=secret123 tagent --api --port 7654

# Live indexer (run in background while editing)
tagent --watch &

# Debug a single agent invocation
RUST_LOG=debug tagent --direct python-engineer --task-file ./task.md
```

See [configuration.md](./configuration.md) for the file/directory layout the
binary expects.
