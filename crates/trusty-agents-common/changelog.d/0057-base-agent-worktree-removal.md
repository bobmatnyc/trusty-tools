Changed
- `BASE-AGENT.md`'s "never remove a worktree" rule and the `version-control`
  agent's "After a Merge" section now describe the direct-removal path
  ADR-0057 grants that one agent, alongside its five preconditions — dispatched
  identity, a target under `.claude/worktrees/` or `.worktrees/`, a clean and
  fully pushed tree, a MERGED pull request on GitHub, and no other live owner.
  `tm session prune-worktrees --merged-prs --force` stays the default sweep and
  keeps the wider scans; direct removal is for a single tree the agent has just
  verified merged. Every other agent is still told never to remove a worktree.
