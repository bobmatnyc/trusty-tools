# DOC-88 — Fix bar: acceptance-criterion class

**Status:** Draft
**Subsystem:** Completion Standard (framework core, Architect core, `tm-workflow`, `architect-completion-standard`), `code-review-standards` rubric, prompt goldens
**Owner:** owner
**Last-updated:** 2026-11-07
**Spec ID:** `SPEC-FIXBAR-01~draft` … `SPEC-FIXBAR-08~draft` (DOC-88)
**DOC-N claim:** `DOC-88`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). No other `DOC-88` on `origin/main` at 2026-11-07.

Owner ruling (Bob, dashboard D25, via the Architect 2026-11-07 23:28Z): "Add to the fix bar: a finding of any severity that directly fails a stated acceptance criterion of the brief or issue blocks, and its fix uses the one fix round. File it for the framework core and the architect-completion-standard skill; spec-first, Architect clearance; ships after the 1.12.0 cut." Issue: [#10803](https://github.com/bobmatnyc/trusty-tools/issues/10803).

## 1. Problem {#SPEC-FIXBAR-01~draft}

**ID:** SPEC-FIXBAR-01~draft

The fix bar blocks on wrong behaviour, security or credential exposure, data loss, a crash, hang or leaked process, a resource pileup, or a broken gate or CI. Everything else is one comment, with no fix round.

A LOW or MEDIUM finding that shows a stated acceptance criterion is unmet or undemonstrated fits none of those classes. Two reviews in [#10780](https://github.com/bobmatnyc/trusty-tools/issues/10780) hit the gap and needed Architect exceptions:

- H2, "quoted lines stay inert": the issue states it as a criterion; no test covers it. Graded MEDIUM, so under today's bar it is one comment.
- `answer_note`: the owner's words were written by the Architect and could not be cited as the owner's. The brief required an owner-sourced record. Graded LOW.

Result: Done item 1 ("acceptance criteria met") can hold on paper while a review lists the criterion as unmet.

## 2. The class {#SPEC-FIXBAR-02~draft}

**ID:** SPEC-FIXBAR-02~draft

One sentence, worded once and reused verbatim in every surface (§4):

> A finding of any severity that directly fails a stated acceptance criterion of the brief or issue blocks; its fix uses the one fix round (no extra round).

Surfaces may wrap it in their own list or bullet, but the sentence is not paraphrased. The bundle test (§6) matches the exact string.

The class is an addition to the fix bar. No existing class changes, and non-matching MEDIUM and LOW findings stay one comment.

## 3. Definitions {#SPEC-FIXBAR-03~draft}

**ID:** SPEC-FIXBAR-03~draft

**Stated acceptance criterion.** A requirement written in one of:

1. the issue body (a checklist item, a "Closure" or "Acceptance" line, or a "must" statement);
2. the dispatch brief to the agent;
3. a recorded owner ruling attached to the work (an issue comment or a ledger/palace record whose source is the owner, per [DOC-87](./DOC-87-typed-owner-constraints.md)).

Not stated: a criterion the reviewer infers, a design preference, an Architect or PM paraphrase with no owner source, a general quality bar ("should be well tested").

**Directly fails.** The finding shows that the criterion is not met, or that the work does not demonstrate it when the criterion asks for proof. Examples: a required test is missing; a required field is absent from the output; the closure line names a surface the diff does not touch. "Directly" excludes a chain of reasoning ("the criterion implies X, X needs Y, Y is untested").

**Citation.** The finding quotes the criterion and names its source, in the form:

```
criterion-fail: "<verbatim quote>" (source: <issue #N body | brief | owner ruling <ref>>)
```

The PM checks the quote against the source in one lookup. A finding with no quote and source is an ordinary finding at its native severity.

## 4. Surfaces to change {#SPEC-FIXBAR-04~draft}

**ID:** SPEC-FIXBAR-04~draft

Each surface adds the §2 sentence to its fix-bar text and extends its limits text as in §5.

| # | File | Change |
|---|---|---|
| 1 | `content/instructions/sections/core.md` ("Fix bar", "Limits") | Add the sentence after the class list. Limits: the criterion-fail fix is the one fix round. |
| 2 | `content/instructions/architect/core.md` (Fix bar, Limits) | Same sentence. |
| 3 | `content/instructions/architect/architect.md` (r-045, r-078) | r-045: a criterion-fail is not a non-blocking MEDIUM or LOW. r-078: "the fix bar decides, not severity" names the class. |
| 4 | `content/skills/tm-workflow/references/completion-standard.md` | Add a bullet to the fix-bar list; the "Everything else" paragraph excludes it; Limits cover it. |
| 5 | `content/instructions/architect/skills/architect-completion-standard.md` | Same as 1 in r-043, r-044, r-078. |
| 6 | `python/trusty-architect/skills/architect-completion-standard.md` | Byte-identical copy of 5 (they are identical today; keep a `cmp` check). |
| 7 | `content/skills/code-review-standards.md` | Severity guidance, Finding Disposition (`Fix here` list), Verdict (BLOCK), tag format. |
| 8 | `content/instructions/trusty-code/skills/code-review-standards/SKILL.md` | The same rubric edits as 7 where the text exists in the copy (it has no Finding Disposition section today). |
| 9 | `crates/trusty-mpm/src/core/testdata/{pm-prompt-*,architect-prompt*}.md` (6 goldens with "Fix bar:"; `supervisor-prompt.md` has none) | Regenerate from 1 and 2. |

Rule markers `r-043`, `r-044`, `r-045`, `r-078` keep their ids; text changes only. `bundle_rule_markers_tests` must still pass.

Prompt-size impact: about 30 words in 1 and 2 (one sentence each) and about 20 words in the Limits line. Every PM and Architect prompt grows by roughly 60 tokens. Skills load on demand, so 3 to 8 add no standing cost. No other section shrinks; the context budget stays inside the existing `core.md` cap (to be confirmed by the existing size gate during implementation).

## 5. Rubric and limits mapping {#SPEC-FIXBAR-05~draft}

**ID:** SPEC-FIXBAR-05~draft

**Rubric.**

- `code-critic` and `code-analyzer` keep the native severity (CRITICAL, HIGH, MEDIUM, LOW) and append the `criterion-fail:` tag from §3.
- The verdict is BLOCK when any finding carries a valid tag, regardless of severity. This extends the BLOCK line ("any CRITICAL finding") with "or any criterion-fail finding".
- The 80% confidence filter applies first. A candidate criterion-fail the reviewer is not more than 80% sure of is dropped, like any other.
- The tag does not raise severity. Dashboards and counts still show the native grade.
- Disposition: the finding is `Fix here`. The `Fix here` class list gains the sentence from §2.
- The critic receives the criteria as part of the brief (existing rule: briefs carry the issue and the required behaviour). A brief with no criteria yields no criterion-fail findings; that is a brief defect, not a review defect.

**Limits.**

- The criterion-fail fix is the one fix round, and its check is the one delta review. No extra round.
- If a criterion-fail remains after the delta review, it goes to the Architect like any blocker. The Architect decides between a fix, a re-scope of the criterion by the owner, or close-and-fold.
- A review round can contain both fix-bar and criterion-fail findings; they share the one round.
- Only the owner or a recorded owner ruling can remove or relax a criterion. A PM or agent cannot waive one.

## 6. Test plan {#SPEC-FIXBAR-06~draft}

**ID:** SPEC-FIXBAR-06~draft

Bundle and golden tests (rung 3 for `core.md`; markdown under `crates/*/src/assets/**` and `content/**` is embedded, so run the named tests, per CLAUDE.md):

- T1. One test reads the §2 sentence from a single constant and asserts it appears byte-for-byte in surfaces 1 to 5, 7 and 8, and in the rendered PM and Architect prompts.
- T2. Surfaces 5 and 6 are identical (`cmp`).
- T3. Goldens 9 are regenerated; the golden diff shows only the §4 additions.
- T4. `bundle_rule_markers_tests` passes with `r-043`, `r-044`, `r-045`, `r-078` unchanged.
- T5. The Limits text in surfaces 1, 2, 4 and 5 states that the criterion-fail fix uses the one fix round.
- T6. Remove the sentence from one surface; T1 goes red. Restore.

Rubric fixture (a review-standards fixture next to the `code-critic` tests):

- F1. Diff plus issue with a criterion "quoted lines stay inert" and no test: expect a MEDIUM finding with a valid `criterion-fail:` tag and verdict BLOCK.
- F2. The same diff with the criterion inferred by the reviewer (no source): expect an ordinary finding, verdict WARN or APPROVE by native severity.
- F3. A LOW naming nit with no criterion: no BLOCK.
- F4. Tagged finding at 70% confidence: dropped by the 80% filter.
- F5. Tagged finding where the criterion appears only in an Architect paraphrase with no owner source: ordinary finding.

Doc gates: `scripts/check_sld.sh`, `scripts/check_test_pointers.sh`.

## 7. Rollout {#SPEC-FIXBAR-07~draft}

**ID:** SPEC-FIXBAR-07~draft

1. Architect clears this spec. No implementation before clearance.
2. Wait for the trusty-mpm 1.12.0 cut. [#10803](https://github.com/bobmatnyc/trusty-tools/issues/10803) is milestoned 1.12.1.
3. One PR changes surfaces 1 to 9 and the tests together (the text and its goldens must move in one change). Rung 6 for the prompt surface: Rust tests for `trusty-mpm` plus one binary smoke run that renders a PM and an Architect prompt.
4. Ships as a content bundle update with a `trusty-mpm` release; the `python/trusty-architect` skill copy ships with the Architect package.
5. Running sessions pick up the text at the next prompt build (session start or `/clear`). Existing in-flight PRs are reviewed under the text loaded in the session that reviews them.

## 8. Open questions {#SPEC-FIXBAR-08~draft}

**ID:** SPEC-FIXBAR-08~draft

For Bob (owner policy):

1. Does an issue-body checklist item count as a "stated acceptance criterion" when the issue is a ticket filed by an agent, not by the owner? Default in this spec: yes, an issue body counts regardless of author, because the PM dispatches against it.
2. May the Architect, in a relay with no owner source, restate a criterion for a brief? Default: it counts only as part of the brief, so a criterion the Architect adds is a brief criterion, not an owner ruling. DOC-87 provenance rules decide whether it carries owner weight.

Engineering (stated defaults):

3. Tag spelling: `criterion-fail:` lowercase with a colon. Default: as in §3.
4. Should the PM prompt carry the citation format, or only the rubric? Default: the rubric only, to hold the prompt budget.
5. Does the class apply to dispatch briefs that reference a spec section? Default: yes, a spec section the brief names as the requirement is a stated criterion; one the reviewer finds on their own is not.
6. Extend `tm-ticketing` closure checks to the same sentence? Default: no, out of scope for #10803; file a follow-up if needed.
