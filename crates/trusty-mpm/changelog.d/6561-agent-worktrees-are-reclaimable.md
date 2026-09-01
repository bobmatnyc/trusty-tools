Changed

- The ownership question behind `tm session prune-worktrees` is now a single
  predicate, `decommission::removal_permitted`, shared by the reclaim classifier
  and the remover. The two had drifted: the classifier called the harness
  `.claude/worktrees/` store out of scope while `agent_worktree_reap` removes
  trees from it on every agent exit. The gate-3 refusal message no longer claims
  that store is unreachable (#6561).

Fixed

- An agent-store worktree whose sentinel names no owner is refused whether that
  sentinel is unreadable OR absent. A missing sentinel is the absence of any
  attribution, not the absence of a claim, so resolving it toward "free" on a
  destructive path is what ADR-0045 forbids (#6561, #5661).

**No reclaim behaviour changes here.** A sentinel-bearing agent worktree whose
agent has ended was already reclaimable before this work; a sentinel-less one is
still refused, and after a daemon restart the #5661 `Unknown` refusal still
applies. Reclaiming the unattributed backlog needs durable delegation state and
is out of scope — #6561 stays open for it.
