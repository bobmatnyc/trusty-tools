# Agents and Skills

## Bundled agents

trusty-agents ships with these agents in `.trusty-agents/agents/`:

| Agent | Role | Purpose |
|---|---|---|
| `pm` | orchestrator | Reads user input and delegates to sub-agents |
| `ctrl` | orchestrator | Multi-project Taskmaster persona used by the CTRL REPL |
| `code-agent` | engineer | General-purpose code generation |
| `engineer` | engineer | Generic engineering agent (alias for code-agent) |
| `python-engineer` | engineer | Python-specialist with pytest skills |
| `gpt-engineer` | engineer | GPT-routed code agent (OpenRouter via OpenAI models) |
| `gpt5-codex-engineer` | engineer | GPT-5 code agent (when available) |
| `claude-code-engineer` | engineer | Routes through the local `claude` CLI (`runner = "claude-code"`) |
| `research-agent` | researcher | Reads docs, searches the web (with `BRAVE_API_KEY`) |
| `plan-agent` | planner | Produces `assignments.json` for the wave-loop |
| `qa-agent` | qa | Runs pytest on extracted files, reports results |
| `observe-agent` | observer | Final summary phase for workflows |
| `docs-agent` | writer | Generates / updates documentation |
| `local-ops-agent` | ops | Allowlisted shell access for local operations |
| `postmortem-agent` | analyst | Analyzes session errors and suggests improvements (#186) |

Run `tagent agents list` for the live list.

## Writing a custom agent

1. Drop a TOML file into `.trusty-agents/agents/<your-name>.toml`:

```toml
[agent]
name = "rust-engineer"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "Rust 2024-edition specialist"

[llm]
temperature = 0.2
max_tokens = 8192

[tools]
allowed = ["read_file", "write_file", "list_dir", "grep_files", "shell_exec"]

[skills]
include = ["rust-async-tokio", "rust-error-handling"]

[system_prompt]
content = """
You are a senior Rust engineer specializing in async Tokio applications…
"""
```

2. (Optional) Use a starter from `.trusty-agents/agent-templates/`:

```bash
cp .trusty-agents/agent-templates/engineer.toml .trusty-agents/agents/rust-engineer.toml
$EDITOR .trusty-agents/agents/rust-engineer.toml
```

3. Verify it loads:

```bash
tagent agents list | grep rust-engineer
```

4. Invoke it:

```bash
tagent --direct rust-engineer --task "Write a tokio TCP echo server"
```

## Bundled skills

Skills are Markdown files with optional YAML frontmatter. By default,
discovery scans two local sources (in priority order):

1. `<project>/.trusty-agents/skills/`
2. `~/.trusty-agents/skills/`

Add more sources — extra local directories or remote git repositories — via
`.trusty-agents/skill-sources.toml`; see [configuration.md](./configuration.md#discovery-order-highest-priority-first).

```bash
tagent skills list
tagent skills sources    # which directories were scanned
```

## Writing a custom skill

A skill is a single Markdown file:

```markdown
---
name: rust-async-tokio
description: Tokio async patterns for Rust 2024
tags: [rust, async, tokio]
---

# Tokio Async Patterns

## Structured concurrency

Use `tokio::spawn` and join handles instead of bare `tokio::spawn` + drop:

…
```

Drop it in any of the discovery directories. Reference it from an agent's
TOML:

```toml
[skills]
include = ["rust-async-tokio"]
```

Or load it dynamically inside an agent turn via the `skill_loader` tool.

## How injection works

The `SystemPromptBuilder` layers content in this order before sending to the LLM:

```
[base system prompt from agent TOML]

[CLAUDE.md walked from CWD up to project root]

[# Skill: <name>\n<content>] for each resolved include
```

Skills resolved at runtime by `skill_loader(skill_name)` are appended to the
running conversation as tool-result messages, so they don't bloat the system
prompt for unrelated turns.

## Tools available to agents

The default tool registry exposes these to most engineering agents (see the
`[tools].allowed` list in each agent TOML for per-agent restrictions):

- File I/O: `read_file`, `write_file`, `list_dir`, `grep_files`
- Shell: `shell_exec` (allowlisted; gated to `local-ops-agent` and `qa-agent`)
- Web: `web_search` (Brave), `fetch_url`
- Memory: `memory_search`, `vector_search`, `kuzu_recall`, `store_memory`,
  `retrieve_memory`, `list_memory_keys`
- Skills: `skill_loader`, `skill_list`
- Code search: `search_code`, `search_memory`, `search_skills`
- Workflow: `phase_audit`, `finish_task`
- Tickets: `create_ticket`, `get_ticket`, `close_ticket`
- Format conversion: `format_translator` (Markdown ↔ HTML, JSON ↔ TOML, YAML ↔ JSON)
- Delegation (PM only): `delegate_to_agent`
- CTRL: `start_pm`, `list_projects`, `task_status`, `self_project_status`,
  `initiate_self_task`, `search_docs`, `memory_store`, `memory_recall`

## Discovering everything from the CLI

```bash
tagent agents list
tagent skills list
tagent skills sources
```

Or, from inside CTRL, just ask:

```
CTRL> what agents are available?
CTRL> how do skills get injected into prompts?
```

The `search_docs` tool will surface the relevant docs.
