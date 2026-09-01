Fixed

- pm-guard: a completed agent's delegation record no longer blocks ADR-0049
  documents-only commits in a shared main checkout. A `SubagentStop` whose
  `agent_id` no record carries now stales the one live record of that
  `agent_type` that never learned an id, instead of only deferring the stop —
  which recovered nothing when the dispatch's `PostToolUse` was lost outright.
  It declines on ambiguity and writes `Stale`, not `Completed`, so a late
  `PostToolUse` still resolves the record to the truth (#6556).
- pm-guard: a dispatch that reported a working tree of its own is no longer
  named as a shared-checkout writer. `live_shared_tree_writers` now reads the
  recorded `worktree_path` — the agent's own hook cwd — ahead of the declared
  `isolation`, so a `rust-engineer` running in `.claude/worktrees/agent-*` is
  not described as "running there with no worktree of its own" when the
  isolation declaration did not reach the tracker (#6556).
