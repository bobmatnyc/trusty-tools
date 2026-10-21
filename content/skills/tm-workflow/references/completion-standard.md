# tm-workflow — the completion standard

## Completion Standard — the Full Text

Owner ruling 2026-10-04. It restates and makes canonical the 2026-09-27 rulings
on the fix bar and on judging by user impact. The PM prompt's `CORE` section
carries the short form, which no project override removes (#9182). It applies
to every PM and to the Architect. These are instructions, not hook enforcement.
Anything beyond this standard needs an explicit request from the user.

**Bias.** Drive work to done and ship it. Prefer finishing and closing over
finding more to fix. A defect with little day-to-day risk or impact does not
delay a merge, a release or a close.

**Done — all four, then stop.**

1. The stated acceptance criteria are met (the issue's, the brief's or the
   user's).
2. The required gates pass: tests for the changed behaviour, the required CI
   checks, a critic with no open CRITICAL or HIGH, and the credential scan.
3. For runtime changes, the live check the change's rung requires has passed.
4. It is shipped by the project's normal path (merged, released or deployed as
   required), and the issue is closed with evidence.

**Good enough: the fix bar.** A finding blocks done ONLY if it causes, or leaves
unguarded, one of:

- wrong behaviour against the acceptance criteria;
- a security or credential exposure;
- data loss or corruption;
- a crash, a hang, or a leaked process;
- a resource pileup (worktrees, slots, disk, memory growth);
- a broken gate or CI.

Everything else (other MEDIUM and LOW findings, missing tests on non-safety
paths, edge cases that do not break daily use, style, docs, table rows) goes as
ONE comment on the PR or the existing issue. No new review round, no new issue
and no fix round for it, unless the user asks.

**Limits.**

- Rounds: after the first review, at most one fix round and one delta review.
  If a blocking finding remains after that, stop and bring it to the Architect:
  close-and-fold, re-scope, or ask the user. Do not start a third round on your
  own. A session no Architect watches brings it to the user. Worked example of
  a branch that should have folded: #4202 → #4207.
- Scope: do not widen a change or hunt adjacent bugs. Record what you notice in
  one comment.
- Measure progress by user impact against the user's goals, not by issue counts
  or close ratios.

A project ruling that sets a stricter bar for a named project (for example
cto-reports, "MEDIUMs fixed") stays in force for that project only.

## Deterministic Work

Use existing search tools and CLIs for extraction, counts, comparisons and
explicit rules before model synthesis. If none fits, delegate a bounded,
read-only disposable helper; spot-check its inputs and representative output.
Use the model for ambiguity and judgment. Recurring tools follow the project's
implementation policy. Batch related routine work within existing budgets,
ownership and guards; this does not relax P10 or require new approval for
work already authorized. See #8021.
