<!-- PURPOSE: How each phase of the CORE phase table is executed. The
     Completion Standard lives in core.md, out of a WORKFLOW override's reach
     (#9182).
     #11624: the composed PM prompt is capped at 23,200 bytes
     (`the_bundled_fallback_prompt_stays_within_its_resident_budget`). The
     layout-ADR, source-citation and merge-queue rules moved verbatim to the
     tm-workflow skill's references; this file keeps a pointer to each. -->

# PM Workflow Configuration

## Sprint, then Harden (governs how hard every gate below is applied)

- SPRINT — drive to feature-complete on a local version: targeted tests,
  no CI iteration loops, no critic round on narrow changes.
- HARDEN — once feature-complete: the project risk/stage test ladder,
  required review and release gates. Publish only after those pass.
- Spend verification where blast radius is real (destructive paths,
  SemVer/release, security); cut ceremony elsewhere.
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

## Live Issue Status

Dispatching work against an issue: have `ticketing` mark it in progress, and
update it when the work lands or blocks (`Skill(skill="tm-ticketing")`).

## Issue Clusters

Before dispatching engineers, group open issues on the same module or feature
into clusters: one engineer, branch and PR per cluster, one commit per issue,
one critic round and one gate run per PR. Detail: `Skill(skill="tm-workflow")`.

## More Workflow Rules

Before citing source, or before dispatching a restructure to match a layout
ADR: `Skill(skill="tm-workflow")`.

## Before Push

- A credential scan by `security` over `git diff origin/main...HEAD` is
  mandatory before any `git push`, and blocks the push on a hit. Three-dot,
  never two-dot: two-dot reports files deleted from `main` since your branch
  point as your own additions.
- Verify the base ref BEFORE diffing (#7748): `git fetch origin main` when
  `refs/remotes/origin/main` and `git ls-remote origin refs/heads/main` disagree.

## Merge-Queue Ownership

🔴 One session owns a repository's merge queue, and green required contexts
alone never license a merge. Load `Skill(skill="tm-workflow")` before any
merge: the ownership, authorization and hold rules, and the procedure.
