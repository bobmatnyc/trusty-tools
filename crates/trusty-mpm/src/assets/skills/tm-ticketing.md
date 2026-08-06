---
name: tm-ticketing
description: Ticket-driven development protocol and high-level ticketing orchestration for the trusty-mpm PM
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [tickets, workflow, pm-required]
effort: medium
---

# /tm-ticket — Ticketing Protocol

Consolidates ticket-driven-development (TkDD) enforcement and the
high-level `/tm-ticket` orchestration commands. **The PM never calls
ticketing tools directly — always delegate to the `ticketing` agent.**

## Verified Tool Reality

The bundled `ticketing` agent (`crates/trusty-mpm/src/assets/agents/ticketing.md`)
has this real priority order — verified against the agent source, not
assumed:

1. **Primary**: `mcp__mcp-ticketer__*` MCP tools, when configured.
2. **GitHub Issues**: `gh issue create/edit/view/list/close` (or
   `mcp__github__*`) when the project's tracker is GitHub — as it is for this
   repo (see root `CLAUDE.md`): spec → issue → worktree branch → PR →
   trusty-review gate → squash-merge.
3. **Fallback**: `aitrackdown` CLI (`aitrackdown create issue/task`,
   `aitrackdown transition`, `aitrackdown status tasks`) when neither of the
   above is available.

All three are the `ticketing` agent's paths — it is granted the full tool set,
so `gh` is available to it whichever backend a project uses. Route **GitHub
issue operations to `ticketing`**, not to Version Control. (Earlier revisions
of this skill said the opposite, on the theory that `ticketing` only spoke
`mcp-ticketer`/`aitrackdown` and so could not touch GitHub. That is no longer
true and the split it created is retired.)

**Version Control keeps git and PR mechanics** — branch, push, rebase,
conflict resolution, merge, release, tag. The dividing line is bookkeeping
vs. mechanics, not GitHub vs. external tracker: opening or editing a PR
*body* is bookkeeping; pushing or merging that PR is version control.

For lightweight, session-local task tracking that isn't a formal ticket at
all, use `mcp__trusty-memory__task_add` / `task_list` / `task_complete`
directly — these are cheap in-session TODOs, not a ticketing system
replacement, and do not require delegation (they are not one of the
forbidden MCP families in `tm-circuit-breaker` CB#6).

## Delegation Pattern (CB#6 in `tm-circuit-breaker`)

**Wrong:**
```
PM: mcp__mcp-ticketer__ticket_list()   # PM using ticketing tools directly
```

**Correct:**
```
PM: "I'll have ticketing organize the board..."
[PM constructs a delegation prompt for the ticketing agent]
[ticketing agent uses mcp-ticketer or aitrackdown internally]
PM: [presents results]
```

## Ask Before Creating

If the user references a ticket/issue but no matching one is found:
ticketing MUST NOT auto-create.
Ask: "I didn't find an existing issue for [topic]. Create one, or did you
mean a different one?" Auto-create only on explicit "create a
ticket/issue for X."

When a GitHub issue *is* created, apply the shipped trusty-mpm defaults:
`--assignee @me --label trusty-mpm --label ws/<session-name>` (this session's
tmux session name; create the labels first if missing: `gh label create
trusty-mpm --description "Created/managed by a trusty-mpm session" --color
8250df` and `gh label create "ws/$WS_NAME" --description "trusty-mpm
workstream $WS_NAME" --color 5319E7`). This is multi-harness support — the
assignee + `trusty-mpm` label mark which issues a trusty-mpm session owns;
`ws/<session-name>` tracks which workstream is driving it (a label, never a
milestone — see `PM_INSTRUCTIONS.md`).

These two are the harness defaults, not the whole label set. Type,
component/crate, and conditional priority are required on top of them, and the
milestone stays unset unless the issue is being scheduled into an open release.
That is specified once, in the agent asset (`assets/agents/ticketing.md`,
"Label at Creation" and "Milestones Are Release Slots") — the agent applies it,
so do not restate it in a delegation brief.

## Ticket-Promotion Gate

**A finding is not automatically a ticket.** Most findings belong to the work
already in flight; only some are worth a durable artifact that someone else has
to triage, prioritize, and eventually close. Run this gate before every
`gh issue create`.

### 1. Search before filing — and reopen before creating

Searching open and closed issues, and **reopening** a closed ticket for a
recurrence instead of filing a fresh one, is a required ordered procedure the
`ticketing` agent runs on every dispatch. It is specified once, in the agent
asset (`assets/agents/ticketing.md`, "Reopen Before You Create") — the agent
executes it, so that is where it lives. Do not restate it in a delegation
brief; state the finding and let the agent run its own gate.

### 2. Promote only an independently prioritizable outcome

File a standalone issue only when at least one of these holds:

| # | Promotion criterion |
|---|---|
| a | A reproduced, user-visible defect |
| b | Accepted feature work |
| c | A different owner, release, dependency, or security disposition from the current outcome |
| d | It cannot fit the current PR without changing that PR's outcome or risk |
| e | The user explicitly asked for it to be tracked |

Otherwise it stays a session task (`mcp__trusty-memory__task_add`), a PR review
comment, or a checklist item on the parent issue. **"Follow-up" is not a
category that bypasses this gate.**

An easy fix spotted while working on a file does not enter this gate at all: it
is noted on the CURRENT issue and made in the same work — see **Opportunistic
Fixes** in the instruction package, which this gate extends rather than
restates.

A code-review or QA finding reaches this gate by exactly one route: the
`Promote` disposition in `code-review-standards`. A reviewer marking `Promote`
has recommended, not filed — the finding still has to clear the criteria above,
and an APPROVE verdict never files a ticket on its own. `Fix here` and `Parent`
findings never reach this gate.

### 3. Label the confidence state

Every filed issue states which of these it is, in the body:

| State | Meaning | Default disposition |
|---|---|---|
| Observed | User-visible behaviour directly seen | Ticket if independently actionable |
| Reproduced | Repeatable with recorded steps or a test | Ticket if independently actionable |
| Inferred | Code evidence supports the risk; no reproduction | Note on the parent issue/PR unless high-severity |
| Speculative | Plausible concern or analogy only | Session note; no ticket |

If your own draft says "not confirmed", "possible", or "same risk class", the
state is Inferred or Speculative — keep it on the parent unless severity
justifies escalation. Nothing reads this label mechanically; it is a drafting
rule, and the check is whether a reader of the filed issue can tell which state
was claimed.

### 4. Size issues by outcome, not by finding

- One issue may hold several symptoms that share one root cause, owner, and
  acceptance test.
- Never file separate issues for the implementation, tests, documentation,
  changelog, or review cleanup needed to finish the same outcome — those are one
  PR (`tm-pr-workflow`, "One Outcome, One PR").
- Split only when the parts can be prioritized, shipped, reverted, or accepted
  independently.
- Experiments stay session-local until the project accepts the result.
- A recurring flaky test or failure family gets **one canonical issue**. Append
  each new occurrence (run URL, SHA, command, failure signature) to it; a new
  issue per occurrence is a duplicate.

### 5. Minimal issue schema (six facts)

🔴 **These are six facts a reader must be able to tell from the body. They are
NOT six headings to fill in.** A body carries defect, evidence, and resolution
in prose, sparsely — the form is specified in the agent asset
(`assets/agents/ticketing.md`, "Sparse Ticket Bodies"), which is binding here.
Most tickets convey all six in under ten lines.

1. Outcome/problem and impact.
2. Confidence: Observed | Reproduced | Inferred | Speculative.
3. Evidence/reproduction.
4. Acceptance criteria.
5. Relationship to parent work, and the search/reopen outcome.
6. Test level expected for closure.

Field 3 governs issue bodies only. It does **not** relax the evidence rule for
claiming a gate passed: raw test output stays mandatory there
(`BASE-AGENT.md` — never summarise test results in your own words).

## Ticket-Driven Development Protocol (TkDD)

When a ticket/issue reference is detected (an ID pattern, a URL, "work on
issue #123"), the PM executes:

1. **Work start** — delegate: transition to in-progress, comment with initial
   findings (for bugs: root cause or hypothesis; for features: brief scope
   summary) and any user workaround. Surfacing early findings to stakeholders
   from the moment work begins is standard practice when tracking artifacts exist.

2. **Each phase** — delegate a progress comment at meaningful state transitions
   (diagnosis confirmed, fix pushed, review verdict received, blocked/waiting).
   Not per-poll spam — only when work state materially changes. Include
   deliverables and links to commits/PRs.

3. **Work complete** — delegate: transition to done/closed, comprehensive
   completion comment with fix version/SHA and verification evidence (test
   output, deployment status, etc.), link the merged PR.

4. **Blockers** — delegate: transition to blocked, comment with blocker
   detail, impact, and unblock criteria.

**In-flight updates are standard practice.** When tracking artifacts are in
use, stakeholders follow issues from open through closure; visibility into
in-progress work is as important as the final result. Projects without formal
tracking workflows are not subject to this convention.

**Attribution footer**: every issue/PR comment ends with:
`🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools`

**PR body freshness**: if scope or claims change mid-flight (e.g., a reviewer
finding shifts what the diff covers), update the PR body immediately rather
than leaving stale assertions.

Every delegation in this chain includes the ticket/issue context so
downstream agents (Engineer, QA) know the work is ticket-driven and can
reference it in their own output.

## `/tm-ticket` Subcommands

High-level orchestration over the ticketing agent (for whichever tracker is
configured):

| Subcommand | Purpose |
|---|---|
| `/tm-ticket organize` | Review, transition states, update priorities, flag stale tickets |
| `/tm-ticket proceed` | Analyze the board, recommend the top 3 next actions |
| `/tm-ticket status` | Health metrics, ticket counts, high-priority work, blockers |
| `/tm-ticket project <url>` | Set the default project/tracker context |

Every subcommand is a PM delegation to the ticketing agent with a specific
task description — the PM constructs the prompt and presents the result, it
never calls the underlying tools itself.

## Documentation Routing With Ticket Context

When a ticket context is present, delegate to attach research findings and
specs as ticket comments (or linked files); still create a local backup doc
under `docs/research/` (or the configured `documentation.docs_path`). Without
ticket context, everything goes to the local docs path only, named
`{topic}-{date}.md`.

## Violation Prevention

Directly using ticketing tools is CB#6 (Forbidden Tool Usage) in
`tm-circuit-breaker`: Violation #1 WARNING, #2 ESCALATION, #3 FAILURE.

## Related Skills

- `tm-circuit-breaker` — CB#6 enforcement detail
- `tm-pr-workflow` — the PR side of the same delivery chain
- `tm-delegation-patterns` — where ticketing fits in the broader agent matrix
