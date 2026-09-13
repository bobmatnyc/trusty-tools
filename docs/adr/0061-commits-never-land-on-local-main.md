# 0061. Commits never land on local `main` — it only fast-forwards to `origin/main`

- **Status:** Amended by [0062](0062-session-history-as-per-session-git-refs.md)
- **Date:** 2026-09-13
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard`
  (`pm_guard_bash::main_checkout`'s commit rule, `core::staged_paths`); the
  project delivery workflow in `docs/reference/worktree-discipline.md` and the
  root `CLAUDE.md`
- **Reversibility Cost:** Low — decision 3's destination constraint can only
  turn ADR-0049's unscoped docs-commit allow into a narrower one; reverting
  it restores that unscoped allow, and strands no data
- **Decision Drivers:** the owner's ruling of 2026-09-13, verbatim: "The main
  checkout should be that clean checkout that tracks head. That is our
  workflow."; "The main checkout should be updated after every merge."; "a)
  worktree from head, branch from main, commit. PR, merge, update main,
  delete branch and worktree." Revised the same day on
  [#7756](https://github.com/bobmatnyc/trusty-tools/issues/7756#issuecomment-5651754326):
  docs and session notes may still be committed from the main checkout, but
  only by reaching origin through a **docs fast-path PR** on a short-lived
  `docs/*` branch, never local `main`; the correction was flagged against
  this ADR's first draft on
  [#7767](https://github.com/bobmatnyc/trusty-tools/issues/7767#issuecomment-5651754370).
- **Supersedes / Superseded by:** Amends
  [ADR-0049](0049-docs-commits-are-permitted-in-a-main-checkout.md) — its
  staged-set classifier and live-writer check still decide whether a
  documents-only commit is permitted; this ADR adds the destination
  constraint (decision 3) and restates decision 4's unconditional deny for
  local `main` specifically.
  [ADR-0044](0044-main-checkout-write-boundary-and-agent-worktree-ownership.md)
  decision 1's narrower WRITE (not commit) permission for documents and
  configuration, ADR-0048's remaining decisions,
  [ADR-0053](0053-fetch-and-pull-are-permitted-in-a-main-checkout.md)'s
  fetch/pull permission, and
  [ADR-0056](0056-main-checkout-write-access-is-granted-by-role.md) /
  [ADR-0057](0057-version-control-owns-worktree-removal.md)'s role-scoped
  exceptions for `version-control` all stay in force unchanged. **Amended by
  [ADR-0062](0062-session-history-as-per-session-git-refs.md)** (owner ruling
  2026-09-13, tracked as #7830): the commit rules below are unchanged;
  ADR-0062 adds a second, ref-based history for session and activity data,
  which the amendment note below had left with no durable home once
  `.trusty-mpm/sessions/` became gitignored.

## Context

ADR-0049 gated the main-checkout commit rule on what is STAGED rather than on
the verb: a documents-and-configuration-only commit was permitted there,
provided no other session was writing the same checkout. It closed a real
gap — ADR-0044 let a session write a document in a main checkout and ADR-0048
gave it no way to land that write — but it did so by making the main checkout
a place delivery work could terminate: the commit landed in place, on
whatever local branch HEAD already pointed to.

The owner ruled on 2026-09-13 that the main checkout is not that place. It is
"that clean checkout that tracks head," updated only by fast-forwarding to
`origin/main` after a merge. The canonical sequence — worktree from `origin/
main`, branch, commit, PR, merge, update main, delete branch and worktree —
commits exclusively in a worktree, never on local `main` itself.

The same day, on #7756, the owner corrected this ADR's first draft (which
read "the main checkout is never written to, and that includes docs"): docs
and session notes may still be committed from data staged in the main
checkout, but never onto local `main`. They reach origin through a **docs
fast-path PR** — a `tm` command commits the staged, documents-only or
session-note-only set onto a short-lived `docs/*` branch (never local
`main`), pushes it, and opens a PR with auto-merge; the checkout stays on
`main`, pinned to `origin/main`, throughout, and only fast-forwards when
that PR lands. #7767 flagged the same first draft against
`evaluate_main_checkout_commit_command_in`: the guard's existing staged-set
classifier is not wrong and is not being removed — it needs one added check,
that the commit's destination is never local `main`.

## Decision

1. **`git commit` in a main checkout, aimed at local `main` itself, is denied
   unconditionally — for every staged set, including one that is entirely
   documents and configuration.** This restores ADR-0048 decision 4's
   original rule for that target. Local `main` never receives a commit
   directly, regardless of what is staged.
2. **The main checkout advances only by fast-forward.** `git fetch` and `git
   pull --ff-only` remain permitted (ADR-0053, unaffected) as the sole way
   local `main`'s HEAD moves — to pick up a merge, never to record one. The
   fast-path merge (decision 3) reaches local `main` the same way as every
   other merge: fast-forward, never a direct commit.
3. **A documents-and-configuration-only or session-note-only staged set
   (ADR-0049's classifier, unchanged) may still be committed from the main
   checkout — but never onto local `main`.** The docs fast path commits that
   staged set onto a short-lived `docs/*` branch, pushes it, and opens a PR
   with auto-merge; ADR-0049 decision 3's live-writer check still gates it.
   This is decision 1's exception, not a second permission: an
   `evaluate_main_checkout_commit_command_in` verdict naming local `main` as
   the destination stays denied; only a verdict naming the fast-path branch
   is permitted (#7767). Where the fast path sits relative to the canonical
   eight-step sequence
   ([worktree-discipline.md](../reference/worktree-discipline.md#the-delivery-sequence)):
   it replaces steps 1–2 (worktree creation, commit in the worktree) with one
   `tm` command that commits the main checkout's own staged set onto the
   fast-path branch; steps 4–6 (PR, merge on green, fast-forward) apply
   unchanged; step 7's worktree removal does not apply, because the fast path
   creates no worktree.
4. **This decision is about commits, not ADR-0044's narrower write
   permission.** ADR-0044 decision 1 still lets a main-checkout session write
   — uncommitted — documents and configuration for framework deployment and
   session bookkeeping (`.claude/` refresh, `TASK.md`,
   `.trusty-mpm/sessions/` snapshots). A durable SOURCE change still takes
   the worktree. A durable docs or session-note change takes either the
   worktree or the fast path in decision 3 — the session's choice.
5. **`version-control`'s role-scoped exception (ADR-0056, ADR-0057) is
   unreached.** Its work in a main checkout is the merge itself (a remote
   operation against GitHub) and worktree reclamation (an administrative
   removal), neither of which commits new content into the main checkout's
   history.

## Consequences

- **A source change and a local-`main` commit are treated alike; a docs
  commit is not.** Commits happen in a worktree, or, for documents and
  session notes only, on the fast-path branch — never on local `main`
  itself. ADR-0049's staged-set special case survives, narrowed to one
  destination.
- **A necessary follow-up, not shipped here.** `pm_guard_bash::main_checkout`'s
  staged-set commit gate still mechanically ALLOWS a documents-only commit
  aimed at local `main`, which decision 1 now forbids; #7767 names the fix as
  one added destination check on `evaluate_main_checkout_commit_command_in`,
  not a removal of the `DocsOnly` classifier. This change is docs-only
  (rung 1); it states the policy now in force and leaves the code gap as a
  tracked follow-up rather than an unstated one.
- **DOC-66 §0.5's "source-restricted, not read-only" framing (ADR-0049
  decision 7) still does not describe the main checkout's commit behavior,
  but for a narrower reason than this ADR's first draft stated.** The
  checkout is not read-only for commits of any kind — a docs or
  session-note commit is still reachable from there, through the fast path.
  A future edit to DOC-66 §0.5 should say the checkout is
  destination-restricted: no commit lands on local `main` directly,
  uncommitted bookkeeping stays permitted per ADR-0044 decision 1, and a
  docs/session-note commit reaches origin only through the fast-path branch.
- **No data is stranded.** Every commit this ADR forbids on local `main` was
  already reachable through one extra step — a worktree, or the fast path for
  documents and session notes — so nothing that used to succeed is now
  impossible.

## Amendment — 2026-09-13: `.trusty-mpm/sessions/` is gitignored

Owner ruling the same day, superseding the 2026-08-31 ruling that tracked the
store: `.trusty-mpm/sessions/` is gitignored and machine-local in this
repository. A pause snapshot is therefore never committed and never reaches
origin, so the fast-path branch in decision 3 carries no session-store file,
and the `.trusty-mpm/sessions/` example in decision 4 is now covered by
ADR-0044 decision 1's uncommitted-write permission alone. Nothing else in this
ADR changes: decision 3 still governs every other document and session note,
and decision 1's deny for local `main` is untouched. Motivation: each pause
owed a PR plus a main fast-forward, concurrent pauses raced (#7782), and the
fast-forward watch blocked on the dirty sessions log.

## Amendment — 2026-09-13: session history moves to per-session git refs

Owner ruling the same day, tracked as #7830:
[ADR-0062](0062-session-history-as-per-session-git-refs.md) gives session and
activity data a durable home again, without reopening the race or the
fast-forward block the amendment above removed. Session pause and resume now
write to a per-session git ref (`refs/tm/sessions/<user-id>/<session-key>`),
an orphan commit chain outside the branch namespace, pushed with
`--force-with-lease` and fetched on its own refspec. This does not change
`.trusty-mpm/sessions/`'s gitignored, local-cache status in the working tree,
and it does not change any commit rule stated above: a session ref write is
not a `git commit` against the checkout's index and is not a fast-forward of
local `main`. See ADR-0062 for the full decision.

## Related Decisions

Vetted against the ADR corpus on 2026-09-13:

- **ADR-0049 (Documents-only commits are permitted in a main checkout):**
  **Amends.** Its staged-set classifier and live-writer check still decide
  whether a documents-only commit is permitted; this ADR adds the one
  constraint ADR-0049 lacked — the commit's destination is never local
  `main`, only the fast-path `docs/*` branch. The guard code enforcing that
  destination check is a tracked follow-up (#7767), not part of this
  decision.
- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** **Restores** decision 4's unconditional commit deny, which
  ADR-0049 had made conditional. Decisions 1–3 and 5–10 are unaffected.
- **ADR-0044 (Main-checkout write boundary and agent worktree ownership):**
  **Consistent, narrowed scope stated explicitly.** Decision 1's write (not
  commit) permission for documents and configuration is untouched; decision 4
  above draws the boundary between the two.
- **ADR-0053 (`git fetch` and `git pull` are permitted in a main checkout):**
  **Consistent.** The fast-forward-only advance this decision relies on is
  exactly what ADR-0053 already permits.
- **ADR-0056 (Main-checkout write access is granted by role) / ADR-0057
  (version-control owns worktree removal):** **Consistent, unreached.** Both
  scope an exception to `version-control` for operations that are not a
  commit of new content into the main checkout; decision 5 above states this
  explicitly.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):**
  **Consistent.** The worktree the canonical sequence commits into is the one
  this ADR already places.

No Accepted decision other than ADR-0049 required reconciling; ADR-0049 is
amended above, not superseded.
