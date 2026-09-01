Fixed

- pm-guard: a completed agent's delegation record no longer blocks ADR-0049
  documents-only commits in a shared main checkout. A `SubagentStop` whose
  `agent_id` no record carries now stales the one live record of that
  `agent_type` that never learned an id, instead of only deferring the stop —
  which recovered nothing when the dispatch's `PostToolUse` was lost outright.
  It declines on ambiguity and writes `Stale`, not `Completed`, so a late
  `PostToolUse` still resolves the record to the truth (#6556).
- pm-guard: that reconciliation releases the tree for a COMMIT but not for a new
  file-mutating DISPATCH. Identification by `agent_type` can name a sibling that
  is still writing, and admitting a second writer onto one git HEAD is the
  ADR-0048 harm no later signal undoes, so `shared_tree_occupants` keeps the
  record counted until its original liveness budget expires (#6556).
- pm-guard: a dispatch that reported a working tree of its own is no longer
  named as a shared-checkout writer — but only while it is still IN that tree.
  `worktree_path` is a one-way latch the reap depends on, so an agent that ran
  `EnterWorktree` and then `ExitWorktree` with `action: "keep"` was excluded
  from the count for life; the guard now reads the agent's current hook cwd
  beside the grant (#6556).
