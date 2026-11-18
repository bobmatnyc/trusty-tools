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
   user's). For a non-code deliverable each criterion quotes the request, and
   the report carries an evidence map ("Non-Code Deliverables", below).
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
- A finding of any severity that directly fails a stated acceptance criterion
  of the brief or issue blocks; its fix uses the one fix round (no extra
  round).

A stated acceptance criterion is a requirement written in the issue body
(whoever wrote it), the dispatch brief, or a recorded owner ruling. A criterion
the reviewer infers, a design preference, a general quality bar, or an
Architect or PM paraphrase with no owner source is not stated; an Architect
restatement with no owner source counts only as a brief criterion. A finding
directly fails a criterion when it shows the criterion is unmet, or undemonstrated
where the criterion asks for proof; a chain of reasoning is not direct. The
review quotes the criterion and its source (`criterion-fail: "<quote>" (source:
<issue #N body | brief | owner ruling <ref>>)`); check the quote in one lookup.
Only the owner or a recorded owner ruling removes or relaxes a criterion. (#10803)

Everything else (other MEDIUM and LOW findings, missing tests on non-safety
paths, edge cases that do not break daily use, style, docs, table rows) goes as
ONE comment on the PR or the existing issue. No new review round, no new issue
and no fix round for it, unless the user asks.

**Limits.**

- Rounds: after the first review, at most one fix round and one delta review;
  a criterion-fail fix uses that one round, and a review round may hold both
  fix-bar and criterion-fail findings. If a blocking finding remains after that, stop and bring it to the Architect:
  close-and-fold, re-scope, or ask the user. Do not start a third round on your
  own. A session no Architect watches brings it to the user. Worked example of
  a branch that should have folded: #4202 → #4207.
- Scope: do not widen a change or hunt adjacent bugs. Record what you notice in
  one comment.
- Measure progress by user impact against the user's goals, not by issue counts
  or close ratios.

A project ruling that sets a stricter bar for a named project (for example
cto-reports, "MEDIUMs fixed") stays in force for that project only.

**Acceptance criteria** set an issue's good-enough bar; the rule lives in
`tm-ticketing`, `references/ticket-content.md`.

## Demo and POC Work — Minimal Tests, Named Intentions

<!-- rule:tm-workflow/demo-test-intentions -->
Owner ruling 2026-11-18. A demo, POC, prototype or spike is work whose issue
or dispatch brief records the declaration in writing ("demo/POC work"). The PR
body cites that declaration by link or quote. Without a recorded declaration
the work is production work, whatever its size.

**Demo paths.** Every directory that holds demo code is listed in
`scripts/demo-intention-paths.txt`, added in the PR that cites the
declaration. `scripts/check_demo_intentions.sh` runs in CI and fails on any
`intent(demo):` marker outside a listed path, naming the file and line.

**Minimal tests.** A demo ships with a smoke run plus a test of the core happy
path. Those two pass like any other test.

**Named intentions.** Each piece of coverage the demo defers is one skipped
case whose reason starts with `intent(demo):` and states what is owed:

- Rust: `#[ignore = "intent(demo): <what is owed>"]`
- JS/TS: `it.todo("intent(demo): <what is owed>")`
- Python: `@pytest.mark.skip(reason="intent(demo): <what is owed>")`

An intention has no body and no assertion. It is not a placeholder assertion
and nothing counts it as coverage. A Rust intention run with
`--include-ignored` reports `ok` because its body is empty; that `ok` is not a
passing test.

**The ignored-test ratchet.** `scripts/check_ignored_tests.sh` lists each Rust
intention as an `INTENTION` row and leaves it out of the
`UNSELECTED-BASELINE` count, because the demo-path lint governs it. Adding an
intention never moves the baseline, and the baseline is never raised for one.
The gate refuses a manifest row that selects an intention.

**Promotion to production.** Intentions tell the harness that the full suite is
owed once scope is accepted for production. Promotion requires turning every
intention into a real test. A reviewer or tool finds the open ones with
`git grep -n 'intent(demo):' -- <promoted paths>`; promotion is done when that
grep returns nothing and the paths are removed from
`scripts/demo-intention-paths.txt`.

**Limits.** This does not relax "scope down, never scope away" or "no
placeholders, never `todo!()`". Intentions apply only to declared demo or POC
work. They never stand in for coverage of production code, and they never
replace or hide a failing test: a red test is fixed, not turned into an
intention.

## Non-Code Deliverables — Quoted Criteria, Evidence Map, No Proxy Targets

A document reported "goals only" was closed after one table row was deleted,
and a word-count target the PM invented cut rules the owner needed (#10575).
The phase gates are code-shaped, so a document, scenario set, plan or brief
needs its own Done test. These rules apply to every deliverable; code already
meets most of them through its tests.

<!-- rule:tm-workflow/criteria-quote-the-request -->
**Criteria quote the request.** Each acceptance criterion for a non-code
deliverable quotes the requester's own words, then says what the finished
artifact shows. A criterion the PM wrote with no quoted source ("remove a
row") is not a stated criterion. For a named option such as "goals only",
state in one line what the finished artifact is, how it differs from the
current one, and what each remaining item now says. A change smaller than that
difference is not done. Size is never the measure.

<!-- rule:tm-workflow/evidence-map -->
**Evidence map.** A completion report ends with a section headed `Evidence
map`. First a table, one row per quoted request line:

| Request (quoted) | Where it shows in the artifact | How you saw it | Status |
|---|---|---|---|
| "Make it goals only: short, outcome-focused." | `docs/scenarios.md`, S1–S24 | `Read` after the last write, lines 1–180 | verified |

Then four lists, each present even when empty:

- **Verified** — you opened the artifact after the last write and saw it.
- **Helper-reported** — an agent said so; you did not check it.
- **Unfinished** — not done yet.
- **Untested** — done, not checked.

A report that says the work is not finished, with an empty Verified list, is a
valid report: "not finished: 12 of 24 rewritten, none checked yet". The same
facts reported as "done" are sent back once. The report says what each
remaining item now says, never only how much shorter the file is.

<!-- rule:tm-workflow/no-proxy-targets -->
**No proxy targets.** A number the owner gave is a constraint: keep it and
quote it. A number the PM or an agent chose (words, lines, pages, a percent
cut) is an invented proxy: drop it and state the outcome test instead, such as
which rules change behaviour and which duplicate another. A size measured
afterwards is information, never a goal. A cut that removes an item the owner
tagged `critical` or approved is not done. This extends the brief rule (#8309)
from briefs to the PM's own plan.

**Warning codes.** Owner ruling Dr8: these warn first and deny nothing.

| Code | Level | Raised when |
|---|---|---|
| `INVENTED_TARGET` | WARNING | A brief or plan pairs a number with words, lines, pages or percent and a cut verb (cut, trim, shorten, reduce, limit, cap, at most, under), and carries no recorded owner quote for it. |
| `OWNER-REQUESTED-LIMIT: "<owner's exact words>"` | WARNING clears | The brief carries the owner's quote and the same string is in a recorded owner ruling or the project's owner-decisions record. The hook checks only that a quote is present; the owner-decisions check is deferred (Dr8). An unrecorded quote keeps `INVENTED_TARGET`. |

On `INVENTED_TARGET`, drop the number and state the outcome test, or add the
recorded quote. The hook checks only that a quote is present (the owner-decisions
check is deferred, Dr8); it never shows the owner meant this use of it.

## Deterministic Work

Use existing search tools and CLIs for extraction, counts, comparisons and
explicit rules before model synthesis. If none fits, delegate a bounded,
read-only disposable helper; spot-check its inputs and representative output.
Use the model for ambiguity and judgment. Recurring tools follow the project's
implementation policy. Batch related routine work within existing budgets,
ownership and guards; this does not relax P10 or require new approval for
work already authorized. See #8021.
