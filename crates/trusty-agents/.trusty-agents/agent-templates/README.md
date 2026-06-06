# Agent Templates

These are starter agent templates in the claude-mpm-style `.md` + YAML frontmatter format. They are NOT loaded by default — they are reference files operators copy into one of the discovered agent directories to activate them.

## Activating a Template

Copy (or symlink) a template into any of the directories the harness scans on startup, in priority order:

1. `./.trusty-agents/agents/` (project-local, highest priority)
2. `./.claude/agents/` (project-local, claude-mpm compatible)
3. `~/.trusty-agents/agents/` (user-wide)
4. `~/.claude/agents/` (user-wide, claude-mpm compatible)
5. `.trusty-agents/agents/` in the harness install root (bundled defaults, lowest priority)

Example:

```bash
mkdir -p .trusty-agents/agents
cp .trusty-agents/agent-templates/typescript-engineer.md .trusty-agents/agents/
```

The harness discovers `.md` and `.toml` agent files from those directories on startup. First occurrence of each agent name wins, so an override earlier in the priority list shadows the bundled copy.

## File Format

Each template is a Markdown file with a YAML frontmatter header. The harness recognizes these frontmatter keys:

| Key | Purpose |
|---|---|
| `name` | Canonical agent name used by `delegate_to_agent` |
| `role` | Declared role (engineer / qa / planner / docs / ...) |
| `model` | Default model id (overridable via `TAGENT_MODEL_*` env var) |
| `runner` | `subprocess` (default), `claude-code`, or `inline` |
| `description` | One-line summary surfaced in `agents list` |
| `capabilities.languages` | Matched by `best_match` language scoring |
| `capabilities.frameworks` | Matched by `best_match` framework scoring |
| `capabilities.roles` | Matched by `best_match` role scoring |
| `capabilities.tags` | Secondary-signal tag matching |

Everything AFTER the closing `---` frontmatter fence becomes the agent's system prompt.

## Available Templates

- `python-engineer.md` — Python 3.11+, FastAPI, pytest, uv
- `typescript-engineer.md` — TypeScript strict mode, React / SvelteKit / Node
- `rust-engineer.md` — tokio, axum, thiserror/anyhow

## Customization Tips

- Add or remove `capabilities.tags` to influence `best_match` routing for specific task signatures.
- Set `runner: claude-code` to execute the agent via the local `claude` CLI (requires `CLAUDE_CODE_OAUTH_TOKEN`).
- Drop the `model` field to inherit `TAGENT_DEFAULT_MODEL` or the harness fallback.
