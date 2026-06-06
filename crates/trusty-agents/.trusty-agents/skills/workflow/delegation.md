---
name: delegation
description: PM agent skill for selecting the best sub-agent based on task capabilities
tags: [pm, delegation, agent-selection, orchestration]
---

## Agent Delegation (PM Privileged Skill)

When delegating a task, select the most capable agent for the job:

1. **Identify signals** from the task: programming language, framework, role needed (engineer/qa/docs/ops/research/plan)
2. **Use the agent registry** — call `list_agents` (if available) to see discovered agents and their capabilities
3. **Specificity wins**: prefer `python-engineer` over `engineer` for Python tasks; prefer `fastapi` framework match over generic
4. **Fallback**: if no specific match, use the most general agent for that role

### Capability Matching Examples

| Task signal | Prefer | Over |
|---|---|---|
| "Python FastAPI REST API" | python-engineer | engineer |
| "Write tests for the above" | qa-agent | engineer |
| "Document the API" | docs-agent | engineer |
| "Deploy to production" | local-ops-agent | engineer |
| "Research async Rust patterns" | research-agent | engineer |
| "Plan a multi-file project" | plan-agent | engineer |

### Language/Framework Tags to Watch For

- **Python**: `python`, `.py`, `pyproject.toml`, `pip`, `uv`, `FastAPI`, `Django`, `Flask`, `pytest`
- **Rust**: `rust`, `.rs`, `Cargo.toml`, `tokio`, `axum`, `serde`
- **TypeScript/JS**: `typescript`, `javascript`, `.ts`, `package.json`, `node`, `react`, `next`
- **Go**: `golang`, `.go`, `go.mod`

### New Project Setup

When initializing a new project, inform users:
> "To add custom agents, create TOML files in `.trusty-agents/agents/` or `.claude/agents/` with
> `[agent.capabilities]` tags. Run `tagent agents list` to verify they are discovered."
