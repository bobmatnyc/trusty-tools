<!-- PURPOSE: How each phase of the CORE phase table is executed. -->

# PM Workflow Configuration

## Sprint, then Harden (governs how hard every gate below is applied)

- SPRINT — drive to feature-complete on a local version: targeted tests,
  no CI iteration loops, no critic round on narrow changes.
- HARDEN — once feature-complete: full suite, critic, release gates.
  Publish only after that.
- Spend the verification budget where blast radius is real — destructive paths,
  SemVer/release, security. Cut ceremony everywhere else. Slow feature release
  *causes* too many things in flight, so shortening time-to-land is the fix;
  capping WIP treats the symptom.
- 🔴 The hard line: never turn red green by deleting coverage. No `#[ignore]`,
  no cfg-gating, no `--exclude`, no narrowing to `--lib`. Going fast licenses
  running fewer gates, never making a failing gate report success.
- A branch that has drawn 3+ review rounds is evidence to close and fold, not
  to attempt round 4.
- Branch = workstream, durable. Worktree = writer, ephemeral.

## Risk — the second input to every skip condition

- Skip conditions live in the CORE phase table; risk is their second input.
- **Low** — docs, comments, mechanical metadata.
- **Normal** — a localized behaviour change inside one package.
- **High** — security, destructive or irreversible paths, persisted state,
  release/SemVer, or a contract another package depends on.
- Where a skip condition is a size or simplicity heuristic, High risk means it
  does not hold: a 30-line change to a credential path still earns its review.
- These labels say nothing about how much testing a change needs. The project's
  test ladder in its `CLAUDE.md` answers that and is authoritative.
- `code-analyzer` is a separate agent from `code-critic`. Per-phase
  dispatch-brief templates and the rest of the delivery chain:
  `Skill(skill="tm-workflow")`.

### Fail-Open Check (BLOCKING wherever a failure branch exists)

- A failure branch is an operation that can fail, whose failure is downgraded to
  a warning, a default, or a `false`, while state advances anyway.
- Where a change adds or touches one, it is not reviewed until an error-arm
  regression test exists that FAILS against the pre-fix commit.
- Name the Fail-Open Check in the dispatch brief for `code-analyzer` or
  `code-critic`; the five checks that find it are in the `code-review-standards`
  skill both agents already load.

## Live Issue Status

Dispatching work against an issue: have `ticketing` mark it in progress, and
update it when the work lands or blocks. Detail: `Skill(skill="tm-ticketing")`.

## Source Citations

- Link to a GitHub blob permalink pinned to a commit SHA, never `blob/main`.
- Link text is `path:line`, and the line number is verified before linking.

## Before Push

- A credential scan by `security` over `git diff origin/main...HEAD` is
  mandatory before any `git push`, and blocks the push on a hit.
- Three-dot, never two-dot: two-dot reports files deleted from `main` since your
  branch point as your own additions.
- The branch protection it sits inside, and the review and changelog gates:
  `Skill(skill="tm-workflow")`.

## Merge-Queue Ownership

- Exactly one session owns a repository's merge queue at a time. A session that
  does not own it routes the merge to the owner and reports that it did.
- An owner's merge authorization is scoped to the PRs presented when it was
  given: "merge them as they clear" is not a standing licence over PRs another
  session opens afterward.
- 🔴 Green required contexts are not sufficient to merge. Check each PR for an
  outstanding review verdict first — a `code-critic` BLOCK, a requested-changes
  review, a hold label. A BLOCK is not a CI context, so no status check sees it.
- Hold a PR by marking it in GitHub state — draft, assignee, or a `do-not-merge`
  label — never by message; messages are advisory and lose races.
- Claiming the queue and handing it off: `Skill(skill="tm-workflow")`.
