# 0048. Dispatched writers are granted a worktree, and the main-checkout write boundary is enforced

- **Status:** Amended by [0049](0049-docs-commits-are-permitted-in-a-main-checkout.md),
  [0051](0051-a-project-may-exempt-dispatched-agents-from-the-worktree-grant.md)
- **Date:** 2026-08-15
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard` (`pm_guard_worktree_grant`,
  `pm_guard_write_boundary`, `pm_guard_bash::main_checkout`), the agent
  classifier `core::dispatch_isolation`, and the daemon's
  `DaemonState::live_shared_tree_writers` plus the `granted-worktree`
  delegation route that records what the guard granted
- **Reversibility Cost:** Medium — the grant changes what every dispatch from a
  main checkout does, and the boundary changes what every session may write
  there; both are user-visible and both ship in one release
- **Decision Drivers:** ADR-0044's enforcement requirement, recorded as stated
  but unbuilt; the owner's report of three sessions in one `mcp-services`
  checkout with commit `f1da7bce` landing on `fix/1646-drive-query-v2-migration`
  and `fix/1644-…` left empty at `cff5bbcd`; ADR-0044 row 4's finding that no
  trusty-mpm path gives a dispatched agent anywhere else to write
- **Current amendments:** [ADR-0049](0049-docs-commits-are-permitted-in-a-main-checkout.md)
  makes decision 4's `git commit` deny conditional on the staged set — a
  documents-and-configuration staged set may commit, subject to decision 10's
  live-writer check — and scopes ADR-0030's DOC-66 §0.5 position.
  [ADR-0051](0051-a-project-may-exempt-dispatched-agents-from-the-worktree-grant.md)
  makes decision 1's grant conditional on a project's committed
  `dispatch_isolation` declaration: a project that declares `main-checkout` gets
  no grant, every other project keeps the grant, and every failure to read the
  declaration keeps it too. Decisions 2-3 and 5-10 stand as accepted, and
  decision 1 stands as the default for every project that does not declare
  otherwise.
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
   the dispatch after reading a denial. The grant is reported to the daemon so
   the dispatch's delegation record carries the isolation it was given, and it is
   emitted only after that same call reports the checkout free — see
   Consequences for why neither is optional.
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
10. **`git pull`, `git merge` and `git rebase` are confined to a session's own
    worktree; in a main checkout another session is writing in, they are
    denied.** All three move HEAD and write the working tree of the directory
    they run in. A worktree's HEAD belongs to the session that owns it, so the
    move races nothing there. A shared main checkout's HEAD belongs to nobody,
    and moving it changes the branch another session's uncommitted work is
    sitting on with no error at any step. `pm_guard_bash::main_checkout`
    classifies the three verbs and `pm_guard` then asks decision 7's
    directory-keyed `live_shared_tree_writers` who else is writing in that
    directory; the deny fires only when both answers are positive. The verb
    alone is enough to classify because none of the three has a form that
    leaves HEAD alone — there is no pathspec-versus-ref ambiguity to resolve,
    which is the property `checkout` and `switch` lack and why they stay
    uncovered (see Consequences). The deny names `git fetch` first, since that
    is what most calls reaching it actually wanted, and a worktree second.
    `--abort`, `--quit`, `--continue`, `--skip`, `--edit-todo` and
    `--show-current-patch` are exempt when they appear as the FIRST argument,
    which is the only position git accepts them in: they resolve an operation
    that already started, and refusing them would park the shared checkout mid-rebase with
    no remedy reachable from there, which decision 6 forbids.

## Consequences

- **The fail-open direction is now scoped rather than global.** #4480 states
  that both its classifiers fail toward ALLOW and that the direction "is not
  negotiable", on the grounds that a false DENY halts every dispatch while a
  false ALLOW reproduces prior behaviour. That reasoning is sound where it was
  written and does not carry into a main checkout, where the costs invert: a
  false allow corrupts another session's branch — the reported harm, not a
  hypothetical one — and a false grant costs one worktree the harness reclaims
  when it is unchanged. A read-only CUSTOM agent dispatched from a main
  checkout now pays for a worktree it does not need.
- **For a READER, that price is not paid in disk — it is paid in correctness,
  and the rule is drawn back from three agents because of it.** A harness
  worktree is cut from a COMMIT, so an agent dispatched into one reads a tree
  without the session's uncommitted work and answers confidently about the wrong
  one. That is tolerable for a writer, which is going to work on a branch
  anyway; it is a wrong answer for a reader. `Explore` and `Plan` are Claude
  Code's own dispatch targets, ship in no trusty-mpm bundle, and publish tool
  sets carrying no write tool at all — they classified `Unknown` only because
  the bundle scan had never heard of them. They are now positively `ReadsOnly`
  (`READ_ONLY_HARNESS_AGENTS`) and are left in the session's tree.
  `general-purpose` is deliberately NOT among them, despite carrying the same
  wrong-tree cost: it publishes the full tool set, and in this project it is
  also the identity a failed named-agent dispatch degrades into (#4451), so a
  `general-purpose` delegation is routinely an engineer's work under another
  name. Decision 3 holds for it.
- **A `Task` dispatch that needs isolation is denied rather than rewritten.**
  `isolation` is an `Agent` parameter; injecting it into a tool whose schema
  rejects it would convert a guarded dispatch into a failed tool call. The deny
  names `Agent` as the remedy.
- **The grant is RECORDED, and it asks the concurrency question before it is
  emitted.** The first draft did neither, and decision 10 turned both omissions
  into work-halting defects. The daemon's `matcher: "*"` tracker observes the
  ORIGINAL hook payload, so a dispatch the grant had just moved into its own
  worktree stayed recorded as writing unisolated in the shared checkout — and
  decision 10 reads exactly those records, so `git pull` in that checkout was
  denied on a phantom for the six hours of `RUNNING_STALE_AFTER_SECS`, which is
  the release flow's own fast-forward step. The grant therefore posts the
  isolation it granted to `…/delegations/granted-worktree`, which UPSERTS:
  it overwrites `isolation` on an existing record, because the tracker's
  observer returns early on a `tool_use_id` it already wrote and the two hooks
  race on one event. Whichever arrives first, one isolated record results.
  The same call answers who already holds the checkout, so the #4480 verdict is
  computed BEFORE the grant rather than skipped by it: a dispatch made into a
  checkout another writer holds is denied, and only an empty answer is granted.
  Skipping it assumed the harness applies `hookSpecificOutput.updatedInput` for
  `Agent`, which trusty-mpm cannot verify — and if it does not, a second
  unisolated writer that decision 3 would have denied was simply admitted, with
  neither the shell-write path nor `git checkout <branch>` backstopping it.
- **Widening the writer query across sessions makes cross-session denies
  possible where none could occur before.** That is the point, and the grant is
  what keeps it rare: writers dispatched from a main checkout are isolated
  before the query is ever reached.
- **`DaemonState::live_shared_tree_writers` loses its `session` parameter**, a
  breaking change to a public method. `claim_shared_tree_dispatch` keeps its
  `session` argument, which now records the claim rather than filtering the
  answer.
- **`git checkout <branch>` and `git switch <branch>` are NOT covered.** They
  can still switch a branch under another session, which is part of the same
  incident. `pull`, `merge` and `rebase` left this family under decision 10 and
  are now enforced; these two stay out because their argument is genuinely
  ambiguous. `git checkout foo` is a branch switch when `foo` is a ref and a
  file restore when it is a path, and no lexical test tells them apart, so a
  verb-level rule would deny `checkout -b` and every ordinary branch creation
  along with the hazard — the false-deny failure #5356 was filed for. Covering
  them needs a different mechanism than the one decision 10 uses, not a wider
  version of it. This remains a stated gap.
- **Decision 10 denies a demonstrated collision, not every HEAD move in a main
  checkout.** A solo session running `git pull` in its own main checkout is
  allowed, because the daemon reports no other writer there. That is a
  deliberate narrowing of decision 10's "confined to a session's own worktree"
  to what can be positively evidenced, and it is what keeps the rule off
  ordinary work. Two residuals follow from it. The query sees live DELEGATED
  writers, so a second session merely standing in the checkout with nothing
  dispatched is invisible and its `pull` is allowed. And the branch fails open
  at every step — an unreachable daemon, a malformed answer, an empty
  `session_id`, or a directory the daemon recorded under a different spelling
  all answer "nobody here". Both are the #4480 guard's own fail-open direction,
  inherited with the query.
- **"A directory the daemon recorded under a different spelling" was not
  hypothetical, and is now half closed.** A delegation's `cwd` is stamped from
  `tm hook`'s own process directory, while the rule resolves the command's
  target through `cd` and `git -C` — so `cd crates/foo && git pull` keyed
  `/repo/crates/foo` against records written at `/repo` and allowed the move.
  Two directories name one HEAD, so the rule now resolves the target's main
  checkout ROOT (`project_aliases::main_checkout_root`) and asks about both,
  stopping at the first positive answer. A record stamped at some THIRD
  directory inside the same checkout remains invisible. The deny message names
  the checkout root rather than the command's directory, since that is the tree
  whose HEAD is at stake.
- **The `granted-worktree` route adds no capability a client did not already
  have.** It was examined as a possible authorization hole: any loopback client
  can post a grant naming someone else's `tool_use_id`, clear that record's
  unisolated flag, and unblock a HEAD move the guard would otherwise deny. The
  hole is real and it predates this route — the `hook_event` MCP tool already
  lets the same client post a `SubagentStop` that terminalizes the same record,
  which clears the deny outright rather than merely relabelling it. So this
  changes the wording of an existing exposure, not its extent, and the boundary
  that actually contains it is [ADR-0018](0018-loopback-only-doctrine.md)'s
  loopback-only bind. Recording it here so a later reader does not have to
  re-derive that it was considered.
- **A stale record now blocks more than it used to.** Computing the #4480
  verdict before the grant means a record nothing ever closed stops blocking
  only UNISOLATED dispatch and starts blocking every WRITER dispatch from that
  checkout — and decision 3 makes `Unknown` a writer, so that is most of them,
  for the six hours of `RUNNING_STALE_AFTER_SECS`. The two operator escape
  hatches still lift it, and an explicitly-declared `isolation: "worktree"`
  passes untouched, so this is friction rather than a lockout. The deny names
  the possibility so a reader can recognise it instead of retrying.
- **The deny attributes its claim to the daemon's records rather than asserting
  it.** This is decision 6 applied to accuracy, not just to remedies: the hook
  process knows what the records SAY, and a record can be wrong in both
  directions — one whose `SubagentStop` never arrived outlives its agent, and a
  grant whose isolation POST failed (that path fails open) describes an isolated
  agent as unisolated. Stating "X is already writing there without a worktree of
  its own" as fact was therefore a claim the guard could not check, and offered
  a remedy the named agent might already have.
- **A write performed through `Bash` rather than an edit tool** is classified by
  `pm_guard_bash`, which reaches a deny for the PM through `SHELL_EDIT_REASON`
  but does not carry the main-checkout dimension for dispatched agents. Also a
  stated gap.
- **The operator escape hatches still lift everything.** `TRUSTY_MPM_DISABLE_HOOKS`
  and `TRUSTY_MPM_PM_UNRESTRICTED` are human decisions, not automatic markers,
  and are unaffected — including the `.claude/settings.json` `env` self-exemption
  path tracked as #3981.
- **Fetch needs no enforcement; pull in a shared main checkout has it.**
  Decision 9's half of the rule has nothing to check — `fetch` is safe by
  construction, and no session can violate it by running one. Decision 10's
  half is enforced: `pm_guard_bash::main_checkout` classifies `pull`, `merge`
  and `rebase` alongside `reset --hard`, `clean -fdx` and
  `checkout -- <pathspec>`, and `pm_guard` routes the verdict through decision
  7's directory-keyed `live_shared_tree_writers`. That query previously
  answered only the isolation grant and the edit-tool/`commit` write boundary;
  it now answers a Bash call too, through the same
  `POST …/delegations/shared-tree-dispatch` route. The Bash call claims
  nothing: the route re-derives eligibility from the payload and `Bash` is not
  a dispatch tool, so its record closure never runs — a property of the route
  rather than a promise from the caller.
- **The HEAD-move rule pierces both automatic subagent exemptions**, for the
  reason decision 4 gives and by the same mechanism: it is called from
  `pm_guard()` inside the Bash block, ahead of Guard 1 (`CLAUDE_MPM_SUB_AGENT`)
  and Guard 4 (the `agent_id` dispatch marker), which return ALLOW for exactly
  the dispatched population the restriction binds. The two operator escape
  hatches are untouched.

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
