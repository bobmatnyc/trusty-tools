---
name: ticketing-epic
description: Epic structure and breakdown patterns for GitHub Issues
tags: [ticketing, github, epic, planning]
---

# Skill: ticketing-epic

## Creating Epics

An epic groups related work under a parent issue. Use when a feature or initiative requires 3+ tickets.

### Epic Structure

1. **Parent issue** — label: `epic`
   - Title: feature area name (e.g. "MCP Registry Support")
   - Body: 1-paragraph goal + `## Tasks` checklist (one `- [ ] Child ticket title` per child)

2. **Child tickets** — label: `feature` or `chore` (or `bug` if applicable)
   - Title: specific, scoped deliverable
   - Body: starts with "Part of #<epic-number>." then implementation details + acceptance criteria

### Example Epic Body

```markdown
Adds a remote MCP service registry so ctrl/PM/research agents know which external
platforms are available without hardcoding.

## Tasks
- [ ] Global config file with MCP service definitions
- [ ] Role-gated prompt injection for ctrl/PM/research
- [ ] Dynamic add/remove tools (mcp_add, mcp_remove, mcp_list)
- [ ] gworkspace-mcp registered with full tool list
```

### Example Child Body

```markdown
Part of #243.

## Summary
Wire native ticketing tools into the ctrl tool registry so a `ctrl` chat
turn can create/update/close tickets without delegating.

## Acceptance Criteria
- [ ] `register_ticketing_tools` called from `build_ctrl_registry`
- [ ] Tools silently absent when no `[github]` identity is configured
- [ ] Unit test asserts tool registration when env vars present
```

### When to Use

- Feature with clear phases → epic
- Bug cluster affecting same component → epic
- Research spike + implementation → epic (spike as the first child)
- Single self-contained task → plain ticket, no epic needed

### Workflow

1. Create the parent epic ticket FIRST. Capture its number.
2. For each child task, call `create_ticket` with body starting `Part of #<n>.`
3. Optionally update the parent body to link concrete child numbers using
   `update_ticket(id=<epic>, body=...)` — this is optional but useful for navigation.

### Anti-patterns

- Don't create child tickets before the parent — you'll have to come back and edit.
- Don't put implementation details in the epic body — keep it goal-focused; details
  belong in child tickets.
- Don't use `epic` for trivial 1–2 ticket batches — overhead exceeds the benefit.
