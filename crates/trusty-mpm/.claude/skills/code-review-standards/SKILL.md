---
name: code-review-standards
description: Adversarial code review rubric — severity taxonomy, the 80% confidence filter, and the APPROVE/WARN/BLOCK verdict protocol. Loaded by the code-critic agent as its primary review reference.
user-invocable: false
version: "1.0.0"
category: agent-reference
tags: [code-review, severity, verdict, quality-gate, code-critic]
effort: low
---

# Code Review Standards

The full rubric behind `code-critic`'s adversarial review. This is the
reference the agent loads before reviewing any diff — the agent body stays
lean; the standard lives here.

## Reviewer Framing

Before reviewing any code, ask: *"What would someone who has seen this exact
type of code fail in production know to check that a first-time implementer
wouldn't think to look for?"* Generic checklist-walking produces generic
findings; experience-grounded scrutiny produces the findings that matter.

Review the code against the **spec**, not against the implementer's stated
intent. If dispatch context includes implementer reasoning — "I did X
because…", commit messages, design notes — ignore it. An LLM critic that sees
the implementer's reasoning tends to agree with it (anchoring bias); a critic
that sees only the spec and the code judges whether the code actually meets
the spec, which is the job.

## Severity Taxonomy

| Severity | Definition | Examples |
|---|---|---|
| **CRITICAL** | Security vulnerability, data loss, production crash, broken contract | SQL injection, unbounded recursion on user input, `unwrap()` on a fallible I/O call in library code, a postcondition that no longer holds |
| **HIGH** | Significant correctness issue, missing error handling, likely regression | Unhandled error path, off-by-one in a boundary condition, race condition in concurrent code, silently swallowed exception |
| **MEDIUM** | Code smell, missing test coverage, maintainability concern | Duplicated logic across >2 call sites, a public function with no tests, a 200-line function doing five unrelated things |
| **LOW** | Style preference, naming, minor inefficiency | Inconsistent brace style, a variable name that could be clearer, an allocation that could be avoided but isn't on a hot path |

**Guard rail — style preferences never outrank LOW.** Whitespace, naming
aesthetics, brace placement, import ordering, and other pure style
preferences are **LOW at most**, never MEDIUM, HIGH, or CRITICAL, regardless
of how strongly the reviewer feels about them. Inflating a style nit to HIGH
to make the review look more rigorous is itself a review defect.

## The 80% Confidence Filter

For every candidate finding, ask: *can I assert this is a real issue with
more than 80% confidence?* If not:
- Downgrade the severity, or
- Drop the finding entirely.

A review full of speculative "this might be a problem" findings is worse than
a shorter review of confirmed issues — it forces the reader to re-triage the
critic's own uncertainty. When genuinely uncertain, say so explicitly in the
Notes section rather than asserting a severity you can't back up.

## Finding Disposition

Every finding ends in exactly one of three states, and the review STATES which
one. This extends the shipped review-finding rule — fix it in the surfacing PR
or drop it — by naming the third exit that rule left implicit: work that is
genuinely separable.

1. **`Fix here`** — corrected in the surfacing PR. The default for correctness,
   security, acceptance criteria, regression coverage, and any small in-scope
   repair.
2. **`Parent`** — kept with the work already in flight, as a PR comment or a
   checklist item on the parent issue. No durable artifact is created.
3. **`Promote`** — recommended for a standalone issue. The reviewer only
   recommends. Whether it is filed is decided by the Ticket-Promotion Gate in
   `tm-ticketing`, and the PM or user makes the prioritization call. Do not
   re-derive that gate's criteria here.

**An APPROVE verdict does not generate tickets.** Approving means zero CRITICAL
and zero HIGH findings; the MEDIUM/LOW observations that remain default to
`Fix here` or `Parent`. `Promote` on an approved review is a recommendation for
someone else to decide, never an instruction to file.

## Review Process

1. Work the rubric top-to-bottom: CRITICAL first, then HIGH, MEDIUM, LOW.
2. For each finding:
   - Cite the exact file + line number.
   - Quote the offending code snippet.
   - Explain why it is a problem — what actually breaks in production, not a
     generic "this is bad practice."
   - Provide the fix: concrete code or a specific, actionable change. Never
     stop at "this needs to be fixed."
   - Assign the disposition: `Fix here`, `Parent`, or `Promote`.
3. Apply the 80% confidence filter to every candidate finding.
4. Compute the verdict from the finding set (see Verdict Protocol below).

## Output Format

```
## Verdict: <APPROVE|WARN|BLOCK>

## Findings

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| CRITICAL | path/to/file.ext | 42 | <one-line description> | <concrete fix> | Fix here |
| HIGH     | path/to/file.ext | 87 | ... | ... | Parent |
| LOW      | path/to/file.ext | 96 | ... | ... | Promote |

## Required Changes (only if WARN or BLOCK)

1. <numbered list of changes required before re-review>

## Notes (optional, for context the PM should know)

<caveats, scope assumptions, things explicitly not flagged and why>
```

**Every row carries exactly one Disposition token — `Fix here`, `Parent`, or
`Promote`.** A findings table with a blank or missing Disposition cell is an
incomplete review: a reader of the posted verdict must be able to tell, per
finding, which of the three was chosen without asking anyone.

A zero-finding APPROVE omits the Findings table entirely and states: "No
issues found at >80% confidence. APPROVED for next pipeline stage." It has no
rows, so there are no dispositions to state. A clean APPROVE is a valid,
correct, and complete outcome — do not manufacture findings to look thorough.

## Verdict Protocol

- **APPROVE** — zero CRITICAL, zero HIGH findings. Proceeds to the next
  pipeline stage.
- **WARN** — zero CRITICAL, one or more HIGH findings. Code proceeds, but
  findings must be tracked: attach the finding table to the next handoff
  (typically Documentation) rather than discarding it.
- **BLOCK** — any CRITICAL finding. Halt immediately. Surface the verdict and
  finding table to the user verbatim and await explicit direction
  (fix-and-retry, override, abandon). Never auto-re-delegate back to the
  implementer without that direction.

## What NOT To Do

- Do not inflate severity to appear rigorous. Calibrate strictly to the
  taxonomy above.
- Do not flag unchanged code unless it contains a CRITICAL security issue.
- Do not consolidate findings into vague summaries — every finding needs
  file + line + fix, individually.
- Do not skip the 80% confidence filter to manufacture findings where none
  exist.
- Do not flag style preferences (whitespace, naming aesthetic, import order)
  as HIGH or CRITICAL — see the guard rail above. LOW at most.
- Do not leave a finding without a disposition. "Noted" is not one of the
  three.
- Do not file issues for your own LOW/MEDIUM polish after an APPROVE. Mark
  them `Promote` and hand the decision to the PM.
- A zero-finding APPROVE is correct and complete. Do not feel pressure to
  find issues that aren't there.

## Handoff Protocol

- **APPROVE** → report verdict to the PM; PM proceeds to the next stage
  (typically Security). Any `Promote` rows go to the PM as recommendations,
  not as filed issues.
- **WARN** → report verdict + findings to the PM; PM proceeds AND attaches
  the finding table, dispositions included, to the Documentation handoff.
- **BLOCK** → report verdict + findings to the PM; PM halts the pipeline and
  surfaces to the user. Do not auto-route back to the implementer.
