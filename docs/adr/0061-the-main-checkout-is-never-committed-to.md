# 0061. The main checkout is never committed to — it only fast-forwards to `origin/main`

- **Status:** Accepted
- **Date:** 2026-09-13
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard`
  (`pm_guard_bash::main_checkout`'s commit rule, `core::staged_paths`); the
  project delivery workflow in `docs/reference/worktree-discipline.md` and the
  root `CLAUDE.md`
- **Reversibility Cost:** Low — the change can only turn an existing allow
  back into a deny, so reverting it restores ADR-0049's staged-set carve-out
  exactly and strands no data
- **Decision Drivers:** the owner's ruling of 2026-09-13, verbatim: "The main
  checkout should be that clean checkout that tracks head. That is our
  workflow."; "The main checkout should be updated after every merge."; "a)
  worktree from head, branch from main, commit. PR, merge, update main,
  delete branch and worktree."
- **Supersedes / Superseded by:** Supersedes
  [ADR-0049](0049-docs-commits-are-permitted-in-a-main-checkout.md) in full.
  [ADR-0044](0044-main-checkout-write-boundary-and-agent-worktree-ownership.md)
  decision 1's narrower WRITE (not commit) permission for documents and
  configuration, ADR-0048's remaining decisions,
  [ADR-0053](0053-fetch-and-pull-are-permitted-in-a-main-checkout.md)'s
  fetch/pull permission, and
  [ADR-0056](0056-main-checkout-write-access-is-granted-by-role.md) /
  [ADR-0057](0057-version-control-owns-worktree-removal.md)'s role-scoped
  exceptions for `version-control` all stay in force unchanged.

## Context

ADR-0049 gated the main-checkout commit rule on what is STAGED rather than on
the verb: a documents-and-configuration-only commit was permitted there,
provided no other session was writing the same checkout. It closed a real
gap — ADR-0044 let a session write a document in a main checkout and ADR-0048
gave it no way to land that write — but it did so by making the main checkout
a place delivery work could terminate.

The owner ruled on 2026-09-13 that the main checkout is not that place. It is
"that clean checkout that tracks head," updated only by fast-forwarding to
`origin/main` after a merge. The canonical sequence — worktree from `origin/
main`, branch, commit, PR, merge, update main, delete branch and worktree —
commits exclusively in the worktree. Its second step states the scope
directly: "Commit in the worktree only. The main checkout is never written
to, and that includes docs." That is a direct reversal of the permission
ADR-0049 exists to grant.

## Decision

1. **`git commit` in a main checkout is denied unconditionally, for every
   staged set, including one that is entirely documents and configuration.**
   This restores ADR-0048 decision 4's original rule and rescinds ADR-0049 in
   full. The staged-set classifier ADR-0049 introduced no longer decides a
   commit outcome in a main checkout.
2. **The main checkout advances only by fast-forward.** `git fetch` and `git
   pull --ff-only` remain permitted (ADR-0053, unaffected) as the sole way a
   main checkout's HEAD moves — to pick up a merge, never to record one.
3. **This decision is about commits, not ADR-0044's narrower write
   permission.** ADR-0044 decision 1 still lets a main-checkout session write
   — uncommitted — documents and configuration for framework deployment and
   session bookkeeping (`.claude/` refresh, `TASK.md`,
   `.trusty-mpm/sessions/` snapshots). A session that wants a durable
   documentation change still takes the worktree, same as a source change.
4. **`version-control`'s role-scoped exception (ADR-0056, ADR-0057) is
   unreached.** Its work in a main checkout is the merge itself (a remote
   operation against GitHub) and worktree reclamation (an administrative
   removal), neither of which commits new content into the main checkout's
   history.

## Consequences

- **One rule replaces a nine-decision carve-out.** Commits happen in a
  worktree, full stop. A documentation change now takes the same
  worktree-branch-PR path as a source change, with no staged-set special case
  to remember.
- **A necessary follow-up, not shipped here.** `pm_guard_bash::main_checkout`'s
  staged-set commit gate still mechanically ALLOWS a documents-only commit in
  a main checkout until the guard code is updated to match this ADR. This
  change is docs-only (rung 1); it states the policy now in force and leaves
  the code gap as a tracked follow-up rather than an unstated one.
- **DOC-66 §0.5's "source-restricted, not read-only" framing (ADR-0049
  decision 7) no longer describes the main checkout's commit behavior.** A
  future edit to DOC-66 §0.5 should say the checkout is read-only for commits
  of any kind and writable only for the uncommitted bookkeeping ADR-0044
  decision 1 still permits.
- **No data is stranded.** Every commit this ADR forbids in a main checkout
  was already reachable through one extra step — a worktree — so nothing that
  used to succeed is now impossible.

## Related Decisions

Vetted against the ADR corpus on 2026-09-13:

- **ADR-0049 (Documents-only commits are permitted in a main checkout):**
  **Supersedes**, in full. Its staged-set classifier and nine decisions no
  longer govern the commit outcome; the guard code implementing it is a
  tracked follow-up, not part of this decision.
- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** **Restores** decision 4's unconditional commit deny, which
  ADR-0049 had made conditional. Decisions 1–3 and 5–10 are unaffected.
- **ADR-0044 (Main-checkout write boundary and agent worktree ownership):**
  **Consistent, narrowed scope stated explicitly.** Decision 1's write (not
  commit) permission for documents and configuration is untouched; decision 3
  above draws the boundary between the two.
- **ADR-0053 (`git fetch` and `git pull` are permitted in a main checkout):**
  **Consistent.** The fast-forward-only advance this decision relies on is
  exactly what ADR-0053 already permits.
- **ADR-0056 (Main-checkout write access is granted by role) / ADR-0057
  (version-control owns worktree removal):** **Consistent, unreached.** Both
  scope an exception to `version-control` for operations that are not a
  commit of new content into the main checkout; decision 4 above states this
  explicitly.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):**
  **Consistent.** The worktree the canonical sequence commits into is the one
  this ADR already places.

No Accepted decision other than ADR-0049 contradicts this ruling.
