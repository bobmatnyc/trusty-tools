---
name: tm-ticketing
description: The single authority on issues — whether one should exist, deduplication disposition, title/body style, labels, milestones, lifecycle comments, and attribution
user-invocable: true
version: "2.0.0"
category: pm-workflow
tags: [tickets, issues, promotion-gate, deduplication, labels, pm-required]
effort: medium
---

# tm-ticketing — Issue Policy

<!-- #5202: this skill owns the ISSUE. It owns no git operation and no PR
     mutation, including PR title and body. The delivery chain that wraps it is
     `tm-workflow`. -->

The PM never calls ticketing tools directly — always delegate to the `ticketing`
agent (P6 / CB#6). This skill is what the PM reads before deciding to file,
comment on, or close anything.

## Scope

Yours: whether an issue should exist, search and dedup, issue title and body,
labels, assignee, milestone, comments, state, parent/child links.

Not yours: any git operation, and **any PR mutation — including the PR title and
body**. Those are the `version-control` agent's, delegated by the PM. Ticketing
supplies the canonical issue context that goes *into* the PR body; version
control writes it. The full boundary and the handoff sequence are stated once, in
`tm-workflow`.

For session-local task tracking that is not a formal ticket, use
`mcp__trusty-memory__task_add` / `task_list` / `task_complete` directly. These
are in-session TODOs, not a ticketing system, and are not a forbidden MCP family
under CB#6.

## Backends

The `ticketing` agent is granted the full tool set, so `gh` is available to it
whichever backend a project uses. Its priority order:

1. `mcp__mcp-ticketer__*` MCP tools, when configured.
2. GitHub Issues via `gh issue …` (or `mcp__github__*`) when the project's
   tracker is GitHub — the common case.
3. `aitrackdown` CLI when neither is available.

Route **every** Issue API operation to `ticketing`, whichever backend applies.

## Ask Before Creating

If the user references a ticket or issue and no matching one is found, ticketing
MUST NOT auto-create. Ask: "I didn't find an existing issue for [topic]. Create
one, or did you mean a different one?" Auto-create only on an explicit "create a
ticket/issue for X."

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

## What a Ticket Says

🔴 **Title**: type-aware and specific — `<type>: <what is wrong or wanted>`, e.g.
`fix(trusty-search): watcher misses renames on external volumes`. Under ~70
characters. Not "Bug in system".

🔴 **Body**: a concise problem/outcome statement, the decisive evidence, and
**one to four observable closure conditions**. Nothing else. The form is binding
and is specified once in the agent asset (`assets/agents/ticketing.md`, "Sparse
Ticket Bodies") — no structured headings, point rather than restate, cite file
and symbol rather than line numbers, and stop when the body fills a short screen.
Most tickets do all of it in under ten lines.

Alongside the closure conditions, a **defect** ticket must let the reader tell
the confidence state (§3); a feature, task, epic, or spec ticket carries none.
Every ticket, defect or not, conveys its relationship to parent work,
including the search/dispatch outcome — a fact about the issue, not a heading
to fill in.

**Bounded exceptions.** Three shapes may exceed the short-body form, and only
these:

| Shape | What it may add |
|---|---|
| `epic` | A child-work checklist and the scope boundary between children |
| Security | Impact, affected versions, and disclosure state |
| Research / audit | The evidence inventory the audit produced |

An exception buys length for *evidence*, never for narrative. Everything else
stays sparse.

This governs issue bodies only. It does **not** relax the evidence rule for
claiming a gate passed: raw test output stays mandatory there (`BASE-AGENT.md` —
never summarise test results in your own words).

## Labels

Three separable families. The `ticketing` agent applies them at creation and the
exact command form lives in the agent asset ("Label at Creation"); this is the
model, so a delegation brief never needs to spell it out.

| Family | Cardinality | Content |
|---|---|---|
| Type | exactly one | `bug`, `enhancement`, `refactor`, `chore`, `documentation`, `epic` |
| Owning component | one or more | The crate or subsystem the defect actually lives in |
| Priority | optional | `P0`–`P3`, **only** when the issue text itself asserts severity. A guessed priority is noise |

🔴 **There is no fourth family for where a finding came from.** Which session,
harness, agent, or tool surfaced an issue is never a labeling input, under any
name — not "provenance", not "umbrella", not "dogfooding". `trusty-mpm` is an
owning-component label like any other: it applies only when the code at fault
sits under `crates/trusty-mpm/` (or the tm CLI's own release tooling), never
because the session that filed the issue happened to run under tm. When no
component label fits, apply none — that decision is final, not a trigger to
reach for `trusty-mpm`.

🔴 **Never invent a label the repository does not carry.** Check `gh label list`
before using one; create a genuinely missing label rather than dropping the
family or substituting an approximation.

## Milestones

🔴 **Leave the milestone UNSET by default.** A milestone is a delivery slot in a
named release or epic — not a field every issue receives. Set one only when the
issue is one of:

- deliberately scheduled into a release you have confirmed is open;
- child work that a release-gating parent already carries into that release;
- identified as a blocker for a release already in flight.

A milestone is not a label and not a project view. An issue holds many labels and
exactly one milestone, so parking a workstream or a theme there evicts the real
release slot. `ws/<session-name>` is always a label.

## Lifecycle — open → in-progress → coded → merged → tested → closed

Four mutually exclusive labels carry the middle of an issue's life, between
GitHub's native `open` and `closed`:

| Label | Meaning |
|---|---|
| `status:in-progress` | A session has claimed it and is working it now |
| `status:coded` | Implementation pushed on a branch; PR not yet merged |
| `status:merged` | PR merged to main; live verification pending |
| `status:tested` | Verified live (installed binary, real run); eligible to close |

Advancing a state removes the prior label in the same edit —
`gh issue edit N --add-label status:merged --remove-label status:coded`. Two
`status:` labels on one issue is a defect. The `ticketing` agent runs every one
of these edits; the PM never runs `gh issue` itself.

**Claim at dispatch.** `status:in-progress` goes on when the work is dispatched,
with a dated comment naming the claiming session ("Claimed by session `<name>`,
`<date>` — fix in flight"). Another session takes a claimed issue only when the
claim is provably stale: the named session is gone AND nothing referencing the
issue — branch push, PR, comment — has moved since the claim. Either alone is
not enough. When in doubt, leave it.

**Advances are event-driven, not swept.** The agent that observed the event owes
the label pass then and there:

| Event | Advance to |
|---|---|
| PR opened for the fix | `status:coded` |
| Merge CONFIRMED (`gh pr view <n> --json state` reports `MERGED`) | `status:merged` |
| Live verification evidence in hand | `status:tested` |

A confirmed merge with no label pass is an incomplete step, not a tidy-up for
later (learned 2026-08-31: auto-merge lands PRs unattended, so nothing is
watching at the moment the state changes). `version-control` reports the
confirmed merge and flags the advance it owes; the PM routes that report to
`ticketing`, which makes the edit.

**The close bar.** An issue closes only from `status:tested`, with the live
verification evidence in the closing comment — what ran against the installed
artifact and what it printed. A merged fix that fails live verification stays
open at `status:merged`. A fix PR carries `Refs #N`, never `Closes #N`, so a
merge cannot auto-close something nobody has verified.

**Comments along the way.** A progress comment at each meaningful transition —
diagnosis confirmed, fix pushed, review verdict received, blocked — carrying
deliverables and links. Not per-poll spam. Blocked work keeps its `status:`
label and gains a comment naming the blocker, its impact, and the unblock
criteria.

Every delegation in this chain carries the ticket context, so downstream agents
can reference it in their own output. Projects without formal tracking workflows
are not subject to any of this.

### Attribution on Issues and Comments

Every issue body and issue comment ends with one line:

```
🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
```

One line, machine-readable, no preamble around it. Commit and PR attribution is
governed separately by the Framework-Guaranteed Conventions in the instruction
package, and the PR body is `version-control`'s to write.

## `/tm-ticket` Subcommands

High-level orchestration over the ticketing agent, for whichever tracker is
configured:

| Subcommand | Purpose |
|---|---|
| `/tm-ticket organize` | Review, transition states, update priorities, flag stale tickets |
| `/tm-ticket proceed` | Analyze the board, recommend the top 3 next actions |
| `/tm-ticket status` | Health metrics, ticket counts, high-priority work, blockers |
| `/tm-ticket project <url>` | Set the default project/tracker context |

Every subcommand is a PM delegation to the ticketing agent — the PM constructs
the prompt and presents the result, never calling the underlying tools itself.

## Documentation Routing With Ticket Context

With a ticket context present, delegate research findings and specs as ticket
comments (or linked files), and still write a local backup doc under
`docs/research/` (or the configured `documentation.docs_path`). Without ticket
context, everything goes to the local docs path only, named `{topic}-{date}.md`.

## Related Skills

- `tm-workflow` — the delivery chain this issue lifecycle sits inside, and the ticketing/version-control boundary
- `tm-circuit-breaker` — CB#6 enforcement detail
- `tm-delegation-patterns` — where ticketing fits in the broader agent matrix
- `tm-bug-reporting` — the MCP-native path for daemon-captured errors
