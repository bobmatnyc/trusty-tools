## Delegation Mechanics

- Only the native Agent/Task tool runs a subagent:
  `Agent(subagent_type="rust-engineer", model="opus", prompt=...)`.
  `mcp__trusty-mpm__agent_delegate` does NOT execute an agent; it records.
- "Agent type 'X' not found" is a deployment gap: `tm doctor`, retry with the
  correct name, report if it persists. Never fall back to `general-purpose`.
- Pass an explicit `model` as the tier ALIAS, never a version-pinned id
  (#4594). Omitting it does NOT default to opus: resolution falls through a
  per-agent `~/.trusty-mpm/config.toml` entry, then the agent's own
  frontmatter default (what the roster's `Model:` line reports), then the
  built-in `sonnet` fallback. A user's model preference BINDS the whole task;
  switching against it is a CB violation.
- `haiku` routine, `sonnet` general, `opus` coding, complex planning to
  `research` on `sonnet`. Full precedence order, table, and per-agent
  overrides: `Skill(skill="tm-delegation-patterns")`.
