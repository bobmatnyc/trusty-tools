# 0056. Main-checkout write access is granted by role

- **Status:** Accepted
- **Date:** 2026-08-31
- **Scope:** crate `trusty-mpm` — `core::dispatch_isolation` and the two guards
  that read it (`pm_guard_worktree_grant::evaluate_worktree_grant`,
  `pm_guard_dispatch::dispatch_shares_the_tree`)
- **Reversibility Cost:** Low — the change turns two existing denies into
  allows for one agent name. Reverting it restores ADR-0048's and #5650's
  behaviour exactly and strands no data.
- **Decision Drivers:** the owner's ruling of 2026-08-31; `version-control`
  denied twice the same day by the #4480 concurrent-dispatch guard for pure
  push/PR work, then worked around with isolation whose fences hid a branch ref
  and forced a push by SHA; the operations `version-control` owns — a merge into
  main, a branch delete, the removal of a merged worktree — cannot be performed
  from inside a worktree
- **Supersedes / Superseded by:** Decision 6 is superseded in part by
  [ADR-0057](0057-version-control-owns-worktree-removal.md), which lets
  `version-control` run a guarded `git worktree remove`. Amends
  [ADR-0044](0044-main-checkout-write-boundary-and-agent-worktree-ownership.md)
  and [ADR-0048](0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)
  for one role. ADR-0044's source-write boundary, ADR-0048's remaining
  decisions, ADR-0049's staged-set commit gate, and ADR-0053's fetch/pull
  permission all stay in force unchanged.

## Context

ADR-0044 made a project's main checkout read-only for source, for the PM and for
every agent it dispatches. ADR-0048 gave a dispatched writer somewhere else to
write: standing in a main checkout, `tm hook --pm-guard` rewrites the dispatch
with `isolation: "worktree"` rather than refusing it. Separately, #4480 denies a
second unisolated file-mutating dispatch into a directory another writer already
holds, and #5650 widened "file-mutating" from engineer-tier alone to include
`documentation`, `version-control`, and the three QA agents that author tests.

Both rules classify an agent by WHETHER it writes. Neither asks WHAT the write
is, and for one role that distinction decides whether the rule protects anything.

`version-control` owns every git and PR operation: branch, push, PR open and
edit, arming auto-merge, `update-branch`, the merge into main, post-merge
verification, and reclaiming the worktrees a merged PR leaves behind. Those are
operations on the repository the checkout points at, not edits to the source
tree ADR-0044 protects. Three of them cannot be performed from a worktree at
all — a merge into main needs main's checkout, and a tree cannot remove itself.

On 2026-08-31 the guards denied `version-control` twice for pure push/PR
dispatches. The workaround was to dispatch it with `isolation: "worktree"`
anyway, and the isolation fences then hid a branch ref well enough that the push
had to be issued by SHA. The guard was not preventing a collision; it was
preventing the work, and the route around it was worse than either.

The read-only roles were never in this position. `research`, `code-analyzer`,
`ticketing`, and `code-critic` classify as non-writers and have always run in a
shared or main checkout untouched. What has had no expression until now is a
role that writes, whose write is precisely the reason it must stay where it is.

## Decision

We will grant main-checkout and shared-checkout operation BY ROLE, not solely by
whether the role writes.

1. `core::dispatch_isolation` gains `SHARED_CHECKOUT_PERMITTED_NAMES`, a
   name-keyed list containing exactly `version-control`, and the predicate
   `permitted_in_shared_checkout`.
2. `requires_own_worktree_in_main_checkout` returns `false` for a permitted
   role, so `evaluate_worktree_grant` no longer diverts it into a worktree.
3. A new predicate `blocked_by_shared_tree` — `shares_the_callers_tree` minus
   the grant — becomes the ADMISSION question, and the guard and the daemon both
   use it. `shares_the_callers_tree` keeps answering the OCCUPANCY question
   unchanged.
4. `agent_write_risk("version-control")` stays `Writes`. The agent still
   occupies a tree, so an engineer dispatched into the same directory beside it
   is denied exactly as before.
5. The grant is keyed by NAME, never by `role:`, so a future agent declaring
   `role: version-control` inherits nothing from it.
6. Engineer source-write confinement is untouched. ADR-0044's boundary still
   denies every agent, `version-control` included, a source-file edit in a main
   checkout, and #5791 still denies every agent an outright
   `git worktree remove`.
   Superseded in part by
   [ADR-0057](0057-version-control-owns-worktree-removal.md): the removal
   clause no longer holds for `version-control`, which may remove a tree the
   guard proves is a harness worktree, clean, pushed, merged on GitHub, and
   held by nobody else. The source-write half of this decision stands.

The prose that ships with the framework states the same thing once each: the
`version-control` agent asset, `BASE-AGENT.md`, `tm-workflow`, and
`tm-delegation-patterns`.

## Consequences

**Easier.** A `version-control` dispatch can push, open a PR, merge into main,
verify the merge, and run `tm session prune-worktrees --merged-prs --force`
without a workaround. The push-by-SHA detour and the "re-dispatch with
isolation" cycle both disappear. Cleanup after a merge stops being undelegable,
which is what left merged worktrees accumulating.

**Harder, or riskier.** One agent may now run in a directory another writer
holds. The exposure is bounded by what that agent does — it commits, merges, and
pushes rather than editing source — but it is a real reduction from #5650, and a
`version-control` agent that ran an engineer's work by mistake would land in a
shared HEAD with no deny. Two things limit it: the grant reaches exactly one
name, and that name still counts as an occupant, so it cannot be joined by an
engineer.

**Known follow-up: cleanup runs through the prune pass, not `git worktree
remove`.** #5791 denies an agent-side `git worktree remove` because a raw
removal cannot tell a merged tree from one holding unsaved work. That guard is
unchanged here, and `version-control` reclaims trees with
`tm session prune-worktrees --merged-prs --force`, which spares any tree still
holding unsaved work, still claimed by a managed session, or still owned by a
live agent. If a future ruling wants `version-control` to run the raw removal,
that is a separate decision with its own harm to weigh.

**Neutral.** The daemon's `live_shared_tree_writers` query is untouched, because
it filters on occupancy. A `version-control` dispatch no longer CLAIMS a
directory through the shared-tree route; the delegation is still recorded by the
daemon's `matcher: "*"` PreToolUse observer, so it remains visible as an
occupant either way.

## Related Decisions

Vetted against prior ADRs on 2026-08-31:

- **ADR-0044 (Main-checkout write boundary and agent worktree ownership):**
  Amended — the boundary's SOURCE-write half is untouched; what this narrows is
  the corollary that any agent which may write must be moved out of the
  checkout. Write access is now granted by role.
- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** Amended — decision 3's Unknown-is-a-writer rule and the grant
  mechanism both stand; one named role is exempted from the grant.
- **ADR-0049 (Documents-only commits are permitted in a main checkout):**
  Consistent — that gate decides on the STAGED set and applies to
  `version-control` exactly as before.
- **ADR-0053 (`git fetch` and `git pull` are permitted in a main checkout):**
  Consistent — same shape of amendment (an existing deny narrowed to an allow
  for an operation that was never the harm), and it touches a different
  classifier.
- **ADR-0055 (Trusty-mpm stops creating worktrees; the sentinel becomes
  authoritative):** Consistent — this grants no worktree creation to anyone;
  trusty-mpm still creates none.
- **ADR-0036 / ADR-0037 (harness-owned worktrees under `.claude/worktrees/`,
  main checkout as the default session directory):** Consistent — the harness
  still owns worktree creation, and the default session directory is unchanged.
