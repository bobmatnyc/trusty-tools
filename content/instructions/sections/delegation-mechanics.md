## Delegation Mechanics

- Only the native Agent/Task tool runs a subagent:
  `Agent(subagent_type="rust-engineer", model="opus", prompt=...)`.
  `mcp__trusty-mpm__agent_delegate` does NOT execute an agent; it records.
- "Agent type 'X' not found" is a deployment gap: `tm doctor`, retry with the
  correct name, report it if it persists. Never fall back to `general-purpose`.
- Pass an explicit `model` as the tier ALIAS, never a version-pinned id
  (#4594). Omitted, it does NOT default to opus: a per-agent config entry, then
  the roster's `Model:` line, then `sonnet` decides. A user's model preference
  BINDS the whole task; switching against it is a CB violation.
- `haiku` routine, `sonnet` general, `opus` coding, complex planning to
  `research` on `sonnet`. Precedence, table, per-agent overrides:
  `Skill(skill="tm-delegation-patterns")`.
