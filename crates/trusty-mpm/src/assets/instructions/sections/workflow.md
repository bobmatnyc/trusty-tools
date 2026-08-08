<!-- PURPOSE: How each phase of the CORE phase table is executed. -->

# PM Workflow Configuration

## Sprint, then Harden (governs how hard every gate below is applied)

Work runs in two phases, not one blended one.

1. **SPRINT** — drive to feature-complete on a local version. Targeted tests
   while developing; no CI iteration loops, no critic round on narrow changes.
2. **HARDEN** — once feature-complete, test and fix carefully:
   full suite, critic, release gates. Publish only after that.

Spend the verification budget where blast radius is real — destructive paths,
SemVer/release, security — and cut ceremony everywhere else. Slow feature
release *causes* too many things in flight, so shortening time-to-land is the
fix; capping WIP treats the symptom.

**The hard line that must never be crossed while going fast:
never turn red green by deleting coverage.** No `#[ignore]`, no cfg-gating, no
`--exclude`, no narrowing to `--lib`. Going fast licenses running fewer gates,
never making a failing gate report success.

A branch that has drawn 3+ review rounds is evidence to close and fold, not to
attempt round 4. Branch = workstream, and it is durable; worktree = writer, and
it is ephemeral.

## Risk — the second input to every skip condition

The CORE section's phase table is canonical for WHETHER a phase runs and carries
each skip condition; where a phase runs, its gate is blocking — "conditional"
governs entry, never rigour (issue #4594).

Label the change **Low** (docs, comments, mechanical metadata), **Normal** (a
localized behaviour change inside one package), or **High** (security,
destructive or irreversible paths, persisted state, release/SemVer, or a
contract another package depends on). Where a skip condition is a size or
simplicity heuristic, High risk means it does not hold: a 30-line change to a
credential path is small and still earns its review.

The labels say nothing about how much testing a change needs. The project's test
ladder in its `CLAUDE.md` answers that, and is authoritative where the project
defines one.

Each phase's executing agent is the one named in the CORE phase table, and
`code-analyzer` is a separate agent from `code-critic`. Per-phase dispatch-brief
templates: `Skill(skill="tm-workflow")`.

### Fail-Open Check (BLOCKING wherever a failure branch exists)

Where a change adds or touches a failure branch — an operation that can fail,
whose failure is downgraded to a warning, a default, or a `false`, while state
advances anyway — that branch is not reviewed until an error-arm regression test
exists that FAILS against the pre-fix commit. **Name the Fail-Open Check in the
dispatch brief** for `code-analyzer` or `code-critic`; the five checks that find
it are in the `code-review-standards` skill both agents already load.

## Source Citations

A source citation links to a GitHub blob permalink pinned to a commit SHA, never
`blob/main`, which silently retargets as lines shift. Link text is `path:line`,
and the line number is verified before linking.

## Before Push

A credential scan by `security` over `git diff origin/main HEAD` is mandatory
before any `git push`, and blocks the push on a hit. The branch protection it
sits inside, and the review and changelog gates:
`Skill(skill="tm-pr-workflow")`.
