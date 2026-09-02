# 0057. version-control owns worktree removal

- **Status:** Accepted
- **Date:** 2026-09-02
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard`'s
  `pm_guard_bash::worktree_remove` rule and the
  `core::worktree_removal_facts` probe behind it
- **Reversibility Cost:** Low — the change turns one deny into a guarded allow
  for one agent name. Reverting it restores #5791's behaviour exactly, strands
  no data, and leaves the prune pass as the only removal path again.
- **Decision Drivers:** the owner's ruling of 2026-09-02; #5791's blanket deny
  makes cleanup after a merge undelegable, so merged worktrees accumulate until
  the PM sweeps by hand; ADR-0056 already gives `version-control` the shared
  checkout the removal must run from; the prune pass is a whole-workspace sweep
  and there was no way to reclaim ONE tree the agent had just merged
- **Supersedes / Superseded by:** Supersedes decision 6 of
  [ADR-0056](0056-main-checkout-write-access-is-granted-by-role.md) in part —
  the clause "#5791 still denies every agent an outright `git worktree remove`"
  no longer holds for `version-control`. Every other part of ADR-0056, and
  ADR-0044's and ADR-0048's source-write boundary, stay in force unchanged.

## Context

#5791 denies `git worktree remove` to every subagent. The reason it gave was
not the verb but the judgement behind it: a raw removal cannot tell a merged
tree from one holding unsaved work, and the sanctioned path —
`tm session prune-worktrees --merged-prs --force` — re-checks exactly that
before it deletes anything.

ADR-0056 then granted `version-control` the shared checkout, on the argument
that its writes ARE repository operations and three of them cannot be performed
from inside a worktree at all. Its "Known follow-up" paragraph names this
decision and leaves it open: "If a future ruling wants `version-control` to run
the raw removal, that is a separate decision with its own harm to weigh."

Two things weigh on that.

The prune pass is a workspace-wide sweep. An agent that has just merged one PR
and wants to reclaim that PR's tree has no narrower instrument, so it either
sweeps every registered worktree on the machine or reports the path and stops.
Reporting and stopping is what the ruling calls out: cleanup becomes a PM
errand that nothing schedules, and merged trees accumulate.

The judgement #5791 protected against is mechanically checkable. "Is this tree
merged" is `gh pr list --head <branch> --state merged`. "Does it hold unsaved
work" is `git status --porcelain` plus a count of commits the upstream does not
have. "Does anyone else own it" is the daemon's `live_shared_tree_writers`
query, which two other guard rules already make. None of those is a judgement
the agent has to be trusted with; all three are facts the guard can establish
itself.

What made the deny the right call in August was that nothing established them.
The owner ruled on 2026-09-02: "[worktree removal] should be handled by
version-manager which should not be just versions and branches but worktrees as
well."

## Decision

We will let a dispatched `version-control` agent run `git worktree remove`, and
the guard will establish every precondition itself.

1. `evaluate_worktree_remove_command` becomes agent-aware. It reports three
   answers rather than two: `Allow`, `Deny(reason)`, and `ReCheck { target }`.
   Only `version-control` reaches `ReCheck`, and `ReCheck` is not an allowance —
   the caller must still run the re-checks below.
2. The permitted name comes from `core::dispatch_isolation`'s
   `SHARED_CHECKOUT_PERMITTED_NAMES`, read through
   `permitted_in_shared_checkout`, so the dispatch-time grant of ADR-0056 and
   this Bash-time one can never read different lists.
3. `agent_type` is read only alongside a non-empty `agent_id`. A payload that
   names `version-control` without one is refused and told why. `agent_type` is
   also stamped on a top-level session launched with `--agent`, which is not a
   dispatched subagent and inherits nothing from this grant.
4. The scope is `remove`. `add`, `move`, `lock`, and `prune` are untouched, and
   `rm -rf` on a worktree directory stays denied to every caller by
   `pm_guard_bash::destructive_delete`.
5. Five re-checks gate the removal, run by the guard, never taken from the
   caller:
   - **`dispatch-identity`** — a non-empty `agent_id` AND
     `agent_type == "version-control"`.
   - **`worktree-scope`** — the resolved target is under `.claude/worktrees/`
     or `.worktrees/` (`core::project_aliases::is_worktree_path`). Lexical, so
     it runs before anything costs a subprocess.
   - **`clean-tree`** — `git status --porcelain` in the target reports nothing.
   - **`unpushed-commits`** — `git rev-list --count @{upstream}..HEAD` is zero.
     No upstream is a DENY, not a pass: nothing then proves the commits reached
     a remote.
   - **`sole-owner`** — the daemon's `live_shared_tree_writers`, keyed on the
     TARGET directory, names nobody.
   - **`merged-pull-request`** — `gh pr list --head <branch> --state merged`
     returns at least one row. Ancestry is not an acceptable substitute: every
     merge on this repository is a squash merge, so a merged branch's tip is
     structurally never an ancestor of the squash commit.
6. Every re-check fails CLOSED. A fact the guard cannot establish denies — the
   ADR-0045 distinction between absent and undeterminable, applied to a gate
   whose ALLOW deletes a checkout. This is the opposite bias from
   `caller_is_subagent`, which fails open, and the two are deliberately not
   unified.
7. A denial names which re-check failed and what was found, so the agent can
   act on the refusal rather than retry it.
8. `tm session prune-worktrees --merged-prs --force` stays the DEFAULT sweep.
   Direct removal is for one tree the agent has just verified merged.

The prose that ships with the framework states the same boundary once each:
`BASE-AGENT.md`'s "never remove a worktree" rule and the `version-control`
agent asset's "After a Merge" section.

## Consequences

**Easier.** Cleanup after a merge stops being a PM errand. A `version-control`
agent that merges PR #N can reclaim that PR's tree in the same turn, without
sweeping every registered worktree on the machine and without the report-and-
stop round trip that left merged trees accumulating.

**Harder, or riskier.** One agent may now issue the command that deletes a
checkout. Three things bound it. The grant reaches one name, read from the same
list ADR-0056 already keys on. Every precondition is established by the guard
from git, GitHub and the daemon rather than from the agent's claim, and each
fails closed. And the scope is one verb — `rm -rf` on a worktree, `worktree
add`, and the main-checkout write boundary are all unchanged.

The residual exposure is a tree that passes all five checks and still holds
something worth keeping: work committed and pushed to a merged branch, but not
represented in the merge. The prune pass's nested-repository and
gitignored-file scans (`worktree_safety::inspect_dirt`) are wider than
`git status --porcelain` and are NOT reproduced here, so the direct path is
narrower in what it inspects than the sweep it supplements. That is the reason
the sweep stays the default.

**Neutral.** The daemon query is the same route ADR-0048 decision 10's HEAD-move
rule already uses, keyed by directory, so it claims nothing and records nothing.
It is asked only after the two local git checks pass, so ordinary traffic and a
dirty tree both cost zero round trips.

## Related Decisions

Vetted against prior ADRs on 2026-09-02:

- **ADR-0056 (Main-checkout write access is granted by role):** Superseded in
  part — its decision 6 clause "#5791 still denies every agent an outright
  `git worktree remove`" is replaced for `version-control` only. Decisions 1–5
  and the grant's name-keyed shape are not merely preserved but reused: this
  rule reads `SHARED_CHECKOUT_PERMITTED_NAMES` through the same predicate.
- **ADR-0045 (Distinguish absent from undeterminable on destructive paths):**
  Extends — every re-check's error arm denies rather than passing, which is that
  ADR's rule applied to a new gate.
- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** Consistent — decision 10's live-writer query is reused verbatim
  and its directory keying is what makes the `sole-owner` check answerable. The
  source-write boundary is untouched.
- **ADR-0044 (Main-checkout write boundary and agent worktree ownership):**
  Consistent — this grants no source write anywhere. A worktree removal is a
  registry operation, not an edit to a tracked file.
- **ADR-0055 (Trusty-mpm stops creating worktrees; the sentinel becomes
  authoritative):** Consistent — this grants no worktree creation to anyone;
  trusty-mpm still creates none.
- **ADR-0037 (PM placement precedence; main checkout by default):** Consistent —
  the harness still owns worktree provisioning under `.claude/worktrees/`, and
  the `worktree-scope` re-check is keyed on exactly that layout.
