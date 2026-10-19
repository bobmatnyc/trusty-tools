# tm-workflow — test scope by stage

## Test Scope Widens by Stage

The project risk/stage test ladder takes precedence. Where it defines no
ladder, use the following stage defaults; widen for affected consumers and
high-risk contracts, not simply for an engineer-to-QA handoff.

| Stage | Scope |
|---|---|
| Developing | tests covering the new or changed code only |
| Merging | the full test files that changed |
| Publishing | the full corpus |

- **Developing** is the inner loop: the targeted test that proves the change,
  re-run as you edit. Nothing wider is owed while the code is still moving.
- **Merging** defaults to whole affected test files. Every test
  file the diff touched runs in full — including the cases you did not edit, and
  any normally-skipped test that lives in one of those files. A change to a
  public interface or to shared test infrastructure alters what a dependent
  package's test files mean, so those files count as changed even though the
  diff never opened them.
  - **Fixture-of-record files widen the same way.** A seed fixture, Terraform
    demo-data file, or content YAML that another package's tests read
    directly is "changed" for every package that reads it, not just the one
    that owns it. Before the critic verdict, grep for the file's consumers
    and run their suites, or state in the review that none exist — gating on
    the changed file's own language tooling (`terraform validate`, `bash -n`)
    alone missed a consumer whose test asserted a fixed row count (#7750).
- **Publishing** is the only stage that owes the whole corpus, plus whatever
  release gates the project defines.

This is the two-phase doctrine at finer grain: developing is SPRINT; merging and
publishing are two different widths inside HARDEN, and merging is the narrower.
A green merge gate is therefore not a release gate.

Stage and rigour are separate axes and both bind. The stage decides how *wide* a
gate runs; the project's risk labels and test ladder decide how *hard* it is
applied and which gates run at all. Pick the rung from the project's `CLAUDE.md`,
then read the stage off where you are in the delivery chain.

A consequence for branch protection: a full-corpus CI job is a publish gate, so a
project may leave it off the required pre-merge contexts. `tm pr merge` still
refuses while it runs (#8614); merge before it settles only when the brief
waives it by name, via `--allow-failing <check>`. A **failing** check blocks the
merge at every stage. The PM may waive a pre-existing or unrelated failure of a
non-required check; a branch-caused failure is never waived.

🔴 **Widening by stage is never licence to skip.** Choosing the narrower scope is
a claim about blast radius that you must be able to prove. It is never licence to
make a red gate green by deleting a test, marking it skipped or ignored, gating
it out of the build, or excluding it from the run. That hard line ("Sprint, then
Harden") is unchanged.
