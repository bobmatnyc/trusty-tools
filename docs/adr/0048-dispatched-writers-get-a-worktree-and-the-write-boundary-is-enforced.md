# 0048. Dispatched writers are granted a worktree, and the main-checkout write boundary is enforced

- **Status:** Accepted
- **Date:** 2026-08-15
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard` (`pm_guard_worktree_grant`,
  `pm_guard_write_boundary`, `pm_guard_bash::main_checkout`), the agent
  classifier `core::dispatch_isolation`, and the daemon's
  `DaemonState::live_shared_tree_writers`
- **Reversibility Cost:** Medium — the grant changes what every dispatch from a
  main checkout does, and the boundary changes what every session may write
  there; both are user-visible and both ship in one release
- **Decision Drivers:** ADR-0044's enforcement requirement, recorded as stated
  but unbuilt; the owner's report of three sessions in one `mcp-services`
  checkout with commit `f1da7bce` landing on `fix/1646-drive-query-v2-migration`
  and `fix/1644-…` left empty at `cff5bbcd`; ADR-0044 row 4's finding that no
  trusty-mpm path gives a dispatched agent anywhere else to write
- **Supersedes / Superseded by:** Amends [ADR-0044](0044-main-checkout-write-boundary-and-agent-worktree-ownership.md).
  ADR-0044's write boundary and its assignment of worktree ownership to the
  harness both remain in force.

## Context

ADR-0044 decided two things and built neither into a mechanism. Decision 2 says
the write restriction "is enforced mechanically across PM and delegated-agent
execution paths" and that "convention alone is insufficient". Decision 4 says
trusty-mpm does not create worktrees for dispatched agents, because nothing in
it ever did.

Held together, those two left a gap that produced the reported harm rather than
preventing it. Enforcement covered only whole-tree-destructive git verbs
(`pm_guard_bash::main_checkout`: `reset --hard`, `clean -fdx`,
`checkout -- <pathspec>`). An ordinary `Write` to a `.rs` file in the shared
checkout, and a `git commit` on the shared HEAD, passed every check in the
process. At the same time `worktree_enabled_for_project` had no production
caller at all, so a dispatched writer had nowhere else to go: it inherited the
session's main checkout and wrote there because that was the only tree it had.

A third defect made the existing concurrency guard blind to the exact shape
reported. `live_shared_tree_writers` filtered `d.session == session` before any
other test, so a guard running in session A could not see session B's agents in
the same directory — and the incident is three sessions in ONE checkout, where
every writer belongs to a different session. The daemon holds every session's
delegations; that filter was the only thing hiding them.

Closing enforcement alone would block all work. Granting worktrees alone would
leave the boundary unenforced. The owner's ruling is that both close together.

## Decision

1. **A dispatched agent that may write is given its own worktree when the
   session is standing in a main checkout.** `tm hook --pm-guard` emits
   `hookSpecificOutput.updatedInput` carrying the dispatch's own `tool_input`
   with `isolation: "worktree"` added. This is a grant, not a refusal: it is
   applied by Claude Code as data, rather than depending on the model re-issuing
   the dispatch after reading a denial.
2. **Trusty-mpm still creates no worktrees.** ADR-0044 decision 4 is unchanged
   and this decision depends on it: the harness provisions the worktree under
   `.claude/worktrees/` per [ADR-0036](0036-all-worktrees-are-siblings-under-claude-worktrees.md)
   and reclaims it on completion. Trusty-mpm asks; it does not own.
3. **An agent this binary cannot classify is treated as a writer, in a main
   checkout only.** `agent_write_risk` separates "bundled and read-only" from
   "not a bundled agent", and `requires_own_worktree_in_main_checkout` isolates
   both `Writes` and `Unknown`. This is the OPPOSITE direction from #4480's
   `shares_the_callers_tree`, which continues to fail open, and the divergence
   is deliberate — see Consequences.
4. **The write boundary is enforced for the PM and every agent it dispatches.**
   In a main checkout, an edit tool targeting a SOURCE file is denied, and
   `git commit` is denied. Both pierce the two automatic subagent exemptions
   (`CLAUDE_MPM_SUB_AGENT`, the `agent_id` dispatch marker) for the reason
   ADR-0044 decision 1 requires: the restriction binds dispatched agents, and
   both markers return ALLOW for exactly that population.
5. **Documents and configuration stay writable**, per ADR-0044 decisions 1 and
   3. "Source" is decided by the extension list `pm_guard` already uses for the
   PM's own rule, so `.md`, `.toml`, `.json`, `.yaml`, and extension-less files
   are unaffected, and framework deployment (`.claude/`, bundled skills,
   `TASK.md`) remains permitted.
6. **Every deny names what to do instead.** A refusal that does not carry a
   remedy is retried differently and worse — the observed shape is an agent
   that reaches for `Bash` and `cat >` after an `Edit` is refused.
7. **The shared-writer query is keyed by directory, not by session.** The
   hazard is a shared git HEAD, and a HEAD is a property of the directory: two
   agents in one directory collide whichever PM dispatched them, and two agents
   in different directories never collide however closely related their
   sessions are.
8. **Two sessions in one main checkout is no longer a warning.** With writers
   isolated and the boundary enforced, sharing a read-only checkout is the
   intended arrangement. `launch_on_main`'s collision record drops from `warn!`
   to `info!` and keeps naming the other session, which is the part an operator
   uses.
9. **Any session may `git fetch`, unconditionally and without coordination, and
   a granted worktree is branched from what that fetch just updated.**
   `fetch` writes remote-tracking refs (`refs/remotes/origin/*`) and objects
   into `.git`; it never touches the working tree and never moves HEAD. Git's
   own per-ref locking makes concurrent fetches in one repository safe — the
   loser of a race retries or no-ops, it does not corrupt a ref. Branch and
   worktree creation resolve their base by reading `origin/main`, the
   remote-tracking ref the fetch just wrote, immediately before creation and
   again after every PR merge — never from the checkout's local `main`, which
   only a `pull` moves. Local `main` in this checkout was measured 92 commits
   behind `origin/main`; every merged-ness check, diff, and branch point taken
   against it was quietly wrong, and `git branch -d` refused five branches
   whose PRs had already merged. A worktree cut from that base starts work on
   ground CI will not actually merge into — a correctness defect, not a
   tidiness one.
10. **`git pull` is confined to a session's own worktree; a HEAD-moving pull is
    not permitted in a shared main checkout.** `pull` is `fetch` plus a merge
    or fast-forward of local `main`, which moves HEAD and can write the working
    tree — the same hazard class as the branch-MOVING verbs
    (`checkout <branch>`, `switch <branch>`, `merge`, `rebase`) this ADR
    already leaves uncovered below. A worktree's HEAD belongs to the session
    that owns it, so a pull there races nothing. The shared main checkout's
    HEAD does not belong to one session, which is exactly the collision
    decision 7's directory-keyed writer query exists to catch for edit-tool
    writes and `commit` — and does not reach here, because `pm_guard_bash`'s
    git-verb classification stops at the verbs it already names. This is the
    same stated gap, not a new one.

## Consequences

- **The fail-open direction is now scoped rather than global.** #4480 states
  that both its classifiers fail toward ALLOW and that the direction "is not
  negotiable", on the grounds that a false DENY halts every dispatch while a
  false ALLOW reproduces prior behaviour. That reasoning is sound where it was
  written and does not carry into a main checkout, where the costs invert: a
  false allow corrupts another session's branch — the reported harm, not a
  hypothetical one — and a false grant costs one worktree the harness reclaims
  when it is unchanged. A read-only CUSTOM agent dispatched from a main
  checkout now pays for a worktree it does not need. That is the price of the
  choice, and it is paid in disk rather than in correctness.
- **A `Task` dispatch that needs isolation is denied rather than rewritten.**
  `isolation` is an `Agent` parameter; injecting it into a tool whose schema
  rejects it would convert a guarded dispatch into a failed tool call. The deny
  names `Agent` as the remedy.
- **A dispatch granted isolation is still recorded by the daemon's tracker as
  unisolated**, because that tracker observes the original hook payload and
  cannot see the rewrite. The record is inert: every dispatch from that main
  checkout takes the grant branch and returns before querying, so nothing reads
  it to deny with. It would matter if the grant were ever made conditional.
- **Widening the writer query across sessions makes cross-session denies
  possible where none could occur before.** That is the point, and the grant is
  what keeps it rare: writers dispatched from a main checkout are isolated
  before the query is ever reached.
- **`DaemonState::live_shared_tree_writers` loses its `session` parameter**, a
  breaking change to a public method. `claim_shared_tree_dispatch` keeps its
  `session` argument, which now records the claim rather than filtering the
  answer.
- **Branch-MOVING verbs are NOT covered.** `git checkout <branch>`,
  `git switch <branch>`, `merge`, and `rebase` can still switch a branch under
  another session, which is part of the same incident. They are left out
  because their safe and unsafe forms differ by argument rather than by verb,
  and a loose rule there costs false denies on ordinary work — the failure
  #5356 was filed for. This is a stated gap, not an oversight.
- **A write performed through `Bash` rather than an edit tool** is classified by
  `pm_guard_bash`, which reaches a deny for the PM through `SHELL_EDIT_REASON`
  but does not carry the main-checkout dimension for dispatched agents. Also a
  stated gap.
- **The operator escape hatches still lift everything.** `TRUSTY_MPM_DISABLE_HOOKS`
  and `TRUSTY_MPM_PM_UNRESTRICTED` are human decisions, not automatic markers,
  and are unaffected — including the `.claude/settings.json` `env` self-exemption
  path tracked as #3981.
- **Fetch needs no enforcement; pull in a shared main checkout does, and does
  not have it.** Decision 9's half of the rule has nothing to check — `fetch`
  is safe by construction, and no session can violate it by running one.
  Decision 10's half is a real gap: `pm_guard_bash` classifies `reset --hard`,
  `clean -fdx`, and `checkout -- <pathspec>`, but not `pull`, `merge`, or
  `rebase`, so a HEAD-moving `pull` in a shared main checkout is the same
  branch-moving-verb gap already named above, not a new one. Decision 7's
  directory-keyed `live_shared_tree_writers` does not reach it either — that
  query answers the isolation grant and the edit-tool/`commit` write boundary,
  and nothing routes a Bash `pull` through it.

## Alternatives Considered

- **Deny the unisolated dispatch and let the PM re-issue it with isolation.**
  Rejected. A denial is applied only if the model reads it, understands it, and
  repeats itself; the rewrite is applied by the harness whether or not anything
  read it. The denial also costs a round trip on every dispatch from a main
  checkout, which is now the default placement under ADR-0037.
- **Have trusty-mpm create the worktree itself, giving `worktree_enabled_for_project`
  the production caller it never had.** Rejected: it directly contradicts
  ADR-0044 decision 4 and ADR-0036's topology ownership, and it would put
  trusty-mpm in the business of reclaiming trees the harness already reclaims.
  The flag stays with the one live effect ADR-0044 assigned it.
- **Treat an unknown agent as read-only in a main checkout too, for consistency
  with #4480.** Rejected. Consistency of direction is not the property worth
  keeping — the two policies answer different questions with different costs,
  and the unknown agent writing into a shared checkout is the reported defect.
- **Key the writer query on session AND directory.** Rejected: it is what
  shipped, and it is what made the guard blind to the only shape that has
  caused harm.
- **Enforce the boundary without granting worktrees, and let operators pass
  `--worktree`.** Rejected by the owner's ruling. Enforcement without a writer
  destination blocks all delegated work on every project whose sessions run on
  the main checkout, which ADR-0037 made the default.

## Related Decisions

Vetted against the ADR corpus on 2026-08-15:

- **ADR-0044 (Main-checkout write boundary):** **Amends.** Decisions 1–3 are
  unchanged and are what this ADR builds the mechanism for. Decision 4 is
  unchanged and is the constraint that makes the grant a request rather than a
  creation. Decision 5's row 4 — "explicit harness isolation only" — is
  extended: isolation may now be requested on the dispatch's behalf by the
  guard, which is still harness-created isolation, not a trusty-mpm worktree.
  Decision 6 is untouched; the per-project `worktree` flag gains no new role
  here, and the grant consults the FILESYSTEM (is this a main checkout?), never
  the project registry.
- **ADR-0037 (PM placement precedence, main checkout by default):**
  **Consistent, and depended upon.** Nothing here changes where a session runs.
  This decision is the answer to what ADR-0037's default implies for the agents
  a main-checkout session dispatches. #3455's wasted-disk complaint stays closed
  for SESSIONS; a worktree per file-mutating dispatch is a new and narrower
  cost, incurred only where a writer would otherwise share a checkout.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):**
  **Consistent.** The granted worktree lands at ADR-0036's location because the
  harness creates it; no new location is introduced. Note that
  `.worktrees/<uuid>` remains the SESSION worktree path — a different family,
  unaffected here.
- **ADR-0030 (Session/workstream model, Proposed):** **Consistent.** Point 7's
  "agent worktrees are children of a workstream by record, not by location" is
  untouched: the granted worktree is recorded against the dispatching session's
  workstream exactly as an operator-declared one is.
- **ADR-0020 / ADR-0023 (worktree ownership and reclamation):** **Consistent,
  no interaction.** Both govern worktrees trusty-mpm owns. A harness-granted
  agent worktree enters neither ADR's bookkeeping, the same as an
  operator-declared `isolation: "worktree"` today.

No Accepted or Proposed decision contradicts this amendment.
