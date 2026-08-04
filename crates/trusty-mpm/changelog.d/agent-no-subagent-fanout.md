Added

- `BASE-AGENT.md` (synced to `trusty-code`) now states a "No Subagent
  Fan-Out" rule: a delegated agent does its own work or reports back to its
  dispatcher — it never spawns its own subagents. The Agent/Task tool is
  reserved for the top-level PM/orchestrator. Prompted by a `rust-engineer`
  spawning an untyped child agent that bypassed the roster entirely for
  routine documentation work
