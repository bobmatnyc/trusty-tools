Changed

- The `ticketing` agent carries the full four-label issue lifecycle: the claim
  at dispatch with a dated session comment, the stale-claim takeover test,
  event-driven advances, and a close bar that requires live verification
  evidence. Every issue verb routes here, whoever wanted it.
- The `version-control` agent owns every git and PR operation end to end —
  including arming auto-merge, the merge into main, post-merge verification
  against the exact head SHA, and reclaiming merged worktrees and their local
  branches. After a confirmed merge it flags the `status:` advance it owes so
  the PM routes it to `ticketing`; it never makes that edit itself.
- `BASE-AGENT` records the one exemption both changes rest on: the
  `version-control` agent keeps the checkout it is dispatched into and runs the
  merged-worktree prune pass (ADR-0056). `git worktree remove` stays denied for
  every agent.
