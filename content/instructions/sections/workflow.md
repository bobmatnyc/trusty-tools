<!-- PURPOSE: How each phase of the CORE phase table is executed. The
     Completion Standard lives in core.md, out of a WORKFLOW override's reach
     (#9182). -->

# PM Workflow Configuration

## Sprint, then Harden (governs how hard every gate below is applied)

- SPRINT — drive to feature-complete on a local version: targeted tests,
  no CI iteration loops, no critic round on narrow changes.
- HARDEN — once feature-complete: the project risk/stage test ladder,
  required review and release gates. Publish only after those pass.
- Spend verification where blast radius is real (destructive paths,
  SemVer/release, security); cut ceremony elsewhere. Slow release
  *causes* too many things in flight: shorten time-to-land, not cap WIP.
- 🔴 The hard line: never turn red green by deleting coverage — no `#[ignore]`,
  cfg-gating, `--exclude`, or narrowing to `--lib`. Going fast licenses fewer
  gates, never a failing gate reporting success.

## Risk — the second input to every skip condition

- **Low** — docs, comments, mechanical metadata. **Normal** — a localized
  behaviour change inside one package. **High** — security, destructive or
  irreversible paths, persisted state, release/SemVer, or a contract another
  package depends on.
- High risk defeats a size or simplicity skip heuristic: a 30-line change to a
  credential path still earns its review.
- These labels do not size testing; the project's `CLAUDE.md` test ladder does.
  Per-phase brief templates and the delivery chain: `Skill(skill="tm-workflow")`.

### Fail-Open Check (BLOCKING: a fix-bar class)

A failure branch downgrades an operation's failure to a warning, a default, or a
`false` while state advances anyway. A change that adds or touches one is not
reviewed until an error-arm regression test FAILS against the pre-fix commit.
Name the Fail-Open Check in the `code-analyzer` or `code-critic` brief; its
seven checks are in the `code-review-standards` skill.

## Layout ADRs Are Defaults, Not Mandates

A layout ADR or scaffold is the framework-specific default for a project whose
layout is still undefined; it never justifies dispatching work to restructure
an existing layout to match it (#8382).

## Live Issue Status

Dispatching work against an issue: have `ticketing` mark it in progress, and
update it when the work lands or blocks (`Skill(skill="tm-ticketing")`).

## Source Citations

Link a GitHub blob permalink pinned to a commit SHA, never `blob/main`, with
link text `path:line` and the line number verified first.

## Before Push

- A credential scan by `security` over `git diff origin/main...HEAD` is
  mandatory before any `git push`, and blocks the push on a hit. Three-dot,
  never two-dot: two-dot reports files deleted from `main` since your branch
  point as your own additions.
- Verify the base ref BEFORE diffing (#7748): `git fetch origin main` when
  `refs/remotes/origin/main` and `git ls-remote origin refs/heads/main` disagree.

## Merge-Queue Ownership

- Exactly one session owns a repository's merge queue; any other routes the
  merge to the owner and reports that it did.
- An owner's merge authorization covers only the PRs presented when it was
  given, not PRs another session opens afterward.
- 🔴 Green required contexts are not sufficient to merge. Check each PR for an
  outstanding review verdict first — a `code-critic` BLOCK, a requested-changes
  review, a hold label. A BLOCK is not a CI context, so no status check sees it.
- Hold a PR in GitHub state — draft, assignee, or a `do-not-merge` label —
  never by message; messages are advisory and lose races.
- Claiming the queue and handing it off: `Skill(skill="tm-workflow")`.
