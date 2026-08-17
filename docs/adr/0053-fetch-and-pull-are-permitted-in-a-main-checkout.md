# 0053. `git fetch` and `git pull` are permitted in a main checkout

- **Status:** Accepted
- **Date:** 2026-08-17
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard`
  (`pm_guard_bash::main_checkout`'s `starts_a_head_move` classifier and
  `head_move_deny_reason`)
- **Reversibility Cost:** Low — the change can only turn an existing deny into
  an allow. Reverting it restores ADR-0048 decision 10's behaviour exactly and
  strands no data.
- **Decision Drivers:** the owner's ruling of 2026-08-17, verbatim: "fetch and
  pull operations are permitted. Only direct code editing is not."; ADR-0037's
  2026-08-17 scope correction, which names permitting `pull` as a change to
  ADR-0048 decision 10 rather than a wording fix; a main checkout observed 117
  commits stale while the guard named `git fetch` as the only update path
- **Supersedes / Superseded by:** Amends
  [ADR-0048](0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)
  decision 10. ADR-0044's write boundary, ADR-0048's remaining decisions, and
  ADR-0049's staged-set commit gate all stay in force unchanged.

## Context

ADR-0048 decision 10 classifies `git pull`, `git merge` and `git rebase` as
HEAD moves and denies all three in a main checkout another session is writing
in. It states the rule by verb, on the ground that none of the three "has a form
that leaves HEAD alone".

That is true of all three and decisive for only two. The hazard decision 10 was
written against is a HEAD move that leaves the shared tree in a state the other
session did not ask for. A `merge` can stop with conflict markers in the working
tree; a `rebase` rewrites the branch the other session's uncommitted work sits
on. A `pull` that fast-forwards does neither — it advances the branch to a
commit already published, and git itself refuses to run it when the working tree
would be disturbed.

The cost of the wider rule is not hypothetical. `git fetch` was the only update
path the deny named, and fetch updates `refs/remotes/origin/*` and nothing else,
so local `main` in a guarded checkout only ever goes further behind. ADR-0037's
2026-08-17 scope correction records a reader hunting for a workaround while the
checkout sat 117 commits stale, and states that permitting `pull` is a change to
this decision rather than a wording fix — which is what this ADR makes.

The owner ruled on 2026-08-17, verbatim: "fetch and pull operations are
permitted. Only direct code editing is not."

## Decision

1. **`git fetch` and `git pull` are permitted in a main checkout, unconditionally
   by this rule.** `starts_a_head_move` no longer classifies `pull`. The daemon's
   live-writer query is never reached for it, so a pull is allowed whether or not
   another session is writing in the same tree. `fetch` was already outside the
   classifier (ADR-0048 decision 9) and stays there.
2. **`git merge` and `git rebase` are unchanged.** They remain classified,
   remain subject to decision 10's live-writer query, and remain denied when that
   query answers positively. The owner's ruling named `fetch` and `pull`; the two
   verbs that can leave the shared tree conflicted or rewritten are outside it.
   Every other part of decision 10 — the directory test, the two query keys, the
   first-position `--abort`/`--continue` carve-out, the fail-open on an
   unreachable daemon — is untouched.
3. **The write boundary is untouched.** ADR-0044 decision 1 and ADR-0048
   decision 4 still deny a source-file edit and a source commit in a main
   checkout, for the PM and for every dispatched agent. This decision is about
   moving HEAD, never about editing.

## Consequences

- **A stale main checkout now has a remedy that works.** `git pull --ff-only` in
  the checkout is the ordinary answer to "local `main` is behind", and it is no
  longer refused. The advice to branch and diff against `origin/main` rather than
  local `main` is still the better habit, and is still what the `merge`/`rebase`
  deny recommends first — it is now a preference rather than the only option.
- **The residual risk is real and accepted.** A plain `git pull` with no
  `--ff-only` can still merge, and a merge started that way can conflict a shared
  tree exactly as `git merge` can. The rule is stated by verb and does not read
  the arguments, so this decision permits that form along with the safe one.
  Reading `--ff-only` versus a merging pull would reintroduce the
  argument-analysis the #5356 false-deny boundary keeps out of this module, and
  the owner's ruling named the verb. A pull that does conflict leaves the tree in
  a state `git merge --abort` resolves, which this rule has never blocked.
- **The deny message now names one remedy where it named two.** `git fetch` is
  no longer the only permitted update path, so the text names `git pull` beside
  it. The worktree remedy for the merge or rebase itself is unchanged.
- **Decision 10's daemon query is asked less often.** `pull` was the shape most
  calls reaching that query actually had, so the query — and the deny it can
  produce on a stale delegation record — now fires only on a real `merge` or
  `rebase`. That narrows the blast radius of a record that outlives its agent,
  which ADR-0048's Consequences already flagged as this rule's main false-deny
  source.

## Related Decisions

Vetted against prior ADRs on 2026-08-17:

- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** Amends — decision 10 is narrowed from three verbs to two.
  Decisions 1–9 stand as accepted.
- **ADR-0044 (Main-checkout write boundary):** Consistent — this decision moves
  HEAD and never edits a file, so the write boundary it states is untouched.
- **ADR-0049 (Documents-only commits are permitted in a main checkout):**
  Consistent — a commit is still gated on its staged set and on the live-writer
  query. Nothing here reaches the commit rule.
- **ADR-0037 (Explicit PM placement, main checkout by default):** Extends — its
  2026-08-17 scope correction says the restriction is on EDITING a main
  checkout's files and that "permitting `pull` is a change to THAT decision, not
  a wording fix here". This ADR makes that change, and a companion note on
  ADR-0037 records that its statement of what the guard blocks is now superseded
  by this one.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees`):**
  Consistent — the worktree remedy the deny names is unchanged.
