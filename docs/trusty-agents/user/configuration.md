# Configuration

open-mpm reads its configuration from `.open-mpm/` in the current working
directory. The same layout works for the harness's own checkout and for any
project that uses open-mpm as a dependency.

## Directory layout

```
.open-mpm/
├── agents/              # Agent TOML configs (pm.toml, python-engineer.toml, …)
├── skills/              # Skill markdown files (project-local)
├── workflows/           # Workflow JSON definitions
├── tasks/               # Optional: bake-off task files (level-1.txt …)
├── agent-templates/     # Starter templates for user-authored agents
└── state/               # Runtime state (gitignored)
    ├── build.json       # Monotonic build counter
    ├── history/         # HistoryIndexer turn log
    ├── code/            # redb+usearch code index
    ├── sessions/<id>/   # Per-invocation turn logs
    ├── worktrees/       # Git worktrees for parallel wave-loop phases
    ├── processes.json   # Tracked sub-agent PIDs
    ├── initialized      # Marker that init has run
    └── project-index.md # Auto-generated project summary
```

User-global state lives under `~/.open-mpm/`:

```
~/.open-mpm/
├── projects.json        # Global project registry
├── sockets/<name>.sock  # MessageBus UNIX sockets for cross-project relay
├── skills/files/        # Globally-shared skills
├── memory/              # Shared memory stores
└── sessions/            # Cross-session audit logs (pm-messages.jsonl)
```

## Agent TOML

`.open-mpm/agents/<name>.toml`:

```toml
[agent]
name = "python-engineer"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "Python software engineer"
# Optional: route to claude CLI instead of REST
# runner = "claude-code"

[llm]
temperature = 0.2
max_tokens = 8192
# Optional: bypass OpenRouter, hit api.anthropic.com directly
# use_anthropic_direct = true

[tools]
# Per-agent tool allowlist (omit for default set)
allowed = ["read_file", "write_file", "list_dir", "grep_files"]

[system_prompt]
content = """
You are a senior Python engineer.

…
"""

[skills]
# Inject these skill markdown files into the system prompt
include = ["python-async-patterns", "pytest-best-practices"]
```

### Required fields

- `[agent].name` — must match the file stem
- `[agent].role` — free-form label
- `[agent].model` — OpenRouter-style model id, e.g. `anthropic/claude-sonnet-4-6`
- `[system_prompt].content` — the base prompt

### Optional fields

- `[agent].runner` — `claude-code` to spawn the local `claude` CLI; default
  is the in-process REST client
- `[llm].temperature`, `[llm].max_tokens`
- `[llm].use_anthropic_direct` — bypass OpenRouter (requires `ANTHROPIC_API_KEY`)
- `[tools].allowed` — per-agent tool allowlist
- `[skills].include` — names of skills to inject (matched against
  `name:` in the skill's frontmatter or filename)

## Skill markdown

`.open-mpm/skills/<name>.md` (or `~/.open-mpm/skills/files/`):

```markdown
---
name: python-async-patterns
description: Idiomatic asyncio patterns for Python 3.11+
tags: [python, async]
---

# Python Async Patterns

Use `asyncio.TaskGroup` for structured concurrency:

…
```

The YAML frontmatter is optional but recommended. Without it, the filename
stem becomes the skill name. Tags are used by `open-mpm skills list --tag`
and by the `skill_loader` tool.

### Discovery order (highest priority first)

1. `<project>/.claude/skills/` (claude-mpm compatible)
2. `~/.claude/skills/` (claude-mpm compatible global)
3. `~/.open-mpm/skills/files/` (open-mpm global)
4. `<project>/.open-mpm/skills/` (project-local)

The first match wins.

## Workflow JSON

`.open-mpm/workflows/<name>.json`:

```json
{
  "name": "prescriptive",
  "description": "research → plan → code → qa → observe",
  "phases": [
    {
      "name": "research",
      "agent": "research-agent",
      "context_template": "Research this task: {{task}}",
      "produces_files": false
    },
    {
      "name": "plan",
      "agent": "plan-agent",
      "context_template": "Task: {{task}}\n\nResearch: {{research}}\n\nWrite assignments.json",
      "produces_files": true
    },
    {
      "name": "code",
      "agent": "code-agent",
      "context_template": "Task: {{task}}\n\nPlan: {{plan}}",
      "produces_files": true,
      "wave_loop": true
    },
    {
      "name": "qa",
      "agent": "qa-agent",
      "context_template": "Run pytest on the generated files."
    }
  ],
  "auto_push": { "enabled": false },
  "ticket_management": { "enabled": false }
}
```

### Phase fields

| Field | Description |
|---|---|
| `name` | Phase identifier; available as `{{phase}}` in later phases |
| `agent` | Agent TOML name to invoke |
| `context_template` | Prompt template; supports `{{task}}`, `{{out_dir}}`, `{{<phase_name>}}` |
| `produces_files` | When `true`, extract `## File:` sections from the output |
| `wave_loop` | When `true`, run one sub-agent per file assignment in topological order |
| `parallel_subtasks` | List of subtask labels to dispatch concurrently |
| `worktree_protection` | Use git worktrees to isolate parallel subtasks |

### Top-level fields

- `auto_push` — auto-commit and push after a successful run
- `ticket_management` — open/close GitHub issues per phase via `gh` CLI

## Environment variables

See [cli-reference.md](./cli-reference.md#environment-variables).

## Project initialization

Running `open-mpm` in a directory for the first time:

1. Creates `.open-mpm/` if missing
2. Drops `.open-mpm/state/initialized` marker
3. Builds an initial code index of the working tree
4. Registers the project in `~/.open-mpm/projects.json`

Force re-initialization with `--reinit`.
