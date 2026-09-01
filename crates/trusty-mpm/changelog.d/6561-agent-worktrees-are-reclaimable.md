Fixed

- `tm session prune-worktrees --merged-prs` now considers `.claude/worktrees/`
  agent worktrees. The ownership question is a single predicate,
  `decommission::removal_permitted`, shared by the reclaim classifier and the
  remover; its third tier admits the harness's `isolation: "worktree"` store,
  which `agent_worktree_reap` already removes from on every agent exit. Before
  this the pass reported `0 of 0 measured` against stores holding 30+ merged,
  clean agent worktrees. Every safety gate is unchanged — the merged-PR
  requirement, the #4091 dirty-work guard, the live-session check, the #5661
  agent-sentinel refusal, and the per-candidate re-read immediately before each
  delete (#6561).
