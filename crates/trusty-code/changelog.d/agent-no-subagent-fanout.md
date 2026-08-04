Added

- Synced `BASE-AGENT.md` from trusty-mpm: a "No Subagent Fan-Out" rule — a
  delegated agent does its own work or reports back to its dispatcher; it
  never spawns its own subagents. The Agent/Task tool is reserved for the
  top-level PM/orchestrator
