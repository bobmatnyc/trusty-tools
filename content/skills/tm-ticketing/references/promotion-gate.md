# tm-ticketing — the ticket-promotion gate

## Ticket-Promotion Gate

**A finding is not automatically a ticket.** Most findings belong to the work
already in flight; only some are worth a durable artifact someone else has to
triage, prioritize, and eventually close. Run this gate before every issue
creation.

### 1. Search first, then choose a disposition

Searching open **and** closed issues is a required ordered procedure the
`ticketing` agent runs on every dispatch, specified once in the agent asset
(`assets/agents/ticketing.md`, "Search, Then Choose a Disposition"). Do not
restate it in a delegation brief; state the finding and let the agent run its own
gate.

Every finding that could have become a ticket ends in exactly one of four
dispositions, and the agent reports which:

| Disposition | When |
|---|---|
| `COMMENT` | An open issue already covers it — add the new occurrence there |
| `REOPEN` | A closed issue covers the same defect and the fix has not held — reopen it with the new occurrence |
| `NEW REGRESSION` | A closed issue's fix landed and verified, and this is a *different* failure mode or a different root cause — file new and link the closed one |
| `NO TICKET` | The promotion criteria below are not met — session task, PR comment, or a checklist item on the parent |

Reopening is not unconditional. Reopen when the same defect recurs with the same
root cause; file a new regression when the recurrence has a different cause, a
different symptom class, or arrives after a verified fix that a reader would need
to see as separate work.

**A reopen comment on a previously-verified issue must carry the observed
command and its actual stderr/output, never a restated prior cause.** #7185's
reopen comment repeated the original causal claim ("tm's guard counts the
marker") instead of the real failure: git's own `fatal: 'wt' contains
modified or untracked files, use --force` in a project that does not
gitignore the marker. One quoted stderr line would have named the real gate
immediately. Quote what actually ran and what it printed; do not assume the
old diagnosis still applies.

### 2. Promote only an independently prioritizable outcome

File a standalone issue only when at least one of these holds:

| # | Promotion criterion |
|---|---|
| a | A reproduced, user-visible defect |
| b | Accepted feature work |
| c | A different owner, release, dependency, or security disposition from the current outcome |
| d | It cannot fit the current PR without changing that PR's outcome or risk |
| e | The user explicitly asked for it to be tracked |

Otherwise it stays a session task, a PR review comment, or a checklist item on
the parent issue. **"Follow-up" is not a category that bypasses this gate.**

An easy fix spotted while working on a file does not enter this gate at all: it
is noted on the CURRENT issue and made in the same work — see **Opportunistic
Fixes** in the instruction package, which this gate extends rather than restates.

A code-review or QA finding reaches this gate by exactly one route: the `Promote`
disposition in `code-review-standards`. A reviewer marking `Promote` has
recommended, not filed — the finding still has to clear the criteria above, and
an APPROVE verdict never files a ticket on its own.

### 3. Label the confidence state — defects only

This distinguishes a reproduced defect from a suspected one, which changes
what a reader does next: a `bug`-typed issue states which of these it is, in
the body.

| State | Meaning | Default disposition |
|---|---|---|
| Observed | User-visible behaviour directly seen | Ticket if independently actionable |
| Reproduced | Repeatable with recorded steps or a test | Ticket if independently actionable |
| Inferred | Code evidence supports the risk; no reproduction | Note on the parent issue/PR unless high-severity |
| Speculative | Plausible concern or analogy only | Session note; no ticket |

If your own draft says "not confirmed", "possible", or "same risk class", the
state is Inferred or Speculative — keep it on the parent unless severity
justifies escalation.

🔴 **A feature, task, epic, or spec issue carries no confidence line.** The
table above is written in defect language (behaviour, reproduction, risk) and
has nothing to distinguish on a feature: the type label already says why the
issue exists, and a line like "Confidence: Observed — accepted feature work"
restates the type label under a different name. Omit it there entirely.

### 4. Size issues by outcome, not by finding

- One issue may hold several symptoms sharing one root cause, owner, and
  acceptance test.
- Never file separate issues for the implementation, tests, documentation,
  changelog, or review cleanup needed to finish the same outcome — those are one
  PR (`tm-workflow`, "One Outcome, One PR").
- Split only when the parts can be prioritized, shipped, reverted, or accepted
  independently.
- Experiments stay session-local until the project accepts the result.
- A recurring flaky test or failure family gets **one canonical issue**. Append
  each new occurrence (run URL, SHA, command, failure signature) to it under
  `COMMENT`.
