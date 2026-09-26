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
     No upstream is not a pass on its own: nothing there proves the commits
     reached a remote. Amended by #7232 — it is no longer a deny on its own
     either. `gh pr merge --delete-branch`, the sanctioned merge flow, deletes
     the remote branch, so `@{upstream}` stops resolving on every squash-merged
     worktree and this check denied exactly the trees the grant exists to let
     `version-control` reclaim. A missing upstream is now carried to
     `merged-pull-request`, which must then supply the landing evidence: a
     MERGED pull request plus a clean tree grants, and no merged pull request
     still denies. The guarantee is unchanged — no removal without evidence
     the commits reached GitHub — only which check supplies it. A missing
     upstream is distinguished from a git failure by proving `git rev-parse
     --verify HEAD` succeeds in the same directory first; when it does not,
     the check denies as before.
   - **`sole-owner`** — the daemon, keyed on the TARGET directory, names
     nobody. Asked through `live_shared_tree_writers_or_deny`, NOT the
     fail-open `live_shared_tree_writers` the HEAD-move rule uses: that one
     collapses "nobody is here" and "nothing answered" into the same empty vec,
     which is right where a wrong ALLOW costs the operator a `git merge` and
     wrong where it costs another session its worktree. An unreachable, timed
     out or unparseable reply denies, and so does a payload with no
     `session_id`, since no session's delegations can be addressed without
     one.
   - **`merged-pull-request`** — `gh pr list --head <branch> --state merged`
     returns at least one row. Ancestry is not an acceptable substitute: every
     merge on this repository is a squash merge, so a merged branch's tip is
     structurally never an ancestor of the squash commit.

     Amended by #7914 — a MERGED pull request is no longer the ONLY route to
     landing evidence. `local-only-commits` (`git rev-list --count HEAD --not
     --remotes=origin`) answers the question this check stands in for directly,
     and a literal zero grants on its own: every commit the worktree holds is
     already on an `origin` ref, so the removal can destroy no history. That is
     what admits the three shapes the original wording refused outright — a
     worktree branched off `origin/main` and never committed to, a branch
     fast-forwarded into a sibling that landed, and a detached-HEAD tree with no
     branch name to search GitHub by. The guarantee is unchanged: no removal
     without evidence the commits reached the remote. Only a zero admits, so a
     non-zero count and a `rev-list` that could not be answered both fall
     through to the merged-PR check unchanged — a relaxation that cannot be
     established never grants, which is decision 6 applied in the one direction
     available to it.

     `refs/remotes/origin/*` is a LOCAL cache, so the check refreshes it with a
     bounded `git fetch --prune origin` immediately before counting. A branch
     deleted on GitHub by any route other than a fetch in that worktree — `gh
     pr close --delete-branch`, the web UI, another clone — otherwise leaves a
     ref vouching for commits the remote no longer has, and the admission would
     destroy the only surviving copy. The bound is 3 s, below the `PreToolUse`
     hook's own 5 s registration: a hook Claude Code kills emits no decision at
     all, and no decision is not a deny. A refresh that fails or expires makes
     the count unanswerable, which does not grant.

     Amended by #7832 — the pull request is resolved by COMMIT when there is
     no branch name to resolve it by. `branch` fails on a detached checkout by
     design, so this check could not run at all for a review worktree parked on
     a merged pull request's head: `.claude/worktrees/review-7751` at
     `02a83032d` was refused with "HEAD is detached" while PR #7794 had already
     merged it, leaving a manual `rm` as the only route. #7914's admission does
     not reach that shape either, because the merge deleted the branch and the
     head commit is therefore on no `origin` ref. `gh pr list --state merged
     --search <sha>` relates the two, and the grant requires a MERGED pull
     request **whose own `headRefOid` IS this worktree's HEAD**. The match is
     exact and one-directional on purpose. The reclaim sweep's ladder accepts
     ancestry either way round because its gate 6 re-inspects whatever the merge
     did not carry; nothing runs after THIS gate, so a pull request opened from
     a descendant — or one that merely mentions the commit, which the search
     also returns — is refused. A fork's row is never a match. The exact-sha
     comparison is made twice, once in the probe and again in the policy, so the
     grant does not rest on a filter the deciding function cannot see.

     Amended by #7850 — the pull request is looked for in the repository the
     branch was PUSHED to, not unconditionally in `origin`'s. #7057 fixed the
     question "which repository" by reading it from the worktree's own remote
     rather than letting `gh` infer one, and hard-coded `origin` as that remote.
     A fork workflow breaks the assumption: in `breezeblue-ai/breeze-tts` local
     `main` tracks `fork` (`bobmatnyc/breeze-tts`) because `origin` 403s for the
     operator's account, `fix/matsuoka-respelling` merged as
     `bobmatnyc/breeze-tts#5`, and the removal was still refused with "GitHub
     has no MERGED pull request … in `breezeblue-ai/breeze-tts` (resolved from
     this worktree's `origin` remote)". The remote now comes from git's own push
     precedence — `branch.<name>.pushRemote`, then `remote.pushDefault`, then
     `branch.<name>.remote` — and only then falls back to `origin`.

     This is a CORRECTION of which repository is asked, not a relaxation of what
     must be found there: a MERGED pull request is still required, and asking
     the wrong repository could only ever produce a false DENY. It therefore
     needs no fail-closed carve-out of its own, and gets one anyway in the one
     place it could matter — a remote name that resolves to no parseable URL is
     an `Err`, which denies, rather than a silent second attempt at `origin`. A
     repository with none of the three keys set answers `None` and takes the
     pre-#7850 `origin` path byte for byte.

     Amended by #7958 — a worktree whose HEAD is its own pull request's
     `headRefOid` grants regardless of the `unpushed-commits` answer. `gh pr
     merge` leaves `@{upstream}` STALE rather than level, so `Ahead(n)` is what
     a landed worktree reports; the `merged pull request and not ahead`
     short-circuit therefore never fired for one, and the decision fell through
     to the merge-tree comparison, which reported residue for a tree holding
     none. PR #7946's worktree, sitting on head `9c8699fe0`, was refused that
     way on 2026-09-14. An exact head-sha match outranks that comparison
     because it is the stronger evidence: the pull request merged THIS commit,
     so there is nothing here the merge did not carry, whatever a tracking ref
     or a moved base says. It is a relaxation, so it inherits decision 6 in the
     one direction available: a pull request GitHub named no `headRefOid` for, a
     HEAD git could not resolve, and any mismatch all leave the pre-#7958
     decision standing.

     Amended by #7889 — landed CONTENT is landing evidence of its own, on both
     reclaim paths. A donor branch fast-forwarded onto a sibling's head and
     squash-merged under THAT name never acquires a pull request carrying its
     own name, so this check's refusal is permanent for a tree that holds
     nothing: nineteen clean worktrees were stuck that way across 2026-09-21 and
     2026-09-22 — `fix/8351-bridge-session-recovery-critic-r1`,
     `fix/8261-pm-guard-oracle`, `fix/8236-cache-race`, `fix/8361-context-budget`
     among them — each holding a 10–25 GB `target-*/` directory, and each
     byte-identical to `origin/main`. Owner ruling 2026-09-22: admit them.
     A new `landed-content` admission runs after this check has ANSWERED with no
     merged pull request. It refreshes `origin` under the same 3 s bound, then
     asks whether merging HEAD into the landing base would change any file
     (`git merge-tree --write-tree <base> HEAD`, then `git diff --name-only
     <base> <tree>`). An empty answer grants and names the base commit; a
     non-empty one refuses and names the first residual path. The base is
     `origin/HEAD`, or `origin/main`/`origin/master` when the repository
     declares none. The same predicate — one implementation, in
     `core::worktree_landed_content` — decides gate 5 of `tm session
     prune-worktrees --merged-prs`. The two paths do not ask it in the same
     places. The guard never asks it once its own or a round sibling's pull
     request has matched, and never for a detached HEAD, so on those paths it
     is stricter than the sweep. Where they disagree, one of them refuses.
     Making them agree would add a fetch and a `gh` call to a guard already
     short of time. That strictness is about the landing question only. Every
     guard grant — merged pull request, landed content, `local-only-commits`
     or detached head — now ends with the sweep's scan for nested repositories
     holding work and high-value gitignored files, inside the deadline, because
     `git worktree remove --force` deletes ignored content. The guard does not
     check commits on `session/<leaf>` that HEAD cannot reach, because
     `git worktree remove` deletes no branch, and it does not read the sweep's
     keep-list. On the sweep, gate 6 counts a donor branch's commits as
     unpushed, because the squash also carried a sibling's work and no patch id
     matches. That count lets the tree reach the admission, which judges only
     the commits reachable from HEAD. Every other place work can live refuses
     before anything is compared: an uncommitted file, a dirty nested
     repository, and a commit on `session/<leaf>` or `<leaf>` that HEAD cannot
     reach, which the removal's `git branch -D` would orphan. The admission can
     take up to 40 s on the sweep, so a grant re-reads that dirt, and HEAD,
     before it is returned. The residue diff runs with `--ignore-submodules=none`,
     so a `diff.ignoreSubmodules` or `submodule.<name>.ignore` setting cannot
     hide a gitlink bump. The pre-delete re-check asks the admission again
     rather than demanding a merged pull request.

     When `landed-content` does not admit, a second route is asked:
     `merged-pr-ancestry` admits when HEAD is the head commit of a MERGED pull
     request, or an ancestor of it (`gh pr list --state merged --search
     <HEAD>`, then `git merge-base --is-ancestor HEAD <headRefOid>`). That is
     the donor shape itself, and it still admits a donor whose change the pull
     request later superseded, or whose files `main` has since edited so the
     merge conflicts. Only that one direction counts: a pull request whose head
     is BEHIND HEAD leaves commits here the merge never saw. On the sweep the
     same pair of routes also judges a donor that gate 5 matched to its
     sibling's merged pull request through the #7267 commit search, whose
     commits gate 6 would otherwise count as unpushed.

     Ancestry against the squash commit is still never evidence: `git
     merge-base --is-ancestor` and `git cherry` both answer "not merged" for a
     squash-merged branch, and neither is consulted that way. Being a
     relaxation, it inherits decision 6 in the one direction available — a
     failed or expired refresh, a base that will not resolve, a `merge-tree`
     that errored or conflicted, a residual path, a commit search that did not
     answer and an ancestry check that could not run all refuse, and so does an
     unanswerable `gh` branch lookup, which never reaches the admission at all.
     An open pull request, a dirty tree and a live owner are decided before it,
     exactly as before.

     Because the admission lengthens the refusing path, the owner query and
     every re-check must now decide within 3.5 s of the `tm` process starting,
     and DENY on expiry, naming the check still running. The `PreToolUse` hook
     is killed at 5 s, and a killed hook returns no decision, which is not a
     deny. The deny is printed and flushed before its audit, and the audit must
     end 4.5 s after process start. The admission reuses the
     `local-only-commits` fetch when that fetch succeeded, instead of fetching
     a second time.

     This SUPERSEDES half of the #7275 round-2 finding. The never-pushed branch
     holding one empty or self-reverting commit is now admitted — not because
     evidence stopped being required, but because the ruling makes the evidence
     CONTENT, and such a tree demonstrably holds none the remote lacks. What
     round 2 established still holds for the merged-PR route: its merge-tree
     comparison against a pull request's base is never asked until that pull
     request is in evidence. The `landed-content` comparison is separate. It
     can grant without any pull request, but only after a successful refresh of
     `origin` and after the clean-tree and ownership checks.

     Amended by #8633 — a `merge-tree` CONFLICT is a verdict, not a failure,
     and the comparison may be made against an earlier commit on the base.
     A squash-merged branch whose files the base edited afterwards conflicts
     with the base's tip, so every such tree was refused — with an empty
     reason, because `merge-tree` reports a conflict on stdout with exit 1.
     PR #8655's worktree was refused that way on 2026-09-26. Both the
     `landed-content` admission and the merged-pull-request residue check now
     ask `core::worktree_landed_history::content_on_base`. A HEAD that is an
     ancestor of the base — a plain merge or a fast-forward — is landed. Any
     other HEAD needs a two-way landing commit `M` on the base: the base's
     tip is tried first, then each first-parent base commit since the fork
     point that touches a file HEAD changed (oldest first, capped). `M`
     admits only when BOTH directions are empty: merging HEAD into `M`
     changes no file, and applying `M`'s own patch (against its first
     parent) onto HEAD changes no file. The first proves HEAD's changes since
     the fork are all in `M`; it cannot see a later branch commit that takes
     part of `M` back — a revert to the fork's version, or the deletion of a
     file `M` added — because relative to the fork that commit changes
     nothing. The second catches exactly that. The tip gets no shortcut: an
     empty merge into the tip no longer admits a non-ancestor on its own,
     because when the base has not moved since the squash, the tip IS the
     squash and the same blind spot applies there. A rebase-merged branch is
     landed at its last replayed commit, which carries all of its content and
     whose own patch HEAD holds. Ancestry is sufficient, never necessary; the
     test is still content, judged only against commits already on the
     remote. A branch holding a different version of the change, a later
     commit the squash never carried, or a later commit that undid part of
     the squash is not admitted, whether or not the base has moved since. A conflict is told apart from a git error by
     the tree id `merge-tree` prints first; a git error (it also exits 1 for a
     ref it cannot merge) stays undeterminable and quotes git's stderr, and
     any error in either direction refuses. Every refusal names the
     conflicted or residual files and how many base commits were searched —
     "the oldest N of M" when the cap cut the search short.
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
dirty tree both cost zero round trips. What it does NOT share with that rule is
the reader: the two failure biases are opposite and are kept as two functions
rather than one function with a flag, so neither caller can acquire the other's
bias by editing a default.

**One consequence of that bias, stated rather than discovered later.** With the
daemon down, `version-control` cannot remove a worktree directly at all. The
sweep is in the same position — it queries the same registry — so the fallback
is to fix the daemon (`tm doctor`, `tm restart`), not to route around the
check.

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
