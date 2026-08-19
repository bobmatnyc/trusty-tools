Fixed

- A `SubagentStop` that reached the daemon before the `PostToolUse` teaching its
  `agent_id` no longer strands its delegation `Running` for six hours. The stop
  is held in a bounded, TTL'd ledger and applied the moment the id is taught, so
  the two arrival orders converge on one record. Resolution still matches only
  the exact `agent_id` the stop quoted — nothing guesses at a "most recent"
  delegation, and an entry the ledger drops leaves the delegation to the
  staleness sweep exactly as before (#4142).
