Fixed

- The ownership question behind `tm session prune-worktrees` is now a single
  predicate, `decommission::removal_permitted`, shared by the reclaim classifier
  and the remover. The two had already drifted: the classifier called the
  harness `.claude/worktrees/` store out of scope while `agent_worktree_reap`
  removes trees from it on every agent exit (#6561).
- An agent-store worktree whose sentinel names no owner is refused whether that
  sentinel is unreadable OR absent. A missing sentinel is the absence of any
  attribution, not the absence of a claim, so resolving it toward "free" on a
  destructive path is exactly what ADR-0045 forbids. This leaves the historical
  backlog of unattributed agent worktrees unreclaimable — stated in #6561 rather
  than silently deleted (#6561, #5661).
